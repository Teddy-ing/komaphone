//! Scoped immutable media and background exports. Downloaded bytes never carry
//! credentials or confer membership; every request uses the active world worker.
use crate::{EngineError, Result, Session};
use photocraft_scene::{
    shared::{TourId, media::MediaFrame},
    world_protocol as wire,
};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc};

#[cfg(not(target_arch = "wasm32"))]
mod native;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorldExport {
    View {
        path: PathBuf,
        width: u32,
        height: u32,
        elapsed_ms: u64,
    },
    TourGif {
        path: PathBuf,
        tour: TourId,
        width: u32,
        height: u32,
        fps: u32,
        looping: bool,
    },
    TourSequence {
        path: PathBuf,
        tour: TourId,
        width: u32,
        height: u32,
        fps: u32,
    },
    Archive {
        path: PathBuf,
    },
    /// Validate and stream a portable archive into an independent private canvas
    /// on the signed-in service. Imported credits never grant live access.
    Restore {
        path: PathBuf,
        canvas: photocraft_scene::CanvasId,
    },
}
impl WorldExport {
    pub fn path(&self) -> &std::path::Path {
        match self {
            Self::View { path, .. } | Self::TourGif { path, .. } | Self::TourSequence { path, .. } | Self::Archive { path } | Self::Restore { path, .. } => {
                path
            }
        }
    }
    pub fn with_path(mut self, value: PathBuf) -> Self {
        match &mut self {
            Self::View { path, .. } | Self::TourGif { path, .. } | Self::TourSequence { path, .. } | Self::Archive { path } | Self::Restore { path, .. } => {
                *path = value
            }
        }
        self
    }
}

/// Trusted capability adapter, parallel to GalleryFileAccess. The automation
/// policy must route file commands here, never through ambient path access.
pub trait WorldFileAccess: Send + Sync {
    fn prepare(&self, action: &WorldExport) -> Result<Box<dyn PreparedWorldFiles>>;
}
/// Owns private staging until atomic publication; Drop cleans only its staging.
pub trait PreparedWorldFiles: Send {
    fn action(&self) -> WorldExport;
    fn finish(&mut self) -> Result<()>;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExportStatus {
    pub busy: bool,
    pub completed: u32,
    pub total: u32,
    pub message: String,
}

#[derive(Default)]
pub struct Assets {
    pub export: Option<ExportStatus>,
    pub error: Option<String>,
    prepared: Option<wire::PreparedMedia>,
    #[cfg(not(target_arch = "wasm32"))]
    inner: native::Assets,
}
impl Assets {
    pub fn prepared(&self) -> Option<&wire::PreparedMedia> {
        self.prepared.as_ref()
    }
    pub fn reset(&mut self) {
        self.prepared = None;
        self.export = None;
        self.error = None;
        #[cfg(not(target_arch = "wasm32"))]
        self.inner.reset();
    }
    /// Cache-only lookup; never performs I/O or decoding on the UI thread.
    pub fn bytes(&self, frame: &MediaFrame) -> Result<Option<Arc<Vec<u8>>>> {
        frame.validate().map_err(error)?;
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner.bytes(frame)
        }
        #[cfg(target_arch = "wasm32")]
        {
            Ok(None)
        }
    }
}
fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError::Other(value.to_string())
}

impl Session {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn enable_world_assets(&mut self, path: PathBuf) -> Result<()> {
        self.worlds.assets.inner.start(path)
    }
    pub fn world_media_bytes(&mut self, frame: &MediaFrame) -> Result<Option<Arc<Vec<u8>>>> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            native::request_frame(self, frame)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = frame;
            Err(error("Shared canvas media requires the native app"))
        }
    }
    pub fn world_export(&mut self, action: WorldExport) -> Result<()> {
        self.world_export_start(action, None)
    }
    pub fn world_export_with_files(&mut self, action: WorldExport, files: Arc<dyn WorldFileAccess>) -> Result<()> {
        self.world_export_start(action, Some(files))
    }
    fn world_export_start(&mut self, action: WorldExport, files: Option<Arc<dyn WorldFileAccess>>) -> Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            native::export(self, action, files)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (action, files);
            Err(error("World export requires the native app"))
        }
    }
    pub fn world_cancel_export(&mut self) -> Result<()> {
        if self.worlds.assets.export.as_ref().is_none_or(|status| !status.busy) {
            return Err(error("No world export is running"));
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.worlds.assets.inner.ensure_cancellable()?;
            self.worlds.assets.inner.cancel_export();
        }
        // Keep the job occupied until its worker reports a terminal result. Old
        // progress cannot overwrite a newer job in this same canvas scope.
        if let Some(status) = &mut self.worlds.assets.export {
            status.message = "Cancelling export…".into();
        }
        Ok(())
    }
    pub fn world_asset_response(&mut self, sequence: u64, request: Option<&wire::Action>, response: &wire::Response) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Err(error) = native::response(self, sequence, request, response) {
            self.worlds.assets.error = Some(error.to_string());
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (sequence, request, response);
    }
    pub fn world_asset_error(&mut self, sequence: u64, message: &str) {
        #[cfg(not(target_arch = "wasm32"))]
        native::failure(self, sequence, message);
        #[cfg(target_arch = "wasm32")]
        let _ = (sequence, message);
    }
    pub fn poll_world_assets(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        native::poll(self);
    }
}
