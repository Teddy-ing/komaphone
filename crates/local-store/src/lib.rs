//! Permanent local Komaphone drafts. Native storage belongs on a background worker.
//!
//! SQLite commits contain the replay record, result checkpoint, project head and
//! device sequence together. An acknowledgment is returned only after commit.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod types;
pub use types::*;

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::{SaveStage, Store, fresh_event_id, fresh_project_id};

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_CHECKPOINT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_PROJECTS: u64 = 1024;
pub const MAX_REPLAY_PAGE: usize = 128;

#[derive(Debug, Clone, Copy)]
pub struct StoreOptions {
    /// Hard database size limit. SQLite reports disk-full rather than pruning history.
    pub quota_bytes: u64,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self { quota_bytes: 2 * 1024 * 1024 * 1024 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub stored_bytes: u64,
    pub quota_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Local save failed: {0}. The previous saved draft is retained.")]
    Storage(String),
    #[error(
        "Local storage is full or its quota was reached. Free disk space or increase the Komaphone data quota, then retry; the previous saved draft is retained."
    )]
    Full,
    #[error("Local draft has changed since this edit. Reopen it or save the current drawing as a new draft.")]
    Conflict,
    #[error("Local record is invalid: {0}")]
    Invalid(String),
    #[error("Local store schema {0} is unsupported. Keep this data directory and use a compatible Komaphone version.")]
    Version(u32),
    #[error("No readable local revision remains: {0}. Keep the data directory for recovery; it has not been overwritten.")]
    Corrupt(String),
    #[error("Local draft was not found")]
    NotFound,
}

pub type Result<T> = std::result::Result<T, Error>;
