//! Private admission, profile labels and restore/revocation boundaries.
use super::{AuthContext, denied, secret, sql_time};
use crate::store::read_u64;
use crate::{Error, Result, Store};
use photocraft_party::{DeviceId, Participant, ParticipantId};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::path::Path;

const MAX_ADMISSIONS: u64 = 256;
const MAX_PROFILES: u64 = 4096;
pub const MAX_ADMISSION_LIFETIME_MS: u64 = 7 * 24 * 60 * 60 * 1000;

pub(crate) fn initialize(db: &mut Connection, path: &Path) -> Result<()> {
    let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='hosted_schema')", [], |r| r.get(0))?;
    if exists {
        verify(db)?;
        let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version < 3 {
            db.pragma_update(None, "user_version", 3)?;
        }
        return Ok(());
    }
    let mut backup = path.as_os_str().to_os_string();
    backup.push(format!(".before-hosted-v1-{}.sqlite3", secret()?));
    crate::backup::copy(db, Path::new(&backup))?;
    let tx = db.transaction()?;
    tx.execute_batch(
        "CREATE TABLE hosted_schema(version INTEGER NOT NULL CHECK(version=1));
         INSERT INTO hosted_schema VALUES(1);
         CREATE TABLE hosted_profiles(profile TEXT PRIMARY KEY REFERENCES identity_profiles(profile),label TEXT NOT NULL,revoked INTEGER NOT NULL DEFAULT 0 CHECK(revoked IN (0,1)));
         INSERT INTO hosted_profiles(profile,label) SELECT actor,label FROM identities;
         INSERT OR IGNORE INTO hosted_profiles(profile,label) SELECT profile,'Komaphone player' FROM identity_profiles;
         CREATE TABLE hosted_admissions(verifier TEXT PRIMARY KEY,expires INTEGER NOT NULL,revoked INTEGER NOT NULL DEFAULT 0 CHECK(revoked IN (0,1)),redeemed_profile TEXT REFERENCES identity_profiles(profile));"
    )?;
    verify(&tx)?;
    tx.pragma_update(None, "user_version", 3)?;
    tx.commit()?;
    Ok(())
}
pub(crate) fn verify(db: &Connection) -> Result<()> {
    let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='hosted_schema')", [], |r| r.get(0))?;
    if !exists {
        let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version >= 3 {
            return Err(Error::Schema);
        }
        return Ok(());
    } // Retained K6 backups remain readable/migratable.
    let versions: (u64, u64) = db.query_row("SELECT count(*),coalesce(sum(version=1),0) FROM hosted_schema", [], |r| Ok((read_u64(r, 0)?, read_u64(r, 1)?)))?;
    if versions != (1, 1) {
        return Err(Error::Schema);
    }
    let broken: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)", [], |r| r.get(0))?;
    if broken {
        return Err(Error::Storage);
    }
    let missing: bool =
        db.query_row("SELECT EXISTS(SELECT 1 FROM identity_profiles p LEFT JOIN hosted_profiles h ON p.profile=h.profile WHERE h.profile IS NULL)", [], |r| {
            r.get(0)
        })?;
    if missing {
        return Err(Error::Storage);
    }
    Ok(())
}
pub(crate) fn check_admission(db: &Connection, profile: &str) -> Result<()> {
    let revoked: Option<bool> = db.query_row("SELECT revoked FROM hosted_profiles WHERE profile=?1", [profile], |r| r.get(0)).optional()?;
    if revoked != Some(false) {
        return Err(denied());
    }
    Ok(())
}

pub(crate) fn link_or_admit(
    tx: &Transaction<'_>,
    device: DeviceId,
    proof: Option<&str>,
    admission: Option<&str>,
    label: &str,
    now: i64,
) -> Result<ParticipantId> {
    // Existing ownership is proved by the original capability, never a label.
    if let Ok(legacy) = crate::store::authenticate(tx, proof) {
        if legacy.device != device {
            return Err(denied());
        }
        let revoked: bool = tx.query_row("SELECT (revoked OR legacy_disabled) FROM identity_devices WHERE device=?1", [device.as_str()], |r| r.get(0))?;
        let linked: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM identity_accounts WHERE profile=?1)", [legacy.id.as_str()], |r| r.get(0))?;
        if revoked || linked {
            return Err(denied());
        }
        check_admission(tx, legacy.id.as_str())?;
        tx.execute("INSERT OR IGNORE INTO hosted_profiles(profile,label) VALUES(?1,?2)", params![legacy.id.as_str(), legacy.display_name])?;
        return Ok(legacy.id);
    }
    let admission = admission.ok_or_else(denied)?;
    photocraft_party::protocol::validate_secret(admission).map_err(|_| denied())?;
    let verifier = crate::store::digest(admission.as_bytes());
    let available: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM hosted_admissions WHERE verifier=?1 AND expires>?2 AND revoked=0 AND redeemed_profile IS NULL)",
        params![verifier, now],
        |r| r.get(0),
    )?;
    if !available {
        return Err(denied());
    }
    let count: u64 = tx.query_row("SELECT count(*) FROM identity_profiles", [], |r| read_u64(r, 0))?;
    if count >= MAX_PROFILES {
        return Err(photocraft_party::Error::Limit("service admitted profiles").into());
    }
    let random = secret()?;
    let profile = ParticipantId::new(random.get(..32).ok_or(Error::Storage)?)?;
    Participant { id: profile, device, display_name: label.into() }.validate()?;
    tx.execute("INSERT INTO identity_profiles(profile) VALUES(?1)", [profile.as_str()])?;
    tx.execute("INSERT INTO hosted_profiles(profile,label) VALUES(?1,?2)", params![profile.as_str(), label])?;
    tx.execute("UPDATE hosted_admissions SET redeemed_profile=?1 WHERE verifier=?2", params![profile.as_str(), verifier])?;
    Ok(profile)
}

