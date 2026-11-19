//! Untrusted portable content becomes an isolated historical prefix, never a grant.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::media::{AssetHash, MediaFrame};
use super::{DeviceId, Edit, Error, Limits, MediaId, OpId, Result, TourId, WorldState};
use crate::{CanvasId, OwnerId, StrokeId};

pub const MAX_CHUNK_BYTES: usize = 65_536;
pub const MAX_MANIFEST_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
/// Input bound, not membership capacity. 128 summary rows remain below 2 MiB.
pub const MAX_IMPORTED_ACTORS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedActor {
    /// Historical archive assertion, not a verified account on this service.
    pub original: OwnerId,
    pub imported: OwnerId,
    /// Historical owner actions in the imported prefix only, never a live grant.
    pub can_rename: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportOrigin {
    pub source_canvas: CanvasId,
    pub source_owner: OwnerId,
    pub source_revision: u64,
    pub source_hash: AssetHash,
    /// Canonical transformed prefix with import_origin omitted (no circular hash).
    pub prefix_hash: AssetHash,
    pub actors: Vec<ImportedActor>,
    /// Monotonic low-64-bit ordinals preserve original device compositing order.
    pub device_prefix: u64,
    pub device_count: u32,
    pub limits: Limits,
}
impl ImportOrigin {
    pub fn validate(&self, canvas: CanvasId, owner: OwnerId) -> Result<()> {
        self.limits.validate()?;
        super::valid_revision(self.source_revision)?;
        if canvas == self.source_canvas
            || self.device_prefix == 0
            || self.actors.is_empty()
            || self.actors.len() > MAX_IMPORTED_ACTORS
            || self.source_revision > self.limits.max_operations as u64
            || u64::from(self.device_count) > self.source_revision
            || (self.source_revision > 0 && self.device_count == 0)
        {
            return Err(Error::Invalid("archive origin bounds"));
        }
        let mut originals = BTreeSet::new();
        let mut aliases = BTreeSet::new();
        for actor in &self.actors {
            originals.insert(actor.original);
            if actor.imported == owner || !aliases.insert(actor.imported) {
                return Err(Error::Invalid("archive artist aliases"));
            }
        }
        if !originals.contains(&self.source_owner)
            || !self.actors.iter().any(|actor| actor.can_rename)
            || !self.actors.windows(2).all(|pair| pair.first().zip(pair.get(1)).is_some_and(|(a, b)| a.imported < b.imported))
        {
            return Err(Error::Invalid("archive original owner or artist order"));
        }
        Ok(())
    }
    pub fn original_author(&self, imported: OwnerId) -> Option<OwnerId> {
        self.actors.iter().find(|actor| actor.imported == imported).map(|actor| actor.original)
    }
    pub fn imported_device(&self, device: DeviceId) -> bool {
        let value = device.as_str();
        let prefix = value.get(..16).and_then(|s| u64::from_str_radix(s, 16).ok());
        let ordinal = value.get(16..).and_then(|s| u64::from_str_radix(s, 16).ok());
        prefix == Some(self.device_prefix) && ordinal.is_some_and(|n| n > 0 && n <= u64::from(self.device_count))
    }
}

/// Exact existing archive envelope. Encoded bounds are enforced before parsing;
/// adapters must additionally verify each PNG's bytes/hash/dimensions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveManifest {
    pub schema: u32,
    pub world: WorldState,
    pub assets: Vec<MediaFrame>,
}
impl ArchiveManifest {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(Error::Limit("archive manifest bytes"));
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| Error::Invalid("archive manifest"))?;
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> Result<()> {
        let expected_world_schema = match self.schema {
            1 => super::SHARED_SCHEMA,
            2 => super::SHARED_SCHEMA_BRUSH,
            _ => return Err(Error::Schema),
        };
        self.world.validate()?;
        if self.world.schema != expected_world_schema {
            return Err(Error::Schema);
        }
        self.world.validate_complete_import()?;
        if self.assets.len() > self.world.limits().max_media_frames {
            return Err(Error::Limit("archive assets"));
        }
        let mut expected = BTreeMap::new();
        for media in self.world.media.values() {
            for frame in &media.media.frames {
                let descriptor = (frame.byte_len, frame.width, frame.height);
                if expected.insert(frame.hash, descriptor).is_some_and(|old| old != descriptor) {
                    return Err(Error::Invalid("conflicting archive asset descriptors"));
                }
            }
        }
        if expected.len() != self.assets.len() {
            return Err(Error::Invalid("archive asset inventory"));
        }
        let mut seen = BTreeSet::new();
        let mut total = 0u64;
        for frame in &self.assets {
            frame.validate()?;
            if !seen.insert(frame.hash) || expected.get(&frame.hash) != Some(&(frame.byte_len, frame.width, frame.height)) {
                return Err(Error::Invalid("archive asset reference"));
            }
            total = total.checked_add(frame.byte_len).filter(|total| *total <= MAX_TOTAL_BYTES).ok_or(Error::Limit("archive total bytes"))?;
        }
        Ok(())
    }
}

