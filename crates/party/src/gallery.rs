//! Portable gallery originals. UI state and credentials never enter these bundles.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::replay_artifact::ReplayArtifact;
use crate::room::{Phase, RevealedEntry, RoomView};
use crate::{AssetHash, AssetRef, Error, GameId, Participant, ParticipantId, ProjectId, Result};

pub const GALLERY_SCHEMA: u16 = 1;
pub const MAX_GALLERY_OBJECTS: usize = 16_384;
pub const MAX_GALLERY_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_GALLERY_MANIFEST_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemixProvenance {
    pub source_entry: ProjectId,
    pub source_game: Option<GameId>,
    pub source_revision: u64,
    pub chain: u8,
    /// None preserves the whole animation or creature. Some explicitly extracts a still.
    pub frame: Option<u8>,
    pub authors: Vec<Participant>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GalleryContent {
    Game {
        room: Box<RoomView>,
    },
    Project {
        checkpoint: AssetRef,
        /// Oversized checkpoint entries may not fit in a framed replay pack.
        #[serde(default)]
        direct_assets: Vec<AssetRef>,
        /// None labels an imported legacy baseline, never invented earlier history.
        replay: Option<Box<ReplayArtifact>>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GalleryManifest {
    pub schema_version: u16,
    pub id: ProjectId,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub authors: Vec<Participant>,
    pub content: GalleryContent,
    pub provenance: Option<RemixProvenance>,
    pub fps: u8,
    pub looping: bool,
    /// Loaded only on explicit request; never part of replay or raster exports.
    #[serde(default)]
    pub notes: Vec<(crate::AssignmentId, String)>,
}

#[derive(Clone, Debug)]
pub struct GalleryBundle {
    pub manifest: GalleryManifest,
    pub objects: BTreeMap<AssetHash, Vec<u8>>,
}

impl GalleryManifest {
    /// Native project identities survive portable copies and import conflict IDs.
    /// Revealed games are authorized by their terminal game projection instead.
    pub fn source_projects(&self) -> Vec<ProjectId> {
        let GalleryContent::Project { replay, .. } = &self.content else { return Vec::new() };
        let mut sources = vec![self.id];
        if let Some(replay) = replay {
            sources.push(replay.project);
        }
        if let Some(provenance) = &self.provenance
            && provenance.source_game.is_none()
        {
            sources.push(provenance.source_entry);
        }
        sources.sort_unstable();
        sources.dedup();
        sources
    }
    /// Only a terminal participant projection may become a local game original.
    pub fn from_reveal(id: ProjectId, room: &RoomView, actor: ParticipantId, timestamp: String) -> Result<Self> {
        if !room.members.iter().any(|member| member.participant.id == actor) {
            return Err(Error::Unauthorized);
        }
        let title = match room.mode {
            crate::Mode::Animation { .. } => "Animation game",
            crate::Mode::ExquisiteCorpse => "Exquisite Corpse game",
        };
        let mut projection = room.clone();
        // Historical game originals carry no live assignment reference or its note.
        projection.assignment = None;
        projection.waiting.clear();
        let manifest = Self {
            schema_version: GALLERY_SCHEMA,
            id,
            title: title.into(),
            created_at: timestamp.clone(),
            updated_at: timestamp,
            authors: room.members.iter().map(|member| member.participant.clone()).collect(),
            content: GalleryContent::Game { room: Box::new(projection) },
            provenance: None,
            fps: 6,
            looping: true,
            notes: Vec::new(),
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn game(&self) -> Option<GameId> {
        match &self.content {
            GalleryContent::Game { room } => room.reveal.first().map(|entry| match entry {
                RevealedEntry::Artwork { assignment, .. } | RevealedEntry::Missing { assignment, .. } => assignment.id.game,
            }),
            GalleryContent::Project { .. } => None,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != GALLERY_SCHEMA {
            return Err(Error::Version(self.schema_version));
        }
        crate::validate_label(&self.title, 96, false)?;
        // Parsing calendar values belongs to the native adapter's chrono boundary.
        for date in [&self.created_at, &self.updated_at] {
            crate::validate_label(date, 64, false)?;
        }
        validate_authors(&self.authors)?;
        if !(1..=24).contains(&self.fps) {
            return Err(Error::Invalid("gallery frame rate"));
        }
        if self.notes.len() > 36 || self.notes.iter().map(|(id, _)| *id).collect::<BTreeSet<_>>().len() != self.notes.len() {
            return Err(Error::Limit("gallery notes"));
        }
        for (id, note) in &self.notes {
            crate::validate_label(note, crate::MAX_NOTE_BYTES, true)?;
            if !matches!(&self.content, GalleryContent::Game {room} if room.reveal.iter().any(|entry|matches!(entry,RevealedEntry::Artwork {assignment,..} if assignment.id == *id)))
            {
                return Err(Error::Invalid("gallery note assignment"));
            }
        }
        if let Some(source) = &self.provenance {
            validate_authors(&source.authors)?;
            if source.source_entry == self.id || source.source_revision == 0 || source.chain >= 3 || source.frame.is_some_and(|frame| frame >= 12) {
                return Err(Error::Invalid("remix provenance"));
            }
        }
        match &self.content {
            GalleryContent::Game { room } => {
                if !matches!(room.phase, Phase::Reveal | Phase::Archived) || room.revision == 0 || room.assignment.is_some() || !room.waiting.is_empty() {
                    return Err(Error::Phase);
                }
                let rounds = room.mode.rounds(room.members.len())?;
                if room.reveal.len() != room.members.len().saturating_mul(usize::from(rounds))
                    || self.authors != room.members.iter().map(|m| m.participant.clone()).collect::<Vec<_>>()
                {
                    return Err(Error::Invalid("gallery roster or game schedule"));
                }
                let game = self.game().ok_or(Error::Invalid("empty gallery game"))?;
                let mut assignments = BTreeSet::new();
                let mut projects = BTreeSet::new();
                for entry in &room.reveal {
                    let assignment = match entry {
                        RevealedEntry::Artwork { assignment, revision, image, checkpoint, replay, assets } => {
                            if *revision == 0 {
                                return Err(Error::Invalid("gallery durable revision"));
                            }
                            image.validate()?;
                            checkpoint.asset.validate()?;
                            replay.validate()?;
                            crate::contracts::validate_assets(assets)?;
                            for required in [Some(*image), Some(checkpoint.asset), Some(replay.baseline.asset), replay.manifest].into_iter().flatten() {
                                if !assets.contains(&required) {
                                    return Err(Error::Invalid("gallery dependency"));
                                }
                            }
                            assignment
                        }
                        RevealedEntry::Missing { assignment, reason } => {
                            crate::validate_label(reason, 256, false)?;
                            assignment
                        }
                    };
                    let slot = usize::from(assignment.id.chain).saturating_add(usize::from(assignment.id.round)) % room.members.len();
                    if assignment.id.game != game
                        || usize::from(assignment.id.chain) >= room.members.len()
                        || assignment.id.round >= rounds
                        || room.members.get(slot).is_none_or(|m| m.participant.id != assignment.player)
                        || !assignments.insert(assignment.id)
                        || !projects.insert(assignment.project)
                    {
                        return Err(Error::Invalid("gallery assignment"));
                    }
                }
            }
            GalleryContent::Project { checkpoint, replay, direct_assets } => {
                checkpoint.validate()?;
                if !direct_assets.is_empty() {
                    crate::contracts::validate_assets(direct_assets)?;
                }
                if let Some(replay) = replay {
                    validate_project_replay(replay, *checkpoint)?;
                }
            }
        }
        Ok(())
    }

    pub fn required_assets(&self) -> Result<Vec<AssetRef>> {
        self.validate()?;
        let mut assets = BTreeMap::new();
        let mut add = |asset: AssetRef| -> Result<()> {
            asset.validate()?;
            if assets.insert(asset.hash, asset).is_some_and(|previous| previous != asset) {
                return Err(Error::Invalid("conflicting asset length"));
            }
            Ok(())
        };
        match &self.content {
            GalleryContent::Game { room } => {
                for entry in &room.reveal {
                    if let RevealedEntry::Artwork { assets, .. } = entry {
                        for asset in assets {
                            add(*asset)?;
                        }
                    }
                }
            }
            GalleryContent::Project { checkpoint, replay, direct_assets } => {
                add(*checkpoint)?;
                for asset in direct_assets {
                    add(*asset)?;
                }
                if let Some(replay) = replay {
                    for asset in &replay.packs {
                        add(*asset)?;
                    }
                    if replay.packs.is_empty() {
                        for step in &replay.steps {
                            add(step.checkpoint.asset)?;
                        }
                    }
                    if let Some(first) = replay.steps.first() {
                        add(first.checkpoint.asset)?;
                    }
                }
            }
        }
        if assets.len() > MAX_GALLERY_OBJECTS {
            return Err(Error::Limit("gallery objects"));
        }
        Ok(assets.into_values().collect())
    }
}

fn validate_authors(authors: &[Participant]) -> Result<()> {
    if authors.len() > 3 || authors.iter().map(|author| author.id).collect::<BTreeSet<_>>().len() != authors.len() {
        return Err(Error::Limit("gallery authors"));
    }
    for author in authors {
        author.validate()?;
    }
    Ok(())
}

fn validate_project_replay(replay: &ReplayArtifact, checkpoint: AssetRef) -> Result<()> {
    crate::contracts::validate_version(replay.schema_version)?;
    if replay.steps.is_empty()
        || replay.steps.len() as u64 > crate::MAX_REPLAY_EVENTS
        || replay.packs.len() > crate::MAX_ASSETS
        || replay.steps.last().is_none_or(|step| step.checkpoint.asset != checkpoint)
    {
        return Err(Error::Invalid("project replay endpoint"));
    }
    let mut elapsed = 0;
    let mut events = BTreeSet::new();
    for (index, step) in replay.steps.iter().enumerate() {
        step.checkpoint.asset.validate()?;
        if step.revision != index as u64 + 1
            || step.elapsed_ms < elapsed
            || step.elapsed_ms > crate::MAX_REPLAY_MS
            || !events.insert(step.event)
            || (index == 0 && step.kind != "baseline")
            || !matches!(step.kind.as_str(), "baseline" | "resolved" | "checkpoint" | "undo" | "redo")
        {
            return Err(Error::Invalid("project replay step"));
        }
        crate::validate_label(&step.command, 256, false)?;
        elapsed = step.elapsed_ms;
    }
    Ok(())
}

impl GalleryBundle {
    pub fn validate(&self) -> Result<()> {
        let required = self.manifest.required_assets()?;
        if self.objects.len() != required.len() {
            return Err(Error::Invalid("gallery object closure"));
        }
        let mut total = 0u64;
        for asset in required {
            let bytes = self.objects.get(&asset.hash).ok_or(Error::Invalid("missing gallery object"))?;
            total = total.checked_add(asset.bytes).ok_or(Error::Limit("gallery bytes"))?;
            if total > MAX_GALLERY_BYTES {
                return Err(Error::Limit("gallery bytes"));
            }
            if bytes.len() as u64 != asset.bytes || format!("{:x}", Sha256::digest(bytes)) != asset.hash.as_str() {
                return Err(Error::Invalid("gallery object checksum"));
            }
        }
        match &self.manifest.content {
            GalleryContent::Project { replay: Some(replay), .. } => self.validate_replay_objects(replay)?,
            GalleryContent::Game { room } => {
                for entry in &room.reveal {
                    if let RevealedEntry::Artwork { assignment, revision, image, checkpoint, replay, assets } = entry
                        && let Some(manifest) = replay.manifest
                    {
                        let submission = crate::Submission {
                            id: crate::SubmissionId::new(assignment.project.as_str())?,
                            assignment: assignment.id,
                            author: assignment.player,
                            project: assignment.project,
                            revision: *revision,
                            image: *image,
                            checkpoint: *checkpoint,
                            replay: **replay,
                            assets: assets.clone(),
                            note: String::new(),
                            seam: None,
                        };
                        let replay = ReplayArtifact::decode(self.object(manifest)?, &submission)?;
                        self.validate_replay_objects(&replay)?;
                    }
                }
            }
            _ => (),
        }
        Ok(())
    }

    fn validate_replay_objects(&self, replay: &ReplayArtifact) -> Result<()> {
        let mut available: BTreeMap<AssetHash, u64> = self.objects.iter().map(|(hash, bytes)| (*hash, bytes.len() as u64)).collect();
        for pack in &replay.packs {
            for (asset, _) in crate::replay_artifact::unpack_checkpoints(self.object(*pack)?)? {
                if available.insert(asset.hash, asset.bytes).is_some_and(|length| length != asset.bytes) {
                    return Err(Error::Invalid("replay checkpoint length conflict"));
                }
            }
        }
        for step in &replay.steps {
            if available.get(&step.checkpoint.asset.hash) != Some(&step.checkpoint.asset.bytes) {
                return Err(Error::Invalid("missing replay checkpoint"));
            }
        }
        Ok(())
    }

    pub fn object(&self, asset: AssetRef) -> Result<&[u8]> {
        let bytes = self.objects.get(&asset.hash).ok_or(Error::Invalid("missing gallery object"))?;
        if bytes.len() as u64 != asset.bytes {
            return Err(Error::Invalid("gallery object length"));
        }
        Ok(bytes)
    }
}
