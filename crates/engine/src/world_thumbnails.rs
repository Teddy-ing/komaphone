//! Account-scoped Canvas Gallery artwork previews. All decoding, history replay,
//! network access and persistence run on one bounded background worker.
#[cfg(not(target_arch = "wasm32"))]
use crate::EngineError;
use crate::{Result, Session};
use photocraft_scene::{CanvasId, shared::WorldState, world_protocol::CanvasSummary};
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
mod native;

#[derive(Clone, Debug)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub revision: u64,
}
#[derive(Default)]
pub struct Thumbnails {
    #[cfg(not(target_arch = "wasm32"))]
    inner: native::State,
}
impl Thumbnails {
    pub fn contains(&self, canvas: CanvasId) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner.contains(canvas)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = canvas;
            false
        }
    }
    pub fn is_pending(&self, canvas: CanvasId) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner.is_pending(canvas)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = canvas;
            false
        }
    }
    pub fn reset(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.inner.reset();
    }
    pub fn remove(&mut self, canvas: CanvasId) {
        #[cfg(not(target_arch = "wasm32"))]
        self.inner.remove(canvas);
        #[cfg(target_arch = "wasm32")]
        let _ = canvas;
    }
    pub fn error(&self, canvas: CanvasId) -> Option<&str> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner.error(canvas)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = canvas;
            None
        }
    }
}
impl Session {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn enable_world_thumbnails(&mut self, path: std::path::PathBuf, certificate: Option<Vec<u8>>) -> Result<()> {
        self.worlds.thumbnails.inner.start(path, certificate)
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn thumbnail_access(&mut self, access: Option<&photocraft_room_client::world_worker::WorldAccess>) {
        self.worlds.thumbnails.inner.access(access);
    }
    pub fn thumbnail_catalog_changed(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.worlds.thumbnails.inner.catalog(&self.worlds.items);
    }
    pub fn thumbnail_remove(&mut self, canvas: CanvasId) {
        self.worlds.thumbnails.remove(canvas);
    }
    pub fn thumbnail_observe(&mut self, info: &CanvasSummary, canonical: Arc<WorldState>) {
        #[cfg(not(target_arch = "wasm32"))]
        if info.canvas == canonical.canvas && info.revision == canonical.revision {
            self.worlds.thumbnails.inner.observe(info);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (info, canonical);
    }
    pub fn poll_world_thumbnails(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.worlds.thumbnails.inner.poll();
    }
    /// Cache-only UI lookup plus a bounded scheduling operation. The returned
    /// RGBA is already decoded and capped at 160 × 100 pixels.
    pub fn world_thumbnail(&mut self, canvas: CanvasId) -> Result<Option<Arc<Thumbnail>>> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let source = self.worlds.active.as_ref().filter(|active| active.info.canvas == canvas).map(|active| Arc::clone(&active.canonical));
            self.worlds.thumbnails.inner.get(canvas, source)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = canvas;
            Ok(None)
        }
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError::Other(value.to_string())
}
