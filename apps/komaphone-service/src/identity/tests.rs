use super::*;
use photocraft_party::protocol::{Operation, RpcRequest};

fn device(n: u64) -> DeviceId {
    DeviceId::new(&format!("{n:032x}")).unwrap()
}
fn pair(store: &mut Store, n: u64) -> (Participant, String) {
    let participant = Participant { id: ParticipantId::new(&format!("{n:032x}")).unwrap(), device: device(n + 100), display_name: "Identical label".into() };
    let token = format!("{:064x}", n + 1000);
    store
        .execute(
            None,
            &RpcRequest {
                protocol_version: 1,
                request_id: RequestId::new(&format!("{n:032x}")).unwrap(),
                operation: Operation::Pair { participant: participant.clone(), credential: token.clone() },
            },
            0,
        )
        .unwrap();
    (participant, token)
}
/// This adapter is compiled only into this unit-test module. It performs no
/// Google verification and must never be exported or enabled by a Cargo feature.
fn local_test_verifier(challenge: &LoginChallenge, subject: &str, now: u64) -> VerifiedLogin {
    VerifiedLogin {
        transaction: challenge.id,
        nonce_hash: crate::store::digest(challenge.nonce.as_bytes()),
        issuer: GOOGLE_ISSUER.into(),
        subject: subject.into(),
        expires_at_ms: now + LOGIN_LIFETIME_MS,
    }
}
fn fixture() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path().join("service.sqlite3"), Default::default(), 0).unwrap();
    (root, store)
}

#[test]
fn proof_link_retains_profile_and_distinct_devices_across_restart() {
    let (root, mut store) = fixture();
    let (legacy, proof) = pair(&mut store, 1);
    let login = store.identity_begin_login(legacy.device, 100).unwrap();
    let session = store.identity_finish_login(local_test_verifier(&login, "account-subject-1", 100), Some(&proof), 100).unwrap();
    assert_eq!(session.context, AuthContext { profile: legacy.id, device: legacy.device });
    let new_device = device(999);
    let login2 = store.identity_begin_login(new_device, 200).unwrap();
    let session2 = store.identity_finish_login(local_test_verifier(&login2, "account-subject-1", 200), None, 200).unwrap();
    assert_eq!(session2.context, AuthContext { profile: legacy.id, device: new_device });
    drop(store);
    let store = Store::open(root.path().join("service.sqlite3"), Default::default(), 300).unwrap();
    assert_eq!(store.identity_authenticate(session.token(), 300).unwrap(), session.context);
    assert_eq!(store.identity_authenticate(session2.token(), 300).unwrap(), session2.context);
    assert_eq!(store.identity_authenticate(&proof, 300).unwrap(), session.context);
    let raw = std::fs::read(root.path().join("service.sqlite3")).unwrap();
    for secret in [session.token(), session2.token(), session2.device_credential(), &proof, &login.nonce] {
        assert!(!raw.windows(secret.len()).any(|window| window == secret.as_bytes()), "durable credentials must be hashed");
    }
}

#[test]
fn unknown_account_requires_legacy_proof_and_labels_never_merge_profiles() {
    let (_root, mut store) = fixture();
    let (alice, proof_a) = pair(&mut store, 1);
    let (bob, proof_b) = pair(&mut store, 2);
    assert_eq!(alice.display_name, bob.display_name);
    let login = store.identity_begin_login(alice.device, 100).unwrap();
    assert!(store.identity_finish_login(local_test_verifier(&login, "alice", 100), None, 100).is_err());
    assert!(store.identity_finish_login(local_test_verifier(&login, "alice", 100), Some(&proof_b), 100).is_err());
    store.identity_finish_login(local_test_verifier(&login, "alice", 100), Some(&proof_a), 100).unwrap();
    let login = store.identity_begin_login(bob.device, 100).unwrap();
    assert!(store.identity_finish_login(local_test_verifier(&login, "alice", 100), Some(&proof_b), 100).is_err());
    // Even the right account cannot steal the other profile's preexisting device.
    assert!(store.identity_finish_login(local_test_verifier(&login, "alice", 100), None, 100).is_err());
    store.identity_finish_login(local_test_verifier(&login, "bob", 100), Some(&proof_b), 100).unwrap();
    let login = store.identity_begin_login(alice.device, 100).unwrap();
    assert!(store.identity_finish_login(local_test_verifier(&login, "wrong-account", 100), Some(&proof_a), 100).is_err());
    assert_eq!(store.identity_authenticate(&proof_a, 100).unwrap().profile, alice.id);
    assert_eq!(store.identity_authenticate(&proof_b, 100).unwrap().profile, bob.id);
}

