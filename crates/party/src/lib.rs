//! Komaphone's bounded, versioned party contracts and deterministic authority.
//! No networking, disk I/O, editor/UI dependency, or implicit wall clock.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod adapter;
pub mod canonical_color;
pub mod contracts;
pub mod protocol;
pub mod replay_artifact;
pub mod room;

pub use contracts::*;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("invalid {0}")]
    Invalid(&'static str),
    #[error("resource limit exceeded: {0}")]
    Limit(&'static str),
    #[error("membership or ownership does not permit this operation")]
    Unauthorized,
    #[error("revision or immutable content conflict; retain the local variant")]
    Conflict,
    #[error("operation is not permitted in this phase")]
    Phase,
    #[error("unsupported schema version {0}; preserve the original and upgrade")]
    Version(u16),
    #[error("invalid JSON: {0}")]
    Json(String),
    #[error("storage failed; no acknowledgment was committed")]
    Storage,
}

pub type Result<T> = std::result::Result<T, Error>;
