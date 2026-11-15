//! Permanent gallery originals and portable, pathless archives.
//!
//! Gallery metadata is separate from immutable originals so renaming or trashing
//! a result cannot change an acknowledged drawing or its replay.
use photocraft_party::{GameId, Participant, ProjectId};
use serde::{Deserialize, Serialize};

pub const GALLERY_STORE_SCHEMA: u32 = 1;
pub const MAX_GALLERY_ENTRIES: usize = 1024;
pub const MAX_STORED_OBJECTS: usize = 262_144;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GalleryKind {
    Project,
    Animation,
    ExquisiteCorpse,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GalleryInfo {
    pub id: ProjectId,
    pub title: String,
    pub kind: GalleryKind,
    pub authors: Vec<Participant>,
    pub complete: bool,
    pub favorite: bool,
    pub trashed: bool,
    pub created_at: String,
    pub updated_at: String,
    pub bytes: u64,
    /// Set only when an imported archive conflicts with an existing identity.
    pub imported_from: Option<ProjectId>,
    pub game: Option<GameId>,
    pub fps: u8,
    pub looping: bool,
    /// Local assignment identities retained through portable copies and remixes.
    /// Empty in older schema-1 metadata; native readers derive the missing cache.
    #[serde(default)]
    pub source_projects: Vec<ProjectId>,
}

/// Preferences for drafts whose editable originals remain in the draft store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DraftPreference {
    pub id: ProjectId,
    pub title: Option<String>,
    pub favorite: bool,
    pub trashed: bool,
    #[serde(default)]
    pub fps: Option<u8>,
    #[serde(default)]
    pub looping: Option<bool>,
}

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::{GalleryStage, GalleryStore, decode_archive, encode_archive, read_archive, write_archive};
