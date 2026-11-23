use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};

use photocraft_local_store::gallery::GalleryStore;
use photocraft_local_store::{DraftInfo, LoadedDraft, SaveAck, SaveRequest, Store, StoreOptions};
use photocraft_party::contracts::{EventId, ProjectId};

use super::{LocalStatus, PendingSave, Session};
use crate::{EngineError, Result};

pub(super) enum Request {
    Save { save: Box<SaveRequest>, dedup_existing: bool, exclude_projects: Vec<ProjectId> },
    List,
    Open(ProjectId),
    OpenAssigned { project: ProjectId, mode: photocraft_party::Mode },
    PrepareRoom(Box<crate::rooms::drafts::PrepareRequest>),
    PruneClosed(Vec<ProjectId>),
}
enum Response {
    Saved(std::result::Result<(SaveAck, Option<u64>), Failure>),
    Listed(std::result::Result<(Vec<DraftInfo>, photocraft_local_store::Usage), String>),
    Opened(Box<std::result::Result<LoadedDraft, Failure>>),
    Prepared(Box<std::result::Result<crate::rooms::drafts::Prepared, String>>),
    Pruned(Vec<ProjectId>),
}

struct Failure {
    message: String,
    global: bool,
}

impl Failure {
    fn global(message: String) -> Self {
        Self { message, global: true }
    }
    fn local(message: String) -> Self {
        Self { message, global: false }
    }
    fn save(error: photocraft_local_store::Error) -> Self {
        let global = matches!(error, photocraft_local_store::Error::Storage(_) | photocraft_local_store::Error::Full);
        Self { message: error.to_string(), global }
    }
}

pub(super) struct Worker {
    tx: SyncSender<Request>,
    rx: Receiver<Response>,
}

impl Worker {
    fn start(path: PathBuf, options: StoreOptions) -> Result<Self> {
        let (tx, requests) = mpsc::sync_channel(8);
        let (responses, rx) = mpsc::sync_channel(8);
        std::thread::Builder::new()
            .name("komaphone-local-store".into())
            .spawn(move || {
                let mut store = None;
                let gallery_path = path.parent().map(|root| root.join("gallery.sqlite3"));
                while let Ok(request) = requests.recv() {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        if store.is_none() {
                            store = Some(Store::open(&path, options).map_err(|e| e.to_string())?);
                        }
                        let store = store.as_mut().ok_or("Local store unavailable")?;
                        Ok::<_, String>(match &request {
                            Request::Save { save, dedup_existing, exclude_projects } => {
                                if *dedup_existing
                                    && save.expected_revision == 0
                                    && let Ok(Some((existing_project, existing_revision, existing_elapsed_ms))) =
                                        store.find_project_matching_document(&save.document, exclude_projects)
                                {
                                    return Ok(Response::Saved(Ok((
                                        SaveAck { project: existing_project, revision: existing_revision, event_id: save.request_id, device_sequence: 0 },
                                        Some(existing_elapsed_ms),
                                    ))));
                                }
                                Response::Saved(store.save(save).map(|ack| (ack, None)).map_err(Failure::save))
                            }
                            Request::List => {
                                Response::Listed(store.list().and_then(|drafts| store.usage().map(|usage| (drafts, usage))).map_err(|e| e.to_string()))
                            }
                            Request::Open(project) => Response::Opened(Box::new(store.load(*project).map_err(|e| Failure::local(e.to_string())))),
                            Request::OpenAssigned { project, mode } => {
                                Response::Opened(Box::new(crate::rooms::drafts::open(store, *project, *mode).map_err(Failure::local)))
                            }
                            Request::PrepareRoom(request) => Response::Prepared(Box::new(crate::rooms::drafts::prepare(store, request))),
                            Request::PruneClosed(projects) => Response::Pruned(prune_closed(store, gallery_path.as_deref(), options, projects)),
                        })
                    }))
                    .unwrap_or_else(|_| Err("The local save worker stopped this operation unexpectedly; retry the retained draft".into()));
                    let response = result.unwrap_or_else(|error| match request {
                        Request::Save { .. } => Response::Saved(Err(Failure::global(error))),
                        Request::List => Response::Listed(Err(error)),
                        Request::Open(_) => Response::Opened(Box::new(Err(Failure::global(error)))),
                        Request::OpenAssigned { .. } => Response::Opened(Box::new(Err(Failure::global(error)))),
                        Request::PrepareRoom(_) => Response::Prepared(Box::new(Err(error))),
                        Request::PruneClosed(_) => Response::Pruned(Vec::new()),
                    });
                    if responses.send(response).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| EngineError::Other(format!("Cannot start local save worker: {e}")))?;
        Ok(Self { tx, rx })
    }
}

