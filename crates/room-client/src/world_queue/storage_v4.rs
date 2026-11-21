//! Incremental client world storage (`WorldOutbox SCHEMA = 4`):
//! O(1) incremental accepted-event persistence, fast local checkpoints,
//! O(rows) offline directory summaries, and pinned resumable checkpoint transfers.
use std::io::{Read, Write};

use photocraft_scene::shared::media::AssetHash;
use photocraft_scene::shared::{
    AcceptedOp, Edit, ImportOrigin, Limits as ReducerLimits, MAX_EVENT_PAGE, SHARED_SCHEMA, SHARED_SCHEMA_BRUSH, WorldCheckpoint, WorldState, chain_hash_init,
    chain_hash_step,
};
use photocraft_scene::world_protocol as wire;
use photocraft_scene::{CanvasId, OwnerId};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};

use super::{Activation, CachedCanvasPage, MAX_DIRECTORY, MAX_PAGE_BYTES, MAX_SNAPSHOT_BYTES, checksum, hash, integer, invalid, read_u64, sql};
use crate::Result;

pub(super) const LOCAL_CHECKPOINT_STRIDE: u64 = 512;
const EVENT_ZLIB_MAGIC: &[u8; 4] = b"KZ1\0";
const EVENT_COMPRESS_MIN_BYTES: usize = 384;
const MAX_EVENT_BYTES: usize = wire::MAX_REQUEST_BYTES + 1024;
const MAX_LIMITS_JSON_BYTES: usize = 4096;
const MAX_IMPORT_ORIGIN_BYTES: usize = 65_536;

pub(super) const TABLES: &str = "
CREATE TABLE IF NOT EXISTS canvas_heads(
    scope TEXT NOT NULL,
    canvas TEXT NOT NULL,
    owner TEXT NOT NULL,
    name TEXT NOT NULL,
    initial_name TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision >= 0),
    base_revision INTEGER NOT NULL CHECK(base_revision >= 0 AND base_revision <= revision),
    schema_ver INTEGER NOT NULL CHECK(schema_ver IN (1, 2)),
    chain_hash TEXT NOT NULL,
    limits_json BLOB NOT NULL,
    import_origin_json BLOB,
    hash TEXT NOT NULL,
    PRIMARY KEY(scope, canvas)
);
CREATE TABLE IF NOT EXISTS accepted_events(
    scope TEXT NOT NULL,
    canvas TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    op_id TEXT NOT NULL,
    body BLOB NOT NULL,
    event_hash TEXT NOT NULL,
    hash TEXT NOT NULL,
    PRIMARY KEY(scope, canvas, revision),
    UNIQUE(scope, canvas, op_id)
);
CREATE TABLE IF NOT EXISTS canvas_checkpoints(
    scope TEXT NOT NULL,
    canvas TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    schema_ver INTEGER NOT NULL CHECK(schema_ver IN (1, 2)),
    chain_hash TEXT NOT NULL,
    state_hash TEXT NOT NULL,
    compressed_sha256 TEXT NOT NULL,
    compressed_bytes INTEGER NOT NULL CHECK(compressed_bytes > 0),
    uncompressed_bytes INTEGER NOT NULL CHECK(uncompressed_bytes > 0),
    body_zlib BLOB NOT NULL,
    hash TEXT NOT NULL,
    PRIMARY KEY(scope, canvas, revision)
);
CREATE TABLE IF NOT EXISTS checkpoint_staging(
    scope TEXT NOT NULL,
    canvas TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    schema_ver INTEGER NOT NULL CHECK(schema_ver IN (1, 2)),
    chain_hash TEXT NOT NULL,
    state_hash TEXT NOT NULL,
    compressed_sha256 TEXT NOT NULL,
    compressed_bytes INTEGER NOT NULL CHECK(compressed_bytes > 0),
    uncompressed_bytes INTEGER NOT NULL CHECK(uncompressed_bytes > 0),
    received_bytes INTEGER NOT NULL CHECK(received_bytes >= 0 AND received_bytes <= compressed_bytes),
    PRIMARY KEY(scope, canvas)
);
CREATE TABLE IF NOT EXISTS checkpoint_staging_chunks(
    scope TEXT NOT NULL,
    canvas TEXT NOT NULL,
    offset INTEGER NOT NULL CHECK(offset >= 0),
    data BLOB NOT NULL,
    PRIMARY KEY(scope, canvas, offset)
);
";

fn is_hex64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn encode_event_body(json: &[u8]) -> Result<Vec<u8>> {
    if json.len() <= EVENT_COMPRESS_MIN_BYTES {
        return Ok(json.to_vec());
    }
    let mut out = Vec::with_capacity(json.len().min(4096) + EVENT_ZLIB_MAGIC.len());
    out.extend_from_slice(EVENT_ZLIB_MAGIC);
    let mut enc = flate2::write::ZlibEncoder::new(out, flate2::Compression::new(5));
    enc.write_all(json).map_err(|_| invalid("Canvas event compression failed"))?;
    let compressed = enc.finish().map_err(|_| invalid("Canvas event compression failed"))?;
    if compressed.len() < json.len() { Ok(compressed) } else { Ok(json.to_vec()) }
}

fn decode_event_body(raw: &[u8]) -> Result<Vec<u8>> {
    if raw.len() > MAX_EVENT_BYTES {
        return Err(invalid("Saved canvas event exceeds limit; preserve recovery data"));
    }
    if let Some(payload) = raw.strip_prefix(EVENT_ZLIB_MAGIC) {
        let mut dec = flate2::read::ZlibDecoder::new(payload).take((MAX_EVENT_BYTES as u64).saturating_add(1));
        let mut json = Vec::new();
        dec.read_to_end(&mut json).map_err(|_| invalid("Saved canvas event decompression failed; preserve recovery data"))?;
        if json.len() > MAX_EVENT_BYTES {
            return Err(invalid("Saved canvas event exceeds limit; preserve recovery data"));
        }
        Ok(json)
    } else {
        Ok(raw.to_vec())
    }
}

#[derive(Clone, Debug)]
pub(super) struct CanvasHead {
    pub(super) canvas: CanvasId,
    pub(super) owner: OwnerId,
    pub(super) name: String,
    pub(super) initial_name: String,
    pub(super) revision: u64,
    pub(super) base_revision: u64,
    pub(super) schema_ver: u32,
    pub(super) chain_hash: String,
    pub(super) limits: ReducerLimits,
    pub(super) import_origin: Option<Box<ImportOrigin>>,
}

#[allow(clippy::too_many_arguments)]
fn head_checksum(
    scope: &str,
    canvas: CanvasId,
    owner: OwnerId,
    name: &str,
    initial_name: &str,
    revision: u64,
    base_revision: u64,
    schema_ver: u32,
    chain_hash: &str,
    limits_json: &[u8],
    import_origin_json: Option<&[u8]>,
) -> String {
    let mut h = Sha256::new();
    h.update(b"komaphone-head-v4:");
    h.update(scope.as_bytes());
    h.update([0]);
    h.update(canvas.as_str().as_bytes());
    h.update([0]);
    h.update(owner.as_str().as_bytes());
    h.update([0]);
    h.update((name.len() as u64).to_be_bytes());
    h.update(name.as_bytes());
    h.update((initial_name.len() as u64).to_be_bytes());
    h.update(initial_name.as_bytes());
    h.update(revision.to_be_bytes());
    h.update(base_revision.to_be_bytes());
    h.update(schema_ver.to_be_bytes());
    h.update(chain_hash.as_bytes());
    h.update((limits_json.len() as u64).to_be_bytes());
    h.update(limits_json);
    match import_origin_json {
        Some(bytes) => {
            h.update([1]);
            h.update((bytes.len() as u64).to_be_bytes());
            h.update(bytes);
        }
        None => h.update([0]),
    }
    format!("{:x}", h.finalize())
}

