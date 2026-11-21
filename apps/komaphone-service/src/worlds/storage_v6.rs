//! Incremental world storage (`world_schema = 6`): O(1) indexed commits, bounded
//! per-event `zlib-rs` compression, and immutable pinned derived checkpoints.
use std::collections::BTreeMap;
use std::io::{Read, Write};

use photocraft_party::Error as Domain;
use photocraft_scene::shared::media::AssetHash;
use photocraft_scene::shared::{
    AcceptedOp, DeviceId, Edit, Limits as ReducerLimits, MAX_EVENT_PAGE, SHARED_SCHEMA, SHARED_SCHEMA_BRUSH, WorldCheckpoint, WorldOp, WorldState,
    chain_hash_init, chain_hash_step,
};
use photocraft_scene::world_protocol::{self as wire, CheckpointRef, Response};
use photocraft_scene::{CanvasId, OwnerId};
use rusqlite::{Connection, OptionalExtension, params};

use super::{MAX_EVENT_BYTES, archive_import, grant, media, member_epoch, prior_receipt, read_event, scene_error};
use crate::store::{digest, read_u64, sql_int};
use crate::{Error, Result};

/// Checkpoint publication stride in revisions. Each checkpoint replays at most
/// `CHECKPOINT_STRIDE` events beyond the prior retained checkpoint.
pub(crate) const CHECKPOINT_STRIDE: u64 = 512;
/// Bounded number of canvases allowed in the background checkpoint backlog simultaneously.
pub(crate) const MAX_CHECKPOINT_BACKLOG: usize = 16;
/// Retain the latest two immutable checkpoints so concurrent downloads do not churn immediately.
const MAX_RETAINED_CHECKPOINTS: i64 = 2;

/// Immutable pinned checkpoint target queued off the serialized authority append path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingCheckpointJob {
    pub canvas: CanvasId,
    pub target_revision: u64,
    pub expected_chain_hash: String,
}

/// Pre-reconstructed, pre-compressed immutable checkpoint ready for fast atomic publication.
pub struct PreparedCheckpoint {
    pub canvas: CanvasId,
    pub revision: u64,
    pub schema_ver: u32,
    pub chain_hash: String,
    pub state_hash: String,
    pub compressed_sha256: String,
    pub compressed_bytes: u64,
    pub uncompressed_bytes: u64,
    pub body_zlib: Vec<u8>,
}
/// Self-describing prefix for zlib-compressed event blobs in `world_events.body`.
/// Canonical JSON always begins with `{`, so `KZ1\0` never collides with raw JSON rows.
const EVENT_ZLIB_MAGIC: &[u8; 4] = b"KZ1\0";
const EVENT_COMPRESS_MIN_BYTES: usize = 384;
const MAX_LIMITS_JSON_BYTES: usize = 4096;

pub(super) const TABLES: &str = "
CREATE TABLE world_meta(
    canvas TEXT PRIMARY KEY REFERENCES worlds(id),
    schema_ver INTEGER NOT NULL CHECK(schema_ver IN (1, 2)),
    metadata_revision INTEGER NOT NULL CHECK(metadata_revision >= 0),
    chain_hash TEXT NOT NULL,
    limits_json BLOB NOT NULL,
    stroke_count INTEGER NOT NULL CHECK(stroke_count >= 0),
    point_count INTEGER NOT NULL CHECK(point_count >= 0),
    media_count INTEGER NOT NULL CHECK(media_count >= 0),
    media_frame_count INTEGER NOT NULL CHECK(media_frame_count >= 0),
    tour_count INTEGER NOT NULL CHECK(tour_count >= 0)
);
CREATE TABLE world_devices(
    canvas TEXT NOT NULL REFERENCES worlds(id),
    device TEXT NOT NULL,
    author TEXT NOT NULL,
    last_sequence INTEGER NOT NULL CHECK(last_sequence > 0),
    PRIMARY KEY(canvas, device)
);
CREATE TABLE world_strokes_idx(
    canvas TEXT NOT NULL REFERENCES worlds(id),
    stroke TEXT NOT NULL,
    owner TEXT NOT NULL,
    device TEXT NOT NULL,
    points INTEGER NOT NULL CHECK(points > 0),
    visible INTEGER NOT NULL CHECK(visible IN (0, 1)),
    created_revision INTEGER NOT NULL CHECK(created_revision > 0),
    changed_revision INTEGER NOT NULL CHECK(changed_revision >= created_revision),
    PRIMARY KEY(canvas, stroke)
);
CREATE TABLE world_media_idx(
    canvas TEXT NOT NULL REFERENCES worlds(id),
    media TEXT NOT NULL,
    owner TEXT NOT NULL,
    device TEXT NOT NULL,
    frame_count INTEGER NOT NULL CHECK(frame_count > 0),
    visible INTEGER NOT NULL CHECK(visible IN (0, 1)),
    created_revision INTEGER NOT NULL CHECK(created_revision > 0),
    changed_revision INTEGER NOT NULL CHECK(changed_revision >= created_revision),
    PRIMARY KEY(canvas, media)
);
CREATE TABLE world_tours_idx(
    canvas TEXT NOT NULL REFERENCES worlds(id),
    tour TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    present INTEGER NOT NULL CHECK(present IN (0, 1)),
    PRIMARY KEY(canvas, tour)
);
CREATE TABLE world_checkpoints(
    canvas TEXT NOT NULL REFERENCES worlds(id),
    revision INTEGER NOT NULL CHECK(revision > 0),
    schema_ver INTEGER NOT NULL CHECK(schema_ver IN (1, 2)),
    chain_hash TEXT NOT NULL,
    state_hash TEXT NOT NULL,
    compressed_sha256 TEXT NOT NULL,
    compressed_bytes INTEGER NOT NULL CHECK(compressed_bytes > 0),
    uncompressed_bytes INTEGER NOT NULL CHECK(uncompressed_bytes > 0),
    body_zlib BLOB NOT NULL,
    PRIMARY KEY(canvas, revision)
);
";

pub(super) const TABLE_NAMES: &[&str] = &["world_meta", "world_devices", "world_strokes_idx", "world_media_idx", "world_tours_idx", "world_checkpoints"];

pub(super) fn encode_event_body(json: &[u8]) -> Result<Vec<u8>> {
    if json.len() <= EVENT_COMPRESS_MIN_BYTES {
        return Ok(json.to_vec());
    }
    let mut out = Vec::with_capacity(json.len().min(4096) + EVENT_ZLIB_MAGIC.len());
    out.extend_from_slice(EVENT_ZLIB_MAGIC);
    let mut enc = flate2::write::ZlibEncoder::new(out, flate2::Compression::new(5));
    enc.write_all(json).map_err(|_| Error::Storage)?;
    let compressed = enc.finish().map_err(|_| Error::Storage)?;
    if compressed.len() < json.len() { Ok(compressed) } else { Ok(json.to_vec()) }
}

pub(super) fn decode_event_body(raw: &[u8]) -> Result<Vec<u8>> {
    if raw.len() > MAX_EVENT_BYTES {
        return Err(Error::Storage);
    }
    if let Some(payload) = raw.strip_prefix(EVENT_ZLIB_MAGIC) {
        let mut dec = flate2::read::ZlibDecoder::new(payload).take((MAX_EVENT_BYTES as u64).saturating_add(1));
        let mut json = Vec::new();
        dec.read_to_end(&mut json).map_err(|_| Error::Storage)?;
        if json.len() > MAX_EVENT_BYTES {
            return Err(Error::Storage);
        }
        Ok(json)
    } else {
        Ok(raw.to_vec())
    }
}

fn is_hex64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

struct CanvasMetaRow {
    owner: OwnerId,
    name: String,
    initial_name: String,
    revision: u64,
    schema_ver: u32,
    metadata_revision: u64,
    chain_hash: String,
    limits: ReducerLimits,
    stroke_count: u64,
    point_count: u64,
    media_count: u64,
    media_frame_count: u64,
    tour_count: u64,
}

