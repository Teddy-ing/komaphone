use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::path::Path;
use std::time::Duration;

use photocraft_party::gallery::{GalleryBundle, GalleryContent, GalleryManifest, MAX_GALLERY_BYTES, MAX_GALLERY_MANIFEST_BYTES};
use photocraft_party::{AssetHash, AssetRef, AssignmentId, Mode, ProjectId};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{DraftPreference, GALLERY_STORE_SCHEMA, GalleryInfo, GalleryKind, MAX_GALLERY_ENTRIES, MAX_STORED_OBJECTS};
use crate::{Error, Result, StoreOptions, Usage, fresh_project_id};

const APPLICATION_ID: u32 = 0x4b47414c;
const CHUNK_BYTES: usize = 64 * 1024;
const MAX_METADATA_BYTES: usize = MAX_GALLERY_MANIFEST_BYTES + 128 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const ARCHIVE_MAGIC: &[u8; 8] = b"KOMAAR1\0";
const BACKUP_MAGIC: &[u8; 8] = b"KOMABK1\0";
const FOOTER: &[u8; 8] = b"KOMAEND1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GalleryStage {
    BeforeCommit,
    AfterCommit,
}

pub struct GalleryStore {
    db: Connection,
    options: StoreOptions,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredEntry {
    manifest: GalleryManifest,
    info: GalleryInfo,
    #[serde(default)]
    notes: Vec<(AssignmentId, String)>,
}

fn sql(error: rusqlite::Error) -> Error {
    match &error {
        rusqlite::Error::SqliteFailure(code, _) if code.extended_code & 0xff == 13 => Error::Full,
        _ => Error::Storage(format!("Gallery: {error}")),
    }
}
fn invalid(error: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("Gallery: {error}"))
}
fn io(error: impl std::fmt::Display) -> Error {
    Error::Storage(format!("Gallery archive: {error}"))
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
fn valid_timestamp(value: &str) -> Result<()> {
    chrono::DateTime::parse_from_rfc3339(value).map_err(|_| invalid("invalid calendar timestamp"))?;
    Ok(())
}
fn checked_manifest(manifest: &GalleryManifest) -> Result<()> {
    manifest.validate().map_err(invalid)?;
    valid_timestamp(&manifest.created_at)?;
    valid_timestamp(&manifest.updated_at)
}

fn summary(manifest: &GalleryManifest) -> Result<GalleryInfo> {
    checked_manifest(manifest)?;
    let (kind, complete) = match &manifest.content {
        GalleryContent::Project { .. } => (GalleryKind::Project, true),
        GalleryContent::Game { room } => (
            match room.mode {
                Mode::Animation { .. } => GalleryKind::Animation,
                Mode::ExquisiteCorpse => GalleryKind::ExquisiteCorpse,
            },
            room.reveal.iter().all(|entry| matches!(entry, photocraft_party::room::RevealedEntry::Artwork { .. })),
        ),
    };
    let bytes = manifest.required_assets().map_err(invalid)?.iter().try_fold(0u64, |sum, asset| {
        sum.checked_add(asset.bytes).filter(|sum| *sum <= MAX_GALLERY_BYTES).ok_or_else(|| invalid("bundle bytes exceed limit"))
    })?;
    Ok(GalleryInfo {
        id: manifest.id,
        title: manifest.title.clone(),
        kind,
        authors: manifest.authors.clone(),
        complete,
        favorite: false,
        trashed: false,
        created_at: manifest.created_at.clone(),
        updated_at: manifest.updated_at.clone(),
        bytes,
        imported_from: None,
        game: manifest.game(),
        fps: manifest.fps,
        looping: manifest.looping,
        source_projects: source_projects(manifest, None),
    })
}

fn source_projects(manifest: &GalleryManifest, imported_from: Option<ProjectId>) -> Vec<ProjectId> {
    let mut sources = manifest.source_projects();
    if matches!(manifest.content, GalleryContent::Project { .. }) {
        sources.extend(imported_from);
    }
    sources.sort();
    sources.dedup();
    sources
}

impl StoredEntry {
    fn hydrate_sources(&mut self) {
        self.info.source_projects = source_projects(&self.manifest, self.info.imported_from);
    }

    fn validate(&self) -> Result<()> {
        let expected = summary(&self.manifest)?;
        validate_info(&self.info)?;
        let mut with_notes = self.manifest.clone();
        with_notes.notes = self.notes.clone();
        with_notes.validate().map_err(invalid)?;
        photocraft_party::validate_label(&self.info.title, 96, false).map_err(invalid)?;
        valid_timestamp(&self.info.updated_at)?;
        if self.info.id != expected.id
            || self.info.kind != expected.kind
            || self.info.authors != expected.authors
            || self.info.complete != expected.complete
            || self.info.created_at != expected.created_at
            || self.info.bytes != expected.bytes
            || self.info.game != expected.game
            || self.info.imported_from == Some(self.info.id)
            || (!self.info.source_projects.is_empty() && self.info.source_projects != source_projects(&self.manifest, self.info.imported_from))
        {
            return Err(invalid("metadata does not match its original"));
        }
        Ok(())
    }
}

struct BoundedBytes {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for BoundedBytes {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().saturating_add(data.len()) > self.limit {
            return Err(std::io::Error::other("encoded record exceeds its limit"));
        }
        self.bytes.try_reserve(data.len()).map_err(|_| std::io::Error::other("not enough memory for gallery record"))?;
        self.bytes.extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn json_bytes(value: &impl Serialize, limit: usize) -> Result<Vec<u8>> {
    let mut writer = BoundedBytes { bytes: Vec::new(), limit };
    serde_json::to_writer(&mut writer, value).map_err(invalid)?;
    Ok(writer.bytes)
}

impl GalleryStore {
    pub fn open(path: impl AsRef<Path>, options: StoreOptions) -> Result<Self> {
        let path = path.as_ref();
        if !(64 * 1024..=MAX_ARCHIVE_BYTES).contains(&options.quota_bytes) {
            return Err(invalid("quota must be 64 KiB through 64 GiB"));
        }
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let mut db = Connection::open(path).map_err(sql)?;
        db.busy_timeout(Duration::from_secs(2)).map_err(sql)?;
        let version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0)).map_err(sql)?;
        let app_id: u32 = db.pragma_query_value(None, "application_id", |row| row.get(0)).map_err(sql)?;
        if version != 0 && version != GALLERY_STORE_SCHEMA {
            return Err(Error::Version(version));
        }
        if app_id != 0 && app_id != APPLICATION_ID {
            return Err(invalid("database belongs to another application; original preserved"));
        }
        if version == 0 {
            let tables: u32 =
                db.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'", [], |row| row.get(0)).map_err(sql)?;
            if tables != 0 {
                return Err(invalid("unversioned nonempty database; original preserved"));
            }
        }
        db.pragma_update(None, "journal_mode", "DELETE").map_err(sql)?;
        db.pragma_update(None, "synchronous", "EXTRA").map_err(sql)?;
        db.pragma_update(None, "foreign_keys", "ON").map_err(sql)?;
        db.pragma_update(None, "trusted_schema", "OFF").map_err(sql)?;
        let journal: String = db.pragma_query_value(None, "journal_mode", |row| row.get(0)).map_err(sql)?;
        let synchronous: u32 = db.pragma_query_value(None, "synchronous", |row| row.get(0)).map_err(sql)?;
        let foreign_keys: u32 = db.pragma_query_value(None, "foreign_keys", |row| row.get(0)).map_err(sql)?;
        if !journal.eq_ignore_ascii_case("delete") || synchronous != 3 || foreign_keys != 1 {
            return Err(io("durable SQLite settings were not enabled"));
        }
        let page_size: u32 = db.pragma_query_value(None, "page_size", |row| row.get(0)).map_err(sql)?;
        if page_size == 0 {
            return Err(invalid("zero SQLite page size"));
        }
        let pages = options.quota_bytes / u64::from(page_size);
        db.pragma_update(None, "max_page_count", pages as i64).map_err(sql)?;
        let actual: u32 = db.pragma_query_value(None, "max_page_count", |row| row.get(0)).map_err(sql)?;
        if u64::from(actual) > pages {
            return Err(Error::Full);
        }
        if version == 0 {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
            tx.execute_batch(
                "CREATE TABLE entries(id TEXT PRIMARY KEY, manifest BLOB NOT NULL, manifest_hash TEXT NOT NULL, info BLOB NOT NULL, info_hash TEXT NOT NULL);
                CREATE TABLE objects(hash TEXT PRIMARY KEY, bytes INTEGER NOT NULL);
                CREATE TABLE chunks(hash TEXT NOT NULL REFERENCES objects(hash), ordinal INTEGER NOT NULL, data BLOB NOT NULL, PRIMARY KEY(hash,ordinal));
                CREATE TABLE refs(entry TEXT NOT NULL REFERENCES entries(id), hash TEXT NOT NULL REFERENCES objects(hash), PRIMARY KEY(entry,hash));
                CREATE TABLE draft_preferences(id TEXT PRIMARY KEY, info BLOB NOT NULL, info_hash TEXT NOT NULL);",
            )
            .map_err(sql)?;
            tx.pragma_update(None, "application_id", APPLICATION_ID).map_err(sql)?;
            tx.pragma_update(None, "user_version", GALLERY_STORE_SCHEMA).map_err(sql)?;
            tx.commit().map_err(sql)?;
        }
        // Additive schema-1 extension: earlier K4 development stores had no notes.
        // Missing note rows mean the viewer never explicitly downloaded notes.
        db.execute_batch("CREATE TABLE IF NOT EXISTS gallery_notes(entry TEXT PRIMARY KEY REFERENCES entries(id), body BLOB NOT NULL, hash TEXT NOT NULL)")
            .map_err(sql)?;
        Ok(Self { db, options })
    }

    pub fn usage(&self) -> Result<Usage> {
        let pages: u32 = self.db.pragma_query_value(None, "page_count", |row| row.get(0)).map_err(sql)?;
        let size: u32 = self.db.pragma_query_value(None, "page_size", |row| row.get(0)).map_err(sql)?;
        Ok(Usage {
            stored_bytes: u64::from(pages).checked_mul(u64::from(size)).ok_or_else(|| invalid("database size overflow"))?,
            quota_bytes: self.options.quota_bytes,
        })
    }

    pub fn list(&self, include_trash: bool) -> Result<Vec<GalleryInfo>> {
        let mut query = self.db.prepare("SELECT substr(id,1,33),substr(info,1,16385),substr(info_hash,1,65) FROM entries ORDER BY id LIMIT ?1").map_err(sql)?;
        let rows = query
            .query_map([(MAX_GALLERY_ENTRIES + 1) as i64], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?, row.get::<_, String>(2)?)))
            .map_err(sql)?;
        let mut result = Vec::new();
        let mut count = 0usize;
        for row in rows {
            count += 1;
            if count > MAX_GALLERY_ENTRIES {
                return Err(invalid("gallery entry limit exceeded"));
            }
            let (id, bytes, hash) = row.map_err(sql)?;
            if bytes.len() > 16 * 1024 || digest(&bytes) != hash {
                return Err(Error::Corrupt("gallery metadata checksum".into()));
            }
            let mut info: GalleryInfo = serde_json::from_slice(&bytes).map_err(invalid)?;
            validate_info(&info)?;
            if info.id.as_str() != id {
                return Err(Error::Corrupt("gallery entry identity".into()));
            }
            if info.kind == GalleryKind::Project && info.source_projects.is_empty() {
                // Older K4 metadata predates the source cache. Read only its
                // bounded manifest/notes, never artwork blobs, to restore it.
                info = existing_entry(&self.db, info.id)?.ok_or(Error::NotFound)?.info;
            }
            if include_trash || !info.trashed {
                result.push(info);
            }
        }
        result.sort_by(|left, right| {
            chrono::DateTime::parse_from_rfc3339(&right.updated_at)
                .ok()
                .cmp(&chrono::DateTime::parse_from_rfc3339(&left.updated_at).ok())
                .then(left.id.cmp(&right.id))
        });
        Ok(result)
    }

    pub fn insert(&mut self, bundle: &GalleryBundle) -> Result<GalleryInfo> {
        self.insert_observed(bundle, |_| {})
    }

    /// Explicitly save or update a user-authored solo project in the permanent Saved gallery.
    /// Immutable Game entries can never be overwritten through this path.
    pub fn save_project(&mut self, bundle: &GalleryBundle, explicit_title: Option<&str>) -> Result<GalleryInfo> {
        if !matches!(bundle.manifest.content, GalleryContent::Project { .. }) {
            return Err(invalid("only drawings/projects can be updated via save_project"));
        }
        if let Some(title) = explicit_title {
            photocraft_party::validate_label(title, 96, false).map_err(invalid)?;
        }
        validate_native(bundle)?;
        let now = timestamp();
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        let (entry, replaced) = if let Some(existing) = existing_entry(&tx, bundle.manifest.id)? {
            if existing.info.kind != GalleryKind::Project {
                return Err(Error::Conflict);
            }
            let mut manifest = bundle.manifest.clone();
            manifest.notes.clear();
            manifest.created_at = existing.manifest.created_at.clone();
            manifest.updated_at = now.clone();
            manifest.title = explicit_title.map(str::to_owned).unwrap_or_else(|| existing.info.title.clone());
            manifest.fps = existing.info.fps;
            manifest.looping = existing.info.looping;
            let mut info = summary(&manifest)?;
            info.favorite = existing.info.favorite;
            info.trashed = false;
            info.imported_from = existing.info.imported_from;
            info.source_projects = source_projects(&manifest, info.imported_from);
            (StoredEntry { manifest, info, notes: Vec::new() }, true)
        } else {
            check_entry_capacity(&tx, 1)?;
            let mut manifest = bundle.manifest.clone();
            manifest.notes.clear();
            if let Some(title) = explicit_title {
                manifest.title = title.into();
            }
            manifest.updated_at = now.clone();
            let info = summary(&manifest)?;
            (StoredEntry { manifest, info, notes: Vec::new() }, false)
        };
        entry.validate()?;
        for (hash, bytes) in &bundle.objects {
            store_object(&tx, AssetRef { hash: *hash, bytes: bytes.len() as u64 }, bytes)?;
        }
        if replaced {
            let manifest_bytes = json_bytes(&entry.manifest, MAX_GALLERY_MANIFEST_BYTES)?;
            let info_bytes = json_bytes(&entry.info, 16 * 1024)?;
            tx.execute(
                "UPDATE entries SET manifest=?1,manifest_hash=?2,info=?3,info_hash=?4 WHERE id=?5",
                params![manifest_bytes, digest(&manifest_bytes), info_bytes, digest(&info_bytes), entry.manifest.id.as_str()],
            )
            .map_err(sql)?;
            tx.execute("DELETE FROM refs WHERE entry=?1", [entry.manifest.id.as_str()]).map_err(sql)?;
            for asset in entry.manifest.required_assets().map_err(invalid)? {
                let bytes: Option<i64> = db_object_bytes(&tx, asset.hash)?;
                if bytes != Some(asset.bytes as i64) {
                    return Err(invalid("missing or mismatched gallery dependency"));
                }
                tx.execute("INSERT INTO refs(entry,hash) VALUES(?1,?2)", params![entry.manifest.id.as_str(), asset.hash.as_str()]).map_err(sql)?;
            }
            gc_unreferenced_objects(&tx)?;
        } else {
            store_entry(&tx, &entry)?;
        }
        tx.commit().map_err(sql)?;
        if replaced {
            let _ = self.db.execute_batch("VACUUM;");
        }
        Ok(entry.info)
    }

    /// Permanently delete a single gallery entry that has already been moved to Trash.
    /// Preserves objects still referenced by other entries (e.g. remixes or siblings).
    pub fn delete_entry(&mut self, id: ProjectId) -> Result<bool> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        let Some(existing) = existing_entry(&tx, id)? else {
            return Ok(false);
        };
        if !existing.info.trashed {
            return Err(invalid("move this item to Trash before deleting it permanently"));
        }
        tx.execute("DELETE FROM gallery_notes WHERE entry=?1", [id.as_str()]).map_err(sql)?;
        tx.execute("DELETE FROM refs WHERE entry=?1", [id.as_str()]).map_err(sql)?;
        let removed = tx.execute("DELETE FROM entries WHERE id=?1", [id.as_str()]).map_err(sql)? > 0;
        if removed {
            gc_unreferenced_objects(&tx)?;
        }
        tx.commit().map_err(sql)?;
        if removed {
            let _ = self.db.execute_batch("VACUUM;");
        }
        Ok(removed)
    }

    /// Permanently delete all currently trashed entries and reclaim unreferenced objects.
    pub fn delete_trashed_entries(&mut self) -> Result<Vec<ProjectId>> {
        let trashed: Vec<ProjectId> = self.list(true)?.into_iter().filter(|info| info.trashed).map(|info| info.id).collect();
        if trashed.is_empty() {
            return Ok(Vec::new());
        }
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        let mut deleted = Vec::new();
        for id in trashed {
            let Some(existing) = existing_entry(&tx, id)? else { continue };
            if !existing.info.trashed {
                continue;
            }
            tx.execute("DELETE FROM gallery_notes WHERE entry=?1", [id.as_str()]).map_err(sql)?;
            tx.execute("DELETE FROM refs WHERE entry=?1", [id.as_str()]).map_err(sql)?;
            if tx.execute("DELETE FROM entries WHERE id=?1", [id.as_str()]).map_err(sql)? > 0 {
                deleted.push(id);
            }
        }
        if !deleted.is_empty() {
            gc_unreferenced_objects(&tx)?;
        }
        tx.commit().map_err(sql)?;
        if !deleted.is_empty() {
            let _ = self.db.execute_batch("VACUUM;");
        }
        Ok(deleted)
    }

    /// Remove any draft preference row once a draft has been permanently deleted or promoted.
    pub fn remove_draft_preference(&mut self, id: ProjectId) -> Result<()> {
        self.remove_draft_preferences(&[id])
    }

    pub fn remove_draft_preferences(&mut self, ids: &[ProjectId]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        for id in ids {
            tx.execute("DELETE FROM draft_preferences WHERE id=?1", [id.as_str()]).map_err(sql)?;
        }
        tx.commit().map_err(sql)
    }

    /// Assignment draft IDs whose submitted artwork is already durably preserved inside
    /// a non-trashed Completed game in `gallery.sqlite3`.
    pub fn covered_game_projects(&self) -> Result<BTreeSet<ProjectId>> {
        let mut covered = BTreeSet::new();
        for info in self.list(false)? {
            if info.kind == GalleryKind::Project {
                continue;
            }
            let Some(entry) = existing_entry(&self.db, info.id)? else { continue };
            if let GalleryContent::Game { room } = entry.manifest.content {
                for reveal in room.reveal {
                    if let photocraft_party::room::RevealedEntry::Artwork { assignment, .. } = reveal {
                        covered.insert(assignment.project);
                    }
                }
            }
        }
        Ok(covered)
    }

    /// Final `.pcraft` checkpoint hashes for non-trashed Saved projects in `gallery.sqlite3`,
    /// read from metadata only (never decoding artwork blobs).
    pub fn saved_project_checkpoints(&self) -> Result<BTreeMap<ProjectId, AssetHash>> {
        let mut result = BTreeMap::new();
        for info in self.list(false)? {
            if info.kind != GalleryKind::Project {
                continue;
            }
            let Some(entry) = existing_entry(&self.db, info.id)? else { continue };
            if let GalleryContent::Project { checkpoint, .. } = entry.manifest.content {
                result.insert(info.id, checkpoint.hash);
            }
        }
        Ok(result)
    }

    pub fn insert_observed(&mut self, bundle: &GalleryBundle, mut observe: impl FnMut(GalleryStage)) -> Result<GalleryInfo> {
        validate_native(bundle)?;
        let mut entry = StoredEntry { manifest: bundle.manifest.clone(), info: summary(&bundle.manifest)?, notes: bundle.manifest.notes.clone() };
        entry.manifest.notes.clear();
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        if let Some(existing) = existing_entry(&tx, entry.manifest.id)? {
            if json_bytes(&existing.manifest, MAX_GALLERY_MANIFEST_BYTES)? != json_bytes(&entry.manifest, MAX_GALLERY_MANIFEST_BYTES)? {
                return Err(Error::Conflict);
            }
            // A successful retry never hides a corrupted original.
            load_bundle(&tx, entry.manifest.id)?.validate().map_err(invalid)?;
            return Ok(existing.info);
        }
        check_entry_capacity(&tx, 1)?;
        for (hash, bytes) in &bundle.objects {
            store_object(&tx, AssetRef { hash: *hash, bytes: bytes.len() as u64 }, bytes)?;
        }
        store_entry(&tx, &entry)?;
        observe(GalleryStage::BeforeCommit);
        tx.commit().map_err(sql)?;
        observe(GalleryStage::AfterCommit);
        Ok(entry.info)
    }

    pub fn load(&self, id: ProjectId) -> Result<GalleryBundle> {
        load_bundle(&self.db, id)
    }

    pub fn draft_preferences(&self) -> Result<Vec<DraftPreference>> {
        read_preferences(&self.db)
    }

    pub fn update_draft(&mut self, id: ProjectId, title: Option<&str>, favorite: Option<bool>, trashed: Option<bool>) -> Result<()> {
        if let Some(title) = title {
            photocraft_party::validate_label(title, 96, false).map_err(invalid)?;
        }
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        let preferences = read_preferences(&tx)?;
        let mut preference = preferences.iter().find(|preference| preference.id == id).cloned().unwrap_or(DraftPreference {
            id,
            title: None,
            favorite: false,
            trashed: false,
            fps: None,
            looping: None,
        });
        if preferences.len() >= MAX_GALLERY_ENTRIES && !preferences.iter().any(|preference| preference.id == id) {
            return Err(invalid("draft preference limit reached"));
        }
        if let Some(title) = title {
            preference.title = Some(title.into());
        }
        if let Some(favorite) = favorite {
            preference.favorite = favorite;
        }
        if let Some(trashed) = trashed {
            preference.trashed = trashed;
        }
        write_preference(&tx, &preference)?;
        tx.commit().map_err(sql)
    }

    pub fn update(&mut self, id: ProjectId, title: Option<&str>, favorite: Option<bool>, trashed: Option<bool>) -> Result<GalleryInfo> {
        if let Some(title) = title {
            photocraft_party::validate_label(title, 96, false).map_err(invalid)?;
        }
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        let mut entry = existing_entry(&tx, id)?.ok_or(Error::NotFound)?;
        if let Some(title) = title {
            entry.info.title = title.into();
        }
        if let Some(favorite) = favorite {
            entry.info.favorite = favorite;
        }
        if let Some(trashed) = trashed {
            entry.info.trashed = trashed;
        }
        entry.info.updated_at = timestamp();
        entry.validate()?;
        let bytes = json_bytes(&entry.info, 16 * 1024)?;
        tx.execute("UPDATE entries SET info=?1,info_hash=?2 WHERE id=?3", params![bytes, digest(&bytes), id.as_str()]).map_err(sql)?;
        tx.commit().map_err(sql)?;
        Ok(entry.info)
    }

    pub fn set_draft_playback(&mut self, id: ProjectId, fps: u8, looping: bool) -> Result<()> {
        if !(1..=24).contains(&fps) {
            return Err(invalid("frame rate must be 1 through 24"));
        }
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        let preferences = read_preferences(&tx)?;
        let mut preference = preferences.iter().find(|preference| preference.id == id).cloned().unwrap_or(DraftPreference {
            id,
            title: None,
            favorite: false,
            trashed: false,
            fps: None,
            looping: None,
        });
        if preferences.len() >= MAX_GALLERY_ENTRIES && !preferences.iter().any(|preference| preference.id == id) {
            return Err(invalid("draft preference limit reached"));
        }
        preference.fps = Some(fps);
        preference.looping = Some(looping);
        write_preference(&tx, &preference)?;
        tx.commit().map_err(sql)
    }

    pub fn set_playback(&mut self, id: ProjectId, fps: u8, looping: bool) -> Result<GalleryInfo> {
        if !(1..=24).contains(&fps) {
            return Err(invalid("frame rate must be 1 through 24"));
        }
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        let mut entry = existing_entry(&tx, id)?.ok_or(Error::NotFound)?;
        entry.info.fps = fps;
        entry.info.looping = looping;
        entry.info.updated_at = timestamp();
        entry.validate()?;
        let bytes = json_bytes(&entry.info, 16 * 1024)?;
        tx.execute("UPDATE entries SET info=?1,info_hash=?2 WHERE id=?3", params![bytes, digest(&bytes), id.as_str()]).map_err(sql)?;
        tx.commit().map_err(sql)?;
        Ok(entry.info)
    }

    pub fn set_notes(&mut self, id: ProjectId, notes: &[(AssignmentId, String)]) -> Result<()> {
        if notes.len() > 36 {
            return Err(invalid("too many handoff notes"));
        }
        for (_, note) in notes {
            photocraft_party::validate_label(note, photocraft_party::MAX_NOTE_BYTES, true).map_err(invalid)?;
        }
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        let mut entry = existing_entry(&tx, id)?.ok_or(Error::NotFound)?;
        entry.notes = notes.to_vec();
        entry.validate()?;
        write_notes(&tx, id, &entry.notes)?;
        tx.commit().map_err(sql)
    }

    pub fn export_archive(&mut self, id: ProjectId, path: impl AsRef<Path>) -> Result<()> {
        let tx = self.db.transaction().map_err(sql)?;
        let entry = existing_entry(&tx, id)?.ok_or(Error::NotFound)?;
        // Validate all replay dependencies before producing a portable result.
        load_bundle(&tx, id)?;
        atomic_write(path.as_ref(), |file| write_database(&tx, file, false, &[entry], &[]))?;
        tx.commit().map_err(sql)
    }

    pub fn import_archive(&mut self, path: impl AsRef<Path>) -> Result<GalleryInfo> {
        self.import_archive_checked(path, |_| Ok(()))
    }

    /// Validate the exact objects held in the importing transaction, avoiding a
    /// check-then-reopen race when an engine additionally verifies rendered pixels.
    pub fn import_archive_checked(&mut self, path: impl AsRef<Path>, mut validate: impl FnMut(&GalleryBundle) -> Result<()>) -> Result<GalleryInfo> {
        let file = open_archive(path.as_ref())?;
        let mut reader = ArchiveReader::new(file);
        let mut entries = reader.entries(false)?;
        reader.preferences(false)?;
        let mut entry = entries.pop().ok_or_else(|| invalid("empty archive"))?;
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        if let Some(existing) = existing_entry(&tx, entry.manifest.id)? {
            if json_bytes(&existing.manifest, MAX_GALLERY_MANIFEST_BYTES)? == json_bytes(&entry.manifest, MAX_GALLERY_MANIFEST_BYTES)? {
                reader.objects(&[entry], |asset, bytes| verify_existing_object(&tx, asset, bytes))?;
                validate(&load_bundle(&tx, existing.manifest.id)?)?;
                return Ok(existing.info);
            }
            if let Some(imported) = find_imported_copy(&tx, &entry)? {
                reader.objects(std::slice::from_ref(&entry), |asset, bytes| verify_existing_object(&tx, asset, bytes))?;
                validate(&load_bundle(&tx, imported.id)?)?;
                return Ok(imported);
            }
            let original = entry.info.imported_from.unwrap_or(entry.manifest.id);
            entry.manifest.id = fresh_project_id()?;
            entry.info.id = entry.manifest.id;
            entry.info.imported_from = Some(original);
            entry.hydrate_sources();
        }
        check_entry_capacity(&tx, 1)?;
        reader.objects(std::slice::from_ref(&entry), |asset, bytes| store_object(&tx, asset, bytes))?;
        store_entry(&tx, &entry)?;
        validate(&load_bundle(&tx, entry.manifest.id)?)?;
        tx.commit().map_err(sql)?;
        Ok(entry.info)
    }

    /// Includes originals, shared blobs, names, favorites, and recoverable trash.
    /// Credentials and local writer identities are deliberately outside this store.
    pub fn backup(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let tx = self.db.transaction().map_err(sql)?;
        let entries = all_entries(&tx)?;
        let preferences = read_preferences(&tx)?;
        for entry in &entries {
            load_bundle(&tx, entry.manifest.id)?;
        }
        atomic_write(path.as_ref(), |file| write_database(&tx, file, true, &entries, &preferences))?;
        tx.commit().map_err(sql)
    }

    /// Restore only to an empty gallery. One transaction prevents partial imports.
    pub fn restore_backup(&mut self, path: impl AsRef<Path>) -> Result<Vec<GalleryInfo>> {
        self.restore_backup_checked(path, |_| Ok(()))
    }

    pub fn restore_backup_checked(&mut self, path: impl AsRef<Path>, mut validate: impl FnMut(&GalleryBundle) -> Result<()>) -> Result<Vec<GalleryInfo>> {
        let file = open_archive(path.as_ref())?;
        let mut reader = ArchiveReader::new(file);
        let entries = reader.entries(true)?;
        let preferences = reader.preferences(true)?;
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        let count: u32 = tx.query_row("SELECT count(*) FROM entries", [], |row| row.get(0)).map_err(sql)?;
        if count != 0 || !read_preferences(&tx)?.is_empty() {
            return Err(invalid("restore requires a fresh empty gallery; existing originals were not replaced"));
        }
        reader.objects(&entries, |asset, bytes| store_object(&tx, asset, bytes))?;
        for entry in &entries {
            store_entry(&tx, entry)?;
        }
        for preference in preferences {
            write_preference(&tx, &preference)?;
        }
        for entry in &entries {
            validate(&load_bundle(&tx, entry.manifest.id)?)?;
        }
        tx.commit().map_err(sql)?;
        Ok(entries.into_iter().map(|entry| entry.info).collect())
    }
}