fn write_head(db: &Connection, scope: &str, head: &CanvasHead) -> Result<()> {
    let limits_json = serde_json::to_vec(&head.limits).map_err(|_| invalid("Canvas limits encoding"))?;
    if limits_json.len() > MAX_LIMITS_JSON_BYTES {
        return Err(invalid("Canvas limits exceed size bound"));
    }
    let import_origin_json =
        head.import_origin.as_deref().map(|origin| serde_json::to_vec(origin).map_err(|_| invalid("Canvas import origin encoding"))).transpose()?;
    if import_origin_json.as_ref().is_some_and(|b| b.len() > MAX_IMPORT_ORIGIN_BYTES) {
        return Err(invalid("Canvas import origin exceeds size bound"));
    }
    let digest = head_checksum(
        scope,
        head.canvas,
        head.owner,
        &head.name,
        &head.initial_name,
        head.revision,
        head.base_revision,
        head.schema_ver,
        &head.chain_hash,
        &limits_json,
        import_origin_json.as_deref(),
    );
    db.execute(
        "INSERT INTO canvas_heads(scope,canvas,owner,name,initial_name,revision,base_revision,schema_ver,chain_hash,limits_json,import_origin_json,hash) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12) \
         ON CONFLICT(scope,canvas) DO UPDATE SET \
             owner=excluded.owner,name=excluded.name,initial_name=excluded.initial_name,\
             revision=excluded.revision,base_revision=excluded.base_revision,schema_ver=excluded.schema_ver,\
             chain_hash=excluded.chain_hash,limits_json=excluded.limits_json,\
             import_origin_json=excluded.import_origin_json,hash=excluded.hash",
        params![
            scope,
            head.canvas.as_str(),
            head.owner.as_str(),
            &head.name,
            &head.initial_name,
            integer(head.revision)?,
            integer(head.base_revision)?,
            head.schema_ver,
            &head.chain_hash,
            limits_json,
            import_origin_json,
            digest,
        ],
    )
    .map_err(sql)?;
    Ok(())
}

pub(super) fn read_head(db: &Connection, scope: &str, canvas: CanvasId) -> Result<Option<CanvasHead>> {
    type RawHead = (String, String, String, u64, u64, u32, String, Vec<u8>, Option<Vec<u8>>, String);
    let row: Option<RawHead> = db
        .query_row(
            "SELECT CAST(substr(CAST(owner AS BLOB),1,33) AS TEXT),\
                    CAST(substr(CAST(name AS BLOB),1,97) AS TEXT),\
                    CAST(substr(CAST(initial_name AS BLOB),1,97) AS TEXT),\
                    revision,base_revision,schema_ver,\
                    CAST(substr(CAST(chain_hash AS BLOB),1,65) AS TEXT),\
                    substr(CAST(limits_json AS BLOB),1,?3),\
                    CASE WHEN import_origin_json IS NULL THEN NULL ELSE substr(CAST(import_origin_json AS BLOB),1,?4) END,\
                    CAST(substr(CAST(hash AS BLOB),1,65) AS TEXT) \
             FROM canvas_heads WHERE scope=?1 AND canvas=?2",
            params![scope, canvas.as_str(), (MAX_LIMITS_JSON_BYTES + 1) as i64, (MAX_IMPORT_ORIGIN_BYTES + 1) as i64],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, read_u64(r, 3)?, read_u64(r, 4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?)),
        )
        .optional()
        .map_err(sql)?;
    let Some((owner_s, name, initial_name, revision, base_revision, schema_ver, chain_hash, limits_json, import_origin_json, saved_hash)) = row else {
        return Ok(None);
    };
    let owner = OwnerId::new(&owner_s).map_err(|_| invalid("Cached canvas owner identity"))?;
    if limits_json.len() > MAX_LIMITS_JSON_BYTES
        || import_origin_json.as_ref().is_some_and(|b| b.len() > MAX_IMPORT_ORIGIN_BYTES)
        || !is_hex64(&chain_hash)
        || base_revision > revision
    {
        return Err(invalid("Cached canvas header bounds failed; preserve recovery data"));
    }
    let expected = head_checksum(
        scope,
        canvas,
        owner,
        &name,
        &initial_name,
        revision,
        base_revision,
        schema_ver,
        &chain_hash,
        &limits_json,
        import_origin_json.as_deref(),
    );
    if saved_hash != expected {
        return Err(invalid("Cached canvas checksum failed; preserve recovery data"));
    }
    photocraft_scene::validate_name(&name).map_err(|_| invalid("Cached canvas name"))?;
    photocraft_scene::validate_name(&initial_name).map_err(|_| invalid("Cached canvas initial name"))?;
    if schema_ver != SHARED_SCHEMA && schema_ver != SHARED_SCHEMA_BRUSH {
        return Err(invalid("Unsupported cached canvas schema"));
    }
    let limits: ReducerLimits = serde_json::from_slice(&limits_json).map_err(|_| invalid("Cached canvas limits are invalid"))?;
    limits.validate().map_err(|_| invalid("Cached canvas limits are invalid"))?;
    let import_origin = import_origin_json
        .as_deref()
        .map(|bytes| {
            let origin: ImportOrigin = serde_json::from_slice(bytes).map_err(|_| invalid("Cached archive origin is invalid"))?;
            origin.validate(canvas, owner).map_err(|_| invalid("Cached archive origin"))?;
            Ok(Box::new(origin))
        })
        .transpose()?;
    Ok(Some(CanvasHead { canvas, owner, name, initial_name, revision, base_revision, schema_ver, chain_hash, limits, import_origin }))
}

fn insert_accepted_event(db: &Connection, scope: &str, canvas: CanvasId, event: &AcceptedOp) -> Result<String> {
    let json = serde_json::to_vec(event).map_err(|_| invalid("Canvas event encoding"))?;
    if json.len() > MAX_EVENT_BYTES {
        return Err(invalid("Canvas event exceeds limit"));
    }
    let ev_hash = hash(&json);
    let row_hash = checksum(scope, canvas, &json);
    let stored = encode_event_body(&json)?;
    db.execute(
        "INSERT INTO accepted_events(scope,canvas,revision,op_id,body,event_hash,hash) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![scope, canvas.as_str(), integer(event.revision)?, event.op.id.as_str(), stored, &ev_hash, row_hash],
    )
    .map_err(sql)?;
    Ok(ev_hash)
}

fn read_accepted_event_row(scope: &str, canvas: CanvasId, expected_rev: u64, raw: &[u8], ev_hash: &str, row_hash: &str) -> Result<(AcceptedOp, String)> {
    let json = decode_event_body(raw)?;
    if !is_hex64(ev_hash) || hash(&json) != ev_hash || checksum(scope, canvas, &json) != row_hash {
        return Err(invalid("Cached canvas event checksum failed; preserve recovery data"));
    }
    let event: AcceptedOp = serde_json::from_slice(&json).map_err(|_| invalid("Saved canvas event is invalid"))?;
    if event.revision != expected_rev || event.op.canvas != canvas {
        return Err(invalid("Saved canvas event does not match its row"));
    }
    event.op.validate().map_err(|e| invalid(&e.to_string()))?;
    Ok((event, ev_hash.to_owned()))
}

