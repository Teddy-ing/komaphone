//! Recoverable local-installation/profile association. This never changes art
//! IDs, authorship, local history or remote permissions. A bare local OwnerId is
//! not proof of ownership at a service; explicit authorized import is separate.
use super::{CanvasStage, CanvasStore};
use crate::{Error, Result};
use photocraft_party::AssetHash;
use photocraft_scene::{OwnerId, shared::DeviceId};
use rusqlite::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const LINK_SCHEMA: u32 = 1;
const MAX_JOURNAL: u32 = 1024;
const MAX_RECORD: usize = 4096;
const CREDENTIAL_NAMESPACE: &str = "Komaphone.canvas-installation.v1";

/// A protected platform credential adapter. Implementations must atomically
/// store the credential, and must never log or serialize its raw value.
pub trait LinkCredentials {
    fn get(&mut self, namespace: &str, key: &str) -> Result<Option<String>>;
    fn set(&mut self, namespace: &str, key: &str, secret: &str) -> Result<()>;
}

/// Trusted native authentication seam, not an identity supplied by UI/JSON.
/// Implementations prove the current service profile/device credentials and,
/// for hosted linking, verified login; bind the result to this one-use nonce.
/// The local install key alone never authorizes remote ownership or membership.
pub trait LinkAuthority {
    fn authorize(&mut self, local_owner: OwnerId, nonce: &str) -> Result<LinkTarget>;
    /// Recheck native account activation immediately before the SQLite commit.
    /// This must be nonblocking; network verification belongs before migration.
    fn confirm(&mut self, _target: &LinkTarget) -> Result<()> {
        Ok(())
    }
}

/// Service is a digest of the canonical endpoint, not a raw URL that could carry
/// credentials. Profiles and device writers remain independent typed identities.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkTarget {
    pub service: AssetHash,
    pub profile: OwnerId,
    pub device: DeviceId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalBinding {
    pub version: u32,
    pub local_owner: OwnerId,
    pub target: LinkTarget,
    pub revision: u32,
    pub active: bool,
}

#[derive(Clone, Debug)]
pub struct LinkReceipt {
    pub binding: LocalBinding,
    pub backup_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    binding: LocalBinding,
    previous_hash: String,
    backup_file: String,
    created_at: String,
}

fn invalid(value: &str) -> Error {
    Error::Invalid(value.into())
}
fn sql(value: rusqlite::Error) -> Error {
    match &value {
        rusqlite::Error::SqliteFailure(code, _) if code.extended_code & 0xff == 13 => Error::Full,
        _ => Error::Storage(value.to_string()),
    }
}
fn hash(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}
fn random_secret() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| Error::Storage("Cannot allocate installation proof".into()))?;
    if bytes.iter().all(|value| *value == 0) {
        return Err(invalid("Invalid installation randomness; retry"));
    }
    Ok(bytes.iter().map(|value| format!("{value:02x}")).collect())
}
fn exists(db: &Connection) -> Result<bool> {
    db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='canvas_link_schema')", [], |r| r.get(0)).map_err(sql)
}

