use crate::store::random_hex;
use crate::{ClientStatus, CredentialStore, Error, FreezePin, Invitation, Outbox, Result, Transport, WindowsCredentials};
use photocraft_party::contracts::{MAX_ASSET_BYTES, MAX_PROJECT_BYTES};
use photocraft_party::protocol::{CHUNK_BYTES, Operation, PROTOCOL_VERSION, RpcRequest, RpcResponse};
use photocraft_party::room::Action;
use photocraft_party::{AssetHash, GameId, Mode, ProjectId, RequestId, RoomId, Submission};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::time::{Duration, Instant};
pub(crate) mod fence;
use fence::{AuthFence, Priority, Stamped};

/// Background requests have no Debug implementation because invitations are capabilities.
pub enum ClientCommand {
    SignIn {
        endpoint: String,
        display_name: String,
        admission: Option<String>,
    },
    Relocate {
        endpoint: String,
        display_name: String,
    },
    CompleteRelocation {
        id: RequestId,
    },
    CancelSignIn,
    SignOut,
    SelectAccount {
        slot: Option<String>,
    },
    /// In-process credential handoff; never serialized or exposed through status.
    WorldAccess,
    Connect {
        endpoint: String,
        display_name: String,
    },
    Create {
        mode: Mode,
        timer_ms: Option<u64>,
    },
    Join {
        room: RoomId,
        invitation: String,
    },
    Invite {
        expires_in_ms: u64,
        uses: u8,
    },
    Revoke {
        invitation_id: RequestId,
    },
    Action(Box<Action>),
    Start,
    /// Byte objects must include every manifest reference that is not already uploaded.
    /// This API does not turn an unsaved editor document into a durable submission.
    Submit {
        submission: Box<Submission>,
        assets: Vec<Vec<u8>>,
        freeze: bool,
    },
    /// Explicitly queue one bounded protocol operation for headless integration/automation.
    Enqueue(Operation),
    Disconnect,
    Retry,
    RetainRejected,
    PinFreeze(FreezePin),
    SaveNote(crate::NoteDraft),
    SaveReveal(crate::RevealPosition),
    Notes,
    History {
        room: RoomId,
        after_revision: u64,
    },
    DownloadGame {
        room: RoomId,
        game: GameId,
        timestamp: String,
    },
    CancelDownload,
    GameNotes {
        room: RoomId,
        game: GameId,
    },
    ImageAck {
        room: RoomId,
        hash: AssetHash,
    },
}

pub enum ClientEvent {
    RelocationReady {
        handled_sequence: u64,
        relocation: crate::AccountRelocation,
        access: crate::world_worker::WorldAccess,
    },
    /// A locally constructed Google authorization URL, never a serialized status field.
    OpenBrowser {
        handled_sequence: u64,
        url: String,
    },
    WorldAccess {
        handled_sequence: u64,
        access: Option<crate::world_worker::WorldAccess>,
    },
    Status {
        handled_sequence: u64,
        status: Box<ClientStatus>,
    },
    Invitation(Invitation),
    Image {
        room: RoomId,
        asset: photocraft_party::AssetRef,
        bytes: std::sync::Arc<Vec<u8>>,
    },
    Notes {
        room: RoomId,
        game: GameId,
        epoch: u64,
        notes: Vec<(photocraft_party::AssignmentId, String)>,
    },
    MediaError(String),
    History {
        room: RoomId,
        games: Vec<photocraft_party::protocol::ArchivedGameSummary>,
        next_revision: Option<u64>,
    },
    Gallery(Box<photocraft_party::gallery::GalleryBundle>),
    GalleryProgress {
        received: u64,
        total: u64,
        waiting: bool,
    },
    GalleryError(String),
    GalleryNotes {
        game: GameId,
        notes: Vec<(photocraft_party::AssignmentId, String)>,
    },
}

#[derive(Default)]
pub struct ClientOptions {
    pub trusted_certificate_pem: Option<Vec<u8>>,
}

impl ClientOptions {
    /// Optional private CA trust is explicit and bounded; certificate verification stays on.
    pub fn from_environment() -> Result<Self> {
        let Some(path) = std::env::var_os("KOMAPHONE_TRUSTED_CERT") else { return Ok(Self::default()) };
        Self::with_trusted_certificate_file(std::path::Path::new(&path))
    }
    pub fn with_trusted_certificate_file(path: &std::path::Path) -> Result<Self> {
        use std::io::Read;
        let file = std::fs::File::open(path).map_err(|e| Error::Invalid(format!("Cannot open KOMAPHONE_TRUSTED_CERT: {e}")))?;
        if !file.metadata().map_err(|e| Error::Invalid(e.to_string()))?.is_file() {
            return Err(Error::Invalid("KOMAPHONE_TRUSTED_CERT must be a PEM file".into()));
        }
        let mut bytes = Vec::new();
        file.take(65_537).read_to_end(&mut bytes).map_err(|e| Error::Invalid(format!("Cannot read KOMAPHONE_TRUSTED_CERT: {e}")))?;
        if bytes.is_empty() || bytes.len() > 65_536 {
            return Err(Error::Invalid("KOMAPHONE_TRUSTED_CERT must contain 1–65536 bytes".into()));
        }
        let _ = Transport::new(Some(&bytes))?;
        Ok(Self { trusted_certificate_pem: Some(bytes) })
    }
}

