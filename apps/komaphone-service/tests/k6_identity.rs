//! Uses only the production capability boundary; fake Google verification is
//! confined to cfg(test) unit tests and is unavailable to this integration test.
use photocraft_party::{
    AssetHash, DeviceId, Mode, Participant, ParticipantId, RequestId,
    protocol::{Operation, RpcRequest, RpcResponse},
};
use photocraft_sync::{Store, StoreOptions};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

fn request(id: u64, operation: Operation) -> RpcRequest {
    RpcRequest { protocol_version: 1, request_id: RequestId::new(&format!("{id:032x}")).unwrap(), operation }
}

fn fixture(path: &std::path::Path) -> (Participant, String) {
    let mut store = Store::open(path, StoreOptions::default(), 0).unwrap();
    let person = Participant {
        id: ParticipantId::new(&format!("{:032x}", 1)).unwrap(),
        device: DeviceId::new(&format!("{:032x}", 101)).unwrap(),
        display_name: "Existing profile".into(),
    };
    let token = format!("{:064x}", 1001);
    store
        .execute(
            None,
            &RpcRequest {
                protocol_version: 1,
                request_id: RequestId::new(&format!("{:032x}", 1)).unwrap(),
                operation: Operation::Pair { participant: person.clone(), credential: token.clone() },
            },
            0,
        )
        .unwrap();
    (person, token)
}
fn downgrade_fixture(path: &std::path::Path) {
    let db = Connection::open(path).unwrap();
    db.execute_batch("DROP TABLE IF EXISTS world_import_assets; DROP TABLE IF EXISTS world_imports; DROP TABLE IF EXISTS world_import_receipts; DROP TABLE IF EXISTS world_import_origins; DROP TABLE IF EXISTS world_trash; DROP TABLE IF EXISTS world_blobs; DROP TABLE IF EXISTS world_media_sources; DROP TABLE IF EXISTS world_events; DROP TABLE IF EXISTS world_invites; DROP TABLE IF EXISTS world_members; DROP TABLE IF EXISTS worlds; DROP TABLE IF EXISTS world_schema; DROP TABLE IF EXISTS hosted_admissions; DROP TABLE IF EXISTS hosted_profiles; DROP TABLE IF EXISTS hosted_schema; DROP TABLE identity_sessions; DROP TABLE identity_logins; DROP TABLE identity_accounts; DROP TABLE identity_devices; DROP TABLE identity_profiles; DROP TABLE identity_schema; PRAGMA user_version=1;").unwrap();
}

#[test]
fn v1_migration_retains_ids_credentials_and_pre_migration_backup() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("service.sqlite3");
    let (person, token) = fixture(&path);
    downgrade_fixture(&path);
    let original = std::fs::read(&path).unwrap();
    let store = Store::open(&path, StoreOptions::default(), 100).unwrap();
    let context = store.identity_authenticate(&token, 100).unwrap();
    assert_eq!(context.profile, person.id);
    assert_eq!(context.device, person.device);
    let snapshots: Vec<_> = std::fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|entry| entry.file_name().unwrap().to_string_lossy().contains("before-identity-v2"))
        .collect();
    assert_eq!(snapshots.len(), 1);
    let backup = Connection::open(&snapshots[0]).unwrap();
    assert_eq!(backup.pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0)).unwrap(), 1);
    let saved: (String, String) = backup.query_row("SELECT actor,device FROM identities", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(saved, (person.id.as_str().into(), person.device.as_str().into()));
    drop(backup);
    // SQLite backup may normalize header counters, so compare immutable rows,
    // then prove rollback can open a fresh copy without touching the snapshot.
    assert!(!original.is_empty());
    let snapshot_bytes = std::fs::read(&snapshots[0]).unwrap();
    let restore = root.path().join("restored.sqlite3");
    Store::copy_verified(&snapshots[0], &restore).unwrap();
    let recovered = Store::open(&restore, StoreOptions::default(), 200).unwrap();
    assert_eq!(recovered.identity_authenticate(&token, 200).unwrap(), context);
    assert_eq!(std::fs::read(&snapshots[0]).unwrap(), snapshot_bytes);
    drop(store);
    let reopened = Store::open(&path, StoreOptions::default(), 300).unwrap();
    assert_eq!(reopened.identity_authenticate(&token, 300).unwrap(), context);
}

#[test]
fn migration_error_preserves_legacy_data_and_a_usable_backup() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("service.sqlite3");
    let (person, _) = fixture(&path);
    downgrade_fixture(&path);
    let db = Connection::open(&path).unwrap();
    // A conflicting unexpected table forces the additive transaction to roll back.
    db.execute_batch("CREATE TABLE identity_devices(unexpected TEXT);").unwrap();
    drop(db);
    let before = std::fs::read(&path).unwrap();
    assert!(Store::open(&path, StoreOptions::default(), 100).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let db = Connection::open(&path).unwrap();
    assert_eq!(db.pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0)).unwrap(), 1);
    assert_eq!(db.query_row::<String, _, _>("SELECT actor FROM identities", [], |r| r.get(0)).unwrap(), person.id.as_str());
    assert!(!db.query_row::<bool, _, _>("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='identity_schema')", [], |r| r.get(0)).unwrap());
}

