//! Bounded canvas protocol. Identity is derived from authenticated transport, never this payload.
use serde::{Deserialize, Serialize};

use crate::shared::media::{AssetHash, MediaFrame, Provenance};
use crate::shared::{AcceptedOp, DeviceId, MediaId, OpId, SourceId, WorldOp};
use crate::{Camera, CanvasId, OwnerId, Point, RegionKey, Srgba, StrokeId};

pub const VERSION: u16 = 4;
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_PAGE: u32 = 128;
pub const MAX_PREVIEW_POINTS: usize = 128;
/// Default chunk size for pinned checkpoint transfers (`256 KiB` raw -> well inside `MAX_RESPONSE_BYTES`).
pub const CHECKPOINT_CHUNK_BYTES: u32 = 262_144;
/// Hard per-chunk wire cap for checkpoint transfers (`1 MiB`).
pub const MAX_CHECKPOINT_CHUNK_BYTES: u32 = 1024 * 1024;
/// Hard compressed checkpoint blob bound (`32 MiB`).
pub const MAX_CHECKPOINT_COMPRESSED_BYTES: u64 = 32 * 1024 * 1024;
/// Hard uncompressed checkpoint JSON bound enforced during streaming decompression (`128 MiB`).
pub const MAX_CHECKPOINT_UNCOMPRESSED_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u16,
    pub action: Action,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Identity {},
    Create {
        canvas: CanvasId,
        name: String,
    },
    /// Restore an independently owned copy of a verified accepted source prefix.
    /// The authority must require current source ownership; archive IDs are data.
    RestoreCopy {
        source_canvas: CanvasId,
        source_revision: u64,
        source_hash: AssetHash,
        new_canvas: CanvasId,
    },
    /// Staging is bound to the authenticated importer/device, never archive IDs.
    ImportBegin {
        id: OpId,
        new_canvas: CanvasId,
        manifest_sha256: AssetHash,
        manifest_bytes: u64,
        total_bytes: u64,
    },
    ImportChunk {
        id: OpId,
        object: ImportObject,
        offset: u64,
        bytes: Vec<u8>,
    },
    ImportCommit {
        id: OpId,
    },
    ImportCancel {
        id: OpId,
    },
    List {
        after: Option<CanvasId>,
        limit: u32,
    },
    /// Only the authenticated owner's recoverable deleted canvases.
    Trash {
        after: Option<CanvasId>,
        limit: u32,
    },
    Delete {
        canvas: CanvasId,
        expected_epoch: u64,
    },
    RestoreOriginal {
        canvas: CanvasId,
        expected_epoch: u64,
    },
    Info {
        canvas: CanvasId,
    },
    Members {
        canvas: CanvasId,
        after: Option<OwnerId>,
        limit: u32,
    },
    People {
        after: Option<OwnerId>,
        limit: u32,
    },
    PrepareGameMedia {
        canvas: CanvasId,
        source: GameMediaSource,
    },
    RemixGame {
        operation: WorldOp,
        room: SourceId,
    },
    RemixCanvas {
        operation: WorldOp,
        source_canvas: CanvasId,
        source_media: MediaId,
    },
    RemixStroke {
        operation: WorldOp,
    },
    FetchMedia {
        canvas: CanvasId,
        hash: AssetHash,
        offset: u64,
    },
    Invite {
        canvas: CanvasId,
        id: OpId,
        recipient: OwnerId,
        secret: String,
        expires_at_ms: u64,
    },
    CancelInvite {
        canvas: CanvasId,
        id: OpId,
    },
    Accept {
        canvas: CanvasId,
        id: OpId,
        secret: String,
    },
    Remove {
        canvas: CanvasId,
        member: OwnerId,
    },
    Leave {
        canvas: CanvasId,
    },
    Append {
        operation: WorldOp,
    },
    Events {
        canvas: CanvasId,
        after: u64,
        through: Option<u64>,
        limit: u32,
    },
    /// Inspect the latest pinned immutable derived checkpoint for a canvas (if any).
    CheckpointMeta {
        canvas: CanvasId,
    },
    /// Fetch a byte range from a checkpoint pinned to its exact immutable `(revision, compressed_sha256)`.
    CheckpointChunk {
        canvas: CanvasId,
        revision: u64,
        compressed_sha256: AssetHash,
        offset: u64,
        len: u32,
    },
    Presence {
        canvas: CanvasId,
        epoch: u64,
        camera: Camera,
        preview: Vec<[f64; 2]>,
        #[serde(default)]
        cursor: Option<Point>,
        #[serde(default)]
        stroke: Option<PreviewStyle>,
    },
    Peers {
        canvas: CanvasId,
    },
    /// Negotiate a transient device-scoped presence stream. A nonce only identifies a retry.
    PresenceOpen {
        canvas: CanvasId,
        epoch: u64,
        nonce: OpId,
    },
    /// Publish the latest sample; the authority ignores serials already superseded.
    PresenceUpdate {
        canvas: CanvasId,
        epoch: u64,
        stream: OpId,
        serial: u64,
        camera: Camera,
        preview: Vec<[f64; 2]>,
        #[serde(default)]
        cursor: Option<Point>,
        #[serde(default)]
        stroke: Option<PreviewStyle>,
    },
    PresenceRead {
        canvas: CanvasId,
        stream: OpId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImportObject {
    Manifest {},
    Asset { hash: AssetHash },
}

/// Explicit playback choices applied to an authorized terminal game, never claimed authorship.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GameMediaSource {
    pub room: SourceId,
    pub game: SourceId,
    pub chain: u8,
    pub frame: Option<u16>,
    pub fps: u8,
    pub looping: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedMedia {
    pub frames: Vec<MediaFrame>,
    pub looping: bool,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Person {
    pub profile: OwnerId,
    pub display_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasSummary {
    pub canvas: CanvasId,
    pub owner: OwnerId,
    pub name: String,
    /// Immutable revision-zero label so replay reconstructs the identical canonical seed after rename.
    pub initial_name: String,
    pub revision: u64,
    /// This viewer's current grant; a revoked queue can never acquire a new epoch implicitly.
    pub epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_origin: Option<Box<crate::shared::ImportOrigin>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub profile: OwnerId,
    pub owner: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrashedCanvas {
    pub canvas: CanvasId,
    pub name: String,
    pub revision: u64,
    pub deleted_at_ms: u64,
    /// Post-delete grant, required to reject delayed deletion/restoration retries.
    pub owner_epoch: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Peer {
    pub profile: OwnerId,
    pub device: DeviceId,
    pub camera: Camera,
    pub preview: Vec<[f64; 2]>,
    pub cursor: Option<Point>,
    pub stroke: Option<PreviewStyle>,
    pub expires_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewStyle {
    pub id: StrokeId,
    pub anchor: RegionKey,
    pub width: f64,
    pub color: Srgba,
    pub erase: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brush: Option<Box<crate::brush::StrokeBrush>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub samples: Option<Vec<crate::brush::StrokeSample>>,
}

/// Immutable pinned checkpoint metadata returned by `Action::CheckpointMeta`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointRef {
    pub canvas: CanvasId,
    pub revision: u64,
    pub schema_ver: u32,
    pub chain_hash: AssetHash,
    pub state_hash: AssetHash,
    pub compressed_sha256: AssetHash,
    pub compressed_bytes: u64,
    pub uncompressed_bytes: u64,
}

impl CheckpointRef {
    pub fn validate(&self) -> crate::Result<()> {
        if self.revision == 0
            || self.revision > i64::MAX as u64
            || (self.schema_ver != crate::shared::SHARED_SCHEMA && self.schema_ver != crate::shared::SHARED_SCHEMA_BRUSH)
            || self.compressed_bytes == 0
            || self.compressed_bytes > MAX_CHECKPOINT_COMPRESSED_BYTES
            || self.uncompressed_bytes == 0
            || self.uncompressed_bytes > MAX_CHECKPOINT_UNCOMPRESSED_BYTES
        {
            return Err(crate::Error::Limit("checkpoint metadata"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Identity {
        profile: OwnerId,
        device: DeviceId,
    },
    Canvas {
        info: CanvasSummary,
    },
    ImportProgress {
        id: OpId,
        object: ImportObject,
        received: u64,
        total: u64,
    },
    Canvases {
        items: Vec<CanvasSummary>,
        next: Option<CanvasId>,
    },
    Trash {
        items: Vec<TrashedCanvas>,
        next: Option<CanvasId>,
    },
    Members {
        items: Vec<Member>,
        next: Option<OwnerId>,
    },
    People {
        items: Vec<Person>,
        next: Option<OwnerId>,
    },
    MediaPrepared {
        media: Box<PreparedMedia>,
    },
    MediaChunk {
        bytes: Vec<u8>,
        total: u64,
    },
    Invited {
        id: OpId,
        expires_at_ms: u64,
    },
    Done,
    Accepted {
        event: Box<AcceptedOp>,
    },
    Events {
        events: Vec<AcceptedOp>,
        through: u64,
        next: Option<u64>,
    },
    CheckpointMeta {
        checkpoint: Option<CheckpointRef>,
    },
    CheckpointChunk {
        revision: u64,
        compressed_sha256: AssetHash,
        offset: u64,
        #[serde(with = "serde_b64")]
        bytes: Vec<u8>,
        total: u64,
    },
    Peers {
        peers: Vec<Peer>,
    },
    PresenceReady {
        canvas: CanvasId,
        epoch: u64,
        nonce: OpId,
        stream: OpId,
    },
    /// Observation orders snapshots in the authority's execution order, within this stream.
    PresenceSnapshot {
        canvas: CanvasId,
        stream: OpId,
        observation: u64,
        peers: Vec<Peer>,
    },
}

impl Request {
    pub fn decode(bytes: &[u8]) -> crate::Result<Self> {
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(crate::Error::Limit("canvas request bytes"));
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| crate::Error::Invalid("canvas request"))?;
        if value.version != VERSION {
            return Err(crate::Error::Invalid("canvas protocol version; upgrade before reconnecting"));
        }
        value.validate_bounds()?;
        Ok(value)
    }
    /// Also used by adapters that parse separately to report version mismatch.
    pub fn validate_bounds(&self) -> crate::Result<()> {
        use crate::shared::archive_import::{MAX_CHUNK_BYTES, MAX_MANIFEST_BYTES, MAX_TOTAL_BYTES};
        match &self.action {
            Action::ImportBegin { manifest_bytes, total_bytes, .. }
                if *manifest_bytes == 0 || *manifest_bytes > MAX_MANIFEST_BYTES || *total_bytes < *manifest_bytes || *total_bytes > MAX_TOTAL_BYTES =>
            {
                return Err(crate::Error::Limit("archive import size"));
            }
            Action::ImportChunk { offset, bytes, .. }
                if bytes.is_empty() || bytes.len() > MAX_CHUNK_BYTES || offset.checked_add(bytes.len() as u64).is_none_or(|end| end > MAX_TOTAL_BYTES) =>
            {
                return Err(crate::Error::Limit("archive import chunk"));
            }
            Action::CheckpointChunk { revision, offset, len, .. }
                if *revision == 0
                    || *revision > i64::MAX as u64
                    || *len == 0
                    || *len > MAX_CHECKPOINT_CHUNK_BYTES
                    || offset.checked_add(u64::from(*len)).is_none_or(|end| end > MAX_CHECKPOINT_COMPRESSED_BYTES) =>
            {
                return Err(crate::Error::Limit("checkpoint chunk bounds"));
            }
            _ => {}
        }
        Ok(())
    }
}

mod serde_b64 {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        serializer.serialize_str(&encoded)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(deserializer)?;
        let max_b64_len = ((super::MAX_CHECKPOINT_CHUNK_BYTES as usize) / 3 + 2) * 4;
        if s.len() > max_b64_len {
            return Err(serde::de::Error::custom("checkpoint chunk exceeds maximum size"));
        }
        let decoded = base64::engine::general_purpose::STANDARD.decode(&s).map_err(serde::de::Error::custom)?;
        if decoded.len() > super::MAX_CHECKPOINT_CHUNK_BYTES as usize {
            return Err(serde::de::Error::custom("checkpoint chunk exceeds maximum size"));
        }
        Ok(decoded)
    }
}
