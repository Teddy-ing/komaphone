//! Thin, nonblocking engine seam for durable private rooms. The native adapter owns
//! credentials, HTTP and the outbox; the UI sees only an authorized projection.
use crate::{EngineError, Result, Session};
use photocraft_room_client::{ClientStatus, Invitation};

pub mod animation;
pub mod corpse;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod drafts;
pub mod media;

#[cfg(not(target_arch = "wasm32"))]
pub use photocraft_room_client::ClientOptions;
pub use photocraft_room_client::FreezePin;
pub use photocraft_room_client::{GameDrafts, NoteDraft, RevealPosition};

#[derive(Default)]
pub struct Rooms {
    pub status: ClientStatus,
    pub media: media::RoomMedia,
    invitation: Option<Invitation>,
    configured: bool,
    sending: Option<u64>,
    submission_sending: Option<(u64, photocraft_party::ProjectId)>,
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<photocraft_room_client::Worker>,
}

impl Rooms {
    pub fn available(&self) -> bool {
        self.configured
    }
    pub fn busy(&self) -> bool {
        self.sending.is_some() || self.status.pending > 0
    }
    /// Explicit UI display only; this capability is never part of serialized room status.
    pub fn invitation(&self) -> Option<&Invitation> {
        self.invitation.as_ref()
    }
    pub fn submission_project(&self) -> Option<photocraft_party::ProjectId> {
        self.submission_sending.map(|(_, project)| project).or(self.status.submission_project)
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn accept_status(&mut self, handled_sequence: u64, status: ClientStatus) {
        if self.sending.is_some_and(|pending| pending > handled_sequence) {
            return;
        }
        if status.invitation_id.is_none() {
            self.invitation = None;
        }
        self.status = status;
        self.media.reconcile(&self.status);
        self.sending = None;
        if self.submission_sending.is_some_and(|(pending, _)| pending <= handled_sequence) {
            self.submission_sending = None;
        }
    }
}

impl Session {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn enable_rooms(&mut self, database: std::path::PathBuf, options: ClientOptions) -> Result<()> {
        if self.rooms.configured {
            return Err(EngineError::Other("Room storage is already configured".into()));
        }
        self.rooms.worker = Some(photocraft_room_client::Worker::start(database, options).map_err(|e| EngineError::Other(e.to_string()))?);
        self.rooms.configured = true;
        Ok(())
    }

