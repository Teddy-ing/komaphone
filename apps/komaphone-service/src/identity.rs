//! Stable profiles, independent device writers and a sealed verification boundary.
//!
//! Google evidence enters only through the signature-validating verifier. The
//! local verification adapter remains compiled exclusively in unit tests.
use crate::store::read_u64;
use crate::{Error, Result, Store};
use photocraft_party::{DeviceId, Error as DomainError, Participant, ParticipantId, RequestId};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::path::{Path, PathBuf};

pub mod google;
pub mod hosted;
pub mod wire;

pub const IDENTITY_SCHEMA: u32 = 1;
pub const LOGIN_LIFETIME_MS: u64 = 5 * 60 * 1000;
pub const SESSION_LIFETIME_MS: u64 = 24 * 60 * 60 * 1000;
const MAX_PENDING_LOGINS: u64 = 1024;
const MAX_SESSIONS: u64 = 4096;
const MAX_DEVICES_PER_PROFILE: u64 = 32;
const GOOGLE_ISSUER: &str = "https://accounts.google.com";

/// A profile is the preserved service participant ID; a device is a writer,
/// never a replacement account/profile ID. Display labels play no role here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthContext {
    pub profile: ParticipantId,
    pub device: DeviceId,
}

// No Debug or Serialize: the nonce and session capability must not enter logs.
pub struct LoginChallenge {
    pub id: RequestId,
    pub device: DeviceId,
    pub nonce: String,
    pub expires_at_ms: u64,
}

/// Opaque evidence from the trusted verifier, not deserializable caller claims.
/// Its fields can only be constructed inside this trusted identity module.
pub struct VerifiedLogin {
    transaction: RequestId,
    nonce_hash: String,
    issuer: String,
    subject: String,
    expires_at_ms: u64,
}

pub struct IssuedSession {
    pub context: AuthContext,
    pub expires_at_ms: u64,
    token: String,
    device_credential: String,
}
impl IssuedSession {
    pub fn token(&self) -> &str {
        &self.token
    }
    /// Store through the platform-protected credential adapter, separately from
    /// expiring sessions. Existing writer reuse needs this possession proof.
    pub fn device_credential(&self) -> &str {
        &self.device_credential
    }
}

fn denied() -> Error {
    DomainError::Unauthorized.into()
}
fn sql_time(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::Invalid("identity clock range"))
}
fn secret() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| Error::Storage)?;
    if bytes.iter().all(|value| *value == 0) {
        return Err(Error::Storage);
    }
    Ok(bytes.iter().map(|value| format!("{value:02x}")).collect())
}

pub(crate) fn initialize(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE identity_schema(version INTEGER NOT NULL CHECK(version=1));
         INSERT INTO identity_schema(version) VALUES(1);
         CREATE TABLE identity_profiles(profile TEXT PRIMARY KEY);
         CREATE TABLE identity_accounts(issuer TEXT NOT NULL, subject TEXT NOT NULL, profile TEXT NOT NULL UNIQUE REFERENCES identity_profiles(profile), PRIMARY KEY(issuer,subject));
         CREATE TABLE identity_devices(device TEXT PRIMARY KEY, profile TEXT NOT NULL REFERENCES identity_profiles(profile), verifier TEXT NOT NULL UNIQUE, revoked INTEGER NOT NULL DEFAULT 0 CHECK(revoked IN (0,1)), legacy_disabled INTEGER NOT NULL DEFAULT 0 CHECK(legacy_disabled IN (0,1)));
         CREATE INDEX identity_device_profile ON identity_devices(profile);
         CREATE TABLE identity_logins(id TEXT PRIMARY KEY, device TEXT NOT NULL, nonce_hash TEXT NOT NULL UNIQUE, expires INTEGER NOT NULL);
         CREATE TABLE identity_sessions(verifier TEXT PRIMARY KEY, device TEXT NOT NULL REFERENCES identity_devices(device), expires INTEGER NOT NULL, revoked INTEGER NOT NULL DEFAULT 0 CHECK(revoked IN (0,1)));
         CREATE INDEX identity_session_device ON identity_sessions(device);
         INSERT INTO identity_profiles(profile) SELECT actor FROM identities;
         INSERT INTO identity_devices(device,profile,verifier) SELECT device,actor,verifier FROM identities;",
    )?;
    Ok(())
}

