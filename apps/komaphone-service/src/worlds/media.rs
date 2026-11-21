//! Authorized immutable derivatives. A digest identifies bytes, never a read grant.
use std::collections::BTreeMap;
use std::io::Cursor;

use image::{ImageDecoder, ImageReader};
use photocraft_party::protocol::RpcResponse;
use photocraft_party::room::{RevealedEntry, RoomView};
use photocraft_party::{AssetRef, GameId, Mode, ParticipantId, RoomId};
use photocraft_scene::shared::media::{AssetHash, Credit, MAX_FRAME_BYTES, MAX_MEDIA_BYTES, MediaFrame, MediaPlacement, MediaSource, Provenance};
use photocraft_scene::shared::{DeviceId, Edit, MediaId, SourceId, WorldOp};
use photocraft_scene::world_protocol::{GameMediaSource, PreparedMedia, Response};
use photocraft_scene::{CanvasId, OwnerId};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::store::{digest, read_u64, sql_int};
use crate::{Error, Result};

const MAX_SOURCES: u64 = 4096;
const MAX_SOURCE_BYTES: usize = 128 * 1024;
const MAX_CANVAS_BYTES: u64 = 512 * 1024 * 1024;
const MAX_CANVAS_BLOBS: u64 = 4096;
pub(super) const TABLES: &str = "CREATE TABLE world_media_sources(source_key TEXT PRIMARY KEY, artifact TEXT UNIQUE NOT NULL, body BLOB NOT NULL, hash TEXT NOT NULL);
    CREATE TABLE world_blobs(canvas TEXT NOT NULL REFERENCES worlds(id), hash TEXT NOT NULL, total INTEGER NOT NULL, data BLOB NOT NULL, PRIMARY KEY(canvas,hash));";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSource {
    source: GameMediaSource,
    prepared: PreparedMedia,
}

