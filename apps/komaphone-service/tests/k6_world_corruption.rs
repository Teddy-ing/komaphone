//! Corrupt SQLite data must fail bounded reads and preserve acknowledged history.
use photocraft_party::{
    DeviceId as PartyDevice, Participant, ParticipantId, RequestId,
    protocol::{Operation, RpcRequest},
};
use photocraft_scene::shared::{DeviceId, Edit, OpId, WorldOp};
use photocraft_scene::world_protocol::{Action, Request, Response, VERSION};
use photocraft_scene::{CanvasId, OwnerId};
use photocraft_sync::{Store, StoreOptions};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

fn hex(n: u64) -> String {
    format!("{n:032x}")
}
fn token(n: u64) -> String {
    format!("{:064x}", n + 1000)
}
fn canvas() -> CanvasId {
    CanvasId::new(&hex(100)).unwrap()
}
fn request(action: Action) -> Request {
    Request { version: VERSION, action }
}
fn call(store: &mut Store, actor: u64, action: Action) -> Response {
    store.world_execute(&token(actor), &request(action), 0).unwrap()
}
fn fixture() -> (tempfile::TempDir, Store) {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path().join("service.sqlite3"), StoreOptions::default(), 0).unwrap();
    for n in 1..=2 {
        store
            .execute(
                None,
                &RpcRequest {
                    protocol_version: 1,
                    request_id: RequestId::new(&hex(n)).unwrap(),
                    operation: Operation::Pair {
                        participant: Participant {
                            id: ParticipantId::new(&hex(n)).unwrap(),
                            device: PartyDevice::new(&hex(n + 100)).unwrap(),
                            display_name: format!("Synthetic {n}"),
                        },
                        credential: token(n),
                    },
                },
                0,
            )
            .unwrap();
    }
    call(&mut store, 1, Action::Create { canvas: canvas(), name: "Original".into() });
    call(
        &mut store,
        1,
        Action::Invite {
            canvas: canvas(),
            id: OpId::new(&hex(50)).unwrap(),
            recipient: OwnerId::new(&hex(2)).unwrap(),
            secret: token(50),
            expires_at_ms: 60_000,
        },
    );
    call(&mut store, 2, Action::Accept { canvas: canvas(), id: OpId::new(&hex(50)).unwrap(), secret: token(50) });
    for sequence in 1..=2 {
        call(
            &mut store,
            1,
            Action::Append {
                operation: WorldOp {
                    id: OpId::new(&hex(500 + sequence)).unwrap(),
                    canvas: canvas(),
                    author: OwnerId::new(&hex(1)).unwrap(),
                    device: DeviceId::new(&hex(101)).unwrap(),
                    sequence,
                    authority_epoch: 1,
                    action: Edit::Rename { name: format!("Saved {sequence}"), expected_revision: sequence - 1 },
                },
            },
        );
    }
    (directory, store)
}
fn page() -> Action {
    Action::Events { canvas: canvas(), after: 0, through: None, limit: 128 }
}

#[test]
fn oversized_event_blob_is_rejected_by_live_reads_backup_and_reopen() {
    let (directory, mut store) = fixture();
    let path = directory.path().join("service.sqlite3");
    let db = Connection::open(&path).unwrap();
    // SQLite creates this without an equally large fixture-side allocation. SQL
    // projections must cap the BLOB before Vec materialization in live/verify reads.
    db.execute("UPDATE world_events SET body=zeroblob(67108864) WHERE revision=1", []).unwrap();
    assert!(store.world_execute(&token(1), &request(page()), 0).is_err());
    let backup = directory.path().join("must-not-publish.sqlite3");
    assert!(store.backup(&backup).is_err());
    assert!(!backup.exists());
    assert_eq!(db.query_row::<i64, _, _>("SELECT length(body) FROM world_events WHERE revision=1", [], |row| row.get(0)).unwrap(), 67_108_864);
    drop(db);
    drop(store);
    assert!(Store::open(&path, StoreOptions::default(), 0).is_err());
    assert!(path.exists());
}

#[test]
fn missing_or_misbound_events_fail_without_nonadvancing_catchup_pages() {
    for damage in ["gap", "tail", "canvas", "revision", "id"] {
        let (directory, mut store) = fixture();
        let db = Connection::open(directory.path().join("service.sqlite3")).unwrap();
        if damage == "gap" || damage == "tail" {
            db.execute("DELETE FROM world_events WHERE revision=?1", [if damage == "gap" { 1 } else { 2 }]).unwrap();
        } else {
            let bytes: Vec<u8> = db.query_row("SELECT body FROM world_events WHERE revision=1", [], |row| row.get(0)).unwrap();
            let mut event: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            match damage {
                "canvas" => event["op"]["canvas"] = hex(999).into(),
                "revision" => event["revision"] = 2.into(),
                "id" => event["op"]["id"] = hex(888).into(),
                _ => panic!("fixture"),
            }
            let bytes = serde_json::to_vec(&event).unwrap();
            db.execute("UPDATE world_events SET body=?1,hash=?2 WHERE revision=1", params![bytes, format!("{:x}", Sha256::digest(&bytes))]).unwrap();
        }
        assert!(store.world_execute(&token(1), &request(page()), 0).is_err(), "{damage}");
        assert!(store.backup(directory.path().join("bad.sqlite3")).is_err(), "{damage}");
    }
}