/// A fresh file beside the original is always retained before mutating schema 1.
/// The snapshot uses SQLite backup, verifies it and refuses to replace a file.
/// Random suffixes make retries after an interrupted migration non-destructive.
pub(crate) fn migrate(db: &mut Connection, path: &Path) -> Result<()> {
    let backup = migration_backup_path(path)?;
    crate::backup::copy(db, &backup)?;
    let tx = db.transaction()?;
    initialize(&tx)?;
    tx.pragma_update(None, "user_version", 2)?;
    verify(&tx)?;
    tx.commit()?;
    Ok(())
}
fn migration_backup_path(path: &Path) -> Result<PathBuf> {
    let file = path.file_name().ok_or(Error::Invalid("identity migration database path"))?;
    let mut name = file.to_os_string();
    name.push(format!(".before-identity-v2-{}.sqlite3", secret()?));
    Ok(path.with_file_name(name))
}

pub(crate) fn register_legacy(tx: &Transaction<'_>, participant: &Participant) -> Result<()> {
    // Existing legacy actor/device IDs remain exactly the same. Collision with
    // an already bound new device is rejected instead of reassigned.
    let verifier: String = tx.query_row("SELECT verifier FROM identities WHERE actor=?1", [participant.id.as_str()], |r| r.get(0))?;
    let existing: Option<(String, String, String)> = tx
        .query_row("SELECT profile,device,verifier FROM identity_devices WHERE device=?1 OR verifier=?2", params![participant.device.as_str(), verifier], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()?;
    if let Some((profile, device, existing_verifier)) = existing {
        return if profile == participant.id.as_str() && device == participant.device.as_str() && existing_verifier == verifier {
            Ok(())
        } else {
            Err(denied())
        };
    }
    tx.execute("INSERT OR IGNORE INTO identity_profiles(profile) VALUES(?1)", [participant.id.as_str()])?;
    tx.execute("INSERT INTO identity_devices(device,profile,verifier) SELECT device,actor,verifier FROM identities WHERE actor=?1", [participant.id.as_str()])?;
    tx.execute("INSERT INTO hosted_profiles(profile,label) VALUES(?1,?2)", params![participant.id.as_str(), participant.display_name])?;
    Ok(())
}

pub(crate) fn verify(db: &Connection) -> Result<()> {
    let count: u64 = db.query_row("SELECT count(*) FROM identity_schema WHERE version=?1", [IDENTITY_SCHEMA], |r| read_u64(r, 0))?;
    let all: u64 = db.query_row("SELECT count(*) FROM identity_schema", [], |r| read_u64(r, 0))?;
    if count != 1 || all != 1 {
        return Err(Error::Schema);
    }
    let orphan: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM identities i LEFT JOIN identity_devices d ON i.device=d.device WHERE d.device IS NULL OR d.profile!=i.actor OR d.verifier!=i.verifier)",
        [],
        |r| r.get(0),
    )?;
    let fk: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)", [], |r| r.get(0))?;
    if orphan || fk {
        return Err(Error::Storage);
    }
    Ok(())
}

