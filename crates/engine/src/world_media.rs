//! Read-only shared-world rendering and frozen-revision exports. The working
//! pixels are floating-point sRGBA, matching the scene's explicit working space.
//! Finite editor documents keep their existing color/depth-general APIs.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorldImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<[f32; 4]>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderStats {
    pub indexed_references: usize,
    pub queried_cells: usize,
    pub candidates: usize,
    pub rendered_objects: usize,
    pub source_points: usize,
    pub raster_samples: u64,
    pub decoded_asset_bytes: usize,
}

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
