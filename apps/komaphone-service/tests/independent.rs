//! Independent authentication, durability and recovery tests against the real SQLite authority.
use std::{
    io::{BufRead, Write},
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

use photocraft_party::{
    DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, RequestId, RoomId,
    protocol::{Operation, PROTOCOL_VERSION, RpcRequest, RpcResponse},
    room::{Action, Phase, RoomView},
};
use photocraft_sync::{SaveStage, Store, StoreOptions};

fn person(n: u16) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{n:032x}")).unwrap(),
        device: DeviceId::new(&format!("{:032x}", n + 100)).unwrap(),
        display_name: format!("Player {n}"),
    }
}

fn credential(n: u16) -> String {
    format!("{:064x}", n + 1000)
}
fn request(id: u64, operation: Operation) -> RpcRequest {
    RpcRequest { protocol_version: PROTOCOL_VERSION, request_id: RequestId::new(&format!("{id:032x}")).unwrap(), operation }
}
fn options() -> StoreOptions {
    StoreOptions { heartbeat_timeout_ms: 60_000, ..StoreOptions::default() }
}
fn open(path: &Path, now_ms: u64) -> Store {
    Store::open(path, options(), now_ms).unwrap()
}
fn view(response: RpcResponse) -> RoomView {
    match response {
        RpcResponse::Room { view, .. } => *view,
        other => panic!("Expected room, received {other:?}"),
    }
}
fn outcome(response: &RpcResponse) -> photocraft_party::room::ApplyOutcome {
    match response {
        RpcResponse::Room { outcome: Some(outcome), .. } => *outcome,
        other => panic!("Expected receipt, received {other:?}"),
    }
}
fn pair(store: &mut Store, n: u16, now_ms: u64) {
    let response = store.execute(None, &request(u64::from(n), Operation::Pair { participant: person(n), credential: credential(n) }), now_ms).unwrap();
    assert_eq!(response, RpcResponse::Paired { participant: person(n) });
}
fn poll(store: &mut Store, n: u16, room: RoomId, id: u64, now_ms: u64) -> RoomView {
    view(store.execute(Some(&credential(n)), &request(id, Operation::Poll { room, after_revision: 0 }), now_ms).unwrap())
}
fn create(store: &mut Store, timer_ms: Option<u64>) -> RoomView {
    view(store.execute(Some(&credential(1)), &request(100, Operation::Create { mode: Mode::Animation { rotations: 1 }, timer_ms }), 0).unwrap())
}
fn apply(store: &mut Store, n: u16, room: RoomId, id: u64, action: Action, now_ms: u64) -> RoomView {
    let before = poll(store, n, room, id + 10_000, now_ms);
    view(
        store
            .execute(Some(&credential(n)), &request(id, Operation::Apply { room, expected_revision: before.revision, action: Box::new(action) }), now_ms)
            .unwrap(),
    )
}
fn invite(store: &mut Store, room: RoomId, id: u64, uses: u8, expires_in_ms: u64) -> RpcRequest {
    let message = request(id, Operation::Invite { room, secret: credential(50), expires_in_ms, uses });
    store.execute(Some(&credential(1)), &message, 0).unwrap();
    message
}
fn join(store: &mut Store, n: u16, room: RoomId, id: u64) -> RpcResponse {
    store.execute(Some(&credential(n)), &request(id, Operation::Join { room, invitation: credential(50) }), 0).unwrap()
}

#[test]
fn credentials_cannot_rebind_an_existing_participant_or_device() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = open(&directory.path().join("service.sqlite3"), 0);
    pair(&mut store, 1, 0);
    let stolen = request(20, Operation::Pair { participant: person(1), credential: credential(2) });
    assert!(store.execute(None, &stolen, 0).is_err());
    let mut cloned_device = person(2);
    cloned_device.device = person(1).device;
    assert!(store.execute(None, &request(21, Operation::Pair { participant: cloned_device, credential: credential(3) }), 0).is_err());
    assert!(store.execute(None, &request(22, Operation::Pair { participant: person(3), credential: credential(1) }), 0).is_err());
    let create_request = request(23, Operation::Create { mode: Mode::ExquisiteCorpse, timer_ms: None });
    assert!(store.execute(None, &create_request, 0).is_err());
    assert!(store.execute(Some(&credential(2)), &create_request, 0).is_err());
    assert!(store.execute(Some(&credential(1)), &create_request, 0).is_ok());
}