#[test]
fn transaction_binding_nonce_expiry_and_successful_replay_are_enforced() {
    let (_root, mut store) = fixture();
    let (legacy, proof) = pair(&mut store, 1);
    let login = store.identity_begin_login(legacy.device, 100).unwrap();
    let mut wrong = local_test_verifier(&login, "alice", 100);
    wrong.nonce_hash = "bad nonce".into();
    assert!(store.identity_finish_login(wrong, Some(&proof), 100).is_err());
    let mut wrong = local_test_verifier(&login, "alice", 100);
    wrong.issuer = "https://example.invalid".into();
    assert!(store.identity_finish_login(wrong, Some(&proof), 100).is_err());
    let mut expired = local_test_verifier(&login, "alice", 100);
    expired.expires_at_ms = 100;
    assert!(store.identity_finish_login(expired, Some(&proof), 100).is_err());
    store.identity_finish_login(local_test_verifier(&login, "alice", 100), Some(&proof), 100).unwrap();
    assert!(store.identity_finish_login(local_test_verifier(&login, "alice", 100), Some(&proof), 101).is_err());
    let expired_login = store.identity_begin_login(legacy.device, 200).unwrap();
    assert!(store.identity_finish_login(local_test_verifier(&expired_login, "alice", 200 + LOGIN_LIFETIME_MS), None, 200 + LOGIN_LIFETIME_MS).is_err());
}

#[test]
fn same_account_cannot_reuse_a_device_writer_without_its_possession_proof() {
    let (_root, mut store) = fixture();
    let (legacy, proof) = pair(&mut store, 1);
    let login = store.identity_begin_login(legacy.device, 100).unwrap();
    store.identity_finish_login(local_test_verifier(&login, "alice", 100), Some(&proof), 100).unwrap();
    let new_device = device(999);
    let login = store.identity_begin_login(new_device, 100).unwrap();
    let new_session = store.identity_finish_login(local_test_verifier(&login, "alice", 100), None, 100).unwrap();
    let reuse = store.identity_begin_login(new_device, 200).unwrap();
    assert!(store.identity_finish_login(local_test_verifier(&reuse, "alice", 200), None, 200).is_err());
    assert!(store.identity_finish_login(local_test_verifier(&reuse, "alice", 200), Some(&proof), 200).is_err());
    assert!(store.identity_finish_login(local_test_verifier(&reuse, "alice", 200), Some(new_session.token()), 200).is_err());
    let renewed = store.identity_finish_login(local_test_verifier(&reuse, "alice", 200), Some(new_session.device_credential()), 200).unwrap();
    assert_eq!(renewed.context, new_session.context);
    assert_eq!(renewed.device_credential(), new_session.device_credential());
    // Legacy pairing must not bypass a credential already reserved for a new
    // writer or leave an orphan that makes the next service restart fail.
    let attempted = Participant { id: ParticipantId::new(&format!("{:032x}", 500)).unwrap(), device: device(501), display_name: "collision".into() };
    let request = RpcRequest {
        protocol_version: 1,
        request_id: RequestId::new(&format!("{:032x}", 501)).unwrap(),
        operation: Operation::Pair { participant: attempted, credential: new_session.device_credential().into() },
    };
    assert!(store.execute(None, &request, 300).is_err());
    verify(&store.db).unwrap();
    assert_eq!(store.identity_authenticate(renewed.token(), 300).unwrap(), new_session.context);
}

