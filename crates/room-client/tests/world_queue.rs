#![cfg(not(target_arch = "wasm32"))]
use photocraft_room_client::world_queue::ViewerState;
use photocraft_room_client::world_queue::{RecoveryReason, WorldOutbox, WorldScope};
use photocraft_scene::{
    CanvasId, OwnerId,
    shared::{AcceptedOp, DeviceId, Edit, WorldState},
};

fn canvas() -> CanvasId {
    CanvasId::new(&format!("{:032x}", 100)).unwrap()
}
fn scope(profile: u64, device: u64, endpoint: &str) -> WorldScope {
    WorldScope::new(endpoint, OwnerId::new(&format!("{profile:032x}")).unwrap(), DeviceId::new(&format!("{device:032x}")).unwrap()).unwrap()
}
fn edit(name: &str) -> Edit {
    Edit::Rename { name: name.into(), expected_revision: 0 }
}

#[test]
fn imported_snapshot_keeps_origin_in_offline_directory_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("imports.sqlite3");
    let account = scope(2, 20, "https://example.invalid");
    let source = WorldState::new(canvas(), OwnerId::new(&format!("{:032x}", 1)).unwrap(), "Historical".into()).unwrap();
    let imported = source.import_to(CanvasId::new(&format!("{:032x}", 200)).unwrap(), account.profile).unwrap();
    {
        let mut queue = WorldOutbox::open(&path).unwrap();
        let lease = queue.activate(account.clone()).unwrap();
        queue.grant(&lease, imported.canvas, 1, 1).unwrap();
        queue.save_snapshot(&lease, &imported).unwrap();
    }
    let mut queue = WorldOutbox::open(&path).unwrap();
    let lease = queue.activate(account).unwrap();
    let summaries = queue.cached_summaries(&lease, None, 128).unwrap();
    assert_eq!(summaries.items.len(), 1);
    assert_eq!(summaries.items[0].import_origin, imported.import_origin);
    let cached = queue.load_snapshot(&lease, imported.canvas).unwrap().unwrap();
    assert_eq!(cached.canonical_hash().unwrap(), imported.canonical_hash().unwrap());
    let other = queue.activate(scope(3, 30, "https://example.invalid")).unwrap();
    assert!(queue.cached_summaries(&other, None, 128).unwrap().items.is_empty());
}

#[test]
fn partial_import_cache_survives_restart_and_completes_after_reconnect() {
    use photocraft_room_client::world_replica::WorldReplica;
    use photocraft_scene::shared::{OpId, WorldOp};
    use photocraft_scene::world_protocol::{CanvasSummary, Response};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("partial-import.sqlite3");
    let account = scope(2, 20, "https://example.invalid");
    let mut source = WorldState::new(canvas(), account.profile, "Start".into()).unwrap();
    for n in 1..=2 {
        source
            .apply(&AcceptedOp {
                revision: n,
                op: WorldOp {
                    id: OpId::new(&format!("{n:032x}")).unwrap(),
                    canvas: source.canvas,
                    author: source.owner,
                    device: account.device,
                    sequence: n,
                    authority_epoch: 1,
                    action: Edit::Rename { name: format!("Title {n}"), expected_revision: n - 1 },
                },
            })
            .unwrap();
    }
    let imported = source.import_to(CanvasId::new(&format!("{:032x}", 200)).unwrap(), account.profile).unwrap();
    let partial = imported.replay_to(1).unwrap();
    {
        let mut queue = WorldOutbox::open(&path).unwrap();
        let lease = queue.activate(account.clone()).unwrap();
        queue.grant(&lease, imported.canvas, 1, 1).unwrap();
        queue.save_snapshot(&lease, &partial).unwrap();
    }
    let mut queue = WorldOutbox::open(&path).unwrap();
    let lease = queue.activate(account).unwrap();
    let error = queue.cached_summaries(&lease, None, 128).unwrap_err().to_string();
    assert!(error.contains("incomplete") && error.contains("preserved"));
    let retained = queue.load_snapshot(&lease, imported.canvas).unwrap().unwrap();
    assert_eq!(retained.canonical_hash().unwrap(), partial.canonical_hash().unwrap());
    let summary = CanvasSummary {
        canvas: imported.canvas,
        owner: imported.owner,
        name: imported.name.clone(),
        initial_name: imported.replay_to(0).unwrap().name,
        import_origin: imported.import_origin.clone(),
        revision: imported.revision,
        epoch: 1,
    };
    let mut replica = WorldReplica::open(summary, Some(retained)).unwrap();
    replica.apply_page(&Response::Events { events: imported.events_after(1, 128).unwrap().to_vec(), through: imported.revision, next: None }).unwrap();
    assert!(replica.complete());
    queue.save_snapshot(&lease, replica.accepted()).unwrap();
    assert_eq!(queue.cached_summaries(&lease, None, 128).unwrap().items.len(), 1);
    assert_eq!(replica.accepted().canonical_hash().unwrap(), imported.canonical_hash().unwrap());
}