/// Pre-compressed local checkpoint ready for fast atomic publication on the `WorldWorker` thread.
pub struct PreparedLocalCheckpoint {
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

/// Pure lock-free compression helper run off the `WorldWorker` thread.
pub(super) fn prepare_local_checkpoint(canvas: CanvasId, ckpt: &WorldCheckpoint, chain_hash: String) -> Result<PreparedLocalCheckpoint> {
    if ckpt.revision == 0 || !is_hex64(&chain_hash) || ckpt.canvas != canvas {
        return Err(invalid("Invalid local checkpoint parameters"));
    }
    let (body_zlib, compressed_sha256, state_hash, uncompressed_bytes) = ckpt.encode_zlib().map_err(|e| invalid(&e.to_string()))?;
    let compressed_bytes = body_zlib.len() as u64;
    if compressed_bytes == 0
        || compressed_bytes > wire::MAX_CHECKPOINT_COMPRESSED_BYTES
        || uncompressed_bytes == 0
        || uncompressed_bytes > wire::MAX_CHECKPOINT_UNCOMPRESSED_BYTES
    {
        return Err(invalid("Local canvas checkpoint exceeds size limit"));
    }
    Ok(PreparedLocalCheckpoint {
        canvas,
        revision: ckpt.revision,
        schema_ver: ckpt.schema,
        chain_hash,
        state_hash,
        compressed_sha256,
        compressed_bytes,
        uncompressed_bytes,
        body_zlib,
    })
}

/// Fast atomic persistence of a pre-compressed local checkpoint under the active `Activation` lease.
pub(super) fn store_prepared_local_checkpoint(tx: &Transaction<'_>, lease: &Activation, prepared: &PreparedLocalCheckpoint) -> Result<bool> {
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
        return Err(invalid("Prepared local canvas checkpoint is invalid"));
    }
    let Some(head) = read_head(tx, &lease.key, prepared.canvas)? else {
        return Ok(false);
    };
    if head.revision < prepared.revision {
        return Ok(false);
    }
    if head.revision == prepared.revision && head.chain_hash != prepared.chain_hash {
        return Ok(false);
    }
    let last_ckpt = latest_local_checkpoint_rev(tx, &lease.key, prepared.canvas)?;
    if last_ckpt >= prepared.revision {
        return Ok(false);
    }
    let row_hash = checksum(&lease.key, prepared.canvas, &prepared.body_zlib);
    tx.execute(
        "INSERT OR REPLACE INTO canvas_checkpoints(scope,canvas,revision,schema_ver,chain_hash,state_hash,compressed_sha256,compressed_bytes,uncompressed_bytes,body_zlib,hash) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            lease.key,
            prepared.canvas.as_str(),
            integer(prepared.revision)?,
            prepared.schema_ver,
            &prepared.chain_hash,
            &prepared.state_hash,
            &prepared.compressed_sha256,
            integer(prepared.compressed_bytes)?,
            integer(prepared.uncompressed_bytes)?,
            &prepared.body_zlib,
            row_hash,
        ],
    )
    .map_err(sql)?;
    tx.execute(
        "DELETE FROM canvas_checkpoints WHERE scope=?1 AND canvas=?2 AND revision NOT IN (\
             SELECT revision FROM canvas_checkpoints WHERE scope=?1 AND canvas=?2 ORDER BY revision DESC LIMIT 1\
         )",
        params![lease.key, prepared.canvas.as_str()],
    )
    .map_err(sql)?;
    Ok(true)
}

fn store_local_checkpoint(db: &Connection, scope: &str, canvas: CanvasId, ckpt: &WorldCheckpoint, chain_hash: &str) -> Result<()> {
    if ckpt.revision == 0 {
        return Ok(());
    }
    let (body_zlib, compressed_sha256, state_hash, uncompressed_bytes) = ckpt.encode_zlib().map_err(|e| invalid(&e.to_string()))?;
    let compressed_bytes = body_zlib.len() as u64;
    if compressed_bytes == 0
        || compressed_bytes > wire::MAX_CHECKPOINT_COMPRESSED_BYTES
        || uncompressed_bytes == 0
        || uncompressed_bytes > wire::MAX_CHECKPOINT_UNCOMPRESSED_BYTES
    {
        return Err(invalid("Local canvas checkpoint exceeds size limit"));
    }
    let row_hash = checksum(scope, canvas, &body_zlib);
    db.execute(
        "INSERT OR REPLACE INTO canvas_checkpoints(scope,canvas,revision,schema_ver,chain_hash,state_hash,compressed_sha256,compressed_bytes,uncompressed_bytes,body_zlib,hash) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            scope,
            canvas.as_str(),
            integer(ckpt.revision)?,
            ckpt.schema,
            chain_hash,
            state_hash,
            compressed_sha256,
            integer(compressed_bytes)?,
            integer(uncompressed_bytes)?,
            body_zlib,
            row_hash,
        ],
    )
    .map_err(sql)?;
    db.execute(
        "DELETE FROM canvas_checkpoints WHERE scope=?1 AND canvas=?2 AND revision NOT IN (\
             SELECT revision FROM canvas_checkpoints WHERE scope=?1 AND canvas=?2 ORDER BY revision DESC LIMIT 1\
         )",
        params![scope, canvas.as_str()],
    )
    .map_err(sql)?;
    Ok(())
}

fn load_latest_local_checkpoint(db: &Connection, scope: &str, canvas: CanvasId, max_revision: u64) -> Result<Option<(WorldCheckpoint, String)>> {
    type RawCkpt = (u64, u32, String, String, String, u64, u64, Vec<u8>, String);
    let row: Option<RawCkpt> = db
        .query_row(
            "SELECT revision,schema_ver,\
                    CAST(substr(CAST(chain_hash AS BLOB),1,65) AS TEXT),\
                    CAST(substr(CAST(state_hash AS BLOB),1,65) AS TEXT),\
                    CAST(substr(CAST(compressed_sha256 AS BLOB),1,65) AS TEXT),\
                    compressed_bytes,uncompressed_bytes,\
                    substr(CAST(body_zlib AS BLOB),1,?4),\
                    CAST(substr(CAST(hash AS BLOB),1,65) AS TEXT) \
             FROM canvas_checkpoints WHERE scope=?1 AND canvas=?2 AND revision<=?3 ORDER BY revision DESC LIMIT 1",
            params![scope, canvas.as_str(), integer(max_revision)?, (wire::MAX_CHECKPOINT_COMPRESSED_BYTES + 1) as i64],
            |r| Ok((read_u64(r, 0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, read_u64(r, 5)?, read_u64(r, 6)?, r.get(7)?, r.get(8)?)),
        )
        .optional()
        .map_err(sql)?;
    let Some((rev, schema_ver, chain_hash, state_hash, comp_sha, comp_bytes, uncomp_bytes, body, row_hash)) = row else {
        return Ok(None);
    };
    if !is_hex64(&chain_hash)
        || !is_hex64(&state_hash)
        || !is_hex64(&comp_sha)
        || body.len() as u64 != comp_bytes
        || comp_bytes > wire::MAX_CHECKPOINT_COMPRESSED_BYTES
        || uncomp_bytes > wire::MAX_CHECKPOINT_UNCOMPRESSED_BYTES
        || checksum(scope, canvas, &body) != row_hash
    {
        return Err(invalid("Canvas checkpoint checksum failed; preserve recovery data"));
    }
    let ckpt = WorldCheckpoint::decode_zlib_bounded(&body, &comp_sha, uncomp_bytes, &state_hash, wire::MAX_CHECKPOINT_UNCOMPRESSED_BYTES)
        .map_err(|_| invalid("Saved canvas checkpoint is invalid; preserve recovery data"))?;
    if ckpt.canvas != canvas || ckpt.revision != rev || ckpt.schema != schema_ver {
        return Err(invalid("Saved canvas checkpoint metadata mismatch; preserve recovery data"));
    }
    Ok(Some((ckpt, chain_hash)))
}

fn latest_local_checkpoint_rev(db: &Connection, scope: &str, canvas: CanvasId) -> Result<u64> {
    let rev: Option<u64> = db
        .query_row("SELECT max(revision) FROM canvas_checkpoints WHERE scope=?1 AND canvas=?2", params![scope, canvas.as_str()], |r| {
            let v: Option<i64> = r.get(0)?;
            match v {
                Some(n) => u64::try_from(n).map(Some).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, n)),
                None => Ok(None),
            }
        })
        .map_err(sql)?;
    Ok(rev.unwrap_or(0))
}