#[test]
fn last_invite_use_retries_without_redeeming_twice_and_outsider_has_no_room_access() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = open(&directory.path().join("service.sqlite3"), 0);
    for n in 1..=4 {
        pair(&mut store, n, 0);
    }
    let room = create(&mut store, None).id;
    invite(&mut store, room, 110, 2, 60_000);
    let first = join(&mut store, 2, room, 120);
    assert_eq!(join(&mut store, 2, room, 120), first);
    let last = join(&mut store, 3, room, 121);
    assert_eq!(join(&mut store, 3, room, 121), last);
    assert!(store.execute(Some(&credential(4)), &request(122, Operation::Join { room, invitation: credential(50) }), 0).is_err());
    assert!(store.execute(Some(&credential(4)), &request(123, Operation::Poll { room, after_revision: 0 }), 0).is_err());
    assert_eq!(poll(&mut store, 1, room, 124, 0).members.len(), 3);
}

#[test]
fn expired_and_revoked_capabilities_never_add_members() {
    for revoke in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut store = open(&directory.path().join("service.sqlite3"), 0);
        pair(&mut store, 1, 0);
        pair(&mut store, 2, 0);
        let room = create(&mut store, None).id;
        let invitation = invite(&mut store, room, 110, 1, 1000);
        if revoke {
            store.execute(Some(&credential(1)), &request(111, Operation::RevokeInvite { room, invitation_id: invitation.request_id }), 0).unwrap();
        }
        let now_ms = if revoke { 0 } else { 1001 };
        assert!(store.execute(Some(&credential(2)), &request(120, Operation::Join { room, invitation: credential(50) }), now_ms).is_err());
        assert_eq!(poll(&mut store, 1, room, 121, now_ms).members.len(), 1);
    }
}

#[test]
fn immutable_receipt_survives_restart_and_changed_body_conflicts() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("service.sqlite3");
    let mut store = open(&path, 0);
    pair(&mut store, 1, 0);
    let room = create(&mut store, None);
    let mutation = request(
        150,
        Operation::Apply {
            room: room.id,
            expected_revision: room.revision,
            action: Box::new(Action::SetOptions { mode: Mode::ExquisiteCorpse, timer_ms: Some(5000) }),
        },
    );
    let accepted = store.execute(Some(&credential(1)), &mutation, 0).unwrap();
    drop(store);
    let mut restarted = open(&path, 0);
    let before_retry = poll(&mut restarted, 1, room.id, 160, 0);
    let retried = restarted.execute(Some(&credential(1)), &mutation, 0).unwrap();
    assert_eq!(outcome(&retried), outcome(&accepted));
    assert_eq!(view(retried).revision, before_retry.revision, "Retry uses the current safe projection");
    assert_eq!(poll(&mut restarted, 1, room.id, 161, 0).revision, before_retry.revision);
    let altered = request(
        150,
        Operation::Apply {
            room: room.id,
            expected_revision: room.revision,
            action: Box::new(Action::SetOptions { mode: Mode::Animation { rotations: 4 }, timer_ms: None }),
        },
    );
    assert!(restarted.execute(Some(&credential(1)), &altered, 0).is_err());
    assert_eq!(poll(&mut restarted, 1, room.id, 162, 0).mode, Mode::ExquisiteCorpse);
}

#[test]
fn service_restart_pauses_timer_and_retains_assignments_until_explicit_resume() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("service.sqlite3");
    let mut store = open(&path, 0);
    pair(&mut store, 1, 0);
    pair(&mut store, 2, 0);
    let room = create(&mut store, Some(1000)).id;
    invite(&mut store, room, 110, 1, 60_000);
    join(&mut store, 2, room, 120);
    apply(&mut store, 1, room, 130, Action::Ready { ready: true }, 0);
    apply(&mut store, 2, room, 131, Action::Ready { ready: true }, 0);
    let projects = (0..4).map(|n| ProjectId::new(&format!("{:032x}", n + 500)).unwrap()).collect();
    let started = apply(&mut store, 1, room, 132, Action::Start { game: GameId::new(&format!("{:032x}", 600)).unwrap(), projects }, 0);
    store.tick(400).unwrap();
    drop(store);
    // A completely new service clock origin must not expire the drawing during downtime.
    let mut restarted = open(&path, 90_000);
    let host = poll(&mut restarted, 1, room, 140, 90_000);
    assert_eq!(host.phase, Phase::Drawing);
    assert!(host.paused.is_some() && host.deadline_ms.is_none());
    assert_eq!(host.assignment, started.assignment);
    poll(&mut restarted, 2, room, 141, 90_000);
    let resumed = apply(&mut restarted, 1, room, 142, Action::Resume, 90_000);
    assert_eq!(resumed.deadline_ms, Some(90_600));
    restarted.tick(90_601).unwrap();
    assert_eq!(poll(&mut restarted, 1, room, 143, 90_601).phase, Phase::Transfer);
}