#[test]
fn account_switch_and_late_response_never_consume_another_accounts_queue_or_cache() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("worlds.sqlite3");
    let mut queue = WorldOutbox::open(&path).unwrap();
    let a = scope(1, 10, "https://example.invalid");
    let b = scope(2, 20, "https://example.invalid");
    let first = queue.activate(a.clone()).unwrap();
    queue.grant(&first, canvas(), 1, 1).unwrap();
    let op = queue.enqueue(&first, canvas(), edit("A pending")).unwrap();
    let pending = queue.pending(&first, 10).unwrap().remove(0);
    let state = WorldState::new(canvas(), a.profile, "A private".into()).unwrap();
    queue.save_snapshot(&first, &state).unwrap();
    let second = queue.activate(b).unwrap();
    queue.grant(&second, canvas(), 1, 1).unwrap();
    let op_b = queue.enqueue(&second, canvas(), edit("B pending")).unwrap();
    assert!(queue.pending(&first, 10).is_err());
    assert!(queue.ensure_current(&first).is_err());
    assert!(queue.load_snapshot(&first, canvas()).is_err());
    assert!(queue.load_snapshot(&second, canvas()).unwrap().is_none());
    assert!(queue.acknowledge(&pending, &AcceptedOp { revision: 1, op: op.clone() }).is_err());
    assert_eq!(queue.pending(&second, 10).unwrap()[0].operation, op_b);
    let first_again = queue.activate(a).unwrap();
    assert_eq!(queue.pending(&first_again, 10).unwrap()[0].operation, op);
    assert_eq!(queue.load_snapshot(&first_again, canvas()).unwrap().unwrap().name, "A private");
    let refreshed = queue.pending(&first_again, 10).unwrap().remove(0);
    let mut saved = queue.load_snapshot(&first_again, canvas()).unwrap().unwrap();
    saved.apply(&AcceptedOp { revision: 1, op: op.clone() }).unwrap();
    queue.save_snapshot(&first_again, &saved).unwrap();
    queue.acknowledge(&refreshed, &AcceptedOp { revision: 1, op }).unwrap();
    assert!(queue.pending(&first_again, 10).unwrap().is_empty());
}