fn prune_closed(store: &mut Store, gallery_path: Option<&std::path::Path>, options: StoreOptions, projects: &[ProjectId]) -> Vec<ProjectId> {
    if projects.is_empty() {
        return Vec::new();
    }
    let mut gallery = gallery_path.filter(|path| path.exists()).and_then(|path| GalleryStore::open(path, options).ok());
    let prefs = gallery.as_ref().and_then(|g| g.draft_preferences().ok()).unwrap_or_default();
    let covered = gallery.as_ref().and_then(|g| g.covered_game_projects().ok()).unwrap_or_default();
    let saved_checkpoints = gallery.as_ref().and_then(|g| g.saved_project_checkpoints().ok()).unwrap_or_default();
    let mut to_delete = Vec::new();
    for &project in projects {
        let pref = prefs.iter().find(|p| p.id == project);
        if pref.is_some_and(|p| p.trashed) {
            continue;
        }
        let has_custom_meta = pref.is_some_and(|p| p.title.is_some() || p.favorite);
        let matches_saved = saved_checkpoints.get(&project).is_some_and(|saved_hash| store.head_checkpoint_hash(project).ok().flatten() == Some(*saved_hash));
        let is_blank = !has_custom_meta && store.is_untouched_blank(project).unwrap_or(false);
        let is_covered = !has_custom_meta && covered.contains(&project);
        if matches_saved || is_blank || is_covered {
            to_delete.push(project);
        }
    }
    if to_delete.is_empty() {
        return Vec::new();
    }
    if store.delete_projects(&to_delete).is_err() {
        return Vec::new();
    }
    if let Some(gallery) = gallery.as_mut() {
        let _ = gallery.remove_draft_preferences(&to_delete);
    }
    to_delete
}

fn random_hex() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| EngineError::Other(format!("Cannot allocate a project identity: {e}")))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

impl Session {
    /// Native application-data root; tests pass an isolated path. No I/O runs on this thread.
    pub fn enable_local_projects(&mut self, database: PathBuf, options: StoreOptions) -> Result<()> {
        if self.local_projects.available() {
            return Err(EngineError::Other("Local storage is already configured".into()));
        }
        self.enable_gallery(database.clone(), options)?;
        self.local_projects.worker = Some(Worker::start(database, options)?);
        self.local_projects.configured = true;
        self.local_replay.enable();
        self.list_local_projects()
    }

    pub(super) fn local_request(&self, request: Request) -> Result<()> {
        let worker = self.local_projects.worker.as_ref().ok_or_else(|| EngineError::Other("Local storage is unavailable".into()))?;
        worker.tx.try_send(request).map_err(|e| EngineError::Other(format!("Local storage is busy or unavailable; retry: {e}")))
    }

