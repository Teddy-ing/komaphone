//! Bounded durable import staging. Archive identities are content, never grants.
use std::io::{Read, Seek, SeekFrom, Write};

use photocraft_party::Error as Domain;
use photocraft_scene::shared::archive_import::{ArchiveManifest, MAX_CHUNK_BYTES, MAX_MANIFEST_BYTES, MAX_TOTAL_BYTES};
use photocraft_scene::shared::media::{AssetHash, MediaFrame};
use photocraft_scene::shared::{DeviceId, OpId};
use photocraft_scene::world_protocol::{ImportObject, Response};
use photocraft_scene::{CanvasId, OwnerId};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{Limits, media, require_owner, scene_error, storage_v6, summary};
use crate::store::{digest, read_u64, sql_int};
use crate::{Error, Result};

const MAX_PENDING: u64 = 16;
const MAX_PER_PROFILE: u64 = 2;
const MAX_RECEIPTS: u64 = 4096;
const LIFETIME_MS: u64 = 24 * 60 * 60 * 1000;
pub(super) const TABLES: &str = "
CREATE TABLE world_imports(id TEXT PRIMARY KEY,profile TEXT NOT NULL REFERENCES identity_profiles(profile),device TEXT NOT NULL REFERENCES identity_devices(device),canvas TEXT NOT NULL,hash TEXT NOT NULL,manifest_bytes INTEGER NOT NULL,total_bytes INTEGER NOT NULL,expires INTEGER NOT NULL,received INTEGER NOT NULL,data BLOB NOT NULL);
CREATE TABLE world_import_assets(import_id TEXT NOT NULL REFERENCES world_imports(id) ON DELETE CASCADE,hash TEXT NOT NULL,total INTEGER NOT NULL,width INTEGER NOT NULL,height INTEGER NOT NULL,received INTEGER NOT NULL,data BLOB NOT NULL,PRIMARY KEY(import_id,hash));
CREATE TABLE world_import_receipts(id TEXT PRIMARY KEY,profile TEXT NOT NULL REFERENCES identity_profiles(profile),device TEXT NOT NULL REFERENCES identity_devices(device),canvas TEXT NOT NULL REFERENCES worlds(id),hash TEXT NOT NULL,manifest_bytes INTEGER NOT NULL,total_bytes INTEGER NOT NULL);
CREATE TABLE world_import_origins(canvas TEXT PRIMARY KEY REFERENCES worlds(id),body BLOB NOT NULL,hash TEXT NOT NULL);
";

pub(super) fn origin(db: &Connection, canvas: CanvasId) -> Result<Option<Box<photocraft_scene::shared::ImportOrigin>>> {
    let value: Option<(Vec<u8>, String)> = db
        .query_row("SELECT substr(body,1,65537),substr(hash,1,65) FROM world_import_origins WHERE canvas=?1", [canvas.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    value
        .map(|(body, hash)| {
            if body.len() > 65536 || digest(&body) != hash {
                return Err(Error::Storage);
            }
            serde_json::from_slice(&body).map_err(|_| Error::Storage)
        })
        .transpose()
}
pub(super) fn store_origin(db: &Connection, world: &photocraft_scene::shared::WorldState) -> Result<()> {
    if let Some(origin) = &world.import_origin {
        let body = serde_json::to_vec(origin)?;
        if body.len() > 65536 {
            return Err(Domain::Limit("archive origin metadata").into());
        }
        db.execute("INSERT INTO world_import_origins VALUES(?1,?2,?3)", params![world.canvas.as_str(), body, digest(&body)])?;
    }
    Ok(())
}

pub(super) fn migrate(db: &Connection, path: &std::path::Path) -> Result<()> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| Error::Storage)?;
    let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let mut name = path.file_name().ok_or(Error::Invalid("canvas migration path"))?.to_os_string();
    name.push(format!(".before-world-v4-{suffix}.sqlite3"));
    crate::backup::copy(db, &path.with_file_name(name))?;
    let tx = db.unchecked_transaction()?;
    tx.execute_batch("DROP TABLE world_schema; CREATE TABLE world_schema(version INTEGER NOT NULL CHECK(version=4)); INSERT INTO world_schema VALUES(4);")?;
    tx.execute_batch(TABLES)?;
    tx.pragma_update(None, "user_version", 4)?;
    super::verify(&tx)?;
    tx.commit()?;
    Ok(())
}

