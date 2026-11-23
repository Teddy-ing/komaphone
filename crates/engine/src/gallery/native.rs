use super::{GalleryAction, GalleryItem};
use crate::{EngineError, Result, Session, gallery_media};
use photocraft_local_store::gallery::{GalleryKind, GalleryStore, read_archive, write_archive};
use photocraft_local_store::{LoadedDraft, ReplayKind, ReplayRecord, SaveRequest, Store, StoreOptions};
use photocraft_party::gallery::{GALLERY_SCHEMA, GalleryBundle, GalleryContent, GalleryManifest, MAX_GALLERY_MANIFEST_BYTES};
use photocraft_party::replay_artifact::{ReplayArtifact, ReplayArtifactStep};
use photocraft_party::{AssetHash, EventId, ProjectId, ReplayId};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

pub(super) enum Request {
    Action { action: GalleryAction, source: Option<(ProjectId, bool)>, protected: BTreeSet<ProjectId> },
    ScopedAction { action: GalleryAction, source: Option<(ProjectId, bool)>, protected: BTreeSet<ProjectId>, files: Arc<dyn super::GalleryFileAccess> },
    Insert(Box<GalleryBundle>),
}
enum Response {
    Listed {
        items: Vec<GalleryItem>,
        used: u64,
        quota: u64,
    },
    SavedToGallery {
        id: ProjectId,
        title: String,
        updated_at: String,
        updated_existing: bool,
        removed_draft: bool,
        items: Vec<GalleryItem>,
        used: u64,
        quota: u64,
        deleted_keys: Vec<(ProjectId, bool)>,
    },
    Purged {
        deleted_keys: Vec<(ProjectId, bool)>,
        items: Vec<GalleryItem>,
        used: u64,
        quota: u64,
        notice: String,
    },
    Opened(Box<GalleryManifest>, bool, Vec<ProjectId>, crate::rooms::media::RoomImage, gallery_media::ReplayInfo, usize),
    OpenedEditable(Box<LoadedDraft>),
    Playback((ProjectId, bool), u8, bool, Option<String>),
    Notes(ProjectId, Vec<(photocraft_party::AssignmentId, String)>),
    Image((ProjectId, bool), crate::rooms::media::RoomImage, Option<gallery_media::ReplayInfo>),
    Thumbnail(ProjectId, bool, Vec<ProjectId>, crate::rooms::media::RoomImage),
    Edited(Box<LoadedDraft>),
    Notice(String),
    Error(String),
}
pub(super) struct Worker {
    tx: mpsc::SyncSender<Request>,
    rx: mpsc::Receiver<Response>,
}
fn error(error: impl std::fmt::Display) -> EngineError {
    EngineError::Other(error.to_string())
}

impl Worker {
    fn start(drafts: PathBuf, options: StoreOptions) -> Result<Self> {
        let (tx, requests) = mpsc::sync_channel(2);
        let (responses, rx) = mpsc::sync_channel(2);
        std::thread::Builder::new()
            .name("komaphone-gallery".into())
            .spawn(move || {
                let mut state = None;
                while let Ok(request) = requests.recv() {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Response> {
                        if state.is_none() {
                            let root = drafts.parent().ok_or_else(|| error("Gallery data directory is missing"))?;
                            state = Some(State {
                                local: Store::open(&drafts, options).map_err(error)?,
                                gallery: GalleryStore::open(root.join("gallery.sqlite3"), options).map_err(error)?,
                                active: None,
                                active_draft: false,
                                options,
                                gif: None,
                            });
                        }
                        state.as_mut().ok_or_else(|| error("Gallery store unavailable"))?.handle(request)
                    }))
                    .unwrap_or_else(|_| Err(error("Gallery stopped this operation unexpectedly; originals remain saved. Retry or restore a backup.")));
                    if responses.send(result.unwrap_or_else(|e| Response::Error(e.to_string()))).is_err() {
                        break;
                    }
                }
            })
            .map_err(error)?;
        Ok(Self { tx, rx })
    }
    pub(super) fn send(&self, request: Request) -> Result<()> {
        self.tx.try_send(request).map_err(|_| error("Gallery storage is busy; retry"))
    }
}

