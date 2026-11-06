use std::path::Path;
use std::time::{Duration, SystemTime};

use photocraft_doc::Document;
use photocraft_format::{LoadOptions, PcraftWriter, SaveOptions};
use photocraft_party::contracts::{
    AssetHash, AssetRef, CheckpointId, CheckpointRef, DeviceId, EventEnvelope, EventId, MAX_REPLAY_EVENTS, MAX_REPLAY_MS, ParticipantId, ProjectId, ReplayStep,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::{
    DraftInfo, Error, LoadedDraft, MAX_CHECKPOINT_BYTES, MAX_PROJECTS, MAX_RECORD_BYTES, MAX_REPLAY_PAGE, ReplayRecord, Result, SCHEMA_VERSION, SaveAck,
    SaveRequest, StoreOptions, Usage,
};

const CHUNK_BYTES: usize = 64 * 1024;
const APPLICATION_ID: u32 = 0x4b4f4d41;
const MAX_PAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_RECOVERY_SEARCH: usize = 256;
const MAX_OBJECT_CHUNKS: usize = 131_072;

/// Observation points for crash tests; callers must not report save success at BeforeCommit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStage {
    BeforeCommit,
    AfterCommit,
}

pub struct Store {
    db: Connection,
    writer: PcraftWriter,
    options: StoreOptions,
}

fn sql_error(error: rusqlite::Error) -> Error {
    match &error {
        rusqlite::Error::SqliteFailure(code, _) if code.extended_code & 0xff == 13 => Error::Full,
        _ => Error::Storage(error.to_string()),
    }
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn random_hex() -> Result<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| Error::Storage(format!("Cannot allocate a secure local identity: {e}")))?;
    if bytes.iter().all(|v| *v == 0) {
        return Err(Error::Storage("Secure random source returned an invalid identity; retry".into()));
    }
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub fn fresh_project_id() -> Result<ProjectId> {
    ProjectId::new(&random_hex()?).map_err(|e| Error::Invalid(e.to_string()))
}

pub fn fresh_event_id() -> Result<EventId> {
    EventId::new(&random_hex()?).map_err(|e| Error::Invalid(e.to_string()))
}

fn timestamp() -> String {
    let now: chrono::DateTime<chrono::Utc> = SystemTime::now().into();
    now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn nonnegative(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| Error::Corrupt(format!("negative {field}")))
}

