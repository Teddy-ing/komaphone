use super::*;
use crate::CredentialStore;
use photocraft_party::ParticipantId;
use photocraft_party::protocol::{Operation, RpcResponse};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Vault(Arc<Mutex<BTreeMap<(String, String), String>>>);
impl CredentialStore for Vault {
    fn get(&self, namespace: &str, key: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(&(namespace.into(), key.into())).cloned())
    }
    fn set(&self, namespace: &str, key: &str, value: &str) -> Result<()> {
        self.0.lock().unwrap().insert((namespace.into(), key.into()), value.into());
        Ok(())
    }
    fn remove(&self, namespace: &str, key: &str) -> Result<()> {
        self.0.lock().unwrap().remove(&(namespace.into(), key.into()));
        Ok(())
    }
}

fn session(login: &LoginOptions, profile: &str) -> Session {
    Session {
        profile: ParticipantId::new(&profile.repeat(32)).unwrap(),
        device: login.device,
        label: login.label.clone(),
        token: random_hex(32).unwrap(),
        device_credential: login.device_proof.clone(),
        expires_at_ms: now_ms().unwrap() + 60_000,
    }
}

fn login(store: &mut Outbox, profile: &str) -> Session {
    let options = store.prepare_hosted_login("https://service.example", "Drawer", None).unwrap();
    let session = session(&options, profile);
    store.install_hosted_session("https://service.example", &session).unwrap();
    session
}

#[test]
fn switching_preserves_exact_queue_and_device_and_isolates_tokens_after_restart() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("rooms.db");
    let vault = Vault::default();
    let mut store = Outbox::open(&path, Box::new(vault.clone())).unwrap();
    let first = login(&mut store, "1");
    let first_slot = store.status().account_slot.unwrap();
    let request = store.enqueue(Operation::Create { mode: photocraft_party::Mode::ExquisiteCorpse, timer_ms: None }).unwrap();
    let original = serde_json::to_vec(&store.next().unwrap().unwrap()).unwrap();
    assert_eq!(store.credential().unwrap(), first.token);
    store.select_account(None).unwrap();
    assert_eq!(store.status().pending, 0);
    assert!(store.status().profile.is_none());
    let second = login(&mut store, "2");
    assert_ne!(first.device, second.device);
    assert_ne!(store.credential().unwrap(), first.token);
    assert!(store.next().unwrap().is_none());
    drop(store);
    let mut store = Outbox::open(&path, Box::new(vault)).unwrap();
    store.select_account(Some(&first_slot)).unwrap();
    assert!(!store.active());
    assert_eq!(store.status().pending, 1);
    assert!(store.next().unwrap().is_none());
    assert!(store.retry().is_err());
    let options = store.prepare_hosted_login("https://service.example", "Drawer", None).unwrap();
    assert_eq!(options.device, first.device);
    assert_eq!(options.device_proof, first.device_credential);
    let returned = session(&options, "1");
    store.install_hosted_session("https://service.example", &returned).unwrap();
    let restored = store.next().unwrap().unwrap();
    assert_eq!(restored.request_id, request);
    assert_eq!(serde_json::to_vec(&restored).unwrap(), original);
    assert_ne!(store.credential().unwrap(), second.token);
}

#[test]
fn signout_masks_cached_private_state_but_preserves_it_and_never_uses_device_proof_as_session() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("rooms.db");
    let vault = Vault::default();
    let mut store = Outbox::open(&path, Box::new(vault.clone())).unwrap();
    let first = login(&mut store, "1");
    let profile = store.status().profile.unwrap();
    let room =
        photocraft_party::room::Room::new(photocraft_party::RoomId::new(&"3".repeat(32)).unwrap(), profile, photocraft_party::Mode::ExquisiteCorpse, None)
            .unwrap();
    store.update_poll(room.view(first.profile).unwrap()).unwrap();
    assert!(store.status().room.is_some());
    store.sign_out_local().unwrap();
    assert!(store.status().room.is_none());
    assert!(!store.status().paired);
    assert!(!store.status().signed_in);
    assert!(store.credential().is_err());
    assert!(store.retry().is_err());
    assert!(store.state.status.room.is_some());
    drop(store);
    let mut store = Outbox::open(&path, Box::new(vault)).unwrap();
    assert!(store.status().room.is_none());
    assert!(store.credential().is_err());
    let next = login(&mut store, "1");
    assert_eq!(next.device, first.device);
    assert!(store.status().room.is_some());
    let bytes = std::fs::read(path).unwrap();
    for secret in [first.token, first.device_credential, next.token] {
        assert!(!bytes.windows(secret.len()).any(|window| window == secret.as_bytes()));
    }
}

#[test]
fn wrong_account_proof_or_device_does_not_replace_saved_identity_or_resume_queue() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Outbox::open(temp.path().join("rooms.db"), Box::new(Vault::default())).unwrap();
    let first = login(&mut store, "1");
    let options = store.prepare_hosted_login("https://service.example", "Drawer", None).unwrap();
    let wrong = session(&options, "2");
    assert!(store.install_hosted_session("https://service.example", &wrong).is_err());
    assert_eq!(store.status().profile.unwrap().id, first.profile);
    assert!(!store.active());
    assert!(store.configure("https://service.example", "Pair bypass").is_err());
    let mut valid = session(&options, "1");
    valid.device_credential = "c".repeat(64);
    assert!(store.install_hosted_session("https://service.example", &valid).is_err());
    assert!(store.credential().is_err());
}