#[derive(Clone)]
struct Header {
    id: OpId,
    profile: OwnerId,
    device: DeviceId,
    canvas: CanvasId,
    hash: AssetHash,
    manifest_bytes: u64,
    total_bytes: u64,
}
impl Header {
    fn validate(&self) -> Result<()> {
        if self.manifest_bytes == 0 || self.manifest_bytes > MAX_MANIFEST_BYTES || self.total_bytes < self.manifest_bytes || self.total_bytes > MAX_TOTAL_BYTES
        {
            return Err(Domain::Limit("archive manifest or total bytes").into());
        }
        Ok(())
    }
    fn authorize(&self, profile: OwnerId, device: DeviceId) -> Result<()> {
        if self.profile != profile || self.device != device {
            return Err(Domain::Unauthorized.into());
        }
        Ok(())
    }
}
fn header(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, String, String, String, String, u64, u64)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, read_u64(row, 5)?, read_u64(row, 6)?))
}
fn parse_header(value: (String, String, String, String, String, u64, u64)) -> Result<Header> {
    let item = Header {
        id: OpId::new(&value.0).map_err(scene_error)?,
        profile: OwnerId::new(&value.1).map_err(scene_error)?,
        device: DeviceId::new(&value.2).map_err(scene_error)?,
        canvas: CanvasId::new(&value.3).map_err(scene_error)?,
        hash: AssetHash::new(&value.4).map_err(scene_error)?,
        manifest_bytes: value.5,
        total_bytes: value.6,
    };
    item.validate()?;
    Ok(item)
}
fn lookup(db: &Connection, id: OpId, committed: bool) -> Result<Option<Header>> {
    let sql = if committed {
        "SELECT substr(id,1,33),substr(profile,1,33),substr(device,1,33),substr(canvas,1,33),substr(hash,1,65),manifest_bytes,total_bytes FROM world_import_receipts WHERE id=?1"
    } else {
        "SELECT substr(id,1,33),substr(profile,1,33),substr(device,1,33),substr(canvas,1,33),substr(hash,1,65),manifest_bytes,total_bytes FROM world_imports WHERE id=?1"
    };
    db.query_row(sql, [id.as_str()], header).optional()?.map(parse_header).transpose()
}
fn active(db: &Connection, profile: OwnerId, device: DeviceId, id: OpId, now: u64) -> Result<Header> {
    let item = lookup(db, id, false)?.ok_or(Error::Invalid("archive upload is absent or expired; restart import from the local archive"))?;
    item.authorize(profile, device)?;
    let expires = db.query_row("SELECT expires FROM world_imports WHERE id=?1", [id.as_str()], |r| read_u64(r, 0))?;
    if expires <= now {
        return Err(Error::Invalid("archive upload expired; restart import from the local archive"));
    }
    Ok(item)
}
fn progress(id: OpId, object: ImportObject, received: u64, total: u64) -> Response {
    Response::ImportProgress { id, object, received, total }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn begin(
    db: &Transaction<'_>,
    profile: OwnerId,
    device: DeviceId,
    id: OpId,
    canvas: CanvasId,
    hash: AssetHash,
    manifest_bytes: u64,
    total_bytes: u64,
    now: u64,
) -> Result<Response> {
    let wanted = Header { id, profile, device, canvas, hash, manifest_bytes, total_bytes };
    wanted.validate()?;
    for committed in [true, false] {
        if let Some(previous) = lookup(db, id, committed)? {
            previous.authorize(profile, device)?;
            if previous.canvas != canvas || previous.hash != hash || previous.manifest_bytes != manifest_bytes || previous.total_bytes != total_bytes {
                return Err(Domain::Conflict.into());
            }
            if committed {
                return Ok(Response::Canvas { info: require_owner(db, canvas, profile)? });
            }
            let (received, expires) =
                db.query_row("SELECT received,expires FROM world_imports WHERE id=?1", [id.as_str()], |r| Ok((read_u64(r, 0)?, read_u64(r, 1)?)))?;
            if expires > now {
                return Ok(progress(id, ImportObject::Manifest {}, received, manifest_bytes));
            }
        }
    }
    db.execute("DELETE FROM world_imports WHERE expires<=?1", [sql_int(now)?])?;
    if db.query_row("SELECT EXISTS(SELECT 1 FROM worlds WHERE id=?1)", [canvas.as_str()], |r| r.get::<_, bool>(0))? {
        return Err(Domain::Conflict.into());
    }
    let (count, own): (u64, u64) =
        db.query_row("SELECT count(*),coalesce(sum(profile=?1),0) FROM world_imports", [profile.as_str()], |r| Ok((read_u64(r, 0)?, read_u64(r, 1)?)))?;
    if count >= MAX_PENDING || own >= MAX_PER_PROFILE {
        return Err(Domain::Limit("active archive imports; cancel an earlier import or wait for expiry").into());
    }
    let expires = now.checked_add(LIFETIME_MS).ok_or(Error::Invalid("archive expiry"))?;
    db.execute(
        "INSERT INTO world_imports VALUES(?1,?2,?3,?4,?5,?6,?7,?8,0,zeroblob(?6))",
        params![
            id.as_str(),
            profile.as_str(),
            device.as_str(),
            canvas.as_str(),
            hash.as_str(),
            sql_int(manifest_bytes)?,
            sql_int(total_bytes)?,
            sql_int(expires)?
        ],
    )?;
    Ok(progress(id, ImportObject::Manifest {}, 0, manifest_bytes))
}

fn manifest(db: &Connection, item: &Header) -> Result<ArchiveManifest> {
    let (bytes, received): (Vec<u8>, u64) =
        db.query_row("SELECT substr(data,1,33554433),received FROM world_imports WHERE id=?1", [item.id.as_str()], |r| Ok((r.get(0)?, read_u64(r, 1)?)))?;
    if received != item.manifest_bytes || bytes.len() as u64 != item.manifest_bytes || digest(&bytes) != item.hash.as_str() {
        return Err(Error::Invalid("archive manifest is incomplete or corrupt"));
    }
    let manifest = ArchiveManifest::decode(&bytes).map_err(scene_error)?;
    let total = manifest.assets.iter().try_fold(item.manifest_bytes, |sum, frame| sum.checked_add(frame.byte_len).ok_or(Error::Invalid("archive total")))?;
    if total != item.total_bytes {
        return Err(Error::Invalid("archive declared total differs from manifest"));
    }
    Ok(manifest)
}

fn write_chunk(db: &Connection, table: &str, row: i64, received: u64, total: u64, offset: u64, bytes: &[u8]) -> Result<u64> {
    let end = offset.checked_add(bytes.len() as u64).filter(|n| *n <= total).ok_or(Error::Invalid("archive chunk bounds"))?;
    if offset > received || offset < received && end > received {
        return Err(Domain::Conflict.into());
    }
    let mut blob = db.blob_open("main", table, "data", row, false)?;
    if blob.len() as u64 != total {
        return Err(Error::Storage);
    }
    blob.seek(SeekFrom::Start(offset))?;
    if offset < received {
        let mut prior = vec![0; bytes.len()];
        blob.read_exact(&mut prior)?;
        if prior != bytes {
            return Err(Domain::Conflict.into());
        }
    } else {
        blob.write_all(bytes)?;
    }
    Ok(received.max(end))
}
fn validate_asset(frame: &MediaFrame, bytes: &[u8]) -> Result<()> {
    frame.validate().map_err(scene_error)?;
    if bytes.len() as u64 != frame.byte_len || digest(bytes) != frame.hash.as_str() {
        return Err(Error::Invalid("archive image checksum or length"));
    }
    media::decode(bytes, (frame.width, frame.height))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)] // Authenticated identity plus the bounded wire chunk.