fn load_meta(db: &Connection, canvas: CanvasId) -> Result<CanvasMetaRow> {
    type RawMeta = (String, String, String, u64, u64, String, u32, u64, String, Vec<u8>, u64, u64, u64, u64, u64);
    let row: RawMeta = db
        .query_row(
            "SELECT CAST(substr(CAST(w.owner AS BLOB),1,33) AS TEXT),\
                    CAST(substr(CAST(w.name AS BLOB),1,97) AS TEXT),\
                    CAST(substr(CAST(w.initial_name AS BLOB),1,97) AS TEXT),\
                    w.revision,\
                    length(CAST(w.state AS BLOB)),\
                    CAST(substr(CAST(w.hash AS BLOB),1,65) AS TEXT),\
                    m.schema_ver,\
                    m.metadata_revision,\
                    CAST(substr(CAST(m.chain_hash AS BLOB),1,65) AS TEXT),\
                    substr(CAST(m.limits_json AS BLOB),1,?2),\
                    m.stroke_count,\
                    m.point_count,\
                    m.media_count,\
                    m.media_frame_count,\
                    m.tour_count \
             FROM worlds w JOIN world_meta m ON m.canvas=w.id WHERE w.id=?1",
            params![canvas.as_str(), (MAX_LIMITS_JSON_BYTES + 1) as i64],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    read_u64(r, 3)?,
                    read_u64(r, 4)?,
                    r.get(5)?,
                    r.get(6)?,
                    read_u64(r, 7)?,
                    r.get(8)?,
                    r.get(9)?,
                    read_u64(r, 10)?,
                    read_u64(r, 11)?,
                    read_u64(r, 12)?,
                    read_u64(r, 13)?,
                    read_u64(r, 14)?,
                ))
            },
        )
        .optional()?
        .ok_or(Error::Storage)?;
    let (
        owner,
        name,
        initial_name,
        revision,
        state_len,
        world_hash,
        schema_ver,
        metadata_revision,
        chain_hash,
        limits_json,
        stroke_count,
        point_count,
        media_count,
        media_frame_count,
        tour_count,
    ) = row;
    if state_len != 0 || !is_hex64(&world_hash) || world_hash != chain_hash || limits_json.len() > MAX_LIMITS_JSON_BYTES {
        return Err(Error::Storage);
    }
    if schema_ver != SHARED_SCHEMA && schema_ver != SHARED_SCHEMA_BRUSH {
        return Err(Error::Schema);
    }
    photocraft_scene::validate_name(&name).map_err(|_| Error::Storage)?;
    photocraft_scene::validate_name(&initial_name).map_err(|_| Error::Storage)?;
    let owner = OwnerId::new(&owner).map_err(|_| Error::Storage)?;
    let limits: ReducerLimits = serde_json::from_slice(&limits_json).map_err(|_| Error::Storage)?;
    limits.validate().map_err(|_| Error::Storage)?;
    Ok(CanvasMetaRow {
        owner,
        name,
        initial_name,
        revision,
        schema_ver,
        metadata_revision,
        chain_hash,
        limits,
        stroke_count,
        point_count,
        media_count,
        media_frame_count,
        tour_count,
    })
}

fn populate_indexes_from_world(db: &Connection, world: &WorldState, chain_hash: &str) -> Result<()> {
    let ckpt = world.to_checkpoint();
    let limits_json = serde_json::to_vec(&ckpt.limits)?;
    if limits_json.len() > MAX_LIMITS_JSON_BYTES {
        return Err(Error::Storage);
    }
    db.execute(
        "INSERT INTO world_meta(canvas,schema_ver,metadata_revision,chain_hash,limits_json,stroke_count,point_count,media_count,media_frame_count,tour_count) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            world.canvas.as_str(),
            world.schema,
            sql_int(world.metadata_revision)?,
            chain_hash,
            limits_json,
            sql_int(ckpt.strokes.len() as u64)?,
            sql_int(ckpt.points as u64)?,
            sql_int(ckpt.media.len() as u64)?,
            sql_int(ckpt.media_frames as u64)?,
            sql_int(ckpt.tour_revisions.len() as u64)?,
        ],
    )?;
    for (dev, &seq) in &ckpt.sequences {
        let author = ckpt.device_authors.get(dev).ok_or(Error::Storage)?;
        db.execute(
            "INSERT INTO world_devices(canvas,device,author,last_sequence) VALUES(?1,?2,?3,?4)",
            params![world.canvas.as_str(), dev.as_str(), author.as_str(), sql_int(seq)?],
        )?;
    }
    for (stroke_id, shared) in &ckpt.strokes {
        db.execute(
            "INSERT INTO world_strokes_idx(canvas,stroke,owner,device,points,visible,created_revision,changed_revision) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                world.canvas.as_str(),
                stroke_id.as_str(),
                shared.stroke.owner.as_str(),
                shared.device.as_str(),
                sql_int(shared.stroke.points.len() as u64)?,
                i64::from(shared.visible),
                sql_int(shared.created_revision)?,
                sql_int(shared.changed_revision)?,
            ],
        )?;
    }
    for (media_id, shared) in &ckpt.media {
        db.execute(
            "INSERT INTO world_media_idx(canvas,media,owner,device,frame_count,visible,created_revision,changed_revision) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                world.canvas.as_str(),
                media_id.as_str(),
                shared.media.owner.as_str(),
                shared.device.as_str(),
                sql_int(shared.media.frames.len() as u64)?,
                i64::from(shared.visible),
                sql_int(shared.created_revision)?,
                sql_int(shared.changed_revision)?,
            ],
        )?;
    }
    for (tour_id, &rev) in &ckpt.tour_revisions {
        let present = ckpt.tours.contains_key(tour_id);
        db.execute(
            "INSERT INTO world_tours_idx(canvas,tour,revision,present) VALUES(?1,?2,?3,?4)",
            params![world.canvas.as_str(), tour_id.as_str(), sql_int(rev)?, i64::from(present)],
        )?;
    }
    Ok(())
}

fn store_checkpoint(db: &Connection, canvas: CanvasId, ckpt: &WorldCheckpoint, chain_hash: &str) -> Result<()> {
    if ckpt.revision == 0 {
        return Ok(());
    }
    let (body_zlib, compressed_sha256, state_hash, uncompressed_bytes) = ckpt.encode_zlib().map_err(scene_error)?;
    let compressed_bytes = body_zlib.len() as u64;
    if compressed_bytes == 0
        || compressed_bytes > wire::MAX_CHECKPOINT_COMPRESSED_BYTES
        || uncompressed_bytes == 0
        || uncompressed_bytes > wire::MAX_CHECKPOINT_UNCOMPRESSED_BYTES
    {
        return Err(Domain::Limit("canvas checkpoint size").into());
    }
    db.execute(
        "INSERT OR REPLACE INTO world_checkpoints(canvas,revision,schema_ver,chain_hash,state_hash,compressed_sha256,compressed_bytes,uncompressed_bytes,body_zlib) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            canvas.as_str(),
            sql_int(ckpt.revision)?,
            ckpt.schema,
            chain_hash,
            state_hash,
            compressed_sha256,
            sql_int(compressed_bytes)?,
            sql_int(uncompressed_bytes)?,
            body_zlib,
        ],
    )?;
    db.execute(
        "DELETE FROM world_checkpoints WHERE canvas=?1 AND revision NOT IN (\
             SELECT revision FROM world_checkpoints WHERE canvas=?1 ORDER BY revision DESC LIMIT ?2\
         )",
        params![canvas.as_str(), MAX_RETAINED_CHECKPOINTS],
    )?;
    Ok(())
}