#[test]
fn migration_preserves_room_membership_asset_authorship_and_retry_receipts() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("service.sqlite3");
    let (host, token) = fixture(&path);
    let mut store = Store::open(&path, StoreOptions::default(), 0).unwrap();
    let create = request(10, Operation::Create { mode: Mode::Animation { rotations: 2 }, timer_ms: None });
    let RpcResponse::Room { view, .. } = store.execute(Some(&token), &create, 0).unwrap() else { panic!("room") };
    let room = view.id;
    let other = Participant {
        id: ParticipantId::new(&format!("{:032x}", 2)).unwrap(),
        device: DeviceId::new(&format!("{:032x}", 102)).unwrap(),
        display_name: "Member".into(),
    };
    let token2 = format!("{:064x}", 1002);
    store.execute(None, &request(11, Operation::Pair { participant: other.clone(), credential: token2.clone() }), 0).unwrap();
    let invitation = format!("{:064x}", 2000);
    store.execute(Some(&token), &request(12, Operation::Invite { room, secret: invitation.clone(), expires_in_ms: 60000, uses: 1 }), 0).unwrap();
    store.execute(Some(&token2), &request(13, Operation::Join { room, invitation }), 0).unwrap();
    let bytes = b"legacy immutable payload".to_vec();
    let hash = AssetHash::new(&format!("{:x}", Sha256::digest(&bytes))).unwrap();
    let upload = request(14, Operation::Upload { room, hash, total: bytes.len() as u64, offset: 0, bytes: bytes.clone(), finish: true });
    let accepted = store.execute(Some(&token), &upload, 0).unwrap();
    drop(store);
    downgrade_fixture(&path);
    let mut store = Store::open(&path, StoreOptions::default(), 100).unwrap();
    let RpcResponse::Room { view, .. } = store.execute(Some(&token2), &request(15, Operation::Poll { room, after_revision: 0 }), 100).unwrap() else {
        panic!("room")
    };
    assert_eq!(view.id, room);
    assert_eq!(view.moderator, host.id);
    assert_eq!(view.members.len(), 2);
    assert_eq!(store.execute(Some(&token), &upload, 100).unwrap(), accepted);
    let db = Connection::open(&path).unwrap();
    let (saved_room, author, data): (String, String, Vec<u8>) =
        db.query_row("SELECT room,actor,data FROM objects WHERE hash=?1", [hash.as_str()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
    assert_eq!(saved_room, room.as_str());
    assert_eq!(author, host.id.as_str());
    assert_eq!(data, bytes);
    assert_eq!(store.identity_authenticate(&token2, 100).unwrap().profile, other.id);
}

#[tokio::test]
async fn http_distinguishes_a_revoked_login_from_canvas_membership_denial() {
    use photocraft_scene::{
        CanvasId,
        world_protocol::{self as wire, Action},
    };
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("service.sqlite3");
    let (owner, token) = fixture(&path);
    let mut store = Store::open(&path, StoreOptions::default(), 0).unwrap();
    let canvas = CanvasId::new(&format!("{:032x}", 500)).unwrap();
    store.world_execute(&token, &wire::Request { version: wire::VERSION, action: Action::Create { canvas, name: "Private canvas".into() } }, 0).unwrap();
    let guest = Participant {
        id: ParticipantId::new(&format!("{:032x}", 2)).unwrap(),
        device: DeviceId::new(&format!("{:032x}", 102)).unwrap(),
        display_name: "Nonmember".into(),
    };
    let guest_token = format!("{:064x}", 1002);
    store.execute(None, &request(2, Operation::Pair { participant: guest, credential: guest_token.clone() }), 0).unwrap();
    store.identity_revoke_device(&token, owner.device, 0).unwrap();
    drop(store);
    let server = photocraft_sync::server::spawn("127.0.0.1:0".parse().unwrap(), &path, StoreOptions::default()).await.unwrap();
    let client = reqwest::Client::new();
    let request = wire::Request { version: wire::VERSION, action: Action::Info { canvas } };
    for (credential, status, code) in [(&token, reqwest::StatusCode::UNAUTHORIZED, "session"), (&guest_token, reqwest::StatusCode::FORBIDDEN, "unauthorized")] {
        let response = client.post(format!("http://{}/v1/canvas", server.address)).bearer_auth(credential).json(&request).send().await.unwrap();
        assert_eq!(response.status(), status);
        let bytes = response.bytes().await.unwrap();
        let fault: photocraft_party::protocol::Fault = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(fault.code, code);
        let body = String::from_utf8_lossy(&bytes);
        assert!(!body.contains(credential));
        assert!(!body.contains("Private canvas"));
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn identity_readback_is_derived_from_bearer_profile_and_writer_and_rejects_claims() {
    use photocraft_scene::world_protocol::{self as wire, Action, Response};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("identity.sqlite3");
    let (person, token) = fixture(&path);
    let server = photocraft_sync::server::spawn("127.0.0.1:0".parse().unwrap(), &path, StoreOptions::default()).await.unwrap();
    let client = reqwest::Client::new();
    let url = format!("http://{}/v1/canvas", server.address);
    let response = client.post(&url).bearer_auth(&token).json(&wire::Request { version: wire::VERSION, action: Action::Identity {} }).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let Response::Identity { profile, device } = response.json::<Response>().await.unwrap() else { panic!("identity") };
    assert_eq!(profile.as_str(), person.id.as_str());
    assert_eq!(device.as_str(), person.device.as_str());
    let claimed = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"version":1,"action":{"kind":"identity","profile":format!("{:032x}",999)}}))
        .send()
        .await
        .unwrap();
    assert_eq!(claimed.status(), reqwest::StatusCode::BAD_REQUEST);
    let denied = client
        .post(&url)
        .bearer_auth(format!("{:064x}", 999))
        .json(&wire::Request { version: wire::VERSION, action: Action::Identity {} })
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), reqwest::StatusCode::UNAUTHORIZED);
    server.shutdown().await.unwrap();
}
