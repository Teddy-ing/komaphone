//! Responsive native canvas coordinator. HTTP runs on a separate bounded worker,
//! so a network timeout cannot delay the local durable edit acknowledgment.
use crate::worker::fence::{AuthFence, Priority, Stamped};
use crate::{
    Error, Outbox, Result, Transport,
    world_queue::{Activation, QueuedOperation, RecoveryPage, RecoveryReason, ViewerState, WorldOutbox, WorldScope},
    world_replica::WorldReplica,
};
use photocraft_scene::{
    CanvasId, OwnerId,
    shared::{DeviceId, Edit, OpId, WorldOp, WorldState},
    world_protocol::{self as wire, Action, Response},
};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
pub mod thumbnail;

/// Secret-bearing handoff inside the native process only. Never Debug/Serialize.
pub struct WorldAccess {
    scope: WorldScope,
    credential: String,
    auth: Option<(Arc<AuthFence>, u64, bool)>,
}
impl PartialEq for WorldAccess {
    fn eq(&self, other: &Self) -> bool {
        self.scope == other.scope
            && self.credential == other.credential
            && match (&self.auth, &other.auth) {
                (None, None) => true,
                (Some((a, at, ap)), Some((b, bt, bp))) => Arc::ptr_eq(a, b) && at >> 1 == bt >> 1 && ap == bp,
                _ => false,
            }
    }
}
impl Eq for WorldAccess {}
/// Fresh service-confirmed profile/writer proof. Only the authenticated transport
/// can construct this value; it cannot be supplied by JSON or a claimed account.
pub struct VerifiedWorldIdentity {
    scope: WorldScope,
    verified_at: Instant,
}
impl VerifiedWorldIdentity {
    pub fn scope(&self) -> &WorldScope {
        &self.scope
    }
    pub fn fresh(&self) -> bool {
        self.verified_at.elapsed() <= Duration::from_secs(60)
    }
}
impl WorldAccess {
    pub(crate) fn for_relocation(scope: WorldScope, credential: String) -> Result<Self> {
        photocraft_party::protocol::validate_secret(&credential).map_err(other)?;
        Ok(Self { scope, credential, auth: None })
    }
    pub fn from_room(store: &Outbox) -> Result<Option<Self>> {
        let status = store.status();
        if !store.active() || !status.paired {
            return Ok(None);
        }
        let Some(profile) = status.profile else { return Ok(None) };
        let Some(endpoint) = status.endpoint else { return Ok(None) };
        let scope = WorldScope::new(&endpoint, OwnerId::new(profile.id.as_str()).map_err(other)?, DeviceId::new(profile.device.as_str()).map_err(other)?)?;
        Ok(Some(Self { scope, credential: store.credential()?, auth: None }))
    }
    pub fn scope(&self) -> &WorldScope {
        &self.scope
    }
    /// Native secret-bearing handoff only; never serialize or log the copy.
    pub fn copy_for_worker(&self) -> Self {
        Self { scope: self.scope.clone(), credential: self.credential.clone(), auth: self.auth.clone() }
    }
    pub(crate) fn with_auth_fence(mut self, fence: Arc<AuthFence>, ticket: u64, allow_paused: bool) -> Self {
        self.auth = Some((fence, ticket, allow_paused));
        self
    }
    fn check_active(&self) -> Result<()> {
        if let Some((fence, ticket, _)) = &self.auth {
            fence.check(*ticket)?;
        }
        Ok(())
    }
    fn transport(&self, certificate: Option<&[u8]>) -> Result<Transport> {
        let transport = Transport::new(certificate)?;
        Ok(match &self.auth {
            Some((fence, ticket, true)) => transport.with_auth_fence_current(Arc::clone(fence), *ticket),
            Some((fence, ticket, false)) => transport.with_auth_fence(Arc::clone(fence), *ticket),
            None => transport,
        })
    }
    pub fn verify_identity(&self, trusted_certificate: Option<&[u8]>) -> Result<VerifiedWorldIdentity> {
        let response = self.transport(trusted_certificate)?.call_world(
            &self.scope.endpoint,
            &self.credential,
            &wire::Request { version: wire::VERSION, action: Action::Identity {} },
        )?;
        let Response::Identity { profile, device } = response else {
            return Err(other("The service did not verify the account and device identity"));
        };
        if profile != self.scope.profile || device != self.scope.device {
            return Err(other("Stored account or device does not match the authenticated service identity; preserve local artwork and reconnect"));
        }
        Ok(VerifiedWorldIdentity { scope: self.scope.clone(), verified_at: Instant::now() })
    }
}