/// Insert a newly created, restored, or imported full-history `WorldState` into v6 tables.
/// Caller is responsible for `archive_import::store_origin` and `world_blobs` if applicable.
pub(super) fn insert_world(db: &Connection, world: &WorldState) -> Result<()> {
    let initial_name = world.initial_name();
    photocraft_scene::validate_name(initial_name).map_err(scene_error)?;
    photocraft_scene::validate_name(&world.name).map_err(scene_error)?;
    let mut chain = chain_hash_init(world.canvas, world.owner, initial_name);
    db.execute(
        "INSERT INTO worlds(id,owner,name,initial_name,revision,state,hash) VALUES(?1,?2,?3,?4,?5,X'',?6)",
        params![world.canvas.as_str(), world.owner.as_str(), &world.name, initial_name, sql_int(world.revision)?, &chain],
    )?;
    db.execute("INSERT INTO world_members VALUES(?1,?2,1,1)", params![world.canvas.as_str(), world.owner.as_str()])?;
    let mut after = 0_u64;
    loop {
        let events = world.events_after(after, MAX_EVENT_PAGE).map_err(scene_error)?;
        if events.is_empty() {
            break;
        }
        for event in events {
            let json = serde_json::to_vec(event)?;
            if json.len() > MAX_EVENT_BYTES {
                return Err(Domain::Limit("canvas event").into());
            }
            let ev_hash = digest(&json);
            chain = chain_hash_step(&chain, event.revision, &ev_hash);
            let stored = encode_event_body(&json)?;
            db.execute(
                "INSERT INTO world_events(canvas,revision,id,body,hash) VALUES(?1,?2,?3,?4,?5)",
                params![world.canvas.as_str(), sql_int(event.revision)?, event.op.id.as_str(), stored, ev_hash],
            )?;
            after = event.revision;
        }
    }
    if after != world.revision {
        return Err(Error::Storage);
    }
    db.execute("UPDATE worlds SET hash=?2 WHERE id=?1", params![world.canvas.as_str(), &chain])?;
    populate_indexes_from_world(db, world, &chain)?;
    if world.revision >= CHECKPOINT_STRIDE {
        let ckpt = world.to_checkpoint();
        store_checkpoint(db, world.canvas, &ckpt, &chain)?;
    }
    Ok(())
}

/// O(1) indexed authorized append on `world_schema = 6`.
/// Preserves exact authorization, device-sequence, limit, and metadata CAS rules of `WorldState::apply`.
pub(super) fn append_authorized_v6(db: &Connection, operation: &WorldOp) -> Result<Response> {
    let import_origin = archive_import::origin(db, operation.canvas)?;
    if import_origin.as_ref().is_some_and(|origin| origin.imported_device(operation.device)) {
        return Err(Domain::Unauthorized.into());
    }
    if let Some(prior) = prior_receipt(db, operation)? {
        return Ok(prior);
    }
    operation.validate().map_err(|error| match error {
        photocraft_scene::shared::Error::Limit(_) => Domain::Limit("canvas operation/history capacity; retain local work").into(),
        _ => Error::Invalid("canvas operation"),
    })?;
    let mut meta = load_meta(db, operation.canvas)?;
    if import_origin.as_ref().is_some_and(|origin| meta.revision < origin.source_revision) {
        return Err(Error::Storage);
    }
    let next_rev = meta.revision.checked_add(1).ok_or(Domain::Limit("canvas revision"))?;
    if next_rev > i64::MAX as u64 || meta.revision >= meta.limits.max_operations as u64 {
        return Err(Domain::Limit("canvas operation/history capacity; retain local work").into());
    }
    let dev_row: Option<(String, u64)> = db
        .query_row(
            "SELECT CAST(substr(CAST(author AS BLOB),1,33) AS TEXT),last_sequence FROM world_devices WHERE canvas=?1 AND device=?2",
            params![operation.canvas.as_str(), operation.device.as_str()],
            |r| Ok((r.get(0)?, read_u64(r, 1)?)),
        )
        .optional()?;
    let expected_seq = dev_row.as_ref().map_or(0, |(_, seq)| *seq).checked_add(1).filter(|n| *n <= i64::MAX as u64).ok_or(Domain::Limit("writer sequence"))?;
    if operation.sequence != expected_seq {
        return Err(Error::Invalid("missing writer operation; synchronize accepted sequence before retry"));
    }
    if let Some((prior_author, _)) = &dev_row
        && prior_author.as_str() != operation.author.as_str()
    {
        return Err(Domain::Unauthorized.into());
    }

    // Validate full action preconditions before any write.
    let mut new_tour_insert = false;
    match &operation.action {
        Edit::PlaceMedia { media } => {
            let exists: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM world_media_idx WHERE canvas=?1 AND media=?2)",
                params![operation.canvas.as_str(), media.id.as_str()],
                |r| r.get(0),
            )?;
            if exists {
                return Err(Domain::Conflict.into());
            }
            if meta.media_count >= meta.limits.max_media as u64
                || meta.media_frame_count.saturating_add(media.frames.len() as u64) > meta.limits.max_media_frames as u64
            {
                return Err(Domain::Limit("canvas operation/history capacity; retain local work").into());
            }
        }
        Edit::SetMediaVisible { media, .. } => {
            let found: Option<(String, String)> = db
                .query_row(
                    "SELECT CAST(substr(CAST(owner AS BLOB),1,33) AS TEXT),CAST(substr(CAST(device AS BLOB),1,33) AS TEXT) \
                     FROM world_media_idx WHERE canvas=?1 AND media=?2",
                    params![operation.canvas.as_str(), media.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let (owner, device) = found.ok_or(Error::Invalid("canvas operation"))?;
            if device.as_str() != operation.device.as_str() || owner.as_str() != operation.author.as_str() {
                return Err(Domain::Unauthorized.into());
            }
        }
        Edit::AddStroke { stroke } | Edit::CopyStroke { stroke, .. } => {
            let exists: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM world_strokes_idx WHERE canvas=?1 AND stroke=?2)",
                params![operation.canvas.as_str(), stroke.id.as_str()],
                |r| r.get(0),
            )?;
            if exists {
                return Err(Domain::Conflict.into());
            }
            if meta.stroke_count >= meta.limits.max_strokes as u64
                || meta.point_count.saturating_add(stroke.points.len() as u64) > meta.limits.max_points as u64
            {
                return Err(Domain::Limit("canvas operation/history capacity; retain local work").into());
            }
        }
        Edit::SetStrokeVisible { stroke, .. } => {
            let found: Option<(String, String)> = db
                .query_row(
                    "SELECT CAST(substr(CAST(owner AS BLOB),1,33) AS TEXT),CAST(substr(CAST(device AS BLOB),1,33) AS TEXT) \
                     FROM world_strokes_idx WHERE canvas=?1 AND stroke=?2",
                    params![operation.canvas.as_str(), stroke.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let (owner, device) = found.ok_or(Error::Invalid("canvas operation"))?;
            if device.as_str() != operation.device.as_str() || owner.as_str() != operation.author.as_str() {
                return Err(Domain::Unauthorized.into());
            }
        }
        Edit::Rename { expected_revision, .. } => {
            if operation.author != meta.owner {
                return Err(Domain::Unauthorized.into());
            }
            if *expected_revision != meta.metadata_revision {
                return Err(Domain::Conflict.into());
            }
        }
        Edit::PutTour { tour, expected_revision } => {
            let found: Option<u64> = db
                .query_row("SELECT revision FROM world_tours_idx WHERE canvas=?1 AND tour=?2", params![operation.canvas.as_str(), tour.id.as_str()], |r| {
                    read_u64(r, 0)
                })
                .optional()?;
            let current = found.unwrap_or(0);
            if *expected_revision != current {
                return Err(Domain::Conflict.into());
            }
            if found.is_none() {
                if meta.tour_count >= meta.limits.max_tours as u64 {
                    return Err(Domain::Limit("canvas operation/history capacity; retain local work").into());
                }
                new_tour_insert = true;
            }
        }
        Edit::RemoveTour { tour, expected_revision } => {
            let found: Option<(u64, u8)> = db
                .query_row("SELECT revision,present FROM world_tours_idx WHERE canvas=?1 AND tour=?2", params![operation.canvas.as_str(), tour.as_str()], |r| {
                    Ok((read_u64(r, 0)?, r.get(1)?))
                })
                .optional()?;
            let (current, present) = found.unwrap_or((0, 0));
            if *expected_revision != current {
                return Err(Domain::Conflict.into());
            }
            if present == 0 {
                return Err(Error::Invalid("canvas operation"));
            }
        }
    }

    let event = AcceptedOp { revision: next_rev, op: operation.clone() };
    let json = serde_json::to_vec(&event)?;
    if json.len() > MAX_EVENT_BYTES {
        return Err(Domain::Limit("canvas event").into());
    }
    let ev_hash = digest(&json);
    let stored_body = encode_event_body(&json)?;
    let new_chain = chain_hash_step(&meta.chain_hash, next_rev, &ev_hash);

    // Apply O(1) index updates.
    match &operation.action {
        Edit::PlaceMedia { media } => {
            meta.media_count = meta.media_count.saturating_add(1);
            meta.media_frame_count = meta.media_frame_count.saturating_add(media.frames.len() as u64);
            db.execute(
                "INSERT INTO world_media_idx(canvas,media,owner,device,frame_count,visible,created_revision,changed_revision) \
                 VALUES(?1,?2,?3,?4,?5,1,?6,?6)",
                params![
                    operation.canvas.as_str(),
                    media.id.as_str(),
                    media.owner.as_str(),
                    operation.device.as_str(),
                    sql_int(media.frames.len() as u64)?,
                    sql_int(next_rev)?,
                ],
            )?;
        }
        Edit::SetMediaVisible { media, visible } => {
            db.execute(
                "UPDATE world_media_idx SET visible=?3,changed_revision=?4 WHERE canvas=?1 AND media=?2",
                params![operation.canvas.as_str(), media.as_str(), i64::from(*visible), sql_int(next_rev)?],
            )?;
        }
        Edit::AddStroke { stroke } | Edit::CopyStroke { stroke, .. } => {
            if stroke.uses_brush_v2() {
                meta.schema_ver = meta.schema_ver.max(SHARED_SCHEMA_BRUSH);
            }
            meta.stroke_count = meta.stroke_count.saturating_add(1);
            meta.point_count = meta.point_count.saturating_add(stroke.points.len() as u64);
            db.execute(
                "INSERT INTO world_strokes_idx(canvas,stroke,owner,device,points,visible,created_revision,changed_revision) \
                 VALUES(?1,?2,?3,?4,?5,1,?6,?6)",
                params![
                    operation.canvas.as_str(),
                    stroke.id.as_str(),
                    stroke.owner.as_str(),
                    operation.device.as_str(),
                    sql_int(stroke.points.len() as u64)?,
                    sql_int(next_rev)?,
                ],
            )?;
        }
        Edit::SetStrokeVisible { stroke, visible } => {
            db.execute(
                "UPDATE world_strokes_idx SET visible=?3,changed_revision=?4 WHERE canvas=?1 AND stroke=?2",
                params![operation.canvas.as_str(), stroke.as_str(), i64::from(*visible), sql_int(next_rev)?],
            )?;
        }
        Edit::Rename { name, .. } => {
            meta.name = name.clone();
            meta.metadata_revision = next_rev;
        }
        Edit::PutTour { tour, .. } => {
            if new_tour_insert {
                meta.tour_count = meta.tour_count.saturating_add(1);
            }
            db.execute(
                "INSERT INTO world_tours_idx(canvas,tour,revision,present) VALUES(?1,?2,?3,1) \
                 ON CONFLICT(canvas,tour) DO UPDATE SET revision=excluded.revision,present=1",
                params![operation.canvas.as_str(), tour.id.as_str(), sql_int(next_rev)?],
            )?;
        }
        Edit::RemoveTour { tour, .. } => {
            db.execute(
                "UPDATE world_tours_idx SET revision=?3,present=0 WHERE canvas=?1 AND tour=?2",
                params![operation.canvas.as_str(), tour.as_str(), sql_int(next_rev)?],
            )?;
        }
    }

    db.execute(
        "INSERT INTO world_devices(canvas,device,author,last_sequence) VALUES(?1,?2,?3,?4) \
         ON CONFLICT(canvas,device) DO UPDATE SET last_sequence=excluded.last_sequence",
        params![operation.canvas.as_str(), operation.device.as_str(), operation.author.as_str(), sql_int(operation.sequence)?],
    )?;
    db.execute(
        "INSERT INTO world_events(canvas,revision,id,body,hash) VALUES(?1,?2,?3,?4,?5)",
        params![operation.canvas.as_str(), sql_int(next_rev)?, operation.id.as_str(), stored_body, ev_hash],
    )?;
    db.execute(
        "UPDATE world_meta SET schema_ver=?2,metadata_revision=?3,chain_hash=?4,stroke_count=?5,point_count=?6,media_count=?7,media_frame_count=?8,tour_count=?9 WHERE canvas=?1",
        params![
            operation.canvas.as_str(),
            meta.schema_ver,
            sql_int(meta.metadata_revision)?,
            &new_chain,
            sql_int(meta.stroke_count)?,
            sql_int(meta.point_count)?,
            sql_int(meta.media_count)?,
            sql_int(meta.media_frame_count)?,
            sql_int(meta.tour_count)?,
        ],
    )?;
    db.execute(
        "UPDATE worlds SET name=?2,revision=?3,state=X'',hash=?4 WHERE id=?1",
        params![operation.canvas.as_str(), &meta.name, sql_int(next_rev)?, &new_chain],
    )?;

    // Checkpoint reconstruction & compression run asynchronously off the authority append
    // transaction (`PendingCheckpointJob` -> `build_prepared_checkpoint_from_path` ->
    // `publish_prepared_checkpoint`) so ordinary appends and concurrent rooms/canvases never block.
    Ok(Response::Accepted { event: Box::new(event) })
}

