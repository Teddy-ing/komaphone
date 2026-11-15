//! Two-store endpoint relocation: publish no new account until the world queue commits.
use super::{Result, Session, error};
use photocraft_room_client::{
    AccountRelocation, ClientCommand,
    world_worker::{Command, Event, WorldAccess},
};

impl Session {
    pub(crate) fn relocate_world_queue(&mut self, relocation: AccountRelocation, access: WorldAccess) -> Result<()> {
        if self.rooms.status.relocation.as_ref() != Some(&relocation) {
            return Err(error("Relocation no longer matches the selected account"));
        }
        if self.worlds.relocation_pending.is_some_and(|(_, id)| id == relocation.id) {
            return Ok(());
        }
        self.world_access(None)?;
        let worker = self.worlds.worker.as_ref().ok_or_else(|| error("Shared canvas storage must be available before moving this account"))?;
        let id = relocation.id;
        let sequence = worker.send(Command::Relocate { relocation, access }).map_err(error)?;
        self.worlds.relocation_pending = Some((sequence, id));
        self.worlds.notice = Some("Moving the saved account and its queued artwork to the verified service…".into());
        Ok(())
    }

    pub(super) fn world_relocation_event(&mut self, event: &Event) -> bool {
        match event {
            Event::Relocated { sequence, id } => {
                if self.worlds.relocation_pending != Some((*sequence, *id)) {
                    return true;
                }
                self.worlds.relocation_pending = None;
                if self.rooms.status.relocation.as_ref().is_some_and(|relocation| relocation.id == *id)
                    && let Err(error) = self.room_request(ClientCommand::CompleteRelocation { id: *id })
                {
                    self.rooms.status.error = Some(error.to_string());
                }
                true
            }
            Event::Error { sequence, message } if self.worlds.relocation_pending.is_some_and(|(pending, _)| pending == *sequence) => {
                self.worlds.relocation_pending = None;
                self.rooms.status.error = Some(format!("Account move is paused: {message}. Sign in again to resume; queued work is retained."));
                true
            }
            _ => false,
        }
    }
}
