//! Durable per-account canvas operations. Authentication remains the caller's
//! responsibility; this store never accepts or persists bearer credentials.
//! A login/scope generation protects against delayed work after account changes.
use crate::{Error, Result, normalize_endpoint};
use photocraft_scene::{
    CanvasId, OwnerId,
    shared::{AcceptedOp, DeviceId, Edit, OpId, WorldOp, WorldState},
    world_protocol as wire,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};

const APPLICATION_ID: u32 = 0x4b575151;
const SCHEMA: u32 = 2;
const MAX_PENDING: u64 = 8192;
const MAX_QUEUE_BYTES: u64 = 384 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;
const MAX_PAGE: usize = 256;
const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_DIRECTORY: usize = 16_384;
mod directory;
mod relocation;
mod viewer;
pub use viewer::ViewerState;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldScope {
    pub endpoint: String,
    pub profile: OwnerId,
    pub device: DeviceId,
}
impl WorldScope {
    pub fn new(endpoint: &str, profile: OwnerId, device: DeviceId) -> Result<Self> {
        Ok(Self { endpoint: normalize_endpoint(endpoint)?, profile, device })
    }
    fn key(&self) -> Result<String> {
        if self.endpoint != normalize_endpoint(&self.endpoint)? {
            return Err(invalid("Canvas endpoint must be normalized"));
        }
        Ok(hash(&serde_json::to_vec(self).map_err(|_| invalid("Canvas identity scope"))?))
    }
}

/// Opaque generation lease. Never reuse it after sign-out/account switching.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activation {
    scope: WorldScope,
    key: String,
    generation: i64,
}
impl Activation {
    pub fn scope(&self) -> &WorldScope {
        &self.scope
    }
}