impl Store {
    /// `path` names the SQLite database, not a directory. Parent directories are created.
    /// Keep real data under local application data, outside synced/network folders.
    pub fn open(path: impl AsRef<Path>, options: StoreOptions) -> Result<Self> {
        let path = path.as_ref();
        if options.quota_bytes < 64 * 1024 || options.quota_bytes > 64 * 1024 * 1024 * 1024 {
            return Err(Error::Invalid("database quota must be 64 KiB through 64 GiB".into()));
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| Error::Storage(e.to_string()))?;
        }
        let mut db = Connection::open(path).map_err(sql_error)?;
        db.busy_timeout(Duration::from_secs(2)).map_err(sql_error)?;
        let version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0)).map_err(sql_error)?;
        let app_id: u32 = db.pragma_query_value(None, "application_id", |row| row.get(0)).map_err(sql_error)?;
        if version != 0 && version != SCHEMA_VERSION {
            return Err(Error::Version(version));
        }
        if app_id != 0 && app_id != APPLICATION_ID {
            return Err(Error::Corrupt("database belongs to another application".into()));
        }
        if version == 0 {
            let tables: u32 =
                db.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'", [], |r| r.get(0)).map_err(sql_error)?;
            if tables != 0 {
                return Err(Error::Corrupt("unversioned nonempty database; original preserved".into()));
            }
        }
        db.pragma_update(None, "journal_mode", "DELETE").map_err(sql_error)?;
        db.pragma_update(None, "synchronous", "EXTRA").map_err(sql_error)?;
        db.pragma_update(None, "foreign_keys", "ON").map_err(sql_error)?;
        db.pragma_update(None, "trusted_schema", "OFF").map_err(sql_error)?;
        let journal: String = db.pragma_query_value(None, "journal_mode", |r| r.get(0)).map_err(sql_error)?;
        let synchronous: u32 = db.pragma_query_value(None, "synchronous", |r| r.get(0)).map_err(sql_error)?;
        let foreign_keys: u32 = db.pragma_query_value(None, "foreign_keys", |r| r.get(0)).map_err(sql_error)?;
        if !journal.eq_ignore_ascii_case("delete") || synchronous != 3 || foreign_keys != 1 {
            return Err(Error::Storage("SQLite durability settings could not be enabled".into()));
        }
        if version == 0 {
            initialize(&mut db)?;
        }
        let page_size: u32 = db.pragma_query_value(None, "page_size", |r| r.get(0)).map_err(sql_error)?;
        if page_size == 0 {
            return Err(Error::Corrupt("zero SQLite page size".into()));
        }
        let requested_pages = options.quota_bytes / u64::from(page_size);
        db.pragma_update(None, "max_page_count", requested_pages as i64).map_err(sql_error)?;
        let actual_pages: u32 = db.pragma_query_value(None, "max_page_count", |r| r.get(0)).map_err(sql_error)?;
        if u64::from(actual_pages) > requested_pages {
            return Err(Error::Full);
        }
        let mut writer = PcraftWriter::new();
        writer.set_compressed_cache_budget(64 * 1024 * 1024);
        Ok(Self { db, writer, options })
    }

    pub fn save(&mut self, request: &SaveRequest) -> Result<SaveAck> {
        self.save_observed(request, |_| {})
    }

    /// Same transaction as `save`, with deterministic observation for process-kill tests.
    /// The callback cannot convert a failed commit into a success.
    pub fn save_observed(&mut self, request: &SaveRequest, mut observe: impl FnMut(SaveStage)) -> Result<SaveAck> {
        validate_request(request)?;
        let record = encode_record(&request.event)?;
        let (checkpoint, _) = self.writer.save_zip(&request.document, &SaveOptions::default()).map_err(|e| Error::Invalid(e.to_string()))?;
        if checkpoint.len() > MAX_CHECKPOINT_BYTES {
            return Err(Error::Invalid("editable checkpoint exceeds 64 MiB; save an external copy before continuing".into()));
        }
        // Encoded size does not bound a compressed blob's decoded size, the manifest,
        // or the loader's aggregate memory budget. Use the same authoritative bounded
        // reader as reopen before any transaction can publish an acknowledgment. Drop
        // this temporary document immediately; all decoding stays on the native worker.
        drop(photocraft_format::load_from_bytes_with(&checkpoint, &load_options()).map_err(|error| {
            Error::Invalid(format!("editable checkpoint cannot be reopened within local draft limits: {error}; save an external copy before continuing"))
        })?);
        let checkpoint_hash = hash(&checkpoint);
        let record_hash = hash(&record);
        let fingerprint = request_fingerprint(request, &checkpoint_hash, &record_hash);
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql_error)?;
        if let Some(ack) = existing_receipt(&tx, request.request_id, &fingerprint)? {
            return Ok(ack);
        }
        let previous: Option<(i64, i64)> = tx
            .query_row("SELECT head_revision, elapsed_ms FROM projects WHERE id=?1", [request.project.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(sql_error)?;
        let (head, previous_ms) = previous.map(|(a, b)| Ok((nonnegative(a, "head revision")?, nonnegative(b, "elapsed time")?))).transpose()?.unwrap_or((0, 0));
        if head != request.expected_revision || request.event.elapsed_ms < previous_ms {
            return Err(Error::Conflict);
        }
        if head >= MAX_REPLAY_EVENTS {
            return Err(Error::Invalid("replay event limit reached; retain this draft and begin a new one".into()));
        }
        if head == 0 {
            let count: u32 = tx.query_row("SELECT count(*) FROM projects", [], |r| r.get(0)).map_err(sql_error)?;
            if u64::from(count) >= MAX_PROJECTS {
                return Err(Error::Invalid("local draft limit reached; no drafts were deleted".into()));
            }
        }
        let (actor, device, sequence): (String, String, i64) = tx
            .query_row("SELECT substr(actor,1,33), substr(device,1,33), sequence FROM identity WHERE singleton=1", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(sql_error)?;
        let sequence = nonnegative(sequence, "device sequence")?
            .checked_add(1)
            .filter(|v| *v <= i64::MAX as u64)
            .ok_or_else(|| Error::Invalid("device sequence exhausted".into()))?;
        let revision = head + 1;
        let created = timestamp();
        store_object(&tx, &checkpoint_hash, &checkpoint)?;
        store_object(&tx, &record_hash, &record)?;
        let checkpoint_ref = object_ref(&checkpoint_hash, checkpoint.len())?;
        let payload_ref = object_ref(&record_hash, record.len())?;
        let envelope = make_envelope(request, &actor, &device, sequence, checkpoint_ref, payload_ref)?;
        let envelope_json = serde_json::to_vec(&envelope).map_err(|e| Error::Invalid(e.to_string()))?;
        if head == 0 {
            tx.execute(
                "INSERT INTO projects(id,title,head_revision,elapsed_ms,updated_at) VALUES(?1,?2,?3,?4,?5)",
                params![request.project.as_str(), request.title, revision as i64, request.event.elapsed_ms as i64, created],
            )
            .map_err(sql_error)?;
        } else {
            tx.execute(
                "UPDATE projects SET title=?2,head_revision=?3,elapsed_ms=?4,updated_at=?5 WHERE id=?1",
                params![request.project.as_str(), request.title, revision as i64, request.event.elapsed_ms as i64, created],
            )
            .map_err(sql_error)?;
        }
        let envelope_hash = hash(&envelope_json);
        tx.execute("INSERT INTO revisions(project,revision,event_id,fingerprint,title,updated_at,elapsed_ms,checkpoint,payload,envelope,envelope_hash) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![request.project.as_str(), revision as i64, request.request_id.as_str(), fingerprint, request.title, created, request.event.elapsed_ms as i64, checkpoint_hash, record_hash, envelope_json,envelope_hash]).map_err(sql_error)?;
        tx.execute("UPDATE identity SET sequence=?1 WHERE singleton=1", [sequence as i64]).map_err(sql_error)?;
        observe(SaveStage::BeforeCommit);
        tx.commit().map_err(sql_error)?;
        observe(SaveStage::AfterCommit);
        Ok(SaveAck { project: request.project, revision, event_id: request.request_id, device_sequence: sequence })
    }

    pub fn usage(&self) -> Result<Usage> {
        let pages: u32 = self.db.pragma_query_value(None, "page_count", |r| r.get(0)).map_err(sql_error)?;
        let size: u32 = self.db.pragma_query_value(None, "page_size", |r| r.get(0)).map_err(sql_error)?;
        let stored_bytes = u64::from(pages).checked_mul(u64::from(size)).ok_or_else(|| Error::Corrupt("database size overflow".into()))?;
        Ok(Usage { stored_bytes, quota_bytes: self.options.quota_bytes })
    }

    pub fn list(&self) -> Result<Vec<DraftInfo>> {
        let mut statement = self
            .db
            .prepare(
                "SELECT substr(id,1,33),substr(title,1,97),head_revision,substr(updated_at,1,65),elapsed_ms FROM projects ORDER BY updated_at DESC,id LIMIT ?1",
            )
            .map_err(sql_error)?;
        let rows = statement
            .query_map([(MAX_PROJECTS + 1) as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?, r.get::<_, i64>(4)?))
            })
            .map_err(sql_error)?;
        let mut result = Vec::new();
        for row in rows {
            let (id, title, revision, updated_at, elapsed_ms) = row.map_err(sql_error)?;
            if result.len() >= MAX_PROJECTS as usize {
                return Err(Error::Corrupt("draft count exceeds limit".into()));
            }
            result.push(draft_info(&id, title, revision, updated_at, elapsed_ms)?);
        }
        Ok(result)
    }

    /// Returns the newest readable checkpoint, preserving a corrupt tail for diagnosis.
    /// A fallback's revision is deliberately below head_revision: saving it back conflicts.
    pub fn load(&self, project: ProjectId) -> Result<LoadedDraft> {
        let head: Option<i64> =
            self.db.query_row("SELECT head_revision FROM projects WHERE id=?1", [project.as_str()], |r| r.get(0)).optional().map_err(sql_error)?;
        let head = nonnegative(head.ok_or(Error::NotFound)?, "head revision")?;
        let mut issue = None;
        for offset in 0..MAX_RECOVERY_SEARCH {
            let Some(revision) = head.checked_sub(offset as u64).filter(|r| *r > 0) else { break };
            match self.load_revision(project, revision) {
                Ok((document, info)) => {
                    let recovery_warning = issue.map(|error| format!("Revision {head} could not be read ({error}). Recovered revision {revision}; the damaged history remains in the data directory. Editing must create a new local draft."));
                    return Ok(LoadedDraft { document, info, head_revision: head, recovery_warning });
                }
                Err(error) => issue = Some(error.to_string()),
            }
        }
        Err(Error::Corrupt(issue.unwrap_or_else(|| "project contains no committed revisions".into())))
    }

    /// Read a retained result checkpoint without changing the editable project head.
    pub fn load_revision(&self, project: ProjectId, revision: u64) -> Result<(Document, DraftInfo)> {
        let verified = verify_revision(&self.db, project, revision)?;
        let bytes = read_object(&self.db, &verified.checkpoint, MAX_CHECKPOINT_BYTES)?;
        let document = photocraft_format::load_from_bytes_with(&bytes, &load_options()).map_err(|e| Error::Corrupt(e.to_string()))?;
        Ok((document, verified.info))
    }

    /// Canonical records after a revision, bounded by count and 16 MiB serialized bytes.
    /// Advance the cursor by the returned count. History is never pruned.
    pub fn replay_page(&self, project: ProjectId, after_revision: u64, limit: usize) -> Result<Vec<ReplayRecord>> {
        if limit == 0 || limit > MAX_REPLAY_PAGE || after_revision > i64::MAX as u64 {
            return Err(Error::Invalid("replay page bounds".into()));
        }
        let head: Option<i64> =
            self.db.query_row("SELECT head_revision FROM projects WHERE id=?1", [project.as_str()], |r| r.get(0)).optional().map_err(sql_error)?;
        let head = nonnegative(head.ok_or(Error::NotFound)?, "head revision")?;
        if after_revision > head {
            return Err(Error::Invalid("replay cursor is after the committed head".into()));
        }
        let mut statement = self
            .db
            .prepare("SELECT revision FROM revisions WHERE project=?1 AND revision>?2 AND revision<=?4 ORDER BY revision LIMIT ?3")
            .map_err(sql_error)?;
        let revisions =
            statement.query_map(params![project.as_str(), after_revision as i64, limit as i64, head as i64], |r| r.get::<_, i64>(0)).map_err(sql_error)?;
        let mut result = Vec::new();
        let mut total = 0_usize;
        let mut byte_limited = false;
        for revision in revisions {
            let revision = nonnegative(revision.map_err(sql_error)?, "revision")?;
            if revision != after_revision.saturating_add(result.len() as u64).saturating_add(1) {
                return Err(Error::Corrupt("replay contains a missing revision".into()));
            }
            let verified = verify_revision(&self.db, project, revision)?;
            total = total.saturating_add(verified.record_bytes);
            if total > MAX_PAGE_BYTES {
                byte_limited = true;
                break;
            }
            result.push(verified.record);
        }
        if !byte_limited && result.len() < limit && after_revision.saturating_add(result.len() as u64) < head {
            return Err(Error::Corrupt("replay contains a missing revision".into()));
        }
        Ok(result)
    }
}

