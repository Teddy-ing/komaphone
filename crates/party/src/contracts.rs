//! Schema 1. Bounds are wire/storage policy, not editor image-format restrictions.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u16 = 1;
pub const MAX_PLAYERS: usize = 3;
pub const MAX_MESSAGE_BYTES: usize = 262_144;
pub const MAX_ROOM_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_NOTE_BYTES: usize = 1024;
pub const MAX_NAME_BYTES: usize = 96;
pub const MAX_ASSETS: usize = 256;
pub const MAX_ASSET_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_PROJECT_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_REPLAY_EVENTS: u64 = 1_000_000;
pub const MAX_REPLAY_MS: u64 = 7 * 24 * 60 * 60 * 1000;
pub const MAX_DEPENDENCIES: usize = 64;
pub const MAX_TIMER_MS: u64 = 3_600_000;
pub const TRANSFER_GRACE_MS: u64 = 30_000;
pub const MAX_IMAGE_SIDE: u32 = 8192;
pub const MAX_IMAGE_PIXELS: u64 = 16_777_216;

// Fixed arrays keep typed values bounded even when constructed outside JSON.
macro_rules! opaque_id {
    ($name:ident, $size:expr) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; $size]);
        impl $name {
            pub fn new(value: &str) -> Result<Self> {
                if value.len() != $size || !value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) || value.bytes().all(|b| b == b'0') {
                    return Err(Error::Invalid(stringify!($name)));
                }
                let mut bytes = [0; $size];
                bytes.copy_from_slice(value.as_bytes());
                Ok(Self(bytes))
            }
            pub fn as_str(&self) -> &str {
                // Constructors and deserialization accept ASCII hex only.
                std::str::from_utf8(&self.0).unwrap_or_default()
            }
        }
        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
                struct Visitor;
                impl serde::de::Visitor<'_> for Visitor {
                    type Value = $name;
                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        write!(f, "a nonzero lowercase hex {}", stringify!($name))
                    }
                    fn visit_str<E: serde::de::Error>(self, value: &str) -> std::result::Result<Self::Value, E> {
                        $name::new(value).map_err(E::custom)
                    }
                }
                deserializer.deserialize_str(Visitor)
            }
        }
    };
}
opaque_id!(ParticipantId, 32);
opaque_id!(DeviceId, 32);
opaque_id!(ProjectId, 32);
opaque_id!(RoomId, 32);
opaque_id!(GameId, 32);
opaque_id!(SubmissionId, 32);
opaque_id!(EventId, 32);
opaque_id!(RequestId, 32);
opaque_id!(CheckpointId, 32);
opaque_id!(ReplayId, 32);
opaque_id!(AssetHash, 64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssignmentId {
    pub game: GameId,
    pub chain: u8,
    pub round: u8,
}
impl AssignmentId {
    pub fn validate(&self) -> Result<()> {
        if self.chain >= 3 || self.round >= 12 {
            return Err(Error::Invalid("assignment coordinates"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Mode {
    Animation { rotations: u8 },
    ExquisiteCorpse,
}
impl Mode {
    pub fn validate(&self) -> Result<()> {
        if matches!(self, Self::Animation { rotations } if !(1..=4).contains(rotations)) {
            return Err(Error::Invalid("rotations (1..4)"));
        }
        Ok(())
    }
    pub fn rounds(&self, players: usize) -> Result<u8> {
        self.validate()?;
        if !(2..=MAX_PLAYERS).contains(&players) {
            return Err(Error::Invalid("player count (2..3)"));
        }
        match self {
            Self::Animation { rotations } => Ok(*rotations * players as u8),
            Self::ExquisiteCorpse => Ok(3),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Participant {
    pub id: ParticipantId,
    pub device: DeviceId,
    pub display_name: String,
}
impl Participant {
    pub fn validate(&self) -> Result<()> {
        validate_label(&self.display_name, MAX_NAME_BYTES, false)
    }
}
pub fn validate_label(value: &str, limit: usize, allow_empty: bool) -> Result<()> {
    if value.len() > limit {
        return Err(Error::Limit("text bytes"));
    }
    if (!allow_empty && value.trim().is_empty()) || value.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err(Error::Invalid("text"));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetRef {
    pub hash: AssetHash,
    pub bytes: u64,
}
impl AssetRef {
    pub fn validate(&self) -> Result<()> {
        if self.bytes == 0 || self.bytes > MAX_ASSET_BYTES {
            return Err(Error::Limit("asset bytes"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointRef {
    pub id: CheckpointId,
    pub asset: AssetRef,
}

/// Validate decoder headers before allocating. Native adapters also bound decompressed bytes
/// for the actual runtime color model/depth; these bounds do not make the editor 8-bit-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageExtent {
    pub width: u32,
    pub height: u32,
}
impl ImageExtent {
    pub fn decoded_bytes(&self, bytes_per_pixel: u64) -> Result<u64> {
        if self.width == 0 || self.height == 0 || self.width > MAX_IMAGE_SIDE || self.height > MAX_IMAGE_SIDE || bytes_per_pixel == 0 {
            return Err(Error::Invalid("image dimensions or pixel format"));
        }
        let pixels = u64::from(self.width) * u64::from(self.height);
        if pixels > MAX_IMAGE_PIXELS {
            return Err(Error::Limit("image pixels"));
        }
        let bytes = pixels.checked_mul(bytes_per_pixel).ok_or(Error::Limit("decoded image bytes"))?;
        if bytes > MAX_PROJECT_BYTES {
            return Err(Error::Limit("decoded image bytes"));
        }
        Ok(bytes)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayRef {
    pub id: ReplayId,
    /// Complete checkpoint-backed replay package. Older K0 fixtures omitted this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<AssetRef>,
    pub baseline: CheckpointRef,
    pub end_event: Option<EventId>,
    pub event_count: u64,
    pub duration_ms: u64,
}
impl ReplayRef {
    pub fn validate(&self) -> Result<()> {
        self.baseline.asset.validate()?;
        if let Some(manifest) = self.manifest {
            manifest.validate()?;
        }
        if self.event_count > MAX_REPLAY_EVENTS || self.duration_ms > MAX_REPLAY_MS {
            return Err(Error::Limit("replay"));
        }
        if (self.event_count == 0) != self.end_event.is_none() {
            return Err(Error::Invalid("replay endpoint"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Submission {
    pub id: SubmissionId,
    pub assignment: AssignmentId,
    pub author: ParticipantId,
    pub project: ProjectId,
    pub revision: u64,
    /// Flattened lossless final frame; references must not expose editable checkpoint internals.
    pub image: AssetRef,
    pub checkpoint: CheckpointRef,
    pub replay: ReplayRef,
    pub assets: Vec<AssetRef>,
    pub note: String,
    /// Service-generated, verified bottom 24px derivative; never a client-supplied permission grant.
    pub seam: Option<AssetRef>,
}
impl Submission {
    pub fn validate(&self) -> Result<()> {
        self.assignment.validate()?;
        if self.revision == 0 {
            return Err(Error::Invalid("durable revision"));
        }
        self.checkpoint.asset.validate()?;
        self.replay.validate()?;
        if self.image.hash == self.checkpoint.asset.hash || self.image.hash == self.replay.baseline.asset.hash {
            return Err(Error::Invalid("image must be a separate rendered asset"));
        }
        validate_assets(&self.assets)?;
        validate_label(&self.note, MAX_NOTE_BYTES, true)?;
        for required in [Some(self.image), Some(self.checkpoint.asset), Some(self.replay.baseline.asset), self.replay.manifest, self.seam].into_iter().flatten()
        {
            required.validate()?;
            if !self.assets.contains(&required) {
                return Err(Error::Invalid("missing asset dependency"));
            }
        }
        if self.seam.is_some_and(|seam| seam.hash == self.image.hash || seam.hash == self.checkpoint.asset.hash || seam.hash == self.replay.baseline.asset.hash)
        {
            return Err(Error::Invalid("seam must be a separate derived asset"));
        }
        Ok(())
    }
}

/// A local durable boundary, written together with device sequence allocation and replay.
/// K1 owns filesystem transactions; the record alone makes no fsync guarantee.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectRecord {
    pub schema_version: u16,
    pub project_id: ProjectId,
    pub owner: ParticipantId,
    pub device: DeviceId,
    pub device_sequence: u64,
    pub local_revision: u64,
    pub synced_revision: Option<u64>,
    pub checkpoint: CheckpointRef,
    pub replay: ReplayRef,
    pub assets: Vec<AssetRef>,
}
impl ProjectRecord {
    pub fn validate(&self) -> Result<()> {
        validate_version(self.schema_version)?;
        if self.device_sequence == 0 || self.local_revision == 0 || self.synced_revision.is_some_and(|r| r > self.local_revision) {
            return Err(Error::Invalid("project durability sequence/revision"));
        }
        self.replay.validate()?;
        validate_assets(&self.assets)?;
        if !self.assets.contains(&self.checkpoint.asset)
            || !self.assets.contains(&self.replay.baseline.asset)
            || self.replay.manifest.is_some_and(|manifest| !self.assets.contains(&manifest))
        {
            return Err(Error::Invalid("project checkpoint dependency"));
        }
        Ok(())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let record: Self = decode_bounded(bytes)?;
        record.validate()?;
        Ok(record)
    }
}
pub fn validate_assets(assets: &[AssetRef]) -> Result<()> {
    if assets.is_empty() || assets.len() > MAX_ASSETS {
        return Err(Error::Limit("asset count"));
    }
    let mut hashes = std::collections::BTreeSet::new();
    let mut total = 0_u64;
    for asset in assets {
        asset.validate()?;
        if !hashes.insert(asset.hash) {
            return Err(Error::Invalid("duplicate asset"));
        }
        total = total.checked_add(asset.bytes).ok_or(Error::Limit("project bytes"))?;
        if total > MAX_PROJECT_BYTES {
            return Err(Error::Limit("project bytes"));
        }
    }
    Ok(())
}

/// K1 persists the resolved canonical event bytes as an immutable asset; never a path or an
/// arbitrary engine command to execute. Unsupported operations use checkpoint transitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReplayStep {
    Resolved { operation_version: u16, payload: AssetRef, result: CheckpointRef },
    Checkpoint { result: CheckpointRef },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventEnvelope {
    pub schema_version: u16,
    pub event_id: EventId,
    pub actor_id: ParticipantId,
    pub device_id: DeviceId,
    pub device_sequence: u64,
    pub project_id: ProjectId,
    pub base_revision: u64,
    pub dependencies: Vec<EventId>,
    pub relative_ms: u64,
    pub step: ReplayStep,
}
impl EventEnvelope {
    pub fn validate(&self) -> Result<()> {
        validate_version(self.schema_version)?;
        if self.device_sequence == 0 || self.base_revision == u64::MAX {
            return Err(Error::Invalid("event sequence/revision"));
        }
        if self.dependencies.len() > MAX_DEPENDENCIES || self.relative_ms > MAX_REPLAY_MS {
            return Err(Error::Limit("event dependencies/time"));
        }
        let mut seen = std::collections::BTreeSet::new();
        if self.dependencies.iter().any(|id| *id == self.event_id || !seen.insert(*id)) {
            return Err(Error::Invalid("event dependencies"));
        }
        match self.step {
            ReplayStep::Resolved { operation_version, payload, result } => {
                if operation_version != 1 {
                    return Err(Error::Version(operation_version));
                }
                payload.validate()?;
                result.asset.validate()?;
            }
            ReplayStep::Checkpoint { result } => result.asset.validate()?,
        }
        Ok(())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value: Self = decode_bounded(bytes)?;
        value.validate()?;
        Ok(value)
    }
}
pub fn validate_version(version: u16) -> Result<()> {
    if version != SCHEMA_VERSION {
        return Err(Error::Version(version));
    }
    Ok(())
}
/// Enforce at the streaming transport before buffering too. Direct serde use is not an ingress API.
pub fn decode_bounded<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(Error::Limit("message bytes"));
    }
    serde_json::from_slice(bytes).map_err(|e| Error::Json(e.to_string()))
}
