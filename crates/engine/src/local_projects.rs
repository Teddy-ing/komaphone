//! Nonblocking local draft coordination. A queued write never counts as a save.
use std::collections::HashMap;

use photocraft_doc::DocId;
use photocraft_local_store::{DraftInfo, SaveRequest};
use photocraft_party::contracts::ProjectId;
use serde::Serialize;

use crate::{EngineError, Result, Session};

#[cfg(not(target_arch = "wasm32"))]
mod native;

#[derive(Clone, Debug, Serialize)]
pub struct LocalStatus {
    pub project: Option<ProjectId>,
    pub local_revision: u64,
    pub pending: usize,
    pub error: Option<String>,
    pub sync: &'static str,
    #[serde(skip)]
    editor_revision: u64,
}

impl Default for LocalStatus {
    fn default() -> Self {
        Self { project: None, local_revision: 0, pending: 0, error: None, sync: "Local only · not synced", editor_revision: 0 }
    }
}

#[derive(Default)]
pub struct LocalProjects {
    pub drafts: Vec<DraftInfo>,
    pub usage: Option<photocraft_local_store::Usage>,
    pub error: Option<String>,
    pub notice: Option<String>,
    states: HashMap<DocId, LocalStatus>,
    configured: bool,
    loading: bool,
    pub(crate) preparing_room: bool,
    pub(crate) frozen_assignment: Option<photocraft_party::AssignmentId>,
    // Retain the exact request on failure, including its idempotency key and pixels.
    pending: Option<(SaveRequest, DocId, u64)>,
    failed: HashMap<DocId, (SaveRequest, DocId, u64)>,
    failed_bytes: usize,
    writing: bool,
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<native::Worker>,
}

impl LocalProjects {
    pub(crate) fn failure_budget_exhausted(&self) -> bool {
        self.failed.len() >= 16 || self.failed_bytes >= 512 * 1024 * 1024
    }
    fn recount_failed_bytes(&mut self) {
        self.failed_bytes = self
            .failed
            .values()
            .map(|(request, _, _)| crate::local_replay::snapshot_retained_bytes(&request.document).saturating_add(photocraft_local_store::MAX_RECORD_BYTES))
            .fold(0usize, usize::saturating_add);
    }
    pub(crate) fn owns_document(&self, id: DocId) -> bool {
        self.states.contains_key(&id)
    }
    pub fn available(&self) -> bool {
        self.configured
    }
    pub fn loading(&self) -> bool {
        self.loading
    }
    pub fn status(&self, document: DocId) -> LocalStatus {
        self.states.get(&document).cloned().unwrap_or_default()
    }
    pub fn status_text(&self, document: DocId) -> String {
        let status = self.status(document);
        if let Some(error) = status.error.as_ref().or(self.error.as_ref()) {
            return format!("Save failed: {error}");
        }
        if !self.available() {
            return "Unsaved · local storage unavailable".into();
        }
        if status.pending > 0 {
            return "Saving locally…".into();
        }
        if status.local_revision == 0 {
            return "Drawing · Unsaved".into();
        }
        format!("Saved locally · revision {}", status.local_revision)
    }
    pub fn has_pending(&self) -> bool {
        self.pending.is_some() || self.loading || self.preparing_room || self.states.values().any(|s| s.pending > 0)
    }
}