#[test]
fn endpoint_and_writer_scopes_are_independent_even_for_same_profile() {
    let root = tempfile::tempdir().unwrap();
    let mut queue = WorldOutbox::open(root.path().join("queue.sqlite3")).unwrap();
    let scopes = [scope(1, 10, "https://one.invalid"), scope(1, 11, "https://one.invalid"), scope(1, 10, "https://two.invalid")];
    let mut expected = Vec::new();
    for (n, scope) in scopes.iter().enumerate() {
        let active = queue.activate(scope.clone()).unwrap();
        queue.grant(&active, canvas(), 1, 1).unwrap();
        expected.push(queue.enqueue(&active, canvas(), edit(&format!("private {n}"))).unwrap());
    }
    for (scope, op) in scopes.into_iter().zip(expected) {
        let active = queue.activate(scope).unwrap();
        let pending = queue.pending(&active, 10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].operation, op);
    }
    for endpoint in ["https://user:secret@example.invalid", "https://example.invalid?token=secret", "http://example.invalid"] {
        assert!(WorldScope::new(endpoint, scopes_profile(), DeviceId::new(&format!("{:032x}", 10)).unwrap()).is_err());
    }
}
fn scopes_profile() -> OwnerId {
    OwnerId::new(&format!("{:032x}", 1)).unwrap()
}

#[test]
fn revocation_rejoin_preserves_old_work_without_implicitly_rekeying_or_publishing_it() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("queue.sqlite3");
    let identity = scope(1, 10, "https://example.invalid");
    let mut queue = WorldOutbox::open(&path).unwrap();
    let active = queue.activate(identity.clone()).unwrap();
    queue.grant(&active, canvas(), 1, 1).unwrap();
    let old = queue.enqueue(&active, canvas(), edit("revoked drawing")).unwrap();
    let delayed = queue.pending(&active, 1).unwrap().remove(0);
    assert_eq!(queue.cached_grant(&active, canvas()).unwrap(), Some((1, 1)));
    queue.revoke(&active, canvas()).unwrap();
    assert_eq!(queue.cached_grant(&active, canvas()).unwrap(), None);
    assert!(queue.pending(&active, 1).unwrap().is_empty());
    assert!(queue.grant(&active, canvas(), 1, 1).is_err());
    assert!(queue.enqueue(&active, canvas(), edit("blocked")).is_err());
    assert!(queue.acknowledge(&delayed, &AcceptedOp { revision: 1, op: old.clone() }).is_err());
    drop(queue);
    let mut queue = WorldOutbox::open(&path).unwrap();
    let active = queue.activate(identity).unwrap();
    queue.grant(&active, canvas(), 2, 1).unwrap();
    assert_eq!(queue.cached_grant(&active, canvas()).unwrap(), Some((2, 1)));
    let fresh = queue.enqueue(&active, canvas(), edit("explicit new drawing")).unwrap();
    assert_eq!(fresh.authority_epoch, 2);
    assert_ne!(fresh.id, old.id);
    assert_eq!(fresh.sequence, 1);
    let recovery = queue.recovery(&active, 10).unwrap();
    assert_eq!(recovery.len(), 1);
    assert_eq!(recovery[0].operation, old);
    assert_eq!(recovery[0].reason, RecoveryReason::AccessRemoved);
    assert_eq!(queue.pending(&active, 10).unwrap()[0].operation, fresh);
}

#[test]
fn conflicting_metadata_keeps_dependent_edits_and_restarts_from_canonical_sequence() {
    let root = tempfile::tempdir().unwrap();
    let mut queue = WorldOutbox::open(root.path().join("queue.sqlite3")).unwrap();
    let active = queue.activate(scope(1, 10, "https://example.invalid")).unwrap();
    queue.grant(&active, canvas(), 1, 1).unwrap();
    let first = queue.enqueue(&active, canvas(), edit("conflicting rename")).unwrap();
    let second = queue.enqueue(&active, canvas(), edit("dependent rename")).unwrap();
    assert_eq!(second.sequence, 2);
    let first_lease = queue.pending(&active, 1).unwrap().remove(0);
    queue.reject(&first_lease, RecoveryReason::Conflict).unwrap();
    assert!(queue.pending(&active, 10).unwrap().is_empty());
    queue.grant(&active, canvas(), 1, 1).unwrap();
    assert!(queue.enqueue(&active, canvas(), edit("must not auto resume")).is_err());
    assert!(queue.resume_after_recovery(&active, canvas(), 1, 1).is_err());
    queue.save_snapshot(&active, &WorldState::new(canvas(), active.scope().profile, "Original".into()).unwrap()).unwrap();
    queue.resume_after_recovery(&active, canvas(), 1, 1).unwrap();
    let fresh = queue.enqueue(&active, canvas(), edit("explicit resolution")).unwrap();
    assert_eq!(fresh.sequence, 1);
    assert_ne!(fresh.id, first.id);
    let recovery = queue.recovery(&active, 10).unwrap();
    assert_eq!(recovery.iter().map(|item| item.operation.clone()).collect::<Vec<_>>(), [first, second]);
}

