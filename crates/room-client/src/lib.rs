//! Nonblocking native room connection and durable outbox. The portable surface contains
//! presentation data only; sockets, credential storage and SQLite stay native-gated.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use photocraft_party::room::RoomView;
use photocraft_party::{AssignmentId, Participant, ProjectId, RequestId, RoomId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClientStatus {
    pub endpoint: Option<String>,
    pub profile: Option<Participant>,
    pub room: Option<RoomView>,
    pub connected: bool,
    pub pending: usize,
    pub blocked: bool,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub invitation_id: Option<RequestId>,
    pub paired: bool,
    pub polling: bool,
    pub submission_project: Option<ProjectId>,
    pub freeze_pin: Option<FreezePin>,
    pub note_draft: Option<NoteDraft>,
    pub reveal_position: Option<RevealPosition>,
    pub game_drafts: Option<GameDrafts>,
}

/// Only this participant's authorized projects in the current game, never opponents' drafts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameDrafts {
    pub room: RoomId,
    pub game: photocraft_party::GameId,
    pub projects: Vec<ProjectId>,
}
impl ClientStatus {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn remember_game_draft(&mut self) -> Result<()> {
        if self.game_drafts.as_ref().is_some_and(|drafts| {
            drafts.projects.len() > 12 || drafts.projects.iter().collect::<std::collections::BTreeSet<_>>().len() != drafts.projects.len()
        }) {
            return Err(Error::Invalid("Invalid saved assignment scope; preserve the client database".into()));
        }
        let Some(room) = &self.room else { return Ok(()) };
        if room.phase == photocraft_party::room::Phase::Lobby {
            self.game_drafts = None;
            return Ok(());
        }
        let Some(assignment) = &room.assignment else { return Ok(()) };
        if Some(assignment.assignment.player) != self.profile.as_ref().map(|profile| profile.id)
            || !room.members.iter().any(|member| member.participant.id == assignment.assignment.player)
        {
            return Err(Error::Invalid("The service assignment belongs to a different participant".into()));
        }
        let game = assignment.assignment.id.game;
        if self.game_drafts.as_ref().is_none_or(|drafts| drafts.game != game || drafts.room != room.id) {
            self.game_drafts = Some(GameDrafts { room: room.id, game, projects: Vec::new() });
        }
        if let Some(drafts) = &mut self.game_drafts
            && !drafts.projects.contains(&assignment.assignment.project)
        {
            if drafts.projects.len() >= 12 {
                return Err(Error::Invalid("A game cannot assign more than twelve drawings per player".into()));
            }
            drafts.projects.push(assignment.assignment.project);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoteDraft {
    pub assignment: AssignmentId,
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevealPosition {
    pub game: photocraft_party::GameId,
    pub chain: u8,
    pub frame: u8,
    pub fps: u8,
    pub looping: bool,
    pub visible_parts: u8,
}

pub mod media;

/// The first observed deadline pins an acknowledged local revision before heavy packing.
/// This is local durability, not a submission or service acceptance acknowledgment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FreezePin {
    pub room: RoomId,
    pub epoch: u64,
    pub assignment: AssignmentId,
    pub project: ProjectId,
    pub revision: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("Room storage failed; pending work is retained: {0}")]
    Storage(String),
    #[error("Windows credentials are unavailable; reconnect after repairing Credential Manager: {0}")]
    Credentials(String),
    #[error("Service unavailable; local drawing remains saved and pending work will retry")]
    Unavailable,
    #[error("{code}: {message}")]
    Rejected { code: String, message: String },
    #[error("Room queue is full; wait for pending work or resolve its error")]
    Busy,
}
pub type Result<T> = std::result::Result<T, Error>;

/// Secrets are intentionally absent from ClientStatus and its serialized control view.
pub struct Invitation {
    pub room: RoomId,
    pub id: RequestId,
    pub secret: String,
    pub expires_at_ms: u64,
}

#[cfg(not(target_arch = "wasm32"))]
mod credentials;
#[cfg(not(target_arch = "wasm32"))]
mod download;
#[cfg(not(target_arch = "wasm32"))]
mod store;
#[cfg(not(target_arch = "wasm32"))]
mod transport;
#[cfg(not(target_arch = "wasm32"))]
mod worker;
#[cfg(not(target_arch = "wasm32"))]
pub use credentials::{CredentialStore, WindowsCredentials};
#[cfg(not(target_arch = "wasm32"))]
pub use store::Outbox;
#[cfg(not(target_arch = "wasm32"))]
pub use transport::{Transport, normalize_endpoint};
#[cfg(not(target_arch = "wasm32"))]
pub use worker::{ClientCommand, ClientEvent, ClientOptions, Worker};