fn invalid() -> Error {
    Error::Invalid("verified media source")
}
fn scene(error: impl std::fmt::Display) -> Error {
    let _ = error;
    invalid()
}
fn participant(profile: OwnerId) -> Result<ParticipantId> {
    Ok(ParticipantId::new(profile.as_str())?)
}
fn ids(source: &GameMediaSource) -> Result<(RoomId, GameId)> {
    Ok((RoomId::new(source.room.as_str())?, GameId::new(source.game.as_str())?))
}
fn game(tx: &Transaction<'_>, profile: OwnerId, source: &GameMediaSource) -> Result<RoomView> {
    let (room, game) = ids(source)?;
    match crate::archive::game(tx, room, game, participant(profile)?)? {
        RpcResponse::Game { view } => Ok(*view),
        _ => Err(Error::Storage),
    }
}
fn source_key(source: &GameMediaSource) -> Result<String> {
    Ok(digest(&serde_json::to_vec(source)?))
}
fn read_source(db: &Connection, column: &str, key: &str) -> Result<Option<StoredSource>> {
    let sql = match column {
        "source_key" => "SELECT substr(CAST(body AS BLOB),1,131073),CAST(substr(CAST(hash AS BLOB),1,65) AS TEXT) FROM world_media_sources WHERE source_key=?1",
        "artifact" => "SELECT substr(CAST(body AS BLOB),1,131073),CAST(substr(CAST(hash AS BLOB),1,65) AS TEXT) FROM world_media_sources WHERE artifact=?1",
        _ => return Err(Error::Storage),
    };
    let value: Option<(Vec<u8>, String)> = db.query_row(sql, [key], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    value
        .map(|(body, hash)| {
            if body.len() > MAX_SOURCE_BYTES || digest(&body) != hash {
                return Err(Error::Storage);
            }
            let source: StoredSource = serde_json::from_slice(&body)?;
            validate_prepared(&source.prepared)?;
            validate_source(&source)?;
            Ok(source)
        })
        .transpose()
}
fn validate_source(stored: &StoredSource) -> Result<()> {
    let source = &stored.source;
    let prepared = &stored.prepared;
    let provenance = &prepared.provenance;
    let expected_source =
        MediaSource::Gallery { entry: source.room, room: Some(source.room), game: Some(source.game), chain: source.chain, frame: source.frame };
    let artifact = digest(&serde_json::to_vec(&(source, provenance.source_revision, &prepared.frames, prepared.looping, &provenance.authors))?);
    if !(1..=24).contains(&source.fps)
        || source.chain >= 3
        || source.frame.is_some_and(|n| n >= 12)
        || provenance.source != expected_source
        || provenance.artifact_hash.as_str() != artifact
        || (source.frame.is_some() && (prepared.frames.len() != 1 || prepared.looping))
    {
        return Err(Error::Storage);
    }
    Ok(())
}
fn validate_prepared(prepared: &PreparedMedia) -> Result<()> {
    // Reuse the pure contract's entire descriptor validation with an innocuous local placement.
    let id = "00000000000000000000000000000001";
    MediaPlacement {
        id: MediaId::new(id).map_err(scene)?,
        canvas: CanvasId::new(id).map_err(scene)?,
        owner: OwnerId::new(id).map_err(scene)?,
        anchor: Default::default(),
        offset: [0., 0.],
        width: 1.,
        height: 1.,
        frames: prepared.frames.clone(),
        looping: prepared.looping,
        provenance: prepared.provenance.clone(),
    }
    .validate()
    .map_err(scene)
}

pub(super) fn prepare(tx: &Transaction<'_>, profile: OwnerId, canvas: CanvasId, source: &GameMediaSource) -> Result<Response> {
    super::grant(tx, canvas, profile)?;
    let view = game(tx, profile, source)?;
    let key = source_key(source)?;
    if let Some(stored) = read_source(tx, "source_key", &key)? {
        if stored.source != *source {
            return Err(Error::Storage);
        }
        return Ok(Response::MediaPrepared { media: Box::new(stored.prepared) });
    }
    let count: u64 = tx.query_row("SELECT count(*) FROM world_media_sources", [], |r| read_u64(r, 0))?;
    if count >= MAX_SOURCES {
        return Err(photocraft_party::Error::Limit("prepared media sources").into());
    }
    let (prepared, _) = build(tx, profile, &view, source, view.revision)?;
    let stored = StoredSource { source: source.clone(), prepared: prepared.clone() };
    let body = serde_json::to_vec(&stored)?;
    if body.len() > MAX_SOURCE_BYTES {
        return Err(invalid());
    }
    tx.execute("INSERT INTO world_media_sources VALUES(?1,?2,?3,?4)", params![key, prepared.provenance.artifact_hash.as_str(), body, digest(&body)])?;
    Ok(Response::MediaPrepared { media: Box::new(prepared) })
}

fn placement<'a>(tx: &Transaction<'_>, profile: OwnerId, device: DeviceId, operation: &'a WorldOp) -> Result<&'a MediaPlacement> {
    if super::grant(tx, operation.canvas, profile)? != operation.authority_epoch || operation.author != profile || operation.device != device {
        return super::denied();
    }
    let Edit::PlaceMedia { media } = &operation.action else {
        return Err(invalid());
    };
    media.validate().map_err(scene)?;
    if media.canvas != operation.canvas || media.owner != profile {
        return super::denied();
    }
    Ok(media)
}
fn matching(media: &MediaPlacement, prepared: &PreparedMedia) -> Result<()> {
    if media.frames != prepared.frames || media.looping != prepared.looping || media.provenance != prepared.provenance {
        return Err(invalid());
    }
    Ok(())
}
pub(super) fn remix_game(tx: &Transaction<'_>, profile: OwnerId, device: DeviceId, operation: &WorldOp, room: SourceId) -> Result<Response> {
    let media = placement(tx, profile, device, operation)?;
    let stored = read_source(tx, "artifact", media.provenance.artifact_hash.as_str())?.ok_or_else(invalid)?;
    if stored.source.room != room {
        return super::denied();
    }
    let view = game(tx, profile, &stored.source)?; // Recheck frozen source roster even for duplicate receipts.
    matching(media, &stored.prepared)?;
    if let Some(response) = super::prior_receipt(tx, operation)? {
        return Ok(response);
    }
    let (verified, blobs) = build(tx, profile, &view, &stored.source, stored.prepared.provenance.source_revision)?;
    if verified != stored.prepared {
        return Err(Error::Storage);
    }
    store_blobs(tx, operation.canvas, &blobs)?;
    super::append_authorized(tx, operation)
}
pub(super) fn remix_canvas(
    tx: &Transaction<'_>,
    profile: OwnerId,
    device: DeviceId,
    operation: &WorldOp,
    source_canvas: CanvasId,
    source_media: MediaId,
) -> Result<Response> {
    let media = placement(tx, profile, device, operation)?;
    super::grant(tx, source_canvas, profile)?;
    let world = super::storage_v6::reconstruct_world(tx, source_canvas, false)?;
    let source = world.media.get(&source_media).ok_or_else(invalid)?;
    // The immutable body remains a usable copy source after personal hide/undo. Hiding never deletes accepted art.
    let prepared = PreparedMedia {
        frames: source.media.frames.clone(),
        looping: source.media.looping,
        provenance: Provenance {
            source: MediaSource::Canvas { canvas: source_canvas, media: source_media },
            source_revision: source.created_revision,
            artifact_hash: source.media.provenance.artifact_hash,
            authors: source.media.provenance.authors.clone(),
        },
    };
    matching(media, &prepared)?;
    if let Some(response) = super::prior_receipt(tx, operation)? {
        return Ok(response);
    }
    let mut blobs = BTreeMap::new();
    for frame in &prepared.frames {
        if let std::collections::btree_map::Entry::Vacant(entry) = blobs.entry(frame.hash) {
            entry.insert(read_blob(tx, source_canvas, frame.hash, Some(frame.byte_len))?);
        }
    }
    store_blobs(tx, operation.canvas, &blobs)?;
    super::append_authorized(tx, operation)
}

fn load_png(tx: &Transaction<'_>, profile: OwnerId, view: &RoomView, source: &GameMediaSource, asset: AssetRef, dimensions: (u32, u32)) -> Result<Vec<u8>> {
    if asset.bytes == 0 || asset.bytes > MAX_FRAME_BYTES {
        return Err(invalid());
    }
    let (room, game) = ids(source)?;
    let mut bytes = Vec::new();
    while (bytes.len() as u64) < asset.bytes {
        let RpcResponse::AssetChunk { bytes: chunk, total } =
            crate::archive::fetch(tx, room, game, participant(profile)?, view.revision, asset.hash, bytes.len() as u64)?
        else {
            return Err(Error::Storage);
        };
        if total != asset.bytes || chunk.is_empty() || (bytes.len() as u64).saturating_add(chunk.len() as u64) > total {
            return Err(Error::Storage);
        }
        bytes.extend_from_slice(&chunk);
    }
    if digest(&bytes) != asset.hash.as_str() {
        return Err(Error::Storage);
    }
    let pixels = decode(&bytes, dimensions)?;
    if crate::assets::encode_png(dimensions.0, dimensions.1, &pixels)? != bytes {
        return Err(Error::Storage);
    }
    Ok(bytes)
}
pub(super) fn decode(bytes: &[u8], dimensions: (u32, u32)) -> Result<Vec<u8>> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(dimensions.0);
    limits.max_image_height = Some(dimensions.1);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(|_| invalid())?;
    if decoder.dimensions() != dimensions || decoder.color_type() != image::ColorType::Rgba8 {
        return Err(invalid());
    }
    Ok(image::DynamicImage::from_decoder(decoder).map_err(|_| invalid())?.into_rgba8().into_raw())
}
fn build(
    tx: &Transaction<'_>,
    profile: OwnerId,
    view: &RoomView,
    source: &GameMediaSource,
    revision: u64,
) -> Result<(PreparedMedia, BTreeMap<AssetHash, Vec<u8>>)> {
    if !(1..=24).contains(&source.fps) || usize::from(source.chain) >= view.members.len() || revision == 0 || revision > view.revision {
        return Err(invalid());
    }
    let rounds = view.mode.rounds(view.members.len())?;
    if source.frame.is_some_and(|n| n >= u16::from(rounds)) {
        return Err(invalid());
    }
    let dimensions = match view.mode {
        Mode::Animation { .. } => (1536, 1024),
        Mode::ExquisiteCorpse => (1024, 512),
    };
    let mut images = Vec::new();
    let mut used = 0_u64;
    for round in 0..rounds {
        if source.frame.is_some_and(|n| n != u16::from(round)) {
            continue;
        }
        let entry = view
            .reveal
            .iter()
            .find(|entry| match entry {
                RevealedEntry::Artwork { assignment, .. } | RevealedEntry::Missing { assignment, .. } => {
                    assignment.id.chain == source.chain && assignment.id.round == round
                }
            })
            .ok_or(Error::Storage)?;
        let png = match entry {
            RevealedEntry::Artwork { image, .. } => load_png(tx, profile, view, source, *image, dimensions)?,
            RevealedEntry::Missing { .. } if source.frame.is_none() => {
                crate::assets::encode_png(dimensions.0, dimensions.1, &vec![0; dimensions.0 as usize * dimensions.1 as usize * 4])?
            }
            RevealedEntry::Missing { .. } => return Err(Error::Invalid("the selected source frame is missing")),
        };
        used = used.saturating_add(png.len() as u64);
        if used > MAX_MEDIA_BYTES {
            return Err(invalid());
        }
        images.push(png);
    }
    let whole_creature = view.mode == Mode::ExquisiteCorpse && source.frame.is_none();
    let output_dimensions = if whole_creature { (1024, 1536) } else { dimensions };
    if whole_creature {
        if images.len() != 3 {
            return Err(Error::Storage);
        }
        let mut pixels = Vec::with_capacity(1024 * 1536 * 4);
        // Three full native bands, not the 24-pixel seam derivatives.
        for bytes in images {
            pixels.extend_from_slice(&decode(&bytes, dimensions)?);
        }
        images = vec![crate::assets::encode_png(1024, 1536, &pixels)?];
    }
    let animated = matches!(view.mode, Mode::Animation { .. }) && source.frame.is_none();
    let mut frames = Vec::new();
    let mut blobs = BTreeMap::new();
    for (index, bytes) in images.into_iter().enumerate() {
        let hash = AssetHash::new(&digest(&bytes)).map_err(scene)?;
        let duration_ms = if animated { ((index as u32 + 1) * 1000 / u32::from(source.fps)) - (index as u32 * 1000 / u32::from(source.fps)) } else { 1000 };
        frames.push(MediaFrame { hash, byte_len: bytes.len() as u64, width: output_dimensions.0, height: output_dimensions.1, duration_ms });
        blobs.insert(hash, bytes);
    }
    let authors = view
        .members
        .iter()
        .map(|member| Ok(Credit { profile: OwnerId::new(member.participant.id.as_str()).map_err(scene)?, label: member.participant.display_name.clone() }))
        .collect::<Result<Vec<_>>>()?;
    let looping = animated && source.looping;
    let artifact_hash = AssetHash::new(&digest(&serde_json::to_vec(&(source, revision, &frames, looping, &authors))?)).map_err(scene)?;
    let prepared = PreparedMedia {
        frames,
        looping,
        provenance: Provenance {
            source: MediaSource::Gallery { entry: source.room, room: Some(source.room), game: Some(source.game), chain: source.chain, frame: source.frame },
            source_revision: revision,
            artifact_hash,
            authors,
        },
    };
    validate_prepared(&prepared)?;
    Ok((prepared, blobs))
}