#[derive(Clone, Debug)]
pub struct QueuedOperation {
    activation: Activation,
    pub operation: WorldOp,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingCursor {
    pub canvas: CanvasId,
    pub sequence: u64,
}
#[derive(Clone, Debug)]
pub struct PendingPage {
    pub items: Vec<QueuedOperation>,
    pub next: Option<PendingCursor>,
}

#[derive(Clone, Debug)]
pub struct CachedCanvasPage {
    pub items: Vec<wire::CanvasSummary>,
    pub next: Option<CanvasId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryReason {
    AccessRemoved,
    Conflict,
    Invalid,
}
impl RecoveryReason {
    fn code(self) -> i64 {
        match self {
            Self::AccessRemoved => 1,
            Self::Conflict => 2,
            Self::Invalid => 3,
        }
    }
    fn from_code(value: i64) -> Result<Self> {
        match value {
            1 => Ok(Self::AccessRemoved),
            2 => Ok(Self::Conflict),
            3 => Ok(Self::Invalid),
            _ => Err(invalid("Canvas recovery reason")),
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct RecoveryItem {
    pub operation: WorldOp,
    pub reason: RecoveryReason,
}
#[derive(Clone, Debug, Serialize)]
pub struct RecoveryPage {
    pub items: Vec<RecoveryItem>,
    pub next: Option<OpId>,
}

pub struct WorldOutbox {
    db: Connection,
    path: std::path::PathBuf,
}
fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
fn sql(error: rusqlite::Error) -> Error {
    Error::Storage(error.to_string())
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn checksum(scope: &str, canvas: CanvasId, body: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(scope.as_bytes());
    digest.update(canvas.as_str().as_bytes());
    digest.update(body);
    format!("{digest:x}", digest = digest.finalize())
}
fn integer(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| invalid("Canvas sequence or epoch limit"))
}
fn read_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}
fn active(db: &Connection, lease: &Activation) -> Result<()> {
    let current: (Option<String>, i64) = db.query_row("SELECT scope,generation FROM activation WHERE id=1", [], |r| Ok((r.get(0)?, r.get(1)?))).map_err(sql)?;
    if current.0.as_deref() != Some(lease.key.as_str()) || current.1 != lease.generation {
        return Err(invalid("Account changed; discard this stale network response and retain its queued artwork"));
    }
    Ok(())
}
fn page(limit: usize) -> Result<usize> {
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(invalid("Canvas queue page must contain 1–256 entries"));
    }
    Ok(limit)
}
fn decode(scope: &Activation, canvas: CanvasId, body: &[u8], digest: &str) -> Result<WorldOp> {
    if body.len() > wire::MAX_REQUEST_BYTES || checksum(&scope.key, canvas, body) != digest {
        return Err(invalid("Canvas queue checksum failed; preserve the database for recovery"));
    }
    let op: WorldOp = serde_json::from_slice(body).map_err(|_| invalid("Invalid saved canvas operation"))?;
    op.validate().map_err(|e| invalid(&e.to_string()))?;
    if op.author != scope.scope.profile || op.device != scope.scope.device || op.canvas != canvas {
        return Err(invalid("Canvas operation belongs to another account, writer or canvas"));
    }
    Ok(op)
}

impl WorldOutbox {
    /// Check immediately before scheduling authenticated network work. Responses
    /// must still pass the operation's lease when acknowledged after the request.
    pub fn ensure_current(&self, lease: &Activation) -> Result<()> {
        active(&self.db, lease)
    }
    /// Historical authorization for offline local work on this same account
    /// and writer. A known revoked/conflicting grant is never returned.
    pub fn cached_grant(&self, lease: &Activation, canvas: CanvasId) -> Result<Option<(u64, u64)>> {
        active(&self.db, lease)?;
        let grant: Option<(u64, u64)> = self
            .db
            .query_row("SELECT epoch,server_next FROM grants WHERE scope=?1 AND canvas=?2 AND blocked=0", params![lease.key, canvas.as_str()], |r| {
                Ok((read_u64(r, 0)?, read_u64(r, 1)?))
            })
            .optional()
            .map_err(sql)?;
        if grant.is_some_and(|(epoch, next)| epoch == 0 || next == 0) {
            return Err(invalid("Saved canvas grant is invalid; preserve its queued work"));
        }
        Ok(grant)
    }
    /// Opening invalidates all prior leases and starts signed out. Persisted
    /// drawings/queues survive; no account resumes network work implicitly.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| Error::Storage(e.to_string()))?;
        }
        let mut db = Connection::open(path).map_err(sql)?;
        db.busy_timeout(Duration::from_secs(2)).map_err(sql)?;
        let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0)).map_err(sql)?;
        let app: u32 = db.pragma_query_value(None, "application_id", |r| r.get(0)).map_err(sql)?;
        if (version != 0 && (!matches!(version, 1 | SCHEMA) || app != APPLICATION_ID)) || (version == 0 && app != 0) {
            return Err(invalid("Unsupported canvas queue database; preserve it and upgrade"));
        }
        if version == 0 {
            let tables: u64 = db.query_row("SELECT count(*) FROM sqlite_master WHERE type='table'", [], |r| read_u64(r, 0)).map_err(sql)?;
            if tables != 0 {
                return Err(invalid("Existing database is not a canvas queue; choose a fresh path"));
            }
        }
        db.pragma_update(None, "journal_mode", "DELETE").map_err(sql)?;
        db.pragma_update(None, "synchronous", "EXTRA").map_err(sql)?;
        db.pragma_update(None, "trusted_schema", false).map_err(sql)?;
        let mode: String = db.pragma_query_value(None, "journal_mode", |r| r.get(0)).map_err(sql)?;
        let sync: u32 = db.pragma_query_value(None, "synchronous", |r| r.get(0)).map_err(sql)?;
        if mode != "delete" || sync != 3 {
            return Err(invalid("Required canvas queue durability is unavailable"));
        }
        let size: u64 = db.pragma_query_value(None, "page_size", |r| read_u64(r, 0)).map_err(sql)?;
        let pages = (512u64 * 1024 * 1024).checked_div(size).ok_or_else(|| invalid("Canvas queue page size"))?;
        db.pragma_update(None, "max_page_count", integer(pages)?).map_err(sql)?;
        let check: String = db.query_row("PRAGMA quick_check", [], |r| r.get(0)).map_err(sql)?;
        if check != "ok" {
            return Err(invalid("Canvas queue integrity failed; preserve its recovery data"));
        }
        if version == 1 {
            relocation::backup_schema_one(&db, path)?;
        }
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        if version == 0 {
            tx.execute_batch("CREATE TABLE activation(id INTEGER PRIMARY KEY CHECK(id=1),scope TEXT,generation INTEGER NOT NULL);
              INSERT INTO activation VALUES(1,NULL,0);
              CREATE TABLE grants(scope TEXT NOT NULL,canvas TEXT NOT NULL,epoch INTEGER NOT NULL,next_sequence INTEGER NOT NULL,server_next INTEGER NOT NULL,blocked INTEGER NOT NULL,PRIMARY KEY(scope,canvas));
              CREATE TABLE operations(scope TEXT NOT NULL,id TEXT NOT NULL,canvas TEXT NOT NULL,epoch INTEGER NOT NULL,sequence INTEGER NOT NULL,body BLOB NOT NULL,hash TEXT NOT NULL,recovery INTEGER,PRIMARY KEY(scope,id));
              CREATE INDEX operations_pending ON operations(scope,recovery,canvas,sequence);
              CREATE TABLE snapshots(scope TEXT NOT NULL,canvas TEXT NOT NULL,revision INTEGER NOT NULL,body BLOB NOT NULL,hash TEXT NOT NULL,PRIMARY KEY(scope,canvas));").map_err(sql)?;
            tx.execute_batch("CREATE TABLE viewers(scope TEXT NOT NULL,canvas TEXT NOT NULL,body BLOB NOT NULL,hash TEXT NOT NULL,PRIMARY KEY(scope,canvas));")
                .map_err(sql)?;
            tx.pragma_update(None, "application_id", APPLICATION_ID).map_err(sql)?;
            tx.pragma_update(None, "user_version", SCHEMA).map_err(sql)?;
        }
        if version < SCHEMA {
            relocation::initialize(&tx)?;
        } else {
            let retirement_table: bool =
                tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='scope_moves')", [], |row| row.get(0)).map_err(sql)?;
            if !retirement_table {
                return Err(invalid("Canvas migration receipts are missing; preserve this database instead of reusing an old writer"));
            }
        }
        tx.pragma_update(None, "user_version", SCHEMA).map_err(sql)?;
        let generation: i64 = tx.query_row("SELECT generation FROM activation WHERE id=1", [], |r| r.get(0)).map_err(sql)?;
        let next = generation.checked_add(1).ok_or_else(|| invalid("Account activation limit"))?;
        tx.execute("UPDATE activation SET scope=NULL,generation=?1 WHERE id=1", [next]).map_err(sql)?;
        tx.commit().map_err(sql)?;
        Ok(Self { db, path: path.to_path_buf() })
    }

    pub fn activate(&mut self, scope: WorldScope) -> Result<Activation> {
        let key = scope.key()?;
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        relocation::ensure_not_retired(&tx, &key)?;
        let generation: i64 = tx.query_row("SELECT generation FROM activation WHERE id=1", [], |r| r.get(0)).map_err(sql)?;
        let generation = generation.checked_add(1).ok_or_else(|| invalid("Account activation limit"))?;
        tx.execute("UPDATE activation SET scope=?1,generation=?2 WHERE id=1", params![key, generation]).map_err(sql)?;
        tx.commit().map_err(sql)?;
        Ok(Activation { scope, key, generation })
    }
    pub fn sign_out(&mut self) -> Result<()> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        let generation: i64 = tx.query_row("SELECT generation FROM activation WHERE id=1", [], |r| r.get(0)).map_err(sql)?;
        let next = generation.checked_add(1).ok_or_else(|| invalid("Account activation limit"))?;
        tx.execute("UPDATE activation SET scope=NULL,generation=?1 WHERE id=1", [next]).map_err(sql)?;
        tx.commit().map_err(sql)
    }

    /// Supply only an authenticated current grant and canonical next writer
    /// sequence. New epochs preserve old operations as recovery variants.
    pub fn grant(&mut self, lease: &Activation, canvas: CanvasId, epoch: u64, server_next_sequence: u64) -> Result<()> {
        if epoch == 0 || server_next_sequence == 0 {
            return Err(invalid("Canvas grant epoch/sequence must be positive"));
        }
        let epoch = integer(epoch)?;
        let server_next = integer(server_next_sequence)?;
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        active(&tx, lease)?;
        let old: Option<(i64, i64, i64)> = tx
            .query_row("SELECT epoch,server_next,blocked FROM grants WHERE scope=?1 AND canvas=?2", params![lease.key, canvas.as_str()], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()
            .map_err(sql)?;
        if let Some((prior_epoch, prior_next, blocked)) = old {
            if epoch < prior_epoch || server_next < prior_next || (epoch == prior_epoch && blocked == 1) {
                return Err(invalid("Stale or revoked canvas grant; keep rejected drawings for recovery"));
            }
            if epoch != prior_epoch {
                tx.execute("UPDATE operations SET recovery=1 WHERE scope=?1 AND canvas=?2 AND recovery IS NULL", params![lease.key, canvas.as_str()])
                    .map_err(sql)?;
            }
        }
        let max_pending: Option<i64> = tx
            .query_row(
                "SELECT max(sequence) FROM operations WHERE scope=?1 AND canvas=?2 AND epoch=?3 AND recovery IS NULL",
                params![lease.key, canvas.as_str(), epoch],
                |r| r.get(0),
            )
            .map_err(sql)?;
        let next = match max_pending {
            Some(value) => server_next.max(value.checked_add(1).ok_or_else(|| invalid("Canvas writer sequence limit"))?),
            None => server_next,
        };
        // A normal authenticated refresh is not explicit conflict recovery.
        // It may advance the canonical prefix, but must keep this writer paused.
        let blocked = if old.is_some_and(|(prior_epoch, _, blocked)| prior_epoch == epoch && blocked == 2) { 2 } else { 0 };
        tx.execute("INSERT INTO grants VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(scope,canvas) DO UPDATE SET epoch=excluded.epoch,next_sequence=excluded.next_sequence,server_next=excluded.server_next,blocked=excluded.blocked",params![lease.key,canvas.as_str(),epoch,next,server_next,blocked]).map_err(sql)?;
        tx.commit().map_err(sql)
    }

    /// Explicitly resume new edits after preserving conflicting work in recovery.
    /// The caller must obtain a fresh authenticated Info response and durably save
    /// all history through that response's revision before invoking this method.
    /// This never changes, deletes, rekeys or republishes a quarantined operation.
    pub fn resume_after_recovery(&mut self, lease: &Activation, canvas: CanvasId, epoch: u64, server_next: u64) -> Result<()> {
        if epoch == 0 || server_next == 0 {
            return Err(invalid("Recovery requires a positive authenticated epoch and writer sequence"));
        }
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        active(&tx, lease)?;
        let saved: Option<(u64, u64, i64)> = tx
            .query_row("SELECT epoch,server_next,blocked FROM grants WHERE scope=?1 AND canvas=?2", params![lease.key, canvas.as_str()], |row| {
                Ok((read_u64(row, 0)?, read_u64(row, 1)?, row.get(2)?))
            })
            .optional()
            .map_err(sql)?;
        if saved != Some((epoch, server_next, 2)) {
            return Err(invalid("Only an explicitly refreshed conflict queue can resume; revoked access requires a new service grant"));
        }
        let pending: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM operations WHERE scope=?1 AND canvas=?2 AND recovery IS NULL)",
                params![lease.key, canvas.as_str()],
                |row| row.get(0),
            )
            .map_err(sql)?;
        if pending {
            return Err(invalid("Preserve all conflicting pending operations in recovery before resuming"));
        }
        let canonical = read_snapshot(&tx, lease, canvas)?.ok_or_else(|| invalid("Save the fresh accepted canvas history before resuming"))?;
        if canonical.next_sequence(lease.scope.device).map_err(|e| invalid(&e.to_string()))? != server_next {
            return Err(invalid("Recovery grant is ahead of the durable canonical writer sequence"));
        }
        tx.execute("UPDATE grants SET next_sequence=?3,blocked=0 WHERE scope=?1 AND canvas=?2", params![lease.key, canvas.as_str(), integer(server_next)?])
            .map_err(sql)?;
        tx.commit().map_err(sql)
    }

    pub fn enqueue(&mut self, lease: &Activation, canvas: CanvasId, action: Edit) -> Result<WorldOp> {
        let id = OpId::new(&crate::store::random_hex(16)?).map_err(|e| invalid(&e.to_string()))?;
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        active(&tx, lease)?;
        let grant: Option<(u64, u64, i64)> = tx
            .query_row("SELECT epoch,next_sequence,blocked FROM grants WHERE scope=?1 AND canvas=?2", params![lease.key, canvas.as_str()], |r| {
                Ok((read_u64(r, 0)?, read_u64(r, 1)?, r.get(2)?))
            })
            .optional()
            .map_err(sql)?;
        let (epoch, sequence, blocked) = grant.ok_or_else(|| invalid("Canvas has no authenticated grant"))?;
        if blocked != 0 {
            return Err(invalid("Canvas queue needs access or conflict recovery before adding shared edits"));
        }
        let next = sequence.checked_add(1).ok_or_else(|| invalid("Canvas sequence limit"))?;
        let next = integer(next)?;
        let operation = WorldOp { id, canvas, author: lease.scope.profile, device: lease.scope.device, sequence, authority_epoch: epoch, action };
        operation.validate().map_err(|e| invalid(&e.to_string()))?;
        let request = wire::Request { version: wire::VERSION, action: wire::Action::Append { operation: operation.clone() } };
        let wire_bytes = serde_json::to_vec(&request).map_err(|_| invalid("Canvas operation encoding"))?;
        if wire_bytes.len() > wire::MAX_REQUEST_BYTES {
            return Err(invalid("Canvas operation exceeds the service message limit"));
        }
        let bytes = serde_json::to_vec(&operation).map_err(|_| invalid("Canvas operation encoding"))?;
        let (count, total): (u64, u64) =
            tx.query_row("SELECT count(*),coalesce(sum(length(body)),0) FROM operations", [], |r| Ok((read_u64(r, 0)?, read_u64(r, 1)?))).map_err(sql)?;
        if count >= MAX_PENDING || total.saturating_add(bytes.len() as u64) > MAX_QUEUE_BYTES {
            return Err(Error::Busy);
        }
        tx.execute(
            "INSERT INTO operations VALUES(?1,?2,?3,?4,?5,?6,?7,NULL)",
            params![lease.key, id.as_str(), canvas.as_str(), integer(epoch)?, integer(sequence)?, bytes, checksum(&lease.key, canvas, &bytes)],
        )
        .map_err(sql)?;
        tx.execute("UPDATE grants SET next_sequence=?3 WHERE scope=?1 AND canvas=?2", params![lease.key, canvas.as_str(), next]).map_err(sql)?;
        tx.commit().map_err(sql)?;
        Ok(operation)
    }

    pub fn pending(&self, lease: &Activation, limit: usize) -> Result<Vec<QueuedOperation>> {
        Ok(self.pending_page(lease, None, limit)?.items)
    }
    /// Cursor pages preserve per-canvas writer order, including while offline
    /// when acknowledgment cannot drain the first page. No pending row is lost.
    pub fn pending_page(&self, lease: &Activation, after: Option<PendingCursor>, limit: usize) -> Result<PendingPage> {
        active(&self.db, lease)?;
        let limit = page(limit)?;
        let after_canvas = after.map_or_else(String::new, |cursor| cursor.canvas.as_str().to_owned());
        let after_sequence = integer(after.map_or(0, |cursor| cursor.sequence))?;
        let mut stmt = self
            .db
            .prepare("SELECT id,canvas,sequence,substr(body,1,?5),hash FROM operations WHERE scope=?1 AND recovery IS NULL AND (canvas>?2 OR (canvas=?2 AND sequence>?3)) ORDER BY canvas,sequence LIMIT ?4")
            .map_err(sql)?;
        let rows = stmt
            .query_map(params![lease.key, after_canvas, after_sequence, (limit + 1) as i64, (wire::MAX_REQUEST_BYTES + 1) as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, read_u64(r, 2)?, r.get::<_, Vec<u8>>(3)?, r.get::<_, String>(4)?))
            })
            .map_err(sql)?;
        let mut result = Vec::new();
        let mut total = 0usize;
        let mut more = false;
        for row in rows {
            let (id, canvas, sequence, bytes, hash) = row.map_err(sql)?;
            if result.len() == limit || total.saturating_add(bytes.len()) > MAX_PAGE_BYTES {
                more = true;
                break;
            }
            total += bytes.len();
            let canvas = CanvasId::new(&canvas).map_err(|e| invalid(&e.to_string()))?;
            let operation = decode(lease, canvas, &bytes, &hash)?;
            if operation.id.as_str() != id || operation.sequence != sequence {
                return Err(invalid("Canvas queue order or identity differs from the saved operation"));
            }
            result.push(QueuedOperation { activation: lease.clone(), operation });
        }
        let next = if more { result.last().map(|item| PendingCursor { canvas: item.operation.canvas, sequence: item.operation.sequence }) } else { None };
        Ok(PendingPage { items: result, next })
    }

    pub fn acknowledge(&mut self, queued: &QueuedOperation, accepted: &AcceptedOp) -> Result<()> {
        if accepted.revision == 0 || accepted.revision > i64::MAX as u64 || accepted.op != queued.operation {
            return Err(invalid("Service acknowledged different canvas content; keep the queued original"));
        }
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        active(&tx, &queued.activation)?;
        check_queued(&tx, queued)?;
        let durable = read_snapshot(&tx, &queued.activation, accepted.op.canvas)?
            .ok_or_else(|| invalid("Download and save the accepted canvas revision before clearing its local queue"))?;
        if durable.receipt(accepted.op.id) != Some(accepted) {
            return Err(invalid("Accepted operation is ahead of durable canvas history; retain it until catch-up is saved"));
        }
        let next = integer(accepted.op.sequence.checked_add(1).ok_or_else(|| invalid("Canvas sequence limit"))?)?;
        tx.execute(
            "UPDATE grants SET server_next=max(server_next,?3),next_sequence=max(next_sequence,?3) WHERE scope=?1 AND canvas=?2",
            params![queued.activation.key, accepted.op.canvas.as_str(), next],
        )
        .map_err(sql)?;
        tx.execute("DELETE FROM operations WHERE scope=?1 AND id=?2", params![queued.activation.key, accepted.op.id.as_str()]).map_err(sql)?;
        tx.commit().map_err(sql)
    }

    pub fn reject(&mut self, queued: &QueuedOperation, reason: RecoveryReason) -> Result<()> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        active(&tx, &queued.activation)?;
        check_queued(&tx, queued)?;
        quarantine(&tx, &queued.activation, queued.operation.canvas, reason)?;
        tx.commit().map_err(sql)
    }
    pub fn revoke(&mut self, lease: &Activation, canvas: CanvasId) -> Result<()> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        active(&tx, lease)?;
        quarantine(&tx, lease, canvas, RecoveryReason::AccessRemoved)?;
        tx.commit().map_err(sql)
    }
    pub fn recovery(&self, lease: &Activation, limit: usize) -> Result<Vec<RecoveryItem>> {
        active(&self.db, lease)?;
        let limit = page(limit)?;
        let mut stmt = self
            .db
            .prepare(
                "SELECT canvas,substr(body,1,?3),hash,recovery FROM operations WHERE scope=?1 AND recovery IS NOT NULL ORDER BY canvas,epoch,sequence LIMIT ?2",
            )
            .map_err(sql)?;
        let rows = stmt
            .query_map(params![lease.key, limit as i64, (wire::MAX_REQUEST_BYTES + 1) as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?))
            })
            .map_err(sql)?;
        let mut result = Vec::new();
        let mut total = 0usize;
        for row in rows {
            let (canvas, bytes, hash, reason) = row.map_err(sql)?;
            if total.saturating_add(bytes.len()) > MAX_PAGE_BYTES {
                break;
            }
            total += bytes.len();
            let canvas = CanvasId::new(&canvas).map_err(|e| invalid(&e.to_string()))?;
            result.push(RecoveryItem { operation: decode(lease, canvas, &bytes, &hash)?, reason: RecoveryReason::from_code(reason)? });
        }
        Ok(result)
    }

    /// Stable operation-ID cursor exposes every retained recovery variant. A
    /// page has both an item limit and an 8 MiB JSON byte budget.
    pub fn recovery_page(&self, lease: &Activation, after: Option<OpId>, limit: usize) -> Result<RecoveryPage> {
        active(&self.db, lease)?;
        let limit = page(limit)?;
        let mut stmt = self
            .db
            .prepare("SELECT id,canvas,substr(body,1,?4),hash,recovery FROM operations WHERE scope=?1 AND recovery IS NOT NULL AND id>?2 ORDER BY id LIMIT ?3")
            .map_err(sql)?;
        let rows = stmt
            .query_map(params![lease.key, after.as_ref().map_or("", OpId::as_str), (limit + 1) as i64, (wire::MAX_REQUEST_BYTES + 1) as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Vec<u8>>(2)?, r.get::<_, String>(3)?, r.get::<_, i64>(4)?))
            })
            .map_err(sql)?;
        let mut items = Vec::new();
        let mut total = 0usize;
        let mut more = false;
        for row in rows {
            let (id, canvas, bytes, hash, reason) = row.map_err(sql)?;
            if items.len() == limit || total.saturating_add(bytes.len()) > MAX_PAGE_BYTES {
                more = true;
                break;
            }
            total += bytes.len();
            let canvas = CanvasId::new(&canvas).map_err(|e| invalid(&e.to_string()))?;
            let operation = decode(lease, canvas, &bytes, &hash)?;
            if operation.id.as_str() != id {
                return Err(invalid("Recovery operation identity does not match its row"));
            }
            items.push(RecoveryItem { operation, reason: RecoveryReason::from_code(reason)? });
        }
        let next = if more { items.last().map(|item| item.operation.id) } else { None };
        Ok(RecoveryPage { items, next })
    }

    pub fn save_snapshot(&mut self, lease: &Activation, state: &WorldState) -> Result<()> {
        state.validate().map_err(|e| invalid(&e.to_string()))?;
        let bytes = serde_json::to_vec(state).map_err(|_| invalid("Canvas snapshot encoding"))?;
        if bytes.len() > MAX_SNAPSHOT_BYTES {
            return Err(invalid("Canvas snapshot exceeds local cache limit"));
        }
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        active(&tx, lease)?;
        let allowed: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM grants WHERE scope=?1 AND canvas=?2 AND blocked IN (0,2))",
                params![lease.key, state.canvas.as_str()],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if !allowed {
            return Err(invalid("Canvas has no current grant for this account"));
        }
        if let Some(prior) = read_snapshot(&tx, lease, state.canvas)? {
            if prior.revision > state.revision {
                return Err(invalid("Stale canvas snapshot; retain newer local state"));
            }
            let prefix = state.replay_to(prior.revision).map_err(|e| invalid(&e.to_string()))?;
            if prefix.canonical_hash().map_err(|e| invalid(&e.to_string()))? != prior.canonical_hash().map_err(|e| invalid(&e.to_string()))? {
                return Err(invalid("Canvas snapshot conflicts with its saved immutable history; retain both recovery sources"));
            }
        }
        tx.execute("INSERT INTO snapshots VALUES(?1,?2,?3,?4,?5) ON CONFLICT(scope,canvas) DO UPDATE SET revision=excluded.revision,body=excluded.body,hash=excluded.hash",params![lease.key,state.canvas.as_str(),integer(state.revision)?,bytes,checksum(&lease.key,state.canvas,&bytes)]).map_err(sql)?;
        tx.commit().map_err(sql)
    }
    pub fn load_snapshot(&self, lease: &Activation, canvas: CanvasId) -> Result<Option<WorldState>> {
        active(&self.db, lease)?;
        read_snapshot(&self.db, lease, canvas)
    }

    /// Previously downloaded art stays discoverable offline for this exact
    /// account/writer. This directory is never a new service membership grant.
    pub fn cached_summaries(&self, lease: &Activation, after: Option<CanvasId>, limit: usize) -> Result<CachedCanvasPage> {
        active(&self.db, lease)?;
        if limit == 0 || limit > 128 {
            return Err(invalid("Cached canvas page must contain 1 to 128 rows"));
        }
        let mut query = self.db.prepare("SELECT s.canvas,s.revision,g.epoch,length(s.body),substr(s.body,1,?3),s.hash FROM snapshots s JOIN grants g ON g.scope=s.scope AND g.canvas=s.canvas WHERE s.scope=?1 AND g.blocked IN(0,2) AND (?2 IS NULL OR s.canvas>?2) ORDER BY s.canvas LIMIT ?4").map_err(sql)?;
        let mut rows =
            query.query(params![lease.key, after.map(|id| id.as_str().to_owned()), (MAX_SNAPSHOT_BYTES + 1) as i64, (limit + 1) as i64]).map_err(sql)?;
        let mut items = Vec::new();
        let mut scanned = 0usize;
        let mut more = false;
        while let Some(row) = rows.next().map_err(sql)? {
            let length = usize::try_from(read_u64(row, 3).map_err(sql)?).map_err(|_| invalid("Cached canvas size"))?;
            if items.len() >= limit || (!items.is_empty() && scanned.saturating_add(length) > MAX_PAGE_BYTES) {
                more = true;
                break;
            }
            if length > MAX_SNAPSHOT_BYTES {
                return Err(invalid("Cached canvas exceeds the snapshot limit; preserve recovery data"));
            }
            let canvas = CanvasId::new(&row.get::<_, String>(0).map_err(sql)?).map_err(|_| invalid("Cached canvas identity"))?;
            let revision = read_u64(row, 1).map_err(sql)?;
            let epoch = read_u64(row, 2).map_err(sql)?;
            let bytes: Vec<u8> = row.get(4).map_err(sql)?;
            let hash: String = row.get(5).map_err(sql)?;
            if bytes.len() != length || checksum(&lease.key, canvas, &bytes) != hash {
                return Err(invalid("Cached canvas checksum failed; preserve recovery data"));
            }
            // Ignoring the retained drawing arrays avoids allocating a whole
            // scene just to show names in the offline gallery.
            #[derive(Deserialize)]
            struct Summary {
                canvas: CanvasId,
                owner: OwnerId,
                name: String,
                initial_name: String,
                #[serde(default)]
                import_origin: Option<Box<photocraft_scene::shared::ImportOrigin>>,
                revision: u64,
            }
            let saved: Summary = serde_json::from_slice(&bytes).map_err(|_| invalid("Cached canvas metadata is invalid"))?;
            if saved.canvas != canvas || saved.revision != revision || epoch == 0 {
                return Err(invalid("Cached canvas metadata does not match its authorized row"));
            }
            photocraft_scene::validate_name(&saved.name).map_err(|_| invalid("Cached canvas name"))?;
            photocraft_scene::validate_name(&saved.initial_name).map_err(|_| invalid("Cached canvas initial name"))?;
            scanned = scanned.saturating_add(length);
            if let Some(origin) = &saved.import_origin {
                origin.validate(canvas, saved.owner).map_err(|_| invalid("Cached archive origin"))?;
                if revision < origin.source_revision {
                    return Err(invalid("Cached archive download is incomplete; reconnect to finish it. Saved artwork is preserved"));
                }
            }
            items.push(wire::CanvasSummary {
                canvas,
                owner: saved.owner,
                name: saved.name,
                initial_name: saved.initial_name,
                import_origin: saved.import_origin,
                revision,
                epoch,
            });
        }
        let next = if more { items.last().map(|item| item.canvas) } else { None };
        Ok(CachedCanvasPage { items, next })
    }
}