pub(super) fn chunk(
    db: &Transaction<'_>,
    profile: OwnerId,
    device: DeviceId,
    id: OpId,
    object: &ImportObject,
    offset: u64,
    bytes: &[u8],
    now: u64,
) -> Result<Response> {
    if bytes.is_empty() || bytes.len() > MAX_CHUNK_BYTES {
        return Err(Domain::Limit("archive chunk bytes").into());
    }
    let item = active(db, profile, device, id, now)?;
    match object {
        ImportObject::Manifest {} => {
            let (row, received): (i64, u64) =
                db.query_row("SELECT rowid,received FROM world_imports WHERE id=?1", [id.as_str()], |r| Ok((r.get(0)?, read_u64(r, 1)?)))?;
            let next = write_chunk(db, "world_imports", row, received, item.manifest_bytes, offset, bytes)?;
            db.execute("UPDATE world_imports SET received=?2 WHERE id=?1", params![id.as_str(), sql_int(next)?])?;
            if next == item.manifest_bytes && received != next {
                let parsed = manifest(db, &item)?;
                // Reject unsupported provenance before receiving any image bytes.
                parsed.world.import_to(item.canvas, profile).map_err(scene_error)?;
                for frame in parsed.assets {
                    db.execute(
                        "INSERT INTO world_import_assets VALUES(?1,?2,?3,?4,?5,0,X'')",
                        params![id.as_str(), frame.hash.as_str(), sql_int(frame.byte_len)?, frame.width, frame.height],
                    )?;
                }
            }
            Ok(progress(id, object.clone(), next, item.manifest_bytes))
        }
        ImportObject::Asset { hash } => {
            let value: Option<(i64, u64, u32, u32, u64, u64)> = db
                .query_row(
                    "SELECT rowid,total,width,height,received,length(data) FROM world_import_assets WHERE import_id=?1 AND hash=?2",
                    params![id.as_str(), hash.as_str()],
                    |r| Ok((r.get(0)?, read_u64(r, 1)?, r.get(2)?, r.get(3)?, read_u64(r, 4)?, read_u64(r, 5)?)),
                )
                .optional()?;
            let (row, total, width, height, received, len) = value.ok_or(Error::Invalid("archive image is not declared by a verified manifest"))?;
            if len == 0 {
                db.execute("UPDATE world_import_assets SET data=zeroblob(total) WHERE rowid=?1", [row])?;
            }
            let next = write_chunk(db, "world_import_assets", row, received, total, offset, bytes)?;
            if next == total && received != next {
                let body: Vec<u8> = db.query_row("SELECT substr(data,1,8388609) FROM world_import_assets WHERE rowid=?1", [row], |r| r.get(0))?;
                validate_asset(&MediaFrame { hash: *hash, byte_len: total, width, height, duration_ms: 10 }, &body)?;
            }
            db.execute("UPDATE world_import_assets SET received=?2 WHERE rowid=?1", params![row, sql_int(next)?])?;
            Ok(progress(id, object.clone(), next, total))
        }
    }
}

