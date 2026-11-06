use photocraft_party::{
    AssetHash, DeviceId, Mode, Participant, ParticipantId, RequestId,
    protocol::{Operation, RpcRequest, RpcResponse},
};
use photocraft_sync::{Store, StoreOptions};
use sha2::{Digest, Sha256};
fn req(id: u64, operation: Operation) -> RpcRequest {
    RpcRequest { protocol_version: 1, request_id: RequestId::new(&format!("{id:032x}")).unwrap(), operation }
}
fn token() -> String {
    format!("{:064x}", 42)
}
fn setup(path: &std::path::Path, quota_bytes: u64) -> (Store, photocraft_party::RoomId) {
    let mut store = Store::open(path, StoreOptions { quota_bytes, ..Default::default() }, 0).unwrap();
    let participant = Participant {
        id: ParticipantId::new(&format!("{:032x}", 1)).unwrap(),
        device: DeviceId::new(&format!("{:032x}", 2)).unwrap(),
        display_name: "Tester".into(),
    };
    store.execute(None, &req(1, Operation::Pair { participant, credential: token() }), 0).unwrap();
    let response = store.execute(Some(&token()), &req(2, Operation::Create { mode: Mode::Animation { rotations: 2 }, timer_ms: None }), 0).unwrap();
    let RpcResponse::Room { view, .. } = response else { panic!("room") };
    (store, view.id)
}
#[test]
fn actual_sqlite_page_quota_rolls_back_upload_without_ack_or_losing_room() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("service.sqlite3");
    let (mut store, room) = setup(&path, 128 * 1024);
    let bytes = vec![1u8; 32768];
    let hash = AssetHash::new(&format!("{:x}", Sha256::digest(&bytes))).unwrap();
    let request = req(3, Operation::Upload { room, hash, total: 256 * 1024, offset: 0, bytes, finish: false });
    assert!(matches!(store.execute(Some(&token()), &request, 0), Err(photocraft_sync::Error::Full)));
    let poll = req(4, Operation::Poll { room, after_revision: 0 });
    assert!(store.execute(Some(&token()), &poll, 0).is_ok());
    drop(store);
    let mut recovered = Store::open(&path, StoreOptions::default(), 0).unwrap();
    assert!(recovered.execute(Some(&token()), &poll, 0).is_ok());
    // A successful retry proves the failed allocation never wrote its receipt or a partial object.
    assert!(recovered.execute(Some(&token()), &request, 0).is_ok());
}
#[test]
fn future_schema_and_oversized_or_corrupt_room_preserve_original_database() {
    for damage in ["schema", "oversize", "json"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("service.sqlite3");
        let (store, _) = setup(&path, 2 * 1024 * 1024 * 1024);
        drop(store);
        let db = rusqlite::Connection::open(&path).unwrap();
        match damage {
            "schema" => db.pragma_update(None, "user_version", 99).unwrap(),
            "oversize" => {
                db.execute("UPDATE rooms SET snapshot=zeroblob(?1)", [(photocraft_party::MAX_ROOM_SNAPSHOT_BYTES + 1) as i64]).unwrap();
            }
            _ => {
                db.execute("UPDATE rooms SET snapshot=?1", [b"{bad".as_slice()]).unwrap();
            }
        }
        drop(db);
        let before = std::fs::read(&path).unwrap();
        assert!(Store::open(&path, StoreOptions::default(), 0).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before, "failed opening must preserve {damage}");
    }
}
#[test]
fn corrupt_complete_asset_is_detected_before_reopening_authority() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("service.sqlite3");
    let (mut store, room) = setup(&path, 2 * 1024 * 1024 * 1024);
    let bytes = b"immutable asset".to_vec();
    let hash = AssetHash::new(&format!("{:x}", Sha256::digest(&bytes))).unwrap();
    store.execute(Some(&token()), &req(3, Operation::Upload { room, hash, total: bytes.len() as u64, offset: 0, bytes, finish: true }), 0).unwrap();
    drop(store);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE objects SET data=zeroblob(total)", []).unwrap();
    drop(db);
    let before = std::fs::read(&path).unwrap();
    assert!(Store::open(&path, StoreOptions::default(), 0).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
