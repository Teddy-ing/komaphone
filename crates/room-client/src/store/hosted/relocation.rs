//! Room-side journal for the two-database service-address migration.
use super::{Error, LoginOptions, Outbox, Result, SavedState, Session, hosted_key, normalize_endpoint, now_ms, sql, write_state};
use crate::AccountRelocation;
use crate::store::request_id;
use crate::world_queue::WorldScope;
use crate::world_worker::WorldAccess;
use photocraft_party::protocol::validate_secret;
use photocraft_party::{Participant, RequestId};
use rusqlite::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StagedSession {
    label: String,
    expires_at_ms: u64,
}

pub(super) fn validate_state(state: &SavedState) -> Result<()> {
    if let Some(move_plan) = &state.relocation {
        validate_plan(move_plan)?;
        if state.status.endpoint.as_deref() != Some(&move_plan.from_endpoint)
            || state.status.profile.as_ref().is_none_or(|profile| profile.id != move_plan.profile || profile.device != move_plan.device)
            || state.active
        {
            return Err(Error::Storage("Service relocation journal does not match its paused account; preserve the database".into()));
        }
    } else if state.relocation_session.is_some() {
        return Err(Error::Storage("Staged sign-in has no service relocation journal".into()));
    }
    if let Some(session) = &state.relocation_session {
        photocraft_party::contracts::validate_label(&session.label, 96, false).map_err(|_| Error::Storage("Invalid relocation profile label".into()))?;
    }
    Ok(())
}

fn validate_plan(plan: &AccountRelocation) -> Result<()> {
    if normalize_endpoint(&plan.from_endpoint)? != plan.from_endpoint
        || normalize_endpoint(&plan.to_endpoint)? != plan.to_endpoint
        || plan.from_endpoint == plan.to_endpoint
    {
        return Err(Error::Invalid("Service relocation requires two distinct normalized addresses".into()));
    }
    Ok(())
}

impl Outbox {
    pub(in crate::store) fn ensure_no_relocation(&self) -> Result<()> {
        if self.state.relocation.is_some() {
            return Err(Error::Invalid(
                "Service move is paused until its verified canvas migration finishes. Resume the move before using this account".into(),
            ));
        }
        Ok(())
    }

    /// This explicit operation retains original IDs and exact unsent requests.
    /// It never grants new service access merely because an address was changed.
    pub fn prepare_hosted_relocation(&mut self, endpoint: &str, label: &str) -> Result<LoginOptions> {
        let endpoint = normalize_endpoint(endpoint)?;
        photocraft_party::contracts::validate_label(label, 96, false).map_err(|_| Error::Invalid("Choose a display name of at most 96 bytes".into()))?;
        let profile = self.state.status.profile.clone().ok_or_else(|| Error::Invalid("Only an existing paired account can move services".into()))?;
        if !self.state.status.paired {
            return Err(Error::Invalid("Complete original service pairing before moving this account".into()));
        }
        let source = self.state.status.endpoint.clone().ok_or_else(|| Error::Invalid("This account has no original service address".into()))?;
        let plan = match &self.state.relocation {
            Some(plan) if plan.to_endpoint == endpoint => plan.clone(),
            Some(_) => return Err(Error::Invalid("Resume the pending move to its recorded destination; a second destination would split this writer".into())),
            None => AccountRelocation {
                id: request_id()?,
                from_endpoint: source.clone(),
                to_endpoint: endpoint.clone(),
                profile: profile.id,
                device: profile.device,
            },
        };
        validate_plan(&plan)?;
        if self.accounts.iter().any(|slot| {
            slot.id != self.state.slot && slot.endpoint.as_deref() == Some(&endpoint) && slot.profile.as_ref().is_some_and(|other| other.id == profile.id)
        }) {
            return Err(Error::Invalid("Destination already has a saved account slot; preserve both queues and resolve them before moving".into()));
        }
        let source_key = if self.state.status.hosted { hosted_key("proof", &self.state, &source) } else { self.legacy_credential_key(&source) };
        let proof = self
            .credentials
            .get(&self.state.namespace, &source_key)?
            .ok_or_else(|| Error::Credentials("Original device proof is missing; service move was not started".into()))?;
        validate_secret(&proof).map_err(|_| Error::Credentials("Original device proof is invalid; preserve this account".into()))?;
        let destination_key = hosted_key("proof", &self.state, &endpoint);
        self.register_credential(&destination_key)?;
        self.credentials.set(&self.state.namespace, &destination_key, &proof)?;
        if self.credentials.get(&self.state.namespace, &destination_key)?.as_deref() != Some(proof.as_str()) {
            return Err(Error::Credentials("Destination proof readback failed; service move was not started".into()));
        }
        if self.state.relocation.is_none() {
            backup_before_move(&self.db, plan.id)?;
        }
        let mut candidate = self.state.clone();
        candidate.relocation = Some(plan);
        candidate.relocation_session = None;
        candidate.active = false;
        candidate.status.connected = false;
        candidate.status.signed_in = false;
        candidate.status.signing_in = true;
        candidate.status.error = None;
        candidate.status.notice = Some("Moving this account to the verified service. Its saved queues are paused".into());
        self.commit_candidate(candidate)?;
        Ok(LoginOptions { endpoint, device: profile.device, device_proof: proof, admission: None, label: label.into() })
    }