pub(super) fn cancel(db: &Transaction<'_>, profile: OwnerId, device: DeviceId, id: OpId) -> Result<Response> {
    for committed in [true, false] {
        if let Some(item) = lookup(db, id, committed)? {
            item.authorize(profile, device)?;
            if !committed {
                db.execute("DELETE FROM world_imports WHERE id=?1", [id.as_str()])?;
            }
            return Ok(Response::Done);
        }
    }
    Ok(Response::Done)
}

pub(super) fn commit(db: &Transaction<'_>, profile: OwnerId, device: DeviceId, id: OpId, now: u64, limits: Limits) -> Result<Response> {
    if let Some(prior) = lookup(db, id, true)? {
        prior.authorize(profile, device)?;
        return Ok(Response::Canvas { info: require_owner(db, prior.canvas, profile)? });
    }
    let item = active(db, profile, device, id, now)?;
    let manifest = manifest(db, &item)?;
    if db.query_row("SELECT EXISTS(SELECT 1 FROM worlds WHERE id=?1)", [item.canvas.as_str()], |r| r.get::<_, bool>(0))? {
        return Err(Domain::Conflict.into());
    }
    let count: u64 = db.query_row("SELECT count(*) FROM worlds", [], |r| read_u64(r, 0))?;
    let receipts: u64 = db.query_row("SELECT count(*) FROM world_import_receipts", [], |r| read_u64(r, 0))?;
    if count >= u64::from(limits.canvases) || receipts >= MAX_RECEIPTS {
        return Err(Domain::Limit("canvas or retained import receipt capacity").into());
    }
    let copied = manifest.world.import_to(item.canvas, profile).map_err(scene_error)?;
    storage_v6::insert_world(db, &copied)?;
    store_origin(db, &copied)?;
    for frame in &manifest.assets {
        let (bytes, received): (Vec<u8>, u64) = db
            .query_row(
                "SELECT substr(data,1,8388609),received FROM world_import_assets WHERE import_id=?1 AND hash=?2",
                params![id.as_str(), frame.hash.as_str()],
                |r| Ok((r.get(0)?, read_u64(r, 1)?)),
            )
            .optional()?
            .ok_or(Error::Invalid("archive image missing"))?;
        if received != frame.byte_len {
            return Err(Error::Invalid("archive image upload incomplete"));
        }
        validate_asset(frame, &bytes)?;
        db.execute("INSERT INTO world_blobs VALUES(?1,?2,?3,?4)", params![item.canvas.as_str(), frame.hash.as_str(), sql_int(frame.byte_len)?, bytes])?;
    }
    media::verify_world(db, &copied)?;
    db.execute(
        "INSERT INTO world_import_receipts SELECT id,profile,device,canvas,hash,manifest_bytes,total_bytes FROM world_imports WHERE id=?1",
        [id.as_str()],
    )?;
    db.execute("DELETE FROM world_imports WHERE id=?1", [id.as_str()])?;
    Ok(Response::Canvas { info: summary(db, item.canvas, profile)? })
}