struct GifPreview {
    key: (ProjectId, u8, u8, bool),
    bytes: Vec<u8>,
}
struct State {
    local: Store,
    gallery: GalleryStore,
    active: Option<GalleryBundle>,
    active_draft: bool,
    options: StoreOptions,
    gif: Option<GifPreview>,
}
impl State {
    fn active(&self) -> Result<&GalleryBundle> {
        self.active.as_ref().ok_or_else(|| error("Open a gallery item first"))
    }
    fn bundle(&self, entry: ProjectId, draft: bool, history: bool) -> Result<GalleryBundle> {
        if draft {
            let mut bundle = project_bundle(&self.local, entry, history)?;
            if let Some(preference) = self.gallery.draft_preferences().map_err(error)?.into_iter().find(|p| p.id == entry) {
                if let Some(title) = preference.title {
                    bundle.manifest.title = title;
                }
                if let Some(fps) = preference.fps {
                    bundle.manifest.fps = fps;
                }
                if let Some(looping) = preference.looping {
                    bundle.manifest.looping = looping;
                }
            }
            Ok(bundle)
        } else {
            self.gallery.load(entry).map_err(error)
        }
    }
    fn sources(&self, bundle: &GalleryBundle, draft: bool) -> Result<Vec<ProjectId>> {
        let mut sources = bundle.manifest.source_projects();
        if !draft && let Some(info) = self.gallery.list(true).map_err(error)?.into_iter().find(|info| info.id == bundle.manifest.id) {
            sources.extend(info.source_projects);
        }
        sources.sort_unstable();
        sources.dedup();
        Ok(sources)
    }
    fn handle(&mut self, request: Request) -> Result<Response> {
        let (action, protected) = match request {
            Request::ScopedAction { action, source, protected, files } => {
                if matches!(action, GalleryAction::Export { .. })
                    && (source.is_none() || source != self.active.as_ref().map(|bundle| (bundle.manifest.id, self.active_draft)))
                {
                    return Err(error("Gallery selection changed; reopen the intended original"));
                }
                let mut prepared = files.prepare(&action)?;
                // Only the paths may change. The trusted adapter cannot change the selected
                // operation, format or media selection that passed the session's checks.
                let staged = match (&action, prepared.action()) {
                    (GalleryAction::Export { format, chain, frame, fps, looping, .. }, GalleryAction::Export { path, .. }) => {
                        GalleryAction::Export { format: format.clone(), path, chain: *chain, frame: *frame, fps: *fps, looping: *looping }
                    }
                    (GalleryAction::Import { .. }, GalleryAction::Import { path }) => GalleryAction::Import { path },
                    (GalleryAction::Backup { .. }, GalleryAction::Backup { path }) => GalleryAction::Backup { path },
                    (GalleryAction::Restore { .. }, GalleryAction::Restore { path, destination }) => GalleryAction::Restore { path, destination },
                    _ => return Err(error("Gallery file adapter changed the requested operation")),
                };
                let mut response = self.handle(Request::Action { action: staged, source, protected })?;
                prepared.finish()?;
                if let Response::Notice(notice) = &mut response {
                    *notice = match action {
                        GalleryAction::Export { path, .. } => format!("Export saved: {path}"),
                        GalleryAction::Backup { path } => format!("Backup saved: {path}"),
                        GalleryAction::Restore { destination, .. } => format!(
                            "Restored a separate data directory: {destination}. Close Komaphone and launch with --komaphone-data-dir pointing there; the current data is unchanged."
                        ),
                        _ => notice.clone(),
                    };
                }
                return Ok(response);
            }
            Request::Insert(bundle) => {
                gallery_media::validate_originals(&bundle)?;
                let info = self.gallery.insert(&bundle).map_err(error)?;
                return Ok(Response::Notice(format!("Saved locally: {}. Native artwork and replay are available offline.", info.title)));
            }
            Request::Action { action, source, protected } => {
                if matches!(
                    action,
                    GalleryAction::Render { .. }
                        | GalleryAction::Remix { .. }
                        | GalleryAction::Export { .. }
                        | GalleryAction::Playback { .. }
                        | GalleryAction::PreviewGif { .. }
                ) && (source.is_none() || source != self.active.as_ref().map(|bundle| (bundle.manifest.id, self.active_draft)))
                {
                    return Err(error("Gallery selection changed; reopen the intended original"));
                }
                (action, protected)
            }
        };
        match action {
            GalleryAction::Refresh => {
                let (items, used, quota) = self.list()?;
                Ok(Response::Listed { items, used, quota })
            }
            GalleryAction::Open { entry, draft } => {
                let bundle = self.bundle(entry, draft, true)?;
                let image = cover(&bundle)?;
                let replay = gallery_media::replay_info(&bundle, 0, 0)?;
                let frames = gallery_media::frame_count(&bundle, 0)?;
                let manifest = Box::new(bundle.manifest.clone());
                let sources = self.sources(&bundle, draft)?;
                self.active = Some(bundle);
                self.active_draft = draft;
                self.gif = None;
                Ok(Response::Opened(manifest, draft, sources, image, replay, frames))
            }
            GalleryAction::Update { entry, draft, title, favorite, trashed } => {
                let updated_at = if draft {
                    self.gallery.update_draft(entry, title.as_deref(), favorite, trashed).map_err(error)?;
                    None
                } else {
                    Some(self.gallery.update(entry, title.as_deref(), favorite, trashed).map_err(error)?.updated_at)
                };
                if self.active_draft == draft
                    && let Some(bundle) = self.active.as_mut().filter(|bundle| bundle.manifest.id == entry)
                {
                    if let Some(title) = title {
                        bundle.manifest.title = title;
                    }
                    if let Some(updated_at) = updated_at {
                        bundle.manifest.updated_at = updated_at;
                    }
                }
                let (items, used, quota) = self.list()?;
                Ok(Response::Listed { items, used, quota })
            }
            GalleryAction::SaveDraft { entry, title } => self.save_draft(entry, title, &protected),
            GalleryAction::OpenEditable { entry } => self.open_editable(entry),
            GalleryAction::Delete { entry, draft } => self.delete_item(entry, draft, &protected),
            GalleryAction::EmptyTrash => self.empty_trash(&protected),
            GalleryAction::ClearTemp => self.clear_temp(&protected),
            GalleryAction::Thumbnail { entry, draft } => {
                let bundle = self.bundle(entry, draft, false)?;
                Ok(Response::Thumbnail(entry, draft, self.sources(&bundle, draft)?, gallery_media::thumbnail(&bundle, 0, 192)?))
            }
            GalleryAction::Render { chain, frame, step, elapsed_ms, creature } => {
                let bundle = self.active()?;
                let image = if let Some(index) = step {
                    gallery_media::replay_step(bundle, chain, frame, index)?
                } else if let Some(elapsed) = elapsed_ms {
                    gallery_media::replay_image(bundle, chain, frame, elapsed)?
                } else if creature {
                    gallery_media::render_creature(bundle, chain)?
                } else {
                    gallery_media::render_frame(bundle, chain, frame)?
                };
                let replay_frame = if matches!(bundle.manifest.content, GalleryContent::Project { .. }) { 0 } else { frame };
                let replay = gallery_media::replay_info(bundle, chain, replay_frame)?;
                Ok(Response::Image((bundle.manifest.id, self.active_draft), image, Some(replay)))
            }
            GalleryAction::Remix { chain, frame } => {
                let (document, _) = gallery_media::remix(self.active()?, chain, frame)?;
                let project = photocraft_local_store::fresh_project_id().map_err(error)?;
                let request = SaveRequest {
                    project,
                    expected_revision: 0,
                    request_id: photocraft_local_store::fresh_event_id().map_err(error)?,
                    title: document.name.chars().take(24).collect(),
                    document,
                    event: ReplayRecord {
                        schema_version: 1,
                        command: "komaphone.gallery.remix".into(),
                        params: json!({"baseline":"Copied result; original history remains in source gallery"}),
                        elapsed_ms: 0,
                        kind: ReplayKind::Baseline,
                    },
                };
                self.local.save(&request).map_err(error)?;
                Ok(Response::Edited(Box::new(self.local.load(project).map_err(error)?)))
            }
            GalleryAction::Playback { fps, looping } => {
                if !(1..=24).contains(&fps) {
                    return Err(error("Playback must be 1–24 fps"));
                }
                let id = self.active()?.manifest.id;
                let updated_at = if self.active_draft {
                    self.gallery.set_draft_playback(id, fps, looping).map_err(error)?;
                    None
                } else {
                    Some(self.gallery.set_playback(id, fps, looping).map_err(error)?.updated_at)
                };
                if let Some(bundle) = &mut self.active {
                    bundle.manifest.fps = fps;
                    bundle.manifest.looping = looping;
                    if let Some(updated_at) = &updated_at {
                        bundle.manifest.updated_at = updated_at.clone();
                    }
                }
                Ok(Response::Playback((id, self.active_draft), fps, looping, updated_at))
            }
            GalleryAction::SetNotes { entry, notes } => {
                self.gallery.set_notes(entry, &notes).map_err(error)?;
                if !self.active_draft
                    && let Some(bundle) = self.active.as_mut().filter(|bundle| bundle.manifest.id == entry)
                {
                    bundle.manifest.notes = notes.clone();
                }
                Ok(Response::Notes(entry, notes))
            }
            GalleryAction::PreviewGif { chain, frame, fps, looping } => {
                let key = (self.active()?.manifest.id, chain, fps, looping);
                if self.gif.as_ref().is_none_or(|cached| cached.key != key) {
                    let bytes = gallery_media::gif_bytes(self.active()?, chain, gallery_media::AnimationOptions { fps, looping })?;
                    self.gif = Some(GifPreview { key, bytes });
                }
                let bytes = self.gif.as_ref().map(|cached| cached.bytes.as_slice()).ok_or_else(|| error("GIF preview unavailable"))?;
                Ok(Response::Image((self.active()?.manifest.id, self.active_draft), gallery_media::gif_preview(bytes, usize::from(frame))?, None))
            }
            GalleryAction::Export { format, path, chain, frame, fps, looping } => {
                let bundle = self.active()?;
                let path = Path::new(&path);
                match format.as_str() {
                    "archive" => {
                        if self.active_draft {
                            write_archive(bundle, path).map_err(error)?;
                        } else {
                            let id = bundle.manifest.id;
                            self.gallery.export_archive(id, path).map_err(error)?;
                        }
                    }
                    "png" => gallery_media::export_png(bundle, chain, frame, path)?,
                    "gif" => gallery_media::export_gif(bundle, chain, gallery_media::AnimationOptions { fps, looping }, path)?,
                    "sequence" => {
                        gallery_media::export_sequence(bundle, chain, path, |_, _| true)?;
                    }
                    _ => return Err(error("Choose archive, png, gif, or sequence")),
                }
                Ok(Response::Notice(format!("Export saved: {}", path.display())))
            }
            GalleryAction::Import { path } => {
                let info = self
                    .gallery
                    .import_archive_checked(Path::new(&path), |bundle| {
                        gallery_media::validate_originals(bundle).map_err(|e| photocraft_local_store::Error::Invalid(e.to_string()))
                    })
                    .map_err(error)?;
                self.handle(Request::Action { action: GalleryAction::Open { entry: info.id, draft: false }, source: None, protected })
            }
            GalleryAction::Backup { path } => {
                self.backup(Path::new(&path))?;
                Ok(Response::Notice(format!("Backup saved: {path}")))
            }
            GalleryAction::Restore { path, destination } => {
                restore_backup(Path::new(&path), Path::new(&destination), self.options)?;
                Ok(Response::Notice(format!(
                    "Restored a separate data directory: {destination}. Close Komaphone and launch with --komaphone-data-dir pointing there; the current data is unchanged."
                )))
            }
        }
    }

