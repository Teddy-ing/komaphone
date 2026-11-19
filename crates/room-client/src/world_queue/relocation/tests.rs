use super::*;
use crate::world_queue::{Edit, WorldOp};
use crate::world_worker::WorldAccess;
use std::io::{Read, Write};
use std::net::TcpListener;

fn proof(profile: OwnerId, device: DeviceId) -> (WorldScope, VerifiedWorldIdentity) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0u8; 4096];
        let _ = stream.read(&mut buffer).unwrap();
        let body = serde_json::to_string(&wire::Response::Identity { profile, device }).unwrap();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    let scope = WorldScope::new(&endpoint, profile, device).unwrap();
    let verified = WorldAccess::for_relocation(scope.clone(), "a".repeat(64)).unwrap().verify_identity(None).unwrap();
    server.join().unwrap();
    (scope, verified)
}

fn fixture() -> (tempfile::TempDir, WorldOutbox, AccountRelocation, VerifiedWorldIdentity, Activation, CanvasId, WorldOp, ViewerState) {
    let temp = tempfile::tempdir().unwrap();
    let mut queue = WorldOutbox::open(temp.path().join("world.db")).unwrap();
    let profile = OwnerId::new(&"1".repeat(32)).unwrap();
    let device = DeviceId::new(&"2".repeat(32)).unwrap();
    let source = WorldScope::new("https://original.example", profile, device).unwrap();
    let (target, verified) = proof(profile, device);
    let plan = AccountRelocation {
        id: photocraft_party::RequestId::new(&"3".repeat(32)).unwrap(),
        from_endpoint: source.endpoint.clone(),
        to_endpoint: target.endpoint,
        profile: photocraft_party::ParticipantId::new(profile.as_str()).unwrap(),
        device: photocraft_party::DeviceId::new(device.as_str()).unwrap(),
    };
    let lease = queue.activate(source).unwrap();
    let canvas = CanvasId::new(&"4".repeat(32)).unwrap();
    queue.grant(&lease, canvas, 1, 1).unwrap();
    queue.save_snapshot(&lease, &WorldState::new(canvas, profile, "Original".into()).unwrap()).unwrap();
    let viewer = ViewerState {
        camera: photocraft_scene::coordinates::home(canvas),
        bookmarks: Vec::new(),
        review: Some(photocraft_scene::shared::ReviewState::first_visit(canvas, profile, 0).unwrap()),
    };
    queue.save_view(&lease, canvas, &viewer).unwrap();
    let operation = queue.enqueue(&lease, canvas, Edit::Rename { name: "Pending".into(), expected_revision: 0 }).unwrap();
    (temp, queue, plan, verified, lease, canvas, operation, viewer)
}

#[test]
fn moves_exact_operations_sequences_snapshots_review_and_rejects_retired_source_after_restart() {
    let (temp, mut queue, plan, verified, lease, canvas, operation, viewer) = fixture();
    queue.relocate_endpoint(&plan, &verified).unwrap();
    assert!(queue.ensure_current(&lease).is_err());
    assert!(queue.activate(lease.scope.clone()).is_err());
    assert_eq!(queue.db.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0)).unwrap(), 3);
    drop(queue);
    let mut queue = WorldOutbox::open(temp.path().join("world.db")).unwrap();
    queue.relocate_endpoint(&plan, &verified).unwrap();
    assert!(queue.activate(lease.scope).is_err());
    let moved = queue.activate(verified.scope().clone()).unwrap();
    assert_eq!(queue.pending(&moved, 10).unwrap()[0].operation, operation);
    assert_eq!(queue.load_view(&moved, canvas).unwrap(), Some(viewer));
    assert_eq!(queue.load_snapshot(&moved, canvas).unwrap().unwrap().name, "Original");
    let next = queue.enqueue(&moved, canvas, Edit::Rename { name: "Next".into(), expected_revision: 0 }).unwrap();
    assert_eq!(next.sequence, operation.sequence + 1);
    assert_eq!(
        std::fs::read_dir(temp.path()).unwrap().filter(|entry| entry.as_ref().unwrap().file_name().to_string_lossy().contains("pre-relocation-")).count(),
        1
    );
}

