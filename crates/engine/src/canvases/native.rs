use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::thread::JoinHandle;

use photocraft_local_store::{StoreOptions, Usage, canvas::CanvasStore};
use photocraft_scene::{CanvasInfo, OwnerId, SceneView, Viewport};

use super::{Action, err};
use crate::{Result, Session};

pub(super) enum Request {
    Link(super::linking::native::Commit),
    Action(Action),
    View(Viewport),
}
pub(super) struct Snapshot {
    binding: Option<super::linking::Binding>,
    items: Vec<CanvasInfo>,
    active: Option<CanvasInfo>,
    owner: OwnerId,
    usage: Usage,
}
pub(super) struct Committed {
    receipt: Option<CanvasInfo>,
    refresh: std::result::Result<Snapshot, String>,
}
pub(super) enum Reply {
    Linked(std::result::Result<photocraft_local_store::canvas::linking::LinkReceipt, String>),
    Ready(std::result::Result<Box<Snapshot>, String>),
    Changed(std::result::Result<Box<Committed>, String>),
    View(std::result::Result<Box<SceneView>, String>),
}

pub(super) struct Worker {
    tx: SyncSender<Request>,
    rx: Receiver<Reply>,
    _join: JoinHandle<()>,
}
impl Worker {
    pub fn start(path: PathBuf, options: StoreOptions, mut credentials: Box<dyn photocraft_room_client::CredentialStore>) -> Result<Self> {
        let (tx, requests) = mpsc::sync_channel(1);
        let (replies, rx) = mpsc::sync_channel(2);
        let join = std::thread::Builder::new()
            .name("komaphone-canvases".into())
            .spawn(move || {
                let backups = path.with_extension("link-backups");
                let mut store = match CanvasStore::open(path, options) {
                    Ok(store) => store,
                    Err(error) => {
                        let _ = replies.send(Reply::Ready(Err(error.to_string())));
                        return;
                    }
                };
                if replies.send(Reply::Ready(snapshot(&store).map(Box::new))).is_err() {
                    return;
                }
                while let Ok(request) = requests.recv() {
                    let reply = match request {
                        Request::Link(proof) => {
                            Reply::Linked(super::linking::native::commit(&mut store, credentials.as_mut(), proof, &backups).map_err(|e| e.to_string()))
                        }
                        Request::View(view) => Reply::View(store.view(&view).map(Box::new).map_err(|e| e.to_string())),
                        Request::Action(action) => {
                            Reply::Changed(apply(&mut store, action).map(|receipt| Box::new(Committed { receipt, refresh: snapshot(&store) })))
                        }
                    };
                    if replies.send(reply).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| err(format!("Cannot start canvas storage: {e}")))?;
        Ok(Self { tx, rx, _join: join })
    }
    pub(in crate::canvases) fn send(&self, request: Request) -> Result<()> {
        self.tx.try_send(request).map_err(|e| err(format!("Canvas worker unavailable: {e}")))
    }
}

fn snapshot(store: &CanvasStore) -> std::result::Result<Snapshot, String> {
    Ok(Snapshot {
        binding: store.local_binding().map_err(|e| e.to_string())?.map(Into::into),
        items: store.list().map_err(|e| e.to_string())?,
        active: store.active().map_err(|e| e.to_string())?,
        owner: store.owner(),
        usage: store.usage().map_err(|e| e.to_string())?,
    })
}

fn apply(store: &mut CanvasStore, action: Action) -> std::result::Result<Option<CanvasInfo>, String> {
    let result = (|| -> photocraft_local_store::Result<Option<CanvasInfo>> {
        let receipt = match action {
            Action::List => return Ok(None),
            Action::Create { name, previous } => store.create_with_previous(&name, previous)?,
            Action::Open { canvas, previous } => store.switch(canvas, previous)?,
            Action::Rename { canvas, name } => {
                let revision = store.info(canvas)?.revision;
                store.rename(canvas, revision, &name)?
            }
            Action::Stroke(stroke) => {
                let revision = store.info(stroke.canvas)?.revision;
                store.append(&stroke, revision)?
            }
            Action::Camera(camera) => store.save_camera(camera)?,
            Action::Bookmarks { canvas, bookmarks } => {
                let revision = store.info(canvas)?.revision;
                store.set_bookmarks(canvas, revision, bookmarks)?
            }
            Action::Undo(canvas) => {
                let revision = store.info(canvas)?.revision;
                store.undo(canvas, revision)?
            }
            Action::Redo(canvas) => {
                let revision = store.info(canvas)?.revision;
                store.redo(canvas, revision)?
            }
        };
        Ok(Some(receipt))
    })();
    result.map_err(|e| e.to_string())
}

impl Session {
    pub(super) fn poll_native_canvases(&mut self) {
        loop {
            let message = self.canvases.worker.as_ref().map(|worker| worker.rx.try_recv());
            let reply = match message {
                Some(Ok(reply)) => reply,
                Some(Err(TryRecvError::Disconnected)) => {
                    if self.canvases.loading || self.canvases.inflight.is_some() || self.canvases.view_inflight {
                        self.canvases.error = Some("Canvas worker stopped. Unsaved work remains in this window; reopen only after preserving it".into());
                        self.canvases.failed = self.canvases.inflight.take();
                        self.canvases.loading = false;
                        self.canvases.view_inflight = false;
                    }
                    break;
                }
                _ => break,
            };
            match reply {
                Reply::Linked(result) => {
                    self.canvases.link_state.committing = false;
                    self.canvases.link.busy = false;
                    match result {
                        Ok(receipt) => {
                            self.canvases.link.binding = Some(receipt.binding.into());
                            self.canvases.link.backup = receipt.backup_path.map(|path| path.to_string_lossy().into_owned());
                            self.canvases.link.error = None;
                        }
                        Err(error) => self.canvases.link.error = Some(error),
                    }
                }
                Reply::Ready(result) => {
                    self.canvases.loading = false;
                    match result {
                        Ok(snapshot) => self.apply_canvas_snapshot(*snapshot, true, true),
                        Err(error) => {
                            self.canvases.configured = false;
                            self.canvases.error = Some(error);
                        }
                    }
                }
                Reply::Changed(result) => {
                    let action = self.canvases.inflight.take();
                    match result {
                        Ok(committed) => {
                            // A failed list/read after COMMIT cannot turn an accepted edit into
                            // an uncommitted retry (Create/Undo/Redo are not idempotent).
                            let snapshot = match committed.refresh {
                                Ok(snapshot) => snapshot,
                                Err(error) => {
                                    let Some(receipt) = committed.receipt else {
                                        self.canvases.error = Some(error);
                                        self.canvases.failed = action;
                                        continue;
                                    };
                                    self.canvases.notice = Some(format!("Saved locally. The canvas list could not refresh: {error}"));
                                    let mut items = self.canvases.items.clone();
                                    if let Some(item) = items.iter_mut().find(|item| item.id == receipt.id) {
                                        *item = receipt.clone();
                                    } else {
                                        items.push(receipt.clone());
                                    }
                                    Snapshot {
                                        binding: self.canvases.link.binding.clone(),
                                        owner: receipt.owner,
                                        items,
                                        active: Some(receipt),
                                        usage: Usage { stored_bytes: self.canvases.usage_bytes, quota_bytes: self.canvases.quota_bytes },
                                    }
                                }
                            };
                            if let Some(Action::Stroke(stroke)) = &action {
                                let revision = snapshot.active.as_ref().map_or(0, |a| a.revision);
                                for pending in &mut self.canvases.pending_strokes {
                                    if pending.id == stroke.id {
                                        pending.revision = revision;
                                    }
                                }
                            }
                            let selection = action.as_ref().is_some_and(Action::changes_selection);
                            let artwork = action.as_ref().is_some_and(Action::changes_artwork);
                            self.apply_canvas_snapshot(snapshot, selection, artwork);
                        }
                        Err(error) => {
                            self.canvases.error = Some(error);
                            self.canvases.failed = action;
                        }
                    }
                }
                Reply::View(result) => {
                    self.canvases.view_inflight = false;
                    match result {
                        Ok(view) => {
                            if self.canvases.active.as_ref().is_some_and(|a| a.id == view.viewport.camera.canvas && a.revision == view.revision) {
                                self.canvases.pending_strokes.retain(|s| s.revision == 0 || s.revision > view.revision);
                                self.canvases.view = Some(*view);
                            }
                        }
                        Err(error) => {
                            self.canvases.error = Some(error);
                        }
                    }
                }
            }
        }
        self.poll_canvas_linking();
        if self.canvases.loading
            || self.canvases.error.is_some()
            || self.canvases.inflight.is_some()
            || self.canvases.view_inflight
            || self.canvases.link_state.committing
        {
            return;
        }
        let Some(worker) = &self.canvases.worker else { return };
        if let Some(action) = self.canvases.queue.pop_front() {
            match worker.send(Request::Action(action.clone())) {
                Ok(()) => self.canvases.inflight = Some(action),
                Err(error) => {
                    self.canvases.failed = Some(action);
                    self.canvases.error = Some(error.to_string());
                }
            }
        } else if let Some(viewport) = self.canvases.desired_view.take() {
            let current =
                self.canvases.view.as_ref().is_some_and(|v| v.viewport == viewport && self.canvases.active.as_ref().is_some_and(|a| a.revision == v.revision));
            if !current {
                match worker.send(Request::View(viewport)) {
                    Ok(()) => self.canvases.view_inflight = true,
                    Err(error) => self.canvases.error = Some(error.to_string()),
                }
            }
        } else if self.canvases.camera_dirty
            && self.canvases.flight.as_ref().is_none_or(|f| f.paused)
            && self.canvases.camera_changed.is_some_and(|time| time.elapsed().as_millis() >= 200)
            && let Some(camera) = self.canvases.camera.clone()
        {
            let action = Action::Camera(camera);
            match worker.send(Request::Action(action.clone())) {
                Ok(()) => {
                    self.canvases.inflight = Some(action);
                    self.canvases.camera_dirty = false;
                }
                Err(error) => self.canvases.error = Some(error.to_string()),
            }
        }
    }

    fn apply_canvas_snapshot(&mut self, snapshot: Snapshot, selection: bool, artwork: bool) {
        self.canvases.link.binding = snapshot.binding;
        self.canvases.items = snapshot.items;
        self.canvases.owner = Some(snapshot.owner);
        self.canvases.usage_bytes = snapshot.usage.stored_bytes;
        self.canvases.quota_bytes = snapshot.usage.quota_bytes;
        if selection {
            self.canvases.activation = self.canvases.activation.wrapping_add(1);
            self.canvases.camera = snapshot.active.as_ref().map(|a| a.camera.clone());
            self.canvases.view = None;
            self.canvases.flight = None;
            self.canvases.pending_strokes.clear();
            self.canvases.camera_dirty = false;
        } else if !artwork
            && let (Some(view), Some(active)) = (&mut self.canvases.view, &snapshot.active)
            && view.viewport.camera.canvas == active.id
        {
            view.revision = active.revision;
        }
        self.canvases.active = snapshot.active;
        self.canvases.refresh_view();
    }
}
