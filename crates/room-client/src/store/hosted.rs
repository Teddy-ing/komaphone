//! Durable account slots. Switching moves exact pending rows in one SQLite commit.
use super::{MAX_STATE_BYTES, Outbox, SCHEMA, SavedState, digest, random_hex, sql, write_state};
use crate::hosted_auth::{LoginOptions, Session};
use crate::{ClientStatus, Error, Result, normalize_endpoint};
use photocraft_party::protocol::validate_secret;
use photocraft_party::{DeviceId, Participant, RequestId};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use std::path::Path;
use std::time::SystemTime;

const MAX_ACCOUNTS: usize = 16;
pub(super) mod relocation;

pub(super) fn initialize(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS account_slots(id TEXT PRIMARY KEY,body BLOB NOT NULL,digest TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS account_outbox(slot TEXT NOT NULL,seq INTEGER NOT NULL,request_id TEXT NOT NULL,
             body BLOB NOT NULL,digest TEXT NOT NULL,secret_key TEXT,rejected TEXT,retained INTEGER NOT NULL,
             PRIMARY KEY(slot,seq),UNIQUE(slot,request_id));
         CREATE TABLE IF NOT EXISTS account_notes(slot TEXT NOT NULL,endpoint TEXT NOT NULL,room TEXT NOT NULL,game TEXT NOT NULL,
             chain INTEGER NOT NULL,round INTEGER NOT NULL,body BLOB NOT NULL,digest TEXT NOT NULL,
             PRIMARY KEY(slot,endpoint,room,game,chain,round));",
    )
    .map_err(sql)
}

pub(super) fn backup_v1(db: &Connection, path: &Path) -> Result<()> {
    let name = path.file_name().and_then(|name| name.to_str()).ok_or_else(|| Error::Storage("Cannot name the pre-sign-in migration backup".into()))?;
    let backup = path.with_file_name(format!("{name}.pre-hosted-v1-{}.sqlite3", random_hex(16)?));
    let destination = backup.to_str().ok_or_else(|| Error::Storage("Migration backup path is not valid Unicode".into()))?;
    db.execute("VACUUM INTO ?1", [destination]).map_err(sql)?;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&backup)
        .and_then(|file| file.sync_all())
        .map_err(|_| Error::Storage("Cannot confirm durable pre-sign-in backup; original database is unchanged".into()))?;
    let restored = Connection::open_with_flags(&backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(sql)?;
    let integrity: String = restored.query_row("PRAGMA quick_check", [], |row| row.get(0)).map_err(sql)?;
    let version: u32 = restored.pragma_query_value(None, "user_version", |row| row.get(0)).map_err(sql)?;
    if integrity != "ok" || version != 1 {
        return Err(Error::Storage("Pre-sign-in backup verification failed; original database is unchanged".into()));
    }
    Ok(())
}

pub(super) fn now_ms() -> Result<u64> {
    let now: chrono::DateTime<chrono::Utc> = SystemTime::now().into();
    u64::try_from(now.timestamp_millis()).map_err(|_| Error::Invalid("Correct the system clock before signing in".into()))
}

pub(super) fn validate_saved(state: &SavedState) -> Result<()> {
    RequestId::new(&state.slot).map_err(|_| Error::Storage("Invalid saved account slot; preserve this database".into()))?;
    RequestId::new(&state.namespace).map_err(|_| Error::Storage("Invalid credential namespace; preserve this database".into()))?;
    if state.status.hosted && state.hosted_device.is_none() {
        return Err(Error::Storage("Hosted account is missing its device identity; preserve this database".into()));
    }
    if let Some(device) = state.hosted_device
        && state.status.profile.as_ref().is_some_and(|profile| profile.device != device)
    {
        return Err(Error::Storage("Hosted account device does not match its saved profile".into()));
    }
    relocation::validate_state(state)?;
    Ok(())
}

fn hosted_key(kind: &str, state: &SavedState, endpoint: &str) -> String {
    format!("hosted/{kind}/{}/{}", state.slot, digest(endpoint.as_bytes()))
}

