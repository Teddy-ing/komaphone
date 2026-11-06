use crate::{ClientStatus, CredentialStore, Error, FreezePin, Invitation, Result, normalize_endpoint};
use photocraft_party::contracts::MAX_MESSAGE_BYTES;
use photocraft_party::protocol::{Operation, PROTOCOL_VERSION, RpcRequest, RpcResponse, validate_secret};
use photocraft_party::{DeviceId, Participant, ParticipantId, RequestId, RoomId};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Duration;

const APPLICATION_ID: u32 = 0x4b524f4d;
const SCHEMA: u32 = 1;
const MAX_PENDING: usize = 8192;
const MAX_PENDING_BYTES: u64 = 384 * 1024 * 1024;
const MAX_STATE_BYTES: usize = 5 * 1024 * 1024;

#[path = "store_notes.rs"]
mod notes;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedState {
    schema: u32,
    namespace: String,
    status: ClientStatus,
    active: bool,
    last_invitation: Option<(RoomId, RequestId, u64)>,
    #[serde(default)]
    frozen: Option<(RoomId, photocraft_party::AssignmentId, u64)>,
}

/// SQLite owns a durable client identity, last authorized room view and exact outbox.
/// Credentials and invitation capabilities are references into a native credential store;
/// SQLite, diagnostic messages and serialized status never contain their raw bytes.
pub struct Outbox {
    db: Connection,
    state: SavedState,
    credentials: Box<dyn CredentialStore>,
}

