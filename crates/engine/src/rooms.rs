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

#[derive(Clone, Copy, PartialEq, Eq)]
enum SubmissionErrorScope {
    Assignment { room: photocraft_party::RoomId, assignment: photocraft_party::AssignmentId },
    Room(Option<photocraft_party::RoomId>),
}

#[derive(Default)]
pub struct Rooms {
    pub status: ClientStatus,
    pub media: media::RoomMedia,
    submission_error: Option<String>,
    submission_error_scope: Option<SubmissionErrorScope>,
    invitation: Option<Invitation>,
    configured: bool,
    sending: Option<u64>,
    submission_sending: Option<(u64, photocraft_party::ProjectId)>,
    account_transition: bool,
    browser_url: Option<String>,
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
    /// One-time native shell effect; never included in serialized room diagnostics.
    pub fn take_browser_url(&mut self) -> Option<String> {
        self.browser_url.take()
    }
    /// Explicit UI display only; this capability is never part of serialized room status.
    pub fn invitation(&self) -> Option<&Invitation> {
        self.invitation.as_ref()
    }
    pub fn submission_project(&self) -> Option<photocraft_party::ProjectId> {
        self.submission_sending.map(|(_, project)| project).or(self.status.submission_project)
    }
    pub fn submission_error(&self) -> Option<&str> {
        self.submission_error.as_deref()
    }
    pub(crate) fn submission_error_for(&self, room: photocraft_party::RoomId, assignment: photocraft_party::AssignmentId) -> Option<&str> {
        match self.submission_error_scope {
            Some(SubmissionErrorScope::Assignment { room: expected_room, assignment: expected_assignment })
                if expected_room == room && expected_assignment == assignment =>
            {
                self.submission_error.as_deref()
            }
            _ => None,
        }
    }
    pub fn set_submission_error(&mut self, error: impl Into<String>) {
        let message = error.into();
        let scope = match self.status.room.as_ref() {
            Some(room) => match room
                .assignment
                .as_ref()
                .filter(|a| !a.submitted && matches!(room.phase, photocraft_party::room::Phase::Drawing | photocraft_party::room::Phase::Transfer))
            {
                Some(assignment) => SubmissionErrorScope::Assignment { room: room.id, assignment: assignment.assignment.id },
                None => SubmissionErrorScope::Room(Some(room.id)),
            },
            None => SubmissionErrorScope::Room(None),
        };
        if self.status.error.is_none() || self.status.error.as_deref() == self.submission_error.as_deref() {
            self.status.error = Some(message.clone());
        }
        self.submission_error = Some(message);
        self.submission_error_scope = Some(scope);
    }
    pub fn clear_submission_error(&mut self) {
        if self.submission_error.is_some() && self.status.error.as_deref() == self.submission_error.as_deref() {
            self.status.error = None;
        }
        self.submission_error = None;
        self.submission_error_scope = None;
    }
    fn reconcile_submission_error(&mut self) {
        let Some(scope) = self.submission_error_scope else { return };
        let keep = match scope {
            SubmissionErrorScope::Assignment { room: expected_room, assignment: expected_assignment } => self.status.room.as_ref().is_some_and(|room| {
                room.id == expected_room
                    && matches!(room.phase, photocraft_party::room::Phase::Drawing | photocraft_party::room::Phase::Transfer)
                    && room
                        .assignment
                        .as_ref()
                        .is_some_and(|a| a.assignment.id == expected_assignment && !a.submitted && self.submission_project() != Some(a.assignment.project))
            }),
            SubmissionErrorScope::Room(expected_room) => self.status.room.as_ref().map(|r| r.id) == expected_room && self.submission_project().is_none(),
        };
        if !keep {
            self.clear_submission_error();
        } else if self.status.error.is_none() {
            self.status.error = self.submission_error.clone();
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn accept_status(&mut self, handled_sequence: u64, status: ClientStatus) -> bool {
        if self.sending.is_some_and(|pending| pending > handled_sequence) {
            return false;
        }
        // Local Retry can restore polling without a room RPC or a connected
        // flag change. Refresh canvas access only after accepting that status.
        let access_changed = self.status.endpoint != status.endpoint
            || self.status.profile != status.profile
            || self.status.paired != status.paired
            || self.status.connected != status.connected
            || self.status.polling != status.polling;
        if status.invitation_id.is_none() {
            self.invitation = None;
        }
        self.status = status;
        self.account_transition = self.status.signing_in;
        self.media.reconcile(&self.status);
        self.sending = None;
        if self.submission_sending.is_some_and(|(pending, _)| pending <= handled_sequence) {
            self.submission_sending = None;
            if self.status.submission_project.is_none()
                && !self.status.room.as_ref().and_then(|r| r.assignment.as_ref()).is_some_and(|a| a.submitted)
                && let Some(error) = self.status.error.clone()
            {
                self.set_submission_error(error);
            }
        }
        self.reconcile_submission_error();
        access_changed
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
            if self.rooms.account_transition
                && matches!(&event, Some(Ok(event)) if !matches!(event,
                    photocraft_room_client::ClientEvent::Status { .. }
                    | photocraft_room_client::ClientEvent::WorldAccess { .. }
                    | photocraft_room_client::ClientEvent::RelocationReady { .. }
                    | photocraft_room_client::ClientEvent::OpenBrowser { .. }))
            {
                continue;
            }
            match event {
                Some(Ok(photocraft_room_client::ClientEvent::RelocationReady { handled_sequence, relocation, access })) => {
                    if self.rooms.sending.is_none_or(|sequence| sequence <= handled_sequence)
                        && let Err(error) = self.relocate_world_queue(relocation, access)
                    {
                        self.rooms.status.error = Some(error.to_string());
                    }
                }
                Some(Ok(photocraft_room_client::ClientEvent::OpenBrowser { handled_sequence, url })) => {
                    if self.rooms.sending.is_none_or(|sequence| sequence <= handled_sequence) && self.rooms.status.signing_in {
                        self.rooms.browser_url = Some(url);
                    }
                }
                Some(Ok(photocraft_room_client::ClientEvent::Status { handled_sequence, status })) => {
                    let changed = self.rooms.accept_status(handled_sequence, *status);
                    if changed
                        && self.worlds.available()
                        && let Some(worker) = &self.rooms.worker
                    {
                        let _ = worker.send(photocraft_room_client::ClientCommand::WorldAccess);
                    }
                }
                Some(Ok(photocraft_room_client::ClientEvent::WorldAccess { handled_sequence, access })) => {
                    if self.rooms.sending.is_none_or(|sequence| sequence <= handled_sequence)
                        && let Err(error) = self.world_access(access)
                    {
                        self.worlds.error = Some(error.to_string());
                    }
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
                Some(Ok(photocraft_room_client::ClientEvent::History { room, games, next_revision })) => {
                    if self.gallery.remote_room != Some(room) {
                        self.gallery.remote.clear();
                    }
                    for game in games {
                        self.gallery.remote.retain(|existing| existing.game != game.game);
                        self.gallery.remote.push(game);
                    }
                    self.gallery.remote_room = Some(room);
                    self.gallery.remote_cursor = next_revision;
                }
                Some(Ok(photocraft_room_client::ClientEvent::Gallery(bundle))) => self.receive_gallery(bundle),
                Some(Ok(photocraft_room_client::ClientEvent::GalleryProgress { received, total, waiting })) => {
                    self.gallery.downloaded_bytes = received;
                    self.gallery.download_bytes = total;
                    self.gallery.waiting = waiting;
                }
                Some(Ok(photocraft_room_client::ClientEvent::GalleryError(error))) => {
                    self.gallery.error = Some(error);
                    self.gallery.downloading = false;
                }
                Some(Ok(photocraft_room_client::ClientEvent::GalleryNotes { game, notes })) => {
                    if let Some(active) = self.gallery.active.as_ref().filter(|active| active.game() == Some(game)) {
                        self.gallery.incoming_notes = Some((active.id, notes));
                    }
                }
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
        let account_transition = matches!(
            command,
            photocraft_room_client::ClientCommand::SignIn { .. }
                | photocraft_room_client::ClientCommand::Relocate { .. }
                | photocraft_room_client::ClientCommand::CancelSignIn
                | photocraft_room_client::ClientCommand::SignOut
                | photocraft_room_client::ClientCommand::SelectAccount { .. }
        );
        let changes_access =
            account_transition || matches!(command, photocraft_room_client::ClientCommand::Disconnect | photocraft_room_client::ClientCommand::Connect { .. });
        let clears_submission_error = changes_access
            || matches!(
                &command,
                photocraft_room_client::ClientCommand::Retry
                    | photocraft_room_client::ClientCommand::RetainRejected
                    | photocraft_room_client::ClientCommand::Create { .. }
                    | photocraft_room_client::ClientCommand::Join { .. }
            )
            || matches!(&command, photocraft_room_client::ClientCommand::Action(action) if matches!(action.as_ref(), photocraft_party::room::Action::Leave));
        if changes_access {
            if let Some(worker) = &self.rooms.worker {
                worker.pause_authenticated();
            }
            self.suspend_world_access();
            // Failed dispatch must leave old responses quarantined too. A later
            // successful retry replaces this sentinel with its actual sequence.
            self.rooms.sending = Some(u64::MAX);
            self.rooms.account_transition = true;
            self.rooms.browser_url = None;
            self.rooms.invitation = None;
            self.rooms.media = Default::default();
            self.gallery.remote.clear();
            self.gallery.remote_room = None;
            self.gallery.remote_cursor = None;
            self.gallery.downloading = false;
            self.gallery.incoming_notes = None;
            self.rooms.status.room = None;
            self.rooms.status.connected = false;
            self.rooms.status.paired = false;
            self.rooms.status.signed_in = false;
            self.rooms.status.signing_in = false;
            self.rooms.status.relocation = None;
            self.rooms.status.note_draft = None;
            self.rooms.status.game_drafts = None;
            self.rooms.status.freeze_pin = None;
            self.rooms.status.reveal_position = None;
            self.rooms.status.submission_project = None;
            self.rooms.status.invitation_id = None;
            self.rooms.status.error = None;
            self.rooms.status.notice = None;
            self.rooms.submission_sending = None;
            self.rooms.clear_submission_error();
            if let Err(error) = self.world_access(None) {
                self.worlds.error = Some(error.to_string());
            }
        }
        // A stopped canvas worker must never prevent room cancellation/sign-out.
        let worker = self.rooms.worker.as_ref().ok_or_else(|| EngineError::Other("Private rooms require the native Windows app".into()))?;
        let sequence = worker.send(command).map_err(|e| EngineError::Other(e.to_string()))?;
        if clears_submission_error {
            self.rooms.clear_submission_error();
        }
        self.rooms.sending = Some(sequence);
        Ok(sequence)
    }

    /// Called only after constructing the manifest from the exact acknowledged project
    /// revision. An unrelated local draft cannot be submitted to an assignment.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn queue_room_submission(&mut self, submission: photocraft_party::Submission, assets: Vec<Vec<u8>>, freeze: bool) -> Result<()> {
        let local_revision = submission.revision;
        self.queue_prepared_room_submission(submission, assets, local_revision, freeze)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn queue_prepared_room_submission(
        &mut self,
        submission: photocraft_party::Submission,
        assets: Vec<Vec<u8>>,
        local_revision: u64,
        freeze: bool,
    ) -> Result<()> {
        self.check_room_submission(&submission, local_revision, freeze)?;
        let project = submission.project;
        let sequence = self.room_request(photocraft_room_client::ClientCommand::Submit { submission: Box::new(submission), assets, local_revision, freeze })?;
        self.rooms.submission_sending = Some((sequence, project));
        self.rooms.clear_submission_error();
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

    pub(crate) fn check_room_submission(&self, submission: &photocraft_party::Submission, local_revision: u64, freeze: bool) -> Result<()> {
        submission.validate().map_err(|e| EngineError::Other(e.to_string()))?;
        if local_revision == 0 || submission.revision > local_revision {
            return Err(EngineError::Other("Submission replay steps cannot exceed the durable local revision".into()));
        }
        let document = self.documents().iter().find(|document| self.local_projects.status(document.doc.id).project == Some(submission.project));
        let saved = if freeze {
            let open_revision = document.map(|document| self.local_projects.status(document.doc.id).local_revision).unwrap_or(0);
            let listed_revision = self.local_projects.drafts.iter().find(|draft| draft.project == submission.project).map_or(0, |draft| draft.revision);
            local_revision <= open_revision.max(listed_revision)
        } else {
            document.is_some_and(|document| {
                self.local_document_saved(document.doc.id) && self.local_projects.status(document.doc.id).local_revision == local_revision
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
                && pin.revision != local_revision
        }) {
            return Err(EngineError::Other("This deadline already pinned a different acknowledged revision; submit the first pinned revision".into()));
        }
        Ok(())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "rooms/polling_tests.rs"]
mod polling_tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    #[test]
    fn failed_sign_out_dispatch_still_quarantines_private_state() {
        let mut session = Session::new();
        session.rooms.status.signed_in = true;
        session.rooms.status.paired = true;
        session.rooms.status.connected = true;
        session.rooms.status.note_draft = Some(NoteDraft {
            assignment: photocraft_party::AssignmentId { game: photocraft_party::GameId::new("00000000000000000000000000000001").unwrap(), chain: 0, round: 0 },
            note: "Private handoff note".into(),
        });
        session.rooms.browser_url = Some("private-browser-effect".into());
        assert!(session.room_request(photocraft_room_client::ClientCommand::SignOut).is_err());
        assert!(!session.rooms.status.signed_in);
        assert!(!session.rooms.status.connected);
        assert!(session.rooms.take_browser_url().is_none());
        assert!(session.rooms.status.note_draft.is_none());
        assert!(!serde_json::to_string(&session.rooms.status).unwrap().contains("Private handoff note"));
        session.rooms.accept_status(5, ClientStatus { paired: true, signed_in: true, ..ClientStatus::default() });
        assert!(!session.rooms.status.paired);
        assert!(session.rooms.account_transition);
    }

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
    #[test]
    fn old_status_cannot_reopen_an_account_during_switch() {
        let mut rooms = Rooms { sending: Some(10), account_transition: true, ..Rooms::default() };
        assert!(!rooms.accept_status(9, ClientStatus { paired: true, signed_in: true, polling: true, ..ClientStatus::default() }));
        assert!(!rooms.status.paired);
        assert!(!rooms.status.polling);
        assert!(rooms.account_transition);
        rooms.accept_status(10, ClientStatus { hosted: true, ..ClientStatus::default() });
        assert!(!rooms.status.signed_in);
        assert!(!rooms.account_transition);
        assert!(rooms.browser_url.is_none());
    }
    #[test]
    fn browser_wait_keeps_old_private_events_quarantined() {
        let mut rooms = Rooms { sending: Some(3), account_transition: true, ..Rooms::default() };
        rooms.accept_status(3, ClientStatus { hosted: true, signing_in: true, ..ClientStatus::default() });
        assert!(rooms.account_transition, "intermediate browser-wait status must not admit old media events");
        rooms.accept_status(3, ClientStatus { hosted: true, signed_in: true, paired: true, ..ClientStatus::default() });
        assert!(!rooms.account_transition);
    }
}