/// Build an immutable `PendingCheckpointJob` for `canvas` if its durable head is at `expected_rev`.
pub(super) fn pending_job_at_head(db: &Connection, canvas: CanvasId, expected_rev: u64) -> Result<Option<PendingCheckpointJob>> {
    let row: Option<(u64, String)> = db
        .query_row(
            "SELECT w.revision, CAST(substr(CAST(m.chain_hash AS BLOB),1,65) AS TEXT) \
             FROM worlds w JOIN world_meta m ON m.canvas=w.id WHERE w.id=?1",
            [canvas.as_str()],
            |r| Ok((read_u64(r, 0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((head_rev, chain_hash)) = row else {
        return Ok(None);
    };
    if head_rev != expected_rev || !is_hex64(&chain_hash) {
        return Ok(None);
    }
    Ok(Some(PendingCheckpointJob { canvas, target_revision: expected_rev, expected_chain_hash: chain_hash }))
}

/// On startup/recovery, detect any canvases whose durable head has advanced at least
/// `CHECKPOINT_STRIDE` revisions past their latest published checkpoint and queue bounded jobs.
pub(crate) fn scan_pending_checkpoints(db: &Connection, max_jobs: usize) -> Result<Vec<PendingCheckpointJob>> {
    let mut stmt = db.prepare(
        "SELECT CAST(substr(CAST(w.id AS BLOB),1,33) AS TEXT), w.revision, CAST(substr(CAST(m.chain_hash AS BLOB),1,65) AS TEXT) \
         FROM worlds w JOIN world_meta m ON m.canvas=w.id \
         WHERE NOT EXISTS(SELECT 1 FROM world_trash t WHERE t.canvas=w.id) \
           AND w.revision - coalesce((SELECT max(c.revision) FROM world_checkpoints c WHERE c.canvas=w.id), 0) >= ?1 \
         ORDER BY w.revision DESC LIMIT ?2",
    )?;
    let mut rows = stmt.query(params![sql_int(CHECKPOINT_STRIDE)?, sql_int(max_jobs as u64)?])?;
    let mut jobs = Vec::new();
    while let Some(row) = rows.next()? {
        let id_str: String = row.get(0)?;
        let rev = read_u64(row, 1)?;
        let chain_hash: String = row.get(2)?;
        if let Ok(canvas) = CanvasId::new(&id_str)
            && is_hex64(&chain_hash)
        {
            jobs.push(PendingCheckpointJob { canvas, target_revision: rev, expected_chain_hash: chain_hash });
        }
    }
    Ok(jobs)
}

/// Phase 1 (short read-only SQLite snapshot) + Phase 2 (100% lock-free in-memory reconstruction &
/// zlib compression) for a pinned immutable `(canvas, target_revision, expected_chain_hash)` target.
///
/// The SQLite read connection is closed immediately after copying raw bytes so the authority writer
/// thread is never blocked during decompression, replay, or zlib compression.
pub(crate) fn build_prepared_checkpoint_from_path(db_path: &std::path::Path, job: &PendingCheckpointJob) -> Result<Option<PreparedCheckpoint>> {
    if job.target_revision == 0 || !is_hex64(&job.expected_chain_hash) {
        return Ok(None);
    }
    type RawPriorCkpt = (u64, u32, String, String, String, u64, u64, Vec<u8>);
    type RawEventRow = (u64, String, Vec<u8>, String);

    // Phase 1: open short-lived read-only connection, copy raw rows, then drop connection immediately.
    let (meta, import_origin, prior_ckpt, raw_events) = {
        let db = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM worlds WHERE id=?1)", [job.canvas.as_str()], |r| r.get(0))?;
        if !exists {
            return Ok(None);
        }
        let meta = load_meta(&db, job.canvas)?;
        if meta.revision < job.target_revision {
            return Ok(None);
        }
        let latest_ckpt: Option<u64> = db.query_row("SELECT max(revision) FROM world_checkpoints WHERE canvas=?1", [job.canvas.as_str()], |r| {
            let v: Option<i64> = r.get(0)?;
            match v {
                Some(n) => u64::try_from(n).map(Some).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, n)),
                None => Ok(None),
            }
        })?;
        if latest_ckpt.is_some_and(|r| r >= job.target_revision) {
            return Ok(None);
        }
        let import_origin = archive_import::origin(&db, job.canvas)?;
        let prior_ckpt: Option<RawPriorCkpt> = db
            .query_row(
                "SELECT revision,schema_ver,\
                        CAST(substr(CAST(chain_hash AS BLOB),1,65) AS TEXT),\
                        CAST(substr(CAST(state_hash AS BLOB),1,65) AS TEXT),\
                        CAST(substr(CAST(compressed_sha256 AS BLOB),1,65) AS TEXT),\
                        compressed_bytes,uncompressed_bytes,\
                        substr(CAST(body_zlib AS BLOB),1,?3) \
                 FROM world_checkpoints WHERE canvas=?1 AND revision<?2 ORDER BY revision DESC LIMIT 1",
                params![job.canvas.as_str(), sql_int(job.target_revision)?, (wire::MAX_CHECKPOINT_COMPRESSED_BYTES + 1) as i64],
                |r| Ok((read_u64(r, 0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, read_u64(r, 5)?, read_u64(r, 6)?, r.get(7)?)),
            )
            .optional()?;
        let base_rev = prior_ckpt.as_ref().map_or(0, |c| c.0);
        let mut raw_events: Vec<RawEventRow> = Vec::new();
        {
            let mut stmt = db.prepare(
                "SELECT revision,CAST(substr(CAST(id AS BLOB),1,33) AS TEXT),substr(CAST(body AS BLOB),1,?4),CAST(substr(CAST(hash AS BLOB),1,65) AS TEXT) \
                 FROM world_events WHERE canvas=?1 AND revision>?2 AND revision<=?3 ORDER BY revision",
            )?;
            let mut rows = stmt.query(params![job.canvas.as_str(), sql_int(base_rev)?, sql_int(job.target_revision)?, (MAX_EVENT_BYTES + 1) as i64])?;
            while let Some(row) = rows.next()? {
                raw_events.push((read_u64(row, 0)?, row.get(1)?, row.get(2)?, row.get(3)?));
            }
        }
        (meta, import_origin, prior_ckpt, raw_events)
    };

    // Phase 2: 100% lock-free CPU reconstruction, chain verification, and zlib compression.
    let (mut world, mut chain, mut after) = if let Some((rev, schema_ver, ckpt_chain, state_hash, comp_sha, comp_bytes, uncomp_bytes, body)) = prior_ckpt {
        if !is_hex64(&ckpt_chain)
            || !is_hex64(&state_hash)
            || !is_hex64(&comp_sha)
            || body.len() as u64 != comp_bytes
            || comp_bytes > wire::MAX_CHECKPOINT_COMPRESSED_BYTES
            || uncomp_bytes > wire::MAX_CHECKPOINT_UNCOMPRESSED_BYTES
        {
            return Err(Error::Storage);
        }
        let ckpt = WorldCheckpoint::decode_zlib_bounded(&body, &comp_sha, uncomp_bytes, &state_hash, wire::MAX_CHECKPOINT_UNCOMPRESSED_BYTES)
            .map_err(|_| Error::Storage)?;
        if ckpt.canvas != job.canvas
            || ckpt.revision != rev
            || ckpt.schema != schema_ver
            || ckpt.owner != meta.owner
            || ckpt.initial_name != meta.initial_name
            || ckpt.import_origin != import_origin
        {
            return Err(Error::Storage);
        }
        (WorldState::from_checkpoint(ckpt).map_err(|_| Error::Storage)?, ckpt_chain, rev)
    } else {
        let mut w = WorldState::with_limits(job.canvas, meta.owner, meta.initial_name.clone(), meta.limits).map_err(|_| Error::Storage)?;
        w.import_origin = import_origin;
        let c = chain_hash_init(job.canvas, meta.owner, &meta.initial_name);
        (w, c, 0)
    };

    for (rev, id_str, raw_body, ev_hash) in raw_events {
        let json = decode_event_body(&raw_body)?;
        if ev_hash.len() != 64 || digest(&json) != ev_hash {
            return Err(Error::Storage);
        }
        let event: AcceptedOp = serde_json::from_slice(&json)?;
        if event.revision != rev || event.op.id.as_str() != id_str || event.op.canvas != job.canvas {
            return Err(Error::Storage);
        }
        event.op.validate().map_err(|_| Error::Storage)?;
        if rev != after.checked_add(1).ok_or(Error::Storage)? {
            return Err(Error::Storage);
        }
        chain = chain_hash_step(&chain, rev, &ev_hash);
        world.apply(&event).map_err(|_| Error::Storage)?;
        after = rev;
    }

    if after != job.target_revision || world.revision != job.target_revision || chain != job.expected_chain_hash {
        return Err(Error::Storage);
    }

    let ckpt = world.to_checkpoint();
    let (body_zlib, compressed_sha256, state_hash, uncompressed_bytes) = ckpt.encode_zlib().map_err(scene_error)?;
    let compressed_bytes = body_zlib.len() as u64;
    if compressed_bytes == 0
        || compressed_bytes > wire::MAX_CHECKPOINT_COMPRESSED_BYTES
        || uncompressed_bytes == 0
        || uncompressed_bytes > wire::MAX_CHECKPOINT_UNCOMPRESSED_BYTES
    {
        return Err(Domain::Limit("canvas checkpoint size").into());
    }

    Ok(Some(PreparedCheckpoint {
        canvas: job.canvas,
        revision: ckpt.revision,
        schema_ver: ckpt.schema,
        chain_hash: chain,
        state_hash,
        compressed_sha256,
        compressed_bytes,
        uncompressed_bytes,
        body_zlib,
    }))
}

/// Fast atomic publication of a pre-compressed `PreparedCheckpoint` on the authority writer thread.
/// Re-verifies canvas existence, monotonic revision, and immutable event presence before committing.
pub(crate) fn publish_prepared_checkpoint(db: &Connection, prepared: &PreparedCheckpoint) -> Result<bool> {
    if prepared.revision == 0
        || prepared.compressed_bytes == 0
        || prepared.compressed_bytes != prepared.body_zlib.len() as u64
        || prepared.compressed_bytes > wire::MAX_CHECKPOINT_COMPRESSED_BYTES
        || prepared.uncompressed_bytes == 0
        || prepared.uncompressed_bytes > wire::MAX_CHECKPOINT_UNCOMPRESSED_BYTES
        || !is_hex64(&prepared.chain_hash)
        || !is_hex64(&prepared.state_hash)
        || !is_hex64(&prepared.compressed_sha256)
    {
        return Err(Error::Invalid("prepared checkpoint"));
    }
    let head_rev: Option<u64> = db.query_row("SELECT revision FROM worlds WHERE id=?1", [prepared.canvas.as_str()], |r| read_u64(r, 0)).optional()?;
    let Some(head_rev) = head_rev else {
        return Ok(false);
    };
    if head_rev < prepared.revision {
        return Ok(false);
    }
    let latest_ckpt: Option<u64> = db.query_row("SELECT max(revision) FROM world_checkpoints WHERE canvas=?1", [prepared.canvas.as_str()], |r| {
        let v: Option<i64> = r.get(0)?;
        match v {
            Some(n) => u64::try_from(n).map(Some).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, n)),
            None => Ok(None),
        }
    })?;
    if latest_ckpt.is_some_and(|r| r >= prepared.revision) {
        return Ok(false);
    }
    let ev_exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM world_events WHERE canvas=?1 AND revision=?2)",
        params![prepared.canvas.as_str(), sql_int(prepared.revision)?],
        |r| r.get(0),
    )?;
    if !ev_exists {
        return Err(Error::Storage);
    }
    db.execute(
        "INSERT OR REPLACE INTO world_checkpoints(canvas,revision,schema_ver,chain_hash,state_hash,compressed_sha256,compressed_bytes,uncompressed_bytes,body_zlib) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            prepared.canvas.as_str(),
            sql_int(prepared.revision)?,
            prepared.schema_ver,
            &prepared.chain_hash,
            &prepared.state_hash,
            &prepared.compressed_sha256,
            sql_int(prepared.compressed_bytes)?,
            sql_int(prepared.uncompressed_bytes)?,
            &prepared.body_zlib,
        ],
    )?;
    db.execute(
        "DELETE FROM world_checkpoints WHERE canvas=?1 AND revision NOT IN (\
             SELECT revision FROM world_checkpoints WHERE canvas=?1 ORDER BY revision DESC LIMIT ?2\
         )",
        params![prepared.canvas.as_str(), MAX_RETAINED_CHECKPOINTS],
    )?;
    Ok(true)
}

