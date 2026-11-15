//! Bounded transient pipeline: server streams order writes and observations order snapshots.
use super::{Action, Coordinator, Error, Event, Instant, Job, OpId, Reply, Response, Result, SyncSender, VecDeque, other, wire};
use photocraft_scene::{CanvasId, StrokeId};
use std::{collections::BTreeSet, time::Duration};
mod pool;
pub(super) use pool::Pool;
#[cfg(test)]
mod tests;
pub(super) const SLOTS: usize = 3;
// At most 1,154 requests/minute, including negotiation/reads. No catch-up bursts.
const CADENCE: Duration = Duration::from_millis(52);
const COMPLETED_LIMIT: usize = 128;
struct Stream {
    id: OpId,
    canvas: CanvasId,
    epoch: u64,
    serial: u64,
    observation: u64,
}
struct Flight {
    scope: u64,
    generation: u64,
    canvas: CanvasId,
    epoch: u64,
    sequence: Option<u64>,
    dispatch: u64,
    stream: Option<OpId>,
    action: Action,
}
#[derive(Default)]
pub(super) struct Lane {
    pending: VecDeque<(u64, Action)>,
    slots: [Option<Flight>; SLOTS],
    scope: u64,
    stream: Option<Stream>,
    next_send: Option<Instant>,
    next_dispatch: u64,
    success_dispatch: u64,
    retry_after: Option<Instant>,
    failures: u32,
    paused: bool,
    info_refresh: Option<CanvasId>,
    completed: BTreeSet<StrokeId>,
    commit_wait: u64,
}
impl Coordinator {
    pub(super) fn invalidate_presence(&mut self) -> Result<()> {
        self.presence.scope = self.presence.scope.checked_add(1).ok_or(Error::Busy)?;
        for (sequence, _) in self.presence.pending.drain(..) {
            self.outbound.push_back(Event::PresenceDropped { sequence });
        }
        self.presence.stream = None;
        self.presence.retry_after = None;
        self.presence.failures = 0;
        self.presence.paused = false;
        self.presence.info_refresh = None;
        self.presence.success_dispatch = 0;
        self.presence.completed.clear();
        self.presence.commit_wait = 0;
        // Physical slots and the shared deadline survive all scope changes.
        Ok(())
    }
    pub(super) fn presence_enqueue(&mut self, sequence: u64, action: Action) {
        let canvas = match &action {
            Action::Presence { canvas, .. } | Action::Peers { canvas } => *canvas,
            _ => {
                self.outbound.push_back(Event::PresenceDropped { sequence });
                return;
            }
        };
        if self.selected != Some(canvas) {
            self.outbound.push_back(Event::PresenceDropped { sequence });
            return;
        }
        if let Some(queued) = self.presence.pending.iter_mut().find(|(_, previous)| {
            matches!((previous, &action), (Action::Presence { .. }, Action::Presence { .. }) | (Action::Peers { .. }, Action::Peers { .. }))
        }) {
            let (sequence, _) = std::mem::replace(queued, (sequence, action));
            self.outbound.push_back(Event::PresenceDropped { sequence });
        } else {
            self.presence.pending.push_back((sequence, action));
        }
    }
    pub(super) fn schedule_presence(&mut self, senders: &[SyncSender<Job>], now: Instant) -> Result<()> {
        if !self.auth_fence.allowed(self.auth_ticket) {
            return Ok(());
        }
        if self.presence.paused || self.presence.next_send.is_some_and(|until| now < until) || self.presence.retry_after.is_some_and(|until| now < until) {
            return Ok(());
        }
        let Some(canvas) = self.selected else { return Ok(()) };
        let Some(replica) = self.replicas.get(&canvas) else { return Ok(()) };
        let epoch = replica.info().epoch;
        if replica.accepted().revision < self.presence.commit_wait {
            return Ok(());
        }
        let Some(slot) = self.presence.slots.iter().position(Option::is_none) else { return Ok(()) };
        let Some(sender) = senders.get(slot) else { return Err(other("Presence worker unavailable")) };
        self.queue.ensure_current(self.lease()?)?;
        if self.access.is_none() {
            return Ok(());
        }
        if self.presence.stream.as_ref().is_some_and(|stream| stream.canvas != canvas || stream.epoch != epoch) {
            self.invalidate_presence()?;
            return Ok(());
        }
        let (sequence, original, action, stream) = if let Some(stream) = &self.presence.stream {
            let Some((sequence, mut original)) = self.presence.pending.pop_front() else { return Ok(()) };
            self.strip_committed_preview(&mut original);
            let action = match &original {
                Action::Presence { canvas, epoch: sampled_epoch, camera, preview, cursor, stroke } if *sampled_epoch == epoch => {
                    let Some(serial) = stream.serial.checked_add(1) else {
                        self.presence.stream = None;
                        self.presence.pending.push_front((sequence, original));
                        return Ok(());
                    };
                    Action::PresenceUpdate {
                        canvas: *canvas,
                        epoch,
                        stream: stream.id,
                        serial,
                        camera: camera.clone(),
                        preview: preview.clone(),
                        cursor: *cursor,
                        stroke: stroke.clone(),
                    }
                }
                Action::Peers { canvas } => Action::PresenceRead { canvas: *canvas, stream: stream.id },
                _ => {
                    self.outbound.push_back(Event::PresenceDropped { sequence });
                    return Ok(());
                }
            };
            (Some(sequence), original, action, Some(stream.id))
        } else {
            if self.presence.pending.is_empty() {
                return Ok(());
            }
            if self
                .presence
                .slots
                .iter()
                .flatten()
                .any(|flight| flight.scope == self.presence.scope && flight.generation == self.generation && flight.stream.is_none())
            {
                return Ok(());
            }
            let nonce = OpId::new(&crate::store::random_hex(16)?).map_err(other)?;
            let action = Action::PresenceOpen { canvas, epoch, nonce };
            (None, action.clone(), action, None)
        };
        let dispatch = self.presence.next_dispatch.checked_add(1).ok_or(Error::Busy)?;
        let Some(access) = &self.access else { return Ok(()) };
        let job = Job {
            ticket: self.auth_ticket,
            auth: access.auth.clone(),
            generation: self.generation,
            sequence: sequence.unwrap_or(0),
            endpoint: access.scope.endpoint.clone(),
            credential: access.credential.clone(),
            action,
        };
        if let Err(error) = sender.try_send(job) {
            if let Some(sequence) = sequence {
                self.presence.pending.push_front((sequence, original));
            }
            return match error {
                std::sync::mpsc::TrySendError::Full(_) => Ok(()),
                std::sync::mpsc::TrySendError::Disconnected(_) => Err(other("Presence worker stopped")),
            };
        }
        if let Some(active) = self.presence.stream.as_mut()
            && matches!(&original, Action::Presence { .. })
        {
            active.serial = active.serial.saturating_add(1);
        }
        self.presence.next_dispatch = dispatch;
        self.presence.next_send = Some(now + CADENCE);
        if let Some(position) = self.presence.slots.get_mut(slot) {
            *position = Some(Flight { scope: self.presence.scope, generation: self.generation, canvas, epoch, sequence, dispatch, stream, action: original });
        }
        Ok(())
    }
    fn strip_committed_preview(&self, action: &mut Action) {
        if let Action::Presence { canvas, stroke, preview, .. } = action
            && stroke.as_ref().is_some_and(|style| {
                self.presence.completed.contains(&style.id)
                    || self.replicas.get(canvas).is_some_and(|replica| replica.accepted().strokes.contains_key(&style.id))
            })
        {
            *stroke = None;
            preview.clear();
        }
    }
    pub(super) fn presence_committed(&mut self, operation: &super::WorldOp, revision: u64) {
        if self.selected != Some(operation.canvas) {
            return;
        }
        let id = match &operation.action {
            super::Edit::AddStroke { stroke } | super::Edit::CopyStroke { stroke, .. } => stroke.id,
            _ => return,
        };
        if self.presence.completed.len() >= COMPLETED_LIMIT && !self.presence.completed.contains(&id) {
            // Do not forget a noncontiguous commit and accidentally republish it.
            self.presence.commit_wait = self.presence.commit_wait.max(revision);
        } else {
            self.presence.completed.insert(id);
        }
        for (_, action) in &mut self.presence.pending {
            if let Action::Presence { stroke, preview, .. } = action
                && stroke.as_ref().is_some_and(|style| style.id == id)
            {
                *stroke = None;
                preview.clear();
            }
        }
    }
    pub(super) fn presence_verified(&mut self, canvas: CanvasId) {
        if self.presence.info_refresh == Some(canvas) {
            self.presence.info_refresh = None;
            self.presence.paused = false;
            self.presence.stream = None;
            self.presence.retry_after = None;
        }
        if self.selected == Some(canvas)
            && let Some(replica) = self.replicas.get(&canvas)
        {
            self.presence.completed.retain(|id| !replica.accepted().strokes.contains_key(id));
            if replica.accepted().revision >= self.presence.commit_wait {
                self.presence.commit_wait = 0;
            }
        }
    }
    pub(super) fn presence_reply(&mut self, slot: usize, reply: Reply, now: Instant) {
        let Some(flight) = self.presence.slots.get_mut(slot).and_then(Option::take) else { return };
        let current = self.auth_fence.allowed(self.auth_ticket)
            && flight.scope == self.presence.scope
            && flight.generation == self.generation
            && reply.generation == self.generation
            && self.selected == Some(flight.canvas)
            && self.replicas.get(&flight.canvas).is_some_and(|replica| replica.info().epoch == flight.epoch)
            && self.lease().and_then(|lease| self.queue.ensure_current(lease)).is_ok()
            && (flight.stream.is_none() || self.presence.stream.as_ref().is_some_and(|stream| Some(stream.id) == flight.stream));
        if !current {
            self.drop_presence_receipt(flight.sequence);
            return;
        }
        let error = match reply.result {
            Ok(Response::PresenceReady { canvas, epoch, nonce, stream })
                if flight.stream.is_none()
                    && canvas == flight.canvas
                    && epoch == flight.epoch
                    && matches!(flight.action, Action::PresenceOpen { nonce: expected, .. } if nonce == expected) =>
            {
                self.presence.stream = Some(Stream { id: stream, canvas, epoch, serial: 0, observation: 0 });
                self.presence_success(flight.dispatch);
                return;
            }
            Ok(Response::PresenceSnapshot { canvas, stream, observation, mut peers })
                if Some(stream) == flight.stream && canvas == flight.canvas && observation > 0 =>
            {
                match validate_peers(canvas, &peers) {
                    Ok(()) => {
                        let Some(active) = self.presence.stream.as_mut() else {
                            self.drop_presence_receipt(flight.sequence);
                            return;
                        };
                        if observation <= active.observation {
                            self.drop_presence_receipt(flight.sequence);
                            return;
                        }
                        active.observation = observation;
                        if let Some(replica) = self.replicas.get(&canvas) {
                            for peer in &mut peers {
                                if peer.stroke.as_ref().is_some_and(|style| replica.accepted().strokes.contains_key(&style.id)) {
                                    peer.stroke = None;
                                    peer.preview.clear();
                                }
                            }
                        }
                        self.presence_success(flight.dispatch);
                        if let Some(sequence) = flight.sequence {
                            self.outbound.push_back(Event::Response { sequence, response: Response::Peers { peers } });
                        }
                        return;
                    }
                    Err(error) => error,
                }
            }
            Ok(_) => other("Service returned an invalid presence response"),
            Err(error) => error,
        };
        if !self.presence_failure(&flight, error, now) {
            self.drop_presence_receipt(flight.sequence);
        }
    }
    fn presence_success(&mut self, dispatch: u64) {
        self.presence.success_dispatch = self.presence.success_dispatch.max(dispatch);
        self.presence.retry_after = None;
        self.presence.failures = 0;
    }
    fn drop_presence_receipt(&mut self, sequence: Option<u64>) {
        if let Some(sequence) = sequence {
            self.outbound.push_back(Event::PresenceDropped { sequence });
        }
    }
    fn presence_failure(&mut self, flight: &Flight, error: Error, now: Instant) -> bool {
        let code = match &error {
            Error::Rejected { code, .. } => Some(code.as_str()),
            _ => None,
        };
        match code {
            Some("presence_stream") => self.presence.stream = None,
            Some("session") => {
                if !self.presence.paused {
                    self.outbound.push_back(Event::Error { sequence: 0, message: error.to_string() });
                }
                self.presence.paused = true;
                self.presence.stream = None;
            }
            Some("unauthorized") => {
                self.presence.paused = true;
                self.presence.stream = None;
                if self.presence.info_refresh != Some(flight.canvas) {
                    self.presence.info_refresh = Some(flight.canvas);
                    if !self.requests.iter().any(|(_, action)| matches!(action, Action::Info { canvas } if *canvas == flight.canvas)) {
                        self.requests.push_front((0, Action::Info { canvas: flight.canvas }));
                    }
                }
            }
            _ if flight.dispatch > self.presence.success_dispatch => {
                self.presence.failures = self.presence.failures.saturating_add(1);
                let multiplier = 1u64.checked_shl(self.presence.failures.saturating_sub(1).min(3)).unwrap_or(8);
                self.presence.retry_after = Some(now + Duration::from_millis(250 * multiplier));
                if self.presence.failures == 3 {
                    self.outbound.push_back(Event::Error { sequence: 0, message: error.to_string() });
                }
            }
            _ => {}
        }
        if matches!(code, Some("session" | "unauthorized"))
            && let Some(sequence) = flight.sequence
        {
            self.outbound.push_back(Event::Response { sequence, response: Response::Peers { peers: vec![] } });
            return true;
        }
        false
    }
}
fn validate_peers(canvas: CanvasId, peers: &[wire::Peer]) -> Result<()> {
    if peers.len() > 4096 {
        return Err(other("Presence page exceeds the local limit"));
    }
    let mut devices = BTreeSet::new();
    for peer in peers {
        if peer.camera.canvas != canvas || !devices.insert(peer.device) || peer.preview.len() > wire::MAX_PREVIEW_POINTS {
            return Err(other("Invalid shared presence"));
        }
        photocraft_scene::coordinates::validate_camera(&peer.camera).map_err(other)?;
        if let Some(cursor) = peer.cursor {
            photocraft_scene::coordinates::validate_point(cursor).map_err(other)?;
        }
        if let Some(style) = &peer.stroke {
            if !peer.preview.is_empty() {
                photocraft_scene::Stroke {
                    id: style.id,
                    canvas,
                    owner: peer.profile,
                    revision: 0,
                    anchor: style.anchor,
                    points: peer.preview.clone(),
                    width: style.width,
                    color: style.color,
                    erase: style.erase,
                }
                .validate()
                .map_err(other)?;
            }
        } else if !peer.preview.is_empty() {
            return Err(other("A preview is missing its stroke style"));
        }
    }
    Ok(())
}