fn validate_info(info: &GalleryInfo) -> Result<()> {
    photocraft_party::validate_label(&info.title, 96, false).map_err(invalid)?;
    valid_timestamp(&info.created_at)?;
    valid_timestamp(&info.updated_at)?;
    if info.authors.len() > 3
        || info.bytes > MAX_GALLERY_BYTES
        || info.imported_from == Some(info.id)
        || !(1..=24).contains(&info.fps)
        || info.source_projects.len() > 8
        || info.source_projects.windows(2).any(|pair| pair.first() >= pair.get(1))
        || (info.kind != GalleryKind::Project && !info.source_projects.is_empty())
        || (info.kind == GalleryKind::Project
            && !info.source_projects.is_empty()
            && (!info.source_projects.contains(&info.id) || info.imported_from.is_some_and(|source| !info.source_projects.contains(&source))))
    {
        return Err(invalid("metadata bounds"));
    }
    let mut authors = BTreeSet::new();
    for author in &info.authors {
        author.validate().map_err(invalid)?;
        if !authors.insert(author.id) {
            return Err(invalid("duplicate author"));
        }
    }
    Ok(())
}

fn validate_preference(preference: &DraftPreference) -> Result<()> {
    if let Some(title) = &preference.title {
        photocraft_party::validate_label(title, 96, false).map_err(invalid)?;
    }
    if preference.fps.is_some_and(|fps| !(1..=24).contains(&fps)) {
        return Err(invalid("draft frame rate must be 1 through 24"));
    }
    Ok(())
}

