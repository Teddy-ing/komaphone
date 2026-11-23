//! Nonblocking local draft coordination. A queued write never counts as a save.
use std::collections::{BTreeSet, HashMap, HashSet};

use photocraft_doc::DocId;
pub use photocraft_local_store::DraftInfo;
use photocraft_local_store::SaveRequest;
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

struct PendingSave {
    request: SaveRequest,
    doc: DocId,
    editor_revision: u64,
    dedup_existing: bool,
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
    // Documents admitted from legacy PhotoCraft recovery whose initial baseline save should
    // bind to an existing identical head checkpoint in `drafts.sqlite3` instead of cloning.
    recovery_dedup: HashSet<DocId>,
    // Closed draft identities awaiting best-effort safe pruning on the local-store worker.
    #[cfg(not(target_arch = "wasm32"))]
    prune_queue: Vec<ProjectId>,
    #[cfg(not(target_arch = "wasm32"))]
    pruning: bool,
    // Retain the exact request on failure, including its idempotency key and pixels.
    pending: Option<PendingSave>,
    failed: HashMap<DocId, PendingSave>,
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
            .map(|pending| crate::local_replay::snapshot_retained_bytes(&pending.request.document).saturating_add(photocraft_local_store::MAX_RECORD_BYTES))
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
        if self.rooms.submission_error_for(room.id, assignment.assignment.id).is_some() {
            return "Submission failed · local draft retained";
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
    /// Whether editing the currently active document is refused by active party-room assignment
    /// rules (previous assignment in the current game, or frozen/submitted/transferring assignment).
    pub fn room_active_edit_refusal(&self) -> Option<String> {
        let room = self.rooms.status.room.as_ref()?;
        let document = self.active()?;
        let project = self.local_projects.status(document.doc.id).project?;
        if self.rooms.status.game_drafts.as_ref().is_some_and(|drafts| drafts.room == room.id && drafts.projects.contains(&project))
            && room.assignment.as_ref().is_none_or(|assignment| assignment.assignment.project != project)
        {
            return Some("This drawing belongs to a previous assignment in the current game. Open your current assignment; its original remains saved.".into());
        }
        let assignment = room.assignment.as_ref()?;
        if project != assignment.assignment.project {
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
    pub fn room_edit_refusal(&self, command: &str) -> Option<String> {
        if matches!(
            command,
            "file.new"
                | "file.open"
                | "layer.select"
                | "select.deselectLayers"
                | "layer.selectLinkedLayers"
                | "layer.selectAbove"
                | "layer.selectBelow"
                | "layer.selectTop"
                | "layer.selectBottom"
        ) || !crate::commands::find(command).is_some_and(|spec| spec.journal)
        {
            return None;
        }
        if let Some(reason) = self.room_active_edit_refusal() {
            return Some(reason);
        }
        if self.room_canvas_size().is_some() && matches!(command, "layer.new.artboard" | "layer.new.artboardFromLayers" | "layer.new.artboardFromGroup") {
            return Some("Artboards are unavailable in a fixed-size party assignment; use layers or groups within this frame or band.".into());
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
        self.rooms.clear_submission_error();
        self.local_projects.preparing_room = true;
        Ok(())
    }
    /// Projects that must not be pruned or permanently deleted right now because they back an
    /// open Workroom document, a pending/failed local save, or an active party-room game.
    pub fn active_protected_projects(&self) -> BTreeSet<ProjectId> {
        let mut protected = BTreeSet::new();
        for doc in self.documents() {
            if let Some(project) = self.local_projects.status(doc.doc.id).project {
                protected.insert(project);
            }
        }
        if let Some(pending) = &self.local_projects.pending {
            protected.insert(pending.request.project);
        }
        for pending in self.local_projects.failed.values() {
            protected.insert(pending.request.project);
        }
        if let Some(project) = self.rooms.submission_project() {
            protected.insert(project);
        }
        if let Some(pin) = &self.rooms.status.freeze_pin {
            protected.insert(pin.project);
        }
        if let Some(room) = &self.rooms.status.room
            && matches!(room.phase, photocraft_party::room::Phase::Drawing | photocraft_party::room::Phase::Transfer)
        {
            if let Some(assignment) = &room.assignment {
                protected.insert(assignment.assignment.project);
            }
            if let Some(drafts) = &self.rooms.status.game_drafts
                && drafts.room == room.id
            {
                protected.extend(drafts.projects.iter().copied());
            }
        }
        protected
    }

    /// True when `document` belongs to an active (in-progress/submitting) party-room game and
    /// therefore must not be saved as an independent solo gallery entry or pruned automatically.
    pub fn is_active_party_document(&self, document: DocId) -> bool {
        let Some(project) = self.local_projects.status(document).project else {
            return false;
        };
        if self.rooms.submission_project() == Some(project) || self.rooms.status.freeze_pin.as_ref().is_some_and(|pin| pin.project == project) {
            return true;
        }
        self.rooms.status.room.as_ref().is_some_and(|room| {
            matches!(room.phase, photocraft_party::room::Phase::Drawing | photocraft_party::room::Phase::Transfer)
                && (room.assignment.as_ref().is_some_and(|assignment| assignment.assignment.project == project)
                    || self.rooms.status.game_drafts.as_ref().is_some_and(|drafts| drafts.room == room.id && drafts.projects.contains(&project)))
        })
    }

    pub(crate) fn mark_recovered_document(&mut self, doc: DocId) {
        self.local_projects.recovery_dedup.insert(doc);
    }

    /// Queue a closed, unprotected draft for serialized safe pruning on the local-store worker
    /// (avoiding any cross-thread race with concurrent `Request::Save` writes).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn queue_safe_local_prune(&mut self, project: ProjectId) {
        if self.local_projects.available() && !self.active_protected_projects().contains(&project) && !self.local_projects.prune_queue.contains(&project) {
            self.local_projects.prune_queue.push(project);
        }
    }

    pub(crate) fn prune_closed_local_project(&mut self, doc: DocId) {
        self.local_projects.recovery_dedup.remove(&doc);
        #[cfg(not(target_arch = "wasm32"))]
        {
            if !self.local_projects.available() {
                return;
            }
            let status = self.local_projects.status(doc);
            let Some(project) = status.project else {
                return;
            };
            if status.local_revision == 0
                || self.local_replay.pending_for(doc) > 0
                || self.local_projects.pending.as_ref().is_some_and(|pending| pending.doc == doc)
                || self.local_projects.failed.contains_key(&doc)
                || self.active_protected_projects().contains(&project)
            {
                return;
            }
            if !self.local_projects.prune_queue.contains(&project) {
                self.local_projects.prune_queue.push(project);
            }
        }
    }

    /// Best-effort clean-exit prune: removes untouched blank canvases, round drafts already stored
    /// in a completed game, and working copies identical to a Saved gallery entry. Never removes
    /// edited unsaved drawings, active party assignments, or failed saves.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn prune_local_projects_on_exit(&mut self, timeout: std::time::Duration) {
        if !self.local_projects.available() || self.local_projects.error.is_some() {
            return;
        }
        let mut candidates = std::mem::take(&mut self.local_projects.prune_queue);
        for doc in self.documents() {
            let id = doc.doc.id;
            if self.is_active_party_document(id)
                || self.local_projects.failed.contains_key(&id)
                || self.local_replay.pending_for(id) > 0
                || self.local_projects.pending.as_ref().is_some_and(|pending| pending.doc == id)
            {
                continue;
            }
            let status = self.local_projects.status(id);
            if status.local_revision > 0
                && let Some(project) = status.project
                && !candidates.contains(&project)
            {
                candidates.push(project);
            }
        }
        if !candidates.is_empty() {
            self.local_projects.prune_queue = candidates;
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            self.poll_local_projects();
            if self.local_projects.error.is_some() || (!self.local_projects.pruning && self.local_projects.prune_queue.is_empty()) {
                return;
            }
            if std::time::Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
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
            && !self.local_projects.pending.as_ref().is_some_and(|pending| pending.doc == document)
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
                || self.local_projects.pruning
                || !self.local_projects.prune_queue.is_empty()
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

#[cfg(test)]
mod room_layer_guard_tests {
    use serde_json::json;

    use super::*;
    use crate::rooms::GameDrafts;
    use photocraft_party::room::{Assignment, AssignmentView, MemberView, Phase, RoomView};
    use photocraft_party::{AssignmentId, DeviceId, GameId, Mode, Participant, ParticipantId, RoomId};

    fn participant(seed: u8) -> Participant {
        Participant {
            id: ParticipantId::new(&format!("{seed:02x}").repeat(16)).unwrap(),
            device: DeviceId::new(&format!("{seed:02x}").repeat(16)).unwrap(),
            display_name: "Alice".into(),
        }
    }

    fn project_id(seed: u8) -> ProjectId {
        ProjectId::new(&format!("{seed:02x}").repeat(16)).unwrap()
    }

    fn bind_assignment(session: &mut Session, mode: Mode, project: ProjectId, submitted: bool, phase: Phase) {
        let doc_id = session.active().unwrap().doc.id;
        session.local_projects.states.entry(doc_id).or_default().project = Some(project);
        let alice = participant(1);
        let room_id = RoomId::new(&"aa".repeat(16)).unwrap();
        let game_id = GameId::new(&"bb".repeat(16)).unwrap();
        session.rooms.status.profile = Some(alice.clone());
        session.rooms.status.room = Some(RoomView {
            id: room_id,
            revision: 3,
            epoch: 1,
            phase,
            round: 1,
            mode,
            timer_ms: None,
            moderator: alice.id,
            members: vec![MemberView { participant: alice.clone(), connected: true, ready: true }],
            waiting: vec![],
            paused: None,
            deadline_ms: None,
            assignment: Some(AssignmentView {
                assignment: Assignment { id: AssignmentId { game: game_id, chain: 0, round: 1 }, player: alice.id, project },
                submitted,
                reference: None,
            }),
            reveal: vec![],
        });
    }

    #[test]
    fn active_party_assignment_allows_layers_and_blocks_artboards_with_reason() {
        for (mode, width, height) in [(Mode::ExquisiteCorpse, 1024, 512), (Mode::Animation { rotations: 2 }, 1536, 1024)] {
            let mut s = Session::new();
            s.execute("file.new", json!({"width": width, "height": height, "background": "white"})).unwrap();
            bind_assignment(&mut s, mode, project_id(5), false, Phase::Drawing);
            assert!(s.room_active_edit_refusal().is_none());

            // Normal layer workflows remain fully enabled inside the fixed frame/band.
            let layer = s.execute("layer.new.layer", json!({"name": "Sketch"})).unwrap()["layer"].as_u64().unwrap();
            s.execute("layer.layerMask.revealAll", json!({})).unwrap();
            s.execute("layer.setLabelColor", json!({"layer": layer, "color": "blue"})).unwrap();
            s.execute("layer.groupLayers", json!({"name": "Lines"})).unwrap();
            let before = s.active().unwrap().doc.clone();

            for cmd in ["layer.new.artboard", "layer.new.artboardFromLayers", "layer.new.artboardFromGroup"] {
                assert!(!s.is_enabled(cmd), "{cmd} must be disabled in fixed-size party assignment");
                let reason = s.disabled_reason(cmd).expect("explicit refusal reason");
                assert!(reason.contains("fixed-size party assignment"), "{cmd}: {reason}");
                assert!(s.execute(cmd, json!({})).is_err(), "{cmd} execute must fail");
            }
            assert_eq!(s.active().unwrap().doc.as_ref(), before.as_ref());
        }
    }

    #[test]
    fn frozen_party_assignment_refuses_cross_document_copy_and_smart_object_commit() {
        let mut s = Session::new();
        // Doc 0: active party assignment with a Smart Object, initially editable.
        s.execute("file.new", json!({"width": 1024, "height": 512, "background": "white"})).unwrap();
        s.execute("layer.new.layer", json!({"name": "Patch"})).unwrap();
        s.execute("edit.fill", json!({"color": "#cc3344"})).unwrap();
        s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
        bind_assignment(&mut s, Mode::ExquisiteCorpse, project_id(7), false, Phase::Drawing);

        // Open Edit Contents child tab (Doc 1) while assignment is editable.
        s.execute("layer.smartObjects.editContents", json!({})).unwrap();
        let child_index = s.active_index().unwrap();
        assert_eq!(child_index, 1);
        s.execute("layer.new.layer", json!({"name": "Child edit"})).unwrap();

        // Also open an unrelated editable local drawing (Doc 2).
        s.execute("file.new", json!({"width": 64, "height": 64, "background": "white"})).unwrap();
        s.execute("layer.new.layer", json!({"name": "Foreign"})).unwrap();
        let foreign_index = s.active_index().unwrap();
        assert_eq!(foreign_index, 2);

        // Freeze the party assignment (submitted = true).
        s.rooms.status.room.as_mut().unwrap().assignment.as_mut().unwrap().submitted = true;
        let frozen_before = s.documents()[0].doc.clone();

        // 1. Copying a layer from the editable local draft into the frozen assignment must fail
        //    and leave the frozen document untouched while keeping the source active.
        let copy_err = s.execute("layer.copyToDocument", json!({"document": 0})).unwrap_err().to_string();
        assert!(copy_err.contains("frozen for submission"), "unexpected copy error: {copy_err}");
        assert_eq!(s.active_index(), Some(foreign_index));
        assert_eq!(s.documents()[0].doc.as_ref(), frozen_before.as_ref());

        // 2. Committing the Smart Object child tab back into the now-frozen parent must also fail
        //    and leave the frozen parent untouched.
        let commit_err = crate::smart_cmds::commit_child(&mut s, child_index).unwrap_err().to_string();
        assert!(commit_err.contains("frozen for submission"), "unexpected smart commit error: {commit_err}");
        assert_eq!(s.documents()[0].doc.as_ref(), frozen_before.as_ref());
    }

    #[test]
    fn previous_game_assignment_is_refused_by_room_active_edit_refusal_and_edit_with() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 1024, "height": 512, "background": "white"})).unwrap();
        let prev_project = project_id(8);
        let current_project = project_id(9);
        bind_assignment(&mut s, Mode::ExquisiteCorpse, current_project, false, Phase::Drawing);
        // Bind Doc 0 to the previous assignment in the same room.
        let doc_id = s.active().unwrap().doc.id;
        s.local_projects.states.entry(doc_id).or_default().project = Some(prev_project);
        let room_id = s.rooms.status.room.as_ref().unwrap().id;
        let game_id = s.rooms.status.room.as_ref().unwrap().assignment.as_ref().unwrap().assignment.id.game;
        s.rooms.status.game_drafts = Some(GameDrafts { room: room_id, game: game_id, projects: vec![prev_project, current_project] });

        let reason = s.room_active_edit_refusal().expect("previous assignment refused");
        assert!(reason.contains("previous assignment"), "{reason}");
        assert!(s.execute("layer.new.layer", json!({})).is_err());
        // Read-only view commands remain allowed.
        assert!(s.room_edit_refusal("layer.select").is_none());
        assert!(s.room_edit_refusal("layer.setExpanded").is_none());
    }
}