impl Outbox {
    /// Secret-bearing native handoff for best-effort server revocation. Unlike
    /// credential(), this also reads an expired or staged relocation session.
    pub(crate) fn session_for_revocation(&self) -> Result<Option<(String, String)>> {
        let endpoint = if let Some(plan) = &self.state.relocation {
            Some(plan.to_endpoint.as_str())
        } else if self.state.status.hosted {
            self.state.status.endpoint.as_deref()
        } else {
            None
        };
        let Some(endpoint) = endpoint else { return Ok(None) };
        let token = self.credentials.get(&self.state.namespace, &hosted_key("session", &self.state, endpoint))?;
        token
            .map(|token| {
                validate_secret(&token).map_err(|_| Error::Credentials("Stored session is invalid".into()))?;
                Ok((endpoint.to_owned(), token))
            })
            .transpose()
    }
    pub(super) fn legacy_credential_key(&self, endpoint: &str) -> String {
        if self.state.legacy_unscoped_credential {
            format!("identity/{}", digest(endpoint.as_bytes()))
        } else {
            format!("identity/{}/{}", self.state.slot, digest(endpoint.as_bytes()))
        }
    }
    pub(super) fn hosted_session_current(&self) -> bool {
        self.state.status.signed_in && self.state.session_expires_at_ms > now_ms().unwrap_or(u64::MAX)
    }

    pub(super) fn check_hosted_access(&self) -> Result<()> {
        self.ensure_no_relocation()?;
        if self.state.status.hosted && !self.hosted_session_current() {
            return Err(Error::Rejected {
                code: "session".into(),
                message: "Sign in with Google to reconnect this account; pending artwork is retained".into(),
            });
        }
        Ok(())
    }

    pub(super) fn hosted_credential(&self) -> Result<String> {
        self.check_hosted_access()?;
        let endpoint = self.state.status.endpoint.as_deref().ok_or_else(|| Error::Invalid("Sign in to a service first".into()))?;
        let token = self
            .credentials
            .get(&self.state.namespace, &hosted_key("session", &self.state, endpoint))?
            .ok_or_else(|| Error::Credentials("Saved sign-in is missing; sign in again. Local artwork is retained".into()))?;
        validate_secret(&token).map_err(|_| Error::Credentials("Saved sign-in is invalid; sign in again".into()))?;
        Ok(token)
    }

    pub(super) fn hosted_status(&self) -> ClientStatus {
        let mut status = self.state.status.clone();
        status.accounts = self.accounts.clone();
        status.account_slot = Some(self.state.slot.clone());
        status.relocation = self.state.relocation.clone();
        if self.state.relocation.is_some() || (status.hosted && !self.hosted_session_current()) {
            status.signed_in = false;
            status.connected = false;
            status.paired = false;
            status.polling = false;
            status.room = None;
            status.invitation_id = None;
            status.freeze_pin = None;
            status.note_draft = None;
            status.reveal_position = None;
            status.game_drafts = None;
            status.submission_project = None;
        }
        status
    }

    pub(super) fn refresh_accounts(&mut self) -> Result<()> {
        let mut statement =
            self.db.prepare("SELECT CASE WHEN length(id)=32 THEN id ELSE NULL END,CASE WHEN length(body)<=?1 THEN body ELSE NULL END,CASE WHEN length(digest)=64 THEN digest ELSE NULL END FROM account_slots ORDER BY id LIMIT 17").map_err(sql)?;
        let mut rows = statement.query([MAX_STATE_BYTES as i64]).map_err(sql)?;
        let mut accounts = Vec::new();
        while let Some(row) = rows.next().map_err(sql)? {
            if accounts.len() >= MAX_ACCOUNTS {
                return Err(Error::Storage("Saved account-slot limit exceeded; preserve this database".into()));
            }
            let id: String = row.get(0).map_err(sql)?;
            let body: Vec<u8> = row.get(1).map_err(sql)?;
            let checksum: String = row.get(2).map_err(sql)?;
            let saved = decode_slot(&id, &body, &checksum, &self.state.namespace)?;
            accounts.push(crate::AccountSlot { id, profile: saved.status.profile, endpoint: saved.status.endpoint });
        }
        self.accounts = accounts;
        Ok(())
    }

