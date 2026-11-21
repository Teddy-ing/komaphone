//! Per-canvas membership and durable event authority. Uses the room service's single SQLite writer.
use photocraft_party::Error as Domain;
use photocraft_scene::shared::{AcceptedOp, DeviceId, Edit, WorldOp, WorldState};
use photocraft_scene::world_protocol::{self as wire, Action, CanvasSummary, Member, Request, Response};
use photocraft_scene::{CanvasId, OwnerId};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::store::{digest, read_u64, sql_int};
use crate::{Error, Result, SaveStage, Store};

mod archive_import;
mod media;
mod presence;
pub(crate) use presence::Presence;
mod restore;
mod storage_v6;
pub(crate) use storage_v6::{MAX_CHECKPOINT_BACKLOG, build_prepared_checkpoint_from_path, publish_prepared_checkpoint};
pub use storage_v6::{PendingCheckpointJob, PreparedCheckpoint};
mod stroke_copy;
mod trash;

pub(crate) fn scan_pending_checkpoints(db: &Connection) -> Result<Vec<PendingCheckpointJob>> {
    storage_v6::scan_pending_checkpoints(db, MAX_CHECKPOINT_BACKLOG)
}

const MAX_STATE_BYTES: usize = 32 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = wire::MAX_REQUEST_BYTES + 1024;
const MAX_INVITE_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// Deployment bounds, distinct from party rosters and invited-versus-online capacity.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub canvases: u32,
    pub members_per_canvas: u32,
    pub invitations_per_canvas: u32,
    pub presence_connections: u32,
}
impl Default for Limits {
    fn default() -> Self {
        Self { canvases: 1024, members_per_canvas: 4096, invitations_per_canvas: 4096, presence_connections: 256 }
    }
}
impl Limits {
    pub(crate) fn validate(self) -> Result<()> {
        if !(1..=16384).contains(&self.canvases)
            || !(1..=65536).contains(&self.members_per_canvas)
            || !(1..=65536).contains(&self.invitations_per_canvas)
            || !(1..=4096).contains(&self.presence_connections)
        {
            return Err(Error::Invalid("canvas service limits"));
        }
        Ok(())
    }
}

pub(crate) fn initialize(db: &Connection, path: &std::path::Path) -> Result<()> {
    let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='world_schema')", [], |r| r.get(0))?;
    if exists {
        verify(db)?;
        let mut version: u32 = db.query_row("SELECT version FROM world_schema", [], |r| r.get(0))?;
        if version == 1 {
            let mut random = [0u8; 16];
            getrandom::fill(&mut random).map_err(|_| Error::Storage)?;
            let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
            let mut name = path.file_name().ok_or(Error::Invalid("canvas migration database path"))?.to_os_string();
            name.push(format!(".before-world-v2-{suffix}.sqlite3"));
            crate::backup::copy(db, &path.with_file_name(name))?;
            let tx = db.unchecked_transaction()?;
            tx.execute_batch(
                "DROP TABLE world_schema; CREATE TABLE world_schema(version INTEGER NOT NULL CHECK(version=2)); INSERT INTO world_schema VALUES(2);",
            )?;
            tx.execute_batch(media::TABLES)?;
            verify(&tx)?;
            tx.commit()?;
            version = 2;
        }
        if version == 2 {
            trash::migrate(db, path)?;
            version = 3;
        }
        if version == 3 {
            archive_import::migrate(db, path)?;
            version = 4;
        }
        if version == 4 {
            migrate_v5(db, path)?;
            version = 5;
        }
        if version == 5 {
            storage_v6::migrate_v6(db, path)?;
            version = 6;
        }
        if version == 6 {
            db.pragma_update(None, "user_version", 6)?;
        }
        return Ok(());
    }
    let partial: u32 = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE name IN ('worlds','world_members','world_invites','world_events','world_media_sources','world_blobs','world_trash','world_imports','world_import_assets','world_import_receipts','world_import_origins','world_meta','world_devices','world_strokes_idx','world_media_idx','world_tours_idx','world_checkpoints')",
        [],
        |r| r.get(0),
    )?;
    if partial != 0 {
        return Err(Error::Schema);
    }
    let tx = db.unchecked_transaction()?;
    tx.execute_batch("CREATE TABLE world_schema(version INTEGER NOT NULL CHECK(version=6)); INSERT INTO world_schema VALUES(6);
        CREATE TABLE worlds(id TEXT PRIMARY KEY, owner TEXT NOT NULL REFERENCES identity_profiles(profile), name TEXT NOT NULL, initial_name TEXT NOT NULL, revision INTEGER NOT NULL, state BLOB NOT NULL, hash TEXT NOT NULL);
        CREATE TABLE world_members(canvas TEXT NOT NULL REFERENCES worlds(id), profile TEXT NOT NULL REFERENCES identity_profiles(profile), epoch INTEGER NOT NULL CHECK(epoch>0), active INTEGER NOT NULL CHECK(active IN (0,1)), PRIMARY KEY(canvas,profile));
        CREATE INDEX world_member_list ON world_members(profile,active,canvas);
        CREATE TABLE world_invites(canvas TEXT NOT NULL REFERENCES worlds(id), id TEXT NOT NULL, recipient TEXT NOT NULL, verifier TEXT NOT NULL, expires INTEGER NOT NULL, status INTEGER NOT NULL CHECK(status IN (0,1,2)), PRIMARY KEY(canvas,id));
        CREATE TABLE world_events(canvas TEXT NOT NULL REFERENCES worlds(id), revision INTEGER NOT NULL, id TEXT NOT NULL, body BLOB NOT NULL, hash TEXT NOT NULL, PRIMARY KEY(canvas,revision), UNIQUE(canvas,id));")?;
    tx.execute_batch(media::TABLES)?;
    tx.execute_batch(trash::TABLES)?;
    tx.execute_batch(archive_import::TABLES)?;
    tx.execute_batch(storage_v6::TABLES)?;
    tx.pragma_update(None, "user_version", 6)?;
    tx.commit()?;
    Ok(())
}