fn load_latest_checkpoint(db: &Connection, canvas: CanvasId) -> Result<Option<(WorldCheckpoint, String)>> {
    type RawCkpt = (u64, u32, String, String, String, u64, u64, Vec<u8>);
    let row: Option<RawCkpt> = db
        .query_row(
            "SELECT revision,schema_ver,\
                    CAST(substr(CAST(chain_hash AS BLOB),1,65) AS TEXT),\
                    CAST(substr(CAST(state_hash AS BLOB),1,65) AS TEXT),\
                    CAST(substr(CAST(compressed_sha256 AS BLOB),1,65) AS TEXT),\
                    compressed_bytes,uncompressed_bytes,\
                    substr(CAST(body_zlib AS BLOB),1,?2) \
             FROM world_checkpoints WHERE canvas=?1 ORDER BY revision DESC LIMIT 1",
            params![canvas.as_str(), (wire::MAX_CHECKPOINT_COMPRESSED_BYTES + 1) as i64],
            |r| Ok((read_u64(r, 0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, read_u64(r, 5)?, read_u64(r, 6)?, r.get(7)?)),
        )
        .optional()?;
    let Some((rev, schema_ver, chain_hash, state_hash, comp_sha, comp_bytes, uncomp_bytes, body)) = row else {
        return Ok(None);
    };
    if !is_hex64(&chain_hash)
        || !is_hex64(&state_hash)
        || !is_hex64(&comp_sha)
        || body.len() as u64 != comp_bytes
        || comp_bytes > wire::MAX_CHECKPOINT_COMPRESSED_BYTES
        || uncomp_bytes > wire::MAX_CHECKPOINT_UNCOMPRESSED_BYTES
    {
        return Err(Error::Storage);
    }
    let ckpt = WorldCheckpoint::decode_zlib_bounded(&body, &comp_sha, uncomp_bytes, &state_hash, wire::MAX_CHECKPOINT_UNCOMPRESSED_BYTES)
        .map_err(|_| Error::Storage)?;
    if ckpt.canvas != canvas || ckpt.revision != rev || ckpt.schema != schema_ver {
        return Err(Error::Storage);
    }
    Ok(Some((ckpt, chain_hash)))
}

/// Reconstruct a canvas's `WorldState` from v6 tables.
/// - `full_history == false`: loads the latest checkpoint + replays at most `<= 511` tail events.
/// - `full_history == true`: replays `1..=head` events so `replay_to`/`copy_to` have full history.
pub(super) fn reconstruct_world(db: &Connection, canvas: CanvasId, full_history: bool) -> Result<WorldState> {
    let meta = load_meta(db, canvas)?;
    let import_origin = archive_import::origin(db, canvas)?;
    let (mut world, mut chain, mut after) = if !full_history && let Some((ckpt, ckpt_chain)) = load_latest_checkpoint(db, canvas)? {
        if ckpt.revision > meta.revision || ckpt.owner != meta.owner || ckpt.initial_name != meta.initial_name || ckpt.import_origin != import_origin {
            return Err(Error::Storage);
        }
        let rev = ckpt.revision;
        (WorldState::from_checkpoint(ckpt).map_err(|_| Error::Storage)?, ckpt_chain, rev)
    } else {
        let mut w = WorldState::with_limits(canvas, meta.owner, meta.initial_name.clone(), meta.limits).map_err(|_| Error::Storage)?;
        w.import_origin = import_origin;
        let c = chain_hash_init(canvas, meta.owner, &meta.initial_name);
        (w, c, 0)
    };

    let mut stmt = db.prepare(
        "SELECT substr(CAST(body AS BLOB),1,?3),CAST(substr(CAST(hash AS BLOB),1,65) AS TEXT),revision,CAST(substr(CAST(id AS BLOB),1,33) AS TEXT) \
         FROM world_events WHERE canvas=?1 AND revision>?2 ORDER BY revision",
    )?;
    let mut rows = stmt.query(params![canvas.as_str(), sql_int(after)?, (MAX_EVENT_BYTES + 1) as i64])?;
    while let Some(row) = rows.next()? {
        let hash_col: String = row.get(1)?;
        let (event, _) = read_event(row, canvas)?;
        if event.revision != after.checked_add(1).ok_or(Error::Storage)? {
            return Err(Error::Storage);
        }
        chain = chain_hash_step(&chain, event.revision, &hash_col);
        world.apply(&event).map_err(|_| Error::Storage)?;
        after = event.revision;
    }

    if after != meta.revision
        || world.revision != meta.revision
        || world.name != meta.name
        || world.schema != meta.schema_ver
        || world.metadata_revision != meta.metadata_revision
        || chain != meta.chain_hash
    {
        return Err(Error::Storage);
    }
    Ok(world)
}

/// Handle `Action::CheckpointMeta`: always re-verifies live membership grant before returning metadata.
pub(super) fn checkpoint_meta(db: &Connection, profile: OwnerId, canvas: CanvasId) -> Result<Response> {
    grant(db, canvas, profile)?;
    type RawMeta = (u64, u32, String, String, String, u64, u64);
    let row: Option<RawMeta> = db
        .query_row(
            "SELECT revision,schema_ver,\
                    CAST(substr(CAST(chain_hash AS BLOB),1,65) AS TEXT),\
                    CAST(substr(CAST(state_hash AS BLOB),1,65) AS TEXT),\
                    CAST(substr(CAST(compressed_sha256 AS BLOB),1,65) AS TEXT),\
                    compressed_bytes,uncompressed_bytes \
             FROM world_checkpoints WHERE canvas=?1 ORDER BY revision DESC LIMIT 1",
            [canvas.as_str()],
            |r| Ok((read_u64(r, 0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, read_u64(r, 5)?, read_u64(r, 6)?)),
        )
        .optional()?;
    let checkpoint = match row {
        Some((revision, schema_ver, chain_hash, state_hash, compressed_sha256, compressed_bytes, uncompressed_bytes)) => {
            let ckpt = CheckpointRef {
                canvas,
                revision,
                schema_ver,
                chain_hash: AssetHash::new(&chain_hash).map_err(|_| Error::Storage)?,
                state_hash: AssetHash::new(&state_hash).map_err(|_| Error::Storage)?,
                compressed_sha256: AssetHash::new(&compressed_sha256).map_err(|_| Error::Storage)?,
                compressed_bytes,
                uncompressed_bytes,
            };
            ckpt.validate().map_err(|_| Error::Storage)?;
            Some(ckpt)
        }
        None => None,
    };
    Ok(Response::CheckpointMeta { checkpoint })
}

/// Handle `Action::CheckpointChunk`: re-verifies live grant on every chunk, pins to exact
/// `(canvas, revision, compressed_sha256)`, and returns `Domain::Conflict` on rotation/miss.
pub(super) fn checkpoint_chunk(
    db: &Connection,
    profile: OwnerId,
    canvas: CanvasId,
    revision: u64,
    compressed_sha256: AssetHash,
    offset: u64,
    len: u32,
) -> Result<Response> {
    grant(db, canvas, profile)?;
    if revision == 0
        || revision > i64::MAX as u64
        || len == 0
        || len > wire::MAX_CHECKPOINT_CHUNK_BYTES
        || offset.checked_add(u64::from(len)).is_none_or(|end| end > wire::MAX_CHECKPOINT_COMPRESSED_BYTES)
    {
        return Err(Error::Invalid("checkpoint chunk bounds"));
    }
    let total: Option<u64> = db
        .query_row(
            "SELECT compressed_bytes FROM world_checkpoints WHERE canvas=?1 AND revision=?2 AND compressed_sha256=?3",
            params![canvas.as_str(), sql_int(revision)?, compressed_sha256.as_str()],
            |r| read_u64(r, 0),
        )
        .optional()?;
    let Some(total) = total else {
        return Err(Domain::Conflict.into());
    };
    if total == 0 || total > wire::MAX_CHECKPOINT_COMPRESSED_BYTES || offset >= total {
        return Err(Error::Invalid("checkpoint chunk offset"));
    }
    let actual_len = (total - offset).min(u64::from(len));
    let bytes: Vec<u8> = db.query_row(
        "SELECT substr(CAST(body_zlib AS BLOB),?4,?5) FROM world_checkpoints WHERE canvas=?1 AND revision=?2 AND compressed_sha256=?3",
        params![canvas.as_str(), sql_int(revision)?, compressed_sha256.as_str(), sql_int(offset + 1)?, sql_int(actual_len)?],
        |r| r.get(0),
    )?;
    if bytes.len() as u64 != actual_len {
        return Err(Error::Storage);
    }
    Ok(Response::CheckpointChunk { revision, compressed_sha256, offset, bytes, total })
}

/// Migrate a verified `world_schema = 5` database to `world_schema = 6` with an automatic
/// verified backup before touching any row.
pub(super) fn migrate_v6(db: &Connection, path: &std::path::Path) -> Result<()> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| Error::Storage)?;
    let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let mut name = path.file_name().ok_or(Error::Invalid("canvas migration path"))?.to_os_string();
    name.push(format!(".before-world-v6-{suffix}.sqlite3"));
    crate::backup::copy(db, &path.with_file_name(name))?;

    let tx = db.unchecked_transaction()?;
    tx.execute_batch(TABLES)?;

    let mut ids = Vec::new();
    {
        let mut stmt = tx.prepare("SELECT CAST(substr(CAST(id AS BLOB),1,33) AS TEXT) FROM worlds ORDER BY id")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id_str: String = row.get(0)?;
            ids.push(CanvasId::new(&id_str).map_err(|_| Error::Storage)?);
        }
    }

    for id in ids {
        let world = super::legacy_state(&tx, id)?;
        world.validate().map_err(|_| Error::Storage)?;
        let initial_name = world.initial_name();
        let mut chain = chain_hash_init(id, world.owner, initial_name);
        let mut stmt = tx.prepare(
            "SELECT substr(CAST(body AS BLOB),1,?2),CAST(substr(CAST(hash AS BLOB),1,65) AS TEXT),revision,CAST(substr(CAST(id AS BLOB),1,33) AS TEXT) \
             FROM world_events WHERE canvas=?1 ORDER BY revision",
        )?;
        let mut rows = stmt.query(params![id.as_str(), (MAX_EVENT_BYTES + 1) as i64])?;
        let mut count = 0_u64;
        while let Some(row) = rows.next()? {
            let ev_hash: String = row.get(1)?;
            let (event, _) = read_event(row, id)?;
            if event.revision != count + 1 || world.receipt(event.op.id) != Some(&event) {
                return Err(Error::Storage);
            }
            chain = chain_hash_step(&chain, event.revision, &ev_hash);
            count += 1;
        }
        drop(rows);
        drop(stmt);
        if count != world.revision {
            return Err(Error::Storage);
        }
        tx.execute("UPDATE worlds SET state=X'',hash=?2 WHERE id=?1", params![id.as_str(), &chain])?;
        populate_indexes_from_world(&tx, &world, &chain)?;
        if world.revision >= CHECKPOINT_STRIDE {
            let ckpt = world.to_checkpoint();
            store_checkpoint(&tx, id, &ckpt, &chain)?;
        }
    }

    tx.execute_batch("DROP TABLE world_schema; CREATE TABLE world_schema(version INTEGER NOT NULL CHECK(version=6)); INSERT INTO world_schema VALUES(6);")?;
    tx.pragma_update(None, "user_version", 6)?;
    super::verify(&tx)?;
    tx.commit()?;
    Ok(())
}