#[test]
fn revoked_device_and_expired_session_do_not_resurrect_other_sessions() {
    let (_root, mut store) = fixture();
    let (legacy, proof) = pair(&mut store, 1);
    let login = store.identity_begin_login(legacy.device, 100).unwrap();
    let session = store.identity_finish_login(local_test_verifier(&login, "alice", 100), Some(&proof), 100).unwrap();
    assert!(store.identity_authenticate(session.token(), session.expires_at_ms).is_err());
    let login2 = store.identity_begin_login(device(999), 100).unwrap();
    let session2 = store.identity_finish_login(local_test_verifier(&login2, "alice", 100), None, 100).unwrap();
    store.identity_revoke_device(session2.token(), legacy.device, 200).unwrap();
    assert!(store.identity_authenticate(session.token(), 201).is_err());
    assert!(store.identity_authenticate(&proof, 201).is_err());
    assert!(store.identity_authenticate(session2.token(), 201).is_ok());
    let login3 = store.identity_begin_login(legacy.device, 300).unwrap();
    let session3 = store.identity_finish_login(local_test_verifier(&login3, "alice", 300), Some(&proof), 300).unwrap();
    assert!(store.identity_authenticate(session3.token(), 301).is_ok());
    assert!(store.identity_authenticate(session.token(), 301).is_err());
    assert!(store.identity_authenticate(&proof, 301).is_err());
    store.identity_revoke_session(session3.token(), 301).unwrap();
    assert!(store.identity_authenticate(session3.token(), 302).is_err());
    assert!(store.identity_authenticate(session2.token(), 302).is_ok());
}

#[test]
fn other_profile_cannot_revoke_and_limits_fail_without_mutating_binding() {
    let (_root, mut store) = fixture();
    let (alice, proof_a) = pair(&mut store, 1);
    let (bob, proof_b) = pair(&mut store, 2);
    assert!(store.identity_revoke_device(&proof_a, bob.device, 0).is_err());
    assert!(store.identity_authenticate(&proof_b, 0).is_ok());
    for now in [u64::MAX, i64::MAX as u64] {
        assert!(store.identity_begin_login(alice.device, now).is_err());
    }
    for bad in ["", "not a credential"] {
        assert!(store.identity_authenticate(bad, 0).is_err());
    }
    let login = store.identity_begin_login(alice.device, 100).unwrap();
    store.identity_finish_login(local_test_verifier(&login, "alice", 100), Some(&proof_a), 100).unwrap();
    for n in 1..MAX_DEVICES_PER_PROFILE {
        let login = store.identity_begin_login(device(1000 + n), 100).unwrap();
        store.identity_finish_login(local_test_verifier(&login, "alice", 100), None, 100).unwrap();
    }
    let login = store.identity_begin_login(device(9000), 100).unwrap();
    assert!(store.identity_finish_login(local_test_verifier(&login, "alice", 100), None, 100).is_err());
    assert_eq!(store.identity_authenticate(&proof_a, 100).unwrap().profile, alice.id);
}

#[test]
fn failed_commit_does_not_consume_transaction_or_reassign_profile() {
    let (_root, mut store) = fixture();
    let (alice, proof) = pair(&mut store, 1);
    let login = store.identity_begin_login(alice.device, 100).unwrap();
    store
        .db
        .execute_batch("CREATE TEMP TRIGGER reject_session BEFORE INSERT ON identity_sessions BEGIN SELECT RAISE(ABORT,'simulated failure'); END;")
        .unwrap();
    assert!(store.identity_finish_login(local_test_verifier(&login, "alice", 100), Some(&proof), 100).is_err());
    let linked: i64 = store.db.query_row("SELECT count(*) FROM identity_accounts", [], |r| r.get(0)).unwrap();
    assert_eq!(linked, 0);
    store.db.execute_batch("DROP TRIGGER reject_session;").unwrap();
    let session = store.identity_finish_login(local_test_verifier(&login, "alice", 100), Some(&proof), 100).unwrap();
    assert_eq!(session.context.profile, alice.id);
}

#[test]
fn revoked_unlinked_legacy_credential_cannot_bind_a_new_verified_account() {
    let (_root, mut store) = fixture();
    let (legacy, proof) = pair(&mut store, 1);
    store.identity_revoke_device(&proof, legacy.device, 100).unwrap();
    let login = store.identity_begin_login(legacy.device, 200).unwrap();
    assert!(store.identity_finish_login(local_test_verifier(&login, "attacker-account", 200), Some(&proof), 200).is_err());
    let linked: i64 = store.db.query_row("SELECT count(*) FROM identity_accounts", [], |r| r.get(0)).unwrap();
    assert_eq!(linked, 0);
    assert!(store.identity_authenticate(&proof, 200).is_err());
}
