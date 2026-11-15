//! Recoverable deletion changes access, never accepted artwork or history.
use super::{denied, member_epoch, page, scene_error, summary};
use crate::store::{read_u64, sql_int};
use crate::{Error, Result};
use photocraft_party::Error as Domain;
use photocraft_scene::world_protocol::{Response, TrashedCanvas};
use photocraft_scene::{CanvasId, OwnerId};
use rusqlite::{Connection, OptionalExtension, params};

pub(super) const TABLES: &str = "CREATE TABLE world_trash(canvas TEXT PRIMARY KEY REFERENCES worlds(id), deleted_at_ms INTEGER NOT NULL CHECK(deleted_at_ms>=0), owner_epoch INTEGER NOT NULL CHECK(owner_epoch>1));";

pub(super) fn migrate(db: &Connection, path: &std::path::Path) -> Result<()> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| Error::Storage)?;
    let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let mut name = path.file_name().ok_or(Error::Invalid("canvas migration database path"))?.to_os_string();
    name.push(format!(".before-world-v3-{suffix}.sqlite3"));
    crate::backup::copy(db, &path.with_file_name(name))?;
    let tx = db.unchecked_transaction()?;
    tx.execute_batch("DROP TABLE world_schema; CREATE TABLE world_schema(version INTEGER NOT NULL CHECK(version=3)); INSERT INTO world_schema VALUES(3);")?;
    tx.execute_batch(TABLES)?;
    super::verify(&tx)?;
    tx.commit()?;
    Ok(())
}

pub(super) fn verify(db: &Connection) -> Result<()> {
    let invalid: u64 = db.query_row(
        "SELECT count(*) FROM world_trash t LEFT JOIN worlds w ON w.id=t.canvas LEFT JOIN world_members m ON m.canvas=t.canvas AND m.profile=w.owner WHERE w.id IS NULL OR m.profile IS NULL OR m.active!=1 OR typeof(t.deleted_at_ms)!='integer' OR t.deleted_at_ms<0 OR typeof(t.owner_epoch)!='integer' OR t.owner_epoch<2 OR t.owner_epoch!=m.epoch OR EXISTS(SELECT 1 FROM world_invites i WHERE i.canvas=t.canvas AND i.status!=2)",
        [], |row| read_u64(row, 0),
    )?;
    if invalid != 0 {
        return Err(Error::Storage);
    }
    Ok(())
}

pub(super) fn require_live(db: &Connection, canvas: CanvasId) -> Result<()> {
    let deleted: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM world_trash WHERE canvas=?1)", [canvas.as_str()], |row| row.get(0))?;
    if deleted {
        return denied();
    }
    Ok(())
}

fn owner_epoch(db: &Connection, canvas: CanvasId, profile: OwnerId) -> Result<u64> {
    let owner: Option<String> =
        db.query_row("SELECT CAST(substr(CAST(owner AS BLOB),1,33) AS TEXT) FROM worlds WHERE id=?1", [canvas.as_str()], |row| row.get(0)).optional()?;
    if owner.as_deref() != Some(profile.as_str()) {
        return denied();
    }
    member_epoch(db, canvas, profile)
}

fn tombstone(db: &Connection, canvas: CanvasId) -> Result<Option<u64>> {
    Ok(db.query_row("SELECT owner_epoch FROM world_trash WHERE canvas=?1", [canvas.as_str()], |row| read_u64(row, 0)).optional()?)
}

fn expected(value: u64) -> Result<()> {
    if value == 0 || value > i64::MAX as u64 {
        return Err(Error::Invalid("canvas membership epoch"));
    }
    Ok(())
}

pub(super) fn delete(db: &Connection, profile: OwnerId, canvas: CanvasId, expected_epoch: u64, now: u64) -> Result<Response> {
    expected(expected_epoch)?;
    let epoch = owner_epoch(db, canvas, profile)?;
    if let Some(deleted_epoch) = tombstone(db, canvas)? {
        return if expected_epoch.checked_add(1) == Some(deleted_epoch) && epoch == deleted_epoch { Ok(Response::Done) } else { Err(Domain::Conflict.into()) };
    }
    if epoch != expected_epoch {
        return Err(Domain::Conflict.into());
    }
    let maximum: u64 = db.query_row("SELECT max(epoch) FROM world_members WHERE canvas=?1 AND active=1", [canvas.as_str()], |row| read_u64(row, 0))?;
    if maximum >= i64::MAX as u64 {
        return Err(Domain::Limit("canvas membership epoch").into());
    }
    let next = epoch.checked_add(1).ok_or(Domain::Limit("canvas membership epoch"))?;
    // Retain which people had access. Fresh epochs invalidate all queued edits,
    // including on devices that remain offline throughout deletion and restore.
    db.execute("UPDATE world_members SET epoch=epoch+1 WHERE canvas=?1 AND active=1", [canvas.as_str()])?;
    db.execute("UPDATE world_invites SET status=2 WHERE canvas=?1", [canvas.as_str()])?;
    db.execute("INSERT INTO world_trash VALUES(?1,?2,?3)", params![canvas.as_str(), sql_int(now)?, sql_int(next)?])?;
    Ok(Response::Done)
}

pub(super) fn restore(db: &Connection, profile: OwnerId, canvas: CanvasId, expected_epoch: u64) -> Result<Response> {
    expected(expected_epoch)?;
    let epoch = owner_epoch(db, canvas, profile)?;
    if epoch != expected_epoch || tombstone(db, canvas)?.is_some_and(|deleted| deleted != expected_epoch) {
        return Err(Domain::Conflict.into());
    }
    // Same-canvas restoration retains its former active members. Archive restore
    // is the distinct operation that makes a new, owner-only independent copy.
    db.execute("DELETE FROM world_trash WHERE canvas=?1", [canvas.as_str()])?;
    Ok(Response::Canvas { info: summary(db, canvas, profile)? })
}

pub(super) fn list(db: &Connection, profile: OwnerId, after: Option<CanvasId>, limit: u32) -> Result<Response> {
    let limit = page(limit)?;
    let mut statement = db.prepare("SELECT CAST(substr(CAST(t.canvas AS BLOB),1,33) AS TEXT),CAST(substr(CAST(w.name AS BLOB),1,97) AS TEXT),w.revision,t.deleted_at_ms,t.owner_epoch FROM world_trash t JOIN worlds w ON w.id=t.canvas JOIN world_members m ON m.canvas=t.canvas AND m.profile=w.owner AND m.active=1 WHERE w.owner=?1 AND t.canvas>?2 ORDER BY t.canvas LIMIT ?3")?;
    let mut rows = statement.query(params![profile.as_str(), after.as_ref().map_or("", CanvasId::as_str), limit + 1])?;
    let mut items = Vec::new();
    let mut more = false;
    while let Some(row) = rows.next()? {
        if items.len() == limit as usize {
            more = true;
            break;
        }
        let canvas: String = row.get(0)?;
        let name: String = row.get(1)?;
        photocraft_scene::validate_name(&name).map_err(|_| Error::Storage)?;
        items.push(TrashedCanvas {
            canvas: CanvasId::new(&canvas).map_err(scene_error)?,
            name,
            revision: read_u64(row, 2)?,
            deleted_at_ms: read_u64(row, 3)?,
            owner_epoch: read_u64(row, 4)?,
        });
    }
    let next = if more { items.last().map(|item| item.canvas) } else { None };
    Ok(Response::Trash { items, next })
}