pub struct Worker {
    alive: Arc<AtomicBool>,
    tx: SyncSender<CommandEnvelope>,
    rx: Receiver<Stamped<ClientEvent>>,
    sequence: AtomicU64,
    login_cancel: Arc<AtomicBool>,
    canceled_through: Arc<AtomicU64>,
    auth_fence: Arc<AuthFence>,
    priority: Arc<Priority<CommandEnvelope>>,
    completion: Arc<Priority<CommandEnvelope>>,
    signout_requested: Arc<AtomicBool>,
    status: Arc<Priority<Stamped<ClientEvent>>>,
}
struct RoomControl {
    login_cancel: Arc<AtomicBool>,
    auth_fence: Arc<AuthFence>,
    priority: Arc<Priority<CommandEnvelope>>,
    completion: Arc<Priority<CommandEnvelope>>,
    signout_requested: Arc<AtomicBool>,
    status: Arc<Priority<Stamped<ClientEvent>>>,
}
struct CommandEnvelope {
    sequence: u64,
    ticket: u64,
    command: ClientCommand,
}
impl Worker {
    pub fn start(database: PathBuf, options: ClientOptions) -> Result<Self> {
        Self::start_with_credentials(database, options, Box::new(WindowsCredentials))
    }
    pub fn start_with_credentials(database: PathBuf, options: ClientOptions, credentials: Box<dyn CredentialStore>) -> Result<Self> {
        let (tx, commands) = mpsc::sync_channel(8);
        let (events, rx) = mpsc::sync_channel(8);
        let login_cancel = Arc::new(AtomicBool::new(false));
        let cancel_worker = Arc::clone(&login_cancel);
        let canceled_through = Arc::new(AtomicU64::new(0));
        let completion = Arc::new(Priority::default());
        let worker_completion = Arc::clone(&completion);
        let auth_fence = Arc::new(AuthFence::default());
        let priority = Arc::new(Priority::default());
        let worker_fence = Arc::clone(&auth_fence);
        let worker_priority = Arc::clone(&priority);
        let signout_requested = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let running = Arc::clone(&alive);
        let status = Arc::new(Priority::default());
        let control = RoomControl {
            login_cancel: cancel_worker,
            auth_fence: worker_fence,
            priority: worker_priority,
            completion: worker_completion,
            signout_requested: Arc::clone(&signout_requested),
            status: Arc::clone(&status),
        };
        std::thread::Builder::new()
            .name("komaphone-rooms".into())
            .spawn(move || {
                run(database, options, credentials, commands, events, control);
                running.store(false, Ordering::Release);
            })
            .map_err(|e| Error::Storage(format!("Cannot start room worker: {e}")))?;
        Ok(Self { tx, rx, sequence: AtomicU64::new(1), login_cancel, canceled_through, auth_fence, priority, completion, signout_requested, status, alive })
    }
    pub fn pause_authenticated(&self) {
        self.auth_fence.pause();
        self.login_cancel.store(true, Ordering::Release);
    }
    pub fn send(&self, command: ClientCommand) -> Result<u64> {
        if !self.alive.load(Ordering::Acquire) {
            self.pause_authenticated();
            return Err(Error::Unavailable);
        }
        let sequence = self.sequence.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| value.checked_add(1)).map_err(|_| Error::Busy)?;
        let transition = matches!(
            command,
            ClientCommand::CancelSignIn
                | ClientCommand::SignOut
                | ClientCommand::SelectAccount { .. }
                | ClientCommand::SignIn { .. }
                | ClientCommand::Relocate { .. }
                | ClientCommand::Connect { .. }
                | ClientCommand::Disconnect
        );
        if transition {
            if matches!(command, ClientCommand::SignOut) {
                self.signout_requested.store(true, Ordering::Release);
            }
            self.canceled_through.fetch_max(sequence, Ordering::AcqRel);
            self.pause_authenticated();
        }
        let envelope = CommandEnvelope { sequence, ticket: self.auth_fence.ticket(), command };
        if transition {
            self.priority.put(envelope);
        } else if matches!(envelope.command, ClientCommand::CompleteRelocation { .. }) {
            self.completion.put(envelope);
        } else {
            self.tx.try_send(envelope).map_err(|_| Error::Busy)?;
        }
        Ok(sequence)
    }
    pub fn poll(&self) -> std::result::Result<ClientEvent, TryRecvError> {
        loop {
            let stamped = self.status.take().map(Ok).unwrap_or_else(|| self.rx.try_recv())?;
            if self.auth_fence.current(stamped.ticket)
                && (!self.auth_fence.paused()
                    || matches!(
                        stamped.value,
                        ClientEvent::Status { .. }
                            | ClientEvent::OpenBrowser { .. }
                            | ClientEvent::RelocationReady { .. }
                            | ClientEvent::WorldAccess { access: None, .. }
                    ))
            {
                return Ok(stamped.value);
            }
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.canceled_through.store(u64::MAX, Ordering::Release);
        self.login_cancel.store(true, Ordering::Release);
        self.auth_fence.stop();
    }
}