    pub fn prepare_hosted_login(&mut self, endpoint: &str, label: &str, admission: Option<String>) -> Result<LoginOptions> {
        self.ensure_no_relocation()?;
        let endpoint = normalize_endpoint(endpoint)?;
        photocraft_party::contracts::validate_label(label, 96, false).map_err(|_| Error::Invalid("Choose a display name of at most 96 bytes".into()))?;
        if let Some(value) = &admission {
            validate_secret(value).map_err(|_| Error::Invalid("Admission invitation is invalid".into()))?;
        }
        if self.state.status.endpoint.as_ref().is_some_and(|previous| previous != &endpoint) {
            return Err(Error::Invalid("This saved account belongs to a different service. Choose Use another account to preserve its queued work".into()));
        }
        // Unacknowledged legacy pairing is not ownership proof. Preserve it in its
        // original slot rather than silently sending it through hosted admission.
        if !self.state.status.hosted && self.state.status.profile.is_some() && !self.state.status.paired {
            return Err(Error::Invalid("Finish legacy pairing before linking this profile, or use a separate account slot".into()));
        }
        let device = match self.state.hosted_device.or_else(|| self.state.status.profile.as_ref().map(|profile| profile.device)) {
            Some(device) => device,
            None => DeviceId::new(&random_hex(16)?).map_err(|_| Error::Storage("Cannot allocate a device identity".into()))?,
        };
        let key = hosted_key("proof", &self.state, &endpoint);
        self.register_credential(&key)?;
        let proof = match self.credentials.get(&self.state.namespace, &key)? {
            Some(proof) => proof,
            None => {
                let proof = if self.state.status.profile.is_some() && !self.state.status.hosted { self.credential()? } else { random_hex(32)? };
                self.credentials.set(&self.state.namespace, &key, &proof)?;
                if self.credentials.get(&self.state.namespace, &key)?.as_deref() != Some(proof.as_str()) {
                    return Err(Error::Credentials("Device proof readback failed; sign-in was not started".into()));
                }
                proof
            }
        };
        validate_secret(&proof).map_err(|_| Error::Credentials("Stored device proof is invalid; preserve this account".into()))?;
        let mut candidate = self.state.clone();
        candidate.linking_legacy = candidate.linking_legacy || (!candidate.status.hosted && candidate.status.profile.is_some());
        candidate.hosted_device = Some(device);
        candidate.status.hosted = true;
        candidate.status.signed_in = false;
        candidate.status.signing_in = true;
        candidate.status.connected = false;
        candidate.status.error = None;
        candidate.status.endpoint = Some(endpoint.clone());
        candidate.active = false;
        self.commit_candidate(candidate)?;
        self.refresh_accounts()?;
        Ok(LoginOptions { endpoint, device, device_proof: proof, admission, label: label.into() })
    }

    pub fn install_hosted_session(&mut self, endpoint: &str, session: &Session) -> Result<()> {
        self.ensure_no_relocation()?;
        let endpoint = normalize_endpoint(endpoint)?;
        if !self.state.status.hosted
            || !self.state.status.signing_in
            || self.state.status.endpoint.as_deref() != Some(&endpoint)
            || self.state.hosted_device != Some(session.device)
            || session.expires_at_ms <= now_ms()?
            || self.state.status.profile.as_ref().is_some_and(|profile| profile.id != session.profile || profile.device != session.device)
        {
            return Err(Error::Invalid("Sign-in does not match this saved account. Choose Use another account to keep both accounts separate".into()));
        }
        let proof_key = hosted_key("proof", &self.state, &endpoint);
        let proof = self
            .credentials
            .get(&self.state.namespace, &proof_key)?
            .ok_or_else(|| Error::Credentials("Device proof is missing; preserve this account".into()))?;
        validate_secret(&session.token).map_err(|_| Error::Invalid("Sign-in returned an invalid session".into()))?;
        if session.device_credential != proof {
            return Err(Error::Invalid("Sign-in changed this device's proof; preserve local artwork".into()));
        }
        let profile = Participant { id: session.profile, device: session.device, display_name: session.label.clone() };
        profile.validate().map_err(|_| Error::Invalid("Sign-in returned an invalid profile".into()))?;
        // Never activate this profile under another slot's existing device/queue.
        // Explicitly selecting the old slot preserves its writer identity.
        if self.accounts.iter().any(|slot| {
            slot.id != self.state.slot && slot.endpoint.as_deref() == Some(&endpoint) && slot.profile.as_ref().is_some_and(|saved| saved.id == profile.id)
        }) {
            return Err(Error::Invalid("This Google account already has a saved account slot. Select it and sign in there to preserve its queued work".into()));
        }
        let token_key = hosted_key("session", &self.state, &endpoint);
        self.register_credential(&token_key)?;
        self.credentials.set(&self.state.namespace, &token_key, &session.token)?;
        if self.credentials.get(&self.state.namespace, &token_key)?.as_deref() != Some(session.token.as_str()) {
            return Err(Error::Credentials("Session readback failed; account remains signed out".into()));
        }
        let mut candidate = self.state.clone();
        candidate.status.profile = Some(profile);
        candidate.linking_legacy = false;
        candidate.status.signed_in = true;
        candidate.status.signing_in = false;
        candidate.status.paired = true;
        candidate.status.connected = true;
        candidate.status.error = None;
        candidate.status.notice = Some("Signed in. Queued work will resume for this account".into());
        candidate.active = true;
        candidate.session_expires_at_ms = session.expires_at_ms;
        self.commit_candidate(candidate)?;
        self.refresh_counts()?;
        self.refresh_accounts()
    }