    /// A successful login is staged, not activated. Canvas migration must commit first.
    pub fn stage_hosted_relocation(&mut self, session: &Session) -> Result<()> {
        let plan = self.state.relocation.clone().ok_or_else(|| Error::Invalid("No service move is pending".into()))?;
        if session.profile != plan.profile || session.device != plan.device || session.expires_at_ms <= now_ms()? {
            return Err(Error::Invalid("Destination did not prove the original profile and device; original queues remain paused".into()));
        }
        let proof_key = hosted_key("proof", &self.state, &plan.to_endpoint);
        let proof = self.credentials.get(&self.state.namespace, &proof_key)?.ok_or_else(|| Error::Credentials("Destination device proof is missing".into()))?;
        if proof != session.device_credential {
            return Err(Error::Invalid("Destination changed the original device proof".into()));
        }
        validate_secret(&session.token).map_err(|_| Error::Invalid("Invalid destination session".into()))?;
        photocraft_party::contracts::validate_label(&session.label, 96, false).map_err(|_| Error::Invalid("Invalid destination profile label".into()))?;
        let key = hosted_key("session", &self.state, &plan.to_endpoint);
        self.register_credential(&key)?;
        self.credentials.set(&self.state.namespace, &key, &session.token)?;
        if self.credentials.get(&self.state.namespace, &key)?.as_deref() != Some(session.token.as_str()) {
            return Err(Error::Credentials("Destination session readback failed; original queues remain paused".into()));
        }
        let mut candidate = self.state.clone();
        candidate.relocation_session = Some(StagedSession { label: session.label.clone(), expires_at_ms: session.expires_at_ms });
        candidate.status.signing_in = false;
        self.commit_candidate(candidate)
    }

    pub fn relocation_access(&self) -> Result<Option<(AccountRelocation, WorldAccess)>> {
        let Some(plan) = self.state.relocation.clone() else { return Ok(None) };
        let Some(session) = &self.state.relocation_session else { return Ok(None) };
        if session.expires_at_ms <= now_ms()? {
            return Err(Error::Rejected { code: "session".into(), message: "Destination sign-in expired; resume the service move with Google sign-in".into() });
        }
        let credential = self
            .credentials
            .get(&self.state.namespace, &hosted_key("session", &self.state, &plan.to_endpoint))?
            .ok_or_else(|| Error::Credentials("Destination sign-in is missing; resume the service move".into()))?;
        let scope = WorldScope::new(
            &plan.to_endpoint,
            photocraft_scene::OwnerId::new(plan.profile.as_str()).map_err(|_| Error::Invalid("Invalid relocation profile".into()))?,
            photocraft_scene::shared::DeviceId::new(plan.device.as_str()).map_err(|_| Error::Invalid("Invalid relocation device".into()))?,
        )?;
        Ok(Some((plan, WorldAccess::for_relocation(scope, credential)?)))
    }