fn initialize(db: &mut Connection) -> Result<()> {
    let actor = random_hex()?;
    let device = random_hex()?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql_error)?;
    tx.execute_batch(
        "CREATE TABLE identity(singleton INTEGER PRIMARY KEY CHECK(singleton=1),actor TEXT NOT NULL,device TEXT NOT NULL,sequence INTEGER NOT NULL CHECK(sequence>=0));
         CREATE TABLE projects(id TEXT PRIMARY KEY,title TEXT NOT NULL,head_revision INTEGER NOT NULL CHECK(head_revision>0),elapsed_ms INTEGER NOT NULL CHECK(elapsed_ms>=0),updated_at TEXT NOT NULL);
         CREATE TABLE objects(hash TEXT PRIMARY KEY,bytes INTEGER NOT NULL CHECK(bytes>0 AND bytes<=67108864),chunks INTEGER NOT NULL CHECK(chunks>0 AND chunks<=131072));
         CREATE TABLE chunks(hash TEXT PRIMARY KEY,data BLOB NOT NULL CHECK(length(data)>0 AND length(data)<=65536));
         CREATE TABLE object_chunks(object TEXT NOT NULL REFERENCES objects(hash),ordinal INTEGER NOT NULL CHECK(ordinal>=0),chunk TEXT NOT NULL REFERENCES chunks(hash),PRIMARY KEY(object,ordinal));
         CREATE TABLE revisions(project TEXT NOT NULL REFERENCES projects(id),revision INTEGER NOT NULL CHECK(revision>0),event_id TEXT NOT NULL UNIQUE,fingerprint TEXT NOT NULL,title TEXT NOT NULL,updated_at TEXT NOT NULL,elapsed_ms INTEGER NOT NULL,checkpoint TEXT NOT NULL REFERENCES objects(hash),payload TEXT NOT NULL REFERENCES objects(hash),envelope BLOB NOT NULL CHECK(length(envelope)<=262144),envelope_hash TEXT NOT NULL,PRIMARY KEY(project,revision));",
    ).map_err(sql_error)?;
    tx.execute("INSERT INTO identity(singleton,actor,device,sequence) VALUES(1,?1,?2,0)", params![actor, device]).map_err(sql_error)?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION).map_err(sql_error)?;
    tx.pragma_update(None, "application_id", APPLICATION_ID).map_err(sql_error)?;
    tx.commit().map_err(sql_error)
}