fn mapped(canvas: CanvasId, kind: &str, original: &str) -> Result<String> {
    let mut hash = Sha256::new();
    hash.update(b"komaphone-import-v1\0");
    hash.update(canvas.as_str().as_bytes());
    hash.update([0]);
    hash.update(kind.as_bytes());
    hash.update([0]);
    hash.update(original.as_bytes());
    let value = format!("{:x}", hash.finalize());
    value.get(..32).map(str::to_owned).ok_or(Error::Invalid("archive identity hash"))
}

impl WorldState {
    /// Portable output must contain the whole retained imported history. Partial
    /// replay is valid for viewing, but is not a complete backup or import input.
    pub fn validate_complete_import(&self) -> Result<()> {
        if self.import_origin.as_ref().is_some_and(|origin| self.revision < origin.source_revision) {
            return Err(Error::Invalid("archive imported history is incomplete; finish synchronizing before export or import"));
        }
        Ok(())
    }
    /// Unverified archive attribution only; never a live account or grant.
    pub fn imported_credit(&self, author: OwnerId) -> Option<super::media::Credit> {
        let original = self.import_origin.as_ref()?.original_author(author)?;
        Some(super::media::Credit { profile: original, label: format!("Archive author {}", original.as_str()) })
    }

    /// Copy from this world's retained source, resolving inert historical aliases
    /// only through its archive provenance. Ordinary artist checks stay intact.
    pub fn copy_stroke(&self, source_id: StrokeId, fallback_credit: super::media::Credit, target: super::StrokeCopyTarget) -> Result<Edit> {
        let source = self.strokes.get(&source_id).ok_or(Error::Invalid("stroke copy source"))?;
        let archived = self.imported_credit(source.stroke.owner).filter(|_| source.provenance.is_none());
        let credit = archived.as_ref().map_or(fallback_credit, |credit| super::media::Credit { profile: source.stroke.owner, label: credit.label.clone() });
        let mut edit = super::stroke_copy(source, self.canvas, credit, target)?;
        if let (Some(credit), Edit::CopyStroke { provenance, .. }) = (archived, &mut edit) {
            provenance.author = credit;
            provenance.validate()?;
        }
        Ok(edit)
    }