/// Full integrity verification of all v6 tables, event chains, index tables, and stored checkpoints.
pub(super) fn verify_v6(db: &Connection) -> Result<()> {
    for table in TABLE_NAMES {
        let orphans: u64 =
            db.query_row(&format!("SELECT count(*) FROM {table} t LEFT JOIN worlds w ON w.id=t.canvas WHERE w.id IS NULL"), [], |r| read_u64(r, 0))?;
        if orphans != 0 {
            return Err(Error::Storage);
        }
    }

    let mut stmt = db.prepare(
        "SELECT CAST(substr(CAST(id AS BLOB),1,33) AS TEXT),\
                CAST(substr(CAST(owner AS BLOB),1,33) AS TEXT),\
                CAST(substr(CAST(name AS BLOB),1,97) AS TEXT),\
                revision,\
                CAST(substr(CAST(initial_name AS BLOB),1,97) AS TEXT) \
         FROM worlds ORDER BY id",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id = CanvasId::new(&row.get::<_, String>(0)?).map_err(|_| Error::Storage)?;
        let owner = OwnerId::new(&row.get::<_, String>(1)?).map_err(|_| Error::Storage)?;
        let name: String = row.get(2)?;
        let revision = read_u64(row, 3)?;
        let initial_name: String = row.get(4)?;
        photocraft_scene::validate_name(&name).map_err(|_| Error::Storage)?;
        photocraft_scene::validate_name(&initial_name).map_err(|_| Error::Storage)?;
        member_epoch(db, id, owner).map_err(|_| Error::Storage)?;

        let meta = load_meta(db, id)?;
        if meta.owner != owner || meta.name != name || meta.initial_name != initial_name || meta.revision != revision {
            return Err(Error::Storage);
        }
        let import_origin = archive_import::origin(db, id)?;
        if import_origin.as_ref().is_some_and(|origin| revision < origin.source_revision) {
            return Err(Error::Storage);
        }

        // Index stored checkpoints by revision so we verify every checkpoint in the single linear pass.
        type CkptMeta = (u32, String, String, String, u64, u64);
        let mut ckpts: BTreeMap<u64, CkptMeta> = BTreeMap::new();
        {
            let mut cstmt = db.prepare(
                "SELECT revision,schema_ver,\
                        CAST(substr(CAST(chain_hash AS BLOB),1,65) AS TEXT),\
                        CAST(substr(CAST(state_hash AS BLOB),1,65) AS TEXT),\
                        CAST(substr(CAST(compressed_sha256 AS BLOB),1,65) AS TEXT),\
                        compressed_bytes,uncompressed_bytes,length(CAST(body_zlib AS BLOB)) \
                 FROM world_checkpoints WHERE canvas=?1 ORDER BY revision",
            )?;
            let mut crows = cstmt.query([id.as_str()])?;
            while let Some(crow) = crows.next()? {
                let rev = read_u64(crow, 0)?;
                let schema_ver: u32 = crow.get(1)?;
                let c_chain: String = crow.get(2)?;
                let s_hash: String = crow.get(3)?;
                let comp_sha: String = crow.get(4)?;
                let comp_bytes = read_u64(crow, 5)?;
                let uncomp_bytes = read_u64(crow, 6)?;
                let body_len = read_u64(crow, 7)?;
                if rev == 0
                    || rev > revision
                    || !is_hex64(&c_chain)
                    || !is_hex64(&s_hash)
                    || !is_hex64(&comp_sha)
                    || comp_bytes == 0
                    || comp_bytes != body_len
                    || comp_bytes > wire::MAX_CHECKPOINT_COMPRESSED_BYTES
                    || uncomp_bytes == 0
                    || uncomp_bytes > wire::MAX_CHECKPOINT_UNCOMPRESSED_BYTES
                {
                    return Err(Error::Storage);
                }
                ckpts.insert(rev, (schema_ver, c_chain, s_hash, comp_sha, comp_bytes, uncomp_bytes));
            }
        }

        let mut world = WorldState::with_limits(id, owner, initial_name.clone(), meta.limits).map_err(|_| Error::Storage)?;
        world.import_origin = import_origin.clone();
        let mut chain = chain_hash_init(id, owner, &initial_name);

        let mut estmt = db.prepare(
            "SELECT substr(CAST(body AS BLOB),1,?2),CAST(substr(CAST(hash AS BLOB),1,65) AS TEXT),revision,CAST(substr(CAST(id AS BLOB),1,33) AS TEXT) \
             FROM world_events WHERE canvas=?1 ORDER BY revision",
        )?;
        let mut erows = estmt.query(params![id.as_str(), (MAX_EVENT_BYTES + 1) as i64])?;
        let mut count = 0_u64;
        while let Some(erow) = erows.next()? {
            let hash_col: String = erow.get(1)?;
            let (event, _) = read_event(erow, id)?;
            if event.revision != count + 1 {
                return Err(Error::Storage);
            }
            chain = chain_hash_step(&chain, event.revision, &hash_col);
            world.apply(&event).map_err(|_| Error::Storage)?;
            count += 1;

            if let Some((schema_ver, c_chain, s_hash, comp_sha, comp_bytes, uncomp_bytes)) = ckpts.remove(&count) {
                if c_chain != chain || schema_ver != world.schema {
                    return Err(Error::Storage);
                }
                let body: Vec<u8> = db.query_row(
                    "SELECT substr(CAST(body_zlib AS BLOB),1,?3) FROM world_checkpoints WHERE canvas=?1 AND revision=?2",
                    params![id.as_str(), sql_int(count)?, (wire::MAX_CHECKPOINT_COMPRESSED_BYTES + 1) as i64],
                    |r| r.get(0),
                )?;
                if body.len() as u64 != comp_bytes {
                    return Err(Error::Storage);
                }
                let decoded = WorldCheckpoint::decode_zlib_bounded(&body, &comp_sha, uncomp_bytes, &s_hash, wire::MAX_CHECKPOINT_UNCOMPRESSED_BYTES)
                    .map_err(|_| Error::Storage)?;
                if decoded != world.to_checkpoint() {
                    return Err(Error::Storage);
                }
            }
        }
        if count != revision || !ckpts.is_empty() || chain != meta.chain_hash {
            return Err(Error::Storage);
        }
        if world.import_origin.is_some() {
            world.validate().map_err(|_| Error::Storage)?;
        }
        media::verify_world(db, &world)?;

        let ckpt = world.to_checkpoint();
        if world.name != meta.name
            || world.schema != meta.schema_ver
            || world.metadata_revision != meta.metadata_revision
            || ckpt.strokes.len() as u64 != meta.stroke_count
            || ckpt.points as u64 != meta.point_count
            || ckpt.media.len() as u64 != meta.media_count
            || ckpt.media_frames as u64 != meta.media_frame_count
            || ckpt.tour_revisions.len() as u64 != meta.tour_count
        {
            return Err(Error::Storage);
        }

        verify_canvas_indexes(db, id, &ckpt)?;
    }
    Ok(())
}