fn validate_request(request: &SaveRequest) -> Result<()> {
    photocraft_party::contracts::validate_label(&request.title, 96, false).map_err(|e| Error::Invalid(e.to_string()))?;
    validate_record(&request.event)?;
    if request.expected_revision >= MAX_REPLAY_EVENTS {
        return Err(Error::Invalid("revision limit reached".into()));
    }
    let doc = &request.document;
    photocraft_party::contracts::ImageExtent { width: doc.size.width, height: doc.size.height }.decoded_bytes(1).map_err(|e| Error::Invalid(e.to_string()))?;
    if doc.max_group_depth() > photocraft_format::MAX_GROUP_DEPTH || doc.layer_count() > 4096 {
        return Err(Error::Invalid("drawing layer/depth limit exceeded".into()));
    }
    let mut bytes = 0_u64;
    doc.for_each_surface(|surface| {
        for (_, tile) in surface.tiles() {
            bytes = bytes.saturating_add(tile.len() as u64);
        }
    });
    if bytes > 512 * 1024 * 1024 {
        return Err(Error::Invalid("drawing exceeds 512 MiB decoded pixel budget".into()));
    }
    Ok(())
}

fn validate_record(record: &ReplayRecord) -> Result<()> {
    if record.schema_version != SCHEMA_VERSION {
        return Err(Error::Version(record.schema_version));
    }
    if record.command.is_empty() || record.command.len() > 256 || record.command.chars().any(char::is_control) || record.elapsed_ms > MAX_REPLAY_MS {
        return Err(Error::Invalid("replay command/time bounds".into()));
    }
    let mut nodes = 0_usize;
    let mut stack = vec![(&record.params, 0_u16)];
    while let Some((value, depth)) = stack.pop() {
        nodes = nodes.saturating_add(1);
        if depth > 64 || nodes > MAX_RECORD_BYTES {
            return Err(Error::Invalid("replay nesting/node limit".into()));
        }
        match value {
            serde_json::Value::Array(values) => {
                if values.len() > MAX_RECORD_BYTES {
                    return Err(Error::Invalid("replay array limit".into()));
                }
                stack.extend(values.iter().map(|v| (v, depth + 1)));
            }
            serde_json::Value::Object(values) => stack.extend(values.values().map(|v| (v, depth + 1))),
            _ => {}
        }
    }
    Ok(())
}