fn migrate_v5(db: &Connection, path: &std::path::Path) -> Result<()> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| Error::Storage)?;
    let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let mut name = path.file_name().ok_or(Error::Invalid("canvas migration path"))?.to_os_string();
    name.push(format!(".before-world-v5-{suffix}.sqlite3"));
    crate::backup::copy(db, &path.with_file_name(name))?;
    let tx = db.unchecked_transaction()?;
    tx.execute_batch("DROP TABLE world_schema; CREATE TABLE world_schema(version INTEGER NOT NULL CHECK(version=5)); INSERT INTO world_schema VALUES(5);")?;
    tx.pragma_update(None, "user_version", 5)?;
    verify(&tx)?;
    tx.commit()?;
    Ok(())
}

/// Backups and fresh restores validate both canonical checkpoints and the full accepted event stream.
pub(crate) fn verify(db: &Connection) -> Result<()> {
    let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='world_schema')", [], |r| r.get(0))?;
    if !exists {
        let partial: u32 = db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name IN ('worlds','world_members','world_invites','world_events','world_media_sources','world_blobs','world_trash','world_imports','world_import_assets','world_import_receipts','world_import_origins','world_meta','world_devices','world_strokes_idx','world_media_idx','world_tours_idx','world_checkpoints')",
            [],
            |r| r.get(0),
        )?;
        return if partial == 0 { Ok(()) } else { Err(Error::Schema) };
    }
    let versions: Vec<u32> = db.prepare("SELECT version FROM world_schema LIMIT 2")?.query_map([], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
    if versions != [1] && versions != [2] && versions != [3] && versions != [4] && versions != [5] && versions != [6] {
        return Err(Error::Schema);
    }
    let media_tables: u32 = db.query_row("SELECT count(*) FROM sqlite_master WHERE name IN ('world_media_sources','world_blobs')", [], |r| r.get(0))?;
    if (versions == [1] && media_tables != 0) || (versions != [1] && media_tables != 2) {
        return Err(Error::Schema);
    }
    if versions != [1] {
        media::verify(db)?;
    }
    let trash_tables: u32 = db.query_row("SELECT count(*) FROM sqlite_master WHERE name='world_trash'", [], |r| r.get(0))?;
    let has_trash = matches!(versions.as_slice(), [3] | [4] | [5] | [6]);
    if (has_trash && trash_tables != 1) || (!has_trash && trash_tables != 0) {
        return Err(Error::Schema);
    }
    if has_trash {
        trash::verify(db)?;
    }
    let import_tables: u32 = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE name IN ('world_imports','world_import_assets','world_import_receipts','world_import_origins')",
        [],
        |r| r.get(0),
    )?;
    let has_imports = matches!(versions.as_slice(), [4] | [5] | [6]);
    if (has_imports && import_tables != 4) || (!has_imports && import_tables != 0) {
        return Err(Error::Schema);
    }
    if has_imports {
        archive_import::verify(db)?;
    }
    let v6_tables: u32 = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE name IN ('world_meta','world_devices','world_strokes_idx','world_media_idx','world_tours_idx','world_checkpoints')",
        [],
        |r| r.get(0),
    )?;
    let is_v6 = versions == [6];
    if (is_v6 && v6_tables != 6) || (!is_v6 && v6_tables != 0) {
        return Err(Error::Schema);
    }
    let invalid:u64=db.query_row("SELECT count(*) FROM world_members m LEFT JOIN worlds w ON w.id=m.canvas LEFT JOIN identity_profiles p ON p.profile=m.profile WHERE w.id IS NULL OR p.profile IS NULL OR typeof(m.epoch)!='integer' OR m.epoch<1 OR m.active NOT IN (0,1)",[],|r|read_u64(r,0))?;
    if invalid != 0 {
        return Err(Error::Storage);
    }
    let invalid_events: u64 = db.query_row(
        "SELECT count(*) FROM world_events e LEFT JOIN worlds w ON w.id=e.canvas WHERE w.id IS NULL OR typeof(e.revision)!='integer' OR e.revision<1",
        [],
        |r| read_u64(r, 0),
    )?;
    let invalid_invites: u64 = db.query_row("SELECT count(*) FROM world_invites i LEFT JOIN worlds w ON w.id=i.canvas WHERE w.id IS NULL OR i.status NOT IN (0,1,2) OR typeof(i.expires)!='integer' OR i.expires<1 OR length(CAST(i.id AS BLOB))!=32 OR i.id GLOB '*[^0-9a-f]*' OR instr(i.id,char(0))>0 OR length(CAST(i.recipient AS BLOB))!=32 OR i.recipient GLOB '*[^0-9a-f]*' OR instr(i.recipient,char(0))>0 OR length(CAST(i.verifier AS BLOB))!=64 OR i.verifier GLOB '*[^0-9a-f]*' OR instr(i.verifier,char(0))>0", [], |r| read_u64(r,0))?;
    let canvas_count: u64 = db.query_row("SELECT count(*) FROM worlds", [], |r| read_u64(r, 0))?;
    if invalid_events != 0 || invalid_invites != 0 || canvas_count > 16384 {
        return Err(Error::Storage);
    }
    if is_v6 {
        return storage_v6::verify_v6(db);
    }
    let mut stmt = db.prepare("SELECT CAST(substr(CAST(id AS BLOB),1,33) AS TEXT),CAST(substr(CAST(owner AS BLOB),1,33) AS TEXT),CAST(substr(CAST(name AS BLOB),1,97) AS TEXT),revision,CAST(substr(CAST(initial_name AS BLOB),1,97) AS TEXT) FROM worlds ORDER BY id")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let owner: String = row.get(1)?;
        let name: String = row.get(2)?;
        let revision: u64 = read_u64(row, 3)?;
        let initial_name: String = row.get(4)?;
        photocraft_scene::validate_name(&initial_name).map_err(|_| Error::Storage)?;
        let id = CanvasId::new(&id).map_err(|_| Error::Storage)?;
        let owner = OwnerId::new(&owner).map_err(|_| Error::Storage)?;
        let world = legacy_state(db, id)?;
        world.validate().map_err(|_| Error::Storage)?;
        if world.import_origin.as_ref().is_some_and(|origin| world.revision < origin.source_revision) {
            return Err(Error::Storage);
        }
        if has_imports && world.import_origin != archive_import::origin(db, id)? {
            return Err(Error::Storage);
        }
        if versions != [5] && world.schema != photocraft_scene::shared::SHARED_SCHEMA {
            return Err(Error::Schema);
        }
        if versions != [1] {
            media::verify_world(db, &world)?;
        } else if !world.media.is_empty() {
            return Err(Error::Storage);
        }
        if world.canvas != id
            || world.owner != owner
            || world.name != name
            || world.revision != revision
            || member_epoch(db, id, owner).is_err()
            || world.replay_to(0).map_err(|_| Error::Storage)?.name != initial_name
        {
            return Err(Error::Storage);
        }
        let mut events = db.prepare("SELECT substr(CAST(body AS BLOB),1,?2),CAST(substr(CAST(hash AS BLOB),1,65) AS TEXT),revision,CAST(substr(CAST(id AS BLOB),1,33) AS TEXT) FROM world_events WHERE canvas=?1 ORDER BY revision")?;
        let mut events = events.query(params![id.as_str(), (MAX_EVENT_BYTES + 1) as i64])?;
        let mut count = 0;
        while let Some(row) = events.next()? {
            let (event, _) = read_event(row, id)?;
            if world.receipt(event.op.id) != Some(&event) || event.revision != count + 1 {
                return Err(Error::Storage);
            }
            count += 1;
        }
        if count != revision {
            return Err(Error::Storage);
        }
    }
    Ok(())
}

