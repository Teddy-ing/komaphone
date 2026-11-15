//! Explicit authenticated endpoint relocation; never merge two writer queues.
use super::{
    APPLICATION_ID, Activation, CanvasId, DeviceId, MAX_DIRECTORY, MAX_PENDING, MAX_QUEUE_BYTES, MAX_SNAPSHOT_BYTES, OptionalExtension, OwnerId, Path,
    RecoveryReason, Result, SCHEMA, TransactionBehavior, ViewerState, WorldOutbox, WorldScope, WorldState, checksum, decode, hash, invalid, params, read_u64,
    sql, viewer, wire,
};
use crate::{AccountRelocation, world_worker::VerifiedWorldIdentity};
use rusqlite::{Connection, Transaction};

pub(super) fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS scope_moves(id TEXT PRIMARY KEY,from_scope TEXT UNIQUE NOT NULL,to_scope TEXT UNIQUE NOT NULL,body BLOB NOT NULL,digest TEXT NOT NULL);").map_err(sql)
}

pub(super) fn backup_schema_one(db: &Connection, path: &Path) -> Result<()> {
    let name = path.file_name().and_then(|name| name.to_str()).ok_or_else(|| invalid("Cannot name canvas schema backup"))?;
    let backup = path.with_file_name(format!("{name}.pre-relocation-schema-v1.sqlite3"));
    verify_backup(db, &backup, 1)
}

pub(super) fn ensure_not_retired(db: &Connection, key: &str) -> Result<()> {
    let retired: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM scope_moves WHERE from_scope=?1)", [key], |row| row.get(0)).map_err(sql)?;
    if retired {
        return Err(invalid("This writer moved to a verified service address; reopen its moved account instead of reusing the old address"));
    }
    Ok(())
}

impl WorldOutbox {
    /// The proof can only be created by a fresh authenticated Identity response.
    /// The caller keeps room sync paused until this durable migration is acknowledged.
    pub fn relocate_endpoint(&mut self, plan: &AccountRelocation, verified: &VerifiedWorldIdentity) -> Result<()> {
        let (source, target) = scopes(plan)?;
        if !verified.fresh() || verified.scope() != &target {
            return Err(invalid("Destination must freshly verify the same profile and device before moving queued artwork"));
        }
        let source_key = source.key()?;
        let target_key = target.key()?;
        let body = serde_json::to_vec(plan).map_err(|_| invalid("Cannot encode service move"))?;
        if receipt(&self.db, plan, &body, &source_key, &target_key)? {
            return Ok(());
        }
        ensure_not_retired(&self.db, &source_key)?;
        ensure_not_retired(&self.db, &target_key)?;
        verify_empty(&self.db, &target_key)?;
        // The deterministic unique name means retry verifies the prior backup,
        // rather than consuming unbounded disk space after a failed commit.
        backup(&self.db, &self.path, plan)?;
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        if receipt(&tx, plan, &body, &source_key, &target_key)? {
            return Ok(());
        }
        ensure_not_retired(&tx, &source_key)?;
        ensure_not_retired(&tx, &target_key)?;
        verify_empty(&tx, &target_key)?;
        let generation: i64 = tx.query_row("SELECT generation FROM activation WHERE id=1", [], |row| row.get(0)).map_err(sql)?;
        let source_lease = Activation { scope: source, key: source_key.clone(), generation };
        validate_grants(&tx, &source_key)?;
        migrate_operations(&tx, &source_lease, &target_key)?;
        migrate_snapshots(&tx, &source_lease, &target_key)?;
        migrate_viewers(&tx, &source_lease, &target_key)?;
        tx.execute("UPDATE grants SET scope=?1 WHERE scope=?2", params![target_key, source_key]).map_err(sql)?;
        let next = generation.checked_add(1).ok_or_else(|| invalid("Account activation limit"))?;
        tx.execute("UPDATE activation SET scope=NULL,generation=?1 WHERE id=1", [next]).map_err(sql)?;
        tx.execute("INSERT INTO scope_moves VALUES(?1,?2,?3,?4,?5)", params![plan.id.as_str(), source_key, target_key, body, hash(&body)]).map_err(sql)?;
        tx.commit().map_err(sql)
    }
}