fn read_preferences(db: &Connection) -> Result<Vec<DraftPreference>> {
    let mut query =
        db.prepare("SELECT substr(id,1,33),substr(info,1,1025),substr(info_hash,1,65) FROM draft_preferences ORDER BY id LIMIT 1025").map_err(sql)?;
    let rows = query.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?, row.get::<_, String>(2)?))).map_err(sql)?;
    let mut preferences = Vec::new();
    for row in rows {
        let (id, bytes, hash) = row.map_err(sql)?;
        if preferences.len() >= MAX_GALLERY_ENTRIES || bytes.len() > 1024 || digest(&bytes) != hash {
            return Err(Error::Corrupt("draft preference bounds or checksum".into()));
        }
        let preference: DraftPreference = serde_json::from_slice(&bytes).map_err(invalid)?;
        validate_preference(&preference)?;
        if preference.id.as_str() != id {
            return Err(Error::Corrupt("draft preference identity".into()));
        }
        preferences.push(preference);
    }
    Ok(preferences)
}

fn write_preference(db: &Connection, preference: &DraftPreference) -> Result<()> {
    validate_preference(preference)?;
    let bytes = json_bytes(preference, 1024)?;
    db.execute(
        "INSERT INTO draft_preferences(id,info,info_hash) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET info=excluded.info,info_hash=excluded.info_hash",
        params![preference.id.as_str(), bytes, digest(&bytes)],
    )
    .map_err(sql)?;
    Ok(())
}