    /// Internal coordinator acknowledgment only, after WorldOutbox's durable receipt.
    /// Do not expose this as a user command or accept a claimed remote acknowledgment.
    pub fn complete_hosted_relocation(&mut self, id: RequestId) -> Result<()> {
        let plan = self.state.relocation.clone().ok_or_else(|| Error::Invalid("No service move is pending".into()))?;
        if plan.id != id {
            return Err(Error::Invalid("Service move acknowledgment is stale".into()));
        }
        let staged =
            self.state.relocation_session.clone().ok_or_else(|| Error::Invalid("Destination sign-in must finish before completing the move".into()))?;
        self.relocation_access()?.ok_or_else(|| Error::Invalid("Destination sign-in is not available".into()))?;
        let mut candidate = self.state.clone();
        candidate.status.endpoint = Some(plan.to_endpoint.clone());
        candidate.status.profile = Some(Participant { id: plan.profile, device: plan.device, display_name: staged.label });
        candidate.hosted_device = Some(plan.device);
        candidate.status.hosted = true;
        candidate.status.signed_in = true;
        candidate.status.signing_in = false;
        candidate.status.paired = true;
        candidate.status.connected = false;
        candidate.status.error = None;
        candidate.status.notice = Some("Service move completed; original artwork and pending requests are retained".into());
        candidate.session_expires_at_ms = staged.expires_at_ms;
        candidate.relocation = None;
        candidate.relocation_session = None;
        candidate.linking_legacy = false;
        candidate.active = true;
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        let collision: bool =
            tx.query_row("SELECT EXISTS(SELECT 1 FROM local_assignment_notes WHERE endpoint=?1)", [&plan.to_endpoint], |row| row.get(0)).map_err(sql)?;
        if collision {
            return Err(Error::Storage("Destination already has local note records; preserve both sources before completing this move".into()));
        }
        tx.execute("UPDATE local_assignment_notes SET endpoint=?1 WHERE endpoint=?2", params![plan.to_endpoint, plan.from_endpoint]).map_err(sql)?;
        write_state(&tx, &candidate)?;
        tx.commit().map_err(sql)?;
        self.state = candidate;
        self.refresh_counts()?;
        self.refresh_accounts()
    }

    pub(super) fn stop_relocation_sign_in(&mut self) -> Result<()> {
        let Some(plan) = self.state.relocation.clone() else { return Ok(()) };
        let mut candidate = self.state.clone();
        candidate.status.signing_in = false;
        candidate.status.signed_in = false;
        candidate.status.connected = false;
        candidate.active = false;
        candidate.relocation_session = None;
        let result = self.commit_candidate(candidate.clone());
        if result.is_err() {
            self.state = candidate;
        }
        let removal = self.credentials.remove(&self.state.namespace, &hosted_key("session", &self.state, &plan.to_endpoint));
        result.and(removal)
    }
}

fn backup_before_move(db: &Connection, id: RequestId) -> Result<()> {
    let original = db.path().ok_or_else(|| Error::Storage("Service move requires a file-backed room database".into()))?;
    let backup = format!("{original}.pre-relocation-{}.sqlite3", id.as_str());
    db.execute("VACUUM INTO ?1", [&backup]).map_err(sql)?;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&backup)
        .and_then(|file| file.sync_all())
        .map_err(|_| Error::Storage("Cannot confirm durable room migration backup".into()))?;
    let verify = Connection::open_with_flags(&backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(sql)?;
    let check: String = verify.query_row("PRAGMA quick_check", [], |row| row.get(0)).map_err(sql)?;
    if check != "ok" {
        return Err(Error::Storage("Room migration backup failed integrity verification".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