    fn save_draft(&mut self, entry: Option<ProjectId>, title: Option<String>, protected: &BTreeSet<ProjectId>) -> Result<Response> {
        let entry = entry.ok_or_else(|| error("Choose a drawing to save to gallery"))?;
        let updated_existing = self.gallery.list(true).map_err(error)?.iter().any(|item| item.id == entry);
        let mut bundle = self.bundle(entry, true, true)?;
        if let Some(title) = title.as_deref() {
            bundle.manifest.title = title.trim().to_owned();
        }
        let info = self.gallery.save_project(&bundle, title.as_deref()).map_err(error)?;
        let mut removed_draft = false;
        let mut deleted_keys = Vec::new();
        if !protected.contains(&entry) && self.local.delete_project(entry).is_ok() {
            let _ = self.gallery.remove_draft_preference(entry);
            removed_draft = true;
            deleted_keys.push((entry, true));
        }
        if self.active.as_ref().is_some_and(|active| active.manifest.id == entry) {
            if let Ok(saved) = self.gallery.load(entry) {
                self.active = Some(saved);
                self.active_draft = false;
            } else if removed_draft {
                self.active = None;
                self.active_draft = false;
            }
            self.gif = None;
        }
        let (items, used, quota) = self.list()?;
        Ok(Response::SavedToGallery {
            id: entry,
            title: info.title,
            updated_at: info.updated_at,
            updated_existing,
            removed_draft,
            items,
            used,
            quota,
            deleted_keys,
        })
    }

    fn open_editable(&mut self, entry: ProjectId) -> Result<Response> {
        match self.local.load(entry) {
            Ok(loaded) => Ok(Response::OpenedEditable(Box::new(loaded))),
            Err(photocraft_local_store::Error::NotFound) => {
                let bundle = self.gallery.load(entry).map_err(error)?;
                if !matches!(bundle.manifest.content, GalleryContent::Project { .. }) {
                    return Err(error("Saved games cannot be edited directly; use Remix to create a new drawing"));
                }
                restore_draft(&mut self.local, &bundle)?;
                let loaded = self.local.load(entry).map_err(error)?;
                Ok(Response::OpenedEditable(Box::new(loaded)))
            }
            Err(err) => Err(error(err)),
        }
    }