fn read_notes(db: &Connection, id: ProjectId) -> Result<Vec<(AssignmentId, String)>> {
    let record: Option<(Vec<u8>, String)> = db
        .query_row("SELECT substr(body,1,131073),substr(hash,1,65) FROM gallery_notes WHERE entry=?1", [id.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()
        .map_err(sql)?;
    let Some((bytes, hash)) = record else { return Ok(Vec::new()) };
    if bytes.len() > 128 * 1024 || digest(&bytes) != hash {
        return Err(Error::Corrupt("gallery notes checksum".into()));
    }
    serde_json::from_slice(&bytes).map_err(invalid)
}

fn write_notes(db: &Connection, id: ProjectId, notes: &[(AssignmentId, String)]) -> Result<()> {
    let bytes = json_bytes(&notes, 128 * 1024)?;
    db.execute(
        "INSERT INTO gallery_notes(entry,body,hash) VALUES(?1,?2,?3) ON CONFLICT(entry) DO UPDATE SET body=excluded.body,hash=excluded.hash",
        params![id.as_str(), bytes, digest(&bytes)],
    )
    .map_err(sql)?;
    Ok(())
}

/// Reopen every unique checkpoint before acknowledging an archive. Hash validity
/// alone does not prove compressed native documents are readable within budgets.
fn validate_native(bundle: &GalleryBundle) -> Result<()> {
    bundle.validate().map_err(invalid)?;
    let mut native = BTreeSet::new();
    let mut replays = Vec::new();
    let mut game_mode = None;
    let mut expected_final_sizes = BTreeMap::new();
    match &bundle.manifest.content {
        GalleryContent::Project { checkpoint, replay, .. } => {
            native.insert(checkpoint.hash);
            if let Some(replay) = replay {
                replays.push((**replay).clone());
            }
        }
        GalleryContent::Game { room } => {
            game_mode = Some(room.mode);
            for entry in &room.reveal {
                if let photocraft_party::room::RevealedEntry::Artwork { checkpoint, replay, .. } = entry {
                    let extent = entry.resolved_extent(room.mode).map_err(invalid)?;
                    let size = photocraft_doc::Size { width: extent.width, height: extent.height };
                    if expected_final_sizes.insert(checkpoint.asset.hash, size).is_some_and(|prev| prev != size) {
                        return Err(invalid("conflicting game checkpoint dimensions"));
                    }
                    native.insert(checkpoint.asset.hash);
                    native.insert(replay.baseline.asset.hash);
                    if let Some(manifest) = replay.manifest {
                        let bytes = bundle.object(manifest).map_err(invalid)?;
                        let artifact: photocraft_party::replay_artifact::ReplayArtifact = serde_json::from_slice(bytes).map_err(invalid)?;
                        replays.push(artifact);
                    }
                }
            }
        }
    }
    let mut lookup = bundle.objects.iter().map(|(hash, bytes)| (*hash, bytes.as_slice())).collect::<BTreeMap<_, _>>();
    for replay in &replays {
        for step in &replay.steps {
            native.insert(step.checkpoint.asset.hash);
        }
        for pack in &replay.packs {
            let bytes = bundle.object(*pack).map_err(invalid)?;
            for (asset, bytes) in photocraft_party::replay_artifact::unpack_checkpoints(bytes).map_err(invalid)? {
                if let Some(existing) = lookup.insert(asset.hash, bytes)
                    && existing != bytes
                {
                    return Err(invalid("conflicting replay checkpoint"));
                }
            }
        }
    }
    let options = photocraft_format::LoadOptions {
        max_manifest_bytes: 8 * 1024 * 1024,
        max_blob_bytes: 64 * 1024 * 1024,
        max_total_bytes: 512 * 1024 * 1024,
        max_canvas_side: 8192,
        max_canvas_pixels: 16_777_216,
        ..photocraft_format::LoadOptions::default()
    };
    for hash in native {
        let bytes = lookup.get(&hash).ok_or_else(|| invalid("replay checkpoint is unavailable"))?;
        let document = photocraft_format::load_from_bytes_with(bytes, &options).map_err(|error| invalid(format!("unreadable native checkpoint: {error}")))?;
        if document.layer_count() > 4096 {
            return Err(invalid("native checkpoint exceeds 4096 layers"));
        }
        if let Some(mode) = game_mode {
            mode.validate_extent(photocraft_party::ImageExtent { width: document.size.width, height: document.size.height })
                .map_err(|_| invalid("game checkpoint dimensions do not match the assigned drawing"))?;
        }
        if expected_final_sizes.get(&hash).is_some_and(|expected| document.size != *expected) {
            return Err(invalid("game checkpoint dimensions do not match the assigned drawing"));
        }
    }
    Ok(())
}

fn existing_entry(db: &Connection, id: ProjectId) -> Result<Option<StoredEntry>> {
    let row: Option<(Vec<u8>, String, Vec<u8>, String)> = db
        .query_row(
            "SELECT substr(manifest,1,?2),substr(manifest_hash,1,65),substr(info,1,16385),substr(info_hash,1,65) FROM entries WHERE id=?1",
            params![id.as_str(), (MAX_GALLERY_MANIFEST_BYTES + 1) as i64],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(sql)?;
    let Some((manifest, manifest_hash, info, info_hash)) = row else { return Ok(None) };
    if manifest.len() > MAX_GALLERY_MANIFEST_BYTES || info.len() > 16 * 1024 || digest(&manifest) != manifest_hash || digest(&info) != info_hash {
        return Err(Error::Corrupt("gallery manifest or metadata checksum".into()));
    }
    let mut entry = StoredEntry {
        manifest: serde_json::from_slice(&manifest).map_err(invalid)?,
        info: serde_json::from_slice(&info).map_err(invalid)?,
        notes: read_notes(db, id)?,
    };
    entry.validate()?;
    entry.hydrate_sources();
    if entry.manifest.id != id {
        return Err(Error::Corrupt("gallery entry identity".into()));
    }
    Ok(Some(entry))
}

fn all_entries(db: &Connection) -> Result<Vec<StoredEntry>> {
    let mut query = db.prepare("SELECT substr(id,1,33) FROM entries ORDER BY id LIMIT ?1").map_err(sql)?;
    let rows = query.query_map([(MAX_GALLERY_ENTRIES + 1) as i64], |row| row.get::<_, String>(0)).map_err(sql)?;
    let mut result = Vec::new();
    let mut bytes = 0usize;
    for row in rows {
        if result.len() >= MAX_GALLERY_ENTRIES {
            return Err(invalid("gallery entry limit"));
        }
        let id = ProjectId::new(&row.map_err(sql)?).map_err(invalid)?;
        let entry = existing_entry(db, id)?.ok_or(Error::NotFound)?;
        bytes = bytes.saturating_add(json_bytes(&entry, MAX_METADATA_BYTES)?.len());
        // Metadata alone cannot consume the full archive's memory budget.
        if bytes > MAX_GALLERY_BYTES as usize {
            return Err(invalid("backup metadata exceeds 512 MiB; export projects individually"));
        }
        result.push(entry);
    }
    Ok(result)
}

fn find_imported_copy(db: &Connection, source: &StoredEntry) -> Result<Option<GalleryInfo>> {
    let canonical = json_bytes(&source.manifest, MAX_GALLERY_MANIFEST_BYTES)?;
    let origin = source.info.imported_from.unwrap_or(source.manifest.id);
    let mut query = db.prepare("SELECT substr(id,1,33),substr(info,1,16385),substr(info_hash,1,65) FROM entries ORDER BY id LIMIT 1025").map_err(sql)?;
    let rows = query.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?, row.get::<_, String>(2)?))).map_err(sql)?;
    for (index, row) in rows.enumerate() {
        if index >= MAX_GALLERY_ENTRIES {
            return Err(invalid("gallery entry limit"));
        }
        let (id, bytes, hash) = row.map_err(sql)?;
        if bytes.len() > 16 * 1024 || digest(&bytes) != hash {
            return Err(Error::Corrupt("gallery metadata checksum".into()));
        }
        let info: GalleryInfo = serde_json::from_slice(&bytes).map_err(invalid)?;
        validate_info(&info)?;
        if info.id.as_str() != id {
            return Err(Error::Corrupt("gallery entry identity".into()));
        }
        if info.imported_from == Some(origin) {
            let mut candidate = existing_entry(db, info.id)?.ok_or(Error::NotFound)?;
            candidate.manifest.id = source.manifest.id;
            if json_bytes(&candidate.manifest, MAX_GALLERY_MANIFEST_BYTES)? == canonical {
                return Ok(Some(candidate.info));
            }
        }
    }
    Ok(None)
}

