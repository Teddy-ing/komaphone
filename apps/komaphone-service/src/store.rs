//! SQLite authority. All candidate state and receipts commit together.
use crate::{Error, Result, assets};
use photocraft_party::protocol::{Operation, PROTOCOL_VERSION, RpcRequest, RpcResponse, validate_secret};
use photocraft_party::room::{Action, Room};
use photocraft_party::{Error as DomainError, Participant, ParticipantId, RequestId, RoomId};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path, time::Duration};

const APPLICATION_ID: u32 = 0x4b504852;
const SCHEMA: u32 = 1;
const MAX_IDENTITIES: u64 = 256;
const MAX_ROOMS: u64 = 128;
const MAX_RECEIPTS: u64 = 65536;

#[derive(Clone, Copy, Debug)]
pub struct StoreOptions {
    pub quota_bytes: u64,
    pub heartbeat_timeout_ms: u64,
}
impl Default for StoreOptions {
    fn default() -> Self {
        Self { quota_bytes: 2 * 1024 * 1024 * 1024, heartbeat_timeout_ms: 5000 }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveStage {
    BeforeCommit,
    AfterCommit,
}

pub struct Store {
    db: Connection,
    options: StoreOptions,
    heartbeats: BTreeMap<(RoomId, ParticipantId), u64>,
}

impl Store {
    /// Open an existing local data file without replacing unknown schemas or corrupt data.
    /// The caller supplies a deterministic clock; the production service uses UTC origin + Instant.
    pub fn open(path: impl AsRef<Path>, options: StoreOptions, now_ms: u64) -> Result<Self> {
        if options.quota_bytes < 64 * 1024 || options.quota_bytes > 16 * 1024 * 1024 * 1024 || !(100..=60000).contains(&options.heartbeat_timeout_ms) {
            return Err(Error::Invalid("store limits"));
        }
        let mut db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(2))?;
        let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let app: u32 = db.pragma_query_value(None, "application_id", |r| r.get(0))?;
        if version > SCHEMA || (version != 0 && app != APPLICATION_ID) {
            return Err(Error::Schema);
        }
        if version == 0 {
            let tables: u64 = db.query_row("SELECT count(*) FROM sqlite_master WHERE type='table'", [], |r| read_u64(r, 0))?;
            if tables != 0 || app != 0 {
                return Err(Error::Schema);
            }
        }
        db.pragma_update(None, "journal_mode", "DELETE")?;
        db.pragma_update(None, "synchronous", "EXTRA")?;
        db.pragma_update(None, "foreign_keys", "ON")?;
        db.pragma_update(None, "trusted_schema", "OFF")?;
        let journal: String = db.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
        let sync: u32 = db.pragma_query_value(None, "synchronous", |r| r.get(0))?;
        let foreign: u32 = db.pragma_query_value(None, "foreign_keys", |r| r.get(0))?;
        if journal != "delete" || sync != 3 || foreign != 1 {
            return Err(Error::Storage);
        }
        let page_size: u64 = db.pragma_query_value(None, "page_size", |r| read_u64(r, 0))?;
        let pages = options.quota_bytes.checked_div(page_size).ok_or(Error::Invalid("page size"))?;
        db.pragma_update(None, "max_page_count", sql_int(pages)?)?;
        let actual_pages: u64 = db.pragma_query_value(None, "max_page_count", |r| read_u64(r, 0))?;
        if actual_pages > pages {
            return Err(Error::Full);
        }
        if version == 0 {
            initialize(&mut db)?;
        }
        let check: String = db.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if check != "ok" {
            return Err(Error::Storage);
        }
        let tx = db.transaction()?;
        assets::verify_store(&tx)?;
        for mut room in all_rooms(&tx)? {
            room.restart(now_ms)?;
            save_room(&tx, &room)?;
        }
        tx.commit()?;
        Ok(Self { db, options, heartbeats: BTreeMap::new() })
    }

    pub fn execute(&mut self, credential: Option<&str>, request: &RpcRequest, now_ms: u64) -> Result<RpcResponse> {
        self.execute_observed(credential, request, now_ms, |_| {})
    }
    pub fn execute_observed(&mut self, credential: Option<&str>, request: &RpcRequest, now_ms: u64, mut observe: impl FnMut(SaveStage)) -> Result<RpcResponse> {
        if request.protocol_version != PROTOCOL_VERSION {
            return Err(DomainError::Version(request.protocol_version).into());
        }
        let request_bytes = serde_json::to_vec(request)?;
        if request_bytes.len() > photocraft_party::MAX_MESSAGE_BYTES {
            return Err(DomainError::Limit("request bytes").into());
        }
        let tx = self.db.transaction()?;
        let actor = match &request.operation {
            Operation::Pair { participant, credential } => {
                pair(&tx, participant, credential)?;
                participant.clone()
            }
            _ => authenticate(&tx, credential)?,
        };
        let digest = digest(&request_bytes);
        let durable = !matches!(request.operation, Operation::Poll { .. } | Operation::Fetch { .. } | Operation::Notes { .. });
        if durable && let Some((prior_hash, response)) = receipt(&tx, actor.id, request.request_id)? {
            if prior_hash != digest {
                return Err(DomainError::Conflict.into());
            }
            let response: RpcResponse = serde_json::from_slice(&response)?;
            // Receipts preserve acceptance, never an obsolete reference grant or hidden projection.
            return if let RpcResponse::Room { view, outcome } = response { room_response(&load_room(&tx, view.id)?, actor.id, outcome) } else { Ok(response) };
        }
        let response = dispatch(&tx, &actor, request, now_ms)?;
        if durable {
            let count: u64 = tx.query_row("SELECT count(*) FROM receipts", [], |r| read_u64(r, 0))?;
            if count >= MAX_RECEIPTS {
                return Err(DomainError::Limit("retained receipts").into());
            }
            let response_bytes = serde_json::to_vec(&response)?;
            if response_bytes.len() > photocraft_party::protocol::MAX_RESPONSE_BYTES {
                return Err(DomainError::Limit("receipt response").into());
            }
            tx.execute(
                "INSERT INTO receipts(actor,id,hash,response) VALUES(?1,?2,?3,?4)",
                params![actor.id.as_str(), request.request_id.as_str(), digest, response_bytes],
            )?;
        }
        tx.cache_flush()?;
        observe(SaveStage::BeforeCommit);
        tx.commit()?;
        observe(SaveStage::AfterCommit);
        if let Some(room) = room_target(&request.operation, &response) {
            if matches!(request.operation, Operation::Disconnect { .. }) {
                self.heartbeats.remove(&(room, actor.id));
            } else {
                self.heartbeats.insert((room, actor.id), now_ms);
            }
        }
        Ok(response)
    }

    /// Heartbeats are ephemeral. Disconnection/clock consequences are durably committed.
    pub fn tick(&mut self, now_ms: u64) -> Result<()> {
        let tx = self.db.transaction()?;
        for mut room in all_rooms(&tx)? {
            for member in room.participants() {
                let seen = self.heartbeats.get(&(room.id(), member.id)).copied();
                if seen.is_none_or(|seen| now_ms.saturating_sub(seen) >= self.options.heartbeat_timeout_ms) {
                    room.set_connected(member.id, false, now_ms)?;
                }
            }
            room.tick(now_ms)?;
            save_room(&tx, &room)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Consistent SQLite backup, never a copy of an active database file.
    pub fn backup(&self, destination: impl AsRef<Path>) -> Result<()> {
        if destination.as_ref().exists() {
            return Err(Error::Invalid("backup destination already exists"));
        }
        let _reserved = std::fs::OpenOptions::new().write(true).create_new(true).open(destination.as_ref())?;
        self.db.backup("main", destination.as_ref(), None::<fn(rusqlite::backup::Progress)>)?;
        let db = Connection::open_with_flags(destination, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let tx = db.unchecked_transaction()?;
        assets::verify_store(&tx)?;
        all_rooms(&tx)?;
        Ok(())
    }
}

fn initialize(db: &mut Connection) -> Result<()> {
    let tx = db.transaction()?;
    tx.execute_batch("CREATE TABLE identities(actor TEXT PRIMARY KEY, device TEXT UNIQUE NOT NULL, label TEXT NOT NULL, verifier TEXT UNIQUE NOT NULL);
        CREATE TABLE rooms(id TEXT PRIMARY KEY,snapshot BLOB NOT NULL);
        CREATE TABLE archives(room TEXT NOT NULL,revision INTEGER NOT NULL,snapshot BLOB NOT NULL,PRIMARY KEY(room,revision));
        CREATE TABLE receipts(actor TEXT NOT NULL,id TEXT NOT NULL,hash TEXT NOT NULL,response BLOB NOT NULL,PRIMARY KEY(actor,id));
        CREATE TABLE invitations(id TEXT PRIMARY KEY,room TEXT NOT NULL,verifier TEXT UNIQUE NOT NULL,expires INTEGER NOT NULL,remaining INTEGER NOT NULL,revoked INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE objects(id INTEGER PRIMARY KEY,room TEXT NOT NULL,actor TEXT NOT NULL,hash TEXT NOT NULL,total INTEGER NOT NULL,received INTEGER NOT NULL,complete INTEGER NOT NULL,data BLOB NOT NULL,updated INTEGER NOT NULL,UNIQUE(room,actor,hash));")?;
    tx.pragma_update(None, "user_version", SCHEMA)?;
    tx.pragma_update(None, "application_id", APPLICATION_ID)?;
    tx.commit()?;
    Ok(())
}
pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn pair(tx: &Transaction<'_>, participant: &Participant, credential: &str) -> Result<()> {
    participant.validate()?;
    validate_secret(credential)?;
    let verifier = digest(credential.as_bytes());
    let prior: Option<(String, String, String)> = tx
        .query_row("SELECT device,label,verifier FROM identities WHERE actor=?1", [participant.id.as_str()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    if let Some((device, label, secret)) = prior {
        return if device == participant.device.as_str() && label == participant.display_name && secret == verifier {
            Ok(())
        } else {
            Err(DomainError::Unauthorized.into())
        };
    }
    let count: u64 = tx.query_row("SELECT count(*) FROM identities", [], |r| read_u64(r, 0))?;
    if count >= MAX_IDENTITIES {
        return Err(DomainError::Limit("paired identities").into());
    }
    let collision: bool =
        tx.query_row("SELECT EXISTS(SELECT 1 FROM identities WHERE device=?1 OR verifier=?2)", params![participant.device.as_str(), verifier], |r| r.get(0))?;
    if collision {
        return Err(DomainError::Unauthorized.into());
    }
    tx.execute(
        "INSERT INTO identities(actor,device,label,verifier) VALUES(?1,?2,?3,?4)",
        params![participant.id.as_str(), participant.device.as_str(), participant.display_name, verifier],
    )?;
    Ok(())
}
fn authenticate(tx: &Transaction<'_>, credential: Option<&str>) -> Result<Participant> {
    let token = credential.ok_or(DomainError::Unauthorized)?;
    validate_secret(token).map_err(|_| DomainError::Unauthorized)?;
    let found: Option<(String, String, String)> = tx
        .query_row("SELECT actor,device,label FROM identities WHERE verifier=?1", [digest(token.as_bytes())], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    let (actor, device, display_name) = found.ok_or(DomainError::Unauthorized)?;
    Ok(Participant { id: ParticipantId::new(&actor)?, device: photocraft_party::DeviceId::new(&device)?, display_name })
}
fn receipt(tx: &Transaction<'_>, actor: ParticipantId, id: RequestId) -> Result<Option<(String, Vec<u8>)>> {
    let row: Option<(String, Vec<u8>, u64)> = tx
        .query_row(
            "SELECT substr(hash,1,65),substr(response,1,?3),length(response) FROM receipts WHERE actor=?1 AND id=?2",
            params![actor.as_str(), id.as_str(), photocraft_party::protocol::MAX_RESPONSE_BYTES as i64],
            |r| Ok((r.get(0)?, r.get(1)?, read_u64(r, 2)?)),
        )
        .optional()?;
    match row {
        Some((hash, bytes, length)) if hash.len() == 64 && length == bytes.len() as u64 => Ok(Some((hash, bytes))),
        Some(_) => Err(Error::Storage),
        None => Ok(None),
    }
}
pub(crate) fn load_room(tx: &Transaction<'_>, id: RoomId) -> Result<Room> {
    let found: Option<(Vec<u8>, u64)> = tx
        .query_row(
            "SELECT substr(snapshot,1,?2),length(snapshot) FROM rooms WHERE id=?1",
            params![id.as_str(), photocraft_party::MAX_ROOM_SNAPSHOT_BYTES as i64],
            |r| Ok((r.get(0)?, read_u64(r, 1)?)),
        )
        .optional()?;
    let (bytes, length) = found.ok_or(DomainError::Unauthorized)?;
    if length != bytes.len() as u64 {
        return Err(Error::Storage);
    }
    Ok(Room::from_snapshot(&bytes)?)
}
fn all_rooms(tx: &Transaction<'_>) -> Result<Vec<Room>> {
    let mut query = tx.prepare("SELECT substr(snapshot,1,?1),length(snapshot) FROM rooms LIMIT 129")?;
    let rows = query.query_map([photocraft_party::MAX_ROOM_SNAPSHOT_BYTES as i64], |r| Ok((r.get::<_, Vec<u8>>(0)?, read_u64(r, 1)?)))?;
    let mut rooms = Vec::new();
    for bytes in rows {
        let (bytes, length) = bytes?;
        if length != bytes.len() as u64 {
            return Err(Error::Storage);
        }
        rooms.push(Room::from_snapshot(&bytes)?);
    }
    if rooms.len() > MAX_ROOMS as usize {
        return Err(DomainError::Limit("stored rooms").into());
    }
    Ok(rooms)
}
pub(crate) fn save_room(tx: &Transaction<'_>, room: &Room) -> Result<()> {
    tx.execute(
        "INSERT INTO rooms(id,snapshot) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET snapshot=excluded.snapshot",
        params![room.id().as_str(), room.snapshot_bytes()?],
    )?;
    Ok(())
}
fn fresh_room() -> Result<RoomId> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| Error::Storage)?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(RoomId::new(&hex)?)
}
fn room_response(room: &Room, actor: ParticipantId, outcome: Option<photocraft_party::room::ApplyOutcome>) -> Result<RpcResponse> {
    Ok(RpcResponse::Room { view: Box::new(room.view(actor)?), outcome })
}
fn room_target(operation: &Operation, response: &RpcResponse) -> Option<RoomId> {
    match operation {
        Operation::Pair { .. } => None,
        Operation::Create { .. } => {
            if let RpcResponse::Room { view, .. } = response {
                Some(view.id)
            } else {
                None
            }
        }
        Operation::Join { room, .. }
        | Operation::Invite { room, .. }
        | Operation::RevokeInvite { room, .. }
        | Operation::Poll { room, .. }
        | Operation::Apply { room, .. }
        | Operation::Disconnect { room }
        | Operation::Upload { room, .. }
        | Operation::Fetch { room, .. }
        | Operation::Notes { room } => Some(*room),
    }
}

fn dispatch(tx: &Transaction<'_>, actor: &Participant, request: &RpcRequest, now: u64) -> Result<RpcResponse> {
    match &request.operation {
        Operation::Pair { .. } => Ok(RpcResponse::Paired { participant: actor.clone() }),
        Operation::Create { mode, timer_ms } => {
            let count: u64 = tx.query_row("SELECT count(*) FROM rooms", [], |r| read_u64(r, 0))?;
            if count >= MAX_ROOMS {
                return Err(DomainError::Limit("rooms").into());
            }
            let room = Room::new(fresh_room()?, actor.clone(), *mode, *timer_ms)?;
            save_room(tx, &room)?;
            room_response(&room, actor.id, None)
        }
        Operation::Join { room, invitation } => join(tx, actor, *room, invitation, now),
        Operation::Invite { room, secret, expires_in_ms, uses } => invite(tx, actor.id, request.request_id, *room, (secret, *expires_in_ms, *uses), now),
        Operation::RevokeInvite { room, invitation_id } => {
            moderator(&load_room(tx, *room)?, actor.id)?;
            let changed = tx.execute("UPDATE invitations SET revoked=1 WHERE id=?1 AND room=?2", params![invitation_id.as_str(), room.as_str()])?;
            if changed != 1 {
                return Err(DomainError::Unauthorized.into());
            }
            Ok(RpcResponse::Revoked)
        }
        Operation::Poll { room, after_revision } => {
            let mut room = load_room(tx, *room)?;
            room.view(actor.id)?;
            if *after_revision > room.revision() {
                return Err(DomainError::Conflict.into());
            }
            room.set_connected(actor.id, true, now)?;
            save_room(tx, &room)?;
            room_response(&room, actor.id, None)
        }
        Operation::Apply { room, expected_revision, action } => apply(tx, actor.id, *room, *expected_revision, action, now),
        Operation::Disconnect { room } => {
            let mut room = load_room(tx, *room)?;
            room.set_connected(actor.id, false, now)?;
            save_room(tx, &room)?;
            Ok(RpcResponse::Disconnected)
        }
        Operation::Upload { room, hash, total, offset, bytes, finish } => {
            let domain = load_room(tx, *room)?;
            domain.view(actor.id)?;
            assets::upload(tx, (*room, actor.id, *hash), (*total, *offset, bytes, *finish), now)
        }
        Operation::Fetch { room, hash, offset } => {
            let domain = load_room(tx, *room)?;
            if !domain.can_fetch(actor.id, *hash) {
                return Err(DomainError::Unauthorized.into());
            }
            assets::fetch(tx, *room, *hash, *offset)
        }
        Operation::Notes { room } => Ok(RpcResponse::Notes { notes: load_room(tx, *room)?.notes(actor.id)? }),
    }
}
fn moderator(room: &Room, actor: ParticipantId) -> Result<()> {
    let view = room.view(actor)?;
    if view.moderator != actor || !view.members.iter().any(|m| m.participant.id == actor && m.connected) {
        return Err(DomainError::Unauthorized.into());
    }
    Ok(())
}
fn invite(tx: &Transaction<'_>, actor: ParticipantId, id: RequestId, room: RoomId, value: (&str, u64, u8), now: u64) -> Result<RpcResponse> {
    let (secret, duration, uses) = value;
    moderator(&load_room(tx, room)?, actor)?;
    validate_secret(secret)?;
    if !(1000..=24 * 60 * 60 * 1000).contains(&duration) || !(1..=2).contains(&uses) {
        return Err(DomainError::Invalid("invitation bounds").into());
    }
    let count: u64 = tx.query_row("SELECT count(*) FROM invitations WHERE room=?1", [room.as_str()], |r| read_u64(r, 0))?;
    if count >= 128 {
        return Err(DomainError::Limit("invitations").into());
    }
    let expiry = now.checked_add(duration).ok_or(DomainError::Limit("invitation time"))?;
    let verifier = digest(secret.as_bytes());
    let collision: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM invitations WHERE verifier=?1)", [&verifier], |r| r.get(0))?;
    if collision {
        return Err(DomainError::Conflict.into());
    }
    tx.execute(
        "INSERT INTO invitations(id,room,verifier,expires,remaining) VALUES(?1,?2,?3,?4,?5)",
        params![id.as_str(), room.as_str(), verifier, sql_int(expiry)?, uses],
    )?;
    Ok(RpcResponse::Invitation { id, expires_at_ms: expiry })
}
fn join(tx: &Transaction<'_>, actor: &Participant, id: RoomId, secret: &str, now: u64) -> Result<RpcResponse> {
    validate_secret(secret).map_err(|_| DomainError::Unauthorized)?;
    let mut room = load_room(tx, id)?;
    let invite: Option<(String, u64, u8, bool)> = tx
        .query_row("SELECT id,expires,remaining,revoked FROM invitations WHERE room=?1 AND verifier=?2", params![id.as_str(), digest(secret.as_bytes())], |r| {
            Ok((r.get(0)?, read_u64(r, 1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?;
    let (invite, expiry, remaining, revoked) = invite.ok_or(DomainError::Unauthorized)?;
    if revoked || remaining == 0 || now >= expiry {
        return Err(DomainError::Unauthorized.into());
    }
    let outcome = room.apply(actor.id, room.revision(), now, Action::Join { participant: actor.clone() })?;
    tx.execute("UPDATE invitations SET remaining=remaining-1 WHERE id=?1", [invite])?;
    save_room(tx, &room)?;
    room_response(&room, actor.id, Some(outcome))
}
fn apply(tx: &Transaction<'_>, actor: ParticipantId, id: RoomId, expected: u64, action: &Action, now: u64) -> Result<RpcResponse> {
    if matches!(action, Action::Join { .. }) {
        return Err(DomainError::Unauthorized.into());
    }
    let mut room = load_room(tx, id)?;
    if let Action::Start { game, projects } = action {
        check_archived_ids(tx, id, *game, projects)?;
    }
    let before = room.clone();
    let outcome = room.apply(actor, expected, now, action.clone())?;
    if !outcome.duplicate
        && let Action::Submit { submission, .. } = action
    {
        assets::validate_submission(tx, &before, submission)?;
    }
    if matches!(action, Action::NextGame) {
        let count: u64 = tx.query_row("SELECT count(*) FROM archives WHERE room=?1", [id.as_str()], |r| read_u64(r, 0))?;
        if count >= 256 {
            return Err(DomainError::Limit("archived games").into());
        }
        tx.execute(
            "INSERT INTO archives(room,revision,snapshot) VALUES(?1,?2,?3)",
            params![id.as_str(), sql_int(before.revision())?, before.snapshot_bytes()?],
        )?;
    }
    save_room(tx, &room)?;
    if matches!(action, Action::Leave) {
        return Ok(RpcResponse::Disconnected);
    }
    room_response(&room, actor, Some(outcome))
}
fn check_archived_ids(tx: &Transaction<'_>, id: RoomId, game: photocraft_party::GameId, projects: &[photocraft_party::ProjectId]) -> Result<()> {
    use photocraft_party::room::RevealedEntry;
    let mut query = tx.prepare("SELECT substr(snapshot,1,?2),length(snapshot) FROM archives WHERE room=?1 LIMIT 257")?;
    let rows = query.query_map(params![id.as_str(), photocraft_party::MAX_ROOM_SNAPSHOT_BYTES as i64], |r| Ok((r.get::<_, Vec<u8>>(0)?, read_u64(r, 1)?)))?;
    for (index, row) in rows.enumerate() {
        if index >= 256 {
            return Err(DomainError::Limit("archived games").into());
        }
        let (bytes, length) = row?;
        if bytes.len() as u64 != length {
            return Err(Error::Storage);
        }
        let archived = Room::from_snapshot(&bytes)?;
        let viewer = archived.participants().first().ok_or(Error::Storage)?.id;
        for entry in archived.view(viewer)?.reveal {
            let assignment = match entry {
                RevealedEntry::Artwork { assignment, .. } | RevealedEntry::Missing { assignment, .. } => assignment,
            };
            if assignment.id.game == game || projects.contains(&assignment.project) {
                return Err(DomainError::Conflict.into());
            }
        }
    }
    Ok(())
}

pub(crate) fn read_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}
pub(crate) fn sql_int(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::Invalid("SQLite integer range"))
}
