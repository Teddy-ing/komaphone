//! Read-only gallery playback and derivatives. Native documents remain the originals;
//! presentation pixels never include notes, references, credits or editor overlays.
use serde::{Deserialize, Serialize};

/// A retained edit boundary. Unsupported operations are honest checkpoint steps.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplayMoment {
    pub revision: u64,
    pub elapsed_ms: u64,
    pub command: String,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplayInfo {
    pub duration_ms: u64,
    pub moments: Vec<ReplayMoment>,
    /// Legacy imports have a current-state baseline, never invented drawing history.
    pub baseline_only: bool,
    /// An explicit missing submission has no baseline or invented earlier history.
    pub missing: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnimationOptions {
    pub fps: u8,
    pub looping: bool,
}

/// GIF's palette and binary alpha are visible limitations, not archive limitations.
pub const GIF_NOTICE: &str = "GIF uses up to 255 visible colors plus transparency per frame, binary transparency (alpha below 50% is transparent), and 10 ms timing steps. Native archives and PNG frames preserve the source.";

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