fn check_entry_capacity(db: &Connection, additional: usize) -> Result<()> {
    let count: u32 = db.query_row("SELECT count(*) FROM entries", [], |row| row.get(0)).map_err(sql)?;
    if u64::from(count).saturating_add(additional as u64) > MAX_GALLERY_ENTRIES as u64 {
        return Err(invalid("gallery entry limit reached; originals remain retained"));
    }
    Ok(())
}

fn store_entry(db: &Connection, entry: &StoredEntry) -> Result<()> {
    entry.validate()?;
    let manifest = json_bytes(&entry.manifest, MAX_GALLERY_MANIFEST_BYTES)?;
    let info = json_bytes(&entry.info, 16 * 1024)?;
    db.execute(
        "INSERT INTO entries(id,manifest,manifest_hash,info,info_hash) VALUES(?1,?2,?3,?4,?5)",
        params![entry.manifest.id.as_str(), manifest, digest(&manifest), info, digest(&info)],
    )
    .map_err(sql)?;
    for asset in entry.manifest.required_assets().map_err(invalid)? {
        let bytes: Option<i64> = db.query_row("SELECT bytes FROM objects WHERE hash=?1", [asset.hash.as_str()], |row| row.get(0)).optional().map_err(sql)?;
        if bytes != Some(asset.bytes as i64) {
            return Err(invalid("missing or mismatched gallery dependency"));
        }
        db.execute("INSERT INTO refs(entry,hash) VALUES(?1,?2)", params![entry.manifest.id.as_str(), asset.hash.as_str()]).map_err(sql)?;
    }
    write_notes(db, entry.manifest.id, &entry.notes)?;
    Ok(())
}

