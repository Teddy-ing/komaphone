//! Versioned, platform-independent shared-canvas operations.
//!
//! This reducer verifies authorship and ordering, not service admission. The
//! authority must authenticate the profile/device and check current membership
//! and its epoch before accepting an operation. An eraser belongs to its device
//! layer; rendering it over other devices' pixels would violate this contract.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{CanvasId, OwnerId, RegionKey, Stroke, StrokeId};

pub mod archive_import;
pub mod media;
pub use archive_import::{ImportOrigin, ImportedActor};
pub mod review;
mod stroke_copy;
pub mod tours;
pub use media::{MediaPlacement, SharedMedia};
pub use review::{Change, ReviewState};
pub use stroke_copy::{StrokeCopyTarget, StrokeProvenance, stroke_copy};
pub use tours::{Tour, TourFrame, TourStop};

pub const SHARED_SCHEMA: u32 = 1;
pub const SHARED_SCHEMA_BRUSH: u32 = 2;
pub const MAX_EVENT_PAGE: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("Invalid shared canvas value: {0}")]
    Invalid(&'static str),
    #[error("Shared canvas limit reached: {0}; retain the local edit for recovery")]
    Limit(&'static str),
    #[error("This device cannot change another device's artwork")]
    Permission,
    #[error("Missing writer operation; expected sequence {expected}")]
    Sequence { expected: u64 },
    #[error("Missing canvas revision; expected revision {expected}")]
    Revision { expected: u64 },
    #[error("Metadata changed; retain the proposed value as a recovery variant (current revision {current})")]
    Conflict { current: u64 },
    #[error("An operation identity was reused with different content")]
    DuplicateMismatch,
    #[error("Unsupported shared canvas schema")]
    Schema,
    #[error(transparent)]
    Scene(#[from] crate::Error),
}
pub type Result<T> = std::result::Result<T, Error>;

macro_rules! identity {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(OwnerId);
        impl $name {
            pub fn new(value: &str) -> Result<Self> {
                Ok(Self(OwnerId::new(value)?))
            }
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }
    };
}
identity!(DeviceId);
identity!(OpId);
identity!(TourId);
identity!(MediaId);
identity!(SourceId);

/// Configurable operational bounds, independent of membership count. The hard
/// maxima keep the in-memory reducer bounded; adapters may choose lower limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub max_operations: usize,
    pub max_strokes: usize,
    pub max_points: usize,
    pub max_tours: usize,
    pub max_media: usize,
    pub max_media_frames: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self { max_operations: 65_536, max_strokes: 16_384, max_points: 262_144, max_tours: 128, max_media: 1024, max_media_frames: 4096 }
    }
}
impl Limits {
    pub fn validate(&self) -> Result<()> {
        if self.max_operations == 0
            || self.max_operations > 1_048_576
            || self.max_strokes == 0
            || self.max_strokes > 262_144
            || self.max_points == 0
            || self.max_points > 4_194_304
            || self.max_tours == 0
            || self.max_tours > 1024
            || self.max_media == 0
            || self.max_media > 4096
            || self.max_media_frames == 0
            || self.max_media_frames > 65_536
        {
            return Err(Error::Invalid("reducer limits"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    PlaceMedia {
        media: Box<MediaPlacement>,
    },
    SetMediaVisible {
        media: MediaId,
        visible: bool,
    },
    AddStroke {
        stroke: Stroke,
    },
    /// Immutable independent drawing copied through current source authorization.
    CopyStroke {
        stroke: Stroke,
        provenance: Box<StrokeProvenance>,
    },
    /// Undo/redo append visibility changes; immutable stroke bodies survive.
    SetStrokeVisible {
        stroke: StrokeId,
        visible: bool,
    },
    Rename {
        name: String,
        expected_revision: u64,
    },
    PutTour {
        tour: Tour,
        expected_revision: u64,
    },
    RemoveTour {
        tour: TourId,
        expected_revision: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldOp {
    pub id: OpId,
    pub canvas: CanvasId,
    pub author: OwnerId,
    pub device: DeviceId,
    pub sequence: u64,
    pub authority_epoch: u64,
    pub action: Edit,
}
impl WorldOp {
    pub fn validate(&self) -> Result<()> {
        if self.sequence == 0 || self.sequence > i64::MAX as u64 || self.authority_epoch == 0 || self.authority_epoch > i64::MAX as u64 {
            return Err(Error::Invalid("operation sequence or authority epoch"));
        }
        if let Edit::CopyStroke { provenance, .. } = &self.action {
            provenance.validate()?;
        }
        match &self.action {
            Edit::PlaceMedia { media } => {
                media.validate()?;
                if media.canvas != self.canvas || media.owner != self.author {
                    return Err(Error::Invalid("media canvas or owner"));
                }
            }
            Edit::AddStroke { stroke } | Edit::CopyStroke { stroke, .. } => {
                stroke.validate()?;
                // Match the K5 persistence boundary: local values alone do not
                // prove that a stroke/brush footprint stays in the world extent.
                crate::render::spatial_cells(stroke, crate::MAX_SPATIAL_CELLS)?;
                if stroke.canvas != self.canvas || stroke.owner != self.author || stroke.revision != 0 {
                    return Err(Error::Invalid("stroke canvas, author, or uncommitted revision"));
                }
            }
            Edit::Rename { name, expected_revision } => {
                crate::validate_name(name)?;
                valid_revision(*expected_revision)?;
            }
            Edit::PutTour { tour, expected_revision } => {
                tour.validate()?;
                if tour.canvas != self.canvas || tour.revision != 0 {
                    return Err(Error::Invalid("tour canvas or uncommitted revision"));
                }
                valid_revision(*expected_revision)?;
            }
            Edit::RemoveTour { expected_revision, .. } => valid_revision(*expected_revision)?,
            Edit::SetStrokeVisible { .. } | Edit::SetMediaVisible { .. } => {}
        }
        Ok(())
    }
}

fn valid_revision(revision: u64) -> Result<()> {
    if revision > i64::MAX as u64 {
        return Err(Error::Limit("revision"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedOp {
    pub revision: u64,
    pub op: WorldOp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedStroke {
    pub stroke: Stroke,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<StrokeProvenance>,
    pub device: DeviceId,
    pub visible: bool,
    pub created_revision: u64,
    pub changed_revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    Applied,
    Duplicate,
}

/// A bounded canonical checkpoint. Adapters retain accepted events durably and
/// load this reducer outside the frame loop. Viewport renderers use their spatial
/// index, not a serialization/replay of this entire state each frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldState {
    pub schema: u32,
    pub canvas: CanvasId,
    pub owner: OwnerId,
    pub name: String,
    pub revision: u64,
    pub metadata_revision: u64,
    pub strokes: BTreeMap<StrokeId, SharedStroke>,
    pub tours: BTreeMap<TourId, Tour>,
    pub media: BTreeMap<MediaId, SharedMedia>,
    /// Untrusted archive provenance, never live membership or authentication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_origin: Option<Box<ImportOrigin>>,
    initial_name: String,
    limits: Limits,
    points: usize,
    media_frames: usize,
    sequences: BTreeMap<DeviceId, u64>,
    device_authors: BTreeMap<DeviceId, OwnerId>,
    receipts: BTreeMap<OpId, usize>,
    events: Vec<AcceptedOp>,
    tour_revisions: BTreeMap<TourId, u64>,
}
impl WorldState {
    pub fn new(canvas: CanvasId, owner: OwnerId, name: String) -> Result<Self> {
        Self::with_limits(canvas, owner, name, Limits::default())
    }
    pub fn with_limits(canvas: CanvasId, owner: OwnerId, name: String, limits: Limits) -> Result<Self> {
        crate::validate_name(&name)?;
        limits.validate()?;
        Ok(Self {
            schema: SHARED_SCHEMA,
            canvas,
            owner,
            initial_name: name.clone(),
            name,
            revision: 0,
            metadata_revision: 0,
            strokes: BTreeMap::new(),
            tours: BTreeMap::new(),
            media: BTreeMap::new(),
            import_origin: None,
            limits,
            points: 0,
            media_frames: 0,
            sequences: BTreeMap::new(),
            device_authors: BTreeMap::new(),
            receipts: BTreeMap::new(),
            events: Vec::new(),
            tour_revisions: BTreeMap::new(),
        })
    }
    pub fn limits(&self) -> Limits {
        self.limits
    }
    pub fn point_count(&self) -> usize {
        self.points
    }
    pub fn operation_count(&self) -> usize {
        self.events.len()
    }
    pub fn next_sequence(&self, device: DeviceId) -> Result<u64> {
        self.sequences.get(&device).copied().unwrap_or(0).checked_add(1).filter(|n| *n <= i64::MAX as u64).ok_or(Error::Limit("writer sequence"))
    }
    pub fn receipt(&self, id: OpId) -> Option<&AcceptedOp> {
        self.receipts.get(&id).and_then(|index| self.events.get(*index))
    }
    pub fn tour_revision(&self, id: TourId) -> u64 {
        self.tour_revisions.get(&id).copied().unwrap_or(0)
    }
    pub fn events_after(&self, after: u64, limit: usize) -> Result<&[AcceptedOp]> {
        if after > self.revision || limit == 0 || limit > MAX_EVENT_PAGE {
            return Err(Error::Invalid("event page"));
        }
        let start = usize::try_from(after).map_err(|_| Error::Limit("event offset"))?;
        let end = start.saturating_add(limit).min(self.events.len());
        self.events.get(start..end).ok_or(Error::Invalid("event range"))
    }
    pub fn apply(&mut self, accepted: &AcceptedOp) -> Result<Applied> {
        if self.schema != SHARED_SCHEMA && self.schema != SHARED_SCHEMA_BRUSH {
            return Err(Error::Schema);
        }
        accepted.op.validate()?;
        valid_revision(accepted.revision)?;
        if let Some(previous) = self.receipt(accepted.op.id) {
            return if previous == accepted { Ok(Applied::Duplicate) } else { Err(Error::DuplicateMismatch) };
        }
        let op = &accepted.op;
        if op.canvas != self.canvas {
            return Err(Error::Invalid("operation canvas"));
        }
        let expected = self.revision.checked_add(1).ok_or(Error::Limit("revision"))?;
        if accepted.revision != expected {
            return Err(Error::Revision { expected });
        }
        if let Some(origin) = &self.import_origin {
            if accepted.revision <= origin.source_revision {
                if !origin.imported_device(op.device) || !origin.actors.iter().any(|actor| actor.imported == op.author) {
                    return Err(Error::Invalid("imported history writer"));
                }
            } else if origin.imported_device(op.device) {
                return Err(Error::Permission);
            }
        }
        let expected = self.next_sequence(op.device)?;
        if op.sequence != expected {
            return Err(Error::Sequence { expected });
        }
        if self.device_authors.get(&op.device).is_some_and(|author| *author != op.author) {
            return Err(Error::Permission);
        }
        if self.events.len() >= self.limits.max_operations {
            return Err(Error::Limit("retained operation history"));
        }
        // Validate the complete candidate before changing any field.
        match &op.action {
            Edit::PlaceMedia { media } => {
                if self.media.contains_key(&media.id) {
                    return Err(Error::DuplicateMismatch);
                }
                if self.media.len() >= self.limits.max_media || self.media_frames.saturating_add(media.frames.len()) > self.limits.max_media_frames {
                    return Err(Error::Limit("retained media or frames"));
                }
            }
            Edit::SetMediaVisible { media, .. } => {
                let existing = self.media.get(media).ok_or(Error::Invalid("missing media"))?;
                if existing.device != op.device || existing.media.owner != op.author {
                    return Err(Error::Permission);
                }
            }
            Edit::AddStroke { stroke } | Edit::CopyStroke { stroke, .. } => {
                if self.strokes.contains_key(&stroke.id) {
                    return Err(Error::DuplicateMismatch);
                }
                if self.strokes.len() >= self.limits.max_strokes {
                    return Err(Error::Limit("stroke count"));
                }
                if self.points.saturating_add(stroke.points.len()) > self.limits.max_points {
                    return Err(Error::Limit("retained stroke points"));
                }
            }
            Edit::SetStrokeVisible { stroke, .. } => {
                let existing = self.strokes.get(stroke).ok_or(Error::Invalid("missing stroke"))?;
                if existing.device != op.device || existing.stroke.owner != op.author {
                    return Err(Error::Permission);
                }
            }
            Edit::Rename { expected_revision, .. } => {
                let historical = self.import_origin.as_ref().filter(|origin| accepted.revision <= origin.source_revision);
                let permitted =
                    historical.map_or(op.author == self.owner, |origin| origin.actors.iter().any(|actor| actor.imported == op.author && actor.can_rename));
                if !permitted {
                    return Err(Error::Permission);
                }
                if *expected_revision != self.metadata_revision {
                    return Err(Error::Conflict { current: self.metadata_revision });
                }
            }
            Edit::PutTour { tour, expected_revision } => {
                let current = self.tour_revision(tour.id);
                if *expected_revision != current {
                    return Err(Error::Conflict { current });
                }
                if !self.tour_revisions.contains_key(&tour.id) && self.tour_revisions.len() >= self.limits.max_tours {
                    return Err(Error::Limit("tour identities"));
                }
            }
            Edit::RemoveTour { tour, expected_revision } => {
                let current = self.tour_revision(*tour);
                if *expected_revision != current {
                    return Err(Error::Conflict { current });
                }
                if !self.tours.contains_key(tour) {
                    return Err(Error::Invalid("missing tour"));
                }
            }
        }
        match &op.action {
            Edit::PlaceMedia { media } => {
                self.media_frames += media.frames.len();
                self.media.insert(
                    media.id,
                    SharedMedia {
                        media: *media.clone(),
                        device: op.device,
                        visible: true,
                        created_revision: accepted.revision,
                        changed_revision: accepted.revision,
                    },
                );
            }
            Edit::SetMediaVisible { media, visible } => {
                let existing = self.media.get_mut(media).ok_or(Error::Invalid("missing media"))?;
                existing.visible = *visible;
                existing.changed_revision = accepted.revision;
            }
            Edit::AddStroke { stroke } | Edit::CopyStroke { stroke, .. } => {
                if stroke.uses_brush_v2() {
                    self.schema = self.schema.max(SHARED_SCHEMA_BRUSH);
                }
                self.points += stroke.points.len();
                self.strokes.insert(
                    stroke.id,
                    SharedStroke {
                        stroke: stroke.clone(),
                        provenance: match &op.action {
                            Edit::CopyStroke { provenance, .. } => Some(*provenance.clone()),
                            _ => None,
                        },
                        device: op.device,
                        visible: true,
                        created_revision: accepted.revision,
                        changed_revision: accepted.revision,
                    },
                );
            }
            Edit::SetStrokeVisible { stroke, visible } => {
                // Presence was checked before mutation. A missing item here would
                // mean corrupt state; return an error instead of indexing/panicking.
                let existing = self.strokes.get_mut(stroke).ok_or(Error::Invalid("missing stroke"))?;
                existing.visible = *visible;
                existing.changed_revision = accepted.revision;
            }
            Edit::Rename { name, .. } => {
                self.name = name.clone();
                self.metadata_revision = accepted.revision;
            }
            Edit::PutTour { tour, .. } => {
                let mut committed = tour.clone();
                committed.revision = accepted.revision;
                self.tours.insert(tour.id, committed);
                self.tour_revisions.insert(tour.id, accepted.revision);
            }
            Edit::RemoveTour { tour, .. } => {
                self.tours.remove(tour);
                self.tour_revisions.insert(*tour, accepted.revision);
            }
        }
        self.revision = accepted.revision;
        self.sequences.insert(op.device, op.sequence);
        self.device_authors.insert(op.device, op.author);
        self.receipts.insert(op.id, self.events.len());
        self.events.push(accepted.clone());
        Ok(Applied::Applied)
    }
    /// Validate untrusted/deserialized checkpoints once at the storage boundary.
    /// Transport adapters must additionally cap encoded/decompressed input bytes.
    pub fn validate(&self) -> Result<()> {
        if self.schema != SHARED_SCHEMA && self.schema != SHARED_SCHEMA_BRUSH {
            return Err(Error::Schema);
        }
        self.limits.validate()?;
        if self.events.len() > self.limits.max_operations
            || self.strokes.len() > self.limits.max_strokes
            || self.points > self.limits.max_points
            || self.tour_revisions.len() > self.limits.max_tours
            || self.media.len() > self.limits.max_media
            || self.media_frames > self.limits.max_media_frames
        {
            return Err(Error::Limit("checkpoint"));
        }
        if let Some(origin) = &self.import_origin {
            origin.validate(self.canvas, self.owner)?;
        }
        let mut rebuilt = Self::with_limits(self.canvas, self.owner, self.initial_name.clone(), self.limits)?;
        rebuilt.import_origin = self.import_origin.clone();
        for event in &self.events {
            rebuilt.apply(event)?;
        }
        if rebuilt != *self {
            return Err(Error::Invalid("checkpoint differs from accepted history"));
        }
        self.validate_imported_prefix()?;
        Ok(())
    }
    /// Replay every causal event even when the review UI filters authors.
    pub fn replay_to(&self, revision: u64) -> Result<Self> {
        if revision > self.revision {
            return Err(Error::Invalid("future replay revision"));
        }
        let mut result = Self::with_limits(self.canvas, self.owner, self.initial_name.clone(), self.limits)?;
        result.import_origin = self.import_origin.clone();
        let count = usize::try_from(revision).map_err(|_| Error::Limit("replay revision"))?;
        for event in self.events.get(..count).ok_or(Error::Invalid("replay history gap"))? {
            result.apply(event)?;
        }
        Ok(result)
    }
    /// Canonical checkpoint hash, independent of map insertion order and clocks.
    /// Compute on accepted changes/export, never on every paint frame.
    pub fn canonical_hash(&self) -> Result<String> {
        let bytes = serde_json::to_vec(self).map_err(|_| Error::Invalid("checkpoint serialization"))?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
    /// Copy accepted history into a fresh canvas namespace. This is pure data
    /// conversion, never proof of ownership or permission to publish the copy.
    /// Historical writer IDs and source provenance retain their original meaning.
    pub fn copy_to(&self, canvas: CanvasId) -> Result<Self> {
        self.validate()?;
        if canvas == self.canvas {
            return Err(Error::Invalid("copy requires a new canvas identity"));
        }
        if self.import_origin.as_ref().is_some_and(|origin| self.revision < origin.source_revision) {
            return Err(Error::Invalid("copy requires the complete imported history prefix; partial replay is read-only"));
        }
        let mut result = Self::with_limits(canvas, self.owner, self.initial_name.clone(), self.limits)?;
        result.import_origin = self.import_origin.clone();
        for original in &self.events {
            let mut event = original.clone();
            event.op.canvas = canvas;
            match &mut event.op.action {
                Edit::AddStroke { stroke } | Edit::CopyStroke { stroke, .. } => stroke.canvas = canvas,
                Edit::PlaceMedia { media } => media.canvas = canvas,
                Edit::PutTour { tour, .. } => {
                    tour.canvas = canvas;
                    for stop in &mut tour.stops {
                        stop.camera.canvas = canvas;
                    }
                }
                _ => {}
            }
            result.apply(&event)?;
        }
        result.refresh_imported_prefix_hash()?;
        Ok(result)
    }
    pub fn change(&self, event: &AcceptedOp) -> Result<Change> {
        if event.op.canvas != self.canvas || self.receipt(event.op.id) != Some(event) {
            return Err(Error::Invalid("unaccepted change"));
        }
        let region: Option<RegionKey> = match &event.op.action {
            Edit::PlaceMedia { media } => Some(media.anchor),
            Edit::SetMediaVisible { media, .. } => self.media.get(media).map(|value| value.media.anchor),
            Edit::AddStroke { stroke } | Edit::CopyStroke { stroke, .. } => Some(stroke.anchor),
            Edit::SetStrokeVisible { stroke, .. } => self.strokes.get(stroke).map(|value| value.stroke.anchor),
            _ => None,
        };
        Ok(Change { revision: event.revision, author: event.op.author, device: event.op.device, region })
    }
}