#[test]
fn explicit_recovery_requires_matching_durable_prefix_and_never_restores_revoked_access() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("queue.sqlite3");
    let identity = scope(1, 10, "https://example.invalid");
    let mut queue = WorldOutbox::open(&path).unwrap();
    let lease = queue.activate(identity.clone()).unwrap();
    queue.grant(&lease, canvas(), 1, 1).unwrap();
    let mut canonical = WorldState::new(canvas(), identity.profile, "Original".into()).unwrap();
    queue.save_snapshot(&lease, &canonical).unwrap();
    let rejected = queue.enqueue(&lease, canvas(), edit("retained original")).unwrap();
    let queued = queue.pending(&lease, 1).unwrap().remove(0);
    queue.reject(&queued, RecoveryReason::Conflict).unwrap();
    let mut accepted = rejected.clone();
    accepted.id = photocraft_scene::shared::OpId::new(&format!("{:032x}", 999)).unwrap();
    accepted.action = edit("accepted elsewhere");
    canonical.apply(&AcceptedOp { revision: 1, op: accepted }).unwrap();
    queue.grant(&lease, canvas(), 1, 2).unwrap();
    assert!(queue.resume_after_recovery(&lease, canvas(), 1, 1).is_err());
    assert!(queue.resume_after_recovery(&lease, canvas(), 2, 2).is_err());
    assert!(queue.resume_after_recovery(&lease, canvas(), 1, 2).is_err());
    assert_eq!(queue.cached_grant(&lease, canvas()).unwrap(), None);
    queue.save_snapshot(&lease, &canonical).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TRIGGER fail_resume BEFORE UPDATE OF blocked ON grants WHEN NEW.blocked=0 BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(queue.resume_after_recovery(&lease, canvas(), 1, 2).is_err());
    assert_eq!(queue.cached_grant(&lease, canvas()).unwrap(), None);
    assert_eq!(queue.recovery(&lease, 10).unwrap()[0].operation, rejected);
    db.execute_batch("DROP TRIGGER fail_resume;").unwrap();
    drop(db);
    let other = queue.activate(scope(2, 20, "https://example.invalid")).unwrap();
    assert!(queue.resume_after_recovery(&lease, canvas(), 1, 2).is_err());
    assert!(queue.resume_after_recovery(&other, canvas(), 1, 2).is_err());
    let lease = queue.activate(identity.clone()).unwrap();
    queue.resume_after_recovery(&lease, canvas(), 1, 2).unwrap();
    assert_eq!(queue.cached_grant(&lease, canvas()).unwrap(), Some((1, 2)));
    let next = queue.enqueue(&lease, canvas(), edit("explicit new work")).unwrap();
    assert_eq!(next.sequence, 2);
    assert_ne!(next.id, rejected.id);
    queue.revoke(&lease, canvas()).unwrap();
    assert!(queue.resume_after_recovery(&lease, canvas(), 1, 2).is_err());
    assert!(queue.grant(&lease, canvas(), 1, 2).is_err());
    drop(queue);
    let mut queue = WorldOutbox::open(&path).unwrap();
    let lease = queue.activate(identity).unwrap();
    assert!(queue.resume_after_recovery(&lease, canvas(), 1, 2).is_err());
    assert_eq!(queue.recovery(&lease, 10).unwrap().len(), 2);
}