fn store_object(db: &Connection, asset: AssetRef, bytes: &[u8]) -> Result<()> {
    asset.validate().map_err(invalid)?;
    if bytes.len() as u64 != asset.bytes || digest(bytes) != asset.hash.as_str() {
        return Err(invalid("object checksum or length"));
    }
    let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM objects WHERE hash=?1)", [asset.hash.as_str()], |row| row.get(0)).map_err(sql)?;
    if exists {
        return verify_existing_object(db, asset, bytes);
    }
    let count: u32 = db.query_row("SELECT count(*) FROM objects", [], |row| row.get(0)).map_err(sql)?;
    if u64::from(count) >= MAX_STORED_OBJECTS as u64 {
        return Err(invalid("gallery object limit reached; originals remain retained"));
    }
    db.execute("INSERT INTO objects(hash,bytes) VALUES(?1,?2)", params![asset.hash.as_str(), asset.bytes as i64]).map_err(sql)?;
    let mut query = db.prepare("INSERT INTO chunks(hash,ordinal,data) VALUES(?1,?2,?3)").map_err(sql)?;
    for (index, chunk) in bytes.chunks(CHUNK_BYTES).enumerate() {
        query.execute(params![asset.hash.as_str(), index as i64, chunk]).map_err(sql)?;
    }
    Ok(())
}

fn db_object_bytes(db: &Connection, hash: AssetHash) -> Result<Option<i64>> {
    db.query_row("SELECT bytes FROM objects WHERE hash=?1", [hash.as_str()], |row| row.get(0)).optional().map_err(sql)
}