struct BoundedBytes(Vec<u8>);
impl std::io::Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_RECORD_BYTES {
            return Err(std::io::Error::other("replay record exceeds 8 MiB"));
        }
        self.0.try_reserve(bytes.len()).map_err(|_| std::io::Error::other("not enough memory for replay"))?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn encode_record(record: &ReplayRecord) -> Result<Vec<u8>> {
    let mut output = BoundedBytes(Vec::new());
    serde_json::to_writer(&mut output, record).map_err(|e| Error::Invalid(e.to_string()))?;
    Ok(output.0)
}

fn decode_record(bytes: &[u8]) -> Result<ReplayRecord> {
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(Error::Corrupt("oversized replay record".into()));
    }
    let record: ReplayRecord = serde_json::from_slice(bytes).map_err(|e| Error::Corrupt(e.to_string()))?;
    validate_record(&record)?;
    Ok(record)
}

struct RevisionRow {
    title: String,
    updated_at: String,
    elapsed_ms: i64,
    checkpoint: String,
    payload: String,
    event: String,
    fingerprint: String,
    envelope: Vec<u8>,
    envelope_hash: String,
}

struct VerifiedRevision {
    info: DraftInfo,
    checkpoint: String,
    record: ReplayRecord,
    record_bytes: usize,
    envelope: EventEnvelope,
}

