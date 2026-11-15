//! Native shared-canvas engine seam. Credentials stay inside the client worker;
//! presentation receives only the current account's authorized scene and queue state.
use crate::{EngineError, Result, Session};
use photocraft_scene::{
    Bookmark, Camera, CanvasId, OwnerId,
    shared::{DeviceId, ReviewState, WorldState},
    world_protocol as wire,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
#[cfg(not(target_arch = "wasm32"))]
mod activation;
#[cfg(not(target_arch = "wasm32"))]
mod relocation;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod request_tests;
#[cfg(not(target_arch = "wasm32"))]
mod response;
mod validation;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Account {
    pub endpoint: String,
    pub profile: OwnerId,
    pub device: DeviceId,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Viewer {
    pub camera: Camera,
    pub bookmarks: Vec<Bookmark>,
    pub review: Option<ReviewState>,
}
pub struct Canvas {
    pub info: wire::CanvasSummary,
    pub canonical: Arc<WorldState>,
    pub display: Arc<WorldState>,
    pub pending: usize,
    pub viewer: Viewer,
    pub notice: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Recovery {
    pub operation: photocraft_scene::shared::WorldOp,
    pub reason: String,
}
#[derive(Default)]
pub struct Worlds {
    pub account: Option<Account>,
    pub items: Vec<wire::CanvasSummary>,
    pub trash: Vec<wire::TrashedCanvas>,
    pub people: Vec<wire::Person>,
    pub members: Vec<wire::Member>,
    pub peers: Vec<wire::Peer>,
    pub dropped_previews: u64,
    pub active: Option<Canvas>,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub activation: u64,
    pub tour_playback: Option<photocraft_scene::shared::tours::TourPlayback>,
    pub flight: Option<crate::canvases::Flight>,
    pub invitation: Option<serde_json::Value>,
    pub assets: crate::world_assets::Assets,
    pub thumbnails: crate::world_thumbnails::Thumbnails,
    pub copy_source: Option<(CanvasId, photocraft_scene::shared::SharedMedia)>,
    pub stroke_copy_source: Option<(Arc<WorldState>, photocraft_scene::StrokeId, photocraft_scene::shared::media::Credit)>,
    pub durable_strokes: BTreeSet<(CanvasId, photocraft_scene::StrokeId)>,
    pub recovery: Vec<Recovery>,
    pub recovery_next: Option<photocraft_scene::shared::OpId>,
    recovery_sequence: Option<u64>,
    pub(crate) configured: bool,
    pending: BTreeSet<u64>,
    saved_view: Option<(CanvasId, Viewer)>,
    view_pending: BTreeMap<u64, (CanvasId, Viewer)>,
    requests: BTreeMap<u64, wire::Action>,
    selected: Option<CanvasId>,
    auto_open: BTreeSet<u64>,
    peers_received: Option<web_time::Instant>,
    fresh_catalog: bool,
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<photocraft_room_client::world_worker::WorldWorker>,
    #[cfg(not(target_arch = "wasm32"))]
    access_request: Option<activation::Request>,
    #[cfg(not(target_arch = "wasm32"))]
    relocation_pending: Option<(u64, photocraft_party::RequestId)>,
}
impl Worlds {
    pub fn available(&self) -> bool {
        self.configured
    }
    pub fn connected(&self) -> bool {
        self.account.is_some()
    }
    pub fn busy(&self) -> bool {
        !self.pending.is_empty()
    }
    pub fn pending_local_sequences(&self) -> Vec<u64> {
        self.pending.iter().copied().collect()
    }
    /// Whether the latest camera/review state still needs a save request. In-flight
    /// identical snapshots are coalesced; `busy` remains true until acknowledgment.
    pub fn view_needs_save(&self) -> bool {
        let Some(active) = &self.active else { return false };
        let latest = self.view_pending.values().rev().find(|(canvas, _)| *canvas == active.info.canvas).or(self.saved_view.as_ref());
        latest.is_none_or(|(canvas, viewer)| *canvas != active.info.canvas || *viewer != active.viewer)
    }
    pub fn requests_pending(&self) -> usize {
        self.requests.len().saturating_sub(self.presence_requests_pending()).saturating_add(usize::from(self.recovery_sequence.is_some()))
    }
    pub fn presence_requests_pending(&self) -> usize {
        self.requests.values().filter(|action| matches!(action, wire::Action::Presence { .. } | wire::Action::Peers { .. })).count()
    }
    pub fn status(&self) -> String {
        if let Some(error) = &self.error {
            return error.clone();
        }
        if self.busy() {
            return "Saving locally…".into();
        }
        match &self.active {
            Some(canvas) if canvas.pending > 0 => format!("Saved locally · {} edits waiting to sync", canvas.pending),
            Some(canvas) => format!("Saved locally · Synced revision {}", canvas.info.revision),
            None if self.connected() => "Private service connected".into(),
            None => "Connect to your private service to open shared canvases; local canvases remain available".into(),
        }
    }
    fn clear_private(&mut self) {
        self.error = None;
        self.notice = None;
        self.items.clear();
        self.trash.clear();
        self.people.clear();
        self.members.clear();
        self.peers.clear();
        self.dropped_previews = 0;
        self.peers_received = None;
        self.fresh_catalog = false;
        self.active = None;
        self.tour_playback = None;
        self.flight = None;
        self.invitation = None;
        self.assets.reset();
        self.thumbnails.reset();
        self.copy_source = None;
        self.stroke_copy_source = None;
        self.durable_strokes.clear();
        self.recovery.clear();
        self.recovery_next = None;
        self.recovery_sequence = None;
        self.pending.clear();
        self.saved_view = None;
        self.view_pending.clear();
        self.requests.clear();
        self.selected = None;
        self.auto_open.clear();
        self.activation = self.activation.saturating_add(1);
    }
}
fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError::Other(value.to_string())
}
/// Allocate a transient stroke identity before sending its ephemeral preview.
pub fn new_stroke_id() -> Result<photocraft_scene::StrokeId> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(error)?;
        photocraft_scene::StrokeId::new(&bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()).map_err(error)
    }
    #[cfg(target_arch = "wasm32")]
    {
        Err(error("Shared drawing requires the native app"))
    }
}
impl Session {
    pub fn world_recovery_page(&mut self, after: Option<photocraft_scene::shared::OpId>) -> Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let worker = self.worlds.worker.as_ref().ok_or_else(|| error("Shared canvas worker unavailable"))?;
            let sequence = worker.send(photocraft_room_client::world_worker::Command::RecoveryPage { after, limit: 128 }).map_err(error)?;
            self.worlds.recovery_sequence = Some(sequence);
            Ok(())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = after;
            Err(error("Shared recovery requires the native app"))
        }
    }
    pub fn world_resume_recovery(&mut self, canvas: CanvasId) -> Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let worker = self.worlds.worker.as_ref().ok_or_else(|| error("Shared canvas worker unavailable"))?;
            let sequence = worker.send(photocraft_room_client::world_worker::Command::ResumeRecovery { canvas }).map_err(error)?;
            self.worlds.requests.insert(sequence, wire::Action::Info { canvas });
            self.worlds.pending.insert(sequence);
            Ok(())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = canvas;
            Err(error("Shared recovery requires the native app"))
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn enable_worlds(&mut self, path: std::path::PathBuf, options: photocraft_room_client::ClientOptions) -> Result<()> {
        if self.worlds.configured {
            return Err(error("Shared canvas storage is already configured"));
        }
        self.configure_canvas_linking(options.trusted_certificate_pem.clone());
        self.enable_world_assets(path.with_extension("media"))?;
        self.enable_world_thumbnails(path.with_extension("thumbnails"), options.trusted_certificate_pem.clone())?;
        self.worlds.worker = Some(photocraft_room_client::world_worker::WorldWorker::start(path, options.trusted_certificate_pem).map_err(error)?);
        self.worlds.configured = true;
        Ok(())
    }
    pub fn world_request(&mut self, action: wire::Action) -> Result<u64> {
        // Read-only duplicate queries may share a reply. A presence publication
        // contains newer gesture samples: let the bounded worker replace its
        // waiting sample instead of silently losing it behind an older request.
        if let Some((sequence, _)) = self
            .worlds
            .requests
            .iter()
            .find(|(_, pending)| matches!((pending, &action), (wire::Action::Peers { canvas: a }, wire::Action::Peers { canvas: b }) if a == b))
        {
            return Ok(*sequence);
        }
        if self.worlds.requests.len() >= 128 {
            return Err(error("Shared canvas requests are busy; try again shortly"));
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let worker = self.worlds.worker.as_ref().ok_or_else(|| error("Shared canvases require the native app"))?;
            let sequence = worker.send(photocraft_room_client::world_worker::Command::Request(action.clone())).map_err(error)?;
            self.worlds.requests.insert(sequence, action);
            if !self.worlds.requests.get(&sequence).is_some_and(|action| {
                matches!(
                    action,
                    wire::Action::Presence { .. }
                        | wire::Action::Peers { .. }
                        | wire::Action::FetchMedia { .. }
                        | wire::Action::Info { .. }
                        | wire::Action::Events { .. }
                )
            }) {
                self.worlds.error = None;
            }
            Ok(sequence)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = action;
            Err(error("Shared canvases require the native app"))
        }
    }
    pub fn world_create(&mut self, canvas: CanvasId, name: String) -> Result<()> {
        photocraft_scene::validate_name(&name).map_err(error)?;
        let sequence = self.world_request(wire::Action::Create { canvas, name })?;
        self.worlds.auto_open.insert(sequence);
        Ok(())
    }
    pub fn world_open(&mut self, canvas: CanvasId) -> Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.worlds.view_needs_save() {
                self.world_save_view()?;
            }
            let worker = self.worlds.worker.as_ref().ok_or_else(|| error("Shared canvases require the native app"))?;
            let sequence = worker.send(photocraft_room_client::world_worker::Command::Open(canvas)).map_err(error)?;
            self.worlds.requests.insert(sequence, wire::Action::Info { canvas });
            self.worlds.selected = Some(canvas);
            self.worlds.active = None;
            self.worlds.saved_view = None;
            self.worlds.tour_playback = None;
            self.worlds.flight = None;
            self.worlds.invitation = None;
            self.worlds.assets.reset();
            self.worlds.peers.clear();
            self.worlds.members.clear();
            self.worlds.error = None;
            self.worlds.activation = self.worlds.activation.saturating_add(1);
            Ok(())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = canvas;
            Err(error("Shared canvases require the native app"))
        }
    }
    pub fn world_edit(&mut self, canvas: CanvasId, action: photocraft_scene::shared::Edit) -> Result<()> {
        if self.worlds.selected != Some(canvas) || self.worlds.active.as_ref().is_none_or(|active| active.info.canvas != canvas) {
            return Err(error("Open the matching shared canvas before drawing"));
        }
        if self.worlds.active.as_ref().is_some_and(|active| active.viewer.review.as_ref().is_some_and(|review| review.interval.is_some())) {
            return Err(error("Exit catch-up playback before editing the current canvas"));
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let worker = self.worlds.worker.as_ref().ok_or_else(|| error("Shared canvas worker unavailable"))?;
            let sequence = worker.send(photocraft_room_client::world_worker::Command::Edit { canvas, action }).map_err(error)?;
            self.worlds.pending.insert(sequence);
            Ok(())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = action;
            Err(error("Shared canvases require the native app"))
        }
    }
    pub fn world_save_view(&mut self) -> Result<()> {
        let active = self.worlds.active.as_ref().ok_or_else(|| error("Open a shared canvas first"))?;
        if !self.worlds.view_needs_save() {
            return Ok(());
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let state = photocraft_room_client::world_queue::ViewerState {
                camera: active.viewer.camera.clone(),
                bookmarks: active.viewer.bookmarks.clone(),
                review: active.viewer.review.clone(),
            };
            let worker = self.worlds.worker.as_ref().ok_or_else(|| error("Shared canvas worker unavailable"))?;
            let sequence = worker.send(photocraft_room_client::world_worker::Command::SaveView { canvas: active.info.canvas, state }).map_err(error)?;
            self.worlds.view_pending.insert(sequence, (active.info.canvas, active.viewer.clone()));
            self.worlds.pending.insert(sequence);
            Ok(())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = active;
            Err(error("Shared canvases require the native app"))
        }
    }
    pub fn poll_worlds(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        while let Some(Ok(event)) = self.worlds.worker.as_ref().map(|worker| worker.poll()) {
            self.world_event(event);
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.poll_world_assets();
        self.poll_world_thumbnails();
        if self.worlds.peers_received.is_some_and(|at| at.elapsed() > std::time::Duration::from_secs(2)) {
            self.worlds.peers.clear();
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn world_event(&mut self, event: photocraft_room_client::world_worker::Event) {
        use photocraft_room_client::world_worker::Event;
        if self.world_relocation_event(&event) {
            return;
        }
        if !self.world_event_current(&event) {
            return;
        }
        match event {
            Event::Activated { .. } | Event::Stopped { .. } | Event::Relocated { .. } => {}
            Event::CachedCanvases { items } => {
                if !self.worlds.fresh_catalog {
                    if self.worlds.items.len().saturating_add(items.len()) > 16_384 {
                        self.worlds.error = Some("Downloaded canvas directory exceeds the local display limit".into());
                    } else {
                        self.worlds.items.extend(items);
                        self.thumbnail_catalog_changed();
                    }
                }
            }
            Event::PresenceDropped { sequence } => {
                self.worlds.requests.remove(&sequence);
                self.worlds.dropped_previews = self.worlds.dropped_previews.saturating_add(1);
            }
            Event::Saved { sequence, operation } => {
                if !self.worlds.pending.remove(&sequence) {
                    return;
                }
                if let photocraft_scene::shared::Edit::AddStroke { stroke } | photocraft_scene::shared::Edit::CopyStroke { stroke, .. } = operation.action {
                    if self.worlds.durable_strokes.len() >= 1024 {
                        self.worlds.durable_strokes.clear();
                    }
                    self.worlds.durable_strokes.insert((operation.canvas, stroke.id));
                }
                self.worlds.error = None;
            }
            Event::ViewSaved { sequence, .. } => {
                self.worlds.pending.remove(&sequence);
                if let Some((canvas, viewer)) = self.worlds.view_pending.remove(&sequence)
                    && self.worlds.active.as_ref().is_some_and(|active| active.info.canvas == canvas)
                {
                    self.worlds.saved_view = Some((canvas, viewer));
                }
            }
            Event::Revoked { canvas } => {
                self.worlds.items.retain(|item| item.canvas != canvas);
                self.thumbnail_remove(canvas);
                if self.worlds.selected == Some(canvas) {
                    self.worlds.active = None;
                    self.worlds.selected = None;
                    self.worlds.peers.clear();
                    self.worlds.members.clear();
                    self.worlds.tour_playback = None;
                    self.worlds.assets.reset();
                    self.worlds.activation = self.worlds.activation.saturating_add(1);
                    self.worlds.notice =
                        Some("Canvas access ended. Downloaded artwork and saved offline edits remain available for recovery on this account.".into());
                }
            }
            Event::RecoveryPage { sequence, page } => {
                if self.worlds.recovery_sequence == Some(sequence) {
                    self.worlds.recovery =
                        page.items.into_iter().map(|item| Recovery { operation: item.operation, reason: format!("{:?}", item.reason) }).collect();
                    self.worlds.recovery_next = page.next;
                    self.worlds.recovery_sequence = None;
                }
            }
            Event::RecoveryResumed { sequence, .. } => {
                self.worlds.pending.remove(&sequence);
                self.worlds.error = None;
                self.worlds.notice = Some("New edits are enabled. Conflicting originals remain in Saved edits for recovery.".into());
            }
            Event::Error { sequence, message } => {
                self.world_asset_error(sequence, &message);
                self.worlds.pending.remove(&sequence);
                self.worlds.view_pending.remove(&sequence);
                self.worlds.requests.remove(&sequence);
                self.worlds.auto_open.remove(&sequence);
                self.worlds.error = Some(message);
            }
            Event::State { info, canonical, display, pending, view, notice } => {
                if let Some(item) = self.worlds.items.iter_mut().find(|item| item.canvas == info.canvas) {
                    *item = info.clone();
                }
                self.thumbnail_observe(&info, Arc::clone(&canonical));
                if self.worlds.selected != Some(info.canvas) {
                    return;
                }
                let existing = self.worlds.active.take();
                let save_baseline = existing.is_none() && view.is_none();
                let loaded_view = existing.is_none() && view.is_some();
                let viewer = if let Some(existing) = existing {
                    existing.viewer
                } else if let Some(view) = view {
                    Viewer { camera: view.camera, bookmarks: view.bookmarks, review: view.review }
                } else {
                    let review = self.worlds.account.as_ref().and_then(|a| ReviewState::first_visit(info.canvas, a.profile, canonical.revision).ok());
                    Viewer { camera: photocraft_scene::coordinates::home(info.canvas), bookmarks: vec![], review }
                };
                if loaded_view {
                    self.worlds.saved_view = Some((info.canvas, viewer.clone()));
                }
                self.worlds.active = Some(Canvas { info, canonical, display, pending, viewer, notice });
                if save_baseline && let Err(error) = self.world_save_view() {
                    self.worlds.error = Some(error.to_string());
                }
            }
            Event::Response { sequence, response } => self.world_response(sequence, response),
        }
    }
}