    fn delete_item(&mut self, entry: ProjectId, draft: bool, protected: &BTreeSet<ProjectId>) -> Result<Response> {
        if draft {
            if protected.contains(&entry) {
                return Err(error("Close this drawing in the Workroom before deleting it permanently"));
            }
            let trashed = self.gallery.draft_preferences().map_err(error)?.iter().any(|pref| pref.id == entry && pref.trashed);
            if !trashed {
                return Err(error("Move this item to Trash before deleting it permanently"));
            }
            self.local.delete_project(entry).map_err(error)?;
            self.gallery.remove_draft_preference(entry).map_err(error)?;
        } else {
            self.gallery.delete_entry(entry).map_err(error)?;
        }
        if self.active_draft == draft && self.active.as_ref().is_some_and(|bundle| bundle.manifest.id == entry) {
            self.active = None;
            self.gif = None;
        }
        let (items, used, quota) = self.list()?;
        let deleted_keys = vec![(entry, draft)];
        Ok(Response::Purged { deleted_keys, items, used, quota, notice: "Deleted permanently.".into() })
    }

    fn empty_trash(&mut self, protected: &BTreeSet<ProjectId>) -> Result<Response> {
        let deleted_saved = self.gallery.delete_trashed_entries().map_err(error)?;
        let prefs = self.gallery.draft_preferences().map_err(error)?;
        let local_ids: BTreeSet<ProjectId> = self.local.list().map_err(error)?.into_iter().map(|draft| draft.project).collect();
        let mut deletable_drafts = Vec::new();
        let mut deleted_existing_drafts = 0usize;
        let mut skipped_open = 0usize;
        for pref in prefs.into_iter().filter(|pref| pref.trashed) {
            if local_ids.contains(&pref.id) && protected.contains(&pref.id) {
                skipped_open = skipped_open.saturating_add(1);
                continue;
            }
            if local_ids.contains(&pref.id) {
                deleted_existing_drafts = deleted_existing_drafts.saturating_add(1);
            }
            deletable_drafts.push(pref.id);
        }
        if !deletable_drafts.is_empty() {
            let existing_only: Vec<ProjectId> = deletable_drafts.iter().copied().filter(|id| local_ids.contains(id)).collect();
            if !existing_only.is_empty() {
                self.local.delete_projects(&existing_only).map_err(error)?;
            }
            self.gallery.remove_draft_preferences(&deletable_drafts).map_err(error)?;
        }
        let deleted_keys: Vec<(ProjectId, bool)> =
            deleted_saved.iter().copied().map(|id| (id, false)).chain(deletable_drafts.iter().copied().map(|id| (id, true))).collect();
        if self.active.as_ref().is_some_and(|bundle| deleted_keys.contains(&(bundle.manifest.id, self.active_draft))) {
            self.active = None;
            self.gif = None;
        }
        let (items, used, quota) = self.list()?;
        let deleted_total = deleted_saved.len().saturating_add(deleted_existing_drafts);
        let notice = if deleted_total == 0 && skipped_open == 0 {
            "Trash is already empty.".into()
        } else if skipped_open > 0 {
            format!("Permanently deleted {deleted_total} item(s). Kept {skipped_open} open drawing(s); close them in the Workroom to delete.")
        } else {
            format!("Permanently deleted {deleted_total} item(s) from Trash.")
        };
        Ok(Response::Purged { deleted_keys, items, used, quota, notice })
    }

    fn clear_temp(&mut self, protected: &BTreeSet<ProjectId>) -> Result<Response> {
        let mut deleted_keys: Vec<(ProjectId, bool)> = self.auto_prune_drafts(protected).into_iter().map(|id| (id, true)).collect();
        let saved_checkpoints = self.gallery.saved_project_checkpoints().unwrap_or_default();
        let prefs = self.gallery.draft_preferences().map_err(error)?;
        let mut synced_to_prune = Vec::new();
        let mut trashed_count = 0usize;
        let mut skipped_open = 0usize;
        for draft in self.local.list().map_err(error)? {
            if prefs.iter().any(|pref| pref.id == draft.project && pref.trashed) {
                continue;
            }
            if protected.contains(&draft.project) {
                skipped_open = skipped_open.saturating_add(1);
                continue;
            }
            let matches_saved = saved_checkpoints
                .get(&draft.project)
                .is_some_and(|saved_hash| self.local.head_checkpoint_hash(draft.project).ok().flatten() == Some(*saved_hash));
            if matches_saved {
                synced_to_prune.push(draft.project);
            } else {
                self.gallery.update_draft(draft.project, None, None, Some(true)).map_err(error)?;
                trashed_count = trashed_count.saturating_add(1);
            }
        }
        if !synced_to_prune.is_empty() && self.local.delete_projects(&synced_to_prune).is_ok() {
            let _ = self.gallery.remove_draft_preferences(&synced_to_prune);
            for id in synced_to_prune {
                deleted_keys.push((id, true));
            }
        }
        if self.active.as_ref().is_some_and(|bundle| deleted_keys.contains(&(bundle.manifest.id, self.active_draft))) {
            self.active = None;
            self.gif = None;
        }
        let pruned_count = deleted_keys.len();
        let (items, used, quota) = self.list()?;
        let notice = if trashed_count > 0 && skipped_open > 0 {
            format!("Moved {trashed_count} temporary draft(s) to Trash. Kept {skipped_open} open drawing(s).")
        } else if trashed_count > 0 {
            format!("Moved {trashed_count} temporary draft(s) to Trash. You can restore them from Trash.")
        } else if pruned_count > 0 {
            format!("Cleaned up {pruned_count} temporary draft(s).")
        } else if skipped_open > 0 {
            "No closed temporary drafts to clear. Open drawings stay in the Workroom.".into()
        } else {
            "Temp is already clean.".into()
        };
        Ok(Response::Purged { deleted_keys, items, used, quota, notice })
    }