    pub(super) fn poll_local_projects_native(&mut self) {
        if !self.local_projects.available() {
            return;
        }
        if let Err(error) = self.capture_local_fallback() {
            self.local_projects.error = Some(error.to_string());
        }
        loop {
            let response = self.local_projects.worker.as_ref().map(|worker| worker.rx.try_recv());
            match response {
                Some(Ok(response)) => self.apply_local_response(response),
                Some(Err(TryRecvError::Disconnected)) => {
                    self.local_projects.writing = false;
                    self.local_projects.loading = false;
                    self.local_projects.error = Some("Local save worker disconnected; the draft remains unsaved".into());
                    break;
                }
                _ => break,
            }
        }
        if self.local_projects.pending.is_none()
            && self.local_projects.error.is_none()
            && let Err(error) = self.prepare_local_save()
        {
            self.local_projects.error = Some(error.to_string());
        }
        if !self.local_projects.writing
            && self.local_projects.error.is_none()
            && let Some(pending) = &self.local_projects.pending
        {
            let exclude_projects = if pending.dedup_existing {
                self.documents().iter().filter(|d| d.doc.id != pending.doc).filter_map(|d| self.local_projects.status(d.doc.id).project).collect()
            } else {
                Vec::new()
            };
            match self.local_request(Request::Save { save: Box::new(pending.request.clone()), dedup_existing: pending.dedup_existing, exclude_projects }) {
                Ok(()) => self.local_projects.writing = true,
                Err(error) => self.local_projects.error = Some(error.to_string()),
            }
        }
        if !self.local_projects.pruning && !self.local_projects.writing && self.local_projects.error.is_none() && !self.local_projects.prune_queue.is_empty() {
            let queued = std::mem::take(&mut self.local_projects.prune_queue);
            if self.local_request(Request::PruneClosed(queued.clone())).is_ok() {
                self.local_projects.pruning = true;
            } else {
                self.local_projects.prune_queue = queued;
            }
        }
        for (&doc, state) in &mut self.local_projects.states {
            state.pending = self
                .local_replay
                .pending_for(doc)
                .saturating_add(usize::from(self.local_projects.pending.as_ref().is_some_and(|pending| pending.doc == doc)))
                .saturating_add(usize::from(self.local_projects.failed.contains_key(&doc)));
        }
    }

    fn prepare_local_save(&mut self) -> Result<()> {
        let blocked: HashSet<_> = self.local_projects.failed.keys().copied().collect();
        let Some(edit) = self.local_replay.peek_unblocked(&blocked) else { return Ok(()) };
        let doc = edit.document.id;
        let dedup_existing = self.local_projects.recovery_dedup.remove(&doc);
        let state = self.local_projects.states.entry(doc).or_default();
        let project = match state.project {
            Some(project) => project,
            None => ProjectId::new(&random_hex()?).map_err(|e| EngineError::Other(e.to_string()))?,
        };
        let request = SaveRequest {
            project,
            expected_revision: state.local_revision,
            request_id: EventId::new(&random_hex()?).map_err(|e| EngineError::Other(e.to_string()))?,
            title: project_title(&edit.document.name),
            document: edit.document.as_ref().clone(),
            event: edit.record.clone(),
        };
        let editor_revision = edit.editor_revision;
        state.project = Some(project);
        self.local_projects.pending = Some(PendingSave { request, doc, editor_revision, dedup_existing });
        self.local_replay.pop_unblocked(&blocked);
        Ok(())
    }

    fn apply_local_response(&mut self, response: Response) {
        match response {
            Response::Prepared(result) => {
                self.local_projects.preparing_room = false;
                let result = (*result)
                    .map_err(EngineError::Other)
                    .and_then(|prepared| self.queue_prepared_room_submission(prepared.submission, prepared.assets, prepared.local_revision, prepared.freeze));
                if let Err(error) = result {
                    self.rooms.set_submission_error(error.to_string());
                }
            }
            Response::Saved(result) => {
                self.local_projects.writing = false;
                match result {
                    Ok((ack, dedup_elapsed_ms)) => {
                        self.gallery.thumbnails.remove(&(ack.project, true));
                        if let Some(pending) = self.local_projects.pending.take() {
                            let exact_new = ack.project == pending.request.project
                                && ack.event_id == pending.request.request_id
                                && ack.revision == pending.request.expected_revision.saturating_add(1);
                            let dedup_match = pending.dedup_existing
                                && dedup_elapsed_ms.is_some()
                                && ack.event_id == pending.request.request_id
                                && ack.revision >= 1
                                && ack.device_sequence == 0;
                            if !exact_new && !dedup_match {
                                self.local_projects.pending = Some(pending);
                                self.local_projects.error = Some("Local save returned a mismatched acknowledgment; the draft is retained".into());
                                return;
                            }
                            if let Some(elapsed_ms) = dedup_elapsed_ms {
                                self.local_replay.advance_elapsed_ms(pending.doc, elapsed_ms);
                            }
                            let state = self.local_projects.states.entry(pending.doc).or_default();
                            state.project = Some(ack.project);
                            state.local_revision = ack.revision;
                            state.editor_revision = pending.editor_revision;
                            state.error = None;
                            if !self.documents().iter().any(|d| d.doc.id == pending.doc) {
                                self.prune_closed_local_project(pending.doc);
                            }
                        }
                    }
                    Err(error) => self.retain_failed_save(error),
                }
            }
            Response::Listed(result) => {
                self.local_projects.loading = false;
                match result {
                    Ok((drafts, usage)) => {
                        self.local_projects.drafts = drafts;
                        self.local_projects.usage = Some(usage);
                        if self.gallery.available() && !self.gallery.busy() && self.gallery.items.is_empty() {
                            let _ = self.gallery_action(crate::gallery::GalleryAction::Refresh);
                        }
                    }
                    Err(error) => self.local_projects.error = Some(error),
                }
            }
            Response::Opened(result) => {
                self.local_projects.loading = false;
                match *result {
                    Ok(loaded) => self.install_local_draft(loaded),
                    Err(error) => {
                        if error.global {
                            self.local_projects.error = Some(error.message);
                        } else {
                            self.local_projects.notice = Some(error.message);
                        }
                    }
                }
            }
            Response::Pruned(deleted) => {
                self.local_projects.pruning = false;
                if !deleted.is_empty() {
                    self.local_projects.drafts.retain(|draft| !deleted.contains(&draft.project));
                    self.gallery.items.retain(|item| !(item.draft && deleted.contains(&item.id)));
                    for project in &deleted {
                        self.gallery.thumbnails.remove(&(*project, true));
                    }
                    if self.gallery.active_draft && self.gallery.active.as_ref().is_some_and(|active| deleted.contains(&active.id)) {
                        self.gallery.active = None;
                        self.gallery.image = None;
                        self.gallery.replay = None;
                    }
                }
            }
        }
    }