/// Migrate any legacy `snapshots` rows (Schema 1/2/3) into `canvas_heads` + `accepted_events` (+ `canvas_checkpoints`).
/// Preserves `grants`, `operations`, `viewers`, `scope_moves`, and `activation` untouched.
pub(super) fn migrate_from_legacy_snapshots(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(TABLES).map_err(sql)?;
    let has_snapshots: bool =
        tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='snapshots')", [], |r| r.get(0)).map_err(sql)?;
    if !has_snapshots {
        return Ok(());
    }
    let mut rows_to_migrate = Vec::new();
    {
        let mut stmt = tx.prepare("SELECT scope,canvas,revision,substr(body,1,?1),hash FROM snapshots ORDER BY scope,canvas").map_err(sql)?;
        let mut rows = stmt.query([(MAX_SNAPSHOT_BYTES + 1) as i64]).map_err(sql)?;
        while let Some(row) = rows.next().map_err(sql)? {
            let scope: String = row.get(0).map_err(sql)?;
            let canvas_s: String = row.get(1).map_err(sql)?;
            let revision = read_u64(row, 2).map_err(sql)?;
            let body: Vec<u8> = row.get(3).map_err(sql)?;
            let digest: String = row.get(4).map_err(sql)?;
            rows_to_migrate.push((scope, canvas_s, revision, body, digest));
        }
    }
    for (scope, canvas_s, revision, body, digest) in rows_to_migrate {
        let canvas = CanvasId::new(&canvas_s).map_err(|_| invalid("Legacy canvas snapshot identity"))?;
        if body.len() > MAX_SNAPSHOT_BYTES || checksum(&scope, canvas, &body) != digest {
            return Err(invalid("Legacy canvas snapshot checksum failed; preserve recovery data"));
        }
        let state: WorldState = serde_json::from_slice(&body).map_err(|_| invalid("Legacy canvas snapshot is invalid"))?;
        state.validate().map_err(|e| invalid(&e.to_string()))?;
        if state.canvas != canvas || state.revision != revision {
            return Err(invalid("Legacy canvas snapshot revision differs from its row"));
        }
        let initial_name = state.initial_name().to_owned();
        let mut chain = chain_hash_init(canvas, state.owner, &initial_name);
        let mut after = 0_u64;
        loop {
            let page = state.events_after(after, MAX_EVENT_PAGE).map_err(|e| invalid(&e.to_string()))?;
            if page.is_empty() {
                break;
            }
            for event in page {
                let ev_hash = insert_accepted_event(tx, &scope, canvas, event)?;
                chain = chain_hash_step(&chain, event.revision, &ev_hash);
                after = event.revision;
            }
        }
        let head = CanvasHead {
            canvas,
            owner: state.owner,
            name: state.name.clone(),
            initial_name,
            revision: state.revision,
            base_revision: 0,
            schema_ver: state.schema,
            chain_hash: chain.clone(),
            limits: state.limits(),
            import_origin: state.import_origin.clone(),
        };
        write_head(tx, &scope, &head)?;
        if state.revision >= LOCAL_CHECKPOINT_STRIDE {
            store_local_checkpoint(tx, &scope, canvas, &state.to_checkpoint(), &chain)?;
        }
    }
    tx.execute("DELETE FROM snapshots", []).map_err(sql)?;
    Ok(())
}

/// Reconstruct `WorldState` from `canvas_heads` + latest `canvas_checkpoints` (when applicable) + tail `accepted_events`.
pub(super) fn read_world(db: &Connection, scope: &str, canvas: CanvasId) -> Result<Option<WorldState>> {
    let Some(head) = read_head(db, scope, canvas)? else {
        return Ok(None);
    };
    let (mut world, mut chain, mut after) = if let Some((ckpt, ckpt_chain)) = load_latest_local_checkpoint(db, scope, canvas, head.revision)? {
        if ckpt.revision < head.base_revision || ckpt.owner != head.owner || ckpt.initial_name != head.initial_name || ckpt.import_origin != head.import_origin
        {
            return Err(invalid("Cached canvas checkpoint disagrees with head; preserve recovery data"));
        }
        // Only start from checkpoint if either we don't have full history before it (`base_revision > 0`)
        // OR the canvas is large enough (`revision >= LOCAL_CHECKPOINT_STRIDE`) that checkpoint fast-load is intended.
        if head.base_revision > 0 || head.revision >= LOCAL_CHECKPOINT_STRIDE {
            let rev = ckpt.revision;
            (WorldState::from_checkpoint(ckpt).map_err(|e| invalid(&e.to_string()))?, ckpt_chain, rev)
        } else {
            let mut w = WorldState::with_limits(canvas, head.owner, head.initial_name.clone(), head.limits).map_err(|e| invalid(&e.to_string()))?;
            w.import_origin = head.import_origin.clone();
            let c = chain_hash_init(canvas, head.owner, &head.initial_name);
            (w, c, 0)
        }
    } else {
        if head.base_revision > 0 {
            return Err(invalid("Missing cached canvas checkpoint; preserve recovery data"));
        }
        let mut w = WorldState::with_limits(canvas, head.owner, head.initial_name.clone(), head.limits).map_err(|e| invalid(&e.to_string()))?;
        w.import_origin = head.import_origin.clone();
        let c = chain_hash_init(canvas, head.owner, &head.initial_name);
        (w, c, 0)
    };

    let mut stmt = db
        .prepare(
            "SELECT revision,substr(CAST(body AS BLOB),1,?4),CAST(substr(CAST(event_hash AS BLOB),1,65) AS TEXT),CAST(substr(CAST(hash AS BLOB),1,65) AS TEXT) \
             FROM accepted_events WHERE scope=?1 AND canvas=?2 AND revision>?3 ORDER BY revision",
        )
        .map_err(sql)?;
    let mut rows = stmt.query(params![scope, canvas.as_str(), integer(after)?, (MAX_EVENT_BYTES + 1) as i64]).map_err(sql)?;
    while let Some(row) = rows.next().map_err(sql)? {
        let rev = read_u64(row, 0).map_err(sql)?;
        let expected = after.checked_add(1).ok_or_else(|| invalid("Canvas revision overflow"))?;
        if rev != expected || rev > head.revision {
            return Err(invalid("Cached canvas event sequence gap; preserve recovery data"));
        }
        let raw: Vec<u8> = row.get(1).map_err(sql)?;
        let ev_hash: String = row.get(2).map_err(sql)?;
        let row_hash: String = row.get(3).map_err(sql)?;
        let (event, ev_hash) = read_accepted_event_row(scope, canvas, rev, &raw, &ev_hash, &row_hash)?;
        chain = chain_hash_step(&chain, rev, &ev_hash);
        world.apply(&event).map_err(|e| invalid(&e.to_string()))?;
        after = rev;
    }

    if after != head.revision || world.name != head.name || world.schema != head.schema_ver || chain != head.chain_hash {
        return Err(invalid("Cached canvas history differs from head checksum; preserve recovery data"));
    }
    Ok(Some(world))
}