    fn auto_prune_drafts(&mut self, protected: &BTreeSet<ProjectId>) -> Vec<ProjectId> {
        let prefs = self.gallery.draft_preferences().unwrap_or_default();
        let covered = self.gallery.covered_game_projects().unwrap_or_default();
        let Ok(drafts) = self.local.list() else {
            return Vec::new();
        };
        let mut to_delete = Vec::new();
        for draft in drafts {
            if protected.contains(&draft.project) {
                continue;
            }
            if prefs.iter().any(|pref| pref.id == draft.project && (pref.trashed || pref.favorite || pref.title.is_some())) {
                continue;
            }
            if self.local.is_untouched_blank(draft.project).unwrap_or(false) || covered.contains(&draft.project) {
                to_delete.push(draft.project);
            }
        }
        if to_delete.is_empty() {
            return Vec::new();
        }
        if self.local.delete_projects(&to_delete).is_err() {
            return Vec::new();
        }
        let _ = self.gallery.remove_draft_preferences(&to_delete);
        if self.active_draft && self.active.as_ref().is_some_and(|bundle| to_delete.contains(&bundle.manifest.id)) {
            self.active = None;
            self.gif = None;
        }
        to_delete
    }

    fn list(&self) -> Result<(Vec<GalleryItem>, u64, u64)> {
        let mut items = Vec::new();
        let preferences = self.gallery.draft_preferences().map_err(error)?;
        let covered = self.gallery.covered_game_projects().unwrap_or_default();
        let saved_checkpoints = self.gallery.saved_project_checkpoints().unwrap_or_default();
        let mut draft_heads: BTreeMap<ProjectId, AssetHash> = BTreeMap::new();
        for draft in self.local.list().map_err(error)? {
            let preference = preferences.iter().find(|p| p.id == draft.project);
            let saved_hash = saved_checkpoints.get(&draft.project).copied();
            let head_hash = if saved_hash.is_some() { self.local.head_checkpoint_hash(draft.project).ok().flatten() } else { None };
            if let Some(hash) = head_hash {
                draft_heads.insert(draft.project, hash);
            }
            let has_saved_copy = saved_hash.is_some();
            let saved_matches_head = saved_hash.is_some() && head_hash == saved_hash;
            let has_unsaved_edits = has_saved_copy && !saved_matches_head;
            let untouched_blank = self.local.is_untouched_blank(draft.project).unwrap_or(false);
            let covered_by_game = covered.contains(&draft.project);
            items.push(GalleryItem {
                id: draft.project,
                title: preference.and_then(|p| p.title.clone()).unwrap_or(draft.title),
                kind: "draft".into(),
                authors: vec!["You".into()],
                updated_at: draft.updated_at,
                favorite: preference.is_some_and(|p| p.favorite),
                trashed: preference.is_some_and(|p| p.trashed),
                complete: true,
                draft: true,
                revision: draft.revision,
                state: if has_unsaved_edits { "Unsaved changes to saved drawing" } else { "Temporary draft" }.into(),
                source_projects: vec![draft.project],
                saved_matches_head,
                has_saved_copy,
                has_unsaved_edits,
                untouched_blank,
                covered_by_game,
            });
        }
        for entry in self.gallery.list(true).map_err(error)? {
            let saved_hash = saved_checkpoints.get(&entry.id).copied();
            let has_unsaved_edits =
                entry.kind == GalleryKind::Project && !entry.trashed && draft_heads.get(&entry.id).is_some_and(|head| Some(*head) != saved_hash);
            let state = match entry.kind {
                GalleryKind::Project => {
                    if has_unsaved_edits {
                        "Saved drawing · newer draft in Temp"
                    } else {
                        "Saved drawing"
                    }
                }
                GalleryKind::Animation | GalleryKind::ExquisiteCorpse => {
                    if entry.complete {
                        "Completed game"
                    } else {
                        "Incomplete game"
                    }
                }
            };
            items.push(GalleryItem {
                id: entry.id,
                title: entry.title,
                kind: match entry.kind {
                    GalleryKind::Project => "project",
                    GalleryKind::Animation => "animation",
                    GalleryKind::ExquisiteCorpse => "creature",
                }
                .into(),
                authors: entry.authors.into_iter().map(|author| author.display_name).collect(),
                updated_at: entry.updated_at,
                favorite: entry.favorite,
                trashed: entry.trashed,
                complete: entry.complete,
                draft: false,
                revision: 0,
                state: state.into(),
                source_projects: entry.source_projects,
                saved_matches_head: false,
                has_saved_copy: false,
                has_unsaved_edits,
                untouched_blank: false,
                covered_by_game: false,
            });
        }
        items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.id.cmp(&b.id)));
        let local = self.local.usage().map_err(error)?;
        let gallery = self.gallery.usage().map_err(error)?;
        Ok((items, local.stored_bytes.saturating_add(gallery.stored_bytes), local.quota_bytes.saturating_add(gallery.quota_bytes)))
    }

    fn backup(&mut self, path: &Path) -> Result<()> {
        if path.exists() {
            return Err(error("Choose a new backup directory; existing files are preserved"));
        }
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
        let stage = tempfile::Builder::new().prefix(".komaphone-backup-").tempdir_in(parent).map_err(error)?;
        self.gallery.backup(stage.path().join("gallery.komabackup")).map_err(error)?;
        let drafts = self.local.list().map_err(error)?;
        let mut projects = Vec::new();
        for draft in drafts {
            let bundle = self.bundle(draft.project, true, true)?;
            let file = format!("{}.komarchive", draft.project.as_str());
            write_archive(&bundle, stage.path().join(&file)).map_err(error)?;
            projects.push(draft.project);
        }
        let bytes = serde_json::to_vec(&BackupManifest { schema_version: 1, projects }).map_err(error)?;
        gallery_media::write_new_file(&stage.path().join("backup.json"), &bytes)?;
        publish_directory(stage, path)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BackupManifest {
    schema_version: u16,
    projects: Vec<ProjectId>,
}

fn publish_directory(stage: tempfile::TempDir, path: &Path) -> Result<()> {
    if path.exists() {
        return Err(error("The destination now exists; choose a new directory"));
    }
    gallery_media::publish_new_directory(stage.path(), path)?;
    Ok(())
}

fn restore_backup(path: &Path, destination: &Path, options: StoreOptions) -> Result<()> {
    if destination.exists() {
        return Err(error("Restore requires a new data directory"));
    }
    use std::io::Read;
    let file = std::fs::File::open(path.join("backup.json")).map_err(error)?;
    let mut bytes = Vec::new();
    file.take(128 * 1024 + 1).read_to_end(&mut bytes).map_err(error)?;
    if bytes.len() > 128 * 1024 {
        return Err(error("Backup index exceeds its limit"));
    }
    let manifest: BackupManifest = serde_json::from_slice(&bytes).map_err(error)?;
    if manifest.schema_version != 1
        || manifest.projects.len() > 1024
        || manifest.projects.iter().collect::<std::collections::BTreeSet<_>>().len() != manifest.projects.len()
    {
        return Err(error("Unsupported or invalid backup index; original backup preserved"));
    }
    let parent = destination.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let stage = tempfile::Builder::new().prefix(".komaphone-restore-").tempdir_in(parent).map_err(error)?;
    {
        let mut gallery = GalleryStore::open(stage.path().join("gallery.sqlite3"), options).map_err(error)?;
        gallery
            .restore_backup_checked(path.join("gallery.komabackup"), |bundle| {
                gallery_media::validate_originals(bundle).map_err(|e| photocraft_local_store::Error::Invalid(e.to_string()))
            })
            .map_err(error)?;
        let mut local = Store::open(stage.path().join("drafts.sqlite3"), options).map_err(error)?;
        for project in manifest.projects {
            let bundle = read_archive(path.join(format!("{}.komarchive", project.as_str()))).map_err(error)?;
            if bundle.manifest.id != project {
                return Err(error("Backup draft identity mismatch"));
            }
            restore_draft(&mut local, &bundle)?;
        }
    }
    publish_directory(stage, destination)
}

fn restore_draft(store: &mut Store, bundle: &GalleryBundle) -> Result<()> {
    let GalleryContent::Project { checkpoint, replay, .. } = &bundle.manifest.content else {
        return Err(error("Backup contains a game in its draft table"));
    };
    if let Some(replay) = replay
        && !replay.steps.is_empty()
    {
        for step in &replay.steps {
            let bytes = checkpoint_bytes(bundle, replay, step.checkpoint.asset)?;
            let document = photocraft_format::load_from_bytes_with(bytes, &load_options()).map_err(error)?;
            let kind = match step.kind.as_str() {
                "baseline" => ReplayKind::Baseline,
                "resolved" => ReplayKind::Resolved,
                "undo" => ReplayKind::Undo,
                "redo" => ReplayKind::Redo,
                _ => ReplayKind::Checkpoint,
            };
            store
                .save(&SaveRequest {
                    project: bundle.manifest.id,
                    expected_revision: step.revision.saturating_sub(1),
                    request_id: photocraft_local_store::fresh_event_id().map_err(error)?,
                    title: bundle.manifest.title.clone(),
                    document,
                    event: ReplayRecord { schema_version: 1, command: step.command.clone(), params: step.params.clone(), elapsed_ms: step.elapsed_ms, kind },
                })
                .map_err(error)?;
        }
    } else {
        let document = photocraft_format::load_from_bytes_with(bundle.object(*checkpoint).map_err(error)?, &load_options()).map_err(error)?;
        store
            .save(&SaveRequest {
                project: bundle.manifest.id,
                expected_revision: 0,
                request_id: photocraft_local_store::fresh_event_id().map_err(error)?,
                title: bundle.manifest.title.clone(),
                document,
                event: ReplayRecord { schema_version: 1, command: "legacy.import".into(), params: json!({}), elapsed_ms: 0, kind: ReplayKind::Baseline },
            })
            .map_err(error)?;
    }
    Ok(())
}

fn load_options() -> photocraft_format::LoadOptions {
    photocraft_format::LoadOptions {
        max_manifest_bytes: 8 * 1024 * 1024,
        max_blob_bytes: 64 * 1024 * 1024,
        max_total_bytes: 256 * 1024 * 1024,
        max_canvas_side: 8192,
        max_canvas_pixels: 16_777_216,
        preserve_ids: true,
    }
}
fn checkpoint_bytes<'a>(bundle: &'a GalleryBundle, replay: &ReplayArtifact, asset: photocraft_party::AssetRef) -> Result<&'a [u8]> {
    if bundle.objects.contains_key(&asset.hash) {
        return bundle.object(asset).map_err(error);
    }
    for pack in &replay.packs {
        if let Some((_, bytes)) = photocraft_party::replay_artifact::unpack_checkpoints(bundle.object(*pack).map_err(error)?)
            .map_err(error)?
            .into_iter()
            .find(|(reference, _)| *reference == asset)
        {
            return Ok(bytes);
        }
    }
    Err(error("Missing replay checkpoint"))
}