pub(crate) fn random_hex(bytes: usize) -> Result<String> {
    if !matches!(bytes, 16 | 32) {
        return Err(Error::Invalid("Invalid identity size".into()));
    }
    let mut data = vec![0u8; bytes];
    getrandom::fill(&mut data).map_err(|_| Error::Storage("Cannot allocate a secure identity".into()))?;
    if data.iter().all(|byte| *byte == 0) {
        return Err(Error::Storage("Random identity was invalid; retry".into()));
    }
    Ok(data.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn sql(error: rusqlite::Error) -> Error {
    Error::Storage(error.to_string())
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn request_id() -> Result<RequestId> {
    RequestId::new(&random_hex(16)?).map_err(|e| Error::Invalid(e.to_string()))
}

impl Outbox {
    pub fn open(path: impl AsRef<Path>, credentials: Box<dyn CredentialStore>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| Error::Storage(e.to_string()))?;
        }
        let mut db = Connection::open(path).map_err(sql)?;
        db.busy_timeout(Duration::from_secs(2)).map_err(sql)?;
        let version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0)).map_err(sql)?;
        let app: u32 = db.pragma_query_value(None, "application_id", |row| row.get(0)).map_err(sql)?;
        if (version != 0 && (version != SCHEMA || app != APPLICATION_ID)) || (app != 0 && app != APPLICATION_ID) {
            return Err(Error::Storage("Unsupported room database; preserve it and upgrade".into()));
        }
        if version == 0 {
            let tables: i64 = db.query_row("SELECT count(*) FROM sqlite_master WHERE type='table'", [], |row| row.get(0)).map_err(sql)?;
            if tables != 0 || app != 0 {
                return Err(Error::Storage("Unrecognized existing database; preserve it and choose a fresh room data root".into()));
            }
        }
        db.pragma_update(None, "journal_mode", "DELETE").map_err(sql)?;
        db.pragma_update(None, "synchronous", "EXTRA").map_err(sql)?;
        db.pragma_update(None, "foreign_keys", true).map_err(sql)?;
        db.pragma_update(None, "trusted_schema", false).map_err(sql)?;
        let journal: String = db.pragma_query_value(None, "journal_mode", |r| r.get(0)).map_err(sql)?;
        let sync: u32 = db.pragma_query_value(None, "synchronous", |r| r.get(0)).map_err(sql)?;
        if journal != "delete" || sync != 3 {
            return Err(Error::Storage("Required SQLite durability settings were not applied".into()));
        }
        let page_size: i64 = db.pragma_query_value(None, "page_size", |r| r.get(0)).map_err(sql)?;
        if page_size <= 0 {
            return Err(Error::Storage("Invalid database page size".into()));
        }
        db.pragma_update(None, "max_page_count", 512 * 1024 * 1024i64 / page_size).map_err(sql)?;
        let transaction = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        transaction
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS state (id INTEGER PRIMARY KEY CHECK(id=1), body BLOB NOT NULL, digest TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS outbox (seq INTEGER PRIMARY KEY AUTOINCREMENT, request_id TEXT UNIQUE NOT NULL, body BLOB NOT NULL,
            digest TEXT NOT NULL, secret_key TEXT, rejected TEXT, retained INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS outbox_submissions (request_id TEXT PRIMARY KEY REFERENCES outbox(request_id) ON DELETE CASCADE, project TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS credential_refs (key TEXT PRIMARY KEY);
            CREATE TABLE IF NOT EXISTS local_assignment_notes (
                endpoint TEXT NOT NULL, room TEXT NOT NULL, game TEXT NOT NULL, chain INTEGER NOT NULL, round INTEGER NOT NULL,
                body BLOB NOT NULL, digest TEXT NOT NULL, PRIMARY KEY(endpoint,room,game,chain,round));",
            )
            .map_err(sql)?;
        transaction.pragma_update(None, "application_id", APPLICATION_ID).map_err(sql)?;
        transaction.pragma_update(None, "user_version", SCHEMA).map_err(sql)?;
        transaction.commit().map_err(sql)?;
        let raw: Option<(Vec<u8>, String)> = db
            .query_row("SELECT body, digest FROM state WHERE id=1 AND length(body)<=?1", [MAX_STATE_BYTES as i64], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(sql)?;
        let state = match raw {
            Some((body, checksum)) => {
                if digest(&body) != checksum {
                    return Err(Error::Storage("Room metadata checksum failed; preserve this database for recovery".into()));
                }
                let mut saved: SavedState =
                    serde_json::from_slice(&body).map_err(|_| Error::Storage("Invalid room metadata; preserve this database for recovery".into()))?;
                if saved.schema != SCHEMA {
                    return Err(Error::Storage("Unsupported room metadata schema".into()));
                }
                if let Some(endpoint) = &saved.status.endpoint {
                    normalize_endpoint(endpoint)?;
                }
                if let Some(profile) = &saved.status.profile {
                    profile.validate().map_err(|e| Error::Storage(e.to_string()))?;
                }
                saved.status.connected = false;
                saved.status.remember_game_draft()?;
                saved
            }
            None => {
                let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM state)", [], |r| r.get(0)).map_err(sql)?;
                if exists {
                    return Err(Error::Storage("Room metadata exceeds its size limit".into()));
                }
                SavedState { schema: SCHEMA, namespace: random_hex(16)?, status: ClientStatus::default(), active: true, last_invitation: None, frozen: None }
            }
        };
        let mut store = Self { db, state, credentials };
        notes::migrate(&mut store.db, &store.state.status)?;
        store.rebuild_submission_index()?;
        store.refresh_counts()?;
        store.save_state()?;
        Ok(store)
    }

    pub fn status(&self) -> ClientStatus {
        self.state.status.clone()
    }
    pub fn active(&self) -> bool {
        self.state.active
    }
    /// Recover this device's latest locally saved note for an assignment, including
    /// skipped/incomplete games. This never fetches service notes or changes the
    /// current party projection; historical notes must stay outside active games.
    pub fn saved_note(&self, endpoint: &str, room: RoomId, assignment: photocraft_party::AssignmentId) -> Result<Option<crate::NoteDraft>> {
        notes::read(&self.db, &normalize_endpoint(endpoint)?, room, assignment)
    }
    pub fn save_note(&mut self, draft: crate::NoteDraft) -> Result<()> {
        photocraft_party::validate_label(&draft.note, photocraft_party::MAX_NOTE_BYTES, true).map_err(|e| Error::Invalid(e.to_string()))?;
        let room = self.state.status.room.as_ref().ok_or_else(|| Error::Invalid("No active assignment".into()))?;
        if room.phase != photocraft_party::room::Phase::Drawing
            || room.assignment.as_ref().is_none_or(|a| {
                a.submitted || a.assignment.id != draft.assignment || Some(a.assignment.player) != self.state.status.profile.as_ref().map(|profile| profile.id)
            })
        {
            return Err(Error::Invalid("This handoff note belongs to a different or frozen assignment".into()));
        }
        let mut candidate = self.state.clone();
        candidate.status.note_draft = Some(draft.clone());
        let endpoint = self.state.status.endpoint.as_deref().ok_or_else(|| Error::Invalid("No local note service scope".into()))?;
        let transaction = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        notes::write(&transaction, endpoint, room.id, &draft)?;
        write_state(&transaction, &candidate)?;
        transaction.commit().map_err(sql)?;
        self.state = candidate;
        Ok(())
    }
    pub fn save_reveal_position(&mut self, position: crate::RevealPosition) -> Result<()> {
        let room = self.state.status.room.as_ref().ok_or_else(|| Error::Invalid("No revealed game".into()))?;
        let valid_game = room.reveal.iter().any(|entry| match entry {
            photocraft_party::room::RevealedEntry::Artwork { assignment, .. } | photocraft_party::room::RevealedEntry::Missing { assignment, .. } => {
                assignment.id.game == position.game
            }
        });
        if !matches!(room.phase, photocraft_party::room::Phase::Reveal | photocraft_party::room::Phase::Archived)
            || !valid_game
            || usize::from(position.chain) >= room.members.len()
            || position.frame >= room.mode.rounds(room.members.len()).map_err(|e| Error::Invalid(e.to_string()))?
            || !(1..=24).contains(&position.fps)
            || !(1..=3).contains(&position.visible_parts)
        {
            return Err(Error::Invalid("Reveal position is outside this revealed game".into()));
        }
        let mut candidate = self.state.clone();
        candidate.status.reveal_position = Some(position);
        self.commit_candidate(candidate)
    }
    fn commit_candidate(&mut self, mut candidate: SavedState) -> Result<()> {
        let transaction = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        notes::transition(&transaction, &self.state.status, &mut candidate.status)?;
        write_state(&transaction, &candidate)?;
        transaction.commit().map_err(sql)?;
        self.state = candidate;
        Ok(())
    }
    pub fn credential(&self) -> Result<String> {
        let endpoint = self.state.status.endpoint.as_deref().ok_or_else(|| Error::Invalid("Connect to a service first".into()))?;
        self.credentials
            .get(&self.state.namespace, &credential_key(endpoint))?
            .ok_or_else(|| Error::Credentials("Saved pairing credential is missing; preserve pending work".into()))
    }

    /// One stable device identity per data root. A changed address is allowed only when
    /// no request is pending; switching service clears the old room view, not local drafts.
    pub fn configure(&mut self, endpoint: &str, display_name: &str) -> Result<()> {
        let endpoint = normalize_endpoint(endpoint)?;
        photocraft_party::contracts::validate_label(display_name, 96, false).map_err(|e| Error::Invalid(e.to_string()))?;
        if self.state.status.pending > 0 {
            if self.state.status.endpoint.as_deref() == Some(&endpoint) {
                self.state.active = true;
                return self.save_state();
            }
            return Err(Error::Invalid("Resolve pending work before changing service".into()));
        }
        let profile = match &self.state.status.profile {
            Some(profile) => profile.clone(),
            None => Participant {
                id: ParticipantId::new(&random_hex(16)?).map_err(|e| Error::Invalid(e.to_string()))?,
                device: DeviceId::new(&random_hex(16)?).map_err(|e| Error::Invalid(e.to_string()))?,
                display_name: display_name.into(),
            },
        };
        let key = credential_key(&endpoint);
        self.register_credential(&key)?;
        let credential = match self.credentials.get(&self.state.namespace, &key)? {
            Some(secret) => secret,
            None => {
                let secret = random_hex(32)?;
                self.credentials.set(&self.state.namespace, &key, &secret)?;
                secret
            }
        };
        validate_secret(&credential).map_err(|_| Error::Credentials("Stored pairing credential is invalid".into()))?;
        let mut candidate = self.state.clone();
        if candidate.status.endpoint.as_deref() != Some(&endpoint) {
            candidate.status.room = None;
            candidate.status.invitation_id = None;
            candidate.last_invitation = None;
        }
        candidate.status.endpoint = Some(endpoint);
        candidate.status.profile = Some(profile.clone());
        candidate.status.paired = false;
        candidate.status.error = None;
        candidate.status.connected = false;
        candidate.active = true;
        let operation = Operation::Pair { participant: profile, credential };
        let prepared = self.prepare(operation)?;
        let transaction = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        insert(&transaction, &prepared)?;
        notes::transition(&transaction, &self.state.status, &mut candidate.status)?;
        write_state(&transaction, &candidate)?;
        transaction.commit().map_err(sql)?;
        self.state = candidate;
        self.refresh_counts()
    }

    pub fn enqueue(&mut self, operation: Operation) -> Result<RequestId> {
        self.enqueue_batch(vec![operation])?.into_iter().next().ok_or_else(|| Error::Storage("Empty request queue".into()))
    }

    /// All upload chunks and the final immutable submission enter one local transaction.
    /// A process kill can leave either the entire pending bundle or none of it.
    pub fn enqueue_batch(&mut self, operations: Vec<Operation>) -> Result<Vec<RequestId>> {
        if self.state.status.endpoint.is_none() {
            return Err(Error::Invalid("Connect to a service first".into()));
        }
        if operations.is_empty() || operations.len() > MAX_PENDING {
            return Err(Error::Busy);
        }
        let prepared = operations.into_iter().map(|op| self.prepare(op)).collect::<Result<Vec<_>>>()?;
        if let Some(existing) = self.state.status.submission_project
            && prepared.iter().any(|request| request.submission_project.is_some_and(|project| project != existing))
        {
            return Err(Error::Busy);
        }
        let bytes = prepared.iter().map(|p| p.body.len() as u64).fold(0u64, u64::saturating_add);
        let count: i64 = self.db.query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get(0)).map_err(sql)?;
        let used: i64 = self.db.query_row("SELECT COALESCE(SUM(length(body)),0) FROM outbox", [], |r| r.get(0)).map_err(sql)?;
        let count = usize::try_from(count).map_err(|_| Error::Storage("Invalid pending request count".into()))?;
        let used = u64::try_from(used).map_err(|_| Error::Storage("Invalid pending request size".into()))?;
        if count.saturating_add(prepared.len()) > MAX_PENDING || used.saturating_add(bytes) > MAX_PENDING_BYTES {
            return Err(Error::Busy);
        }
        let transaction = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        for request in &prepared {
            insert(&transaction, request)?;
        }
        transaction.commit().map_err(sql)?;
        self.state.status.notice = Some("Saved locally · waiting for service acceptance".into());
        self.refresh_counts()?;
        Ok(prepared.into_iter().map(|p| p.id).collect())
    }

    fn prepare(&self, operation: Operation) -> Result<Prepared> {
        let id = request_id()?;
        let mut request = RpcRequest { protocol_version: PROTOCOL_VERSION, request_id: id, operation };
        let secret = match &mut request.operation {
            Operation::Pair { credential, .. } => Some(std::mem::take(credential)),
            Operation::Join { invitation, .. } => Some(std::mem::take(invitation)),
            Operation::Invite { secret, .. } => Some(std::mem::take(secret)),
            _ => None,
        };
        let secret_key = if let Some(secret) = secret {
            validate_secret(&secret).map_err(|_| Error::Invalid("Invitation or credential must be a 256-bit capability".into()))?;
            let key = format!("request/{}", id.as_str());
            self.register_credential(&key)?;
            self.credentials.set(&self.state.namespace, &key, &secret)?;
            Some(key)
        } else {
            None
        };
        let body = serde_json::to_vec(&request).map_err(|_| Error::Invalid("Cannot encode room request".into()))?;
        // Leave space for the redacted secret when reconstructed for transport.
        if body.len().saturating_add(64) > MAX_MESSAGE_BYTES {
            return Err(Error::Invalid("Room request exceeds 256 KiB".into()));
        }
        let submission_project = submission_project(&request.operation);
        Ok(Prepared { id, checksum: digest(&body), body, secret_key, submission_project })
    }

    pub fn next(&self) -> Result<Option<RpcRequest>> {
        let record: Option<(Vec<u8>, String, Option<String>)> = self.db.query_row(
            "SELECT CASE WHEN length(body)<=?1 THEN body ELSE NULL END,digest,secret_key FROM outbox WHERE retained=0 AND rejected IS NULL ORDER BY seq LIMIT 1", [MAX_MESSAGE_BYTES as i64],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(sql)?;
        let Some((body, checksum, secret_key)) = record else { return Ok(None) };
        if body.len() > MAX_MESSAGE_BYTES || digest(&body) != checksum {
            return Err(Error::Storage("Pending request checksum failed; preserve the database".into()));
        }
        let mut request = RpcRequest::decode(&body).map_err(|_| Error::Storage("Invalid pending request".into()))?;
        if let Some(key) = secret_key {
            let secret = self
                .credentials
                .get(&self.state.namespace, &key)?
                .ok_or_else(|| Error::Credentials("Pending invitation or pairing secret is missing".into()))?;
            validate_secret(&secret).map_err(|_| Error::Credentials("Pending capability is invalid".into()))?;
            match &mut request.operation {
                Operation::Pair { credential, .. } => *credential = secret,
                Operation::Join { invitation, .. } => *invitation = secret,
                Operation::Invite { secret: value, .. } => *value = secret,
                _ => return Err(Error::Storage("Unexpected secret reference in pending request".into())),
            }
        }
        Ok(Some(request))
    }

    pub fn acknowledge(&mut self, request: &RpcRequest, response: &RpcResponse) -> Result<()> {
        if self.next()?.as_ref() != Some(request) {
            return Err(Error::Invalid("Acknowledgment request differs from the durable outbox".into()));
        }
        let mut candidate = self.state.clone();
        apply_response(&mut candidate, request, response)?;
        candidate.status.remember_game_draft()?;
        candidate.status.connected = candidate.active;
        candidate.status.error = None;
        candidate.status.notice = Some("Service committed the room change".into());
        let transaction = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        let removed = transaction.execute("DELETE FROM outbox WHERE request_id=?1 AND retained=0", [request.request_id.as_str()]).map_err(sql)?;
        if removed != 1 {
            return Err(Error::Storage("Acknowledgment did not match pending work".into()));
        }
        notes::transition(&transaction, &self.state.status, &mut candidate.status)?;
        write_state(&transaction, &candidate)?;
        transaction.commit().map_err(sql)?;
        self.state = candidate;
        self.refresh_counts()?;
        if !matches!(request.operation, Operation::Invite { .. }) {
            // A failed deletion leaves an inaccessible orphan, never a lost credential.
            let key = format!("request/{}", request.request_id.as_str());
            if self.credentials.remove(&self.state.namespace, &key).is_ok() {
                let _ = self.db.execute("DELETE FROM credential_refs WHERE key=?1", [&key]);
            }
        }
        Ok(())
    }

    pub fn update_poll(&mut self, view: photocraft_party::room::RoomView) -> Result<()> {
        if self.state.status.room.as_ref().is_some_and(|old| old.id == view.id && old.revision > view.revision) {
            return Err(Error::Invalid("Service revision moved backwards; cached work was retained".into()));
        }
        let mut candidate = self.state.clone();
        candidate.status.room = Some(view);
        candidate.status.remember_game_draft()?;
        candidate.status.connected = true;
        if !candidate.status.blocked {
            candidate.status.error = None;
        }
        self.commit_candidate(candidate)
    }

    pub fn mark_error(&mut self, error: &Error, request: Option<RequestId>, permanent: bool) -> Result<()> {
        let message = error.to_string();
        if permanent && let Some(id) = request {
            self.db.execute("UPDATE outbox SET rejected=?1 WHERE request_id=?2", params![message, id.as_str()]).map_err(sql)?;
        }
        self.state.status.error = Some(message);
        self.state.status.connected = false;
        self.refresh_counts()?;
        self.save_state()
    }

    pub fn retry(&mut self) -> Result<()> {
        self.db.execute("UPDATE outbox SET rejected=NULL WHERE retained=0", []).map_err(sql)?;
        self.state.status.error = None;
        self.state.active = true;
        self.refresh_counts()?;
        self.save_state()
    }

    /// Keep a recoverable copy of rejected requests while allowing new user actions.
    /// This never discards the local drawing or rewrites an accepted submission.
    pub fn retain_rejected(&mut self) -> Result<()> {
        // A submission consists of a manifest plus many dependent chunks. Retaining only
        // the rejected chunk would send its dependent tail and cascade avoidable failures.
        let transaction = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        let rejected: bool =
            transaction.query_row("SELECT EXISTS(SELECT 1 FROM outbox WHERE retained=0 AND rejected IS NOT NULL)", [], |row| row.get(0)).map_err(sql)?;
        if rejected {
            transaction.execute("UPDATE outbox SET retained=1 WHERE retained=0", []).map_err(sql)?;
        }
        transaction.commit().map_err(sql)?;
        self.state.status.error = None;
        self.state.status.notice = Some("Rejected request retained locally; review the current room before submitting again".into());
        self.refresh_counts()?;
        self.save_state()
    }

    pub fn stop_polling(&mut self) -> Result<()> {
        self.state.active = false;
        self.state.status.connected = false;
        self.state.status.polling = false;
        self.save_state()
    }

    pub fn pin_freeze(&mut self, pin: FreezePin) -> Result<()> {
        let valid = self.state.status.room.as_ref().is_some_and(|room| {
            room.id == pin.room
                && room.epoch == pin.epoch
                && room.phase == photocraft_party::room::Phase::Transfer
                && room.assignment.as_ref().is_some_and(|a| !a.submitted && a.assignment.id == pin.assignment && a.assignment.project == pin.project)
        });
        if !valid || pin.revision == 0 {
            return Err(Error::Invalid("Freeze pin must name the current deadline assignment and an acknowledged local revision".into()));
        }
        if let Some(existing) = &self.state.status.freeze_pin
            && existing.room == pin.room
            && existing.assignment == pin.assignment
            && existing.epoch == pin.epoch
        {
            return if *existing == pin {
                Ok(())
            } else {
                Err(Error::Invalid("This deadline already pinned an earlier acknowledged revision; retain that first pin".into()))
            };
        }
        let mut candidate = self.state.clone();
        candidate.status.freeze_pin = Some(pin);
        let transaction = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        write_state(&transaction, &candidate)?;
        transaction.commit().map_err(sql)?;
        self.state = candidate;
        Ok(())
    }

    pub fn invitation(&self) -> Result<Option<Invitation>> {
        let Some((room, id, expires_at_ms)) = self.state.last_invitation else { return Ok(None) };
        let secret = self.credentials.get(&self.state.namespace, &format!("request/{}", id.as_str()))?;
        Ok(secret.map(|secret| Invitation { room, id, secret, expires_at_ms }))
    }

    /// Test/administration cleanup for an explicitly discarded, isolated data root.
    /// Calling this makes its pairing/outbox unusable; the database itself is retained.
    pub fn remove_credentials_for_discarded_root(&self) -> Result<usize> {
        let mut statement = self.db.prepare("SELECT CASE WHEN length(key)<=256 THEN key ELSE NULL END FROM credential_refs LIMIT 16385").map_err(sql)?;
        let keys = statement.query_map([], |row| row.get::<_, String>(0)).map_err(sql)?.collect::<std::result::Result<Vec<_>, _>>().map_err(sql)?;
        if keys.len() > 16384 {
            return Err(Error::Storage("Credential-reference limit exceeded; preserve the database".into()));
        }
        for key in &keys {
            self.credentials.remove(&self.state.namespace, key)?;
        }
        Ok(keys.len())
    }

    fn register_credential(&self, key: &str) -> Result<()> {
        let count: i64 = self.db.query_row("SELECT COUNT(*) FROM credential_refs", [], |row| row.get(0)).map_err(sql)?;
        if count >= 16384 {
            return Err(Error::Storage("Credential history limit reached; retain this profile and resolve old invitations before adding more".into()));
        }
        self.db.execute("INSERT OR IGNORE INTO credential_refs(key) VALUES(?1)", [key]).map_err(sql)?;
        Ok(())
    }

    /// When a server deadline arrives during an upload, freeze the already durable exact
    /// queued manifest before its Submit. No request ID/body or drawing revision changes.
    pub fn ensure_transfer_freeze(&mut self) -> Result<()> {
        let Some(view) = self.state.status.room.as_ref().filter(|r| r.phase == photocraft_party::room::Phase::Transfer) else { return Ok(()) };
        let Some(assignment) = view.assignment.as_ref().filter(|a| !a.submitted) else { return Ok(()) };
        let (room, epoch, expected_revision, assigned) = (view.id, view.epoch, view.revision, assignment.assignment.id);
        if self.state.frozen == Some((room, assigned, epoch)) || self.state.status.pending == 0 {
            return Ok(());
        }
        let mut submission = None;
        let mut prior_freeze = false;
        let mut statement = self
            .db
            .prepare("SELECT CASE WHEN length(body)<=?1 THEN body ELSE NULL END,digest FROM outbox WHERE retained=0 ORDER BY seq LIMIT 8193")
            .map_err(sql)?;
        let mut rows = statement.query([MAX_MESSAGE_BYTES as i64]).map_err(sql)?;
        let mut count = 0usize;
        while let Some(row) = rows.next().map_err(sql)? {
            count = count.saturating_add(1);
            if count > MAX_PENDING {
                return Err(Error::Busy);
            }
            let body: Vec<u8> = row.get(0).map_err(sql)?;
            let checksum: String = row.get(1).map_err(sql)?;
            if digest(&body) != checksum {
                return Err(Error::Storage("Pending request checksum failed".into()));
            }
            let request = RpcRequest::decode(&body).map_err(|_| Error::Storage("Invalid pending request".into()))?;
            if let Operation::Apply { room: target, action, .. } = request.operation
                && target == room
            {
                match *action {
                    photocraft_party::room::Action::FreezeReport { epoch: phase, submission: value } if phase == epoch && value.assignment == assigned => {
                        prior_freeze = true
                    }
                    photocraft_party::room::Action::Submit { epoch: phase, submission: value } if phase == epoch && value.assignment == assigned => {
                        submission = Some((request.request_id, value))
                    }
                    _ => {}
                }
            }
        }
        drop(rows);
        drop(statement);
        if prior_freeze {
            return Ok(());
        }
        let Some((submit_id, submission)) = submission else { return Ok(()) };
        let prepared =
            self.prepare(Operation::Apply { room, expected_revision, action: Box::new(photocraft_party::room::Action::FreezeReport { epoch, submission }) })?;
        let transaction = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        let first: i64 = transaction.query_row("SELECT COALESCE(MIN(seq),1) FROM outbox", [], |r| r.get(0)).map_err(sql)?;
        let sequence = first.checked_sub(1).ok_or_else(|| Error::Storage("Request sequence exhausted".into()))?;
        transaction
            .execute(
                "INSERT INTO outbox(seq,request_id,body,digest,secret_key) VALUES(?1,?2,?3,?4,?5)",
                params![sequence, prepared.id.as_str(), prepared.body, prepared.checksum, prepared.secret_key],
            )
            .map_err(sql)?;
        if let Some(project) = prepared.submission_project {
            transaction
                .execute("INSERT INTO outbox_submissions(request_id,project) VALUES(?1,?2)", params![prepared.id.as_str(), project.as_str()])
                .map_err(sql)?;
        }
        // A phase rejection at the deadline is recoverable by supplying its exact frozen
        // manifest. Other rejected operations remain blocked for explicit user review.
        transaction.execute("UPDATE outbox SET rejected=NULL WHERE request_id=?1 AND rejected LIKE 'phase:%'", [submit_id.as_str()]).map_err(sql)?;
        transaction.commit().map_err(sql)?;
        self.refresh_counts()?;
        if !self.state.status.blocked {
            self.state.status.error = None;
        }
        Ok(())
    }

    fn refresh_counts(&mut self) -> Result<()> {
        let count: i64 = self.db.query_row("SELECT COUNT(*) FROM outbox WHERE retained=0", [], |r| r.get(0)).map_err(sql)?;
        self.state.status.pending = usize::try_from(count).map_err(|_| Error::Storage("Invalid pending request count".into()))?;
        self.state.status.blocked =
            self.db.query_row("SELECT EXISTS(SELECT 1 FROM outbox WHERE retained=0 AND rejected IS NOT NULL)", [], |r| r.get(0)).map_err(sql)?;
        self.state.status.polling = self.state.active;
        let project: Option<String> = self
            .db
            .query_row("SELECT s.project FROM outbox_submissions s JOIN outbox o ON o.request_id=s.request_id WHERE o.retained=0 LIMIT 1", [], |row| row.get(0))
            .optional()
            .map_err(sql)?;
        self.state.status.submission_project = project
            .map(|project| photocraft_party::ProjectId::new(&project).map_err(|_| Error::Storage("Invalid pending submission project".into())))
            .transpose()?;
        Ok(())
    }

    fn rebuild_submission_index(&mut self) -> Result<()> {
        let mut statement = self.db.prepare("SELECT CASE WHEN length(body)<=?1 THEN body ELSE NULL END,digest FROM outbox LIMIT 8193").map_err(sql)?;
        let mut rows = statement.query([MAX_MESSAGE_BYTES as i64]).map_err(sql)?;
        let mut projects = Vec::new();
        let mut count = 0usize;
        while let Some(row) = rows.next().map_err(sql)? {
            count = count.saturating_add(1);
            if count > MAX_PENDING {
                return Err(Error::Busy);
            }
            let body: Vec<u8> = row.get(0).map_err(sql)?;
            let checksum: String = row.get(1).map_err(sql)?;
            if digest(&body) != checksum {
                return Err(Error::Storage("Pending request checksum failed; preserve this database".into()));
            }
            let request = RpcRequest::decode(&body).map_err(|_| Error::Storage("Invalid pending request".into()))?;
            if let Some(project) = submission_project(&request.operation) {
                projects.push((request.request_id, project));
            }
        }
        drop(rows);
        drop(statement);
        let transaction = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        transaction.execute("DELETE FROM outbox_submissions", []).map_err(sql)?;
        for (request, project) in projects {
            transaction
                .execute("INSERT INTO outbox_submissions(request_id,project) VALUES(?1,?2)", params![request.as_str(), project.as_str()])
                .map_err(sql)?;
        }
        transaction.commit().map_err(sql)
    }

    fn save_state(&mut self) -> Result<()> {
        let transaction = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        write_state(&transaction, &self.state)?;
        transaction.commit().map_err(sql)
    }
}

fn credential_key(endpoint: &str) -> String {
    format!("identity/{}", digest(endpoint.as_bytes()))
}
struct Prepared {
    id: RequestId,
    body: Vec<u8>,
    checksum: String,
    secret_key: Option<String>,
    submission_project: Option<photocraft_party::ProjectId>,
}
fn insert(transaction: &Transaction<'_>, request: &Prepared) -> Result<()> {
    transaction
        .execute(
            "INSERT INTO outbox(request_id,body,digest,secret_key) VALUES (?1,?2,?3,?4)",
            params![request.id.as_str(), request.body, request.checksum, request.secret_key],
        )
        .map_err(sql)?;
    if let Some(project) = request.submission_project {
        transaction.execute("INSERT INTO outbox_submissions(request_id,project) VALUES(?1,?2)", params![request.id.as_str(), project.as_str()]).map_err(sql)?;
    }
    Ok(())
}
fn submission_project(operation: &Operation) -> Option<photocraft_party::ProjectId> {
    match operation {
        Operation::Apply { action, .. } => match action.as_ref() {
            photocraft_party::room::Action::Submit { submission, .. } | photocraft_party::room::Action::FreezeReport { submission, .. } => {
                Some(submission.project)
            }
            _ => None,
        },
        _ => None,
    }
}
fn write_state(transaction: &Transaction<'_>, state: &SavedState) -> Result<()> {
    let body = serde_json::to_vec(state).map_err(|_| Error::Storage("Cannot encode room metadata".into()))?;
    if body.len() > MAX_STATE_BYTES {
        return Err(Error::Storage("Room metadata exceeds 5 MiB".into()));
    }
    transaction.execute("INSERT OR REPLACE INTO state(id,body,digest) VALUES(1,?1,?2)", params![body, digest(&body)]).map_err(sql)?;
    Ok(())
}
fn apply_response(state: &mut SavedState, request: &RpcRequest, response: &RpcResponse) -> Result<()> {
    match (&request.operation, response) {
        (Operation::Pair { participant, .. }, RpcResponse::Paired { participant: accepted }) if participant == accepted => state.status.paired = true,
        (Operation::Create { .. } | Operation::Join { .. } | Operation::Apply { .. }, RpcResponse::Room { view, .. }) => {
            if let Operation::Join { room, .. } | Operation::Apply { room, .. } = request.operation
                && room != view.id
            {
                return Err(Error::Invalid("Service acknowledgment named the wrong room".into()));
            }
            if !state.status.room.as_ref().is_some_and(|old| old.id == view.id && old.revision > view.revision) {
                state.status.room = Some(view.as_ref().clone());
            }
            if matches!(&request.operation, Operation::Create { .. } | Operation::Join { .. }) {
                state.active = true;
            }
            if matches!(&request.operation, Operation::Apply { action, .. } if matches!(action.as_ref(), photocraft_party::room::Action::Leave)) {
                leave_projection(state, view.id)?;
            }
            if let Operation::Apply { room, action, .. } = &request.operation
                && let photocraft_party::room::Action::FreezeReport { epoch, submission } = action.as_ref()
            {
                state.frozen = Some((*room, submission.assignment, *epoch));
            }
        }
        (Operation::Invite { room, .. }, RpcResponse::Invitation { id, expires_at_ms }) if *id == request.request_id => {
            state.last_invitation = Some((*room, *id, *expires_at_ms));
            state.status.invitation_id = Some(*id);
        }
        (Operation::RevokeInvite { invitation_id, .. }, RpcResponse::Revoked) => {
            if state.status.invitation_id == Some(*invitation_id) {
                state.last_invitation = None;
                state.status.invitation_id = None;
            }
        }
        (Operation::Disconnect { .. }, RpcResponse::Disconnected) => state.active = false,
        (Operation::Apply { room, action, .. }, RpcResponse::Disconnected) if matches!(action.as_ref(), photocraft_party::room::Action::Leave) => {
            leave_projection(state, *room)?;
        }
        (Operation::Upload { offset, bytes, finish, hash, total, .. }, RpcResponse::Uploaded { asset, next_offset }) => {
            let chunk_bytes = u64::try_from(bytes.len()).map_err(|_| Error::Invalid("Invalid pending upload size".into()))?;
            let end = offset.checked_add(chunk_bytes).ok_or_else(|| Error::Invalid("Invalid pending upload offset".into()))?;
            // An identical chunk may already belong to a larger received prefix or
            // completed object. Keep sending the queued chunks: the service verifies
            // their bytes before returning its durable high-water mark.
            if end > *total
                || *next_offset < end
                || *next_offset > *total
                || asset.is_some_and(|asset| asset.hash != *hash || asset.bytes != *total || *next_offset != *total)
                || (*finish && (end != *total || asset.is_none()))
            {
                return Err(Error::Invalid("Service upload acknowledgment did not match pending bytes".into()));
            }
        }
        _ => return Err(Error::Invalid("Service acknowledgment did not match the pending action".into())),
    }
    Ok(())
}

fn leave_projection(state: &mut SavedState, room: RoomId) -> Result<()> {
    let view = state
        .status
        .room
        .as_mut()
        .filter(|view| view.id == room)
        .ok_or_else(|| Error::Invalid("Leave acknowledgment named a different current room".into()))?;
    let actor = state.status.profile.as_ref().map(|profile| profile.id);
    let own_member = view.members.iter().any(|member| Some(member.participant.id) == actor);
    let retained_member = own_member && (view.phase != photocraft_party::room::Phase::Lobby || view.members.len() == 1);
    if retained_member {
        // An active game's roster is frozen. Keep its stable room/assignment for explicit
        // reconnect, but never resume heartbeat polling after this acknowledged leave.
        if let Some(member) = view.members.iter_mut().find(|member| Some(member.participant.id) == actor) {
            member.connected = false;
            member.ready = false;
        }
    } else {
        state.status.room = None;
        state.status.freeze_pin = None;
        state.frozen = None;
    }
    state.active = false;
    state.status.polling = false;
    state.status.connected = false;
    state.status.invitation_id = None;
    state.last_invitation = None;
    Ok(())
}
