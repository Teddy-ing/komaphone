//! Durable private Komaphone authority; separate from the editor's control port.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod archive;
mod assets;
mod backup;
pub mod identity;
pub mod server;
mod store;
pub mod tls;
pub mod worlds;
pub use assets::encode_png;
pub use store::{SaveStage, Store, StoreOptions};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Domain(#[from] photocraft_party::Error),
    #[error("Service storage failed; no acknowledgment was committed. Keep the local draft and retry after repairing service storage.")]
    Storage,
    #[error("Service storage quota reached; keep local work and increase the service quota or restore storage space.")]
    Full,
    #[error("Service data schema is unsupported; preserve the data directory and use a compatible version.")]
    Schema,
    #[error("The service is busy. Keep local work and retry shortly.")]
    Busy,
    #[error("The application session has expired or been revoked; reconnect before synchronizing. Local artwork remains saved.")]
    Session,
    #[error("Google sign-in verification is temporarily unavailable; keep drawing locally and retry shortly.")]
    IdentityProvider,
    #[error("The transient presence stream expired or changed; open a fresh stream before publishing.")]
    PresenceStream,
    #[error("Invalid request: {0}")]
    Invalid(&'static str),
    #[error("Service I/O failed: {0}")]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, Error>;
impl From<rusqlite::Error> for Error {
    fn from(value: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(code, _) = &value
            && code.code == rusqlite::ErrorCode::DiskFull
        {
            return Self::Full;
        }
        Self::Storage
    }
}
impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self::Invalid("JSON record")
    }
}