#[test]
fn exact_ack_required_and_reopen_invalidates_pre_crash_leases() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("queue.sqlite3");
    let identity = scope(1, 10, "https://example.invalid");
    let mut queue = WorldOutbox::open(&path).unwrap();
    let active = queue.activate(identity.clone()).unwrap();
    queue.grant(&active, canvas(), 1, 1).unwrap();
    let op = queue.enqueue(&active, canvas(), edit("durable")).unwrap();
    let lease = queue.pending(&active, 10).unwrap().remove(0);
    let mut mismatch = op.clone();
    mismatch.action = edit("wrong");
    assert!(queue.acknowledge(&lease, &AcceptedOp { revision: 1, op: mismatch }).is_err());
    drop(queue);
    let mut queue = WorldOutbox::open(&path).unwrap();
    assert!(queue.acknowledge(&lease, &AcceptedOp { revision: 1, op: op.clone() }).is_err());
    let fresh = queue.activate(identity).unwrap();
    let pending = queue.pending(&fresh, 10).unwrap();
    assert_eq!(pending[0].operation, op);
    let mut saved = WorldState::new(canvas(), fresh.scope().profile, "Initial".into()).unwrap();
    saved.apply(&AcceptedOp { revision: 1, op: op.clone() }).unwrap();
    queue.save_snapshot(&fresh, &saved).unwrap();
    queue.acknowledge(&pending[0], &AcceptedOp { revision: 1, op }).unwrap();
    assert!(queue.grant(&fresh, canvas(), 1, 1).is_err());
    queue.grant(&fresh, canvas(), 1, 2).unwrap();
    assert_eq!(queue.enqueue(&fresh, canvas(), edit("next")).unwrap().sequence, 2);
}

#[test]
fn checksum_corruption_is_reported_without_discarding_the_original_row() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("queue.sqlite3");
    let identity = scope(1, 10, "https://example.invalid");
    let mut queue = WorldOutbox::open(&path).unwrap();
    let active = queue.activate(identity.clone()).unwrap();
    queue.grant(&active, canvas(), 1, 1).unwrap();
    queue.enqueue(&active, canvas(), edit("recoverable")).unwrap();
    drop(queue);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE operations SET hash='bad'", []).unwrap();
    drop(db);
    let mut queue = WorldOutbox::open(&path).unwrap();
    let active = queue.activate(identity).unwrap();
    assert!(queue.pending(&active, 10).is_err());
    drop(queue);
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(db.query_row::<i64, _, _>("SELECT count(*) FROM operations", [], |r| r.get(0)).unwrap(), 1);
}

#[test]
fn failed_sqlite_commit_does_not_advance_the_writer_or_claim_save_success() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("queue.sqlite3");
    let mut queue = WorldOutbox::open(&path).unwrap();
    let active = queue.activate(scope(1, 10, "https://example.invalid")).unwrap();
    queue.grant(&active, canvas(), 1, 1).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TRIGGER reject_op BEFORE INSERT ON operations BEGIN SELECT RAISE(ABORT,'injected write failure'); END;").unwrap();
    assert!(queue.enqueue(&active, canvas(), edit("not saved")).is_err());
    assert!(queue.pending(&active, 10).unwrap().is_empty());
    db.execute_batch("DROP TRIGGER reject_op;").unwrap();
    assert_eq!(queue.enqueue(&active, canvas(), edit("saved")).unwrap().sequence, 1);
}

