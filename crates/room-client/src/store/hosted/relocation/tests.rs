use super::*;
use crate::CredentialStore;
use crate::store::random_hex;
use photocraft_party::ParticipantId;
use photocraft_party::protocol::{Operation, RpcResponse};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
#[derive(Clone, Default)]
struct Vault(Arc<Mutex<BTreeMap<(String, String), String>>>);
impl CredentialStore for Vault {
    fn get(&self, n: &str, k: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(&(n.into(), k.into())).cloned())
    }
    fn set(&self, n: &str, k: &str, v: &str) -> Result<()> {
        self.0.lock().unwrap().insert((n.into(), k.into()), v.into());
        Ok(())
    }
    fn remove(&self, n: &str, k: &str) -> Result<()> {
        self.0.lock().unwrap().remove(&(n.into(), k.into()));
        Ok(())
    }
}
fn legacy(path: &Path, vault: Vault) -> Outbox {
    let mut store = Outbox::open(path, Box::new(vault)).unwrap();
    store.configure("http://127.0.0.1:43127", "Original").unwrap();
    let request = store.next().unwrap().unwrap();
    let Operation::Pair { participant, .. } = &request.operation else { panic!("pair") };
    store.acknowledge(&request, &RpcResponse::Paired { participant: participant.clone() }).unwrap();
    store
}
fn staged(store: &mut Outbox) -> (AccountRelocation, Session) {
    let profile = store.state.status.profile.clone().unwrap();
    let options = store.prepare_hosted_relocation("https://moved.example", "Original").unwrap();
    let session = Session {
        profile: profile.id,
        device: profile.device,
        label: profile.display_name,
        token: random_hex(32).unwrap(),
        device_credential: options.device_proof,
        expires_at_ms: now_ms().unwrap() + 60_000,
    };
    store.stage_hosted_relocation(&session).unwrap();
    (store.state.relocation.clone().unwrap(), session)
}

#[test]
fn move_journal_survives_restart_and_preserves_pending_request_ids_until_world_ack() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("room.db");
    let vault = Vault::default();
    let mut store = legacy(&path, vault.clone());
    let id = store.enqueue(Operation::Create { mode: photocraft_party::Mode::ExquisiteCorpse, timer_ms: None }).unwrap();
    let (plan, session) = staged(&mut store);
    assert_eq!(store.status().endpoint.as_deref(), Some("http://127.0.0.1:43127"));
    assert!(!store.active());
    assert!(store.credential().is_err());
    assert!(store.next().unwrap().is_none());
    assert!(store.select_account(None).is_err());
    assert!(store.retry().is_err());
    assert!(store.prepare_hosted_login("https://moved.example", "Original", None).is_err());
    drop(store);
    let mut store = Outbox::open(path, Box::new(vault)).unwrap();
    let (restored, access) = store.relocation_access().unwrap().unwrap();
    assert_eq!(restored, plan);
    assert_eq!(access.scope().endpoint, "https://moved.example");
    assert!(!store.active());
    store.complete_hosted_relocation(plan.id).unwrap();
    assert!(store.active());
    assert_eq!(store.credential().unwrap(), session.token);
    assert_eq!(store.next().unwrap().unwrap().request_id, id);
    assert_eq!(store.status().endpoint.as_deref(), Some("https://moved.example"));
    assert!(store.status().relocation.is_none());
}

#[test]
fn signout_after_staging_prevents_late_migration_ack_reauthentication() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = legacy(&temp.path().join("room.db"), Vault::default());
    let (plan, session) = staged(&mut store);
    assert_eq!(store.session_for_revocation().unwrap(), Some((plan.to_endpoint.clone(), session.token)));
    store.sign_out_local().unwrap();
    assert!(store.session_for_revocation().unwrap().is_none());
    assert!(store.relocation_access().unwrap().is_none());
    assert!(store.complete_hosted_relocation(plan.id).is_err());
    assert_eq!(store.status().relocation, Some(plan));
    assert!(!store.active());
}

#[test]
fn wrong_identity_and_failed_final_commit_keep_original_queue_paused() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = legacy(&temp.path().join("room.db"), Vault::default());
    let (plan, mut session) = staged(&mut store);
    session.profile = ParticipantId::new(&"a".repeat(32)).unwrap();
    assert!(store.stage_hosted_relocation(&session).is_err());
    assert!(store.complete_hosted_relocation(RequestId::new(&"f".repeat(32)).unwrap()).is_err());
    store.db.pragma_update(None, "query_only", true).unwrap();
    assert!(store.complete_hosted_relocation(plan.id).is_err());
    assert_eq!(store.status().endpoint.as_deref(), Some("http://127.0.0.1:43127"));
    assert_eq!(store.status().relocation, Some(plan));
    assert!(!store.active());
}