impl Session {
    pub(crate) fn room_canvas_size(&self) -> Option<photocraft_doc::Size> {
        let room = self.rooms.status.room.as_ref()?;
        let assignment = room.assignment.as_ref()?;
        let document = self.active()?;
        if self.local_projects.status(document.doc.id).project != Some(assignment.assignment.project) {
            return None;
        }
        Some(match room.mode {
            photocraft_party::Mode::Animation { .. } => photocraft_doc::Size { width: 1536, height: 1024 },
            photocraft_party::Mode::ExquisiteCorpse => photocraft_doc::Size { width: 1024, height: 512 },
        })
    }
    pub fn room_sync_label(&self, document: DocId) -> &'static str {
        let Some(room) = self.rooms.status.room.as_ref() else { return "Local only · not synced" };
        let Some(assignment) = room.assignment.as_ref() else { return "Local draft · open the room for game status" };
        if self.local_projects.status(document).project != Some(assignment.assignment.project) {
            return "Local only · not synced";
        }
        if assignment.submitted {
            return "Synced · submission accepted";
        }
        if self.rooms.status.blocked {
            return "Sync blocked · local draft retained";
        }
        if self.local_projects.preparing_room || self.rooms.busy() {
            return "Sync pending · awaiting service confirmation";
        }
        if !self.rooms.status.connected {
            return "Service unavailable · not submitted";
        }
        "Local draft · not submitted"
    }
    /// Deadline observation is driven by the authorized room projection, never by a UI timer.
    pub fn poll_room_deadline(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.freeze_room_deadline();
    }
    pub(crate) fn room_edit_refusal(&self, command: &str) -> Option<String> {
        let room = self.rooms.status.room.as_ref()?;
        if !matches!(command, "file.new" | "file.open")
            && crate::commands::find(command).is_some_and(|spec| spec.journal)
            && let Some(project) = self.active().and_then(|document| self.local_projects.status(document.doc.id).project)
            && self.rooms.status.game_drafts.as_ref().is_some_and(|drafts| drafts.room == room.id && drafts.projects.contains(&project))
            && room.assignment.as_ref().is_none_or(|assignment| assignment.assignment.project != project)
        {
            return Some("This drawing belongs to a previous assignment in the current game. Open your current assignment; its original remains saved.".into());
        }
        let assignment = room.assignment.as_ref()?;
        let document = self.active()?;
        if matches!(command, "file.new" | "file.open")
            || self.local_projects.status(document.doc.id).project != Some(assignment.assignment.project)
            || !crate::commands::find(command).is_some_and(|spec| spec.journal)
        {
            return None;
        }
        if room.phase == photocraft_party::room::Phase::Transfer
            || assignment.submitted
            || self
                .rooms
                .status
                .freeze_pin
                .as_ref()
                .is_some_and(|pin| pin.room == room.id && pin.assignment == assignment.assignment.id && pin.project == assignment.assignment.project)
            || self.local_projects.preparing_room
            || self.rooms.submission_project() == Some(assignment.assignment.project)
        {
            return Some("This assigned drawing is frozen for submission. Its saved local revision is retained.".into());
        }
        None
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn open_assigned_local_project(&mut self, project: ProjectId, mode: photocraft_party::Mode) -> Result<()> {
        if self.documents().iter().any(|d| self.local_projects.status(d.doc.id).project == Some(project)) {
            return self.open_local_project(project);
        }
        if self.local_projects.loading {
            return Err(EngineError::Other("A local draft is already loading".into()));
        }
        self.check_new_local_replay_capacity()?;
        self.local_request(native::Request::OpenAssigned { project, mode })?;
        self.local_projects.loading = true;
        Ok(())
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn prepare_assigned_submission(&mut self, request: crate::rooms::drafts::PrepareRequest) -> Result<()> {
        if self.local_projects.preparing_room {
            return Err(EngineError::Other("Submission preparation is already running".into()));
        }
        self.local_request(native::Request::PrepareRoom(Box::new(request)))?;
        self.local_projects.preparing_room = true;
        Ok(())
    }
    /// True only for the current, completed document snapshot after its exact save ACK.
    /// UI callers also check for an unfinished gesture which has not entered the engine.
    pub fn local_document_saved(&self, document: DocId) -> bool {
        let Some(state) = self.documents().iter().find(|state| state.doc.id == document) else { return false };
        let status = self.local_projects.status(document);
        self.local_projects.available()
            && status.project.is_some()
            && status.local_revision > 0
            && status.error.is_none()
            && self.local_projects.error.is_none()
            && self.local_replay.pending_for(document) == 0
            && !self.local_projects.pending.as_ref().is_some_and(|(_, id, _)| *id == document)
            && self.local_replay.is_current(state)
    }

    /// Called each native frame and by headless clients waiting for durable completion.
    pub fn poll_local_projects(&mut self) {
        // Compound commands may still roll back their tentative edits. Only drain after
        // their outer command boundary, even if a nested action queries project status.
        if self.local_replay.in_command() {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.poll_local_projects_native();
    }

    pub fn list_local_projects(&mut self) -> Result<()> {
        // One refresh/open at a time. A stale list response must not clear an open's
        // loading state and allow two editors to race on the same project revision.
        if self.local_projects.loading {
            return Ok(());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.local_request(native::Request::List)?;
            self.local_projects.loading = true;
            Ok(())
        }
        #[cfg(target_arch = "wasm32")]
        Err(EngineError::Other("Permanent local drafts require the Windows desktop app".into()))
    }

    pub fn open_local_project(&mut self, project: ProjectId) -> Result<()> {
        if let Some(index) = self.documents().iter().position(|d| self.local_projects.status(d.doc.id).project == Some(project)) {
            self.set_active(index);
            return Ok(());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.local_projects.loading {
                return Err(EngineError::Other("A local draft is already loading".into()));
            }
            self.local_request(native::Request::Open(project))?;
            self.local_projects.loading = true;
            Ok(())
        }
        #[cfg(target_arch = "wasm32")]
        Err(EngineError::Other("Permanent local drafts require the Windows desktop app".into()))
    }

    pub fn retry_local_projects(&mut self) -> Result<()> {
        if !self.local_projects.available() {
            return Err(EngineError::Other("Local storage is not configured".into()));
        }
        self.local_projects.error = None;
        if let Some(document) = self.active().map(|state| state.doc.id) {
            if self.local_projects.pending.is_none()
                && let Some(request) = self.local_projects.failed.remove(&document)
            {
                self.local_projects.pending = Some(request);
                self.local_projects.recount_failed_bytes();
            }
            if !self.local_projects.failed.contains_key(&document)
                && let Some(state) = self.local_projects.states.get_mut(&document)
            {
                state.error = None;
            }
        }
        self.poll_local_projects();
        self.list_local_projects()
    }

    /// Normal shutdown waits for completed edits; failures remain visible before shutdown.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn flush_local_projects(&mut self, timeout: std::time::Duration) -> Result<()> {
        if !self.local_projects.available() {
            return Ok(());
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            self.poll_local_projects();
            if let Some(error) = &self.local_projects.error {
                return Err(EngineError::Other(error.clone()));
            }
            let blocked = self.local_projects.failed.keys().copied().collect();
            let active_work = self.local_projects.pending.is_some()
                || self.local_projects.loading
                || self.local_projects.writing
                || self.local_replay.peek_unblocked(&blocked).is_some();
            if !active_work {
                if let Some(error) = self.local_projects.failed.keys().find_map(|doc| self.local_projects.states.get(doc).and_then(|state| state.error.clone()))
                {
                    return Err(EngineError::Other(error));
                }
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(EngineError::Other("Local save is still pending; keep the app open and retry".into()));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