#[test]
fn viewer_camera_bookmarks_and_fixed_review_interval_survive_restart_without_marking_downloads_seen() {
    use photocraft_scene::{Bookmark, coordinates, shared::ReviewState};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("queue.sqlite3");
    let identity = scope(1, 10, "https://example.invalid");
    let mut queue = WorldOutbox::open(&path).unwrap();
    let active = queue.activate(identity.clone()).unwrap();
    queue.grant(&active, canvas(), 1, 1).unwrap();
    let mut state = WorldState::new(canvas(), identity.profile, "Original".into()).unwrap();
    queue.save_snapshot(&active, &state).unwrap();
    let mut review = ReviewState::first_visit(canvas(), identity.profile, 0).unwrap();
    let first = queue.enqueue(&active, canvas(), edit("First")).unwrap();
    state.apply(&AcceptedOp { revision: 1, op: first }).unwrap();
    queue.save_snapshot(&active, &state).unwrap();
    review.begin(1).unwrap();
    review.scrub(1).unwrap();
    let camera = coordinates::home(canvas());
    let view = ViewerState { camera: camera.clone(), bookmarks: vec![Bookmark { name: "Home".into(), camera }], review: Some(review) };
    queue.save_view(&active, canvas(), &view).unwrap();
    let second = queue.enqueue(&active, canvas(), Edit::Rename { name: "Second".into(), expected_revision: 1 }).unwrap();
    state.apply(&AcceptedOp { revision: 2, op: second }).unwrap();
    queue.save_snapshot(&active, &state).unwrap();
    assert_eq!(queue.load_view(&active, canvas()).unwrap().unwrap(), view);
    drop(queue);
    let mut queue = WorldOutbox::open(&path).unwrap();
    let active = queue.activate(identity).unwrap();
    let mut restored = queue.load_view(&active, canvas()).unwrap().unwrap();
    assert_eq!(restored, view);
    assert_eq!(restored.review.as_ref().unwrap().reviewed_revision, 0);
    assert_eq!(restored.review.as_ref().unwrap().interval.as_ref().unwrap().end_inclusive, 1);
    restored.review.as_mut().unwrap().mark_seen(2).unwrap();
    queue.save_view(&active, canvas(), &restored).unwrap();
    assert_eq!(restored.review.as_ref().unwrap().unseen_revisions(2).unwrap(), 1);
    let other = queue.activate(scope(2, 20, "https://example.invalid")).unwrap();
    assert!(queue.load_view(&other, canvas()).unwrap().is_none());
    assert!(queue.save_view(&other, canvas(), &restored).is_err());
}

#[test]
fn queue_process_child() {
    let Some(path) = std::env::var_os("K6_QUEUE_PROCESS_CHILD") else { return };
    let path = std::path::PathBuf::from(path);
    let mut queue = WorldOutbox::open(&path).unwrap();
    let active = queue.activate(scope(1, 10, "https://example.invalid")).unwrap();
    queue.grant(&active, canvas(), 1, 1).unwrap();
    queue.enqueue(&active, canvas(), edit("acknowledged before process kill")).unwrap();
    std::fs::write(path.with_extension("ack"), b"durable").unwrap();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[test]
fn acknowledged_operation_survives_process_kill_and_signout_preserves_it() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("queue.sqlite3");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "queue_process_child", "--nocapture"])
        .env("K6_QUEUE_PROCESS_CHILD", &path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    while !path.with_extension("ack").exists() && start.elapsed() < std::time::Duration::from_secs(15) {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let acknowledged = path.with_extension("ack").exists();
    let _ = child.kill();
    child.wait().unwrap();
    assert!(acknowledged, "child never acknowledged its durable save");
    let mut queue = WorldOutbox::open(&path).unwrap();
    let identity = scope(1, 10, "https://example.invalid");
    let active = queue.activate(identity.clone()).unwrap();
    let recovered = queue.pending(&active, 10).unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].operation.sequence, 1);
    assert_eq!(recovered[0].operation.action, edit("acknowledged before process kill"));
    queue.sign_out().unwrap();
    assert!(queue.pending(&active, 10).is_err());
    let active = queue.activate(identity).unwrap();
    assert_eq!(queue.pending(&active, 10).unwrap()[0].operation, recovered[0].operation);
}