#[test]
fn migration_preserves_revoked_recovery_without_republishing() {
    let (_temp, mut queue, plan, verified, lease, canvas, operation, _) = fixture();
    queue.revoke(&lease, canvas).unwrap();
    queue.relocate_endpoint(&plan, &verified).unwrap();
    let moved = queue.activate(verified.scope().clone()).unwrap();
    assert!(queue.pending(&moved, 10).unwrap().is_empty());
    assert_eq!(queue.recovery(&moved, 10).unwrap()[0].operation, operation);
    assert!(queue.cached_grant(&moved, canvas).unwrap().is_none());
    assert!(queue.enqueue(&moved, canvas, Edit::Rename { name: "Blocked".into(), expected_revision: 0 }).is_err());
}

#[test]
fn target_collision_wrong_identity_and_corruption_leave_source_unchanged() {
    let (_temp, mut queue, plan, verified, lease, canvas, operation, _) = fixture();
    let mut wrong = plan.clone();
    wrong.profile = photocraft_party::ParticipantId::new(&"9".repeat(32)).unwrap();
    assert!(queue.relocate_endpoint(&wrong, &verified).is_err());
    let target = queue.activate(verified.scope().clone()).unwrap();
    queue.grant(&target, canvas, 1, 1).unwrap();
    assert!(queue.relocate_endpoint(&plan, &verified).is_err());
    queue.db.execute("DELETE FROM grants WHERE scope=?1", [&target.key]).unwrap();
    queue.db.execute("UPDATE operations SET hash='invalid' WHERE scope=?1", [&lease.key]).unwrap();
    assert!(queue.relocate_endpoint(&plan, &verified).is_err());
    let saved: Vec<u8> =
        queue.db.query_row("SELECT body FROM operations WHERE scope=?1 AND id=?2", params![lease.key, operation.id.as_str()], |row| row.get(0)).unwrap();
    assert_eq!(serde_json::from_slice::<WorldOp>(&saved).unwrap(), operation);
    assert_eq!(queue.db.query_row("SELECT count(*) FROM scope_moves", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn failed_commit_rolls_back_rekey_and_retry_uses_one_verified_backup() {
    let (temp, mut queue, plan, verified, lease, canvas, operation, _) = fixture();
    // Abort only the final receipt insert, after every scope table was changed.
    queue.db.execute_batch("CREATE TRIGGER fail_move BEFORE INSERT ON scope_moves BEGIN SELECT RAISE(ABORT,'synthetic receipt failure'); END;").unwrap();
    assert!(queue.relocate_endpoint(&plan, &verified).is_err());
    assert_eq!(queue.pending(&lease, 10).unwrap()[0].operation, operation);
    assert!(queue.load_snapshot(&lease, canvas).unwrap().is_some());
    queue.db.execute_batch("DROP TRIGGER fail_move;").unwrap();
    queue.relocate_endpoint(&plan, &verified).unwrap();
    assert_eq!(
        std::fs::read_dir(temp.path()).unwrap().filter(|entry| entry.as_ref().unwrap().file_name().to_string_lossy().contains("pre-relocation-")).count(),
        1
    );
}

#[test]
fn old_schema_gets_readable_backup_before_version_two_retirement_rules() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("old.db");
    let queue = WorldOutbox::open(&path).unwrap();
    queue.db.execute_batch("DROP TABLE scope_moves; PRAGMA user_version=1;").unwrap();
    drop(queue);
    let queue = WorldOutbox::open(&path).unwrap();
    assert_eq!(queue.db.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0)).unwrap(), 3);
    let backup = Connection::open(path.with_file_name("old.db.pre-relocation-schema-v1.sqlite3")).unwrap();
    assert_eq!(backup.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0)).unwrap(), 1);
    drop(queue);
    let path2 = temp.path().join("old2.db");
    let queue2 = WorldOutbox::open(&path2).unwrap();
    queue2.db.execute_batch("PRAGMA user_version=2;").unwrap();
    drop(queue2);
    let queue2 = WorldOutbox::open(&path2).unwrap();
    assert_eq!(queue2.db.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0)).unwrap(), 3);
    let backup2 = Connection::open(path2.with_file_name("old2.db.pre-brush-schema-v2.sqlite3")).unwrap();
    assert_eq!(backup2.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0)).unwrap(), 2);
}