pub(crate) fn profile_label(db: &Connection, profile: &str) -> Result<String> {
    let label: String = db.query_row("SELECT CAST(substr(CAST(label AS BLOB),1,97) AS TEXT) FROM hosted_profiles WHERE profile=?1", [profile], |r| r.get(0))?;
    photocraft_party::validate_label(&label, photocraft_party::MAX_NAME_BYTES, false).map_err(|_| Error::Storage)?;
    Ok(label)
}

impl Store {
    pub(crate) fn identity_participant(&self, context: AuthContext) -> Result<Participant> {
        Ok(Participant { id: context.profile, device: context.device, display_name: profile_label(&self.db, context.profile.as_str())? })
    }
    /// Administrator-only local method; never exposed as an HTTP operation.
    pub fn identity_create_admission(&mut self, lifetime_ms: u64, now_ms: u64) -> Result<String> {
        if lifetime_ms == 0 || lifetime_ms > MAX_ADMISSION_LIFETIME_MS {
            return Err(Error::Invalid("admission lifetime must be at most seven days"));
        }
        let expires = sql_time(now_ms.checked_add(lifetime_ms).ok_or(Error::Invalid("admission expiry"))?)?;
        let now = sql_time(now_ms)?;
        let token = secret()?;
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM hosted_admissions WHERE expires<=?1", [now])?;
        let count: u64 = tx.query_row("SELECT count(*) FROM hosted_admissions", [], |r| read_u64(r, 0))?;
        if count >= MAX_ADMISSIONS {
            return Err(photocraft_party::Error::Limit("service admissions").into());
        }
        tx.execute("INSERT INTO hosted_admissions(verifier,expires) VALUES(?1,?2)", params![crate::store::digest(token.as_bytes()), expires])?;
        tx.commit()?;
        Ok(token)
    }
    pub fn identity_revoke_profile(&mut self, profile: ParticipantId) -> Result<()> {
        let tx = self.db.transaction()?;
        if tx.execute("UPDATE hosted_profiles SET revoked=1 WHERE profile=?1", [profile.as_str()])? != 1 {
            return Err(denied());
        }
        tx.execute("UPDATE identity_sessions SET revoked=1 WHERE device IN (SELECT device FROM identity_devices WHERE profile=?1)", [profile.as_str()])?;
        tx.execute("UPDATE identity_devices SET revoked=1,legacy_disabled=1 WHERE profile=?1", [profile.as_str()])?;
        tx.commit()?;
        Ok(())
    }
    /// Explicit administrator readmission after comparing a restored profile with
    /// the live revocation record. This cannot reactivate any old session.
    pub fn identity_admit_profile(&mut self, profile: ParticipantId) -> Result<()> {
        if self.db.execute("UPDATE hosted_profiles SET revoked=0 WHERE profile=?1", [profile.as_str()])? != 1 {
            return Err(denied());
        }
        Ok(())
    }
    /// Quarantine every restored admission as well as sessions and invitations.
    /// The administrator explicitly readmits only reconciled active profiles.
    pub fn identity_invalidate_restored_access(&mut self) -> Result<()> {
        let tx = self.db.transaction()?;
        tx.execute("UPDATE identity_sessions SET revoked=1", [])?;
        tx.execute("UPDATE hosted_profiles SET revoked=1", [])?;
        tx.execute("DELETE FROM identity_logins", [])?;
        tx.execute("UPDATE hosted_admissions SET revoked=1", [])?;
        tx.execute("UPDATE invitations SET revoked=1", [])?;
        // Canvas invitation table is part of the independently versioned K6 store.
        tx.execute("UPDATE world_invites SET status=2 WHERE status=0", [])?;
        // An old snapshot must not resurrect abandoned/canceled upload staging.
        // Accepted imported canvases and no-grant idempotency receipts survive.
        tx.execute("DELETE FROM world_imports", [])?;
        tx.commit()?;
        Ok(())
    }
    pub(crate) fn identity_cancel_login(&mut self, transaction: photocraft_party::RequestId, nonce: &str) -> Result<()> {
        photocraft_party::protocol::validate_secret(nonce).map_err(|_| denied())?;
        self.db.execute("DELETE FROM identity_logins WHERE id=?1 AND nonce_hash=?2", params![transaction.as_str(), crate::store::digest(nonce.as_bytes())])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