pub enum Command {
    Relocate { relocation: crate::AccountRelocation, access: WorldAccess },
    Access(Option<WorldAccess>),
    Request(Action),
    Open(CanvasId),
    Edit { canvas: CanvasId, action: Edit },
    SaveView { canvas: CanvasId, state: ViewerState },
    RecoveryPage { after: Option<OpId>, limit: usize },
    ResumeRecovery { canvas: CanvasId },
    Retry,
}
pub enum Event {
    Relocated {
        sequence: u64,
        id: photocraft_party::RequestId,
    },
    Activated {
        sequence: u64,
        scope: Option<WorldScope>,
    },
    /// This worker has terminated; unlike request errors this is independent of account scope.
    Stopped {
        message: String,
    },
    CachedCanvases {
        items: Vec<wire::CanvasSummary>,
    },
    PresenceDropped {
        sequence: u64,
    },
    Response {
        sequence: u64,
        response: Response,
    },
    Saved {
        sequence: u64,
        operation: WorldOp,
    },
    ViewSaved {
        sequence: u64,
        canvas: CanvasId,
    },
    Revoked {
        canvas: CanvasId,
    },
    RecoveryPage {
        sequence: u64,
        page: RecoveryPage,
    },
    RecoveryResumed {
        sequence: u64,
        canvas: CanvasId,
    },
    State {
        info: wire::CanvasSummary,
        canonical: Arc<WorldState>,
        display: Arc<WorldState>,
        pending: usize,
        view: Option<ViewerState>,
        notice: Option<String>,
    },
    Error {
        sequence: u64,
        message: String,
    },
}
struct Envelope {
    sequence: u64,
    ticket: u64,
    command: Command,
}
struct Job {
    ticket: u64,
    auth: Option<(Arc<AuthFence>, u64, bool)>,
    generation: u64,
    sequence: u64,
    endpoint: String,
    credential: String,
    action: Action,
}
impl Job {
    fn call(&self, transport: &Transport, fence: &Arc<AuthFence>) -> Result<Response> {
        let mut transport = transport.with_auth_fence(Arc::clone(fence), self.ticket);
        if let Some((parent, ticket, _)) = &self.auth {
            transport = transport.with_auth_fence(Arc::clone(parent), *ticket);
        }
        transport.call_world(&self.endpoint, &self.credential, &wire::Request { version: wire::VERSION, action: self.action.clone() })
    }
}
struct Reply {
    generation: u64,
    sequence: u64,
    action: Action,
    result: Result<Response>,
}
pub struct WorldWorker {
    alive: Arc<std::sync::atomic::AtomicBool>,
    tx: SyncSender<Envelope>,
    rx: Receiver<Stamped<Event>>,
    sequence: AtomicU64,
    _join: JoinHandle<()>,
    auth_fence: Arc<AuthFence>,
    priority: Arc<Priority<Envelope>>,
}
fn other(value: impl std::fmt::Display) -> Error {
    Error::Invalid(value.to_string())
}
fn upload_action(operation: WorldOp) -> Result<Action> {
    if matches!(operation.action, Edit::CopyStroke { .. }) {
        return Ok(Action::RemixStroke { operation });
    }
    use photocraft_scene::shared::media::MediaSource;
    let source = if let Edit::PlaceMedia { media } = &operation.action { Some(media.provenance.source.clone()) } else { None };
    match source {
        Some(MediaSource::Gallery { room: Some(room), .. }) => Ok(Action::RemixGame { operation, room }),
        Some(MediaSource::Canvas { canvas, media }) => Ok(Action::RemixCanvas { operation, source_canvas: canvas, source_media: media }),
        Some(_) => Err(other("Prepare the authorized gallery source before placing media")),
        None => Ok(Action::Append { operation }),
    }
}
impl WorldWorker {
    pub fn start(database: PathBuf, trusted_certificate: Option<Vec<u8>>) -> Result<Self> {
        let (tx, commands) = mpsc::sync_channel::<Envelope>(32);
        let (events, rx) = mpsc::sync_channel(32);
        let auth_fence = Arc::new(AuthFence::default());
        let priority = Arc::new(Priority::default());
        let worker_fence = Arc::clone(&auth_fence);
        let worker_priority = Arc::clone(&priority);
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let running = Arc::clone(&alive);
        let join = std::thread::Builder::new()
            .name("komaphone-world-local".into())
            .spawn(move || {
                if let Err(error) = run(database, trusted_certificate, commands, &events, Arc::clone(&worker_fence), worker_priority) {
                    running.store(false, Ordering::Release);
                    let _ = events.try_send(Stamped { ticket: worker_fence.ticket(), value: Event::Stopped { message: error.to_string() } });
                }
                running.store(false, Ordering::Release);
            })
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(Self { tx, rx, sequence: AtomicU64::new(1), _join: join, auth_fence, priority, alive })
    }
    pub fn pause_authenticated(&self) {
        self.auth_fence.pause();
    }
    pub fn send(&self, command: Command) -> Result<u64> {
        if !self.alive.load(Ordering::Acquire) {
            self.pause_authenticated();
            return Err(Error::Unavailable);
        }
        let sequence = self.sequence.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1)).map_err(|_| Error::Busy)?;
        if matches!(command, Command::Access(_) | Command::Relocate { .. }) {
            self.pause_authenticated();
            self.priority.put(Envelope { sequence, ticket: self.auth_fence.ticket(), command });
        } else {
            self.tx.try_send(Envelope { sequence, ticket: self.auth_fence.ticket(), command }).map_err(|_| Error::Busy)?;
        }
        Ok(sequence)
    }
    pub fn poll(&self) -> std::result::Result<Event, TryRecvError> {
        loop {
            let event = self.rx.try_recv()?;
            if matches!(event.value, Event::Saved { .. } | Event::ViewSaved { .. } | Event::Stopped { .. })
                || (self.auth_fence.current(event.ticket)
                    && (!self.auth_fence.paused()
                        || matches!(event.value, Event::Activated { scope: None, .. } | Event::Relocated { .. } | Event::Error { .. })))
            {
                return Ok(event.value);
            }
        }
    }
}
impl Drop for WorldWorker {
    fn drop(&mut self) {
        self.pause_authenticated();
    }
}