pub(super) fn read_blob(db: &Connection, canvas: CanvasId, hash: AssetHash, expected: Option<u64>) -> Result<Vec<u8>> {
    let (body, total, length): (Vec<u8>, u64, u64) = db
        .query_row(
            "SELECT substr(CAST(data AS BLOB),1,8388609),total,length(CAST(data AS BLOB)) FROM world_blobs WHERE canvas=?1 AND hash=?2",
            params![canvas.as_str(), hash.as_str()],
            |r| Ok((r.get(0)?, read_u64(r, 1)?, read_u64(r, 2)?)),
        )
        .optional()?
        .ok_or(Error::Storage)?;
    if total == 0
        || total > MAX_FRAME_BYTES
        || total != length
        || total != body.len() as u64
        || expected.is_some_and(|n| n != total)
        || digest(&body) != hash.as_str()
    {
        return Err(Error::Storage);
    }
    Ok(body)
}
pub(super) fn store_blobs(tx: &Transaction<'_>, canvas: CanvasId, blobs: &BTreeMap<AssetHash, Vec<u8>>) -> Result<()> {
    let (mut count, mut used): (u64, u64) =
        tx.query_row("SELECT count(*),coalesce(sum(total),0) FROM world_blobs WHERE canvas=?1", [canvas.as_str()], |r| Ok((read_u64(r, 0)?, read_u64(r, 1)?)))?;
    for (hash, bytes) in blobs {
        let exists: bool =
            tx.query_row("SELECT EXISTS(SELECT 1 FROM world_blobs WHERE canvas=?1 AND hash=?2)", params![canvas.as_str(), hash.as_str()], |r| r.get(0))?;
        if exists {
            if read_blob(tx, canvas, *hash, Some(bytes.len() as u64))? != *bytes {
                return Err(Error::Storage);
            }
            continue;
        }
        count = count.saturating_add(1);
        used = used.saturating_add(bytes.len() as u64);
        if count > MAX_CANVAS_BLOBS || used > MAX_CANVAS_BYTES || bytes.len() as u64 > MAX_FRAME_BYTES || digest(bytes) != hash.as_str() {
            return Err(photocraft_party::Error::Limit("canvas media storage").into());
        }
        tx.execute("INSERT INTO world_blobs VALUES(?1,?2,?3,?4)", params![canvas.as_str(), hash.as_str(), sql_int(bytes.len() as u64)?, bytes])?;
    }
    Ok(())
}
pub(super) fn fetch(db: &Connection, profile: OwnerId, canvas: CanvasId, hash: AssetHash, offset: u64) -> Result<Response> {
    super::grant(db, canvas, profile)?;
    let total: u64 = db
        .query_row("SELECT total FROM world_blobs WHERE canvas=?1 AND hash=?2", params![canvas.as_str(), hash.as_str()], |r| read_u64(r, 0))
        .optional()?
        .ok_or(photocraft_party::Error::Unauthorized)?;
    if total == 0 || total > MAX_FRAME_BYTES || offset >= total {
        return Err(invalid());
    }
    let size = (total - offset).min(photocraft_party::protocol::CHUNK_BYTES as u64);
    let bytes: Vec<u8> = db.query_row(
        "SELECT substr(CAST(data AS BLOB),?3,?4) FROM world_blobs WHERE canvas=?1 AND hash=?2",
        params![canvas.as_str(), hash.as_str(), sql_int(offset + 1)?, sql_int(size)?],
        |r| r.get(0),
    )?;
    if bytes.len() as u64 != size {
        return Err(Error::Storage);
    }
    Ok(Response::MediaChunk { bytes, total })
}

