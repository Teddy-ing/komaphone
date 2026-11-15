//! Local scene coordination. A pending command is never a durable acknowledgment.
use std::collections::VecDeque;

use photocraft_scene::{Bookmark, Camera, CanvasId, CanvasInfo, OwnerId, SceneView, Stroke, Viewport};
use serde::Serialize;

use crate::{EngineError, Result, Session};

pub mod linking;
#[cfg(not(target_arch = "wasm32"))]
mod native;

const MAX_PENDING: usize = 32;

#[derive(Clone, Debug)]
pub(crate) enum Action {
    List,
    Create { name: String, previous: Option<Camera> },
    Open { canvas: CanvasId, previous: Option<Camera> },
    Rename { canvas: CanvasId, name: String },
    Stroke(Stroke),
    Camera(Camera),
    Bookmarks { canvas: CanvasId, bookmarks: Vec<Bookmark> },
    Undo(CanvasId),
    Redo(CanvasId),
}

impl Action {
    fn changes_selection(&self) -> bool {
        matches!(self, Self::Create { .. } | Self::Open { .. })
    }
    fn changes_artwork(&self) -> bool {
        matches!(self, Self::Stroke(_) | Self::Undo(_) | Self::Redo(_))
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Flight {
    pub from: Camera,
    pub to: Camera,
    pub elapsed: f64,
    pub duration: f64,
    pub paused: bool,
}

#[derive(Default)]
pub struct Canvases {
    pub link: linking::Status,
    #[cfg(not(target_arch = "wasm32"))]
    link_state: linking::native::State,
    pub items: Vec<CanvasInfo>,
    pub active: Option<CanvasInfo>,
    pub camera: Option<Camera>,
    pub view: Option<SceneView>,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub pending_strokes: Vec<Stroke>,
    pub flight: Option<Flight>,
    pub owner: Option<OwnerId>,
    pub activation: u64,
    pub usage_bytes: u64,
    pub quota_bytes: u64,
    configured: bool,
    loading: bool,
    queue: VecDeque<Action>,
    failed: Option<Action>,
    inflight: Option<Action>,
    view_inflight: bool,
    desired_view: Option<Viewport>,
    viewport_size: Option<(u32, u32)>,
    camera_dirty: bool,
    camera_changed: Option<web_time::Instant>,
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<native::Worker>,
}

impl Canvases {
    pub fn available(&self) -> bool {
        self.configured && !self.loading
    }
    pub fn busy(&self) -> bool {
        self.loading || self.inflight.is_some() || !self.queue.is_empty() || self.failed.is_some()
    }
    pub(crate) fn switching(&self) -> bool {
        self.inflight.as_ref().is_some_and(Action::changes_selection) || self.queue.iter().any(Action::changes_selection)
    }
    pub fn has_pending(&self) -> bool {
        self.busy() || self.camera_dirty || self.view_inflight || self.desired_view.is_some()
    }
    pub fn status_text(&self) -> String {
        if let Some(error) = &self.error {
            return format!("Save or loading failed: {error}");
        }
        if !self.available() {
            return "Local canvas storage unavailable".into();
        }
        if self.busy() || self.camera_dirty {
            return "Saving locally…".into();
        }
        if let Some(active) = &self.active {
            return format!("Saved locally · revision {} · Local only", active.revision);
        }
        "Local canvases · private on this device".into()
    }
    fn refresh_view(&mut self) {
        if let (Some(camera), Some((width, height))) = (&self.camera, self.viewport_size) {
            self.desired_view = Some(Viewport { camera: camera.clone(), width, height });
        }
    }
}

fn err(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}

impl Session {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn enable_canvases(&mut self, path: impl AsRef<std::path::Path>, options: photocraft_local_store::StoreOptions) -> Result<()> {
        self.enable_canvases_with_link_credentials(path, options, Box::new(photocraft_room_client::WindowsCredentials))
    }
    /// Trusted native credential adapter injection, principally for isolated tests.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn enable_canvases_with_link_credentials(
        &mut self,
        path: impl AsRef<std::path::Path>,
        options: photocraft_local_store::StoreOptions,
        credentials: Box<dyn photocraft_room_client::CredentialStore>,
    ) -> Result<()> {
        if self.canvases.worker.is_some() {
            return Err(err("Canvas storage is already configured"));
        }
        self.canvases.worker = Some(native::Worker::start(path.as_ref().to_path_buf(), options, credentials)?);
        self.canvases.loading = true;
        self.canvases.configured = true;
        Ok(())
    }

    pub fn poll_canvases(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.poll_native_canvases();
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn flush_canvases(&mut self, timeout: std::time::Duration) -> Result<()> {
        if !self.canvases.configured {
            return Ok(());
        }
        self.canvases.flight = None;
        let started = web_time::Instant::now();
        loop {
            self.poll_canvases();
            if let Some(error) = &self.canvases.error {
                return Err(err(error.clone()));
            }
            if !self.canvases.has_pending() {
                return Ok(());
            }
            if started.elapsed() >= timeout {
                return Err(err("Canvas saves did not finish before the deadline; pending work is not acknowledged"));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    pub fn set_canvas_viewport(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 || width > photocraft_scene::MAX_VIEW_SIDE || height > photocraft_scene::MAX_VIEW_SIDE {
            return;
        }
        self.canvases.viewport_size = Some((width, height));
        let Some(camera) = self.canvases.camera.clone() else { return };
        let viewport = Viewport { camera, width, height };
        if self.canvases.view.as_ref().is_none_or(|v| v.viewport != viewport || self.canvases.active.as_ref().is_some_and(|a| v.revision != a.revision)) {
            self.canvases.desired_view = Some(viewport);
        }
    }

    pub(crate) fn canvas_action(&mut self, action: Action) -> Result<()> {
        if !self.canvases.available() {
            return Err(err("Local canvases require configured native storage"));
        }
        if self.canvases.error.is_some() {
            return Err(err("Resolve the canvas error and use Retry; unsaved work is retained"));
        }
        if self.canvases.queue.len() >= MAX_PENDING || (matches!(action, Action::Stroke(_)) && self.canvases.pending_strokes.len() >= MAX_PENDING) {
            return Err(err("Canvas save queue is full. Wait for the existing edits to save, then retry"));
        }
        if self.canvases.inflight.as_ref().is_some_and(Action::changes_selection) || self.canvases.queue.iter().any(Action::changes_selection) {
            return Err(err("Wait until the canvas switch finishes before editing or navigating"));
        }
        if (action.changes_selection() || matches!(action, Action::Undo(_) | Action::Redo(_))) && self.canvases.busy() {
            return Err(err("Wait for the current canvas edits to save before switching or changing history"));
        }
        if let Action::Stroke(stroke) = &action {
            stroke.validate().map_err(|e| err(e.to_string()))?;
            photocraft_scene::render::spatial_cells(stroke, photocraft_scene::MAX_SPATIAL_CELLS).map_err(|e| err(e.to_string()))?;
            let active = self.canvases.active.as_ref().ok_or_else(|| err("Open a canvas first"))?;
            if active.id != stroke.canvas || active.owner != stroke.owner {
                return Err(err("Stroke ownership does not match the active canvas"));
            }
            self.canvases.pending_strokes.push(stroke.clone());
        }
        if action.changes_selection() {
            self.canvases.flight = None;
        }
        self.canvases.queue.push_back(action);
        self.poll_canvases();
        Ok(())
    }

    pub(crate) fn canvas_set_camera(&mut self, camera: Camera) -> Result<()> {
        if self.canvases.inflight.as_ref().is_some_and(Action::changes_selection) || self.canvases.queue.iter().any(Action::changes_selection) {
            return Err(err("Wait for the canvas switch to finish"));
        }
        photocraft_scene::coordinates::center(&camera).map_err(|e| err(e.to_string()))?;
        if self.canvases.active.as_ref().is_none_or(|a| a.id != camera.canvas) {
            return Err(err("Camera belongs to another canvas"));
        }
        if self.canvases.camera.as_ref() != Some(&camera) {
            self.canvases.camera = Some(camera);
            self.canvases.camera_dirty = true;
            self.canvases.camera_changed = Some(web_time::Instant::now());
            self.canvases.refresh_view();
        }
        Ok(())
    }

    pub fn tick_canvas_flight(&mut self, dt: f64) {
        if !dt.is_finite() || dt < 0.0 {
            return;
        }
        if self.canvases.switching() {
            self.canvases.flight = None;
            return;
        }
        let Some(flight) = self.canvases.flight.as_mut() else { return };
        if flight.paused {
            return;
        }
        flight.elapsed = (flight.elapsed + dt.min(0.1)).min(flight.duration);
        let t = flight.elapsed / flight.duration;
        let camera = photocraft_scene::coordinates::flight(&flight.from, &flight.to, t);
        match camera {
            Ok(camera) => {
                if let Err(error) = self.canvas_set_camera(camera) {
                    self.canvases.error = Some(error.to_string());
                    self.canvases.flight = None;
                } else if t >= 1.0 {
                    self.canvases.flight = None;
                }
            }
            Err(error) => {
                self.canvases.error = Some(error.to_string());
                self.canvases.flight = None;
            }
        }
    }

    pub(crate) fn retry_canvases(&mut self) -> Result<()> {
        if !self.canvases.available() {
            return Err(err("Canvas storage is unavailable; reopen the app after resolving the storage error"));
        }
        self.canvases.error = None;
        if let Some(action) = self.canvases.failed.take() {
            self.canvases.queue.push_front(action);
        }
        self.canvases.refresh_view();
        self.poll_canvases();
        Ok(())
    }
}