struct Coordinator {
    auth_fence: Arc<AuthFence>,
    auth_ticket: u64,
    queue: WorldOutbox,
    access: Option<WorldAccess>,
    lease: Option<Activation>,
    generation: u64,
    replicas: BTreeMap<CanvasId, WorldReplica>,
    selected: Option<CanvasId>,
    requests: VecDeque<(u64, Action)>,
    recovery_resume: BTreeMap<CanvasId, RecoveryResume>,
    outbound: VecDeque<Event>,
    inflight: bool,
    next_poll: Instant,
    network_after: Instant,
    cached_directory: Option<Option<CanvasId>>,
    directory_start: Option<u64>,
    directory_scan: Option<directory::Scan>,
    presence: presence::Lane,
}
mod directory;
mod presence;
struct RecoveryResume {
    sequence: u64,
    refreshed: bool,
}
impl Coordinator {
    fn lease(&self) -> Result<&Activation> {
        self.lease.as_ref().ok_or_else(|| other("Connect to the private service before sharing canvases"))
    }
    fn access(&mut self, sequence: u64, access: Option<WorldAccess>) -> Result<()> {
        if let Some(access) = &access {
            access.check_active()?;
        }
        if self.access == access && (access.is_none() || self.lease.is_some()) {
            // Redundant handoffs acknowledge their own sequence without losing
            // pending local durability acknowledgments or resetting the queue.
            self.outbound.push_back(Event::Activated { sequence, scope: access.as_ref().map(|value| value.scope.clone()) });
            return Ok(());
        }
        self.generation = self.generation.checked_add(1).ok_or(Error::Busy)?;
        self.queue.sign_out()?;
        self.lease = None;
        self.replicas.clear();
        self.selected = None;
        self.requests.clear();
        self.recovery_resume.clear();
        self.outbound.retain(|event| matches!(event, Event::Saved { .. } | Event::ViewSaved { .. }));
        self.cached_directory = None;
        self.directory_start = None;
        self.directory_scan = None;
        self.invalidate_presence()?;
        self.access = access;
        if let Some(access) = &self.access {
            self.lease = Some(self.queue.activate(access.scope.clone())?);
            self.outbound.push_back(Event::Activated { sequence, scope: Some(access.scope.clone()) });
            self.cached_directory = Some(None);
            self.directory_start = Some(0);
            self.requests.push_back((0, Action::List { after: None, limit: wire::MAX_PAGE }));
        } else {
            self.outbound.push_back(Event::Activated { sequence, scope: None });
        }
        self.network_after = Instant::now();
        Ok(())
    }
    fn cached_directory_page(&mut self) -> Result<()> {
        let Some(after) = self.cached_directory.take() else { return Ok(()) };
        let page = self.queue.cached_summaries(self.lease()?, after, wire::MAX_PAGE as usize)?;
        self.cached_directory = page.next.map(Some);
        self.outbound.push_back(Event::CachedCanvases { items: page.items });
        Ok(())
    }
    fn command(&mut self, sequence: u64, command: Command) -> Result<()> {
        match command {
            Command::Relocate { .. } => Err(other("Endpoint relocation requires the verified native coordinator")),
            Command::Access(access) => self.access(sequence, access),
            Command::Request(action) => {
                self.lease()?;
                if matches!(action, Action::Presence { .. } | Action::Peers { .. }) {
                    self.presence_enqueue(sequence, action);
                    return Ok(());
                }
                if matches!(action, Action::PresenceOpen { .. } | Action::PresenceUpdate { .. } | Action::PresenceRead { .. }) {
                    return Err(other("Presence streams are managed by the native worker"));
                }
                if self.requests.len() >= 32 {
                    return Err(Error::Busy);
                }
                if matches!(action, Action::Append { .. } | Action::RemixGame { .. } | Action::RemixCanvas { .. } | Action::RemixStroke { .. }) {
                    return Err(other("Drawing operations must be durably enqueued before upload"));
                }
                self.directory_request(sequence, &action);
                self.requests.push_back((sequence, action));
                Ok(())
            }
            Command::Open(canvas) => {
                let lease = self.lease()?.clone();
                self.invalidate_presence()?;
                self.selected = Some(canvas);
                if let Some(world) = self.queue.load_snapshot(&lease, canvas)?
                    && let Some((epoch, _)) = self.queue.cached_grant(&lease, canvas)?
                    && world.import_origin.as_ref().is_none_or(|origin| world.revision >= origin.source_revision)
                {
                    let info = wire::CanvasSummary {
                        import_origin: world.import_origin.clone(),
                        canvas,
                        owner: world.owner,
                        name: world.name.clone(),
                        initial_name: world.replay_to(0).map_err(other)?.name,
                        revision: world.revision,
                        epoch,
                    };
                    self.replicas.insert(canvas, WorldReplica::open(info, Some(world))?);
                    self.publish(canvas)?;
                }
                self.requests.push_front((sequence, Action::Info { canvas }));
                Ok(())
            }
            Command::Edit { canvas, action } => {
                let lease = self.lease()?.clone();
                if self.selected != Some(canvas) || !self.replicas.get(&canvas).is_some_and(WorldReplica::complete) {
                    return Err(other("Wait for the selected canvas history to finish loading"));
                }
                if let Edit::PlaceMedia { media } = &action {
                    media.validate().map_err(other)?;
                    if matches!(media.provenance.source, photocraft_scene::shared::media::MediaSource::Gallery { room: None, .. }) {
                        return Err(other("Prepare the authorized gallery source before placing media"));
                    }
                }
                let operation = self.queue.enqueue(&lease, canvas, action)?;
                self.outbound.push_back(Event::Saved { sequence, operation });
                self.publish(canvas)
            }
            Command::SaveView { canvas, state } => {
                let lease = self.lease()?.clone();
                self.queue.save_view(&lease, canvas, &state)?;
                self.outbound.push_back(Event::ViewSaved { sequence, canvas });
                Ok(())
            }
            Command::RecoveryPage { after, limit } => {
                let page = self.queue.recovery_page(self.lease()?, after, limit)?;
                self.outbound.push_back(Event::RecoveryPage { sequence, page });
                Ok(())
            }
            Command::ResumeRecovery { canvas } => {
                self.lease()?;
                if self.selected != Some(canvas) || !self.replicas.contains_key(&canvas) {
                    return Err(other("Open the affected canvas before resuming new edits"));
                }
                if self.recovery_resume.contains_key(&canvas) || self.requests.len() >= 32 {
                    return Err(Error::Busy);
                }
                self.recovery_resume.insert(canvas, RecoveryResume { sequence, refreshed: false });
                self.requests.push_front((sequence, Action::Info { canvas }));
                self.network_after = Instant::now();
                Ok(())
            }
            Command::Retry => {
                self.network_after = Instant::now();
                self.next_poll = Instant::now();
                Ok(())
            }
        }
    }
    fn canvas_pending(&self, lease: &Activation, canvas: CanvasId) -> Result<Vec<QueuedOperation>> {
        let mut cursor = None;
        let mut items = Vec::new();
        let mut bytes = 0usize;
        loop {
            let page = self.queue.pending_page(lease, cursor, 256)?;
            for item in page.items {
                if item.operation.canvas == canvas {
                    bytes = bytes.saturating_add(serde_json::to_vec(&item.operation).map_err(other)?.len());
                    if bytes > 32 * 1024 * 1024 || items.len() >= 8192 {
                        return Err(other("Saved offline canvas edits exceed the preview memory limit; synchronize or recover the retained queue"));
                    }
                    items.push(item);
                }
            }
            match page.next {
                Some(next) if next.canvas <= canvas => cursor = Some(next),
                _ => break,
            }
        }
        Ok(items)
    }
    fn publish(&mut self, canvas: CanvasId) -> Result<()> {
        let lease = self.lease()?.clone();
        let Some(replica) = self.replicas.get(&canvas) else { return Ok(()) };
        let pending = self.canvas_pending(&lease, canvas)?.into_iter().map(|q| q.operation).collect::<Vec<_>>();
        let canonical = Arc::new(replica.accepted().clone());
        let (display, notice) = match replica.with_pending(lease.scope().profile, &pending) {
            Ok(world) => (Arc::new(world), None),
            Err(error) => (canonical.clone(), Some(format!("Saved offline edits need recovery before preview: {error}"))),
        };
        let view = self.queue.load_view(&lease, canvas)?;
        // Replace stale presentation snapshots, but retain every durable save/error acknowledgment.
        self.outbound.retain(|event| !matches!(event,Event::State {info,..} if info.canvas==canvas));
        self.outbound.push_back(Event::State { info: replica.info().clone(), canonical, display, pending: pending.len(), view, notice });
        Ok(())
    }
    fn ready(&mut self, canvas: CanvasId) -> Result<()> {
        let lease = self.lease()?.clone();
        let Some(replica) = self.replicas.get(&canvas) else { return Ok(()) };
        if !replica.complete() {
            return Ok(());
        }
        self.queue.grant(&lease, canvas, replica.info().epoch, replica.accepted().next_sequence(lease.scope().device).map_err(other)?)?;
        self.queue.save_snapshot(&lease, replica.accepted())?;
        for queued in self.canvas_pending(&lease, canvas)? {
            if queued.operation.canvas == canvas
                && let Some(accepted) = replica.accepted().receipt(queued.operation.id)
            {
                self.queue.acknowledge(&queued, accepted)?;
            }
        }
        if self.recovery_resume.get(&canvas).is_some_and(|resume| resume.refreshed)
            && let Some(resume) = self.recovery_resume.remove(&canvas)
        {
            let next = replica.accepted().next_sequence(lease.scope().device).map_err(other)?;
            match self.queue.resume_after_recovery(&lease, canvas, replica.info().epoch, next) {
                Ok(()) => self.outbound.push_back(Event::RecoveryResumed { sequence: resume.sequence, canvas }),
                Err(error) => self.outbound.push_back(Event::Error { sequence: resume.sequence, message: error.to_string() }),
            }
        }
        self.publish(canvas)
    }
    fn reply(&mut self, reply: Reply) -> Result<()> {
        self.inflight = false;
        if reply.generation != self.generation {
            return Ok(());
        }
        let lease = self.lease()?.clone();
        self.queue.ensure_current(&lease)?;
        let response = match reply.result {
            Ok(response) => response,
            Err(error) => {
                if matches!(reply.action, Action::List { .. }) {
                    self.directory_scan = None;
                }
                if let Error::Rejected { code, .. } = &error {
                    let canvas = match &reply.action {
                        Action::Append { operation }
                        | Action::RemixGame { operation, .. }
                        | Action::RemixCanvas { operation, .. }
                        | Action::RemixStroke { operation } => Some(operation.canvas),
                        Action::Info { canvas } | Action::Events { canvas, .. } => Some(*canvas),
                        _ => None,
                    };
                    if let Some(canvas) = canvas {
                        let source_rejected = code == "unauthorized"
                            && matches!(&reply.action, Action::RemixGame { .. } | Action::RemixCanvas { .. } | Action::RemixStroke { .. });
                        if code == "unauthorized" && !source_rejected {
                            self.queue.revoke(&lease, canvas)?;
                            self.replicas.remove(&canvas);
                            if self.selected == Some(canvas) {
                                self.selected = None;
                                self.invalidate_presence()?;
                            }
                            self.outbound.push_back(Event::Revoked { canvas });
                        } else if (source_rejected || matches!(code.as_str(), "conflict" | "invalid"))
                            && let Action::Append { operation }
                            | Action::RemixGame { operation, .. }
                            | Action::RemixCanvas { operation, .. }
                            | Action::RemixStroke { operation } = &reply.action
                            && let Some(queued) = self.canvas_pending(&lease, canvas)?.into_iter().find(|q| q.operation.id == operation.id)
                        {
                            self.queue.reject(&queued, RecoveryReason::Conflict)?;
                            self.publish(canvas)?;
                            // A remix requires both source and destination access.
                            // Only an independent destination Info failure proves
                            // destination membership was removed.
                            self.requests.push_front((0, Action::Info { canvas }));
                        }
                    }
                }
                self.fail_resume_for_action(&reply.action, reply.sequence, &error.to_string());
                self.network_after = Instant::now() + Duration::from_secs(2);
                return Err(error);
            }
        };
        match (&reply.action, &response) {
            (Action::List { after, limit }, Response::Canvases { items, next }) => self.directory_page(reply.sequence, *after, *limit, items, *next)?,
            (Action::List { .. }, _) => {
                self.directory_scan = None;
                return Err(other("Service returned an incompatible canvas directory"));
            }
            (
                Action::Create { .. }
                | Action::Info { .. }
                | Action::Accept { .. }
                | Action::RestoreOriginal { .. }
                | Action::ImportBegin { .. }
                | Action::ImportCommit { .. },
                Response::Canvas { info },
            ) => {
                if matches!(&reply.action, Action::Create {canvas,..} | Action::Info {canvas} | Action::Accept { canvas, .. } | Action::RestoreOriginal { canvas, .. } | Action::ImportBegin { new_canvas: canvas, .. } if *canvas != info.canvas)
                {
                    return Err(other("Service returned a different canvas"));
                }
                if matches!(reply.action, Action::ImportBegin { .. } | Action::ImportCommit { .. }) && info.owner != lease.scope().profile {
                    return Err(other("Imported canvas has a different owner"));
                }
                let unchanged = self.replicas.get(&info.canvas).is_some_and(|replica| replica.complete() && replica.info() == info);
                if matches!(reply.action, Action::Info { .. })
                    && let Some(resume) = self.recovery_resume.get_mut(&info.canvas)
                    && resume.sequence == reply.sequence
                {
                    resume.refreshed = true;
                }
                if let Some(replica) = self.replicas.get_mut(&info.canvas) {
                    replica.update_info(info.clone())?;
                } else {
                    let cached = self.queue.load_snapshot(&lease, info.canvas)?;
                    self.replicas.insert(info.canvas, WorldReplica::open(info.clone(), cached)?);
                }
                self.directory_refreshed(info.canvas);
                self.presence_verified(info.canvas);
                let Some(replica) = self.replicas.get(&info.canvas) else { return Err(other("Canvas state unavailable")) };
                if !replica.complete() {
                    self.requests.push_front((
                        0,
                        Action::Events { canvas: info.canvas, after: replica.accepted().revision, through: Some(info.revision), limit: wire::MAX_PAGE },
                    ));
                } else if !unchanged || self.recovery_resume.get(&info.canvas).is_some_and(|resume| resume.refreshed) {
                    self.ready(info.canvas)?;
                }
            }
            (Action::Events { canvas, .. }, Response::Events { through, next, .. }) => {
                let replica = self.replicas.get_mut(canvas).ok_or_else(|| other("Canvas changed before event page arrived"))?;
                replica.apply_page(&response)?;
                if let Some(after) = next {
                    self.requests.push_front((0, Action::Events { canvas: *canvas, after: *after, through: Some(*through), limit: wire::MAX_PAGE }));
                } else {
                    self.ready(*canvas)?;
                }
            }
            (
                Action::Append { operation } | Action::RemixGame { operation, .. } | Action::RemixCanvas { operation, .. } | Action::RemixStroke { operation },
                Response::Accepted { event },
            ) => {
                if event.op != *operation {
                    return Err(other("Service acknowledged different canvas content; local edit retained"));
                }
                self.presence_committed(operation, event.revision);
                let replica = self.replicas.get_mut(&operation.canvas).ok_or_else(|| other("Canvas changed before upload finished"))?;
                if replica.append_contiguous(event)? {
                    self.ready(operation.canvas)?;
                }
                // Always catch up before another upload: a noncontiguous ACK is not a local-history commit.
                self.requests.push_front((0, Action::Info { canvas: operation.canvas }));
            }
            (Action::Leave { canvas } | Action::Delete { canvas, .. }, Response::Done) => {
                self.queue.revoke(&lease, *canvas)?;
                self.replicas.remove(canvas);
                if self.selected == Some(*canvas) {
                    self.selected = None;
                    self.invalidate_presence()?;
                }
                self.outbound.push_back(Event::Revoked { canvas: *canvas });
            }
            _ => {}
        }
        if reply.sequence != 0
            || !matches!(
                reply.action,
                Action::Info { .. }
                    | Action::Events { .. }
                    | Action::Append { .. }
                    | Action::RemixGame { .. }
                    | Action::RemixCanvas { .. }
                    | Action::RemixStroke { .. }
            )
        {
            self.outbound.push_back(Event::Response { sequence: reply.sequence, response });
        }
        Ok(())
    }
    fn fail_resume_for_action(&mut self, action: &Action, sequence: u64, message: &str) {
        let canvas = match action {
            Action::Info { canvas } | Action::Events { canvas, .. } => Some(*canvas),
            _ => None,
        };
        if let Some(canvas) = canvas
            && self.recovery_resume.get(&canvas).is_some_and(|resume| resume.sequence == sequence || resume.refreshed)
            && let Some(resume) = self.recovery_resume.remove(&canvas)
            && resume.sequence != sequence
        {
            self.outbound.push_back(Event::Error { sequence: resume.sequence, message: message.into() });
        }
    }
    fn schedule(&mut self, sender: &SyncSender<Job>) -> Result<()> {
        if !self.auth_fence.allowed(self.auth_ticket) {
            return Ok(());
        }
        if self.inflight || self.access.is_none() || Instant::now() < self.network_after {
            return Ok(());
        }
        let lease = self.lease()?.clone();
        self.queue.ensure_current(&lease)?;
        let work = if let Some(work) = self.requests.pop_front() {
            Some(work)
        } else if let Some(queued) = self.queue.pending(&lease, 1)?.first() {
            if self.replicas.get(&queued.operation.canvas).is_some_and(WorldReplica::complete) {
                Some((0, upload_action(queued.operation.clone())?))
            } else {
                Some((0, Action::Info { canvas: queued.operation.canvas }))
            }
        } else if Instant::now() >= self.next_poll {
            self.next_poll = Instant::now() + Duration::from_millis(500);
            self.selected.map(|canvas| (0, Action::Info { canvas }))
        } else {
            None
        };
        let Some((sequence, action)) = work else { return Ok(()) };
        let Some(access) = &self.access else { return Ok(()) };
        sender
            .try_send(Job {
                ticket: self.auth_ticket,
                auth: access.auth.clone(),
                generation: self.generation,
                sequence,
                endpoint: access.scope.endpoint.clone(),
                credential: access.credential.clone(),
                action,
            })
            .map_err(|_| Error::Busy)?;
        self.inflight = true;
        Ok(())
    }
}