#[test]
fn oversized_or_nul_suffixed_metadata_is_rejected_and_initial_seed_is_verified() {
    for damage in ["name-size", "name-nul", "initial-seed", "state-hash-nul", "event-hash-nul", "schema-rows", "orphan-event"] {
        let (directory, mut store) = fixture();
        let path = directory.path().join("service.sqlite3");
        let db = Connection::open(&path).unwrap();
        match damage {
            "name-size" => {
                db.execute("UPDATE worlds SET name=?1", ["x".repeat(2 * 1024 * 1024)]).unwrap();
            }
            "name-nul" => {
                db.execute("UPDATE worlds SET name=?1", ["Saved 2\0hidden"]).unwrap();
            }
            "initial-seed" => {
                db.execute("UPDATE worlds SET initial_name='Wrong seed'", []).unwrap();
            }
            "state-hash-nul" => {
                db.execute("UPDATE worlds SET hash=hash||char(0)||'hidden'", []).unwrap();
            }
            "event-hash-nul" => {
                db.execute("UPDATE world_events SET hash=hash||char(0)||'hidden' WHERE revision=1", []).unwrap();
            }
            "schema-rows" => {
                db.execute("INSERT INTO world_schema SELECT version FROM world_schema LIMIT 1", []).unwrap();
            }
            "orphan-event" => {
                db.pragma_update(None, "foreign_keys", false).unwrap();
                db.execute("INSERT INTO world_events SELECT ?1,revision,id,body,hash FROM world_events WHERE revision=1", [hex(999)]).unwrap();
            }
            _ => panic!("fixture"),
        }
        if damage.starts_with("name-") {
            assert!(store.world_execute(&token(1), &request(Action::Info { canvas: canvas() }), 0).is_err(), "{damage}");
        }
        assert!(store.backup(directory.path().join("bad.sqlite3")).is_err(), "{damage}");
        drop(db);
        drop(store);
        assert!(Store::open(&path, StoreOptions::default(), 0).is_err(), "{damage}");
    }
}

#[test]
fn corrupt_invitation_status_never_reactivates_a_revoked_member() {
    let (directory, mut store) = fixture();
    call(&mut store, 1, Action::Remove { canvas: canvas(), member: OwnerId::new(&hex(2)).unwrap() });
    let db = Connection::open(directory.path().join("service.sqlite3")).unwrap();
    db.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE world_invites SET status=3;").unwrap();
    assert!(store.world_execute(&token(2), &request(Action::Accept { canvas: canvas(), id: OpId::new(&hex(50)).unwrap(), secret: token(50) }), 0).is_err());
    assert_eq!(db.query_row::<i64, _, _>("SELECT active FROM world_members WHERE profile=?1", [hex(2)], |row| row.get(0)).unwrap(), 0);
    assert!(store.backup(directory.path().join("bad.sqlite3")).is_err());
}

#[test]
fn exhausted_membership_epoch_rejects_revocation_without_promoting_sqlite_integer_to_real() {
    let (directory, mut store) = fixture();
    let path = directory.path().join("service.sqlite3");
    let db = Connection::open(&path).unwrap();
    db.execute("UPDATE world_members SET epoch=?1 WHERE profile=?2", params![i64::MAX, hex(2)]).unwrap();
    let result = store.world_execute(&token(1), &request(Action::Remove { canvas: canvas(), member: OwnerId::new(&hex(2)).unwrap() }), 0);
    assert!(result.unwrap_err().to_string().contains("epoch"));
    let row: (i64, i64, String) = db
        .query_row("SELECT epoch,active,typeof(epoch) FROM world_members WHERE profile=?1", [hex(2)], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap();
    assert_eq!(row, (i64::MAX, 1, "integer".into()));
    let Response::Canvas { info } = call(&mut store, 2, Action::Info { canvas: canvas() }) else { panic!("info") };
    assert_eq!(info.epoch, i64::MAX as u64);
    drop(db);
    drop(store);
    assert!(Store::open(&path, StoreOptions::default(), 0).is_ok());
}