/// Backward-compatible `save_snapshot` backed by incremental `canvas_heads` + `accepted_events` + `canvas_checkpoints`
/// (no 32 MiB JSON blob).
pub(super) fn save_snapshot_v4(tx: &Transaction<'_>, lease: &Activation, state: &WorldState) -> Result<()> {
    state.validate().map_err(|e| invalid(&e.to_string()))?;
    let initial_name = state.initial_name().to_owned();
    let prior = read_head(tx, &lease.key, state.canvas)?;

    let (mut chain, start_after, base_revision) = if let Some(prior) = prior {
        if prior.revision > state.revision {
            return Err(invalid("Stale canvas snapshot; retain newer local state"));
        }
        if prior.owner != state.owner || prior.initial_name != initial_name || prior.import_origin != state.import_origin {
            return Err(invalid("Canvas snapshot conflicts with its saved immutable history; retain both recovery sources"));
        }
        if state.base_revision() == 0 && prior.base_revision == 0 {
            let mut prefix_chain = chain_hash_init(state.canvas, state.owner, &initial_name);
            let mut cursor = 0_u64;
            while cursor < prior.revision {
                let want = usize::try_from(prior.revision - cursor).unwrap_or(MAX_EVENT_PAGE).min(MAX_EVENT_PAGE);
                let slice = state.events_after(cursor, want).map_err(|e| invalid(&e.to_string()))?;
                if slice.is_empty() {
                    return Err(invalid("Canvas snapshot conflicts with its saved immutable history; retain both recovery sources"));
                }
                for event in slice {
                    let json = serde_json::to_vec(event).map_err(|_| invalid("Canvas event encoding"))?;
                    prefix_chain = chain_hash_step(&prefix_chain, event.revision, &hash(&json));
                    cursor = event.revision;
                }
            }
            if prefix_chain != prior.chain_hash {
                return Err(invalid("Canvas snapshot conflicts with its saved immutable history; retain both recovery sources"));
            }
            (prior.chain_hash, prior.revision, 0)
        } else if state.base_revision() <= prior.revision {
            if state.revision == prior.revision && state.base_revision() == 0 {
                let mut check = chain_hash_init(state.canvas, state.owner, &initial_name);
                let mut c = 0_u64;
                while c < prior.revision {
                    let want = usize::try_from(prior.revision - c).unwrap_or(MAX_EVENT_PAGE).min(MAX_EVENT_PAGE);
                    for ev in state.events_after(c, want).map_err(|e| invalid(&e.to_string()))? {
                        let json = serde_json::to_vec(ev).map_err(|_| invalid("Canvas event encoding"))?;
                        check = chain_hash_step(&check, ev.revision, &hash(&json));
                        c = ev.revision;
                    }
                }
                if check != prior.chain_hash {
                    return Err(invalid("Canvas snapshot conflicts with its saved immutable history; retain both recovery sources"));
                }
            }
            (prior.chain_hash, prior.revision, prior.base_revision)
        } else {
            return Err(invalid("Canvas snapshot base revision jumped past stored history; publish via checkpoint"));
        }
    } else if state.base_revision() == 0 {
        (chain_hash_init(state.canvas, state.owner, &initial_name), 0, 0)
    } else {
        // Fresh checkpoint-backed snapshot without prior head.
        let ckpt = state.to_checkpoint();
        let chain_seed = ckpt.state_hash().map_err(|e| invalid(&e.to_string()))?;
        store_local_checkpoint(tx, &lease.key, state.canvas, &ckpt, &chain_seed)?;
        let head = CanvasHead {
            canvas: state.canvas,
            owner: state.owner,
            name: state.name.clone(),
            initial_name,
            revision: state.revision,
            base_revision: state.revision,
            schema_ver: state.schema,
            chain_hash: chain_seed,
            limits: state.limits(),
            import_origin: state.import_origin.clone(),
        };
        write_head(tx, &lease.key, &head)?;
        return Ok(());
    };

    let mut after = start_after;
    while after < state.revision {
        let slice = state.events_after(after, MAX_EVENT_PAGE).map_err(|e| invalid(&e.to_string()))?;
        if slice.is_empty() {
            return Err(invalid("Canvas snapshot missing tail events"));
        }
        for event in slice {
            let ev_hash = insert_accepted_event(tx, &lease.key, state.canvas, event)?;
            chain = chain_hash_step(&chain, event.revision, &ev_hash);
            after = event.revision;
        }
    }

    let head = CanvasHead {
        canvas: state.canvas,
        owner: state.owner,
        name: state.name.clone(),
        initial_name,
        revision: state.revision,
        base_revision,
        schema_ver: state.schema,
        chain_hash: chain.clone(),
        limits: state.limits(),
        import_origin: state.import_origin.clone(),
    };
    write_head(tx, &lease.key, &head)?;

    let last_ckpt = latest_local_checkpoint_rev(tx, &lease.key, state.canvas)?;
    if state.revision >= LOCAL_CHECKPOINT_STRIDE && state.revision.saturating_sub(last_ckpt) >= LOCAL_CHECKPOINT_STRIDE {
        store_local_checkpoint(tx, &lease.key, state.canvas, &state.to_checkpoint(), &chain)?;
    }
    Ok(())
}