    pub fn poll_rooms(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        loop {
            let event = self.rooms.worker.as_ref().map(|worker| worker.poll());
            match event {
                Some(Ok(photocraft_room_client::ClientEvent::Status { handled_sequence, status })) => {
                    self.rooms.accept_status(handled_sequence, *status);
                }
                Some(Ok(photocraft_room_client::ClientEvent::Invitation(invitation))) => self.rooms.invitation = Some(invitation),
                Some(Ok(photocraft_room_client::ClientEvent::Image { room, asset, bytes })) => {
                    if let Err(error) = self.rooms.media.accept(&self.rooms.status, room, asset, bytes) {
                        self.rooms.media.error = Some(error.to_string());
                    } else if self.rooms.media.images.contains_key(&asset.hash)
                        && let Some(worker) = &self.rooms.worker
                    {
                        let _ = worker.send(photocraft_room_client::ClientCommand::ImageAck { room, hash: asset.hash });
                    }
                }
                Some(Ok(photocraft_room_client::ClientEvent::Notes { room, game, epoch, notes })) => {
                    self.rooms.media.accept_notes(&self.rooms.status, room, game, epoch, notes)
                }
                Some(Ok(photocraft_room_client::ClientEvent::MediaError(error))) => self.rooms.media.error = Some(error),
                Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                    self.rooms.sending = None;
                    self.rooms.submission_sending = None;
                    self.rooms.status.connected = false;
                    if self.rooms.status.error.is_none() {
                        self.rooms.status.error = Some("Room worker stopped; local drafts remain saved. Restart the app to reconnect".into());
                    }
                    break;
                }
                _ => break,
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.poll_room_deadline();
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn room_request(&mut self, command: photocraft_room_client::ClientCommand) -> Result<u64> {
        let worker = self.rooms.worker.as_ref().ok_or_else(|| EngineError::Other("Private rooms require the native Windows app".into()))?;
        let sequence = worker.send(command).map_err(|e| EngineError::Other(e.to_string()))?;
        self.rooms.sending = Some(sequence);
        Ok(sequence)
    }

    /// Called only after constructing the manifest from the exact acknowledged project
    /// revision. An unrelated local draft cannot be submitted to an assignment.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn queue_room_submission(&mut self, submission: photocraft_party::Submission, assets: Vec<Vec<u8>>, freeze: bool) -> Result<()> {
        self.check_room_submission(&submission, freeze)?;
        let project = submission.project;
        let sequence = self.room_request(photocraft_room_client::ClientCommand::Submit { submission: Box::new(submission), assets, freeze })?;
        self.rooms.submission_sending = Some((sequence, project));
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn pin_room_freeze(&mut self, pin: FreezePin) -> Result<()> {
        let open = self
            .documents()
            .iter()
            .filter_map(|document| {
                let local = self.local_projects.status(document.doc.id);
                (local.project == Some(pin.project)).then_some(local.local_revision)
            })
            .max()
            .unwrap_or(0);
        let listed = self.local_projects.drafts.iter().find(|draft| draft.project == pin.project).map_or(0, |draft| draft.revision);
        if pin.revision == 0 || pin.revision > open.max(listed) {
            return Err(EngineError::Other("A deadline can pin only an acknowledged local revision".into()));
        }
        self.room_request(photocraft_room_client::ClientCommand::PinFreeze(pin)).map(|_| ())
    }

    pub(crate) fn check_room_submission(&self, submission: &photocraft_party::Submission, freeze: bool) -> Result<()> {
        submission.validate().map_err(|e| EngineError::Other(e.to_string()))?;
        let document = self.documents().iter().find(|document| self.local_projects.status(document.doc.id).project == Some(submission.project));
        let saved = if freeze {
            let open_revision = document.map(|document| self.local_projects.status(document.doc.id).local_revision).unwrap_or(0);
            let listed_revision = self.local_projects.drafts.iter().find(|draft| draft.project == submission.project).map_or(0, |draft| draft.revision);
            submission.revision > 0 && submission.revision <= open_revision.max(listed_revision)
        } else {
            document.is_some_and(|document| {
                self.local_document_saved(document.doc.id) && self.local_projects.status(document.doc.id).local_revision == submission.revision
            })
        };
        if !saved {
            return Err(EngineError::Other("Only the assigned project's last acknowledged local revision can be submitted; wait for its local save".into()));
        }
        let room = self.rooms.status.room.as_ref().ok_or_else(|| EngineError::Other("No active room assignment".into()))?;
        let assignment = room.assignment.as_ref().ok_or_else(|| EngineError::Other("No active room assignment".into()))?;
        if assignment.assignment.project != submission.project
            || assignment.assignment.id != submission.assignment
            || self.rooms.status.profile.as_ref().map(|p| p.id) != Some(submission.author)
        {
            return Err(EngineError::Other("This drawing does not belong to the current room assignment".into()));
        }
        if self.rooms.status.freeze_pin.as_ref().is_some_and(|pin| {
            pin.room == room.id
                && pin.epoch == room.epoch
                && pin.assignment == submission.assignment
                && pin.project == submission.project
                && pin.revision != submission.revision
        }) {
            return Err(EngineError::Other("This deadline already pinned a different acknowledged revision; submit the first pinned revision".into()));
        }
        Ok(())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    #[test]
    fn delayed_status_cannot_thaw_a_newly_queued_submission() {
        let mut rooms = Rooms { sending: Some(2), configured: true, ..Rooms::default() };
        rooms.accept_status(1, ClientStatus::default());
        assert!(rooms.busy(), "a status published before the latest command cannot acknowledge its queueing");
        rooms.accept_status(2, ClientStatus { pending: 1, ..ClientStatus::default() });
        assert!(rooms.busy(), "durably queued submission remains immutable until acceptance");
        rooms.accept_status(2, ClientStatus::default());
        assert!(!rooms.busy());
    }
}