impl Store {
    /// Checked context for shared-canvas authority. Legacy capabilities remain
    /// accepted only for the existing development/private-network service path.
    pub fn identity_authenticate(&self, token: &str, now_ms: u64) -> Result<AuthContext> {
        photocraft_party::protocol::validate_secret(token).map_err(|_| denied())?;
        let now = sql_time(now_ms)?;
        let verifier = crate::store::digest(token.as_bytes());
        let session: Option<(String, String, i64, bool, bool)> = self.db.query_row(
            "SELECT d.profile,d.device,s.expires,s.revoked,d.revoked FROM identity_sessions s JOIN identity_devices d ON s.device=d.device WHERE s.verifier=?1",
            [&verifier], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
        ).optional()?;
        if let Some((profile, device, expires, session_revoked, device_revoked)) = session {
            if expires <= now || session_revoked || device_revoked {
                return Err(Error::Session);
            }
            hosted::check_admission(&self.db, &profile).map_err(|error| match error {
                Error::Domain(DomainError::Unauthorized) => Error::Session,
                other => other,
            })?;
            return Ok(AuthContext { profile: ParticipantId::new(&profile)?, device: DeviceId::new(&device)? });
        }
        if self.options.hosted {
            return Err(Error::Session);
        }
        let legacy: Option<(String, String, bool)> = self
            .db
            .query_row(
                "SELECT d.profile,d.device,(d.revoked OR d.legacy_disabled) FROM identities i JOIN identity_devices d ON d.device=i.device AND d.profile=i.actor WHERE i.verifier=?1",
                [&verifier],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let (profile, device, revoked) = legacy.ok_or_else(denied)?;
        if revoked {
            return Err(denied());
        }
        Ok(AuthContext { profile: ParticipantId::new(&profile)?, device: DeviceId::new(&device)? })
    }

    /// Begin a bounded, one-use verifier transaction. This method is not exposed
    /// through the legacy RPC protocol. Browser state/PKCE remain client concerns.
    pub fn identity_begin_login(&mut self, device: DeviceId, now_ms: u64) -> Result<LoginChallenge> {
        let expiry = now_ms.checked_add(LOGIN_LIFETIME_MS).ok_or(Error::Invalid("identity clock range"))?;
        let now = sql_time(now_ms)?;
        let expires = sql_time(expiry)?;
        let nonce = secret()?;
        let random = secret()?;
        let id = RequestId::new(random.get(..32).ok_or(Error::Storage)?)?;
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM identity_logins WHERE expires<=?1", [now])?;
        let count: u64 = tx.query_row("SELECT count(*) FROM identity_logins", [], |r| read_u64(r, 0))?;
        if count >= MAX_PENDING_LOGINS {
            return Err(DomainError::Limit("pending identity transactions").into());
        }
        tx.execute(
            "INSERT INTO identity_logins(id,device,nonce_hash,expires) VALUES(?1,?2,?3,?4)",
            params![id.as_str(), device.as_str(), crate::store::digest(nonce.as_bytes()), expires],
        )?;
        tx.commit()?;
        Ok(LoginChallenge { id, device, nonce, expires_at_ms: expiry })
    }

    /// Link with proof of the old profile, or sign in to an already linked one.
    /// Unknown Google accounts are never automatically admitted. A different
    /// legacy profile cannot be merged merely because the account label matches.
    /// Reusing an existing writer additionally requires its durable device proof.
    pub fn identity_finish_login(&mut self, verified: VerifiedLogin, device_proof: Option<&str>, now_ms: u64) -> Result<IssuedSession> {
        self.identity_finish_admitted_login(verified, device_proof, None, "Komaphone player", now_ms)
    }

    pub(crate) fn identity_finish_admitted_login(
        &mut self,
        verified: VerifiedLogin,
        device_proof: Option<&str>,
        admission: Option<&str>,
        label: &str,
        now_ms: u64,
    ) -> Result<IssuedSession> {
        let now = sql_time(now_ms)?;
        if verified.issuer != GOOGLE_ISSUER
            || verified.subject.is_empty()
            || verified.subject.len() > 255
            || !verified.subject.bytes().all(|value| value.is_ascii_graphic())
            || verified.expires_at_ms <= now_ms
        {
            return Err(denied());
        }
        let expires_at_ms = now_ms.checked_add(SESSION_LIFETIME_MS).ok_or(Error::Invalid("identity clock range"))?;
        let expires = sql_time(expires_at_ms)?;
        let token = secret()?;
        let tx = self.db.transaction()?;
        let login: Option<(String, String, i64)> = tx
            .query_row("SELECT device,nonce_hash,expires FROM identity_logins WHERE id=?1", [verified.transaction.as_str()], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?;
        let (device, nonce_hash, login_expiry) = login.ok_or_else(denied)?;
        if nonce_hash != verified.nonce_hash || login_expiry <= now {
            return Err(denied());
        }
        let device = DeviceId::new(&device)?;
        let prior_profile: Option<String> = tx
            .query_row("SELECT profile FROM identity_accounts WHERE issuer=?1 AND subject=?2", params![verified.issuer, verified.subject], |r| r.get(0))
            .optional()?;
        let profile = match prior_profile {
            Some(profile) => {
                hosted::check_admission(&tx, &profile)?;
                ParticipantId::new(&profile)?
            }
            None => {
                let profile = hosted::link_or_admit(&tx, device, device_proof, admission, label, now)?;
                tx.execute(
                    "INSERT INTO identity_accounts(issuer,subject,profile) VALUES(?1,?2,?3)",
                    params![verified.issuer, verified.subject, profile.as_str()],
                )?;
                profile
            }
        };
        let existing_device: Option<(String, String)> =
            tx.query_row("SELECT profile,verifier FROM identity_devices WHERE device=?1", [device.as_str()], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        let device_credential = if let Some((existing_profile, verifier)) = existing_device {
            let proof = device_proof.ok_or_else(denied)?;
            photocraft_party::protocol::validate_secret(proof).map_err(|_| denied())?;
            if existing_profile != profile.as_str() || crate::store::digest(proof.as_bytes()) != verifier {
                return Err(denied());
            }
            proof.to_owned()
        } else {
            let count: u64 = tx.query_row("SELECT count(*) FROM identity_devices WHERE profile=?1", [profile.as_str()], |r| read_u64(r, 0))?;
            if count >= MAX_DEVICES_PER_PROFILE {
                return Err(DomainError::Limit("profile device writers").into());
            }
            let credential = if let Some(proof) = device_proof {
                photocraft_party::protocol::validate_secret(proof).map_err(|_| denied())?;
                proof.to_owned()
            } else {
                secret()?
            };
            tx.execute(
                "INSERT INTO identity_devices(device,profile,verifier) VALUES(?1,?2,?3)",
                params![device.as_str(), profile.as_str(), crate::store::digest(credential.as_bytes())],
            )?;
            credential
        };
        // A fresh verified login may reactivate this device, but previously
        // revoked sessions remain revoked and cannot spring back to life.
        tx.execute("UPDATE identity_devices SET revoked=0 WHERE device=?1", [device.as_str()])?;
        tx.execute("DELETE FROM identity_sessions WHERE expires<=?1", [now])?;
        let sessions: u64 = tx.query_row("SELECT count(*) FROM identity_sessions", [], |r| read_u64(r, 0))?;
        if sessions >= MAX_SESSIONS {
            return Err(DomainError::Limit("retained identity sessions").into());
        }
        tx.execute(
            "INSERT INTO identity_sessions(verifier,device,expires) VALUES(?1,?2,?3)",
            params![crate::store::digest(token.as_bytes()), device.as_str(), expires],
        )?;
        tx.execute("DELETE FROM identity_logins WHERE id=?1", [verified.transaction.as_str()])?;
        tx.commit()?;
        Ok(IssuedSession { context: AuthContext { profile, device }, expires_at_ms, token, device_credential })
    }

    pub fn identity_revoke_session(&mut self, token: &str, now_ms: u64) -> Result<()> {
        self.identity_authenticate(token, now_ms)?;
        let changed = self.db.execute("UPDATE identity_sessions SET revoked=1 WHERE verifier=?1", [crate::store::digest(token.as_bytes())])?;
        if changed != 1 {
            return Err(Error::Invalid("legacy capability is not a revocable login session"));
        }
        Ok(())
    }

    pub fn identity_revoke_device(&mut self, token: &str, device: DeviceId, now_ms: u64) -> Result<()> {
        let context = self.identity_authenticate(token, now_ms)?;
        let tx = self.db.transaction()?;
        let changed = tx.execute(
            "UPDATE identity_devices SET revoked=1,legacy_disabled=1 WHERE device=?1 AND profile=?2",
            params![device.as_str(), context.profile.as_str()],
        )?;
        if changed != 1 {
            return Err(denied());
        }
        tx.execute("UPDATE identity_sessions SET revoked=1 WHERE device=?1", [device.as_str()])?;
        tx.commit()?;
        self.world_presence.remove_device(device.as_str());
        Ok(())
    }
}

#[cfg(test)]
mod tests;