    pub fn cancel_hosted_login(&mut self) -> Result<()> {
        if self.state.relocation.is_some() {
            return self.stop_relocation_sign_in();
        }
        if !self.state.status.hosted {
            return Ok(());
        }
        let mut candidate = self.state.clone();
        if candidate.linking_legacy {
            candidate.status.hosted = false;
            candidate.linking_legacy = false;
        }
        candidate.status.signing_in = false;
        candidate.status.signed_in = false;
        candidate.status.connected = false;
        candidate.active = false;
        self.commit_candidate(candidate)
    }

    pub fn sign_out_local(&mut self) -> Result<()> {
        if self.state.relocation.is_some() {
            return self.stop_relocation_sign_in();
        }
        if !self.state.status.hosted {
            return self.stop_polling();
        }
        // Commit the disabled state before touching the vault: a vault outage must
        // not resurrect authenticated work after restart.
        let mut candidate = self.state.clone();
        candidate.status.signing_in = false;
        candidate.status.signed_in = false;
        candidate.status.connected = false;
        candidate.status.notice = Some("Signed out. Drawings and pending edits remain saved on this device".into());
        candidate.active = false;
        candidate.session_expires_at_ms = 0;
        if self.commit_candidate(candidate.clone()).is_err() {
            // Even a full/read-only disk cannot leave this process uploading after
            // Sign out. Also attempt to remove its protected session so reopening
            // cannot authenticate with the stale persisted state.
            self.state = candidate;
            if let Some(endpoint) = self.state.status.endpoint.as_deref() {
                let _ = self.credentials.remove(&self.state.namespace, &hosted_key("session", &self.state, endpoint));
            }
            return Err(Error::Storage("Sign-out could not be saved. Network work is paused; repair storage before reopening this account".into()));
        }
        if let Some(endpoint) = self.state.status.endpoint.as_deref() {
            self.credentials.remove(&self.state.namespace, &hosted_key("session", &self.state, endpoint))?;
        }
        self.refresh_counts()
    }