fn verify_canvas_indexes(db: &Connection, canvas: CanvasId, ckpt: &WorldCheckpoint) -> Result<()> {
    // 1. world_devices
    let mut dstmt = db.prepare(
        "SELECT CAST(substr(CAST(device AS BLOB),1,33) AS TEXT),CAST(substr(CAST(author AS BLOB),1,33) AS TEXT),last_sequence \
         FROM world_devices WHERE canvas=?1",
    )?;
    let mut drows = dstmt.query([canvas.as_str()])?;
    let mut dcount = 0usize;
    while let Some(r) = drows.next()? {
        let dev = DeviceId::new(&r.get::<_, String>(0)?).map_err(|_| Error::Storage)?;
        let author = OwnerId::new(&r.get::<_, String>(1)?).map_err(|_| Error::Storage)?;
        let seq = read_u64(r, 2)?;
        if ckpt.sequences.get(&dev) != Some(&seq) || ckpt.device_authors.get(&dev) != Some(&author) {
            return Err(Error::Storage);
        }
        dcount += 1;
    }
    if dcount != ckpt.sequences.len() {
        return Err(Error::Storage);
    }

    // 2. world_strokes_idx
    let mut sstmt = db.prepare(
        "SELECT CAST(substr(CAST(stroke AS BLOB),1,33) AS TEXT),CAST(substr(CAST(owner AS BLOB),1,33) AS TEXT),\
                CAST(substr(CAST(device AS BLOB),1,33) AS TEXT),points,visible,created_revision,changed_revision \
         FROM world_strokes_idx WHERE canvas=?1",
    )?;
    let mut srows = sstmt.query([canvas.as_str()])?;
    let mut scount = 0usize;
    while let Some(r) = srows.next()? {
        let sid = photocraft_scene::StrokeId::new(&r.get::<_, String>(0)?).map_err(|_| Error::Storage)?;
        let owner: String = r.get(1)?;
        let dev: String = r.get(2)?;
        let pts = read_u64(r, 3)?;
        let vis: i64 = r.get(4)?;
        let created = read_u64(r, 5)?;
        let changed = read_u64(r, 6)?;
        let Some(shared) = ckpt.strokes.get(&sid) else {
            return Err(Error::Storage);
        };
        if shared.stroke.owner.as_str() != owner
            || shared.device.as_str() != dev
            || shared.stroke.points.len() as u64 != pts
            || i64::from(shared.visible) != vis
            || shared.created_revision != created
            || shared.changed_revision != changed
        {
            return Err(Error::Storage);
        }
        scount += 1;
    }
    if scount != ckpt.strokes.len() {
        return Err(Error::Storage);
    }

    // 3. world_media_idx
    let mut mstmt = db.prepare(
        "SELECT CAST(substr(CAST(media AS BLOB),1,33) AS TEXT),CAST(substr(CAST(owner AS BLOB),1,33) AS TEXT),\
                CAST(substr(CAST(device AS BLOB),1,33) AS TEXT),frame_count,visible,created_revision,changed_revision \
         FROM world_media_idx WHERE canvas=?1",
    )?;
    let mut mrows = mstmt.query([canvas.as_str()])?;
    let mut mcount = 0usize;
    while let Some(r) = mrows.next()? {
        let mid = photocraft_scene::shared::MediaId::new(&r.get::<_, String>(0)?).map_err(|_| Error::Storage)?;
        let owner: String = r.get(1)?;
        let dev: String = r.get(2)?;
        let frames = read_u64(r, 3)?;
        let vis: i64 = r.get(4)?;
        let created = read_u64(r, 5)?;
        let changed = read_u64(r, 6)?;
        let Some(shared) = ckpt.media.get(&mid) else {
            return Err(Error::Storage);
        };
        if shared.media.owner.as_str() != owner
            || shared.device.as_str() != dev
            || shared.media.frames.len() as u64 != frames
            || i64::from(shared.visible) != vis
            || shared.created_revision != created
            || shared.changed_revision != changed
        {
            return Err(Error::Storage);
        }
        mcount += 1;
    }
    if mcount != ckpt.media.len() {
        return Err(Error::Storage);
    }

    // 4. world_tours_idx
    let mut tstmt = db.prepare("SELECT CAST(substr(CAST(tour AS BLOB),1,33) AS TEXT),revision,present FROM world_tours_idx WHERE canvas=?1")?;
    let mut trows = tstmt.query([canvas.as_str()])?;
    let mut tcount = 0usize;
    while let Some(r) = trows.next()? {
        let tid = photocraft_scene::shared::TourId::new(&r.get::<_, String>(0)?).map_err(|_| Error::Storage)?;
        let rev = read_u64(r, 1)?;
        let present: i64 = r.get(2)?;
        if ckpt.tour_revisions.get(&tid) != Some(&rev) || i64::from(ckpt.tours.contains_key(&tid)) != present {
            return Err(Error::Storage);
        }
        tcount += 1;
    }
    if tcount != ckpt.tour_revisions.len() {
        return Err(Error::Storage);
    }
    Ok(())
}