pub(super) fn verify(db: &Connection) -> Result<()> {
    let orphans: u64 = db.query_row("SELECT count(*) FROM world_blobs b LEFT JOIN worlds w ON w.id=b.canvas WHERE w.id IS NULL", [], |r| read_u64(r, 0))?;
    if orphans != 0 {
        return Err(Error::Storage);
    }
    let mut query = db.prepare(
        "SELECT CAST(substr(CAST(source_key AS BLOB),1,65) AS TEXT),CAST(substr(CAST(artifact AS BLOB),1,65) AS TEXT) FROM world_media_sources LIMIT 4097",
    )?;
    let rows = query.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    for (index, row) in rows.enumerate() {
        if index as u64 >= MAX_SOURCES {
            return Err(Error::Storage);
        }
        let (key, artifact) = row?;
        let source = read_source(db, "source_key", &key)?.ok_or(Error::Storage)?;
        if source_key(&source.source)? != key || source.prepared.provenance.artifact_hash.as_str() != artifact {
            return Err(Error::Storage);
        }
    }
    let mut query = db.prepare(
        "SELECT CAST(substr(CAST(canvas AS BLOB),1,33) AS TEXT),CAST(substr(CAST(hash AS BLOB),1,65) AS TEXT) FROM world_blobs ORDER BY canvas,hash",
    )?;
    let mut rows = query.query([])?;
    let mut current = None;
    let mut count = 0_u64;
    let mut used = 0_u64;
    while let Some(row) = rows.next()? {
        let canvas = CanvasId::new(&row.get::<_, String>(0)?).map_err(scene)?;
        let hash = AssetHash::new(&row.get::<_, String>(1)?).map_err(scene)?;
        if current != Some(canvas) {
            current = Some(canvas);
            count = 0;
            used = 0;
        }
        count = count.saturating_add(1);
        used = used.saturating_add(read_blob(db, canvas, hash, None)?.len() as u64);
        if count > MAX_CANVAS_BLOBS || used > MAX_CANVAS_BYTES {
            return Err(Error::Storage);
        }
    }
    Ok(())
}
pub(super) fn verify_world(db: &Connection, world: &photocraft_scene::shared::WorldState) -> Result<()> {
    let mut verified = BTreeMap::new();
    for media in world.media.values() {
        for frame in &media.media.frames {
            if let Some((bytes, width, height)) = verified.get(&frame.hash) {
                if (*bytes, *width, *height) != (frame.byte_len, frame.width, frame.height) {
                    return Err(Error::Storage);
                }
                continue;
            }
            let bytes = read_blob(db, world.canvas, frame.hash, Some(frame.byte_len))?;
            decode(&bytes, (frame.width, frame.height))?;
            verified.insert(frame.hash, (frame.byte_len, frame.width, frame.height));
        }
    }
    Ok(())
}
