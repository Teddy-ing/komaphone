//! Experimental single-user canvas persistence, isolated from finite party documents.
//!
//! Artwork payloads are loaded through a bounded spatial index. SQLite metadata and
//! stroke commits are durable before acknowledgments; offscreen strokes stay on disk.
//! Explicit writes may target any canvas owned by this local store identity, with
//! revision checks. The engine separately enforces the currently active editing
//! context. Active selection is navigation state, not a membership permission.

pub const CANVAS_STORE_SCHEMA: u32 = 1;
pub const MAX_CANVAS_INFO_BYTES: usize = 128 * 1024;
pub const MAX_CANVAS_STROKE_BYTES: usize = 1024 * 1024;
pub const MAX_CANVAS_VIEW_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_CANVAS_QUERY_ROWS: usize = 65_536;

#[cfg(not(target_arch = "wasm32"))]
pub mod linking;
#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::{CanvasStage, CanvasStore, fresh_stroke_id};
