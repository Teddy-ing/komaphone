//! Versioned private-service wire contract. Credentials never belong in URLs or logs.
use crate::{
    Result,
    contracts::{
        AssetHash, AssetRef, AssignmentId, MAX_MESSAGE_BYTES, MAX_ROOM_SNAPSHOT_BYTES, Mode, Participant, RequestId, RoomId, decode_bounded, validate_version,
    },
    room::{Action, ApplyOutcome, RoomView},
};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;
pub const CHUNK_BYTES: usize = 32 * 1024;
pub const MAX_RESPONSE_BYTES: usize = MAX_ROOM_SNAPSHOT_BYTES + MAX_MESSAGE_BYTES;

// Deliberately no Debug: pairing and invitation operations contain secrets.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RpcRequest {
    pub protocol_version: u16,
    pub request_id: RequestId,
    pub operation: Operation,
}
impl RpcRequest {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let request: Self = decode_bounded(bytes)?;
        validate_version(request.protocol_version)?;
        Ok(request)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Pair { participant: Participant, credential: String },
    Create { mode: Mode, timer_ms: Option<u64> },
    Join { room: RoomId, invitation: String },
    Invite { room: RoomId, secret: String, expires_in_ms: u64, uses: u8 },
    RevokeInvite { room: RoomId, invitation_id: RequestId },
    Poll { room: RoomId, after_revision: u64 },
    Apply { room: RoomId, expected_revision: u64, action: Box<Action> },
    Disconnect { room: RoomId },
    Upload { room: RoomId, hash: AssetHash, total: u64, offset: u64, bytes: Vec<u8>, finish: bool },
    Fetch { room: RoomId, hash: AssetHash, offset: u64 },
    Notes { room: RoomId },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RpcResponse {
    Paired { participant: Participant },
    Room { view: Box<RoomView>, outcome: Option<ApplyOutcome> },
    Invitation { id: RequestId, expires_at_ms: u64 },
    Revoked,
    Disconnected,
    Uploaded { asset: Option<AssetRef>, next_offset: u64 },
    AssetChunk { bytes: Vec<u8>, total: u64 },
    Notes { notes: Vec<(AssignmentId, String)> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fault {
    pub code: String,
    pub message: String,
}

/// A 256-bit capability, generated from OS randomness by native adapters.
pub fn validate_secret(secret: &str) -> Result<()> {
    AssetHash::new(secret).map(|_| ())
}