#[allow(
    clippy::cognitive_complexity,
    reason = "The coordinator keeps command authorization and account transition ordering explicit in one bounded event loop"
)]
fn run(
    database: PathBuf,
    options: ClientOptions,
    credentials: Box<dyn CredentialStore>,
    commands: Receiver<CommandEnvelope>,
    event_tx: SyncSender<Stamped<ClientEvent>>,
    control: RoomControl,
) {
    let RoomControl { login_cancel, auth_fence, priority, completion, signout_requested, status } = control;
    let mut events = RoomEvents { tx: event_tx, ticket: auth_fence.ticket(), fence: Arc::clone(&auth_fence), status };
    let opened =
        Outbox::open(database, credentials).and_then(|store| Transport::new(options.trusted_certificate_pem.as_deref()).map(|transport| (store, transport)));
    let (mut store, transport) = match opened {
        Ok(value) => value,
        Err(error) => {
            let _ = events.try_send(ClientEvent::Status {
                handled_sequence: 0,
                status: Box::new(ClientStatus { error: Some(error.to_string()), ..ClientStatus::default() }),
            });
            return;
        }
    };
    let mut handled_sequence = 0;
    let mut downloads = crate::download::Downloads::default();
    let mut gallery = crate::gallery_download::Downloads::default();
    let mut pending_notes = None;
    let mut pending_media = std::collections::VecDeque::new();
    let mut pending_access = None;
    let mut pending_relocation = store
        .relocation_access()
        .ok()
        .flatten()
        .map(|(relocation, access)| (0, relocation, access.with_auth_fence(Arc::clone(&auth_fence), events.ticket, true)));
    publish(&store, &events, handled_sequence);
    let mut pending_invitation = store.invitation().ok().flatten();
    let mut next_network = Instant::now();
    let mut failures = 0u32;
    let mut last_poll = Instant::now().checked_sub(Duration::from_secs(1)).unwrap_or_else(Instant::now);
    let mut observed_ticket = auth_fence.ticket();
    let mut relocation_ticket = pending_relocation.as_ref().map(|_| observed_ticket);
    loop {
        let ticket = auth_fence.ticket();
        if auth_fence.stopped() {
            // A process close fences old responses but preserves a resumable
            // session. A preceding explicit account action must remain paused.
            finish_shutdown(&mut store, &options, &auth_fence, &signout_requested);
            break;
        }
        if ticket >> 1 != observed_ticket >> 1 {
            observed_ticket = ticket;
            events.ticket = ticket;
            gallery.cancel();
            downloads = crate::download::Downloads::default();
            pending_notes = None;
            pending_invitation = None;
            pending_access = None;
            pending_relocation = None;
            relocation_ticket = None;
            pending_media.clear();
            let _ = store.stop_polling();
        }
        if let Some((handled_sequence, relocation, access)) = pending_relocation.take()
            && let Err(mpsc::TrySendError::Full(ClientEvent::RelocationReady { handled_sequence, relocation, access })) =
                events.try_send(ClientEvent::RelocationReady { handled_sequence, relocation, access })
        {
            pending_relocation = Some((handled_sequence, relocation, access));
        }
        if let Some((handled_sequence, access)) = pending_access.take()
            && let Err(mpsc::TrySendError::Full(ClientEvent::WorldAccess { handled_sequence, access })) =
                events.try_send(ClientEvent::WorldAccess { handled_sequence, access })
        {
            pending_access = Some((handled_sequence, access));
        }
        if store.status().invitation_id.is_none() {
            pending_invitation = None;
        }
        if let Some(invitation) = pending_invitation.take()
            && let Err(mpsc::TrySendError::Full(ClientEvent::Invitation(invitation))) = events.try_send(ClientEvent::Invitation(invitation))
        {
            pending_invitation = Some(invitation);
        }
        match priority.take().or_else(|| completion.take()).map(Ok).unwrap_or_else(|| commands.recv_timeout(Duration::from_millis(50))) {
            Ok(envelope) => {
                if !auth_fence.current(envelope.ticket) {
                    continue;
                }
                if observed_ticket >> 1 != envelope.ticket >> 1 {
                    gallery.cancel();
                    downloads = crate::download::Downloads::default();
                    pending_notes = None;
                    pending_invitation = None;
                    pending_access = None;
                    pending_relocation = None;
                    relocation_ticket = None;
                    pending_media.clear();
                    let _ = store.stop_polling();
                }
                observed_ticket = envelope.ticket;
                events.ticket = envelope.ticket;
                let transport = transport.with_auth_fence(Arc::clone(&auth_fence), envelope.ticket);
                let cleanup = consume_signout(&mut store, &options, &signout_requested);
                let result = if let Err(error) = cleanup {
                    Err(error)
                } else if !auth_fence.current(envelope.ticket) {
                    continue;
                } else {
                    match envelope.command {
                        command @ (ClientCommand::SignIn { .. } | ClientCommand::Relocate { .. }) => {
                            let (endpoint, display_name, admission, relocating) = match command {
                                ClientCommand::SignIn { endpoint, display_name, admission } => {
                                    (endpoint, display_name, admission, store.status().relocation.is_some())
                                }
                                ClientCommand::Relocate { endpoint, display_name } => (endpoint, display_name, None, true),
                                _ => {
                                    continue;
                                }
                            };
                            gallery.cancel();
                            downloads = crate::download::Downloads::default();
                            pending_notes = None;
                            pending_invitation = None;
                            pending_access = None;
                            pending_relocation = None;
                            login_cancel.store(false, Ordering::Release);
                            let result = (|| {
                                if !auth_fence.current(envelope.ticket) {
                                    return Err(Error::Invalid("Sign-in canceled; local drawings are unchanged".into()));
                                }
                                let login = if relocating {
                                    store.prepare_hosted_relocation(&endpoint, &display_name)?
                                } else {
                                    store.prepare_hosted_login(&endpoint, &display_name, admission)?
                                };
                                publish(&store, &events, envelope.sequence);
                                let client = crate::hosted_auth::Client::from_installed_app(options.trusted_certificate_pem.as_deref())?;
                                let session = client.sign_in(login, &login_cancel, |url| {
                                    events.try_send(ClientEvent::OpenBrowser { handled_sequence: envelope.sequence, url: url.into() }).map_err(|_| Error::Busy)
                                })?;
                                if login_cancel.load(Ordering::Acquire) {
                                    let _ = client.revoke(&endpoint, &session.token);
                                    return Err(Error::Invalid("Sign-in canceled; local drawings are unchanged".into()));
                                }
                                let installed =
                                    if relocating { store.stage_hosted_relocation(&session) } else { store.install_hosted_session(&endpoint, &session) };
                                if installed.is_err() {
                                    let _ = client.revoke(&endpoint, &session.token);
                                }
                                installed?;
                                if login_cancel.load(Ordering::Acquire) || !auth_fence.current(envelope.ticket) {
                                    let _ = store.sign_out_local();
                                    let _ = client.revoke(&endpoint, &session.token);
                                    return Err(Error::Invalid("Sign-in canceled; local drawings are unchanged".into()));
                                }
                                if relocating {
                                    relocation_ticket = Some(envelope.ticket);
                                    pending_relocation = store.relocation_access()?.map(|(relocation, access)| {
                                        (envelope.sequence, relocation, access.with_auth_fence(Arc::clone(&auth_fence), envelope.ticket, true))
                                    });
                                } else if !auth_fence.resume(envelope.ticket) {
                                    let _ = store.sign_out_local();
                                    let _ = client.revoke(&endpoint, &session.token);
                                    return Err(Error::Invalid("Sign-in superseded by a newer account action".into()));
                                }
                                Ok(())
                            })();
                            if result.is_err() {
                                let _ = store.cancel_hosted_login();
                            }
                            result
                        }
                        ClientCommand::CancelSignIn => {
                            let revocation = store.session_for_revocation().ok().flatten();
                            let result = store.cancel_hosted_login();
                            if let Some((endpoint, token)) = revocation
                                && let Ok(client) = crate::hosted_auth::Client::new(options.trusted_certificate_pem.as_deref())
                            {
                                let _ = client.revoke(&endpoint, &token);
                            }
                            result
                        }
                        ClientCommand::CompleteRelocation { id } => {
                            let result = if relocation_ticket.is_some_and(|ticket| ticket >> 1 == envelope.ticket >> 1) {
                                store.complete_hosted_relocation(id)
                            } else {
                                Err(Error::Invalid("Service move acknowledgment was superseded by an account action; sign in to resume".into()))
                            };
                            result.and_then(|()| activate_completed_relocation(&mut store, &options, &auth_fence, envelope.ticket))
                        }
                        ClientCommand::SignOut => {
                            let revocation = store.session_for_revocation().ok().flatten();
                            let result = store.sign_out_local();
                            publish(&store, &events, envelope.sequence);
                            gallery.cancel();
                            downloads = crate::download::Downloads::default();
                            pending_notes = None;
                            pending_invitation = None;
                            pending_access = Some((envelope.sequence, None));
                            pending_relocation = None;
                            if let Some((endpoint, token)) = revocation
                                && let Ok(client) = crate::hosted_auth::Client::new(options.trusted_certificate_pem.as_deref())
                            {
                                let _ = client.revoke(&endpoint, &token);
                            }
                            result
                        }
                        ClientCommand::SelectAccount { slot } => {
                            let previous_slot = store.status().account_slot;
                            let revocation = store.session_for_revocation().ok().flatten();

                            let result = store.select_account(slot.as_deref());
                            if result.is_ok() {
                                gallery.cancel();
                                downloads = crate::download::Downloads::default();
                                pending_notes = None;
                                pending_invitation = None;
                                pending_access = Some((envelope.sequence, None));
                                publish(&store, &events, envelope.sequence);
                                if previous_slot != store.status().account_slot
                                    && let Some((endpoint, token)) = revocation
                                    && let Ok(client) = crate::hosted_auth::Client::new(options.trusted_certificate_pem.as_deref())
                                {
                                    let _ = client.revoke(&endpoint, &token);
                                }
                            }
                            result
                        }
                        ClientCommand::WorldAccess => {
                            if store.status().relocation.is_some() {
                                store.relocation_access().map(|ready| {
                                    pending_relocation = ready.filter(|_| relocation_ticket.is_some_and(|ticket| ticket >> 1 == envelope.ticket >> 1)).map(
                                        |(relocation, access)| {
                                            (envelope.sequence, relocation, access.with_auth_fence(Arc::clone(&auth_fence), envelope.ticket, true))
                                        },
                                    );
                                })
                            } else {
                                crate::world_worker::WorldAccess::from_room(&store).map(|access| {
                                    pending_access =
                                        Some((envelope.sequence, access.map(|access| access.with_auth_fence(Arc::clone(&auth_fence), envelope.ticket, false))))
                                })
                            }
                        }
                        ClientCommand::Notes => crate::download::notes(&store, &transport).map(|event| pending_notes = Some(event)),
                        ClientCommand::History { room, after_revision } => {
                            crate::gallery_download::history(&store, &transport, room, after_revision).map(|event| pending_notes = Some(event))
                        }
                        ClientCommand::DownloadGame { room, game, timestamp } => gallery.start(&store, room, game, timestamp),
                        ClientCommand::CancelDownload => {
                            gallery.cancel();
                            Ok(())
                        }
                        ClientCommand::GameNotes { room, game } => {
                            crate::gallery_download::notes(&store, &transport, room, game).map(|event| pending_notes = Some(event))
                        }
                        ClientCommand::ImageAck { room, hash } => {
                            downloads.acknowledge(room, hash);
                            Ok(())
                        }
                        ClientCommand::Connect { endpoint, display_name } => {
                            let result = store.configure(&endpoint, &display_name);
                            if result.is_ok() && !auth_fence.resume(envelope.ticket) {
                                let _ = store.stop_polling();
                            }
                            result
                        }
                        command => {
                            if auth_fence.paused() && !matches!(command, ClientCommand::Disconnect | ClientCommand::CancelDownload) {
                                Err(Error::Invalid("Account network work is paused; sign in or reconnect explicitly".into()))
                            } else {
                                handle(&mut store, command)
                            }
                        }
                    }
                };
                if let Err(error) = result
                    && !auth_fence.stopped()
                {
                    let _ = store.mark_error(&error, None, false);
                }
                handled_sequence = handled_sequence.max(envelope.sequence);
                next_network = Instant::now();
                publish(&store, &events, handled_sequence);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                finish_shutdown(&mut store, &options, &auth_fence, &signout_requested);
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        // A queued cancellation/sign-out takes priority over starting another
        // authenticated download or outbox request. An already in-flight request
        // may finish, but cannot authorize a subsequent old-account request.
        if auth_fence.paused() || !auth_fence.current(events.ticket) {
            continue;
        }
        let transport = transport.with_auth_fence(Arc::clone(&auth_fence), events.ticket);
        if let Some(event) = pending_notes.take()
            && let Err(mpsc::TrySendError::Full(event)) = events.try_send(event)
        {
            pending_notes = Some(event);
        }
        if pending_media.is_empty() {
            let (media_tx, media_rx) = mpsc::sync_channel(8);
            if let Err(error) = downloads.step(&store, &transport, &media_tx) {
                let _ = events.try_send(ClientEvent::MediaError(error.to_string()));
            }
            if auth_fence.allowed(events.ticket) {
                gallery.step(&store, &transport, &media_tx);
            }
            while let Ok(event) = media_rx.try_recv() {
                pending_media.push_back(event);
            }
        }
        while let Some(event) = pending_media.pop_front() {
            if let Err(mpsc::TrySendError::Full(event)) = events.try_send(event) {
                pending_media.push_front(event);
                break;
            }
        }
        if Instant::now() < next_network {
            continue;
        }
        match network_step(&mut store, &transport, &mut last_poll, &auth_fence) {
            Ok(invitation) => {
                failures = 0;
                if let Some(invitation) = invitation {
                    pending_invitation = Some(invitation);
                }
                let status = store.status();
                let delay = if status.pending > 0 && !status.blocked {
                    10
                } else if status.room.is_some() {
                    250
                } else {
                    1000
                };
                next_network = Instant::now() + Duration::from_millis(delay);
            }
            Err(error) => {
                if auth_fence.stopped() {
                    continue;
                }
                failures = failures.saturating_add(1).min(5);
                let _ = store.mark_error(&error, None, false);
                next_network = Instant::now() + Duration::from_millis(250 * (1u64 << failures));
            }
        }
        publish(&store, &events, handled_sequence);
    }
}

struct RoomEvents {
    tx: SyncSender<Stamped<ClientEvent>>,
    ticket: u64,
    fence: Arc<AuthFence>,
    status: Arc<Priority<Stamped<ClientEvent>>>,
}
fn finish_shutdown(store: &mut Outbox, options: &ClientOptions, fence: &AuthFence, signout_requested: &AtomicBool) {
    let _ = consume_signout(store, options, signout_requested);
    if fence.pause_requested() {
        let _ = store.stop_polling();
    }
}
impl RoomEvents {
    #[allow(clippy::result_large_err, reason = "Match SyncSender ownership semantics without allocating on each bounded event handoff")]
    fn try_send(&self, value: ClientEvent) -> std::result::Result<(), mpsc::TrySendError<ClientEvent>> {
        if matches!(value, ClientEvent::Status { .. }) {
            self.status.put(Stamped { ticket: self.ticket, value });
            return Ok(());
        }
        self.tx.try_send(Stamped { ticket: self.ticket, value }).map_err(|error| match error {
            mpsc::TrySendError::Full(value) => mpsc::TrySendError::Full(value.value),
            mpsc::TrySendError::Disconnected(value) => mpsc::TrySendError::Disconnected(value.value),
        })
    }
}
fn sign_out_and_revoke(store: &mut Outbox, options: &ClientOptions) -> Result<()> {
    let revocation = store.session_for_revocation().ok().flatten();
    let result = store.sign_out_local();
    if let Some((endpoint, token)) = revocation
        && let Ok(client) = crate::hosted_auth::Client::new(options.trusted_certificate_pem.as_deref())
    {
        let _ = client.revoke(&endpoint, &token);
    }
    result
}
fn consume_signout(store: &mut Outbox, options: &ClientOptions, requested: &AtomicBool) -> Result<()> {
    if requested.swap(false, Ordering::AcqRel) { sign_out_and_revoke(store, options) } else { Ok(()) }
}

/// Runs only after the relocation transaction commits. A pause during that
/// commit must win before the worker may start using the destination session.
fn activate_completed_relocation(store: &mut Outbox, options: &ClientOptions, fence: &AuthFence, ticket: u64) -> Result<()> {
    if fence.resume(ticket) {
        return Ok(());
    }
    let _ = sign_out_and_revoke(store, options);
    Err(Error::Invalid("Service move completion was canceled by an account action".into()))
}

#[cfg(test)]
mod account_tests;
fn publish(store: &Outbox, events: &RoomEvents, handled_sequence: u64) {
    let mut status = store.status();
    if events.fence.paused() {
        status.connected = false;
        status.paired = false;
        status.signed_in = false;
        status.polling = false;
        status.room = None;
        status.invitation_id = None;
        status.freeze_pin = None;
        status.note_draft = None;
        status.reveal_position = None;
        status.game_drafts = None;
        status.submission_project = None;
    }
    let _ = events.try_send(ClientEvent::Status { handled_sequence, status: Box::new(status) });
}

fn room_and_revision(store: &Outbox) -> Result<(RoomId, u64)> {
    let status = store.status();
    if status.pending > 0 {
        return Err(Error::Busy);
    }
    status.room.map(|room| (room.id, room.revision)).ok_or_else(|| Error::Invalid("Create or join a room first".into()))
}

fn handle(store: &mut Outbox, command: ClientCommand) -> Result<()> {
    match command {
        ClientCommand::SignIn { .. }
        | ClientCommand::Relocate { .. }
        | ClientCommand::CompleteRelocation { .. }
        | ClientCommand::CancelSignIn
        | ClientCommand::SignOut
        | ClientCommand::SelectAccount { .. } => Err(Error::Invalid("Account actions require the native sign-in worker".into())),
        ClientCommand::WorldAccess => Err(Error::Invalid("Credential handoff requires the native account worker".into())),
        ClientCommand::Connect { endpoint, display_name } => store.configure(&endpoint, &display_name),
        ClientCommand::Create { mode, timer_ms } => {
            mode.validate().map_err(|e| Error::Invalid(e.to_string()))?;
            if store.status().pending > 0 {
                return Err(Error::Busy);
            }
            store.enqueue(Operation::Create { mode, timer_ms })?;
            store.resume_for_room_action()
        }
        ClientCommand::Join { room, invitation } => {
            if store.status().pending > 0 {
                return Err(Error::Busy);
            }
            store.enqueue(Operation::Join { room, invitation })?;
            store.resume_for_room_action()
        }
        ClientCommand::Invite { expires_in_ms, uses } => {
            let (room, _) = room_and_revision(store)?;
            store.enqueue(Operation::Invite { room, secret: random_hex(32)?, expires_in_ms, uses }).map(|_| ())
        }
        ClientCommand::Revoke { invitation_id } => {
            let (room, _) = room_and_revision(store)?;
            store.enqueue(Operation::RevokeInvite { room, invitation_id }).map(|_| ())
        }
        ClientCommand::Action(action) => {
            let is_leave = matches!(action.as_ref(), Action::Leave);
            let (room, expected_revision) = room_and_revision(store)?;
            store.enqueue(Operation::Apply { room, expected_revision, action })?;
            if is_leave {
                store.resume_for_room_action()?;
            }
            Ok(())
        }
        ClientCommand::Start => start(store),
        ClientCommand::Submit { submission, assets, freeze } => submit(store, *submission, assets, freeze),
        ClientCommand::Enqueue(operation) => store.enqueue(operation).map(|_| ()),
        ClientCommand::Disconnect => {
            let status = store.status();
            if let Some(room) = status.room {
                store.enqueue(Operation::Disconnect { room: room.id })?;
            }
            // Stop heartbeats immediately, even if the endpoint is unavailable. Pending
            // disconnect is retained for the next explicit reconnect.
            store.stop_polling()
        }
        ClientCommand::Retry => store.retry(),
        ClientCommand::RetainRejected => store.retain_rejected(),
        ClientCommand::PinFreeze(pin) => store.pin_freeze(pin),
        ClientCommand::SaveNote(note) => store.save_note(note),
        ClientCommand::SaveReveal(position) => store.save_reveal_position(position),
        ClientCommand::Notes => Err(Error::Invalid("Notes require an authenticated service request".into())),
        ClientCommand::History { .. } | ClientCommand::DownloadGame { .. } | ClientCommand::CancelDownload | ClientCommand::GameNotes { .. } => {
            Err(Error::Invalid("Gallery downloads require an authenticated service request".into()))
        }
        ClientCommand::ImageAck { .. } => Ok(()),
    }
}

fn start(store: &mut Outbox) -> Result<()> {
    let (room, expected_revision) = room_and_revision(store)?;
    let view = store.status().room.ok_or_else(|| Error::Invalid("Room is unavailable".into()))?;
    let rounds = view.mode.rounds(view.members.len()).map_err(|e| Error::Invalid(e.to_string()))?;
    let count = usize::from(rounds).saturating_mul(view.members.len());
    if count > 36 {
        return Err(Error::Invalid("Room assignment limit exceeded".into()));
    }
    let projects = (0..count).map(|_| ProjectId::new(&random_hex(16)?).map_err(|e| Error::Invalid(e.to_string()))).collect::<Result<Vec<_>>>()?;
    let game = GameId::new(&random_hex(16)?).map_err(|e| Error::Invalid(e.to_string()))?;
    store.enqueue(Operation::Apply { room, expected_revision, action: Box::new(Action::Start { game, projects }) }).map(|_| ())
}

fn submit(store: &mut Outbox, submission: Submission, assets: Vec<Vec<u8>>, freeze: bool) -> Result<()> {
    submission.validate().map_err(|e| Error::Invalid(e.to_string()))?;
    let (room, expected_revision) = room_and_revision(store)?;
    let status = store.status();
    let view = status.room.ok_or_else(|| Error::Invalid("Room is unavailable".into()))?;
    let assignment = view.assignment.as_ref().ok_or_else(|| Error::Invalid("No assignment is available".into()))?;
    if status.profile.as_ref().map(|p| p.id) != Some(submission.author)
        || assignment.assignment.id != submission.assignment
        || assignment.assignment.project != submission.project
        || assignment.submitted
    {
        return Err(Error::Invalid("Submission must use the current player's durable assigned project".into()));
    }
    if status.freeze_pin.as_ref().is_some_and(|pin| {
        pin.room == room
            && pin.epoch == view.epoch
            && pin.assignment == submission.assignment
            && pin.project == submission.project
            && pin.revision != submission.revision
    }) {
        return Err(Error::Invalid("This deadline already pinned a different acknowledged revision; submit the first pinned revision".into()));
    }
    let total = assets.iter().map(|bytes| bytes.len() as u64).fold(0u64, u64::saturating_add);
    if total > MAX_PROJECT_BYTES || assets.len() > 256 {
        return Err(Error::Invalid("Submission asset budget exceeded; the local draft is retained".into()));
    }
    let mut operations = Vec::new();
    if freeze {
        operations.push(Operation::Apply {
            room,
            expected_revision,
            action: Box::new(Action::FreezeReport { epoch: view.epoch, submission: submission.clone() }),
        });
    }
    for bytes in assets {
        if bytes.is_empty() || bytes.len() as u64 > MAX_ASSET_BYTES {
            return Err(Error::Invalid("Submission object must be 1 byte through 64 MiB".into()));
        }
        let hash = AssetHash::new(&format!("{:x}", Sha256::digest(&bytes))).map_err(|e| Error::Invalid(e.to_string()))?;
        let total = bytes.len() as u64;
        for (index, chunk) in bytes.chunks(CHUNK_BYTES).enumerate() {
            let offset = index.saturating_mul(CHUNK_BYTES) as u64;
            operations.push(Operation::Upload { room, hash, total, offset, bytes: chunk.to_vec(), finish: offset.saturating_add(chunk.len() as u64) == total });
            if operations.len() > 8190 {
                return Err(Error::Busy);
            }
        }
    }
    operations.push(Operation::Apply {
        room,
        expected_revision: expected_revision.saturating_add(u64::from(freeze)),
        action: Box::new(Action::Submit { epoch: view.epoch, submission }),
    });
    store.enqueue_batch(operations).map(|_| ())
}

fn network_step(store: &mut Outbox, transport: &Transport, last_poll: &mut Instant, fence: &AuthFence) -> Result<Option<Invitation>> {
    if !store.active() {
        return Ok(None);
    }
    let status = store.status();
    let Some(endpoint) = status.endpoint else { return Ok(None) };
    let credential = store.credential()?;
    if store.active()
        && status.paired
        && last_poll.elapsed() >= Duration::from_millis(250)
        && let Some(room) = status.room.as_ref()
    {
        let request = RpcRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(&random_hex(16)?).map_err(|e| Error::Invalid(e.to_string()))?,
            operation: Operation::Poll { room: room.id, after_revision: room.revision },
        };
        match transport.call(&endpoint, &credential, &request) {
            Ok(RpcResponse::Room { view, .. }) if view.id == room.id => {
                let revision = view.revision;
                store.update_poll(*view)?;
                let _ = store.refresh_pending_leave(room.id, revision, false)?;
            }
            Ok(_) => return Err(Error::Invalid("Service returned the wrong room view".into())),
            Err(Error::Rejected { code, .. }) if code == "unauthorized" && store.complete_already_left(room.id)? => {
                *last_poll = Instant::now();
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
        *last_poll = Instant::now();
    }
    store.ensure_transfer_freeze()?;
    if !store.status().blocked
        && let Some(room) = store.status().room.as_ref()
    {
        let _ = store.refresh_pending_leave(room.id, room.revision, false)?;
    }
    if !store.status().blocked
        && let Some(request) = store.next()?
    {
        match transport.call(&endpoint, &credential, &request) {
            Ok(response) => {
                store.acknowledge(&request, &response)?;
                if matches!(response, RpcResponse::Invitation { .. }) {
                    return store.invitation();
                }
            }
            Err(error) => {
                if fence.stopped() {
                    return Err(error);
                }
                if let Operation::Apply { room, action, .. } = &request.operation
                    && matches!(action.as_ref(), Action::Leave)
                    && matches!(&error, Error::Rejected { code, .. } if matches!(code.as_str(), "conflict" | "unauthorized"))
                {
                    let poll = RpcRequest {
                        protocol_version: PROTOCOL_VERSION,
                        request_id: RequestId::new(&random_hex(16)?).map_err(|e| Error::Invalid(e.to_string()))?,
                        operation: Operation::Poll { room: *room, after_revision: 0 },
                    };
                    match transport.call(&endpoint, &credential, &poll) {
                        Ok(RpcResponse::Room { view, .. }) if view.id == *room => {
                            *last_poll = Instant::now();
                            let revision = view.revision;
                            store.update_poll(*view)?;
                            if store.refresh_pending_leave(*room, revision, true)?
                                && let Some(rebound) = store.next()?
                            {
                                match transport.call(&endpoint, &credential, &rebound) {
                                    Ok(response) => {
                                        store.acknowledge(&rebound, &response)?;
                                        return Ok(None);
                                    }
                                    Err(retry_error) => {
                                        if fence.stopped() {
                                            return Err(retry_error);
                                        }
                                        let permanent = !matches!(retry_error, Error::Unavailable)
                                            && !matches!(
                                                &retry_error,
                                                Error::Rejected { code, .. } if matches!(code.as_str(), "busy" | "storage" | "account_changed")
                                            );
                                        store.mark_error(&retry_error, Some(rebound.request_id), permanent)?;
                                        return Err(retry_error);
                                    }
                                }
                            }
                        }
                        Err(Error::Rejected { code, .. }) if code == "unauthorized" && store.complete_already_left(*room)? => {
                            *last_poll = Instant::now();
                            return Ok(None);
                        }
                        _ => {}
                    }
                }
                let permanent = !matches!(error, Error::Unavailable)
                    && !matches!(&error, Error::Rejected { code, .. } if matches!(code.as_str(), "busy" | "storage"|"account_changed"));
                store.mark_error(&error, Some(request.request_id), permanent)?;
                return Err(error);
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_party::room::{Assignment, AssignmentView, Phase, Room};
    use photocraft_party::{AssetRef, AssignmentId, CheckpointId, CheckpointRef, ReplayId, ReplayRef, SubmissionId};
    struct FixtureCredentials;
    #[test]
    fn cancel_reaches_a_busy_login_without_waiting_for_the_command_queue() {
        let (tx, _commands) = mpsc::sync_channel(1);
        let (_events, rx) = mpsc::sync_channel(1);
        let canceled = Arc::new(AtomicBool::new(false));
        let through = Arc::new(AtomicU64::new(0));
        let worker = Worker {
            alive: Arc::new(AtomicBool::new(true)),
            tx,
            rx,
            sequence: AtomicU64::new(1),
            login_cancel: canceled.clone(),
            canceled_through: through.clone(),
            auth_fence: Arc::new(AuthFence::default()),
            priority: Arc::new(Priority::default()),
            completion: Arc::new(Priority::default()),
            signout_requested: Arc::new(AtomicBool::new(false)),
            status: Arc::new(Priority::default()),
        };
        worker.send(ClientCommand::WorldAccess).unwrap();
        assert_eq!(worker.send(ClientCommand::CancelSignIn).unwrap(), 2, "cancellation has a separate priority slot");
        let id = photocraft_party::RequestId::new(&"1".repeat(32)).unwrap();
        worker.send(ClientCommand::CompleteRelocation { id }).unwrap();
        assert!(matches!(worker.priority.take().unwrap().command, ClientCommand::CancelSignIn), "late completion cannot replace cancellation");
        assert!(matches!(worker.completion.take().unwrap().command, ClientCommand::CompleteRelocation { .. }));
        assert!(canceled.load(Ordering::Acquire), "cancellation still interrupts the callback worker");
        assert_eq!(through.load(Ordering::Acquire), 2, "an earlier queued sign-in must not reset cancellation");
        worker.send(ClientCommand::SignOut).unwrap();
        worker.send(ClientCommand::Connect { endpoint: "invalid".into(), display_name: "Replacement".into() }).unwrap();
        assert!(worker.signout_requested.load(Ordering::Acquire), "coalescing never discards sign-out cleanup");
        assert!(matches!(worker.priority.take().unwrap().command, ClientCommand::Connect { .. }));
        let events = RoomEvents { tx: _events, ticket: worker.auth_fence.ticket(), fence: Arc::clone(&worker.auth_fence), status: Arc::clone(&worker.status) };
        assert!(events.try_send(ClientEvent::MediaError("queue full".into())).is_ok());
        assert!(events.try_send(ClientEvent::Status { handled_sequence: 4, status: Box::default() }).is_ok());
        assert!(matches!(worker.poll().unwrap(), ClientEvent::Status { handled_sequence: 4, .. }), "transition status survives a full media channel");
        drop(worker);
        assert_eq!(through.load(Ordering::Acquire), u64::MAX, "closing the app cancels queued as well as active sign-in");
    }
    impl CredentialStore for FixtureCredentials {
        fn get(&self, _: &str, _: &str) -> Result<Option<String>> {
            Ok(Some("a".repeat(64)))
        }
        fn set(&self, _: &str, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn remove(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn wrong_pinned_revision_never_queues_artifacts_or_a_submission() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Outbox::open(root.path().join("client.sqlite3"), Box::new(FixtureCredentials)).unwrap();
        store.configure("http://127.0.0.1:43127", "Pin fixture").unwrap();
        let pair = store.next().unwrap().unwrap();
        let profile = store.status().profile.unwrap();
        store.acknowledge(&pair, &RpcResponse::Paired { participant: profile.clone() }).unwrap();
        let room = Room::new(RoomId::new(&"1".repeat(32)).unwrap(), profile.clone(), Mode::Animation { rotations: 1 }, Some(1000)).unwrap();
        let mut view = room.view(profile.id).unwrap();
        view.phase = Phase::Transfer;
        view.epoch = 2;
        let assignment = Assignment {
            id: AssignmentId { game: GameId::new(&"2".repeat(32)).unwrap(), chain: 0, round: 0 },
            player: profile.id,
            project: ProjectId::new(&"3".repeat(32)).unwrap(),
        };
        view.assignment = Some(AssignmentView { assignment: assignment.clone(), submitted: false, reference: None });
        store.update_poll(view.clone()).unwrap();
        store.pin_freeze(FreezePin { room: view.id, epoch: view.epoch, assignment: assignment.id, project: assignment.project, revision: 1 }).unwrap();
        let image = AssetRef { hash: AssetHash::new(&"4".repeat(64)).unwrap(), bytes: 4 };
        let checkpoint =
            CheckpointRef { id: CheckpointId::new(&"5".repeat(32)).unwrap(), asset: AssetRef { hash: AssetHash::new(&"6".repeat(64)).unwrap(), bytes: 4 } };
        let submission = Submission {
            id: SubmissionId::new(&"7".repeat(32)).unwrap(),
            assignment: assignment.id,
            author: profile.id,
            project: assignment.project,
            revision: 2,
            image,
            checkpoint,
            replay: ReplayRef {
                id: ReplayId::new(&"8".repeat(32)).unwrap(),
                manifest: None,
                baseline: checkpoint,
                end_event: None,
                event_count: 0,
                duration_ms: 0,
            },
            assets: vec![image, checkpoint.asset],
            note: String::new(),
            seam: None,
        };
        for freeze in [true, false] {
            assert!(submit(&mut store, submission.clone(), vec![b"future artifact".to_vec()], freeze).is_err());
            assert_eq!(store.status().pending, 0);
            assert!(store.next().unwrap().is_none());
            assert!(store.status().submission_project.is_none());
        }
        let mut original = submission;
        original.revision = 1;
        submit(&mut store, original, Vec::new(), true).unwrap();
        assert_eq!(store.status().pending, 2);
    }
}