fn verify_revision(db: &Connection, project: ProjectId, revision: u64) -> Result<VerifiedRevision> {
    if revision == 0 || revision > MAX_REPLAY_EVENTS {
        return Err(Error::Corrupt("revision bounds".into()));
    }
    let row = db.query_row(
        "SELECT title,updated_at,elapsed_ms,checkpoint,payload,event_id,fingerprint,envelope,envelope_hash FROM revisions WHERE project=?1 AND revision=?2 AND length(envelope)<=262144 AND length(title)<=96 AND length(updated_at)<=64 AND length(checkpoint)=64 AND length(payload)=64 AND length(event_id)=32 AND length(fingerprint)=64 AND length(envelope_hash)=64",
        params![project.as_str(),revision as i64], |r| Ok(RevisionRow {title:r.get(0)?,updated_at:r.get(1)?,elapsed_ms:r.get(2)?,checkpoint:r.get(3)?,payload:r.get(4)?,event:r.get(5)?,fingerprint:r.get(6)?,envelope:r.get(7)?,envelope_hash:r.get(8)?}),
    ).optional().map_err(sql_error)?.ok_or_else(|| Error::Corrupt("missing or oversized revision metadata".into()))?;
    if hash(&row.envelope) != row.envelope_hash {
        return Err(Error::Corrupt("event envelope checksum".into()));
    }
    let envelope = EventEnvelope::decode(&row.envelope).map_err(|e| Error::Corrupt(e.to_string()))?;
    let event = EventId::new(&row.event).map_err(|e| Error::Corrupt(e.to_string()))?;
    if envelope.project_id != project
        || envelope.event_id != event
        || envelope.base_revision != revision - 1
        || envelope.relative_ms != nonnegative(row.elapsed_ms, "elapsed time")?
    {
        return Err(Error::Corrupt("event envelope disagrees with committed revision".into()));
    }
    let payload_bytes = read_object(db, &row.payload, MAX_RECORD_BYTES)?;
    let record = decode_record(&payload_bytes)?;
    let checkpoint_bytes: Option<i64> =
        db.query_row("SELECT bytes FROM objects WHERE hash=?1", [&row.checkpoint], |r| r.get(0)).optional().map_err(sql_error)?;
    let checkpoint_bytes = checkpoint_bytes.ok_or_else(|| Error::Corrupt("missing checkpoint reference".into()))?;
    let result = match &envelope.step {
        ReplayStep::Resolved { payload, result, .. } => {
            if payload.hash.as_str() != row.payload
                || payload.bytes != payload_bytes.len() as u64
                || matches!(record.kind, crate::ReplayKind::Baseline | crate::ReplayKind::Checkpoint)
            {
                return Err(Error::Corrupt("event payload association".into()));
            }
            result
        }
        ReplayStep::Checkpoint { result } => {
            if !matches!(record.kind, crate::ReplayKind::Baseline | crate::ReplayKind::Checkpoint) {
                return Err(Error::Corrupt("checkpoint kind association".into()));
            }
            result
        }
    };
    if result.id.as_str() != event.as_str()
        || result.asset.hash.as_str() != row.checkpoint
        || result.asset.bytes != nonnegative(checkpoint_bytes, "checkpoint length")?
        || record.elapsed_ms != envelope.relative_ms
    {
        return Err(Error::Corrupt("event checkpoint/timing association".into()));
    }
    if fingerprint_fields(project, event, &row.title, revision - 1, &row.checkpoint, &row.payload) != row.fingerprint {
        return Err(Error::Corrupt("revision fingerprint mismatch".into()));
    }
    let info = draft_info(project.as_str(), row.title, revision as i64, row.updated_at, row.elapsed_ms)?;
    Ok(VerifiedRevision { info, checkpoint: row.checkpoint, record, record_bytes: payload_bytes.len(), envelope })
}

fn request_fingerprint(request: &SaveRequest, checkpoint: &str, payload: &str) -> String {
    fingerprint_fields(request.project, request.request_id, &request.title, request.expected_revision, checkpoint, payload)
}

