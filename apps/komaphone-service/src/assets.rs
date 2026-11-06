//! Bounded immutable chunks and canonical image verification.
use crate::{
    Error, Result,
    store::{digest, read_u64, sql_int},
};
use image::ImageEncoder;
use photocraft_party::protocol::{CHUNK_BYTES, RpcResponse};
use photocraft_party::room::Room;
use photocraft_party::{AssetHash, AssetRef, Error as DomainError, MAX_ASSET_BYTES, MAX_PROJECT_BYTES, Mode, ParticipantId, RoomId, Submission};
use rusqlite::{OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom, Write};

pub(crate) fn upload(tx: &Transaction<'_>, key: (RoomId, ParticipantId, AssetHash), payload: (u64, u64, &[u8], bool), now: u64) -> Result<RpcResponse> {
    let (room, actor, hash) = key;
    let (total, offset, bytes, finish) = payload;
    if total == 0 || total > MAX_ASSET_BYTES || bytes.is_empty() || bytes.len() > CHUNK_BYTES {
        return Err(DomainError::Limit("upload chunk").into());
    }
    let end = offset.checked_add(bytes.len() as u64).ok_or(DomainError::Limit("upload offset"))?;
    if end > total || (finish && end != total) {
        return Err(DomainError::Invalid("upload length").into());
    }
    let found: Option<(i64, u64, u64, bool, u64)> = tx
        .query_row(
            "SELECT id,total,received,complete,updated FROM objects WHERE room=?1 AND actor=?2 AND hash=?3",
            params![room.as_str(), actor.as_str(), hash.as_str()],
            |r| Ok((r.get(0)?, read_u64(r, 1)?, read_u64(r, 2)?, r.get(3)?, read_u64(r, 4)?)),
        )
        .optional()?;
    let (id, stored_total, received, complete, _updated) = match found {
        Some(value) => value,
        None => {
            if offset != 0 {
                return Err(DomainError::Conflict.into());
            }
            let (used, count): (u64, u64) =
                tx.query_row("SELECT coalesce(sum(total),0),count(*) FROM objects WHERE room=?1", [room.as_str()], |r| Ok((read_u64(r, 0)?, read_u64(r, 1)?)))?;
            if used.saturating_add(total) > MAX_PROJECT_BYTES || count >= 9216 {
                return Err(DomainError::Limit("room objects").into());
            }
            tx.execute(
                "INSERT INTO objects(room,actor,hash,total,received,complete,data,updated) VALUES(?1,?2,?3,?4,0,0,zeroblob(?4),?5)",
                params![room.as_str(), actor.as_str(), hash.as_str(), sql_int(total)?, sql_int(now)?],
            )?;
            (tx.last_insert_rowid(), total, 0, false, now)
        }
    };
    if stored_total != total {
        return Err(DomainError::Conflict.into());
    }
    if received > total {
        return Err(Error::Storage);
    }
    if offset > received {
        return Err(DomainError::Conflict.into());
    }
    let mut blob = tx.blob_open("main", "objects", "data", id, false)?;
    if blob.len() as u64 != total {
        return Err(Error::Storage);
    }
    blob.seek(SeekFrom::Start(offset))?;
    if offset < received || complete {
        if end > received {
            return Err(DomainError::Conflict.into());
        }
        let mut old = vec![0; bytes.len()];
        blob.read_exact(&mut old)?;
        if old != bytes {
            return Err(DomainError::Conflict.into());
        }
    } else {
        blob.write_all(bytes)?;
    }
    drop(blob);
    let next = received.max(end);
    let completed = complete || (finish && next == total);
    if completed && !complete {
        verify_hash(tx, id, hash)?;
    }
    tx.execute("UPDATE objects SET received=?1,complete=?2,updated=?3 WHERE id=?4", params![sql_int(next)?, completed, sql_int(now)?, id])?;
    Ok(RpcResponse::Uploaded { asset: completed.then_some(AssetRef { hash, bytes: total }), next_offset: next })
}
fn verify_hash(tx: &Transaction<'_>, id: i64, expected: AssetHash) -> Result<()> {
    let mut blob = tx.blob_open("main", "objects", "data", id, true)?;
    let mut buffer = [0u8; CHUNK_BYTES];
    let mut hash = Sha256::new();
    loop {
        let n = blob.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(buffer.get(..n).ok_or(Error::Invalid("chunk read"))?);
    }
    if format!("{:x}", hash.finalize()) != expected.as_str() {
        return Err(DomainError::Invalid("asset checksum").into());
    }
    Ok(())
}
fn object_id(tx: &Transaction<'_>, room: RoomId, hash: AssetHash) -> Result<(i64, u64)> {
    let (id, total, length): (i64, u64, u64) = tx
        .query_row("SELECT id,total,length(data) FROM objects WHERE room=?1 AND hash=?2 AND complete=1 LIMIT 1", params![room.as_str(), hash.as_str()], |r| {
            Ok((r.get(0)?, read_u64(r, 1)?, read_u64(r, 2)?))
        })
        .optional()?
        .ok_or(DomainError::Invalid("missing complete asset"))?;
    if total == 0 || total > MAX_ASSET_BYTES || total != length {
        return Err(Error::Storage);
    }
    Ok((id, total))
}
pub(crate) fn verify_store(tx: &Transaction<'_>) -> Result<()> {
    let mut query = tx.prepare("SELECT id,substr(hash,1,65),total,received,complete,length(data) FROM objects LIMIT 65537")?;
    let rows =
        query.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, read_u64(r, 2)?, read_u64(r, 3)?, r.get::<_, bool>(4)?, read_u64(r, 5)?)))?;
    for (index, row) in rows.enumerate() {
        if index >= 65536 {
            return Err(DomainError::Limit("stored objects").into());
        }
        let (id, hash, total, received, complete, length) = row?;
        if total == 0 || total > MAX_ASSET_BYTES || total != length || received > total || complete && received != total {
            return Err(Error::Storage);
        }
        let hash = AssetHash::new(&hash)?;
        if complete {
            verify_hash(tx, id, hash)?;
        }
    }
    Ok(())
}
pub(crate) fn fetch(tx: &Transaction<'_>, room: RoomId, hash: AssetHash, offset: u64) -> Result<RpcResponse> {
    let (id, total) = object_id(tx, room, hash)?;
    if offset >= total {
        return Err(DomainError::Invalid("fetch offset").into());
    }
    let mut blob = tx.blob_open("main", "objects", "data", id, true)?;
    blob.seek(SeekFrom::Start(offset))?;
    let size = usize::try_from((total - offset).min(CHUNK_BYTES as u64)).map_err(|_| Error::Invalid("fetch size"))?;
    let mut bytes = vec![0; size];
    blob.read_exact(&mut bytes)?;
    Ok(RpcResponse::AssetChunk { bytes, total })
}
fn load(tx: &Transaction<'_>, room: RoomId, asset: AssetRef) -> Result<Vec<u8>> {
    asset.validate()?;
    let (id, total) = object_id(tx, room, asset.hash)?;
    if total != asset.bytes {
        return Err(DomainError::Invalid("asset length").into());
    }
    let mut blob = tx.blob_open("main", "objects", "data", id, true)?;
    // Length came from a verified bounded upload, not an unchecked remote allocation.
    let mut bytes = vec![0; usize::try_from(total).map_err(|_| Error::Invalid("object length"))?];
    blob.read_exact(&mut bytes)?;
    if digest(&bytes) != asset.hash.as_str() {
        return Err(DomainError::Invalid("stored asset checksum").into());
    }
    Ok(bytes)
}
fn decode_checkpoint(bytes: &[u8]) -> Result<photocraft_doc::Document> {
    let options = photocraft_format::LoadOptions {
        // Match the local draft store's verified reopen budget, so a locally
        // acknowledged native checkpoint remains admissible at submission.
        max_manifest_bytes: 8 * 1024 * 1024,
        max_blob_bytes: MAX_ASSET_BYTES as usize,
        max_total_bytes: MAX_PROJECT_BYTES,
        max_canvas_side: photocraft_party::MAX_IMAGE_SIDE,
        max_canvas_pixels: photocraft_party::MAX_IMAGE_PIXELS,
        preserve_ids: true,
    };
    photocraft_format::load_from_bytes_with(bytes, &options).map_err(|_| DomainError::Invalid("native checkpoint; preserve the local original").into())
}
/// Verify every referenced object plus authoritative CPU-rendered final pixels and derived seam.
/// There is no remotely callable derivative-registration or grant-by-hash operation.
pub(crate) fn validate_submission(tx: &Transaction<'_>, room: &Room, submission: &Submission) -> Result<()> {
    submission.validate()?;
    for asset in &submission.assets {
        let (_, length) = object_id(tx, room.id(), asset.hash)?;
        if length != asset.bytes {
            return Err(DomainError::Invalid("asset length").into());
        }
        let owns: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM objects WHERE room=?1 AND actor=?2 AND hash=?3 AND complete=1)",
            params![room.id().as_str(), submission.author.as_str(), asset.hash.as_str()],
            |r| r.get(0),
        )?;
        if !owns && !room.can_fetch(submission.author, asset.hash) {
            return Err(DomainError::Unauthorized.into());
        }
    }
    let checkpoint = load(tx, room.id(), submission.checkpoint.asset)?;
    let doc = decode_checkpoint(&checkpoint)?;
    let mode = room.view(submission.author)?.mode;
    let (width, height) = match mode {
        Mode::Animation { .. } => (1536, 1024),
        Mode::ExquisiteCorpse => (1024, 512),
    };
    if doc.size.width != width || doc.size.height != height {
        return Err(DomainError::Invalid("party checkpoint dimensions").into());
    }
    if submission.replay.baseline.asset != submission.checkpoint.asset {
        let baseline = load(tx, room.id(), submission.replay.baseline.asset)?;
        let baseline = decode_checkpoint(&baseline)?;
        if baseline.size.width != width || baseline.size.height != height {
            return Err(DomainError::Invalid("replay baseline dimensions").into());
        }
    }
    validate_replay(tx, room, submission, (width, height))?;
    let mut rendered = photocraft_compose::flatten(&doc);
    photocraft_party::canonical_color::to_srgb(&doc, &mut rendered.px)
        .map_err(|_| DomainError::Invalid("checkpoint color profile; preserve the local original"))?;
    let rendered = rendered.to_rgba8();
    let uploaded = load(tx, room.id(), submission.image)?;
    // Comparing our own canonical encoding proves exact pixels and excludes metadata/trailing
    // payloads, without invoking a second decoder on attacker-controlled PNG metadata.
    if encode_png(width, height, &rendered.pixels)? != uploaded {
        return Err(DomainError::Invalid("image must use canonical metadata-free PNG encoding").into());
    }
    if let Some(seam) = submission.seam {
        let seam_bytes = load(tx, room.id(), seam)?;
        let start = usize::try_from(u64::from(height.saturating_sub(24)) * u64::from(width) * 4).map_err(|_| Error::Invalid("seam offset"))?;
        let expected = rendered.pixels.get(start..).ok_or(Error::Invalid("seam bounds"))?;
        if encode_png(width, 24, expected)? != seam_bytes {
            return Err(DomainError::Invalid("seam must use canonical metadata-free PNG encoding").into());
        }
    }
    Ok(())
}
fn validate_replay(tx: &Transaction<'_>, room: &Room, submission: &Submission, dimensions: (u32, u32)) -> Result<()> {
    let Some(manifest) = submission.replay.manifest else {
        if submission.replay.event_count != 0 || submission.revision != 1 || submission.replay.baseline != submission.checkpoint {
            return Err(DomainError::Invalid("complete replay package is required").into());
        }
        return Ok(());
    };
    let bytes = load(tx, room.id(), manifest)?;
    let replay = photocraft_party::replay_artifact::ReplayArtifact::decode(&bytes, submission)?;
    let mut checked = std::collections::BTreeSet::new();
    let mut wanted = std::collections::BTreeMap::new();
    for checkpoint in replay.checkpoints() {
        if let Some(previous) = wanted.insert(checkpoint.hash, checkpoint.bytes)
            && previous != checkpoint.bytes
        {
            return Err(DomainError::Invalid("replay checkpoint length conflict").into());
        }
    }
    let mut packs = std::collections::BTreeSet::new();
    for pack in &replay.packs {
        if !packs.insert(pack.hash) {
            return Err(DomainError::Invalid("duplicate replay pack").into());
        }
        let bytes = load(tx, room.id(), *pack)?;
        for (checkpoint, bytes) in photocraft_party::replay_artifact::unpack_checkpoints(&bytes)? {
            if wanted.get(&checkpoint.hash) != Some(&checkpoint.bytes) || !checked.insert(checkpoint.hash) {
                return Err(DomainError::Invalid("duplicate or unrelated packed checkpoint").into());
            }
            validate_checkpoint_dimensions(bytes, dimensions)?;
        }
    }
    for checkpoint in replay.checkpoints() {
        if !checked.insert(checkpoint.hash) {
            continue;
        }
        if !submission.assets.contains(&checkpoint) {
            return Err(DomainError::Invalid("missing packed checkpoint").into());
        }
        let bytes = load(tx, room.id(), checkpoint)?;
        validate_checkpoint_dimensions(&bytes, dimensions)?;
    }
    Ok(())
}
fn validate_checkpoint_dimensions(bytes: &[u8], dimensions: (u32, u32)) -> Result<()> {
    let document = decode_checkpoint(bytes)?;
    if (document.size.width, document.size.height) != dimensions {
        return Err(DomainError::Invalid("replay checkpoint dimensions").into());
    }
    Ok(())
}

/// Shared deterministic PNG encoding for service fixtures and native submission preparation.
pub fn encode_png(width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>> {
    photocraft_party::ImageExtent { width, height }.decoded_bytes(4)?;
    if pixels.len() as u64 != u64::from(width) * u64::from(height) * 4 {
        return Err(Error::Invalid("PNG pixel length"));
    }
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(pixels, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|_| Error::Invalid("PNG encoding"))?;
    Ok(bytes)
}