pub(super) fn verify_existing(db: &Connection, owner: OwnerId) -> Result<()> {
    read_binding(db, owner).map(|_| ())
}
fn read_binding(db: &Connection, owner: OwnerId) -> Result<Option<LocalBinding>> {
    if !exists(db)? {
        let partial: u32 =
            db.query_row("SELECT count(*) FROM sqlite_master WHERE name IN ('canvas_link_identity','canvas_link_journal')", [], |r| r.get(0)).map_err(sql)?;
        return if partial == 0 { Ok(None) } else { Err(invalid("Incomplete local linking schema; preserve the database and its backup")) };
    }
    let versions: Vec<u32> = db
        .prepare("SELECT version FROM canvas_link_schema LIMIT 2")
        .map_err(sql)?
        .query_map([], |r| r.get(0))
        .map_err(sql)?
        .collect::<std::result::Result<_, _>>()
        .map_err(sql)?;
    if versions != [LINK_SCHEMA] {
        return Err(Error::Version(versions.first().copied().unwrap_or(0)));
    }
    let (record, verifier, revision, active): (Vec<u8>, String, u32, bool) = db
        .query_row(
            "SELECT substr(target,1,?1),substr(verifier,1,65),revision,active FROM canvas_link_identity WHERE singleton=1",
            [MAX_RECORD as i64 + 1],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(sql)?;
    if record.len() > MAX_RECORD || revision == 0 || revision > MAX_JOURNAL || AssetHash::new(&verifier).is_err() {
        return Err(invalid("Invalid local installation association"));
    }
    let target: LinkTarget = serde_json::from_slice(&record).map_err(|_| invalid("Invalid pinned profile association"))?;
    let count: u32 = db.query_row("SELECT count(*) FROM canvas_link_journal", [], |r| r.get(0)).map_err(sql)?;
    if count != revision {
        return Err(invalid("Local linking journal has missing or excess revisions"));
    }
    let mut stmt = db.prepare("SELECT revision,substr(body,1,?1),substr(hash,1,65) FROM canvas_link_journal ORDER BY revision").map_err(sql)?;
    let mut rows = stmt.query([MAX_RECORD as i64 + 1]).map_err(sql)?;
    let mut previous = String::new();
    let mut expected = 1u32;
    let mut last = None;
    while let Some(row) = rows.next().map_err(sql)? {
        let actual: u32 = row.get(0).map_err(sql)?;
        let bytes: Vec<u8> = row.get(1).map_err(sql)?;
        let digest: String = row.get(2).map_err(sql)?;
        if actual != expected || bytes.len() > MAX_RECORD || hash(&bytes) != digest {
            return Err(invalid("Local linking journal checksum or ordering failed"));
        }
        let entry: Entry = serde_json::from_slice(&bytes).map_err(|_| invalid("Invalid local linking journal"))?;
        if entry.binding.version != LINK_SCHEMA
            || entry.binding.local_owner != owner
            || entry.binding.target != target
            || entry.binding.revision != actual
            || entry.previous_hash != previous
            || entry.binding.active != (actual % 2 == 1)
            || !valid_backup_name(&entry.backup_file)
            || entry.created_at.len() > 64
            || chrono::DateTime::parse_from_rfc3339(&entry.created_at).is_err()
        {
            return Err(invalid("Local linking journal identity or history mismatch"));
        }
        previous = digest;
        last = Some(entry.binding);
        expected = expected.checked_add(1).ok_or_else(|| invalid("Local linking revision limit"))?;
    }
    let last = last.ok_or_else(|| invalid("Missing local linking history"))?;
    if last.active != active {
        return Err(invalid("Local linking state disagrees with its journal"));
    }
    Ok(Some(last))
}
fn valid_backup_name(value: &str) -> bool {
    value.starts_with("canvas-link-before-") && value.ends_with(".sqlite3") && value.len() < 128 && !value.contains(['/', '\\', ':'])
}

fn possession(db: &Connection, owner: OwnerId, credentials: &mut dyn LinkCredentials) -> Result<String> {
    let verifier: Option<String> =
        if exists(db)? { Some(db.query_row("SELECT verifier FROM canvas_link_identity WHERE singleton=1", [], |r| r.get(0)).map_err(sql)?) } else { None };
    let stored = credentials.get(CREDENTIAL_NAMESPACE, owner.as_str())?;
    let secret = match (verifier.as_ref(), stored) {
        (Some(_), None) => return Err(invalid("The protected installation credential is missing; local drawing is intact, but linking requires recovery")),
        (_, Some(secret)) => secret,
        (None, None) => {
            let secret = random_secret()?;
            credentials.set(CREDENTIAL_NAMESPACE, owner.as_str(), &secret)?;
            if credentials.get(CREDENTIAL_NAMESPACE, owner.as_str())?.as_deref() != Some(secret.as_str()) {
                return Err(invalid("Installation credential was not durably stored"));
            }
            secret
        }
    };
    AssetHash::new(&secret).map_err(|_| invalid("The protected installation credential is invalid"))?;
    let calculated = hash(secret.as_bytes());
    if verifier.as_ref().is_some_and(|expected| expected != &calculated) {
        return Err(invalid("Protected credential does not prove this local installation"));
    }
    Ok(calculated)
}

impl CanvasStore {
    /// Signed-out local drawing remains fully usable; association does not
    /// migrate pixels, replace owner IDs or grant any remote capability.
    pub fn local_binding(&self) -> Result<Option<LocalBinding>> {
        read_binding(&self.db, self.owner())
    }
    pub fn binding_for(&self, target: LinkTarget) -> Result<Option<LocalBinding>> {
        Ok(self.local_binding()?.filter(|binding| binding.active && binding.target == target))
    }
    pub fn link_identity(
        &mut self,
        credentials: &mut dyn LinkCredentials,
        authority: &mut dyn LinkAuthority,
        backup_directory: impl AsRef<Path>,
    ) -> Result<LinkReceipt> {
        self.change_link(credentials, authority, backup_directory.as_ref(), true, |_| {})
    }
    pub fn link_identity_observed(
        &mut self,
        credentials: &mut dyn LinkCredentials,
        authority: &mut dyn LinkAuthority,
        backup_directory: impl AsRef<Path>,
        observe: impl FnMut(CanvasStage),
    ) -> Result<LinkReceipt> {
        self.change_link(credentials, authority, backup_directory.as_ref(), true, observe)
    }
    /// Reverses only this local association, never accepted service artwork or
    /// permissions. The original pinned target stays in the journal; rollback
    /// cannot be used to move another account's queues/artwork to a new account.
    pub fn rollback_link(
        &mut self,
        credentials: &mut dyn LinkCredentials,
        authority: &mut dyn LinkAuthority,
        backup_directory: impl AsRef<Path>,
    ) -> Result<LinkReceipt> {
        self.change_link(credentials, authority, backup_directory.as_ref(), false, |_| {})
    }
    fn change_link(
        &mut self,
        credentials: &mut dyn LinkCredentials,
        authority: &mut dyn LinkAuthority,
        backup_directory: &Path,
        active: bool,
        mut observe: impl FnMut(CanvasStage),
    ) -> Result<LinkReceipt> {
        let owner = self.owner();
        let prior = read_binding(&self.db, owner)?;
        if !active && prior.is_none() {
            return Err(invalid("This local installation has no association to roll back"));
        }
        let target = authority.authorize(owner, &random_secret()?)?;
        if prior.as_ref().is_some_and(|binding| binding.target != target) {
            return Err(invalid("Another account, service or device already owns this association; original artwork and queues are unchanged"));
        }
        if let Some(prior) = &prior
            && prior.active == active
        {
            possession(&self.db, owner, credentials)?;
            return Ok(LinkReceipt { binding: prior.clone(), backup_path: None });
        }
        let revision = prior.as_ref().map_or(1, |binding| binding.revision.saturating_add(1));
        if revision > MAX_JOURNAL {
            return Err(invalid("Local linking journal capacity reached; preserve existing associations"));
        }
        let backup = backup(&self.db, owner, backup_directory)?;
        let backup_file = backup.file_name().and_then(|name| name.to_str()).ok_or_else(|| invalid("Local backup filename"))?.to_owned();
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        // A second writer cannot change the association during an external
        // credential/authentication callback or while the backup is being made.
        if read_binding(&tx, owner)? != prior {
            return Err(Error::Conflict);
        }
        // Serialize initial credential enrollment through the SQLite writer
        // lock. A failed DB commit may leave its protected key, which a retry
        // reuses; it can never replace a previously pinned installation key.
        let verifier = possession(&tx, owner, credentials)?;
        if prior.is_none() {
            tx.execute_batch("CREATE TABLE canvas_link_schema(version INTEGER NOT NULL CHECK(version=1)); INSERT INTO canvas_link_schema VALUES(1);
                CREATE TABLE canvas_link_identity(singleton INTEGER PRIMARY KEY CHECK(singleton=1),target BLOB NOT NULL,verifier TEXT NOT NULL,revision INTEGER NOT NULL,active INTEGER NOT NULL CHECK(active IN (0,1)));
                CREATE TABLE canvas_link_journal(revision INTEGER PRIMARY KEY,body BLOB NOT NULL,hash TEXT NOT NULL);").map_err(sql)?;
        }
        let previous_hash: String = if prior.is_some() {
            tx.query_row("SELECT hash FROM canvas_link_journal ORDER BY revision DESC LIMIT 1", [], |r| r.get(0)).map_err(sql)?
        } else {
            String::new()
        };
        let binding = LocalBinding { version: LINK_SCHEMA, local_owner: owner, target, revision, active };
        let entry =
            Entry { binding: binding.clone(), previous_hash, backup_file, created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true) };
        let body = serde_json::to_vec(&entry).map_err(|_| invalid("Local linking journal encoding"))?;
        if body.len() > MAX_RECORD {
            return Err(invalid("Local linking journal record limit"));
        }
        let target_body = serde_json::to_vec(&target).map_err(|_| invalid("Local target encoding"))?;
        tx.execute(
            "INSERT INTO canvas_link_identity VALUES(1,?1,?2,?3,?4) ON CONFLICT(singleton) DO UPDATE SET revision=excluded.revision,active=excluded.active",
            params![target_body, verifier, revision, active],
        )
        .map_err(sql)?;
        tx.execute("INSERT INTO canvas_link_journal VALUES(?1,?2,?3)", params![revision, body, hash(&body)]).map_err(sql)?;
        verify_existing(&tx, owner)?;
        authority.confirm(&binding.target)?;
        observe(CanvasStage::BeforeCommit);
        tx.commit().map_err(sql)?;
        observe(CanvasStage::AfterCommit);
        Ok(LinkReceipt { binding, backup_path: Some(backup) })
    }
}

struct Stage(PathBuf);
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn backup(db: &Connection, owner: OwnerId, directory: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(directory).map_err(|e| Error::Storage(e.to_string()))?;
    let tag = random_secret()?;
    let destination = directory.join(format!("canvas-link-before-{tag}.sqlite3"));
    let stage = directory.join(format!(".canvas-link-{tag}.tmp"));
    let reserved = std::fs::OpenOptions::new().write(true).create_new(true).open(&stage).map_err(|e| Error::Storage(e.to_string()))?;
    drop(reserved);
    let stage = Stage(stage);
    db.backup("main", &stage.0, None::<fn(rusqlite::backup::Progress)>).map_err(sql)?;
    let copied = Connection::open_with_flags(&stage.0, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(sql)?;
    let check: String = copied.query_row("PRAGMA quick_check", [], |r| r.get(0)).map_err(sql)?;
    let saved_owner: String = copied.query_row("SELECT owner FROM identity WHERE singleton=1", [], |r| r.get(0)).map_err(sql)?;
    if check != "ok" || saved_owner != owner.as_str() {
        return Err(invalid("Local linking backup verification failed"));
    }
    verify_existing(&copied, owner)?;
    drop(copied);
    std::fs::OpenOptions::new().read(true).write(true).open(&stage.0).and_then(|file| file.sync_all()).map_err(|e| Error::Storage(e.to_string()))?;
    std::fs::hard_link(&stage.0, &destination).map_err(|e| Error::Storage(e.to_string()))?;
    Ok(destination)
}
