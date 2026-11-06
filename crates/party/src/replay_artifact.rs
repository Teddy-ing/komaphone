//! Portable exact-result replay package for accepted party drawings. Playback is K4.
use crate::{AssetRef, CheckpointRef, Error, EventId, MAX_ASSET_BYTES, ProjectId, ReplayId, Result, Submission};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayArtifact {
    pub schema_version: u16,
    pub replay: ReplayId,
    pub project: ProjectId,
    /// Bounded containers for intermediate checkpoints; final/baseline stay directly fetchable.
    #[serde(default)]
    pub packs: Vec<AssetRef>,
    /// Includes the first locally durable baseline, followed by every retained edit.
    pub steps: Vec<ReplayArtifactStep>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayArtifactStep {
    pub event: EventId,
    pub revision: u64,
    pub elapsed_ms: u64,
    pub command: String,
    pub params: serde_json::Value,
    pub kind: String,
    pub checkpoint: CheckpointRef,
}
impl ReplayArtifact {
    pub fn decode(bytes: &[u8], submission: &Submission) -> Result<Self> {
        if bytes.len() as u64 > MAX_ASSET_BYTES {
            return Err(Error::Limit("replay package"));
        }
        let artifact: Self = serde_json::from_slice(bytes).map_err(|_| Error::Invalid("replay package JSON"))?;
        artifact.validate(submission)?;
        Ok(artifact)
    }
    pub fn validate(&self, submission: &Submission) -> Result<()> {
        submission.validate()?;
        crate::contracts::validate_version(self.schema_version)?;
        let first = self.steps.first().ok_or(Error::Invalid("replay baseline"))?;
        let last = self.steps.last().ok_or(Error::Invalid("replay endpoint"))?;
        if self.project != submission.project
            || self.replay != submission.replay.id
            || self.steps.len() as u64 != submission.revision
            || self.steps.len().saturating_sub(1) as u64 != submission.replay.event_count
            || first.checkpoint != submission.replay.baseline
            || last.checkpoint != submission.checkpoint
            || last.elapsed_ms != submission.replay.duration_ms
            || submission.replay.end_event != (self.steps.len() > 1).then_some(last.event)
        {
            return Err(Error::Invalid("replay package endpoint"));
        }
        let mut elapsed = 0;
        if self.packs.len() > crate::MAX_ASSETS
            || self.packs.iter().map(|pack| pack.hash).collect::<std::collections::BTreeSet<_>>().len() != self.packs.len()
            || self.packs.iter().any(|pack| !submission.assets.contains(pack))
        {
            return Err(Error::Invalid("replay pack dependency"));
        }
        let mut events = std::collections::BTreeSet::new();
        for (index, step) in self.steps.iter().enumerate() {
            if step.revision != index as u64 + 1
                || step.elapsed_ms < elapsed
                || step.elapsed_ms > crate::MAX_REPLAY_MS
                || !events.insert(step.event)
                || !matches!(step.kind.as_str(), "baseline" | "resolved" | "checkpoint" | "undo" | "redo")
                || (index == 0 && step.kind != "baseline")
                || step.command.is_empty()
                || step.command.len() > 256
                || (self.packs.is_empty() && !submission.assets.contains(&step.checkpoint.asset))
            {
                return Err(Error::Invalid("replay package step"));
            }
            step.checkpoint.asset.validate()?;
            elapsed = step.elapsed_ms;
        }
        Ok(())
    }
    pub fn checkpoints(&self) -> impl Iterator<Item = AssetRef> + '_ {
        self.steps.iter().map(|step| step.checkpoint.asset)
    }
}

/// A pack is a simple bounded framing of already compressed native checkpoints, not a
/// filesystem archive. There are no paths, compression ratios, or external references.
pub fn pack_checkpoints(entries: &[(AssetRef, Vec<u8>)]) -> Result<Vec<u8>> {
    use sha2::{Digest, Sha256};
    if entries.is_empty() || entries.len() as u64 > crate::MAX_REPLAY_EVENTS {
        return Err(Error::Limit("replay pack entries"));
    }
    let mut seen = std::collections::BTreeSet::new();
    let total = entries.iter().try_fold(10usize, |size, (asset, bytes)| {
        asset.validate()?;
        if !seen.insert(asset.hash) {
            return Err(Error::Invalid("duplicate replay checkpoint"));
        }
        if bytes.len() as u64 != asset.bytes || format!("{:x}", Sha256::digest(bytes)) != asset.hash.as_str() {
            return Err(Error::Invalid("replay checkpoint checksum"));
        }
        size.checked_add(72).and_then(|n| n.checked_add(bytes.len())).filter(|n| *n as u64 <= MAX_ASSET_BYTES).ok_or(Error::Limit("replay pack bytes"))
    })?;
    let mut bytes = Vec::with_capacity(total);
    bytes.extend_from_slice(b"KPRP1\0");
    bytes.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (asset, data) in entries {
        bytes.extend_from_slice(asset.hash.as_str().as_bytes());
        bytes.extend_from_slice(&asset.bytes.to_le_bytes());
        bytes.extend_from_slice(data);
    }
    Ok(bytes)
}

pub fn unpack_checkpoints(bytes: &[u8]) -> Result<Vec<(AssetRef, &[u8])>> {
    use sha2::{Digest, Sha256};
    if bytes.len() as u64 > MAX_ASSET_BYTES || bytes.get(..6) != Some(b"KPRP1\0") {
        return Err(Error::Invalid("replay pack header"));
    }
    let count = u32::from_le_bytes(bytes.get(6..10).ok_or(Error::Invalid("replay pack count"))?.try_into().map_err(|_| Error::Invalid("replay pack count"))?);
    if count == 0 || u64::from(count) > crate::MAX_REPLAY_EVENTS || count as usize > bytes.len() / 73 {
        return Err(Error::Limit("replay pack count"));
    }
    let mut cursor = 10usize;
    let mut result = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..count {
        let end = cursor.checked_add(64).ok_or(Error::Limit("replay pack offset"))?;
        let hash = crate::AssetHash::new(
            std::str::from_utf8(bytes.get(cursor..end).ok_or(Error::Invalid("replay pack hash"))?).map_err(|_| Error::Invalid("replay pack hash"))?,
        )?;
        cursor = end;
        let end = cursor.checked_add(8).ok_or(Error::Limit("replay pack offset"))?;
        let len = u64::from_le_bytes(
            bytes.get(cursor..end).ok_or(Error::Invalid("replay pack length"))?.try_into().map_err(|_| Error::Invalid("replay pack length"))?,
        );
        cursor = end;
        let asset = AssetRef { hash, bytes: len };
        asset.validate()?;
        let end = cursor.checked_add(usize::try_from(len).map_err(|_| Error::Limit("replay pack length"))?).ok_or(Error::Limit("replay pack offset"))?;
        let data = bytes.get(cursor..end).ok_or(Error::Invalid("replay pack body"))?;
        if !seen.insert(hash) || format!("{:x}", Sha256::digest(data)) != hash.as_str() {
            return Err(Error::Invalid("replay checkpoint checksum"));
        }
        result.push((asset, data));
        cursor = end;
    }
    if cursor != bytes.len() {
        return Err(Error::Invalid("replay pack trailing bytes"));
    }
    Ok(result)
}