fn run(
    database: PathBuf,
    trusted_certificate: Option<Vec<u8>>,
    commands: Receiver<Envelope>,
    events: &SyncSender<Stamped<Event>>,
    auth_fence: Arc<AuthFence>,
    priority: Arc<Priority<Envelope>>,
) -> Result<()> {
    let queue = WorldOutbox::open(database)?;
    let transport = Transport::new(trusted_certificate.as_deref())?;
    let presence_pool = presence::Pool::start(trusted_certificate.as_deref(), Arc::clone(&auth_fence))?;
    let (sender, network) = mpsc::sync_channel::<Job>(1);
    let (replies, receive) = mpsc::sync_channel(1);
    let network_fence = Arc::clone(&auth_fence);
    let network_thread = std::thread::Builder::new()
        .name("komaphone-world-http".into())
        .spawn(move || {
            while let Ok(job) = network.recv() {
                let result = job.call(&transport, &network_fence);
                if replies.send(Reply { generation: job.generation, sequence: job.sequence, action: job.action, result }).is_err() {
                    break;
                }
            }
        })
        .map_err(|e| Error::Storage(e.to_string()))?;
    let now = Instant::now();
    let mut state = Coordinator {
        auth_ticket: auth_fence.ticket(),
        auth_fence: Arc::clone(&auth_fence),
        queue,
        access: None,
        lease: None,
        generation: 0,
        replicas: BTreeMap::new(),
        selected: None,
        requests: VecDeque::new(),
        recovery_resume: BTreeMap::new(),
        outbound: VecDeque::new(),
        inflight: false,
        next_poll: now,
        network_after: now,
        cached_directory: None,
        directory_start: None,
        directory_scan: None,
        presence: presence::Lane::default(),
    };
    loop {
        if !auth_fence.current(state.auth_ticket) {
            state.auth_ticket = auth_fence.ticket();
            if let Err(error) = state.access(0, None) {
                state.access = None;
                state.lease = None;
                state.requests.clear();
                state.outbound.retain(|event| matches!(event, Event::Saved { .. } | Event::ViewSaved { .. }));
                state.outbound.push_back(Event::Error { sequence: 0, message: error.to_string() });
            }
        }
        let immediate = priority.take();
        if immediate.is_some() || state.outbound.len() < 32 {
            match immediate.map(Ok).unwrap_or_else(|| commands.recv_timeout(Duration::from_millis(10))) {
                Ok(envelope) => {
                    if !auth_fence.current(envelope.ticket) {
                        continue;
                    }
                    if state.auth_ticket >> 1 != envelope.ticket >> 1 {
                        let _ = state.access(0, None);
                    }
                    state.auth_ticket = envelope.ticket;
                    let result = match envelope.command {
                        Command::Relocate { relocation, access } => (|| {
                            state.access(envelope.sequence, None)?;
                            let identity = access.verify_identity(trusted_certificate.as_deref())?;
                            if !auth_fence.current(envelope.ticket) {
                                return Err(other("Service move canceled by account change"));
                            }
                            state.queue.relocate_endpoint(&relocation, &identity)?;
                            state.outbound.push_back(Event::Relocated { sequence: envelope.sequence, id: relocation.id });
                            Ok(())
                        })(),
                        Command::Access(access) => {
                            let active = access.is_some();
                            let result = state.access(envelope.sequence, access);
                            if result.is_ok() && active && !auth_fence.resume(envelope.ticket) {
                                let _ = state.access(envelope.sequence, None);
                            }
                            result
                        }
                        command => {
                            if auth_fence.allowed(envelope.ticket) {
                                state.command(envelope.sequence, command)
                            } else {
                                Err(other("Canvas account is paused; reconnect before sharing"))
                            }
                        }
                    };
                    if let Err(error) = result {
                        state.outbound.push_back(Event::Error { sequence: envelope.sequence, message: error.to_string() });
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Ok(reply) = receive.try_recv() {
            let sequence = reply.sequence;
            if !auth_fence.allowed(state.auth_ticket) {
                state.inflight = false;
            } else if let Err(error) = state.reply(reply) {
                state.outbound.push_back(Event::Error { sequence, message: error.to_string() });
            }
        }
        for _ in 0..presence::SLOTS {
            if state.outbound.len() >= 32 {
                break;
            }
            let Some((slot, reply)) = presence_pool.poll() else { break };
            state.presence_reply(slot, reply, Instant::now());
        }
        if state.outbound.len() < 32
            && let Err(error) = state.schedule_presence(presence_pool.senders(), Instant::now())
        {
            state.outbound.push_back(Event::Error { sequence: 0, message: error.to_string() });
        }
        if state.outbound.len() < 32
            && let Err(error) = state.cached_directory_page()
        {
            state.outbound.push_back(Event::Error { sequence: 0, message: error.to_string() });
        }
        if state.outbound.len() < 32
            && let Err(error) = state.schedule(&sender)
        {
            state.outbound.push_back(Event::Error { sequence: 0, message: error.to_string() });
            state.network_after = Instant::now() + Duration::from_secs(2);
        }
        while let Some(event) = state.outbound.pop_front() {
            match events.try_send(Stamped { ticket: state.auth_ticket, value: event }) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(event)) => {
                    state.outbound.push_front(event.value);
                    break;
                }
                Err(mpsc::TrySendError::Disconnected(_)) => return Ok(()),
            }
        }
    }
    drop(sender);
    drop(receive);
    drop(presence_pool);
    network_thread.join().map_err(|_| Error::Unavailable)?;
    Ok(())
}

#[cfg(test)]
mod tests;