fn short_hash(bytes: &[u8]) -> Result<String> {
    format!("{:x}", Sha256::digest(bytes)).get(..32).map(str::to_owned).ok_or_else(|| error("Invalid project identity"))
}
fn project_bundle(store: &Store, project: ProjectId, history: bool) -> Result<GalleryBundle> {
    let loaded = store.load(project).map_err(error)?;
    if let Some(warning) = &loaded.recovery_warning {
        return Err(error(format!(
            "A complete archive cannot be made from a recovered earlier revision: {warning}. Keep the original data directory and save the recovered draft as a new project."
        )));
    }
    let mut objects = BTreeMap::new();
    let checkpoint = crate::rooms::drafts::checkpoint(&mut objects, &loaded.document).map_err(error)?;
    let provenance = loaded
        .document
        .metadata
        .text
        .iter()
        .find(|(key, _)| key == "Komaphone remix source")
        .map(|(_, value)| serde_json::from_str(value))
        .transpose()
        .map_err(error)?;
    let fps = loaded.document.timeline.as_ref().map_or(6, |timeline| timeline.fps.round().clamp(1.0, 24.0) as u8);
    let looping = loaded.document.metadata.text.iter().find(|(key, _)| key == "Komaphone animation looping").is_none_or(|(_, value)| value != "false");
    let mut replay = ReplayArtifact {
        schema_version: 1,
        replay: ReplayId::new(&short_hash(project.as_str().as_bytes())?).map_err(error)?,
        project,
        packs: Vec::new(),
        steps: Vec::new(),
    };
    let mut metadata_bytes = 0usize;
    if history {
        for revision in 1..=loaded.info.revision {
            let (document, _) = store.load_revision(project, revision).map_err(error)?;
            let checkpoint = crate::rooms::drafts::checkpoint(&mut objects, &document).map_err(error)?;
            let record = store.replay_page(project, revision - 1, 1).map_err(error)?.into_iter().next().ok_or_else(|| error("Missing local replay record"))?;
            let step = ReplayArtifactStep {
                event: EventId::new(&short_hash(format!("{}:{revision}", project.as_str()).as_bytes())?).map_err(error)?,
                revision,
                elapsed_ms: record.elapsed_ms,
                command: record.command,
                params: record.params,
                kind: match record.kind {
                    ReplayKind::Baseline => "baseline",
                    ReplayKind::Resolved => "resolved",
                    ReplayKind::Checkpoint => "checkpoint",
                    ReplayKind::Undo => "undo",
                    ReplayKind::Redo => "redo",
                }
                .into(),
                checkpoint,
            };
            metadata_bytes = metadata_bytes.saturating_add(serde_json::to_vec(&step).map_err(error)?.len());
            if metadata_bytes > MAX_GALLERY_MANIFEST_BYTES.saturating_sub(4096) {
                return Err(error("Replay metadata exceeds the portable archive limit; local history is preserved"));
            }
            replay.steps.push(step);
        }
        let first = replay.steps.first().ok_or_else(|| error("Missing local replay baseline"))?.checkpoint;
        replay.packs = crate::rooms::drafts::pack_history(&mut objects, [first.asset.hash, checkpoint.asset.hash]).map_err(error)?;
    }
    let packed: std::collections::BTreeSet<_> = replay
        .packs
        .iter()
        .map(|pack| pack.hash)
        .chain(std::iter::once(checkpoint.asset.hash))
        .chain(replay.steps.first().map(|step| step.checkpoint.asset.hash))
        .collect();
    let direct_assets = if !replay.packs.is_empty() {
        objects
            .iter()
            .filter(|(hash, _)| !packed.contains(hash))
            .map(|(hash, bytes)| photocraft_party::AssetRef { hash: *hash, bytes: bytes.len() as u64 })
            .collect()
    } else {
        Vec::new()
    };
    let bundle = GalleryBundle {
        manifest: GalleryManifest {
            schema_version: GALLERY_SCHEMA,
            id: project,
            title: loaded.info.title,
            created_at: loaded.info.updated_at.clone(),
            updated_at: loaded.info.updated_at,
            authors: Vec::new(),
            content: GalleryContent::Project { checkpoint: checkpoint.asset, direct_assets, replay: history.then_some(Box::new(replay)) },
            provenance,
            fps,
            looping,
            notes: Vec::new(),
        },
        objects,
    };
    bundle.validate().map_err(error)?;
    Ok(bundle)
}