fn fingerprint_fields(project: ProjectId, event: EventId, title: &str, expected: u64, checkpoint: &str, payload: &str) -> String {
    let mut digest = Sha256::new();
    for bytes in [project.as_str().as_bytes(), event.as_str().as_bytes(), title.as_bytes(), checkpoint.as_bytes(), payload.as_bytes()] {
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    digest.update(expected.to_le_bytes());
    format!("{:x}", digest.finalize())
}

fn object_ref(digest: &str, bytes: usize) -> Result<AssetRef> {
    Ok(AssetRef { hash: AssetHash::new(digest).map_err(|e| Error::Invalid(e.to_string()))?, bytes: bytes as u64 })
}

fn make_envelope(request: &SaveRequest, actor: &str, device: &str, sequence: u64, checkpoint: AssetRef, payload: AssetRef) -> Result<EventEnvelope> {
    let result = CheckpointRef { id: CheckpointId::new(request.request_id.as_str()).map_err(|e| Error::Invalid(e.to_string()))?, asset: checkpoint };
    let envelope = EventEnvelope {
        schema_version: 1,
        event_id: request.request_id,
        actor_id: ParticipantId::new(actor).map_err(|e| Error::Corrupt(e.to_string()))?,
        device_id: DeviceId::new(device).map_err(|e| Error::Corrupt(e.to_string()))?,
        device_sequence: sequence,
        project_id: request.project,
        base_revision: request.expected_revision,
        dependencies: Vec::new(),
        relative_ms: request.event.elapsed_ms,
        step: if matches!(request.event.kind, crate::ReplayKind::Baseline | crate::ReplayKind::Checkpoint) {
            ReplayStep::Checkpoint { result }
        } else {
            ReplayStep::Resolved { operation_version: 1, payload, result }
        },
    };
    envelope.validate().map_err(|e| Error::Invalid(e.to_string()))?;
    Ok(envelope)
}

fn existing_receipt(db: &Connection, event: EventId, fingerprint: &str) -> Result<Option<SaveAck>> {
    let row: Option<(String, String, i64)> = db
        .query_row("SELECT substr(fingerprint,1,65),substr(project,1,33),revision FROM revisions WHERE event_id=?1", [event.as_str()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()
        .map_err(sql_error)?;
    let Some((stored, project, revision)) = row else { return Ok(None) };
    if stored != fingerprint {
        return Err(Error::Conflict);
    }
    let project = ProjectId::new(&project).map_err(|e| Error::Corrupt(e.to_string()))?;
    let verified = verify_revision(db, project, nonnegative(revision, "revision")?)?;
    let envelope = verified.envelope;
    if envelope.event_id != event {
        return Err(Error::Corrupt("receipt event mismatch".into()));
    }
    read_object(db, &verified.checkpoint, MAX_CHECKPOINT_BYTES)?;
    Ok(Some(SaveAck { project: envelope.project_id, revision: envelope.base_revision + 1, event_id: event, device_sequence: envelope.device_sequence }))
}

fn store_object(db: &Connection, digest: &str, bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_CHECKPOINT_BYTES {
        return Err(Error::Invalid("object bytes".into()));
    }
    let existing: Option<i64> = db.query_row("SELECT bytes FROM objects WHERE hash=?1", [digest], |r| r.get(0)).optional().map_err(sql_error)?;
    if let Some(length) = existing {
        if nonnegative(length, "object length")? != bytes.len() as u64 || read_object(db, digest, MAX_CHECKPOINT_BYTES)? != bytes {
            return Err(Error::Corrupt("existing immutable object differs; original retained".into()));
        }
        return Ok(());
    }
    let chunks = object_chunks(bytes)?;
    let count = chunks.len();
    db.execute("INSERT INTO objects(hash,bytes,chunks) VALUES(?1,?2,?3)", params![digest, bytes.len() as i64, count as i64]).map_err(sql_error)?;
    for (index, chunk) in chunks.into_iter().enumerate() {
        let chunk_hash = hash(chunk);
        db.execute("INSERT OR IGNORE INTO chunks(hash,data) VALUES(?1,?2)", params![chunk_hash, chunk]).map_err(sql_error)?;
        let stored: Vec<u8> = db.query_row("SELECT data FROM chunks WHERE hash=?1 AND length(data)<=65536", [&chunk_hash], |r| r.get(0)).map_err(sql_error)?;
        if stored != chunk {
            return Err(Error::Corrupt("existing immutable chunk differs".into()));
        }
        db.execute("INSERT INTO object_chunks(object,ordinal,chunk) VALUES(?1,?2,?3)", params![digest, index as i64, chunk_hash]).map_err(sql_error)?;
    }
    Ok(())
}

fn read_object(db: &Connection, digest: &str, max: usize) -> Result<Vec<u8>> {
    AssetHash::new(digest).map_err(|e| Error::Corrupt(e.to_string()))?;
    let metadata: Option<(i64, i64)> =
        db.query_row("SELECT bytes,chunks FROM objects WHERE hash=?1", [digest], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(sql_error)?;
    let (length, count) = metadata.ok_or_else(|| Error::Corrupt("missing object".into()))?;
    let length = usize::try_from(length).map_err(|_| Error::Corrupt("object length".into()))?;
    let count = usize::try_from(count).map_err(|_| Error::Corrupt("object chunk count".into()))?;
    if length == 0 || length > max || count == 0 || count > MAX_OBJECT_CHUNKS || count > length {
        return Err(Error::Corrupt("object size/count limit".into()));
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length).map_err(|_| Error::Storage("not enough memory to recover checkpoint".into()))?;
    let mut statement = db.prepare("SELECT o.ordinal,c.hash,c.data FROM object_chunks o JOIN chunks c ON c.hash=o.chunk WHERE o.object=?1 AND length(c.data)<=65536 AND length(c.hash)=64 ORDER BY o.ordinal LIMIT 131073").map_err(sql_error)?;
    let chunks = statement.query_map([digest], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Vec<u8>>(2)?))).map_err(sql_error)?;
    let mut seen = 0_usize;
    for row in chunks {
        let (ordinal, expected, data) = row.map_err(sql_error)?;
        if ordinal != seen as i64 || seen >= count || data.is_empty() || bytes.len().saturating_add(data.len()) > length || hash(&data) != expected {
            return Err(Error::Corrupt("checkpoint chunk integrity failed".into()));
        }
        bytes.extend_from_slice(&data);
        seen += 1;
    }
    if bytes.len() != length || seen != count || hash(&bytes) != digest {
        return Err(Error::Corrupt("checkpoint checksum/length mismatch".into()));
    }
    Ok(bytes)
}

/// Align immutable chunks with native STORE entry boundaries. Unchanged tile entries
/// deduplicate even if an earlier manifest/tile changes length. This only partitions
/// bytes; the existing format decoder remains authoritative for actual document loading.
fn object_chunks(bytes: &[u8]) -> Result<Vec<&[u8]>> {
    let mut result = Vec::new();
    let mut position = 0_usize;
    while bytes.get(position..position.saturating_add(4)) == Some(b"PK\x03\x04") {
        let header = bytes.get(position..position.saturating_add(30)).ok_or_else(|| Error::Invalid("truncated native ZIP header".into()))?;
        let size =
            header.get(18..22).and_then(|s| <[u8; 4]>::try_from(s).ok()).map(u32::from_le_bytes).ok_or_else(|| Error::Invalid("ZIP size".into()))? as usize;
        let name =
            header.get(26..28).and_then(|s| <[u8; 2]>::try_from(s).ok()).map(u16::from_le_bytes).ok_or_else(|| Error::Invalid("ZIP name".into()))? as usize;
        let extra =
            header.get(28..30).and_then(|s| <[u8; 2]>::try_from(s).ok()).map(u16::from_le_bytes).ok_or_else(|| Error::Invalid("ZIP extra".into()))? as usize;
        let end = position
            .checked_add(30)
            .and_then(|v| v.checked_add(name))
            .and_then(|v| v.checked_add(extra))
            .and_then(|v| v.checked_add(size))
            .ok_or_else(|| Error::Invalid("ZIP length overflow".into()))?;
        let entry = bytes.get(position..end).ok_or_else(|| Error::Invalid("ZIP entry bounds".into()))?;
        result.extend(entry.chunks(CHUNK_BYTES));
        if result.len() > MAX_OBJECT_CHUNKS {
            return Err(Error::Invalid("checkpoint chunk limit".into()));
        }
        position = end;
    }
    let tail = bytes.get(position..).ok_or_else(|| Error::Invalid("checkpoint tail bounds".into()))?;
    result.extend(tail.chunks(CHUNK_BYTES));
    if result.is_empty() || result.len() > MAX_OBJECT_CHUNKS {
        return Err(Error::Invalid("checkpoint chunk limit".into()));
    }
    Ok(result)
}

fn load_options() -> LoadOptions {
    LoadOptions {
        max_manifest_bytes: 8 * 1024 * 1024,
        max_blob_bytes: 64 * 1024 * 1024,
        max_total_bytes: 512 * 1024 * 1024,
        max_canvas_side: 8192,
        max_canvas_pixels: 16_777_216,
        ..LoadOptions::default()
    }
}

fn draft_info(id: &str, title: String, revision: i64, updated_at: String, elapsed_ms: i64) -> Result<DraftInfo> {
    photocraft_party::contracts::validate_label(&title, 96, false).map_err(|e| Error::Corrupt(e.to_string()))?;
    if updated_at.len() > 64 || chrono::DateTime::parse_from_rfc3339(&updated_at).is_err() {
        return Err(Error::Corrupt("draft date".into()));
    }
    let revision = nonnegative(revision, "revision")?;
    let replay_elapsed_ms = nonnegative(elapsed_ms, "elapsed time")?;
    if revision == 0 || revision > MAX_REPLAY_EVENTS || replay_elapsed_ms > MAX_REPLAY_MS {
        return Err(Error::Corrupt("draft revision/time limit".into()));
    }
    Ok(DraftInfo { project: ProjectId::new(id).map_err(|e| Error::Corrupt(e.to_string()))?, title, revision, updated_at, replay_elapsed_ms })
}