#[test]
fn an_accepted_receipt_ahead_of_saved_history_cannot_destroy_the_only_local_copy() {
    use photocraft_scene::shared::{OpId, WorldOp};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("queue.sqlite3");
    let identity = scope(1, 10, "https://example.invalid");
    let mut queue = WorldOutbox::open(&path).unwrap();
    let active = queue.activate(identity.clone()).unwrap();
    queue.grant(&active, canvas(), 1, 1).unwrap();
    let state = WorldState::new(canvas(), identity.profile, "Initial".into()).unwrap();
    queue.save_snapshot(&active, &state).unwrap();
    let op = queue.enqueue(&active, canvas(), Edit::Rename { name: "My offline edit".into(), expected_revision: 1 }).unwrap();
    let receipt = AcceptedOp { revision: 2, op: op.clone() };
    let lease = queue.pending(&active, 10).unwrap().remove(0);
    assert!(queue.acknowledge(&lease, &receipt).is_err());
    drop(queue);
    let mut queue = WorldOutbox::open(&path).unwrap();
    let active = queue.activate(identity.clone()).unwrap();
    let lease = queue.pending(&active, 10).unwrap().remove(0);
    assert_eq!(lease.operation, op);
    assert!(queue.acknowledge(&lease, &receipt).is_err());
    let mut caught_up = state;
    caught_up
        .apply(&AcceptedOp {
            revision: 1,
            op: WorldOp {
                id: OpId::new(&format!("{:032x}", 999)).unwrap(),
                canvas: canvas(),
                author: identity.profile,
                device: DeviceId::new(&format!("{:032x}", 11)).unwrap(),
                sequence: 1,
                authority_epoch: 1,
                action: edit("Other device"),
            },
        })
        .unwrap();
    caught_up.apply(&receipt).unwrap();
    queue.save_snapshot(&active, &caught_up).unwrap();
    queue.acknowledge(&lease, &receipt).unwrap();
    assert!(queue.pending(&active, 10).unwrap().is_empty());
}

#[test]
fn snapshots_cannot_overwrite_same_revision_or_extend_a_divergent_prefix() {
    let root = tempfile::tempdir().unwrap();
    let mut queue = WorldOutbox::open(root.path().join("queue.sqlite3")).unwrap();
    let identity = scope(1, 10, "https://example.invalid");
    let active = queue.activate(identity.clone()).unwrap();
    queue.grant(&active, canvas(), 1, 1).unwrap();
    let op = queue.enqueue(&active, canvas(), edit("original")).unwrap();
    let mut saved = WorldState::new(canvas(), identity.profile, "Initial".into()).unwrap();
    saved.apply(&AcceptedOp { revision: 1, op: op.clone() }).unwrap();
    queue.save_snapshot(&active, &saved).unwrap();
    let mut changed = op;
    changed.action = edit("divergent");
    let mut divergent = WorldState::new(canvas(), identity.profile, "Initial".into()).unwrap();
    divergent.apply(&AcceptedOp { revision: 1, op: changed }).unwrap();
    assert!(queue.save_snapshot(&active, &divergent).is_err());
    let second = queue.enqueue(&active, canvas(), Edit::Rename { name: "later but conflicting".into(), expected_revision: 1 }).unwrap();
    divergent.apply(&AcceptedOp { revision: 2, op: second }).unwrap();
    assert!(queue.save_snapshot(&active, &divergent).is_err());
    assert_eq!(queue.load_snapshot(&active, canvas()).unwrap().unwrap().canonical_hash().unwrap(), saved.canonical_hash().unwrap());
}