/// O(1) per-event incremental persistence for `WorldWorker` event pages & contiguous ACKs.
/// Never compresses checkpoints synchronously inside the SQLite write transaction; returns
/// `Some((revision, chain_hash))` when a background local checkpoint should be generated off-thread.
pub(super) fn append_accepted_events_v4(
    tx: &Transaction<'_>,
    lease: &Activation,
    info: &wire::CanvasSummary,
    events: &[AcceptedOp],
) -> Result<Option<(u64, String)>> {
    let mut head = match read_head(tx, &lease.key, info.canvas)? {
        Some(existing) => {
            if existing.owner != info.owner || existing.initial_name != info.initial_name || existing.import_origin != info.import_origin {
                return Err(invalid("Canvas identity differs from its saved local head"));
            }
            existing
        }
        None => CanvasHead {
            canvas: info.canvas,
            owner: info.owner,
            name: info.initial_name.clone(),
            initial_name: info.initial_name.clone(),
            revision: 0,
            base_revision: 0,
            schema_ver: SHARED_SCHEMA,
            chain_hash: chain_hash_init(info.canvas, info.owner, &info.initial_name),
            limits: ReducerLimits::default(),
            import_origin: info.import_origin.clone(),
        },
    };

    for event in events {
        if event.op.canvas != info.canvas {
            return Err(invalid("Accepted event belongs to another canvas"));
        }
        event.op.validate().map_err(|e| invalid(&e.to_string()))?;
        if event.revision <= head.revision {
            if event.revision > head.base_revision {
                let row: Option<(Vec<u8>, String, String)> = tx
                    .query_row(
                        "SELECT substr(CAST(body AS BLOB),1,?4),CAST(substr(CAST(event_hash AS BLOB),1,65) AS TEXT),CAST(substr(CAST(hash AS BLOB),1,65) AS TEXT) \
                         FROM accepted_events WHERE scope=?1 AND canvas=?2 AND revision=?3",
                        params![lease.key, info.canvas.as_str(), integer(event.revision)?, (MAX_EVENT_BYTES + 1) as i64],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()
                    .map_err(sql)?;
                let Some((raw, ev_hash, row_hash)) = row else {
                    return Err(invalid("Missing previously accepted local event; preserve recovery data"));
                };
                let (saved, _) = read_accepted_event_row(&lease.key, info.canvas, event.revision, &raw, &ev_hash, &row_hash)?;
                if saved != *event {
                    return Err(invalid("Accepted event conflicts with saved immutable history"));
                }
            }
            continue;
        }
        if event.revision != head.revision.checked_add(1).ok_or_else(|| invalid("Canvas revision overflow"))? {
            return Err(invalid("Non-contiguous canvas event cannot be appended incrementally"));
        }
        let ev_hash = insert_accepted_event(tx, &lease.key, info.canvas, event)?;
        head.chain_hash = chain_hash_step(&head.chain_hash, event.revision, &ev_hash);
        head.revision = event.revision;
        match &event.op.action {
            Edit::Rename { name, .. } => head.name = name.clone(),
            Edit::AddStroke { stroke } | Edit::CopyStroke { stroke, .. } if stroke.uses_brush_v2() => {
                head.schema_ver = head.schema_ver.max(SHARED_SCHEMA_BRUSH);
            }
            _ => {}
        }
    }

    write_head(tx, &lease.key, &head)?;

    if head.revision >= LOCAL_CHECKPOINT_STRIDE {
        let last_ckpt = latest_local_checkpoint_rev(tx, &lease.key, info.canvas)?;
        if head.revision.saturating_sub(last_ckpt) >= LOCAL_CHECKPOINT_STRIDE {
            return Ok(Some((head.revision, head.chain_hash)));
        }
    }
    Ok(None)
}

/// O(1) durable verification for `WorldOutbox::acknowledge` (no 32 MiB snapshot decode!).
pub(super) fn verify_accepted_for_ack(tx: &Transaction<'_>, lease: &Activation, accepted: &AcceptedOp) -> Result<()> {
    let head = read_head(tx, &lease.key, accepted.op.canvas)?
        .ok_or_else(|| invalid("Download and save the accepted canvas revision before clearing its local queue"))?;
    if accepted.revision > head.revision {
        return Err(invalid("Accepted operation is ahead of durable canvas history; retain it until catch-up is saved"));
    }
    if accepted.revision > head.base_revision {
        let row: Option<(Vec<u8>, String, String)> = tx
            .query_row(
                "SELECT substr(CAST(body AS BLOB),1,?5),CAST(substr(CAST(event_hash AS BLOB),1,65) AS TEXT),CAST(substr(CAST(hash AS BLOB),1,65) AS TEXT) \
                 FROM accepted_events WHERE scope=?1 AND canvas=?2 AND revision=?3 AND op_id=?4",
                params![lease.key, accepted.op.canvas.as_str(), integer(accepted.revision)?, accepted.op.id.as_str(), (MAX_EVENT_BYTES + 1) as i64],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(sql)?;
        let Some((raw, ev_hash, row_hash)) = row else {
            return Err(invalid("Accepted operation is ahead of durable canvas history; retain it until catch-up is saved"));
        };
        let (saved, _) = read_accepted_event_row(&lease.key, accepted.op.canvas, accepted.revision, &raw, &ev_hash, &row_hash)?;
        if saved != *accepted {
            return Err(invalid("Service acknowledged different canvas content; keep the queued original"));
        }
        return Ok(());
    }
    // Covered by a published pinned checkpoint at `head.base_revision >= accepted.revision`:
    // verify the checkpoint is durably stored and includes this writer's sequence.
    let (ckpt, _) = load_latest_local_checkpoint(tx, &lease.key, accepted.op.canvas, head.revision)?
        .ok_or_else(|| invalid("Download and save the accepted canvas revision before clearing its local queue"))?;
    if ckpt.revision < accepted.revision
        || ckpt.sequences.get(&accepted.op.device).copied().unwrap_or(0) < accepted.op.sequence
        || ckpt.device_authors.get(&accepted.op.device) != Some(&accepted.op.author)
    {
        return Err(invalid("Accepted operation is ahead of durable canvas history; retain it until catch-up is saved"));
    }
    Ok(())
}

/// O(rows) offline directory summaries directly from `canvas_heads JOIN grants`.
pub(super) fn cached_summaries_v4(db: &Connection, lease: &Activation, after: Option<CanvasId>, limit: usize) -> Result<CachedCanvasPage> {
    if limit == 0 || limit > 128 {
        return Err(invalid("Cached canvas page must contain 1 to 128 rows"));
    }
    let mut query = db
        .prepare(
            "SELECT h.canvas,g.epoch \
             FROM canvas_heads h JOIN grants g ON g.scope=h.scope AND g.canvas=h.canvas \
             WHERE h.scope=?1 AND g.blocked IN(0,2) AND (?2 IS NULL OR h.canvas>?2) \
             ORDER BY h.canvas LIMIT ?3",
        )
        .map_err(sql)?;
    let mut rows = query.query(params![lease.key, after.map(|id| id.as_str().to_owned()), (limit + 1) as i64]).map_err(sql)?;
    let mut items = Vec::new();
    let mut more = false;
    while let Some(row) = rows.next().map_err(sql)? {
        if items.len() >= limit {
            more = true;
            break;
        }
        let canvas = CanvasId::new(&row.get::<_, String>(0).map_err(sql)?).map_err(|_| invalid("Cached canvas identity"))?;
        let epoch = read_u64(row, 1).map_err(sql)?;
        if epoch == 0 {
            return Err(invalid("Cached canvas metadata does not match its authorized row"));
        }
        let head = read_head(db, &lease.key, canvas)?.ok_or_else(|| invalid("Cached canvas header missing"))?;
        if let Some(origin) = &head.import_origin
            && head.revision < origin.source_revision
        {
            return Err(invalid("Cached archive download is incomplete; reconnect to finish it. Saved artwork is preserved"));
        }
        items.push(wire::CanvasSummary {
            canvas,
            owner: head.owner,
            name: head.name,
            initial_name: head.initial_name,
            import_origin: head.import_origin,
            revision: head.revision,
            epoch,
        });
    }
    let next = if more { items.last().map(|item| item.canvas) } else { None };
    Ok(CachedCanvasPage { items, next })
}

/// Begin or resume staging a pinned immutable checkpoint identified by `(revision, compressed_sha256, ...)`.
/// Returns `received_bytes` already durably staged for this exact pinned checkpoint.
pub(super) fn begin_checkpoint_stage(tx: &Transaction<'_>, lease: &Activation, ckpt: &wire::CheckpointRef) -> Result<u64> {
    ckpt.validate().map_err(|e| invalid(&e.to_string()))?;
    type Existing = (u64, u32, String, String, String, u64, u64, u64);
    let existing: Option<Existing> = tx
        .query_row(
            "SELECT revision,schema_ver,chain_hash,state_hash,compressed_sha256,compressed_bytes,uncompressed_bytes,received_bytes \
             FROM checkpoint_staging WHERE scope=?1 AND canvas=?2",
            params![lease.key, ckpt.canvas.as_str()],
            |r| Ok((read_u64(r, 0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, read_u64(r, 5)?, read_u64(r, 6)?, read_u64(r, 7)?)),
        )
        .optional()
        .map_err(sql)?;
    if let Some((rev, schema_ver, chain_hash, state_hash, comp_sha, comp_bytes, uncomp_bytes, received)) = existing
        && rev == ckpt.revision
        && schema_ver == ckpt.schema_ver
        && chain_hash == ckpt.chain_hash.as_str()
        && state_hash == ckpt.state_hash.as_str()
        && comp_sha == ckpt.compressed_sha256.as_str()
        && comp_bytes == ckpt.compressed_bytes
        && uncomp_bytes == ckpt.uncompressed_bytes
        && received <= comp_bytes
    {
        return Ok(received);
    }
    clear_checkpoint_stage(tx, &lease.key, ckpt.canvas)?;
    tx.execute(
        "INSERT INTO checkpoint_staging(scope,canvas,revision,schema_ver,chain_hash,state_hash,compressed_sha256,compressed_bytes,uncompressed_bytes,received_bytes) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,0)",
        params![
            lease.key,
            ckpt.canvas.as_str(),
            integer(ckpt.revision)?,
            ckpt.schema_ver,
            ckpt.chain_hash.as_str(),
            ckpt.state_hash.as_str(),
            ckpt.compressed_sha256.as_str(),
            integer(ckpt.compressed_bytes)?,
            integer(ckpt.uncompressed_bytes)?,
        ],
    )
    .map_err(sql)?;
    Ok(0)
}

/// Stage one chunk for a pinned checkpoint. Enforces exact pin match and idempotent retry semantics.
/// Returns the new durable `received_bytes` offset.
#[allow(clippy::too_many_arguments)]
pub(super) fn stage_checkpoint_chunk(
    tx: &Transaction<'_>,
    lease: &Activation,
    canvas: CanvasId,
    revision: u64,
    compressed_sha256: AssetHash,
    offset: u64,
    bytes: &[u8],
    total: u64,
) -> Result<u64> {
    if bytes.is_empty() || bytes.len() > wire::MAX_CHECKPOINT_CHUNK_BYTES as usize {
        return Err(invalid("Checkpoint chunk size out of bounds"));
    }
    let row: Option<(u64, String, u64, u64)> = tx
        .query_row(
            "SELECT revision,compressed_sha256,compressed_bytes,received_bytes FROM checkpoint_staging WHERE scope=?1 AND canvas=?2",
            params![lease.key, canvas.as_str()],
            |r| Ok((read_u64(r, 0)?, r.get(1)?, read_u64(r, 2)?, read_u64(r, 3)?)),
        )
        .optional()
        .map_err(sql)?;
    let Some((staged_rev, staged_sha, staged_total, received)) = row else {
        return Err(invalid("No active checkpoint stage for this canvas"));
    };
    if staged_rev != revision || staged_sha != compressed_sha256.as_str() || staged_total != total {
        return Err(invalid("Checkpoint chunk does not match pinned transfer identity"));
    }
    let end = offset.checked_add(bytes.len() as u64).filter(|e| *e <= total).ok_or_else(|| invalid("Checkpoint chunk bounds"))?;
    if offset > received {
        return Err(invalid("Out-of-order checkpoint chunk"));
    }
    if offset < received {
        // Idempotent retry: verify exact prior chunk if present at this offset.
        let prior: Option<Vec<u8>> = tx
            .query_row(
                "SELECT substr(CAST(data AS BLOB),1,?4) FROM checkpoint_staging_chunks WHERE scope=?1 AND canvas=?2 AND offset=?3",
                params![lease.key, canvas.as_str(), integer(offset)?, (wire::MAX_CHECKPOINT_CHUNK_BYTES + 1) as i64],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?;
        if end > received || prior.as_deref() != Some(bytes) {
            return Err(invalid("Retried checkpoint chunk disagrees with staged bytes"));
        }
        return Ok(received);
    }
    tx.execute(
        "INSERT INTO checkpoint_staging_chunks(scope,canvas,offset,data) VALUES(?1,?2,?3,?4)",
        params![lease.key, canvas.as_str(), integer(offset)?, bytes],
    )
    .map_err(sql)?;
    tx.execute("UPDATE checkpoint_staging SET received_bytes=?3 WHERE scope=?1 AND canvas=?2", params![lease.key, canvas.as_str(), integer(end)?])
        .map_err(sql)?;
    Ok(end)
}

pub(super) fn clear_checkpoint_stage(db: &Connection, scope: &str, canvas: CanvasId) -> Result<()> {
    db.execute("DELETE FROM checkpoint_staging_chunks WHERE scope=?1 AND canvas=?2", params![scope, canvas.as_str()]).map_err(sql)?;
    db.execute("DELETE FROM checkpoint_staging WHERE scope=?1 AND canvas=?2", params![scope, canvas.as_str()]).map_err(sql)?;
    Ok(())
}

/// Phase 1 of non-blocking staged checkpoint publication: fast read-only verification of the
/// staging header and contiguous assembly of compressed bytes (no decompression under write lock).
pub(super) fn read_staged_checkpoint_bytes(db: &Connection, lease: &Activation, info: &wire::CanvasSummary, ckpt_ref: &wire::CheckpointRef) -> Result<Vec<u8>> {
    ckpt_ref.validate().map_err(|e| invalid(&e.to_string()))?;
    if ckpt_ref.canvas != info.canvas || ckpt_ref.revision > info.revision {
        return Err(invalid("Checkpoint metadata disagrees with authorized canvas summary"));
    }
    verify_staged_header(db, &lease.key, info.canvas, ckpt_ref)?;
    let comp_bytes = ckpt_ref.compressed_bytes;
    let mut compressed = Vec::with_capacity(usize::try_from(comp_bytes).map_err(|_| invalid("Checkpoint size overflow"))?);
    let mut stmt =
        db.prepare("SELECT offset,substr(CAST(data AS BLOB),1,?3) FROM checkpoint_staging_chunks WHERE scope=?1 AND canvas=?2 ORDER BY offset").map_err(sql)?;
    let mut rows = stmt.query(params![lease.key, info.canvas.as_str(), (wire::MAX_CHECKPOINT_CHUNK_BYTES + 1) as i64]).map_err(sql)?;
    while let Some(row) = rows.next().map_err(sql)? {
        let off = read_u64(row, 0).map_err(sql)?;
        let data: Vec<u8> = row.get(1).map_err(sql)?;
        if off != compressed.len() as u64 || compressed.len().saturating_add(data.len()) as u64 > comp_bytes {
            return Err(invalid("Staged checkpoint chunks are not contiguous"));
        }
        compressed.extend_from_slice(&data);
    }
    if compressed.len() as u64 != comp_bytes {
        return Err(invalid("Staged checkpoint byte length mismatch"));
    }
    Ok(compressed)
}

fn verify_staged_header(db: &Connection, scope: &str, canvas: CanvasId, ckpt_ref: &wire::CheckpointRef) -> Result<()> {
    struct StagedCheckpointHeader {
        rev: u64,
        schema_ver: u32,
        chain_hash: String,
        state_hash: String,
        comp_sha: String,
        comp_bytes: u64,
        uncomp_bytes: u64,
        received: u64,
    }
    let staged: Option<StagedCheckpointHeader> = db
        .query_row(
            "SELECT revision,schema_ver,chain_hash,state_hash,compressed_sha256,compressed_bytes,uncompressed_bytes,received_bytes \
             FROM checkpoint_staging WHERE scope=?1 AND canvas=?2",
            params![scope, canvas.as_str()],
            |r| {
                Ok(StagedCheckpointHeader {
                    rev: read_u64(r, 0)?,
                    schema_ver: r.get(1)?,
                    chain_hash: r.get(2)?,
                    state_hash: r.get(3)?,
                    comp_sha: r.get(4)?,
                    comp_bytes: read_u64(r, 5)?,
                    uncomp_bytes: read_u64(r, 6)?,
                    received: read_u64(r, 7)?,
                })
            },
        )
        .optional()
        .map_err(sql)?;
    let Some(StagedCheckpointHeader { rev, schema_ver, chain_hash, state_hash, comp_sha, comp_bytes, uncomp_bytes, received }) = staged else {
        return Err(invalid("Checkpoint staging header missing"));
    };
    if rev != ckpt_ref.revision
        || schema_ver != ckpt_ref.schema_ver
        || chain_hash != ckpt_ref.chain_hash.as_str()
        || state_hash != ckpt_ref.state_hash.as_str()
        || comp_sha != ckpt_ref.compressed_sha256.as_str()
        || comp_bytes != ckpt_ref.compressed_bytes
        || uncomp_bytes != ckpt_ref.uncompressed_bytes
        || received != comp_bytes
    {
        return Err(invalid("Staged checkpoint is incomplete or mismatched"));
    }
    Ok(())
}

/// Pure lock-free Phase 2 verification & bounded decompression (`<= 128 MiB`) run off the
/// `WorldWorker` thread so local durable ACKs (`Event::Saved`) are never blocked.
pub(super) fn verify_and_decode_checkpoint(
    info: &wire::CanvasSummary,
    ckpt_ref: &wire::CheckpointRef,
    compressed: &[u8],
) -> Result<(WorldCheckpoint, WorldState)> {
    ckpt_ref.validate().map_err(|e| invalid(&e.to_string()))?;
    if ckpt_ref.canvas != info.canvas || ckpt_ref.revision > info.revision || compressed.len() as u64 != ckpt_ref.compressed_bytes {
        return Err(invalid("Checkpoint payload disagrees with authorized canvas summary"));
    }
    let ckpt = WorldCheckpoint::decode_zlib_bounded(
        compressed,
        ckpt_ref.compressed_sha256.as_str(),
        ckpt_ref.uncompressed_bytes,
        ckpt_ref.state_hash.as_str(),
        wire::MAX_CHECKPOINT_UNCOMPRESSED_BYTES,
    )
    .map_err(|e| invalid(&format!("Checkpoint verification failed: {e}")))?;

    if ckpt.canvas != info.canvas
        || ckpt.owner != info.owner
        || ckpt.initial_name != info.initial_name
        || ckpt.import_origin != info.import_origin
        || ckpt.revision != ckpt_ref.revision
        || ckpt.schema != ckpt_ref.schema_ver
    {
        return Err(invalid("Decoded checkpoint identity disagrees with authorized canvas summary"));
    }
    let world = WorldState::from_checkpoint(ckpt.clone()).map_err(|e| invalid(&e.to_string()))?;
    world.validate().map_err(|e| invalid(&e.to_string()))?;
    Ok((ckpt, world))
}

/// Fast atomic Phase 3 publication under the current activation lease (`Immediate` tx).
/// Re-verifies staged pin & head monotonicity; never alters pending/recovery `operations` or `viewers`.
pub(super) fn commit_verified_checkpoint(
    tx: &Transaction<'_>,
    lease: &Activation,
    info: &wire::CanvasSummary,
    ckpt_ref: &wire::CheckpointRef,
    compressed: &[u8],
    ckpt: &WorldCheckpoint,
) -> Result<()> {
    verify_staged_header(tx, &lease.key, info.canvas, ckpt_ref)?;
    if compressed.len() as u64 != ckpt_ref.compressed_bytes || ckpt.canvas != info.canvas || ckpt.revision != ckpt_ref.revision {
        return Err(invalid("Verified checkpoint payload disagrees with staged pin"));
    }
    if let Some(prior) = read_head(tx, &lease.key, info.canvas)? {
        if prior.revision > ckpt.revision {
            return Err(invalid("Stale checkpoint cannot overwrite newer durable local state"));
        }
        if prior.owner != ckpt.owner || prior.initial_name != ckpt.initial_name || prior.import_origin != ckpt.import_origin {
            return Err(invalid("Checkpoint identity conflicts with saved local head"));
        }
        if prior.revision == ckpt.revision && prior.chain_hash != ckpt_ref.chain_hash.as_str() {
            return Err(invalid("Checkpoint chain hash conflicts with saved immutable history"));
        }
    }

    let row_hash = checksum(&lease.key, info.canvas, compressed);
    tx.execute(
        "INSERT OR REPLACE INTO canvas_checkpoints(scope,canvas,revision,schema_ver,chain_hash,state_hash,compressed_sha256,compressed_bytes,uncompressed_bytes,body_zlib,hash) \
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            lease.key,
            info.canvas.as_str(),
            integer(ckpt.revision)?,
            ckpt.schema,
            ckpt_ref.chain_hash.as_str(),
            ckpt_ref.state_hash.as_str(),
            ckpt_ref.compressed_sha256.as_str(),
            integer(ckpt_ref.compressed_bytes)?,
            integer(ckpt_ref.uncompressed_bytes)?,
            compressed,
            row_hash,
        ],
    )
    .map_err(sql)?;
    tx.execute(
        "DELETE FROM canvas_checkpoints WHERE scope=?1 AND canvas=?2 AND revision NOT IN (\
             SELECT revision FROM canvas_checkpoints WHERE scope=?1 AND canvas=?2 ORDER BY revision DESC LIMIT 1\
         )",
        params![lease.key, info.canvas.as_str()],
    )
    .map_err(sql)?;
    tx.execute("DELETE FROM accepted_events WHERE scope=?1 AND canvas=?2 AND revision<=?3", params![lease.key, info.canvas.as_str(), integer(ckpt.revision)?])
        .map_err(sql)?;

    let head = CanvasHead {
        canvas: ckpt.canvas,
        owner: ckpt.owner,
        name: ckpt.name.clone(),
        initial_name: ckpt.initial_name.clone(),
        revision: ckpt.revision,
        base_revision: ckpt.revision,
        schema_ver: ckpt.schema,
        chain_hash: ckpt_ref.chain_hash.as_str().to_owned(),
        limits: ckpt.limits,
        import_origin: ckpt.import_origin.clone(),
    };
    write_head(tx, &lease.key, &head)?;
    clear_checkpoint_stage(tx, &lease.key, info.canvas)?;
    Ok(())
}

/// Atomically verify and publish a completely staged pinned checkpoint under the current activation lease.
/// Never alters `operations` (pending/recovery) or `viewers`.
pub(super) fn publish_checkpoint(tx: &Transaction<'_>, lease: &Activation, info: &wire::CanvasSummary, ckpt_ref: &wire::CheckpointRef) -> Result<WorldState> {
    let compressed = read_staged_checkpoint_bytes(tx, lease, info, ckpt_ref)?;
    let (ckpt, world) = verify_and_decode_checkpoint(info, ckpt_ref, &compressed)?;
    commit_verified_checkpoint(tx, lease, info, ckpt_ref, &compressed, &ckpt)?;
    Ok(world)
}

/// Used by `relocation.rs` to verify destination scope has no v4 rows.
pub(super) fn count_scope_rows(db: &Connection, scope: &str) -> Result<u64> {
    db.query_row(
        "SELECT (SELECT count(*) FROM canvas_heads WHERE scope=?1)\
               +(SELECT count(*) FROM accepted_events WHERE scope=?1)\
               +(SELECT count(*) FROM canvas_checkpoints WHERE scope=?1)\
               +(SELECT count(*) FROM checkpoint_staging WHERE scope=?1)\
               +(SELECT count(*) FROM checkpoint_staging_chunks WHERE scope=?1)",
        [scope],
        |r| read_u64(r, 0),
    )
    .map_err(sql)
}

/// Used by `relocation.rs` to re-key v4 rows from `source_key` to `target_key`,
/// validating and recomputing scoped checksums atomically.
pub(super) fn migrate_scope_v4(tx: &Transaction<'_>, source_key: &str, target_key: &str) -> Result<()> {
    // 1. Discard transient incomplete staging on endpoint move (resumable from target endpoint).
    tx.execute("DELETE FROM checkpoint_staging_chunks WHERE scope=?1", [source_key]).map_err(sql)?;
    tx.execute("DELETE FROM checkpoint_staging WHERE scope=?1", [source_key]).map_err(sql)?;

    // 2. Validate & move canvas_heads.
    let mut head_canvases = Vec::new();
    {
        let mut stmt = tx.prepare("SELECT CASE WHEN length(canvas)=32 THEN canvas ELSE NULL END FROM canvas_heads WHERE scope=?1 LIMIT ?2").map_err(sql)?;
        let mut rows = stmt.query(params![source_key, (MAX_DIRECTORY + 1) as i64]).map_err(sql)?;
        while let Some(row) = rows.next().map_err(sql)? {
            if head_canvases.len() >= MAX_DIRECTORY {
                return Err(invalid("Canvas migration heads exceed their directory limit"));
            }
            let canvas = CanvasId::new(&row.get::<_, String>(0).map_err(sql)?).map_err(|_| invalid("Canvas migration head identity"))?;
            head_canvases.push(canvas);
        }
    }
    for canvas in head_canvases {
        let head = read_head(tx, source_key, canvas)?.ok_or_else(|| invalid("Canvas migration head missing"))?;
        write_head(tx, target_key, &head)?;
        tx.execute("DELETE FROM canvas_heads WHERE scope=?1 AND canvas=?2", params![source_key, canvas.as_str()]).map_err(sql)?;
    }

    // 3. Validate & move accepted_events.
    let mut event_updates = Vec::new();
    {
        let mut stmt = tx
            .prepare(
                "SELECT canvas,revision,substr(CAST(body AS BLOB),1,?2),CAST(substr(CAST(event_hash AS BLOB),1,65) AS TEXT),CAST(substr(CAST(hash AS BLOB),1,65) AS TEXT) \
                 FROM accepted_events WHERE scope=?1",
            )
            .map_err(sql)?;
        let mut rows = stmt.query(params![source_key, (MAX_EVENT_BYTES + 1) as i64]).map_err(sql)?;
        while let Some(row) = rows.next().map_err(sql)? {
            let canvas_s: String = row.get(0).map_err(sql)?;
            let canvas = CanvasId::new(&canvas_s).map_err(|_| invalid("Canvas migration event identity"))?;
            let rev = read_u64(row, 1).map_err(sql)?;
            let raw: Vec<u8> = row.get(2).map_err(sql)?;
            let ev_hash: String = row.get(3).map_err(sql)?;
            let row_hash: String = row.get(4).map_err(sql)?;
            let json = decode_event_body(&raw)?;
            if hash(&json) != ev_hash || checksum(source_key, canvas, &json) != row_hash {
                return Err(invalid("Canvas migration event checksum failed"));
            }
            event_updates.push((canvas_s, integer(rev)?, checksum(target_key, canvas, &json)));
        }
    }
    for (canvas_s, rev, new_hash) in event_updates {
        tx.execute(
            "UPDATE accepted_events SET scope=?1,hash=?2 WHERE scope=?3 AND canvas=?4 AND revision=?5",
            params![target_key, new_hash, source_key, canvas_s, rev],
        )
        .map_err(sql)?;
    }

    // 4. Validate & move canvas_checkpoints.
    let mut ckpt_updates = Vec::new();
    {
        let mut stmt = tx
            .prepare(
                "SELECT canvas,revision,substr(CAST(body_zlib AS BLOB),1,?2),CAST(substr(CAST(hash AS BLOB),1,65) AS TEXT) \
                 FROM canvas_checkpoints WHERE scope=?1",
            )
            .map_err(sql)?;
        let mut rows = stmt.query(params![source_key, (wire::MAX_CHECKPOINT_COMPRESSED_BYTES + 1) as i64]).map_err(sql)?;
        while let Some(row) = rows.next().map_err(sql)? {
            let canvas_s: String = row.get(0).map_err(sql)?;
            let canvas = CanvasId::new(&canvas_s).map_err(|_| invalid("Canvas migration checkpoint identity"))?;
            let rev = read_u64(row, 1).map_err(sql)?;
            let body: Vec<u8> = row.get(2).map_err(sql)?;
            let row_hash: String = row.get(3).map_err(sql)?;
            if body.len() as u64 > wire::MAX_CHECKPOINT_COMPRESSED_BYTES || checksum(source_key, canvas, &body) != row_hash {
                return Err(invalid("Canvas migration checkpoint checksum failed"));
            }
            ckpt_updates.push((canvas_s, integer(rev)?, checksum(target_key, canvas, &body)));
        }
    }
    for (canvas_s, rev, new_hash) in ckpt_updates {
        tx.execute(
            "UPDATE canvas_checkpoints SET scope=?1,hash=?2 WHERE scope=?3 AND canvas=?4 AND revision=?5",
            params![target_key, new_hash, source_key, canvas_s, rev],
        )
        .map_err(sql)?;
    }

    let _ = MAX_PAGE_BYTES;
    Ok(())
}