    /// The summary supplies only inert provenance; membership stays external.
    pub fn with_import_origin(canvas: CanvasId, owner: OwnerId, name: String, origin: Option<Box<ImportOrigin>>) -> Result<Self> {
        if let Some(value) = &origin {
            value.validate(canvas, owner)?;
        }
        let limits = origin.as_ref().map_or_else(Limits::default, |value| value.limits);
        let mut result = Self::with_limits(canvas, owner, name, limits)?;
        result.import_origin = origin;
        Ok(result)
    }
    pub fn imported_device(&self, device: DeviceId) -> bool {
        self.import_origin.as_ref().is_some_and(|origin| origin.imported_device(device))
    }
    /// Pure deterministic conversion. Every source actor/device is inert, every
    /// object namespace is fresh, and historical assertions remain labeled data.
    /// Re-import flattens artist credits while keeping distinct inert aliases;
    /// the immediate source hash identifies its complete prior history.
    pub fn import_to(&self, canvas: CanvasId, importer: OwnerId) -> Result<Self> {
        self.validate()?;
        self.validate_complete_import()?;
        if canvas == self.canvas {
            return Err(Error::Invalid("archive import requires a new canvas"));
        }
        let mut originals = BTreeSet::from([self.owner]);
        for event in &self.events {
            originals.insert(event.op.author);
        }
        if originals.len() > MAX_IMPORTED_ACTORS {
            return Err(Error::Limit("archive historical artists (128); original archive is unchanged"));
        }
        let mut actors = Vec::new();
        let mut authors = BTreeMap::new();
        for source in originals {
            let imported = OwnerId::new(&mapped(canvas, "artist", source.as_str())?)?;
            let previous = self.import_origin.as_ref().and_then(|origin| origin.actors.iter().find(|actor| actor.imported == source));
            actors.push(ImportedActor {
                original: previous.map_or(source, |actor| actor.original),
                imported,
                can_rename: source == self.owner || previous.is_some_and(|actor| actor.can_rename),
            });
            authors.insert(source, imported);
        }
        actors.sort_by_key(|actor| actor.imported);
        let namespace = mapped(canvas, "devices", self.canvas.as_str())?;
        let prefix = namespace.get(..16).and_then(|s| u64::from_str_radix(s, 16).ok()).filter(|n| *n != 0).ok_or(Error::Invalid("archive device namespace"))?;
        let mut devices = BTreeMap::new();
        for (index, original) in self.device_authors.keys().enumerate() {
            let ordinal = u64::try_from(index).ok().and_then(|n| n.checked_add(1)).ok_or(Error::Limit("archive devices"))?;
            devices.insert(*original, DeviceId::new(&format!("{prefix:016x}{ordinal:016x}"))?);
        }
        let source_hash = AssetHash::new(&self.canonical_hash()?)?;
        let origin = ImportOrigin {
            source_canvas: self.canvas,
            source_owner: self.owner,
            source_revision: self.revision,
            source_hash,
            prefix_hash: source_hash,
            actors,
            device_prefix: prefix,
            device_count: u32::try_from(devices.len()).map_err(|_| Error::Limit("archive devices"))?,
            limits: self.limits,
        };
        let mut result = Self::with_import_origin(canvas, importer, self.initial_name.clone(), Some(Box::new(origin)))?;
        for original in &self.events {
            let mut event = original.clone();
            event.op.id = OpId::new(&mapped(canvas, "operation", original.op.id.as_str())?)?;
            event.op.canvas = canvas;
            event.op.author = *authors.get(&original.op.author).ok_or(Error::Invalid("archive author"))?;
            event.op.device = *devices.get(&original.op.device).ok_or(Error::Invalid("archive device"))?;
            // Source epochs are history, not destination access epochs.
            event.op.authority_epoch = 1;
            match &mut event.op.action {
                Edit::AddStroke { stroke } | Edit::CopyStroke { stroke, .. } => {
                    stroke.id = StrokeId::new(&mapped(canvas, "stroke", stroke.id.as_str())?)?;
                    stroke.canvas = canvas;
                    stroke.owner = event.op.author;
                }
                Edit::SetStrokeVisible { stroke, .. } => *stroke = StrokeId::new(&mapped(canvas, "stroke", stroke.as_str())?)?,
                Edit::PlaceMedia { media } => {
                    media.id = MediaId::new(&mapped(canvas, "media", media.id.as_str())?)?;
                    media.canvas = canvas;
                    media.owner = event.op.author;
                }
                Edit::SetMediaVisible { media, .. } => *media = MediaId::new(&mapped(canvas, "media", media.as_str())?)?,
                Edit::PutTour { tour, .. } => {
                    tour.id = TourId::new(&mapped(canvas, "tour", tour.id.as_str())?)?;
                    tour.canvas = canvas;
                    for stop in &mut tour.stops {
                        stop.camera.canvas = canvas;
                        if let Some(target) = stop.target {
                            stop.target = Some(StrokeId::new(&mapped(canvas, "stroke", target.as_str())?)?);
                        }
                    }
                }
                Edit::RemoveTour { tour, .. } => *tour = TourId::new(&mapped(canvas, "tour", tour.as_str())?)?,
                Edit::Rename { .. } => {}
            }
            result.apply(&event)?;
        }
        result.refresh_imported_prefix_hash()?;
        result.validate()?;
        Ok(result)
    }
    fn imported_prefix_hash(&self) -> Result<Option<AssetHash>> {
        let Some(origin) = &self.import_origin else {
            return Ok(None);
        };
        if self.revision < origin.source_revision {
            return Ok(None);
        }
        let mut prefix = self.replay_to(origin.source_revision)?;
        prefix.import_origin = None;
        Ok(Some(AssetHash::new(&prefix.canonical_hash()?)?))
    }
    pub(super) fn validate_imported_prefix(&self) -> Result<()> {
        if let Some(hash) = self.imported_prefix_hash()?
            && self.import_origin.as_ref().is_none_or(|origin| origin.prefix_hash != hash)
        {
            return Err(Error::Invalid("imported prefix hash"));
        }
        Ok(())
    }
    pub(super) fn refresh_imported_prefix_hash(&mut self) -> Result<()> {
        if let Some(hash) = self.imported_prefix_hash()?
            && let Some(origin) = &mut self.import_origin
        {
            origin.prefix_hash = hash;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