pub(super) fn verify(db: &Connection) -> Result<()> {
    let counts: (u64, u64) = db.query_row("SELECT (SELECT count(*) FROM world_imports),(SELECT count(*) FROM world_import_receipts)", [], |r| {
        Ok((read_u64(r, 0)?, read_u64(r, 1)?))
    })?;
    if counts.0 > MAX_PENDING || counts.1 > MAX_RECEIPTS {
        return Err(Error::Storage);
    }
    for committed in [false, true] {
        let table = if committed { "world_import_receipts" } else { "world_imports" };
        let mut query = db.prepare(&format!(
            "SELECT substr(id,1,33),substr(profile,1,33),substr(device,1,33),substr(canvas,1,33),substr(hash,1,65),manifest_bytes,total_bytes FROM {table}"
        ))?;
        for row in query.query_map([], header)? {
            let item = parse_header(row?)?;
            let bound: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM identity_devices WHERE device=?1 AND profile=?2)",
                params![item.device.as_str(), item.profile.as_str()],
                |r| r.get(0),
            )?;
            if !bound {
                return Err(Error::Storage);
            }
            if committed {
                let owned: bool =
                    db.query_row("SELECT EXISTS(SELECT 1 FROM worlds WHERE id=?1 AND owner=?2)", params![item.canvas.as_str(), item.profile.as_str()], |r| {
                        r.get(0)
                    })?;
                if !owned {
                    return Err(Error::Storage);
                }
                continue;
            }
            let (len, received, expires): (u64, u64, u64) =
                db.query_row("SELECT length(data),received,expires FROM world_imports WHERE id=?1", [item.id.as_str()], |r| {
                    Ok((read_u64(r, 0)?, read_u64(r, 1)?, read_u64(r, 2)?))
                })?;
            if len != item.manifest_bytes || received > len || expires == 0 {
                return Err(Error::Storage);
            }
            if received == len {
                let parsed = manifest(db, &item)?;
                let asset_count: u64 = db.query_row("SELECT count(*) FROM world_import_assets WHERE import_id=?1", [item.id.as_str()], |r| read_u64(r, 0))?;
                if asset_count != parsed.assets.len() as u64 {
                    return Err(Error::Storage);
                }
                for frame in &parsed.assets {
                    let (total, width, height, received, len): (u64, u32, u32, u64, u64) = db.query_row(
                        "SELECT total,width,height,received,length(data) FROM world_import_assets WHERE import_id=?1 AND hash=?2",
                        params![item.id.as_str(), frame.hash.as_str()],
                        |r| Ok((read_u64(r, 0)?, r.get(1)?, r.get(2)?, read_u64(r, 3)?, read_u64(r, 4)?)),
                    )?;
                    if total != frame.byte_len
                        || width != frame.width
                        || height != frame.height
                        || received > total
                        || (len != total && (len != 0 || received != 0))
                    {
                        return Err(Error::Storage);
                    }
                    if received == total {
                        let bytes: Vec<u8> = db.query_row(
                            "SELECT substr(data,1,8388609) FROM world_import_assets WHERE import_id=?1 AND hash=?2",
                            params![item.id.as_str(), frame.hash.as_str()],
                            |r| r.get(0),
                        )?;
                        validate_asset(frame, &bytes)?;
                    }
                }
            } else {
                let count: u64 = db.query_row("SELECT count(*) FROM world_import_assets WHERE import_id=?1", [item.id.as_str()], |r| read_u64(r, 0))?;
                if count != 0 {
                    return Err(Error::Storage);
                }
            }
        }
    }
    Ok(())
}