fn gc_unreferenced_objects(db: &Connection) -> Result<()> {
    db.execute_batch(
        "DELETE FROM chunks WHERE hash IN (
            SELECT hash FROM objects WHERE hash NOT IN (SELECT hash FROM refs)
         );
         DELETE FROM objects WHERE hash NOT IN (SELECT hash FROM refs);",
    )
    .map_err(sql)
}

fn verify_existing_object(db: &Connection, asset: AssetRef, bytes: &[u8]) -> Result<()> {
    if read_object(db, asset)? != bytes {
        return Err(Error::Corrupt("existing gallery object differs from its checksum".into()));
    }
    Ok(())
}

fn stream_object(db: &Connection, asset: AssetRef, mut consume: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
    asset.validate().map_err(invalid)?;
    let actual: Option<i64> = db.query_row("SELECT bytes FROM objects WHERE hash=?1", [asset.hash.as_str()], |row| row.get(0)).optional().map_err(sql)?;
    if actual != Some(asset.bytes as i64) {
        return Err(Error::Corrupt("missing gallery object or invalid length".into()));
    }
    let mut query = db.prepare("SELECT ordinal,substr(data,1,65537) FROM chunks WHERE hash=?1 ORDER BY ordinal LIMIT 1025").map_err(sql)?;
    let rows = query.query_map([asset.hash.as_str()], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))).map_err(sql)?;
    let mut total = 0u64;
    let mut checksum = Sha256::new();
    for (count, row) in rows.enumerate() {
        let (ordinal, bytes) = row.map_err(sql)?;
        if ordinal != count as i64 || bytes.is_empty() || bytes.len() > CHUNK_BYTES || total.saturating_add(bytes.len() as u64) > asset.bytes {
            return Err(Error::Corrupt("gallery object chunks are missing or invalid".into()));
        }
        total += bytes.len() as u64;
        checksum.update(&bytes);
        consume(&bytes)?;
    }
    if total != asset.bytes || format!("{:x}", checksum.finalize()) != asset.hash.as_str() {
        return Err(Error::Corrupt("gallery object checksum".into()));
    }
    Ok(())
}

fn read_object(db: &Connection, asset: AssetRef) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    stream_object(db, asset, |chunk| {
        bytes.try_reserve(chunk.len()).map_err(|_| invalid("not enough memory to open gallery object"))?;
        bytes.extend_from_slice(chunk);
        Ok(())
    })?;
    Ok(bytes)
}

fn load_bundle(db: &Connection, id: ProjectId) -> Result<GalleryBundle> {
    let entry = existing_entry(db, id)?.ok_or(Error::NotFound)?;
    let mut objects = BTreeMap::new();
    let mut total = 0u64;
    for asset in entry.manifest.required_assets().map_err(invalid)? {
        total = total.saturating_add(asset.bytes);
        if total > MAX_GALLERY_BYTES {
            return Err(invalid("gallery bundle exceeds memory limit"));
        }
        objects.insert(asset.hash, read_object(db, asset)?);
    }
    let mut manifest = entry.manifest;
    manifest.title = entry.info.title;
    manifest.updated_at = entry.info.updated_at;
    manifest.fps = entry.info.fps;
    manifest.looping = entry.info.looping;
    manifest.notes = entry.notes;
    let bundle = GalleryBundle { manifest, objects };
    validate_native(&bundle)?;
    Ok(bundle)
}

fn required_objects(entries: &[StoredEntry]) -> Result<BTreeMap<AssetHash, AssetRef>> {
    let mut objects = BTreeMap::new();
    let mut total = 0u64;
    for entry in entries {
        entry.validate()?;
        for asset in entry.manifest.required_assets().map_err(invalid)? {
            match objects.insert(asset.hash, asset) {
                Some(previous) if previous != asset => return Err(invalid("conflicting object length")),
                Some(_) => {}
                None => total = total.checked_add(asset.bytes).ok_or_else(|| invalid("archive size overflow"))?,
            }
            if objects.len() > MAX_STORED_OBJECTS || total > MAX_ARCHIVE_BYTES {
                return Err(invalid("archive exceeds object or byte limit"));
            }
        }
    }
    Ok(objects)
}

fn write_header(writer: &mut impl Write, backup: bool, entries: &[StoredEntry], preferences: &[DraftPreference]) -> Result<()> {
    if entries.len() > MAX_GALLERY_ENTRIES || (!backup && entries.len() != 1) {
        return Err(invalid("archive entry count"));
    }
    writer.write_all(if backup { BACKUP_MAGIC } else { ARCHIVE_MAGIC }).map_err(io)?;
    writer.write_all(&(entries.len() as u32).to_le_bytes()).map_err(io)?;
    for entry in entries {
        entry.validate()?;
        let bytes = json_bytes(entry, MAX_METADATA_BYTES)?;
        writer.write_all(&(bytes.len() as u64).to_le_bytes()).map_err(io)?;
        writer.write_all(&Sha256::digest(&bytes)).map_err(io)?;
        writer.write_all(&bytes).map_err(io)?;
    }
    if preferences.len() > MAX_GALLERY_ENTRIES || (!backup && !preferences.is_empty()) {
        return Err(invalid("archive preferences count"));
    }
    let preferences = json_bytes(&preferences, 1024 * 1024)?;
    writer.write_all(&(preferences.len() as u64).to_le_bytes()).map_err(io)?;
    writer.write_all(&Sha256::digest(&preferences)).map_err(io)?;
    writer.write_all(&preferences).map_err(io)?;
    Ok(())
}

fn write_database(db: &Connection, writer: &mut impl Write, backup: bool, entries: &[StoredEntry], preferences: &[DraftPreference]) -> Result<()> {
    write_header(writer, backup, entries, preferences)?;
    let objects = required_objects(entries)?;
    writer.write_all(&(objects.len() as u32).to_le_bytes()).map_err(io)?;
    for asset in objects.values() {
        writer.write_all(asset.hash.as_str().as_bytes()).map_err(io)?;
        writer.write_all(&asset.bytes.to_le_bytes()).map_err(io)?;
        stream_object(db, *asset, |bytes| writer.write_all(bytes).map_err(io))?;
    }
    writer.write_all(FOOTER).map_err(io)
}