fn signal_and_wait(marker: &str) {
    println!("{marker}");
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
}

#[test]
fn service_crash_child() {
    let Some(root) = std::env::var_os("KOMAPHONE_SERVICE_CRASH_ROOT") else { return };
    let path = Path::new(&root).join("service.sqlite3");
    let phase = std::env::var("KOMAPHONE_SERVICE_CRASH_PHASE").unwrap();
    let room = RoomId::new(&std::env::var("KOMAPHONE_SERVICE_CRASH_ROOM").unwrap()).unwrap();
    let mut store = open(&path, 0);
    let current = poll(&mut store, 1, room, 170, 0);
    let mutation = request(
        180,
        Operation::Apply {
            room,
            expected_revision: current.revision,
            action: Box::new(Action::SetOptions { mode: Mode::ExquisiteCorpse, timer_ms: Some(5000) }),
        },
    );
    std::fs::write(Path::new(&root).join("request.json"), serde_json::to_vec(&mutation).unwrap()).unwrap();
    if phase == "during" {
        store
            .execute_observed(Some(&credential(1)), &mutation, 0, |stage| {
                if stage == SaveStage::BeforeCommit {
                    assert!(
                        std::fs::metadata(Path::new(&root).join("service.sqlite3-journal")).unwrap().len() > 512,
                        "Actual rollback journal must be written before the process kill"
                    );
                    signal_and_wait("KOMAPHONE_SERVICE_BEFORE_COMMIT");
                }
            })
            .unwrap();
    } else {
        let accepted = store.execute(Some(&credential(1)), &mutation, 0).unwrap();
        std::fs::write(Path::new(&root).join("accepted.json"), serde_json::to_vec(&accepted).unwrap()).unwrap();
        signal_and_wait("KOMAPHONE_SERVICE_ACKNOWLEDGED");
    }
}

fn kill_service_transaction(phase: &str, marker: &str) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("service.sqlite3");
    let mut store = open(&path, 0);
    pair(&mut store, 1, 0);
    let room = create(&mut store, None).id;
    drop(store);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "service_crash_child", "--nocapture"])
        .env("KOMAPHONE_SERVICE_CRASH_ROOT", directory.path())
        .env("KOMAPHONE_SERVICE_CRASH_PHASE", phase)
        .env("KOMAPHONE_SERVICE_CRASH_ROOM", room.as_str())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let output = child.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in std::io::BufReader::new(output).lines() {
            if sender.send(line.unwrap_or_default()).is_err() {
                break;
            }
        }
    });
    let started = std::time::Instant::now();
    let mut observed = false;
    while started.elapsed() < Duration::from_secs(30) {
        match receiver.recv_timeout(Duration::from_secs(1)) {
            Ok(line) if line.contains(marker) => {
                observed = true;
                break;
            }
            Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let killed = child.kill();
    let status = child.wait().unwrap();
    reader.join().unwrap();
    assert!(observed, "Child never reached {marker}: {status}");
    killed.unwrap();
    assert!(!status.success(), "This must be forced termination");
    let mut recovered = open(&path, 0);
    let room_after = poll(&mut recovered, 1, room, 190, 0);
    let expected = if phase == "during" { Mode::Animation { rotations: 1 } } else { Mode::ExquisiteCorpse };
    assert_eq!(room_after.mode, expected);
    let mutation = RpcRequest::decode(&std::fs::read(directory.path().join("request.json")).unwrap()).unwrap();
    if phase == "ack" {
        let original: RpcResponse = serde_json::from_slice(&std::fs::read(directory.path().join("accepted.json")).unwrap()).unwrap();
        let retry = recovered.execute(Some(&credential(1)), &mutation, 0).unwrap();
        assert_eq!(outcome(&retry), outcome(&original));
        assert_eq!(view(retry).revision, room_after.revision);
    } else {
        // No receipt for the killed mutation; reuse its ID with the now-current precondition.
        let retry = request(
            180,
            Operation::Apply {
                room,
                expected_revision: room_after.revision,
                action: Box::new(Action::SetOptions { mode: Mode::ExquisiteCorpse, timer_ms: Some(5000) }),
            },
        );
        assert!(recovered.execute(Some(&credential(1)), &retry, 0).is_ok());
    }
}

#[test]
fn process_kill_after_service_ack_preserves_state_and_exact_receipt() {
    kill_service_transaction("ack", "KOMAPHONE_SERVICE_ACKNOWLEDGED");
}

#[test]
fn process_kill_before_service_commit_preserves_previous_state_without_receipt() {
    kill_service_transaction("during", "KOMAPHONE_SERVICE_BEFORE_COMMIT");
}