fn denied<T>() -> Result<T> {
    Err(Domain::Unauthorized.into())
}
fn scene_error(error: impl std::fmt::Display) -> Error {
    // Remote data never supplies error strings containing bearer material.
    let _ = error;
    Domain::Conflict.into()
}
fn grant(db: &Connection, canvas: CanvasId, profile: OwnerId) -> Result<u64> {
    trash::require_live(db, canvas)?;
    member_epoch(db, canvas, profile)
}
fn member_epoch(db: &Connection, canvas: CanvasId, profile: OwnerId) -> Result<u64> {
    db.query_row("SELECT epoch FROM world_members WHERE canvas=?1 AND profile=?2 AND active=1", params![canvas.as_str(), profile.as_str()], |r| read_u64(r, 0))
        .optional()?
        .ok_or_else(|| Domain::Unauthorized.into())
}

fn summary(db: &Connection, canvas: CanvasId, profile: OwnerId) -> Result<CanvasSummary> {
    let epoch = grant(db, canvas, profile)?;
    let (owner, name, initial_name, revision): (String, String, String, u64) =
        db.query_row("SELECT CAST(substr(CAST(owner AS BLOB),1,33) AS TEXT),CAST(substr(CAST(name AS BLOB),1,97) AS TEXT),CAST(substr(CAST(initial_name AS BLOB),1,97) AS TEXT),revision FROM worlds WHERE id=?1", [canvas.as_str()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, read_u64(r, 3)?))
        })?;
    photocraft_scene::validate_name(&name).map_err(|_| Error::Storage)?;
    photocraft_scene::validate_name(&initial_name).map_err(|_| Error::Storage)?;
    let import_origin = archive_import::origin(db, canvas)?;
    if import_origin.as_ref().is_some_and(|origin| revision < origin.source_revision) {
        return Err(Error::Storage);
    }
    Ok(CanvasSummary { canvas, owner: OwnerId::new(&owner).map_err(scene_error)?, name, initial_name, revision, epoch, import_origin })
}
fn require_owner(db: &Connection, canvas: CanvasId, profile: OwnerId) -> Result<CanvasSummary> {
    let info = summary(db, canvas, profile)?;
    if info.owner != profile {
        return denied();
    }
    Ok(info)
}
fn page(limit: u32) -> Result<u32> {
    if !(1..=wire::MAX_PAGE).contains(&limit) {
        return Err(Error::Invalid("canvas page size"));
    }
    Ok(limit)
}
fn legacy_state(db: &Connection, canvas: CanvasId) -> Result<WorldState> {
    let (body, hash): (Vec<u8>, String) = db.query_row(
        "SELECT substr(CAST(state AS BLOB),1,?2),CAST(substr(CAST(hash AS BLOB),1,65) AS TEXT) FROM worlds WHERE id=?1",
        params![canvas.as_str(), (MAX_STATE_BYTES + 1) as i64],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if body.len() > MAX_STATE_BYTES || digest(&body) != hash {
        return Err(Error::Storage);
    }
    Ok(serde_json::from_slice(&body)?)
}

/// Callers cap both BLOB and text projections in SQL before reading this row.
fn read_event(row: &rusqlite::Row<'_>, canvas: CanvasId) -> Result<(AcceptedOp, usize)> {
    let raw: Vec<u8> = row.get(0)?;
    let hash: String = row.get(1)?;
    let revision = read_u64(row, 2)?;
    let id: String = row.get(3)?;
    let bytes = storage_v6::decode_event_body(&raw)?;
    if hash.len() != 64 || digest(&bytes) != hash {
        return Err(Error::Storage);
    }
    let event: AcceptedOp = serde_json::from_slice(&bytes)?;
    if event.revision != revision || event.op.id.as_str() != id || event.op.canvas != canvas {
        return Err(Error::Storage);
    }
    event.op.validate().map_err(|_| Error::Storage)?;
    Ok((event, bytes.len()))
}

impl Store {
    pub fn world_execute(&mut self, credential: &str, request: &Request, now: u64) -> Result<Response> {
        self.world_execute_observed(credential, request, now, self.options.world_limits, |_| {})
    }
    pub fn world_execute_observed(
        &mut self,
        credential: &str,
        request: &Request,
        now: u64,
        limits: Limits,
        mut observe: impl FnMut(SaveStage),
    ) -> Result<Response> {
        limits.validate()?;
        if request.version != wire::VERSION {
            return Err(Domain::Version(request.version).into());
        }
        if serde_json::to_vec(request)?.len() > wire::MAX_REQUEST_BYTES {
            return Err(Domain::Limit("canvas request").into());
        }
        let auth = self.identity_authenticate(credential, now).map_err(|error| match error {
            Error::Domain(Domain::Unauthorized) => Error::Session,
            other => other,
        })?;
        let profile = OwnerId::new(auth.profile.as_str()).map_err(scene_error)?;
        let device = DeviceId::new(auth.device.as_str()).map_err(scene_error)?;
        if matches!(
            request.action,
            Action::Presence { .. } | Action::Peers { .. } | Action::PresenceOpen { .. } | Action::PresenceUpdate { .. } | Action::PresenceRead { .. }
        ) {
            let response = self.world_presence_request(profile, device, &request.action, now, limits)?;
            if serde_json::to_vec(&response)?.len() > wire::MAX_RESPONSE_BYTES {
                return Err(Domain::Limit("canvas presence response; reduce concurrent presence").into());
            }
            return Ok(response);
        }
        let tx = self.db.transaction()?;
        let response = dispatch(&tx, profile, device, &request.action, now, limits)?;
        if serde_json::to_vec(&response)?.len() > wire::MAX_RESPONSE_BYTES {
            return Err(Domain::Limit("canvas response; use a smaller page").into());
        }
        let queued_ckpt = if let Response::Accepted { event } = &response
            && event.revision > 0
            && event.revision.is_multiple_of(storage_v6::CHECKPOINT_STRIDE)
        {
            storage_v6::pending_job_at_head(&tx, event.op.canvas, event.revision)?
        } else {
            None
        };
        tx.cache_flush()?;
        observe(SaveStage::BeforeCommit);
        tx.commit()?;
        observe(SaveStage::AfterCommit);
        if let Some(job) = queued_ckpt {
            self.queue_pending_checkpoint(job);
        }
        match &request.action {
            Action::Delete { canvas, .. } => {
                self.world_presence.remove_canvas(*canvas);
                self.pending_checkpoints.remove(canvas);
            }
            Action::Remove { canvas, member } => self.world_presence.remove_member(*canvas, *member),
            Action::Leave { canvas } => self.world_presence.remove_member(*canvas, profile),
            _ => {}
        }
        if let Response::Accepted { event } = &response {
            self.world_presence.committed(&event.op);
        }
        Ok(response)
    }
}

fn dispatch(db: &Transaction<'_>, profile: OwnerId, device: DeviceId, action: &Action, now: u64, limits: Limits) -> Result<Response> {
    match action {
        Action::Identity {} => Ok(Response::Identity { profile, device }),
        Action::ImportBegin { id, new_canvas, manifest_sha256, manifest_bytes, total_bytes } => {
            archive_import::begin(db, profile, device, *id, *new_canvas, *manifest_sha256, *manifest_bytes, *total_bytes, now)
        }
        Action::ImportChunk { id, object, offset, bytes } => archive_import::chunk(db, profile, device, *id, object, *offset, bytes, now),
        Action::ImportCommit { id } => archive_import::commit(db, profile, device, *id, now, limits),
        Action::ImportCancel { id } => archive_import::cancel(db, profile, device, *id),
        Action::Trash { after, limit } => trash::list(db, profile, *after, *limit),
        Action::Delete { canvas, expected_epoch } => trash::delete(db, profile, *canvas, *expected_epoch, now),
        Action::RestoreOriginal { canvas, expected_epoch } => trash::restore(db, profile, *canvas, *expected_epoch),
        Action::RestoreCopy { source_canvas, source_revision, source_hash, new_canvas } => {
            restore::copy(db, profile, *source_canvas, *source_revision, *source_hash, *new_canvas, limits)
        }
        Action::PrepareGameMedia { canvas, source } => media::prepare(db, profile, *canvas, source),
        Action::RemixGame { operation, room } => media::remix_game(db, profile, device, operation, *room),
        Action::RemixCanvas { operation, source_canvas, source_media } => media::remix_canvas(db, profile, device, operation, *source_canvas, *source_media),
        Action::RemixStroke { operation } => stroke_copy::copy(db, profile, device, operation),
        Action::FetchMedia { canvas, hash, offset } => media::fetch(db, profile, *canvas, *hash, *offset),
        Action::People { after, limit } => {
            let limit = page(*limit)?;
            let mut stmt = db.prepare(
                "SELECT CAST(substr(CAST(profile AS BLOB),1,33) AS TEXT),CAST(substr(CAST(label AS BLOB),1,97) AS TEXT) FROM hosted_profiles WHERE revoked=0 AND profile>?1 ORDER BY profile LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(params![after.as_ref().map_or("", OwnerId::as_str), limit + 1], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let mut items = Vec::new();
            for (profile, display_name) in rows.iter().take(limit as usize) {
                photocraft_party::validate_label(display_name, photocraft_party::MAX_NAME_BYTES, false).map_err(|_| Error::Storage)?;
                items.push(wire::Person { profile: OwnerId::new(profile).map_err(scene_error)?, display_name: display_name.clone() });
            }
            let next = if rows.len() > limit as usize { items.last().map(|p| p.profile) } else { None };
            Ok(Response::People { items, next })
        }
        Action::Create { canvas, name } => {
            photocraft_scene::validate_name(name).map_err(|_| Error::Invalid("canvas name"))?;
            if db.query_row("SELECT 1 FROM worlds WHERE id=?1", [canvas.as_str()], |r| r.get::<_, u8>(0)).optional()?.is_some() {
                let info = require_owner(db, *canvas, profile)?;
                if info.name != *name {
                    return Err(Domain::Conflict.into());
                }
                return Ok(Response::Canvas { info });
            }
            let count: u32 = db.query_row("SELECT count(*) FROM worlds", [], |r| r.get(0))?;
            if count >= limits.canvases {
                return Err(Domain::Limit("canvas count").into());
            }
            let world = WorldState::new(*canvas, profile, name.clone()).map_err(scene_error)?;
            storage_v6::insert_world(db, &world)?;
            Ok(Response::Canvas { info: summary(db, *canvas, profile)? })
        }
        Action::List { after, limit } => {
            let limit = page(*limit)?;
            let mut stmt = db.prepare("SELECT CAST(substr(CAST(canvas AS BLOB),1,33) AS TEXT) FROM world_members WHERE profile=?1 AND active=1 AND canvas>?2 AND NOT EXISTS(SELECT 1 FROM world_trash t WHERE t.canvas=world_members.canvas) ORDER BY canvas LIMIT ?3")?;
            let ids = stmt
                .query_map(params![profile.as_str(), after.as_ref().map_or("", CanvasId::as_str), limit + 1], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let mut items = Vec::new();
            for id in ids.iter().take(limit as usize) {
                items.push(summary(db, CanvasId::new(id).map_err(scene_error)?, profile)?);
            }
            let next = if ids.len() > limit as usize { items.last().map(|i| i.canvas) } else { None };
            Ok(Response::Canvases { items, next })
        }
        Action::Info { canvas } => Ok(Response::Canvas { info: summary(db, *canvas, profile)? }),
        Action::Members { canvas, after, limit } => {
            let info = summary(db, *canvas, profile)?;
            let limit = page(*limit)?;
            let mut stmt = db.prepare("SELECT CAST(substr(CAST(profile AS BLOB),1,33) AS TEXT) FROM world_members WHERE canvas=?1 AND active=1 AND profile>?2 ORDER BY profile LIMIT ?3")?;
            let ids = stmt
                .query_map(params![canvas.as_str(), after.as_ref().map_or("", OwnerId::as_str), limit + 1], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let mut items = Vec::new();
            for id in ids.iter().take(limit as usize) {
                let profile = OwnerId::new(id).map_err(scene_error)?;
                items.push(Member { profile, owner: profile == info.owner });
            }
            let next = if ids.len() > limit as usize { items.last().map(|i| i.profile) } else { None };
            Ok(Response::Members { items, next })
        }
        Action::Invite { canvas, id, recipient, secret, expires_at_ms } => {
            require_owner(db, *canvas, profile)?;
            photocraft_party::protocol::validate_secret(secret)?;
            if *recipient == profile || *expires_at_ms <= now || expires_at_ms.saturating_sub(now) > MAX_INVITE_MS {
                return Err(Error::Invalid("canvas invitation recipient/expiry"));
            }
            let old: Option<(String, String, u64, u8)> = db
                .query_row(
                    "SELECT CAST(substr(CAST(recipient AS BLOB),1,33) AS TEXT),CAST(substr(CAST(verifier AS BLOB),1,65) AS TEXT),expires,status FROM world_invites WHERE canvas=?1 AND id=?2",
                    params![canvas.as_str(), id.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?, read_u64(r, 2)?, r.get(3)?)),
                )
                .optional()?;
            if let Some((who, hash, expiry, status)) = old {
                if who != recipient.as_str() || hash != digest(secret.as_bytes()) || expiry != *expires_at_ms || status != 0 {
                    return Err(Domain::Conflict.into());
                }
            } else {
                let count: u32 = db.query_row("SELECT count(*) FROM world_invites WHERE canvas=?1", [canvas.as_str()], |r| r.get(0))?;
                if count >= limits.invitations_per_canvas {
                    return Err(Domain::Limit("retained canvas invitations").into());
                }
                db.execute(
                    "INSERT INTO world_invites VALUES(?1,?2,?3,?4,?5,0)",
                    params![canvas.as_str(), id.as_str(), recipient.as_str(), digest(secret.as_bytes()), sql_int(*expires_at_ms)?],
                )?;
            }
            Ok(Response::Invited { id: *id, expires_at_ms: *expires_at_ms })
        }
        Action::CancelInvite { canvas, id } => {
            require_owner(db, *canvas, profile)?;
            db.execute("UPDATE world_invites SET status=2 WHERE canvas=?1 AND id=?2", params![canvas.as_str(), id.as_str()])?;
            Ok(Response::Done)
        }
        Action::Accept { canvas, id, secret } => {
            trash::require_live(db, *canvas)?;
            photocraft_party::protocol::validate_secret(secret)?;
            let old: Option<(String, String, u64, u8)> = db
                .query_row(
                    "SELECT CAST(substr(CAST(recipient AS BLOB),1,33) AS TEXT),CAST(substr(CAST(verifier AS BLOB),1,65) AS TEXT),expires,status FROM world_invites WHERE canvas=?1 AND id=?2",
                    params![canvas.as_str(), id.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?, read_u64(r, 2)?, r.get(3)?)),
                )
                .optional()?;
            let Some((who, hash, expiry, status)) = old else {
                return denied();
            };
            if who != profile.as_str() || hash != digest(secret.as_bytes()) || expiry <= now || !matches!(status, 0 | 1) {
                return denied();
            }
            if status == 1 {
                return Ok(Response::Canvas { info: summary(db, *canvas, profile)? });
            }
            let count: u32 = db.query_row("SELECT count(*) FROM world_members WHERE canvas=?1 AND active=1", [canvas.as_str()], |r| r.get(0))?;
            if count >= limits.members_per_canvas {
                return Err(Domain::Limit("canvas members").into());
            }
            // Membership epochs are retained after removal. A fresh invitation cannot reactivate an old queue.
            db.execute(
                "INSERT INTO world_members VALUES(?1,?2,1,1) ON CONFLICT(canvas,profile) DO UPDATE SET active=1",
                params![canvas.as_str(), profile.as_str()],
            )?;
            db.execute("UPDATE world_invites SET status=1 WHERE canvas=?1 AND id=?2", params![canvas.as_str(), id.as_str()])?;
            Ok(Response::Canvas { info: summary(db, *canvas, profile)? })
        }
        Action::Remove { canvas, member } => {
            let info = require_owner(db, *canvas, profile)?;
            if *member == info.owner {
                return Err(Error::Invalid("canvas owner cannot leave; use recoverable canvas deletion"));
            }
            revoke(db, *canvas, *member)?;
            Ok(Response::Done)
        }
        Action::Leave { canvas } => {
            let info = summary(db, *canvas, profile)?;
            if profile == info.owner {
                return Err(Error::Invalid("canvas owner cannot leave"));
            }
            revoke(db, *canvas, profile)?;
            Ok(Response::Done)
        }
        Action::Append { operation } => {
            let epoch = grant(db, operation.canvas, profile)?;
            if operation.author != profile || operation.device != device || operation.authority_epoch != epoch {
                return denied();
            }
            if matches!(operation.action, Edit::PlaceMedia { .. } | Edit::CopyStroke { .. }) {
                return Err(Error::Invalid("copied artwork requires verified source authorization"));
            }
            if matches!(operation.action, Edit::Rename { .. }) {
                require_owner(db, operation.canvas, profile)?;
            }
            append_authorized(db, operation)
        }
        Action::Events { canvas, after, through, limit } => {
            let info = summary(db, *canvas, profile)?;
            let through = through.unwrap_or(info.revision);
            if *after > through || through > info.revision {
                return Err(Error::Invalid("canvas event interval"));
            }
            let limit = page(*limit)?;
            let mut stmt = db.prepare("SELECT substr(CAST(body AS BLOB),1,?5),CAST(substr(CAST(hash AS BLOB),1,65) AS TEXT),revision,CAST(substr(CAST(id AS BLOB),1,33) AS TEXT) FROM world_events WHERE canvas=?1 AND revision>?2 AND revision<=?3 ORDER BY revision LIMIT ?4")?;
            let mut rows = stmt.query(params![canvas.as_str(), sql_int(*after)?, sql_int(through)?, limit, (MAX_EVENT_BYTES + 1) as i64])?;
            let mut events = Vec::new();
            let mut used = 1024;
            let mut last = *after;
            let mut byte_limited = false;
            while let Some(row) = rows.next()? {
                let (event, bytes) = read_event(row, *canvas)?;
                if event.revision != last.checked_add(1).ok_or(Error::Storage)? {
                    return Err(Error::Storage);
                }
                if used + bytes > wire::MAX_RESPONSE_BYTES - 1024 {
                    byte_limited = true;
                    break;
                }
                used += bytes;
                last = event.revision;
                events.push(event);
            }
            if last < through && (events.is_empty() || (!byte_limited && events.len() < limit as usize)) {
                return Err(Error::Storage);
            }
            Ok(Response::Events { events, through, next: (last < through).then_some(last) })
        }
        Action::CheckpointMeta { canvas } => storage_v6::checkpoint_meta(db, profile, *canvas),
        Action::CheckpointChunk { canvas, revision, compressed_sha256, offset, len } => {
            storage_v6::checkpoint_chunk(db, profile, *canvas, *revision, *compressed_sha256, *offset, *len)
        }
        Action::Presence { .. } | Action::Peers { .. } | Action::PresenceOpen { .. } | Action::PresenceUpdate { .. } | Action::PresenceRead { .. } => {
            Err(Error::Invalid("presence requires the live service channel"))
        }
    }
}

fn revoke(db: &Connection, canvas: CanvasId, member: OwnerId) -> Result<()> {
    let epoch = db
        .query_row("SELECT epoch FROM world_members WHERE canvas=?1 AND profile=?2 AND active=1", params![canvas.as_str(), member.as_str()], |row| {
            read_u64(row, 0)
        })
        .optional()?;
    if let Some(epoch) = epoch {
        let next = epoch.checked_add(1).filter(|value| *value <= i64::MAX as u64).ok_or(Domain::Limit("canvas membership epoch"))?;
        db.execute(
            "UPDATE world_members SET active=0,epoch=?3 WHERE canvas=?1 AND profile=?2 AND active=1",
            params![canvas.as_str(), member.as_str(), sql_int(next)?],
        )?;
    }
    db.execute("UPDATE world_invites SET status=2 WHERE canvas=?1 AND recipient=?2", params![canvas.as_str(), member.as_str()])?;
    Ok(())
}

/// Caller has already checked the current destination grant and any source authority.
fn prior_receipt(db: &Connection, operation: &WorldOp) -> Result<Option<Response>> {
    let mut statement = db.prepare("SELECT substr(CAST(body AS BLOB),1,?3),CAST(substr(CAST(hash AS BLOB),1,65) AS TEXT),revision,CAST(substr(CAST(id AS BLOB),1,33) AS TEXT) FROM world_events WHERE canvas=?1 AND id=?2")?;
    let mut rows = statement.query(params![operation.canvas.as_str(), operation.id.as_str(), (MAX_EVENT_BYTES + 1) as i64])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let (event, _) = read_event(row, operation.canvas)?;
    if event.op != *operation {
        return Err(Domain::Conflict.into());
    }
    Ok(Some(Response::Accepted { event: Box::new(event) }))
}

fn append_authorized(db: &Connection, operation: &WorldOp) -> Result<Response> {
    storage_v6::append_authorized_v6(db, operation)
}