fn cover(bundle: &GalleryBundle) -> Result<crate::rooms::media::RoomImage> {
    if matches!(&bundle.manifest.content, GalleryContent::Game { room } if room.mode == photocraft_party::Mode::ExquisiteCorpse) {
        gallery_media::render_creature(bundle, 0)
    } else {
        gallery_media::render_frame(bundle, 0, 0)
    }
}

impl Session {
    pub(crate) fn enable_gallery(&mut self, database: PathBuf, options: StoreOptions) -> Result<()> {
        self.gallery.worker = Some(Worker::start(database, options)?);
        self.gallery.configured = true;
        Ok(())
    }
    pub(super) fn poll_gallery_native(&mut self) {
        loop {
            let response = self.gallery.worker.as_ref().map(|worker| worker.rx.try_recv());
            let Some(Ok(response)) = response else {
                break;
            };
            self.gallery.busy = false;
            match response {
                Response::Listed { items, used, quota } => {
                    let protected = self.active_protected_projects();
                    for item in &items {
                        if item.draft && !item.trashed && !item.favorite && (item.untouched_blank || item.covered_by_game) && !protected.contains(&item.id) {
                            self.queue_safe_local_prune(item.id);
                        }
                    }
                    if let Some(active) = &mut self.gallery.active
                        && let Some(item) = items.iter().find(|item| item.id == active.id && item.draft == self.gallery.active_draft)
                    {
                        let active = Arc::make_mut(active);
                        active.title = item.title.clone();
                        active.updated_at = item.updated_at.clone();
                    }
                    self.gallery.items = items;
                    self.gallery.usage_bytes = used;
                    self.gallery.quota_bytes = quota;
                }
                Response::SavedToGallery { id, title, updated_at, updated_existing, removed_draft, items, used, quota, deleted_keys } => {
                    for key in std::iter::once((id, false)).chain(deleted_keys.iter().copied()) {
                        self.gallery.thumbnails.remove(&key);
                        self.gallery.thumbnail_sources.remove(&key);
                        self.gallery.thumbnail_order.retain(|existing| *existing != key);
                        if key.1 {
                            self.local_projects.drafts.retain(|draft| draft.project != key.0);
                        }
                    }
                    if let Some(active) = self.gallery.active.as_mut().filter(|active| active.id == id) {
                        if self.gallery.active_draft && removed_draft {
                            self.gallery.active_draft = false;
                        }
                        let active = Arc::make_mut(active);
                        active.title = title.clone();
                        active.updated_at = updated_at;
                    } else if self.gallery.active.as_ref().is_some_and(|active| deleted_keys.contains(&(active.id, self.gallery.active_draft))) {
                        self.gallery.active = None;
                        self.gallery.active_sources.clear();
                        self.gallery.image = None;
                        self.gallery.replay = None;
                    }
                    self.gallery.items = items;
                    self.gallery.usage_bytes = used;
                    self.gallery.quota_bytes = quota;
                    self.gallery.notice = Some(if updated_existing { format!("Updated in gallery: {title}") } else { format!("Saved to gallery: {title}") });
                }
                Response::Purged { deleted_keys, items, used, quota, notice } => {
                    for key in &deleted_keys {
                        self.gallery.thumbnails.remove(key);
                        self.gallery.thumbnail_sources.remove(key);
                        self.gallery.thumbnail_order.retain(|existing| existing != key);
                        if key.1 {
                            self.local_projects.drafts.retain(|draft| draft.project != key.0);
                        }
                    }
                    if self.gallery.active.as_ref().is_some_and(|active| deleted_keys.contains(&(active.id, self.gallery.active_draft))) {
                        self.gallery.active = None;
                        self.gallery.active_sources.clear();
                        self.gallery.image = None;
                        self.gallery.replay = None;
                    }
                    self.gallery.items = items;
                    self.gallery.usage_bytes = used;
                    self.gallery.quota_bytes = quota;
                    self.gallery.notice = Some(notice);
                }
                Response::Opened(manifest, draft, sources, image, replay, frames) => {
                    if !self.gallery_sources_allowed(&sources) {
                        self.gallery.active = None;
                        self.gallery.active_sources.clear();
                        self.gallery.image = None;
                        self.gallery.replay = None;
                        self.gallery.error = Some("This assignment history is private until reveal".into());
                        continue;
                    }
                    let refresh = !self.gallery.items.iter().any(|item| item.id == manifest.id && item.draft == draft);
                    self.gallery.active = Some(Arc::from(manifest));
                    self.gallery.active_draft = draft;
                    self.gallery.active_sources = sources;
                    self.gallery.image = Some(Arc::new(image));
                    self.gallery.replay = Some(replay);
                    self.gallery.frame_count = frames;
                    self.gallery.image_revision = self.gallery.image_revision.saturating_add(1);
                    // Import can open a newly stored original before it has a list row.
                    // Keep the operation busy until navigation, metadata controls and usage
                    // reflect the committed store, distinguishing a same-ID live draft.
                    if refresh && let Err(error) = self.gallery_action(GalleryAction::Refresh) {
                        self.gallery.error = Some(error.to_string());
                    }
                }
                Response::OpenedEditable(loaded) => {
                    self.install_local_draft(*loaded);
                    self.gallery.opened_document = true;
                    self.gallery.notice = Some("Opened in Workroom for editing.".into());
                }
                Response::Playback(source, fps, looping, updated_at) => {
                    if let Some(active) = self.gallery.active.as_mut().filter(|active| (active.id, self.gallery.active_draft) == source) {
                        let active = Arc::make_mut(active);
                        active.fps = fps;
                        active.looping = looping;
                        if let Some(updated_at) = updated_at {
                            active.updated_at = updated_at;
                        }
                    }
                }
                Response::Notes(id, notes) => {
                    if !self.gallery.active_draft
                        && let Some(active) = self.gallery.active.as_mut().filter(|active| active.id == id)
                    {
                        Arc::make_mut(active).notes = notes;
                    }
                }
                Response::Image(source, image, replay) => {
                    if self.gallery.active.as_ref().is_some_and(|active| (active.id, self.gallery.active_draft) == source) && self.gallery_active_allowed() {
                        self.gallery.image = Some(Arc::new(image));
                        self.gallery.replay = replay;
                        self.gallery.image_revision = self.gallery.image_revision.saturating_add(1);
                    }
                }
                Response::Thumbnail(id, draft, sources, image) => {
                    if !self.gallery_sources_allowed(&sources) {
                        self.gallery.error = Some("This assignment history is private until reveal".into());
                        continue;
                    }
                    let key = (id, draft);
                    self.gallery.thumbnail_order.retain(|existing| *existing != key);
                    self.gallery.thumbnail_order.push_back(key);
                    while self.gallery.thumbnail_order.len() > 64 {
                        if let Some(old) = self.gallery.thumbnail_order.pop_front() {
                            self.gallery.thumbnails.remove(&old);
                            self.gallery.thumbnail_sources.remove(&old);
                        }
                    }
                    self.gallery.thumbnails.insert(key, Arc::new(image));
                    self.gallery.thumbnail_sources.insert(key, sources);
                }
                Response::Edited(loaded) => {
                    self.install_local_draft(*loaded);
                    self.gallery.opened_document = true;
                    self.gallery.notice = Some("Remix saved as a new editable draft. The source and its history are unchanged.".into());
                }
                Response::Notice(notice) => {
                    self.gallery.notice = Some(notice);
                    let _ = self.gallery_action(GalleryAction::Refresh);
                }
                Response::Error(error) => self.gallery.error = Some(error),
            }
        }
        if !self.gallery_active_allowed() {
            self.gallery.active = None;
            self.gallery.active_sources.clear();
            self.gallery.image = None;
            self.gallery.replay = None;
        }
        let private: Vec<_> = self
            .gallery
            .thumbnails
            .keys()
            .copied()
            .filter(|(id, draft)| {
                self.gallery.thumbnail_sources.get(&(*id, *draft)).is_some_and(|sources| !self.gallery_sources_allowed(sources))
                    || (*draft && !self.gallery_draft_allowed(*id))
                    || self.gallery.items.iter().find(|item| item.id == *id && item.draft == *draft).is_some_and(|item| !self.gallery_item_allowed(item))
            })
            .collect();
        for key in private {
            self.gallery.thumbnails.remove(&key);
            self.gallery.thumbnail_sources.remove(&key);
        }
        if !self.gallery.busy()
            && let Some((entry, notes)) = self.gallery.incoming_notes.take()
            && let Err(error) = self.gallery_action(GalleryAction::SetNotes { entry, notes: notes.clone() })
        {
            self.gallery.incoming_notes = Some((entry, notes));
            self.gallery.error = Some(error.to_string());
        }
        if !self.gallery.busy
            && let Some(bundle) = self.gallery.incoming.take()
        {
            if let Some(worker) = &self.gallery.worker {
                match worker.tx.try_send(Request::Insert(bundle)) {
                    Ok(()) => self.gallery.busy = true,
                    Err(mpsc::TrySendError::Full(Request::Insert(bundle)) | mpsc::TrySendError::Disconnected(Request::Insert(bundle))) => {
                        self.gallery.incoming = Some(bundle);
                        self.gallery.error = Some("Gallery store is busy; complete original retained for retry".into());
                    }
                    Err(_) => self.gallery.error = Some("Gallery storage is unavailable".into()),
                }
            } else {
                self.gallery.incoming = Some(bundle);
            }
        }
    }
}
