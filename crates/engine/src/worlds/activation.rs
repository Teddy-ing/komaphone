//! Sequence-bound account handoff. Buffered old-account events cannot reopen
//! private presentation while a newer requested account is being activated.
use super::{Account, Result, Session, error};
use photocraft_room_client::world_worker::{Command, Event, WorldAccess};

/// Opaque native credentials are retained only for exact handoff coalescing.
/// This type intentionally has no Debug or serialization implementation.
pub(super) struct Request {
    sequence: u64,
    access: Option<WorldAccess>,
    awaiting: bool,
    failed: bool,
}
impl Session {
    /// Fence authenticated work before a room transition can fail or back up.
    pub(crate) fn suspend_world_access(&mut self) {
        if let Some(worker) = &self.worlds.worker {
            worker.pause_authenticated();
        }
        self.worlds.relocation_pending = None;
        self.begin_world_activation(u64::MAX, None);
        if let Some(request) = self.worlds.access_request.as_mut() {
            request.failed = true;
        }
    }

    pub(crate) fn world_access(&mut self, access: Option<WorldAccess>) -> Result<()> {
        if self.worlds.worker.is_none() {
            self.canvas_link_access(access.as_ref());
            return Ok(());
        }
        if self.worlds.access_request.as_ref().is_some_and(|request| !request.failed && request.access == access) {
            return Ok(());
        }
        let copied = access.as_ref().map(WorldAccess::copy_for_worker);
        // A full command channel must not clear the current account or leave
        // presentation permanently waiting for an Access command never sent.
        let worker = self.worlds.worker.as_ref().ok_or_else(|| error("Shared canvas worker unavailable"))?;
        let sequence = worker.send(Command::Access(access)).map_err(error)?;
        self.begin_world_activation(sequence, copied);
        Ok(())
    }
    fn begin_world_activation(&mut self, sequence: u64, access: Option<WorldAccess>) {
        self.canvas_link_access(access.as_ref());
        self.worlds.clear_private();
        self.worlds.account = None;
        self.thumbnail_access(access.as_ref());
        self.worlds.access_request = Some(Request { sequence, access, awaiting: true, failed: false });
    }
    pub(super) fn world_event_current(&mut self, event: &Event) -> bool {
        if let Event::Stopped { message } = event {
            if let Some(request) = self.worlds.access_request.as_mut() {
                request.failed = true;
            }
            self.worlds.error = Some(format!("Shared canvas storage worker stopped: {message}. Resolve the storage error and restart Komaphone."));
            return false;
        }
        if let Event::Activated { sequence, scope } = event {
            let Some(request) = self.worlds.access_request.as_mut() else { return false };
            if request.sequence != *sequence || request.access.as_ref().map(WorldAccess::scope) != scope.as_ref() {
                return false;
            }
            if request.awaiting {
                request.awaiting = false;
                request.failed = false;
                self.worlds.account = scope.as_ref().map(|scope| Account { endpoint: scope.endpoint.clone(), profile: scope.profile, device: scope.device });
            }
            return false;
        }
        if let Some(request) = self.worlds.access_request.as_mut()
            && request.awaiting
        {
            if let Event::Error { sequence, message } = event
                && *sequence == request.sequence
            {
                request.failed = true;
                self.worlds.error = Some(message.clone());
            }
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests;