#[test]
fn every_recovery_operation_is_accessible_through_bounded_cursor_pages() {
    let root = tempfile::tempdir().unwrap();
    let mut queue = WorldOutbox::open(root.path().join("queue.sqlite3")).unwrap();
    let active = queue.activate(scope(1, 10, "https://example.invalid")).unwrap();
    queue.grant(&active, canvas(), 1, 1).unwrap();
    let mut expected = std::collections::BTreeSet::new();
    for n in 0..260 {
        expected.insert(queue.enqueue(&active, canvas(), edit(&format!("retained {n}"))).unwrap().id);
    }
    let mut pending_cursor = None;
    let mut pending_sequence = Vec::new();
    let mut pending_ids = std::collections::BTreeSet::new();
    loop {
        let page = queue.pending_page(&active, pending_cursor, 128).unwrap();
        assert!(page.items.len() <= 128);
        for item in page.items {
            pending_sequence.push(item.operation.sequence);
            assert!(pending_ids.insert(item.operation.id));
        }
        pending_cursor = page.next;
        if pending_cursor.is_none() {
            break;
        }
    }
    assert_eq!(pending_sequence, (1..=260).collect::<Vec<_>>());
    assert_eq!(pending_ids, expected);
    queue.revoke(&active, canvas()).unwrap();
    let mut seen = std::collections::BTreeSet::new();
    let mut cursor = None;
    loop {
        let page = queue.recovery_page(&active, cursor, 128).unwrap();
        assert!(page.items.len() <= 128);
        for item in page.items {
            assert!(seen.insert(item.operation.id));
            assert_eq!(item.reason, RecoveryReason::AccessRemoved);
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen, expected);
    assert!(queue.recovery_page(&active, None, 257).is_err());
}

#[test]
fn cached_directory_reopens_scoped_and_paginated_without_revoked_grants() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("queue.sqlite3");
    let identity = scope(1, 10, "https://example.invalid");
    let mut queue = WorldOutbox::open(&path).unwrap();
    let lease = queue.activate(identity.clone()).unwrap();
    for number in 1..=130 {
        let canvas = CanvasId::new(&format!("{number:032x}")).unwrap();
        queue.grant(&lease, canvas, 1, 1).unwrap();
        queue.save_snapshot(&lease, &WorldState::new(canvas, identity.profile, format!("Saved {number}")).unwrap()).unwrap();
    }
    let revoked = CanvasId::new(&format!("{:032x}", 2)).unwrap();
    queue.revoke(&lease, revoked).unwrap();
    let conflicted = CanvasId::new(&format!("{:032x}", 1)).unwrap();
    queue.enqueue(&lease, conflicted, edit("Conflicting original")).unwrap();
    let pending = queue.pending(&lease, 1).unwrap().remove(0);
    queue.reject(&pending, RecoveryReason::Conflict).unwrap();
    drop(queue);
    let mut queue = WorldOutbox::open(&path).unwrap();
    let lease = queue.activate(identity.clone()).unwrap();
    let first = queue.cached_summaries(&lease, None, 128).unwrap();
    assert_eq!(first.items.len(), 128);
    assert!(first.items.iter().any(|item| item.canvas == conflicted));
    assert!(first.items.iter().all(|item| item.canvas != revoked));
    let last = queue.cached_summaries(&lease, first.next, 128).unwrap();
    assert_eq!(last.items.len(), 1);
    assert!(last.next.is_none());
    assert_eq!(last.items[0].name, "Saved 130");
    assert!(queue.cached_summaries(&lease, None, 129).is_err());
    let other = queue.activate(scope(2, 20, "https://example.invalid")).unwrap();
    assert!(queue.cached_summaries(&other, None, 128).unwrap().items.is_empty());
    assert!(queue.cached_summaries(&lease, None, 128).is_err());
    let lease = queue.activate(identity).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE snapshots SET hash='corrupt' WHERE canvas=?1", [conflicted.as_str()]).unwrap();
    assert!(queue.cached_summaries(&lease, None, 128).unwrap_err().to_string().contains("checksum"));
    assert_eq!(db.query_row("SELECT count(*) FROM snapshots", [], |row| row.get::<_, u32>(0)).unwrap(), 130);
}