#[test]
fn pending_login_reopen_keeps_device_proof_and_returns_to_signedout() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("rooms.db");
    let vault = Vault::default();
    let mut store = Outbox::open(&path, Box::new(vault.clone())).unwrap();
    let options = store.prepare_hosted_login("https://service.example", "Drawer", None).unwrap();
    assert!(store.status().signing_in);
    drop(store);
    let mut store = Outbox::open(path, Box::new(vault)).unwrap();
    assert!(!store.status().signing_in);
    assert!(!store.active());
    let resumed = store.prepare_hosted_login("https://service.example", "Drawer", None).unwrap();
    assert_eq!(options.device, resumed.device);
    assert_eq!(options.device_proof, resumed.device_proof);
}

#[test]
fn schema_one_migration_retains_legacy_profile_and_takes_readable_backup() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("rooms.db");
    let vault = Vault::default();
    let mut store = Outbox::open(&path, Box::new(vault.clone())).unwrap();
    store.configure("https://service.example", "Legacy").unwrap();
    let request = store.next().unwrap().unwrap();
    let Operation::Pair { participant, .. } = &request.operation else { panic!("pair") };
    let original = participant.clone();
    store.acknowledge(&request, &RpcResponse::Paired { participant: original.clone() }).unwrap();
    let proof = store.credential().unwrap();
    let legacy_key = format!("identity/{}", digest(b"https://service.example"));
    vault.set(&store.state.namespace, &legacy_key, &proof).unwrap();
    store.register_credential(&legacy_key).unwrap();
    let mut value = serde_json::to_value(&store.state).unwrap();
    let object = value.as_object_mut().unwrap();
    object.insert("schema".into(), 1.into());
    for field in ["slot", "hosted_device", "session_expires_at_ms", "legacy_unscoped_credential", "linking_legacy"] {
        object.remove(field);
    }
    let body = serde_json::to_vec(&value).unwrap();
    store.db.execute("UPDATE state SET body=?1,digest=?2", params![body, digest(&body)]).unwrap();
    store.db.execute_batch("DROP TABLE account_slots; DROP TABLE account_outbox; DROP TABLE account_notes; PRAGMA user_version=1;").unwrap();
    drop(store);
    let mut store = Outbox::open(&path, Box::new(vault)).unwrap();
    assert_eq!(store.status().profile, Some(original.clone()));
    let backups: Vec<_> =
        std::fs::read_dir(temp.path()).unwrap().map(|entry| entry.unwrap().path()).filter(|path| path.to_string_lossy().contains("pre-hosted-v1")).collect();
    assert_eq!(backups.len(), 1);
    let backup = Connection::open(&backups[0]).unwrap();
    assert_eq!(backup.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0)).unwrap(), 1);
    let options = store.prepare_hosted_login("https://service.example", "Legacy", None).unwrap();
    assert_eq!(options.device, original.device);
    assert_eq!(options.device_proof, proof);
    store.cancel_hosted_login().unwrap();
    assert!(!store.status().hosted);
    assert_eq!(store.credential().unwrap(), proof);
}

#[test]
fn fresh_legacy_slots_never_reuse_another_profiles_device_credential() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Outbox::open(temp.path().join("rooms.db"), Box::new(Vault::default())).unwrap();
    store.configure("http://127.0.0.1:43127", "First").unwrap();
    let first = store.credential().unwrap();
    let first_slot = store.status().account_slot.unwrap();
    store.select_account(None).unwrap();
    store.configure("http://127.0.0.1:43127", "Second").unwrap();
    assert_ne!(store.credential().unwrap(), first);
    store.select_account(Some(&first_slot)).unwrap();
    assert_eq!(store.credential().unwrap(), first);
}

#[test]
fn expired_service_session_does_not_poison_pending_request() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Outbox::open(temp.path().join("rooms.db"), Box::new(Vault::default())).unwrap();
    login(&mut store, "1");
    let request = store.enqueue(Operation::Create { mode: photocraft_party::Mode::ExquisiteCorpse, timer_ms: None }).unwrap();
    store.mark_error(&Error::Rejected { code: "session".into(), message: "Expired".into() }, Some(request), true).unwrap();
    assert!(!store.status().blocked);
    assert!(!store.active());
    assert_eq!(store.status().pending, 1);
    login(&mut store, "1");
    assert_eq!(store.next().unwrap().unwrap().request_id, request);
}

#[test]
fn signout_with_unwritable_database_still_pauses_and_removes_session() {
    let temp = tempfile::tempdir().unwrap();
    let vault = Vault::default();
    let mut store = Outbox::open(temp.path().join("rooms.db"), Box::new(vault.clone())).unwrap();
    login(&mut store, "1");
    let key = hosted_key("session", &store.state, "https://service.example");
    assert!(vault.get(&store.state.namespace, &key).unwrap().is_some());
    store.db.pragma_update(None, "query_only", true).unwrap();
    let error = store.sign_out_local().unwrap_err().to_string();
    assert!(error.contains("Network work is paused"));
    assert!(!store.active());
    assert!(!store.status().signed_in);
    assert!(store.credential().is_err());
    assert!(vault.get(&store.state.namespace, &key).unwrap().is_none());
}