    fn retain_failed_save(&mut self, error: Failure) {
        if error.global {
            // Disk/open failures affect the whole worker. Keep the current exact request
            // in place for retry; no following project is falsely acknowledged.
            self.local_projects.error = Some(error.message);
            return;
        }
        if let Some(pending) = &self.local_projects.pending {
            self.local_projects.states.entry(pending.doc).or_default().error = Some(error.message.clone());
        }
        if let Some(failed) = self.local_projects.pending.take() {
            let doc = failed.doc;
            if self.local_projects.failed.contains_key(&doc) {
                self.local_projects.pending = Some(failed);
                self.local_projects.error = Some("A local draft already retains a failed request; both edits remain pending".into());
                return;
            }
            self.local_projects.failed.insert(doc, failed);
            self.local_projects.recount_failed_bytes();
        }
    }

    pub(crate) fn install_local_draft(&mut self, loaded: LoadedDraft) {
        if let Some(index) = self.documents().iter().position(|state| self.local_projects.status(state.doc.id).project == Some(loaded.info.project)) {
            self.set_active(index);
            return;
        }
        // Reopening a checkpoint adds no invented earlier events or duplicate baseline.
        let mut recorder = std::mem::take(&mut self.local_replay);
        let index = self.add_document(loaded.document, None);
        if let Some(st) = self.documents().get(index) {
            recorder.adopt(st, loaded.info.replay_elapsed_ms);
            self.local_projects.states.insert(
                st.doc.id,
                LocalStatus { project: Some(loaded.info.project), local_revision: loaded.info.revision, editor_revision: st.revision, ..Default::default() },
            );
        }
        self.local_replay = recorder;
        if let Some(warning) = loaded.recovery_warning {
            // A recovered prior checkpoint cannot overwrite the corrupt head. Keep it open;
            // subsequent edits form an independent draft, preserving all original records.
            if let Some(id) = self.active().map(|st| st.doc.id) {
                self.local_projects.states.insert(id, LocalStatus::default());
                self.local_replay.forget(id);
                if let Err(error) = self.capture_local_replay_baseline() {
                    self.local_projects.error = Some(error.to_string());
                    return;
                }
            }
            self.local_projects.notice = Some(format!("{warning} Recovered drawing will save as a new draft."));
        }
    }
}

fn project_title(name: &str) -> String {
    let mut title = String::new();
    for character in name.trim().chars() {
        if title.len().saturating_add(character.len_utf8()) > 96 {
            break;
        }
        if !character.is_control() {
            title.push(character);
        }
    }
    if title.is_empty() { "Untitled drawing".into() } else { title }
}
