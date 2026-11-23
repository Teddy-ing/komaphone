//! Nonblocking gallery command seam. Originals never become editable room state.
use crate::{EngineError, Result, Session};
use photocraft_party::gallery::GalleryManifest;
use photocraft_party::{GameId, ProjectId, RoomId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
mod native;

/// Trusted platform adapter for gallery file operations. Request paths must be resolved
/// through granted directory capabilities, never passed through as ambient paths.
/// Preparation and publication run on the gallery worker, outside the UI thread.
pub trait GalleryFileAccess: Send + Sync {
    fn prepare(&self, action: &GalleryAction) -> Result<Box<dyn PreparedGalleryFiles>>;
}

/// Owns private staging and output capabilities until publication finishes. Dropping it
/// must clean up only its own staging. Success is reported only after `finish` succeeds.
pub trait PreparedGalleryFiles: Send {
    fn action(&self) -> GalleryAction;
    fn finish(&mut self) -> Result<()>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastExport {
    pub path: String,
    pub is_directory: bool,
    pub format: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GalleryItem {
    pub id: ProjectId,
    pub title: String,
    pub kind: String,
    pub authors: Vec<String>,
    pub updated_at: String,
    pub favorite: bool,
    pub trashed: bool,
    pub complete: bool,
    pub draft: bool,
    pub revision: u64,
    pub state: String,
    pub source_projects: Vec<ProjectId>,
    /// True on a draft row when `gallery.sqlite3` holds a non-trashed Saved project with the
    /// exact same head checkpoint hash (so the Temp tab can hide the redundant working copy).
    #[serde(default)]
    pub saved_matches_head: bool,
    /// True on a draft row when a non-trashed Saved copy with the same ID exists in Gallery.
    #[serde(default)]
    pub has_saved_copy: bool,
    /// True when a working draft diverges from its Saved copy (set on both the draft row and
    /// the corresponding Saved project row).
    #[serde(default)]
    pub has_unsaved_edits: bool,
    /// True on a draft row that is still an untouched `file.new` baseline (`rev == 1`).
    #[serde(default)]
    pub untouched_blank: bool,
    /// True on a draft row whose `ProjectId` is already preserved in a non-trashed Saved game.
    #[serde(default)]
    pub covered_by_game: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum GalleryAction {
    Refresh,
    Open {
        entry: ProjectId,
        #[serde(default)]
        draft: bool,
    },
    Update {
        entry: ProjectId,
        #[serde(default)]
        draft: bool,
        title: Option<String>,
        favorite: Option<bool>,
        trashed: Option<bool>,
    },
    SaveDraft {
        #[serde(default)]
        entry: Option<ProjectId>,
        #[serde(default)]
        title: Option<String>,
    },
    OpenEditable {
        entry: ProjectId,
    },
    Delete {
        entry: ProjectId,
        #[serde(default)]
        draft: bool,
    },
    EmptyTrash,
    ClearTemp,
    Thumbnail {
        entry: ProjectId,
        #[serde(default)]
        draft: bool,
    },
    Render {
        chain: u8,
        frame: u8,
        step: Option<usize>,
        elapsed_ms: Option<u64>,
        #[serde(default)]
        creature: bool,
    },
    Remix {
        chain: u8,
        frame: Option<u8>,
    },
    Playback {
        fps: u8,
        looping: bool,
    },
    SetNotes {
        entry: ProjectId,
        notes: Vec<(photocraft_party::AssignmentId, String)>,
    },
    PreviewGif {
        chain: u8,
        frame: u8,
        fps: u8,
        looping: bool,
    },
    Export {
        format: String,
        path: String,
        chain: u8,
        frame: Option<u8>,
        fps: u8,
        looping: bool,
    },
    ExportManaged {
        format: String,
        name: String,
        chain: u8,
        frame: Option<u8>,
        fps: u8,
        looping: bool,
    },
    Import {
        path: String,
    },
    Backup {
        path: String,
    },
    Restore {
        path: String,
        destination: String,
    },
}

impl GalleryAction {
    pub(crate) fn from_params(params: &serde_json::Value, action: &str) -> Result<Self> {
        let mut value = if params.is_null() { serde_json::json!({}) } else { params.clone() };
        let object = value.as_object_mut().ok_or_else(|| EngineError::Other("Gallery parameters must be an object".into()))?;
        if object.contains_key("action") {
            return Err(EngineError::Other("Unexpected gallery action parameter".into()));
        }
        if matches!(action, "refresh" | "empty_trash" | "clear_temp") && !object.is_empty() {
            return Err(EngineError::Other("Unexpected gallery parameter".into()));
        }
        object.insert("action".into(), serde_json::json!(action));
        let action: Self = serde_json::from_value(value).map_err(|e| EngineError::Other(format!("Invalid gallery request: {e}")))?;
        if let Self::SaveDraft { title: Some(title), .. } = &action {
            let trimmed = title.trim();
            if trimmed.is_empty() || trimmed.len() > 96 || trimmed.chars().any(char::is_control) {
                return Err(EngineError::Other("Gallery title must be 1–96 characters without control characters".into()));
            }
        }
        if let Self::Export { fps, format, .. } = &action
            && (!(1..=24).contains(fps) || !matches!(format.as_str(), "archive" | "png" | "gif" | "sequence"))
        {
            return Err(EngineError::Other("Choose a valid export format and 1–24 fps".into()));
        }
        if let Self::ExportManaged { fps, format, name, .. } = &action {
            crate::gallery_exports::ExportFormat::from_str_name(format)?;
            if !(1..=24).contains(fps) {
                return Err(EngineError::Other("Choose a valid export format and 1–24 fps".into()));
            }
            crate::gallery_exports::validate_export_basename(name)?;
        }
        let valid_path = |path: &str| !path.trim().is_empty() && path.len() <= 32768 && !path.contains('\0');
        let valid = match &action {
            Self::Export { path, .. } | Self::Import { path } | Self::Backup { path } => valid_path(path),
            Self::Restore { path, destination } => valid_path(path) && valid_path(destination),
            _ => true,
        };
        if !valid {
            return Err(EngineError::Other("Choose a valid gallery file path".into()));
        }
        Ok(action)
    }
}

#[derive(Default)]
pub struct Gallery {
    pub items: Vec<GalleryItem>,
    pub active: Option<Arc<GalleryManifest>>,
    pub active_draft: bool,
    active_sources: Vec<ProjectId>,
    pub image: Option<Arc<crate::rooms::media::RoomImage>>,
    pub image_revision: u64,
    pub replay: Option<crate::gallery_media::ReplayInfo>,
    pub frame_count: usize,
    pub thumbnails: BTreeMap<(ProjectId, bool), Arc<crate::rooms::media::RoomImage>>,
    thumbnail_order: std::collections::VecDeque<(ProjectId, bool)>,
    thumbnail_sources: BTreeMap<(ProjectId, bool), Vec<ProjectId>>,
    pub remote: Vec<photocraft_party::protocol::ArchivedGameSummary>,
    pub remote_room: Option<RoomId>,
    pub remote_cursor: Option<u64>,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub last_export: Option<LastExport>,
    pub exports_dir: Option<String>,
    pub downloading: bool,
    pub downloaded_bytes: u64,
    pub download_bytes: u64,
    pub waiting: bool,
    pub opened_document: bool,
    pub usage_bytes: u64,
    pub quota_bytes: u64,
    busy: bool,
    exporting: bool,
    configured: bool,
    #[cfg(not(target_arch = "wasm32"))]
    configured_exports_dir: Option<std::path::PathBuf>,
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<native::Worker>,
    #[cfg(not(target_arch = "wasm32"))]
    incoming: Option<Box<photocraft_party::gallery::GalleryBundle>>,
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) incoming_notes: Option<(ProjectId, Vec<(photocraft_party::AssignmentId, String)>)>,
}

impl Gallery {
    pub fn available(&self) -> bool {
        self.configured
    }
    pub fn busy(&self) -> bool {
        self.busy
    }
    pub fn exporting(&self) -> bool {
        self.exporting
    }
}

impl Session {
    pub fn gallery_sources_allowed(&self, sources: &[ProjectId]) -> bool {
        sources.iter().all(|project| self.gallery_draft_allowed(*project))
    }
    fn gallery_active_sources(&self) -> Vec<ProjectId> {
        let mut sources = self.gallery.active.as_ref().map_or_else(Vec::new, |active| active.source_projects());
        sources.extend_from_slice(&self.gallery.active_sources);
        sources.sort_unstable();
        sources.dedup();
        sources
    }
    fn gallery_active_allowed(&self) -> bool {
        self.gallery_sources_allowed(&self.gallery_active_sources())
    }
    pub fn gallery_item_allowed(&self, item: &GalleryItem) -> bool {
        self.gallery_sources_allowed(&item.source_projects) && (!item.draft || self.gallery_draft_allowed(item.id))
    }
    fn gallery_game_active(&self) -> bool {
        self.rooms
            .status
            .room
            .as_ref()
            .is_some_and(|room| matches!(room.phase, photocraft_party::room::Phase::Drawing | photocraft_party::room::Phase::Transfer))
    }
    pub fn gallery_draft_allowed(&self, project: ProjectId) -> bool {
        let Some(room) = &self.rooms.status.room else { return true };
        if matches!(room.phase, photocraft_party::room::Phase::Reveal | photocraft_party::room::Phase::Archived | photocraft_party::room::Phase::Lobby) {
            return true;
        }
        !self.rooms.status.game_drafts.as_ref().is_some_and(|drafts| drafts.room == room.id && drafts.projects.contains(&project))
            || room.assignment.as_ref().is_some_and(|assignment| assignment.assignment.project == project)
    }

    pub fn gallery_action(&mut self, action: GalleryAction) -> Result<()> {
        self.gallery_action_with_files(action, None)
    }

    /// Capability-scoped counterpart to the four existing file commands. The ordinary
    /// automation command policy continues to deny their ambient-path implementations.
    /// This entry point shares parsing, source/phase checks and worker execution with UI.
    pub fn gallery_file_command(&mut self, id: &str, params: &serde_json::Value, files: Arc<dyn GalleryFileAccess>) -> Result<serde_json::Value> {
        let action = match id {
            "komaphone.gallery.export" => "export",
            "komaphone.gallery.import" => "import",
            "komaphone.gallery.backup" => "backup",
            "komaphone.gallery.restore" => "restore",
            _ => return Err(EngineError::Other("Only gallery export, import, backup and restore accept file capabilities".into())),
        };
        self.gallery_action_with_files(GalleryAction::from_params(params, action)?, Some(files))?;
        Ok(serde_json::json!({"pending":true,"saved":false}))
    }

    fn gallery_action_with_files(&mut self, mut action: GalleryAction, files: Option<Arc<dyn GalleryFileAccess>>) -> Result<()> {
        if !self.gallery.available() {
            return Err(EngineError::Other("Gallery requires the native app's local storage".into()));
        }
        if self.gallery.busy {
            return Err(EngineError::Other("Gallery is still working; wait for its result".into()));
        }
        if matches!(
            action,
            GalleryAction::Render { .. }
                | GalleryAction::Remix { .. }
                | GalleryAction::Export { .. }
                | GalleryAction::ExportManaged { .. }
                | GalleryAction::Playback { .. }
                | GalleryAction::PreviewGif { .. }
        ) && self.gallery.active.is_none()
        {
            return Err(EngineError::Other("Open an authorized gallery original first".into()));
        }
        if !self.gallery_active_allowed()
            && matches!(
                action,
                GalleryAction::Render { .. }
                    | GalleryAction::Remix { .. }
                    | GalleryAction::Export { .. }
                    | GalleryAction::ExportManaged { .. }
                    | GalleryAction::Playback { .. }
                    | GalleryAction::PreviewGif { .. }
            )
        {
            return Err(EngineError::Other("Previous assignment history is available after this game reveals".into()));
        }
        match &mut action {
            GalleryAction::Open { entry, draft } | GalleryAction::Thumbnail { entry, draft } | GalleryAction::Update { entry, draft, .. }
                if (*draft && !self.gallery_draft_allowed(*entry))
                    || self
                        .gallery
                        .items
                        .iter()
                        .find(|item| item.id == *entry && item.draft == *draft)
                        .is_some_and(|item| !self.gallery_item_allowed(item)) =>
            {
                return Err(EngineError::Other("Previous assigned drawings remain private until this game reveals".into()));
            }
            GalleryAction::SaveDraft { entry, .. } => match *entry {
                None => {
                    let doc = self.active().ok_or_else(|| EngineError::Other("Open a drawing in the Workroom before saving to gallery".into()))?;
                    let doc_id = doc.doc.id;
                    if let Some(reason) = self.local_replay.excluded_reason(doc_id) {
                        return Err(EngineError::Other(format!("External documents use File > Save instead of Gallery: {reason}")));
                    }
                    if self.is_active_party_document(doc_id) {
                        return Err(EngineError::Other("Party room drawings save together when the game reveals".into()));
                    }
                    if !self.local_document_saved(doc_id) {
                        return Err(EngineError::Other("Wait for local saving to finish before saving to gallery".into()));
                    }
                    let project =
                        self.local_projects.status(doc_id).project.ok_or_else(|| EngineError::Other("This drawing has not saved locally yet".into()))?;
                    *entry = Some(project);
                }
                Some(project) => {
                    if !self.gallery_draft_allowed(project) {
                        return Err(EngineError::Other("Previous assigned drawings remain private until this game reveals".into()));
                    }
                    if self.gallery_game_active() && self.rooms.status.game_drafts.as_ref().is_some_and(|drafts| drafts.projects.contains(&project)) {
                        return Err(EngineError::Other("Party room drawings save together when the game reveals".into()));
                    }
                    if let Some(open_doc) = self.documents().iter().find(|d| self.local_projects.status(d.doc.id).project == Some(project))
                        && !self.local_document_saved(open_doc.doc.id)
                    {
                        return Err(EngineError::Other("Wait for local saving to finish before saving to gallery".into()));
                    }
                }
            },
            GalleryAction::OpenEditable { entry } => {
                let entry = *entry;
                if !self.gallery_draft_allowed(entry)
                    || self.gallery.items.iter().find(|item| item.id == entry && !item.draft).is_some_and(|item| !self.gallery_item_allowed(item))
                {
                    return Err(EngineError::Other("Previous assigned drawings remain private until this game reveals".into()));
                }
                if let Some(index) = self.documents().iter().position(|d| self.local_projects.status(d.doc.id).project == Some(entry)) {
                    self.set_active(index);
                    self.gallery.opened_document = true;
                    self.gallery.notice = Some("Switched to open drawing in Workroom.".into());
                    return Ok(());
                }
                self.check_new_local_replay_capacity()?;
            }
            GalleryAction::Delete { entry, draft } => {
                if (*draft && !self.gallery_draft_allowed(*entry))
                    || self.gallery.items.iter().find(|item| item.id == *entry && item.draft == *draft).is_some_and(|item| !self.gallery_item_allowed(item))
                {
                    return Err(EngineError::Other("Previous assigned drawings remain private until this game reveals".into()));
                }
                if *draft && self.active_protected_projects().contains(entry) {
                    return Err(EngineError::Other("Close this drawing in the Workroom before deleting it permanently".into()));
                }
            }
            GalleryAction::Remix { .. } => {
                if self
                    .rooms
                    .status
                    .game_drafts
                    .as_ref()
                    .is_some_and(|drafts| self.gallery_active_sources().iter().any(|source| drafts.projects.contains(source)))
                    && self.gallery_game_active()
                {
                    return Err(EngineError::Other("Finish the game before remixing an assigned drawing".into()));
                }
                self.check_new_local_replay_capacity()?;
            }
            GalleryAction::Import { .. } | GalleryAction::Restore { .. } if self.gallery_game_active() => {
                return Err(EngineError::Other("Finish the active game before importing or restoring archives; current drafts remain saved".into()));
            }
            _ => (),
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let protected = self.active_protected_projects();
            let worker = self.gallery.worker.as_ref().ok_or_else(|| EngineError::Other("Gallery storage is unavailable".into()))?;
            let source = self.gallery.active.as_ref().map(|active| (active.id, self.gallery.active_draft));
            let is_export = matches!(action, GalleryAction::Export { .. } | GalleryAction::ExportManaged { .. });
            if matches!(
                action,
                GalleryAction::Export { .. }
                    | GalleryAction::ExportManaged { .. }
                    | GalleryAction::Open { .. }
                    | GalleryAction::Delete { .. }
                    | GalleryAction::EmptyTrash
                    | GalleryAction::Import { .. }
                    | GalleryAction::OpenEditable { .. }
                    | GalleryAction::Remix { .. }
            ) {
                self.gallery.last_export = None;
            }
            worker.cancel_export.store(false, std::sync::atomic::Ordering::Release);
            let request = match files {
                Some(files) => native::Request::ScopedAction { action, source, protected, files },
                None => native::Request::Action { action, source, protected },
            };
            worker.send(request)?;
            self.gallery.busy = true;
            self.gallery.exporting = is_export;
            self.gallery.error = None;
            Ok(())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (action, files);
            Err(EngineError::Other("Gallery storage requires the Windows desktop app".into()))
        }
    }

    /// Request cooperative cancellation of an in-flight gallery export.
    pub fn cancel_gallery_export(&mut self) -> Result<bool> {
        if !self.gallery.available() {
            return Err(EngineError::Other("Gallery requires the native app's local storage".into()));
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.gallery.exporting
                && let Some(worker) = &self.gallery.worker
            {
                worker.cancel_export.store(true, std::sync::atomic::Ordering::Release);
                return Ok(true);
            }
            Ok(false)
        }
        #[cfg(target_arch = "wasm32")]
        {
            Ok(false)
        }
    }

    /// Configure the dedicated app-managed Exports folder (used by `komaphone.gallery.exportManaged`).
    pub fn set_gallery_exports_dir(&mut self, dir: std::path::PathBuf) -> Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let resolved = crate::gallery_exports::resolve_exports_dir(&dir, Some(&dir))?;
            let display = crate::gallery_exports::strip_windows_verbatim(&resolved);
            if let Some(worker) = &self.gallery.worker {
                *worker.exports_dir.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Ok(resolved.clone());
            }
            self.gallery.configured_exports_dir = Some(resolved);
            self.gallery.exports_dir = Some(display);
            Ok(())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = dir;
            Err(EngineError::Other("Gallery exports require the Windows desktop app".into()))
        }
    }

    pub fn poll_gallery(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.poll_gallery_native();
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn receive_gallery(&mut self, bundle: Box<photocraft_party::gallery::GalleryBundle>) {
        self.gallery.downloading = false;
        if self.gallery.incoming.is_some() {
            self.gallery.error = Some("Another gallery original is still waiting for storage; retry this game's download".into());
            return;
        }
        self.gallery.incoming = Some(bundle);
        self.poll_gallery();
    }

    pub fn download_gallery_game(&mut self, game: Option<GameId>) -> Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.gallery.downloading || self.gallery.incoming.is_some() {
                return Err(EngineError::Other("A game download is already pending".into()));
            }
            let room = self.rooms.status.room.as_ref().ok_or_else(|| EngineError::Other("Connect to the game's private room first".into()))?;
            let game = game
                .or_else(|| {
                    room.reveal.first().map(|entry| match entry {
                        photocraft_party::room::RevealedEntry::Artwork { assignment, .. }
                        | photocraft_party::room::RevealedEntry::Missing { assignment, .. } => assignment.id.game,
                    })
                })
                .ok_or_else(|| EngineError::Other("This game has not revealed".into()))?;
            let id = ProjectId::new(game.as_str()).map_err(|e| EngineError::Other(e.to_string()))?;
            if self.gallery.items.iter().any(|item| item.id == id && !item.draft) {
                return self.gallery_action(GalleryAction::Open { entry: id, draft: false });
            }
            self.room_request(photocraft_room_client::ClientCommand::DownloadGame {
                room: room.id,
                game,
                timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            })?;
            self.gallery.downloading = true;
            self.gallery.downloaded_bytes = 0;
            self.gallery.download_bytes = 0;
            self.gallery.error = None;
            Ok(())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = game;
            Err(EngineError::Other("Private gallery downloads require the native app".into()))
        }
    }
}