fn scopes(plan: &AccountRelocation) -> Result<(WorldScope, WorldScope)> {
    let profile = OwnerId::new(plan.profile.as_str()).map_err(|_| invalid("Service move profile"))?;
    let device = DeviceId::new(plan.device.as_str()).map_err(|_| invalid("Service move device"))?;
    let source = WorldScope::new(&plan.from_endpoint, profile, device)?;
    let target = WorldScope::new(&plan.to_endpoint, profile, device)?;
    if source.endpoint != plan.from_endpoint || target.endpoint != plan.to_endpoint || source == target {
        return Err(invalid("Service move requires two distinct normalized endpoints"));
    }
    Ok((source, target))
}

fn receipt(db: &Connection, plan: &AccountRelocation, body: &[u8], source: &str, target: &str) -> Result<bool> {
    let row: Option<(String, String, Vec<u8>, String)> = db
        .query_row(
            "SELECT CASE WHEN length(from_scope)=64 THEN from_scope ELSE NULL END,CASE WHEN length(to_scope)=64 THEN to_scope ELSE NULL END,CASE WHEN length(body)<=8192 THEN body ELSE NULL END,CASE WHEN length(digest)=64 THEN digest ELSE NULL END FROM scope_moves WHERE id=?1",
            [plan.id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(sql)?;
    let Some((from, to, saved, digest)) = row else { return Ok(false) };
    if from != source || to != target || saved != body || hash(&saved) != digest {
        return Err(invalid("Service move receipt conflicts with the requested journal; preserve both databases"));
    }
    ensure_not_retired(db, target)?;
    Ok(true)
}

fn verify_empty(db: &Connection, key: &str) -> Result<()> {
    let count:u64=db.query_row("SELECT (SELECT count(*) FROM grants WHERE scope=?1)+(SELECT count(*) FROM operations WHERE scope=?1)+(SELECT count(*) FROM snapshots WHERE scope=?1)+(SELECT count(*) FROM viewers WHERE scope=?1)",[key],|row|read_u64(row,0)).map_err(sql)?;
    if count > 0 {
        return Err(invalid("Destination already contains this writer's data; preserve both queues instead of merging or resetting sequences"));
    }
    Ok(())
}

fn validate_grants(db: &Connection, key: &str) -> Result<()> {
    let mut statement = db
        .prepare("SELECT CASE WHEN length(canvas)=32 THEN canvas ELSE NULL END,epoch,next_sequence,server_next,blocked FROM grants WHERE scope=?1 LIMIT ?2")
        .map_err(sql)?;
    let mut rows = statement.query(params![key, (MAX_DIRECTORY + 1) as i64]).map_err(sql)?;
    let mut count = 0usize;
    while let Some(row) = rows.next().map_err(sql)? {
        count = count.saturating_add(1);
        if count > MAX_DIRECTORY {
            return Err(invalid("Canvas migration directory exceeds its bounded limit"));
        }
        CanvasId::new(&row.get::<_, String>(0).map_err(sql)?).map_err(|_| invalid("Canvas migration grant identity"))?;
        let epoch = read_u64(row, 1).map_err(sql)?;
        let next = read_u64(row, 2).map_err(sql)?;
        let server = read_u64(row, 3).map_err(sql)?;
        let blocked: i64 = row.get(4).map_err(sql)?;
        if epoch == 0 || server == 0 || next < server || !(0..=2).contains(&blocked) {
            return Err(invalid("Canvas migration grant is corrupt; preserve source"));
        }
    }
    Ok(())
}

fn migrate_operations(tx: &Transaction<'_>, source: &Activation, target: &str) -> Result<()> {
    let mut statement = tx.prepare("SELECT CASE WHEN length(id)=32 THEN id ELSE NULL END,CASE WHEN length(canvas)=32 THEN canvas ELSE NULL END,epoch,sequence,substr(body,1,?2),CASE WHEN length(hash)=64 THEN hash ELSE NULL END,recovery FROM operations WHERE scope=?1 LIMIT ?3").map_err(sql)?;
    let mut rows = statement.query(params![source.key, (wire::MAX_REQUEST_BYTES + 1) as i64, (MAX_PENDING + 1) as i64]).map_err(sql)?;
    let mut updates = Vec::new();
    let mut total = 0u64;
    while let Some(row) = rows.next().map_err(sql)? {
        if updates.len() as u64 >= MAX_PENDING {
            return Err(invalid("Canvas migration operations exceed their limit"));
        }
        let id: String = row.get(0).map_err(sql)?;
        let canvas = CanvasId::new(&row.get::<_, String>(1).map_err(sql)?).map_err(|_| invalid("Canvas migration operation identity"))?;
        let epoch = read_u64(row, 2).map_err(sql)?;
        let sequence = read_u64(row, 3).map_err(sql)?;
        let bytes: Vec<u8> = row.get(4).map_err(sql)?;
        let digest: String = row.get(5).map_err(sql)?;
        let recovery: Option<i64> = row.get(6).map_err(sql)?;
        let operation = decode(source, canvas, &bytes, &digest)?;
        if operation.id.as_str() != id || operation.authority_epoch != epoch || operation.sequence != sequence {
            return Err(invalid("Canvas migration operation differs from its indexed identity"));
        }
        if let Some(reason) = recovery {
            RecoveryReason::from_code(reason)?;
        }
        total = total.saturating_add(bytes.len() as u64);
        if total > MAX_QUEUE_BYTES {
            return Err(invalid("Canvas migration exceeds the queued byte limit"));
        }
        updates.push((id, checksum(target, canvas, &bytes)));
    }
    drop(rows);
    drop(statement);
    for (id, digest) in updates {
        tx.execute("UPDATE operations SET scope=?1,hash=?2 WHERE scope=?3 AND id=?4", params![target, digest, source.key, id]).map_err(sql)?;
    }
    Ok(())
}

fn migrate_snapshots(tx: &Transaction<'_>, source: &Activation, target: &str) -> Result<()> {
    let mut statement = tx.prepare("SELECT CASE WHEN length(canvas)=32 THEN canvas ELSE NULL END,revision,substr(body,1,?2),CASE WHEN length(hash)=64 THEN hash ELSE NULL END FROM snapshots WHERE scope=?1 LIMIT ?3").map_err(sql)?;
    let mut rows = statement.query(params![source.key, (MAX_SNAPSHOT_BYTES + 1) as i64, (MAX_DIRECTORY + 1) as i64]).map_err(sql)?;
    let mut updates = Vec::new();
    while let Some(row) = rows.next().map_err(sql)? {
        if updates.len() >= MAX_DIRECTORY {
            return Err(invalid("Canvas migration snapshots exceed their directory limit"));
        }
        let canvas = CanvasId::new(&row.get::<_, String>(0).map_err(sql)?).map_err(|_| invalid("Canvas migration snapshot identity"))?;
        let revision = read_u64(row, 1).map_err(sql)?;
        let bytes: Vec<u8> = row.get(2).map_err(sql)?;
        let digest: String = row.get(3).map_err(sql)?;
        if bytes.len() > MAX_SNAPSHOT_BYTES || checksum(&source.key, canvas, &bytes) != digest {
            return Err(invalid("Canvas migration snapshot checksum failed"));
        }
        let state: WorldState = serde_json::from_slice(&bytes).map_err(|_| invalid("Canvas migration snapshot is invalid"))?;
        state.validate().map_err(|_| invalid("Canvas migration snapshot is invalid"))?;
        if state.canvas != canvas || state.revision != revision {
            return Err(invalid("Canvas migration snapshot revision differs from its row"));
        }
        updates.push((canvas, checksum(target, canvas, &bytes)));
    }
    drop(rows);
    drop(statement);
    for (canvas, digest) in updates {
        tx.execute("UPDATE snapshots SET scope=?1,hash=?2 WHERE scope=?3 AND canvas=?4", params![target, digest, source.key, canvas.as_str()]).map_err(sql)?;
    }
    Ok(())
}

fn migrate_viewers(tx: &Transaction<'_>, source: &Activation, target: &str) -> Result<()> {
    let mut statement = tx.prepare("SELECT CASE WHEN length(canvas)=32 THEN canvas ELSE NULL END,substr(body,1,?2),CASE WHEN length(hash)=64 THEN hash ELSE NULL END FROM viewers WHERE scope=?1 LIMIT ?3").map_err(sql)?;
    let mut rows = statement.query(params![source.key, (viewer::MAX_VIEWER_BYTES + 1) as i64, (MAX_DIRECTORY + 1) as i64]).map_err(sql)?;
    let mut updates = Vec::new();
    while let Some(row) = rows.next().map_err(sql)? {
        if updates.len() >= MAX_DIRECTORY {
            return Err(invalid("Canvas migration viewers exceed their directory limit"));
        }
        let canvas = CanvasId::new(&row.get::<_, String>(0).map_err(sql)?).map_err(|_| invalid("Canvas migration viewer identity"))?;
        let bytes: Vec<u8> = row.get(1).map_err(sql)?;
        let digest: String = row.get(2).map_err(sql)?;
        if bytes.len() > viewer::MAX_VIEWER_BYTES || checksum(&source.key, canvas, &bytes) != digest {
            return Err(invalid("Canvas migration viewer checksum failed"));
        }
        let viewer: ViewerState = serde_json::from_slice(&bytes).map_err(|_| invalid("Canvas migration viewer is invalid"))?;
        // Snapshots have already moved, so validate against the target revision.
        let revision: Option<u64> = tx
            .query_row("SELECT revision FROM snapshots WHERE scope=?1 AND canvas=?2", params![target, canvas.as_str()], |row| read_u64(row, 0))
            .optional()
            .map_err(sql)?;
        viewer.validate(canvas, source.scope.profile, revision.unwrap_or(0))?;
        updates.push((canvas, checksum(target, canvas, &bytes)));
    }
    drop(rows);
    drop(statement);
    for (canvas, digest) in updates {
        tx.execute("UPDATE viewers SET scope=?1,hash=?2 WHERE scope=?3 AND canvas=?4", params![target, digest, source.key, canvas.as_str()]).map_err(sql)?;
    }
    Ok(())
}

fn backup(db: &Connection, path: &Path, plan: &AccountRelocation) -> Result<()> {
    let name = path.file_name().and_then(|name| name.to_str()).ok_or_else(|| invalid("Cannot name canvas relocation backup"))?;
    let backup = path.with_file_name(format!("{name}.pre-relocation-{}.sqlite3", plan.id.as_str()));
    verify_backup(db, &backup, SCHEMA)
}

fn verify_backup(db: &Connection, backup: &Path, expected_schema: u32) -> Result<()> {
    if !backup.try_exists().map_err(|_| invalid("Cannot inspect canvas migration backup"))? {
        let destination = backup.to_str().ok_or_else(|| invalid("Canvas migration backup path"))?;
        db.execute("VACUUM INTO ?1", [destination]).map_err(sql)?;
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(backup)
            .and_then(|file| file.sync_all())
            .map_err(|_| invalid("Cannot confirm durable canvas migration backup"))?;
    }
    let verify = Connection::open_with_flags(backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(sql)?;
    let check: String = verify.query_row("PRAGMA quick_check", [], |row| row.get(0)).map_err(sql)?;
    let app: u32 = verify.pragma_query_value(None, "application_id", |row| row.get(0)).map_err(sql)?;
    let version: u32 = verify.pragma_query_value(None, "user_version", |row| row.get(0)).map_err(sql)?;
    if check != "ok" || app != APPLICATION_ID || version != expected_schema {
        return Err(invalid("Canvas migration backup verification failed; preserve source"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