struct ArchiveReader<R> {
    reader: R,
    read: u64,
}
impl<R: Read> ArchiveReader<R> {
    fn new(reader: R) -> Self {
        Self { reader, read: 0 }
    }
    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut bytes = [0; N];
        self.exact(&mut bytes)?;
        Ok(bytes)
    }
    fn exact(&mut self, bytes: &mut [u8]) -> Result<()> {
        self.read = self.read.checked_add(bytes.len() as u64).filter(|value| *value <= MAX_ARCHIVE_BYTES).ok_or_else(|| invalid("archive byte limit"))?;
        self.reader.read_exact(bytes).map_err(|error| invalid(format!("truncated or unreadable archive: {error}")))
    }
    fn body(&mut self, length: u64, limit: u64) -> Result<Vec<u8>> {
        if length == 0 || length > limit {
            return Err(invalid("archive record length"));
        }
        let length = usize::try_from(length).map_err(invalid)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).map_err(|_| invalid("not enough memory to read archive record"))?;
        bytes.resize(length, 0);
        self.exact(&mut bytes)?;
        Ok(bytes)
    }
    fn entries(&mut self, backup: bool) -> Result<Vec<StoredEntry>> {
        let magic = self.take::<8>()?;
        if &magic != if backup { BACKUP_MAGIC } else { ARCHIVE_MAGIC } {
            return Err(invalid("unrecognized archive format/version"));
        }
        let count = u32::from_le_bytes(self.take::<4>()?) as usize;
        if count > MAX_GALLERY_ENTRIES || (!backup && count != 1) {
            return Err(invalid("archive entry count"));
        }
        let mut entries = Vec::new();
        let mut ids = BTreeSet::new();
        let mut total = 0usize;
        for _ in 0..count {
            let length = u64::from_le_bytes(self.take::<8>()?);
            let checksum = self.take::<32>()?;
            let bytes = self.body(length, MAX_METADATA_BYTES as u64)?;
            total = total.saturating_add(bytes.len());
            if total > MAX_GALLERY_BYTES as usize {
                return Err(invalid("archive metadata exceeds 512 MiB"));
            }
            if Sha256::digest(&bytes).as_slice() != checksum {
                return Err(invalid("archive manifest checksum"));
            }
            let mut entry: StoredEntry = serde_json::from_slice(&bytes).map_err(invalid)?;
            entry.validate()?;
            entry.hydrate_sources();
            if !ids.insert(entry.manifest.id) {
                return Err(invalid("duplicate archive identity"));
            }
            entries.push(entry);
        }
        Ok(entries)
    }
    fn objects(&mut self, entries: &[StoredEntry], mut consume: impl FnMut(AssetRef, &[u8]) -> Result<()>) -> Result<()> {
        let mut required = required_objects(entries)?;
        let count = u32::from_le_bytes(self.take::<4>()?) as usize;
        if count != required.len() {
            return Err(invalid("archive object count/closure"));
        }
        for _ in 0..count {
            let hash = self.take::<64>()?;
            let hash = AssetHash::new(std::str::from_utf8(&hash).map_err(invalid)?).map_err(invalid)?;
            let length = u64::from_le_bytes(self.take::<8>()?);
            let asset = required.remove(&hash).ok_or_else(|| invalid("unknown or repeated archive object"))?;
            if length != asset.bytes {
                return Err(invalid("archive object length"));
            }
            let bytes = self.body(length, photocraft_party::MAX_ASSET_BYTES)?;
            if digest(&bytes) != hash.as_str() {
                return Err(invalid("archive object checksum"));
            }
            consume(asset, &bytes)?;
        }
        if !required.is_empty() || &self.take::<8>()? != FOOTER {
            return Err(invalid("missing archive dependencies or footer"));
        }
        let mut trailing = [0];
        if self.reader.read(&mut trailing).map_err(io)? != 0 {
            return Err(invalid("trailing archive bytes"));
        }
        Ok(())
    }

    fn preferences(&mut self, backup: bool) -> Result<Vec<DraftPreference>> {
        let length = u64::from_le_bytes(self.take::<8>()?);
        let checksum = self.take::<32>()?;
        let bytes = self.body(length, 1024 * 1024)?;
        if Sha256::digest(&bytes).as_slice() != checksum {
            return Err(invalid("archive preferences checksum"));
        }
        let preferences: Vec<DraftPreference> = serde_json::from_slice(&bytes).map_err(invalid)?;
        if preferences.len() > MAX_GALLERY_ENTRIES || (!backup && !preferences.is_empty()) {
            return Err(invalid("archive preference count"));
        }
        let mut ids = BTreeSet::new();
        for preference in &preferences {
            validate_preference(preference)?;
            if !ids.insert(preference.id) {
                return Err(invalid("duplicate draft preference"));
            }
        }
        Ok(preferences)
    }
}

fn open_archive(path: &Path) -> Result<File> {
    let file = File::open(path).map_err(io)?;
    if file.metadata().map_err(io)?.len() > MAX_ARCHIVE_BYTES {
        return Err(invalid("archive exceeds 64 GiB"));
    }
    Ok(file)
}

/// The final path is created atomically and never replaces an existing file.
/// A failed write leaves every original intact and removes only its own temp file.
fn atomic_write(path: &Path, write: impl FnOnce(&mut File) -> Result<()>) -> Result<()> {
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    if path.exists() {
        return Err(io("destination already exists; choose a new filename"));
    }
    let temp = parent.join(format!(".komaphone-{}.tmp", fresh_project_id()?.as_str()));
    let mut file = OpenOptions::new().create_new(true).write(true).open(&temp).map_err(io)?;
    let result = (|| {
        write(&mut file)?;
        file.sync_all().map_err(io)?;
        drop(file);
        // Hard-link publication is atomic and fails if the target already exists.
        std::fs::hard_link(&temp, path).map_err(io)?;
        Ok(())
    })();
    let cleanup = std::fs::remove_file(&temp);
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(io(format!("export was created, but temporary-file cleanup failed: {error}"))),
        (Ok(()), Ok(())) => Ok(()),
    }
}

pub fn encode_archive(bundle: &GalleryBundle) -> Result<Vec<u8>> {
    validate_native(bundle)?;
    let entry = StoredEntry { manifest: bundle.manifest.clone(), info: summary(&bundle.manifest)?, notes: bundle.manifest.notes.clone() };
    let mut writer = BoundedBytes { bytes: Vec::new(), limit: MAX_GALLERY_BYTES as usize + MAX_METADATA_BYTES + 2 * 1024 * 1024 };
    write_bundle(&mut writer, bundle, entry)?;
    Ok(writer.bytes)
}

fn write_bundle(writer: &mut impl Write, bundle: &GalleryBundle, entry: StoredEntry) -> Result<()> {
    write_header(writer, false, &[entry], &[])?;
    writer.write_all(&(bundle.objects.len() as u32).to_le_bytes()).map_err(io)?;
    for (hash, bytes) in &bundle.objects {
        writer.write_all(hash.as_str().as_bytes()).map_err(io)?;
        writer.write_all(&(bytes.len() as u64).to_le_bytes()).map_err(io)?;
        writer.write_all(bytes).map_err(io)?;
    }
    writer.write_all(FOOTER).map_err(io)
}

pub fn decode_archive(bytes: &[u8]) -> Result<GalleryBundle> {
    read_bundle(Cursor::new(bytes))
}

fn read_bundle(reader: impl Read) -> Result<GalleryBundle> {
    let mut reader = ArchiveReader::new(reader);
    let mut entries = reader.entries(false)?;
    reader.preferences(false)?;
    let entry = entries.pop().ok_or_else(|| invalid("empty archive"))?;
    let mut objects = BTreeMap::new();
    reader.objects(std::slice::from_ref(&entry), |asset, bytes| {
        objects.insert(asset.hash, bytes.to_vec());
        Ok(())
    })?;
    let mut manifest = entry.manifest;
    manifest.title = entry.info.title;
    manifest.updated_at = entry.info.updated_at;
    manifest.fps = entry.info.fps;
    manifest.looping = entry.info.looping;
    manifest.notes = entry.notes;
    let bundle = GalleryBundle { manifest, objects };
    validate_native(&bundle)?;
    Ok(bundle)
}

pub fn write_archive(bundle: &GalleryBundle, path: impl AsRef<Path>) -> Result<()> {
    validate_native(bundle)?;
    let entry = StoredEntry { manifest: bundle.manifest.clone(), info: summary(&bundle.manifest)?, notes: bundle.manifest.notes.clone() };
    atomic_write(path.as_ref(), |file| write_bundle(file, bundle, entry))
}

pub fn read_archive(path: impl AsRef<Path>) -> Result<GalleryBundle> {
    read_bundle(open_archive(path.as_ref())?)
}