fn read_snapshot(db: &Connection, lease: &Activation, canvas: CanvasId) -> Result<Option<WorldState>> {
    let row: Option<(Vec<u8>, String)> = db
        .query_row(
            "SELECT substr(body,1,?3),hash FROM snapshots WHERE scope=?1 AND canvas=?2",
            params![lease.key, canvas.as_str(), (MAX_SNAPSHOT_BYTES + 1) as i64],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sql)?;
    let Some((bytes, hash)) = row else { return Ok(None) };
    if bytes.len() > MAX_SNAPSHOT_BYTES || checksum(&lease.key, canvas, &bytes) != hash {
        return Err(invalid("Canvas snapshot checksum failed; preserve recovery data"));
    }
    let state: WorldState = serde_json::from_slice(&bytes).map_err(|_| invalid("Saved canvas snapshot is invalid"))?;
    state.validate().map_err(|e| invalid(&e.to_string()))?;
    if state.canvas != canvas {
        return Err(invalid("Saved snapshot belongs to another canvas"));
    }
    Ok(Some(state))
}

fn check_queued(db: &Connection, queued: &QueuedOperation) -> Result<()> {
    let lease = &queued.activation;
    let op = &queued.operation;
    let row: Option<(Vec<u8>, String, Option<i64>)> = db
        .query_row(
            "SELECT substr(body,1,?3),hash,recovery FROM operations WHERE scope=?1 AND id=?2",
            params![lease.key, op.id.as_str(), (wire::MAX_REQUEST_BYTES + 1) as i64],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sql)?;
    let (bytes, hash, recovery) = row.ok_or_else(|| invalid("Canvas operation is no longer pending"))?;
    if recovery.is_some() || decode(lease, op.canvas, &bytes, &hash)? != *op {
        return Err(invalid("Canvas acknowledgment is stale or conflicts with retained recovery data"));
    }
    Ok(())
}
fn quarantine(db: &Connection, lease: &Activation, canvas: CanvasId, reason: RecoveryReason) -> Result<()> {
    db.execute("UPDATE operations SET recovery=?3 WHERE scope=?1 AND canvas=?2 AND recovery IS NULL", params![lease.key, canvas.as_str(), reason.code()])
        .map_err(sql)?;
    let block = if reason == RecoveryReason::AccessRemoved { 1 } else { 2 };
    db.execute("UPDATE grants SET blocked=?3 WHERE scope=?1 AND canvas=?2", params![lease.key, canvas.as_str(), block]).map_err(sql)?;
    Ok(())
}