    pub fn select_account(&mut self, slot: Option<&str>) -> Result<()> {
        self.ensure_no_relocation()?;
        if slot == Some(self.state.slot.as_str()) {
            return Ok(());
        }
        if let Some(slot) = slot {
            RequestId::new(slot).map_err(|_| Error::Invalid("Invalid saved account slot".into()))?;
        }
        self.refresh_accounts()?;
        if slot.is_none() && self.accounts.len() >= MAX_ACCOUNTS {
            return Err(Error::Invalid("This installation has 16 saved accounts; preserve their work and use an existing account slot".into()));
        }
        let mut candidate = if let Some(slot) = slot {
            let (body, checksum): (Vec<u8>, String) = self
                .db
                .query_row(
                    "SELECT CASE WHEN length(body)<=?2 THEN body ELSE NULL END,CASE WHEN length(digest)=64 THEN digest ELSE NULL END FROM account_slots WHERE id=?1",
                    params![slot, MAX_STATE_BYTES as i64],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(sql)?
                .ok_or_else(|| Error::Invalid("Saved account was not found".into()))?;
            decode_slot(slot, &body, &checksum, &self.state.namespace)?
        } else {
            SavedState {
                schema: SCHEMA,
                namespace: self.state.namespace.clone(),
                status: ClientStatus::default(),
                active: false,
                last_invitation: None,
                frozen: None,
                slot: random_hex(16)?,
                hosted_device: None,
                session_expires_at_ms: 0,
                legacy_unscoped_credential: false,
                linking_legacy: false,
                relocation: None,
                relocation_session: None,
            }
        };
        candidate.active = false;
        candidate.status.connected = false;
        candidate.status.signing_in = false;
        candidate.status.signed_in = false;
        candidate.session_expires_at_ms = 0;
        let mut departing = self.state.clone();
        departing.active = false;
        departing.status.connected = false;
        departing.status.signing_in = false;
        departing.status.signed_in = false;
        departing.session_expires_at_ms = 0;
        let transaction = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        write_state(&transaction, &departing)?;
        transaction.execute("DELETE FROM account_outbox WHERE slot=?1", [&departing.slot]).map_err(sql)?;
        transaction
            .execute("INSERT INTO account_outbox SELECT ?1,seq,request_id,body,digest,secret_key,rejected,retained FROM outbox", [&departing.slot])
            .map_err(sql)?;
        transaction.execute("DELETE FROM outbox", []).map_err(sql)?;
        transaction.execute("INSERT INTO outbox(seq,request_id,body,digest,secret_key,rejected,retained) SELECT seq,request_id,body,digest,secret_key,rejected,retained FROM account_outbox WHERE slot=?1", [&candidate.slot]).map_err(sql)?;
        transaction.execute("DELETE FROM account_outbox WHERE slot=?1", [&candidate.slot]).map_err(sql)?;
        transaction.execute("DELETE FROM account_notes WHERE slot=?1", [&departing.slot]).map_err(sql)?;
        transaction
            .execute("INSERT INTO account_notes SELECT ?1,endpoint,room,game,chain,round,body,digest FROM local_assignment_notes", [&departing.slot])
            .map_err(sql)?;
        transaction.execute("DELETE FROM local_assignment_notes", []).map_err(sql)?;
        transaction
            .execute("INSERT INTO local_assignment_notes SELECT endpoint,room,game,chain,round,body,digest FROM account_notes WHERE slot=?1", [&candidate.slot])
            .map_err(sql)?;
        transaction.execute("DELETE FROM account_notes WHERE slot=?1", [&candidate.slot]).map_err(sql)?;
        write_state(&transaction, &candidate)?;
        transaction.commit().map_err(sql)?;
        self.state = candidate;
        self.rebuild_submission_index()?;
        self.refresh_counts()?;
        self.refresh_accounts()?;
        if departing.status.hosted
            && let Some(endpoint) = &departing.status.endpoint
        {
            // A stale protected token is unusable locally even if vault deletion fails.
            let _ = self.credentials.remove(&departing.namespace, &hosted_key("session", &departing, endpoint));
        }
        Ok(())
    }
}

fn decode_slot(id: &str, body: &[u8], checksum: &str, namespace: &str) -> Result<SavedState> {
    if body.len() > MAX_STATE_BYTES || digest(body) != checksum {
        return Err(Error::Storage("Saved account checksum failed; preserve this database".into()));
    }
    let state: SavedState = serde_json::from_slice(body).map_err(|_| Error::Storage("Invalid saved account; preserve this database".into()))?;
    validate_saved(&state)?;
    if state.schema != SCHEMA || state.slot != id || state.namespace != namespace {
        return Err(Error::Storage("Saved account identity does not match its slot".into()));
    }
    if let Some(endpoint) = &state.status.endpoint {
        normalize_endpoint(endpoint)?;
    }
    if let Some(profile) = &state.status.profile {
        profile.validate().map_err(|_| Error::Storage("Saved account profile is invalid".into()))?;
    }
    Ok(state)
}

#[cfg(test)]
mod tests;
