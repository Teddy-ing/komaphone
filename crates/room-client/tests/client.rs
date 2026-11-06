#![cfg(not(target_arch = "wasm32"))]
use photocraft_party::protocol::{Operation, RpcResponse};
use photocraft_party::room::{Action, Room};
use photocraft_party::{Mode, RoomId};
use photocraft_room_client::{CredentialStore, Error, Outbox, Result, Transport, normalize_endpoint};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct MemoryCredentials(Arc<Mutex<BTreeMap<(String, String), String>>>);
impl CredentialStore for MemoryCredentials {
    fn get(&self, ns: &str, key: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(&(ns.into(), key.into())).cloned())
    }
    fn set(&self, ns: &str, key: &str, value: &str) -> Result<()> {
        self.0.lock().unwrap().insert((ns.into(), key.into()), value.into());
        Ok(())
    }
    fn remove(&self, ns: &str, key: &str) -> Result<()> {
        self.0.lock().unwrap().remove(&(ns.into(), key.into()));
        Ok(())
    }
}

fn paired(path: &std::path::Path, credentials: MemoryCredentials) -> Outbox {
    let mut store = Outbox::open(path, Box::new(credentials)).unwrap();
    store.configure("http://127.0.0.1:43127", "Player").unwrap();
    let request = store.next().unwrap().unwrap();
    let Operation::Pair { participant, .. } = &request.operation else { panic!("pair") };
    store.acknowledge(&request, &RpcResponse::Paired { participant: participant.clone() }).unwrap();
    store
}

#[test]
fn http_is_numeric_loopback_only_and_urls_cannot_contain_secrets() {
    for input in ["http://127.0.0.1:43127", "http://[::1]:43127/", "https://private.example:43127"] {
        assert!(normalize_endpoint(input).is_ok(), "{input}");
    }
    for input in [
        "http://localhost:43127",
        "http://192.168.1.2",
        "http://example.com",
        "file:///a",
        "https://user:secret@example.com",
        "https://example.com/x",
        "https://example.com/?token=secret",
        "https://example.com/#secret",
    ] {
        assert!(normalize_endpoint(input).is_err(), "{input}");
    }
}

#[test]
fn pairing_and_exact_pending_operation_survive_restart_without_plaintext_secrets() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rooms.sqlite3");
    let credentials = MemoryCredentials::default();
    let mut store = Outbox::open(&path, Box::new(credentials.clone())).unwrap();
    store.configure("http://127.0.0.1:43127", "Player").unwrap();
    let expected = store.next().unwrap().unwrap();
    let secret = store.credential().unwrap();
    let profile = store.status().profile;
    assert_eq!(store.status().pending, 1);
    drop(store);
    let mut reopened = Outbox::open(&path, Box::new(credentials)).unwrap();
    let actual = reopened.next().unwrap().unwrap();
    assert!(expected == actual);
    assert_eq!(profile, reopened.status().profile);
    assert!(!reopened.status().connected);
    let bytes = std::fs::read(&path).unwrap();
    assert!(!bytes.windows(secret.len()).any(|w| w == secret.as_bytes()));
    assert!(!serde_json::to_string(&reopened.status()).unwrap().contains(&secret));
    let Operation::Pair { participant, .. } = &actual.operation else { panic!("pair") };
    reopened.acknowledge(&actual, &RpcResponse::Paired { participant: participant.clone() }).unwrap();
    assert_eq!(reopened.status().pending, 0);
    assert!(reopened.status().paired);
}

#[test]
fn rejected_invitation_is_retained_with_its_exact_identity_and_credential_reference() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rooms.sqlite3");
    let vault = MemoryCredentials::default();
    let mut store = paired(&path, vault.clone());
    let room = RoomId::new(&"a".repeat(32)).unwrap();
    let secret = "b".repeat(64);
    let id = store.enqueue(Operation::Join { room, invitation: secret.clone() }).unwrap();
    let original = store.next().unwrap().unwrap();
    store.mark_error(&Error::Rejected { code: "unauthorized".into(), message: "Denied".into() }, Some(id), true).unwrap();
    assert!(store.status().blocked);
    drop(store);
    let mut store = Outbox::open(&path, Box::new(vault)).unwrap();
    assert_eq!(store.status().pending, 1);
    store.retry().unwrap();
    assert!(original == store.next().unwrap().unwrap());
    store.mark_error(&Error::Invalid("Rejected".into()), Some(id), true).unwrap();
    store.enqueue(Operation::Apply { room, expected_revision: 0, action: Box::new(Action::Ready { ready: true }) }).unwrap();
    store.retain_rejected().unwrap();
    assert_eq!(store.status().pending, 0);
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(db.query_row("SELECT COUNT(*) FROM outbox WHERE retained=1", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
    assert!(!std::fs::read(&path).unwrap().windows(secret.len()).any(|w| w == secret.as_bytes()));
}

#[test]
fn acknowledgment_and_authorized_room_view_commit_together() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rooms.sqlite3");
    let vault = MemoryCredentials::default();
    let mut store = paired(&path, vault.clone());
    let profile = store.status().profile.unwrap();
    let room = Room::new(RoomId::new(&"a".repeat(32)).unwrap(), profile.clone(), Mode::Animation { rotations: 2 }, None).unwrap();
    store.enqueue(Operation::Create { mode: Mode::Animation { rotations: 2 }, timer_ms: None }).unwrap();
    let request = store.next().unwrap().unwrap();
    let view = room.view(profile.id).unwrap();
    store.acknowledge(&request, &RpcResponse::Room { view: Box::new(view.clone()), outcome: None }).unwrap();
    drop(store);
    let store = Outbox::open(&path, Box::new(vault)).unwrap();
    assert_eq!(store.status().room, Some(view));
    assert_eq!(store.status().pending, 0);
}

#[test]
fn disconnected_leave_ack_clears_removed_lobby_membership_and_rejoin_resumes_polling() {
    use photocraft_party::{DeviceId, Participant, ParticipantId};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rooms.sqlite3");
    let vault = MemoryCredentials::default();
    let mut store = paired(&path, vault.clone());
    let profile = store.status().profile.unwrap();
    let guest =
        Participant { id: ParticipantId::new(&"2".repeat(32)).unwrap(), device: DeviceId::new(&"3".repeat(32)).unwrap(), display_name: "Friend".into() };
    let mut room = Room::new(RoomId::new(&"1".repeat(32)).unwrap(), profile.clone(), Mode::Animation { rotations: 2 }, None).unwrap();
    room.apply(guest.id, room.revision(), 0, Action::Join { participant: guest }).unwrap();
    store.update_poll(room.view(profile.id).unwrap()).unwrap();
    let invite_id = store.enqueue(Operation::Invite { room: room.id(), secret: "4".repeat(64), expires_in_ms: 1000, uses: 1 }).unwrap();
    store.acknowledge(&store.next().unwrap().unwrap(), &RpcResponse::Invitation { id: invite_id, expires_at_ms: 1000 }).unwrap();
    assert!(store.invitation().unwrap().is_some());
    store.enqueue(Operation::Apply { room: room.id(), expected_revision: room.revision(), action: Box::new(Action::Leave) }).unwrap();
    let leave = store.next().unwrap().unwrap();
    room.apply(profile.id, room.revision(), 0, Action::Leave).unwrap();
    store.acknowledge(&leave, &RpcResponse::Disconnected).unwrap();
    assert_eq!(store.status().pending, 0);
    assert!(store.status().room.is_none());
    assert!(store.invitation().unwrap().is_none());
    assert!(!store.active());
    assert!(!store.status().polling);
    assert!(!store.status().connected);
    drop(store);
    let mut store = Outbox::open(&path, Box::new(vault)).unwrap();
    assert_eq!(store.status().pending, 0);
    assert!(store.status().room.is_none());
    assert!(store.status().paired);
    assert!(!store.active());
    assert_eq!(store.status().profile, Some(profile.clone()));
    store.enqueue(Operation::Join { room: room.id(), invitation: "5".repeat(64) }).unwrap();
    room.apply(profile.id, room.revision(), 0, Action::Join { participant: profile.clone() }).unwrap();
    store.acknowledge(&store.next().unwrap().unwrap(), &RpcResponse::Room { view: Box::new(room.view(profile.id).unwrap()), outcome: None }).unwrap();
    assert_eq!(store.status().pending, 0);
    assert!(store.active());
    assert!(store.status().polling);
    assert!(store.status().connected);
    assert_eq!(store.status().room.as_ref().map(|view| view.id), Some(room.id()));
}

#[test]
fn active_game_leave_retains_assignment_for_explicit_reconnect() {
    use photocraft_party::{DeviceId, GameId, Participant, ParticipantId, ProjectId};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rooms.sqlite3");
    let vault = MemoryCredentials::default();
    let mut store = paired(&path, vault.clone());
    let profile = store.status().profile.unwrap();
    let guest =
        Participant { id: ParticipantId::new(&"2".repeat(32)).unwrap(), device: DeviceId::new(&"3".repeat(32)).unwrap(), display_name: "Friend".into() };
    let mut room = Room::new(RoomId::new(&"1".repeat(32)).unwrap(), profile.clone(), Mode::Animation { rotations: 1 }, None).unwrap();
    room.apply(guest.id, room.revision(), 0, Action::Join { participant: guest.clone() }).unwrap();
    for actor in [profile.id, guest.id] {
        room.apply(actor, room.revision(), 0, Action::Ready { ready: true }).unwrap();
    }
    room.apply(
        profile.id,
        room.revision(),
        0,
        Action::Start { game: GameId::new(&"4".repeat(32)).unwrap(), projects: (10..14).map(|n| ProjectId::new(&format!("{n:032x}")).unwrap()).collect() },
    )
    .unwrap();
    let before = room.view(profile.id).unwrap();
    store.update_poll(before.clone()).unwrap();
    store.enqueue(Operation::Apply { room: room.id(), expected_revision: room.revision(), action: Box::new(Action::Leave) }).unwrap();
    store.acknowledge(&store.next().unwrap().unwrap(), &RpcResponse::Disconnected).unwrap();
    assert!(!store.active());
    assert!(!store.status().polling);
    assert_eq!(store.status().room.as_ref().and_then(|view| view.assignment.clone()), before.assignment);
    assert!(store.status().room.unwrap().members.iter().any(|member| member.participant.id == profile.id && !member.connected));
    drop(store);
    let store = Outbox::open(path, Box::new(vault)).unwrap();
    assert!(!store.active());
    assert_eq!(store.status().pending, 0);
    assert_eq!(store.status().room.as_ref().and_then(|view| view.assignment.clone()), before.assignment);
}

#[test]
fn disconnected_ack_cannot_accept_an_unrelated_room_action() {
    let root = tempfile::tempdir().unwrap();
    let mut store = paired(&root.path().join("rooms.sqlite3"), MemoryCredentials::default());
    store
        .enqueue(Operation::Apply { room: RoomId::new(&"1".repeat(32)).unwrap(), expected_revision: 0, action: Box::new(Action::Ready { ready: true }) })
        .unwrap();
    let request = store.next().unwrap().unwrap();
    assert!(store.acknowledge(&request, &RpcResponse::Disconnected).is_err());
    assert_eq!(store.status().pending, 1);
    assert!(request == store.next().unwrap().unwrap());
}

#[test]
fn wrong_acknowledgment_never_drops_pending_work() {
    let root = tempfile::tempdir().unwrap();
    let mut store = paired(&root.path().join("rooms.sqlite3"), MemoryCredentials::default());
    store.enqueue(Operation::Create { mode: Mode::ExquisiteCorpse, timer_ms: None }).unwrap();
    let request = store.next().unwrap().unwrap();
    assert!(store.acknowledge(&request, &RpcResponse::Revoked).is_err());
    assert_eq!(store.status().pending, 1);
    assert!(request == store.next().unwrap().unwrap());
}

#[test]
fn corrupted_pending_request_is_reported_without_sending_or_overwriting_it() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rooms.sqlite3");
    let vault = MemoryCredentials::default();
    let mut store = paired(&path, vault.clone());
    store.enqueue(Operation::Create { mode: Mode::ExquisiteCorpse, timer_ms: None }).unwrap();
    drop(store);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE outbox SET body=x'7b7d'", []).unwrap();
    drop(db);
    assert!(Outbox::open(&path, Box::new(vault)).is_err());
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(db.query_row("SELECT body FROM outbox", [], |row| row.get::<_, Vec<u8>>(0)).unwrap(), b"{}");
}

#[test]
fn future_database_schema_is_preserved() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rooms.sqlite3");
    drop(paired(&path, MemoryCredentials::default()));
    let db = rusqlite::Connection::open(&path).unwrap();
    db.pragma_update(None, "user_version", 2).unwrap();
    drop(db);
    let before = std::fs::read(&path).unwrap();
    assert!(Outbox::open(&path, Box::new(MemoryCredentials::default())).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn byte_limit_failure_keeps_previously_durable_outbox() {
    let root = tempfile::tempdir().unwrap();
    let mut store = paired(&root.path().join("rooms.sqlite3"), MemoryCredentials::default());
    let room = RoomId::new(&"a".repeat(32)).unwrap();
    store.enqueue(Operation::Apply { room, expected_revision: 0, action: Box::new(Action::Ready { ready: true }) }).unwrap();
    let first = store.next().unwrap().unwrap();
    assert!(store.enqueue(Operation::Apply { room, expected_revision: 0, action: Box::new(Action::End { reason: "x".repeat(300_000) }) }).is_err());
    assert_eq!(store.status().pending, 1);
    assert!(first == store.next().unwrap().unwrap());
}

fn one_response(response: String) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(stream) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("Test listener did not accept a request: {error}"),
            }
        };
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let mut bytes = [0u8; 8192];
        let _ = stream.read(&mut bytes).unwrap();
        stream.write_all(response.as_bytes()).unwrap();
    });
    (endpoint, handle)
}

#[test]
fn transport_never_follows_redirects_or_echoes_remote_secret_errors() {
    let root = tempfile::tempdir().unwrap();
    let mut store = paired(&root.path().join("rooms.sqlite3"), MemoryCredentials::default());
    store.enqueue(Operation::Create { mode: Mode::ExquisiteCorpse, timer_ms: None }).unwrap();
    let request = store.next().unwrap().unwrap();
    let credential = store.credential().unwrap();
    let transport = Transport::new(None).unwrap();
    let body = format!(r#"{{"code":"unauthorized","message":"{credential}"}}"#);
    let (endpoint, handle) = one_response(format!(
        "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    let error = transport.call(&endpoint, &credential, &request).unwrap_err();
    assert!(!error.to_string().contains(&credential));
    handle.join().unwrap();
    let (endpoint, handle) =
        one_response("HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into());
    assert!(matches!(transport.call(&endpoint, &credential, &request), Err(Error::Rejected { .. })));
    handle.join().unwrap();
}

#[test]
fn oversized_service_response_is_rejected_before_allocation() {
    let root = tempfile::tempdir().unwrap();
    let store = paired(&root.path().join("rooms.sqlite3"), MemoryCredentials::default());
    let request = photocraft_party::protocol::RpcRequest {
        protocol_version: 1,
        request_id: photocraft_party::RequestId::new(&"a".repeat(32)).unwrap(),
        operation: Operation::Poll { room: RoomId::new(&"b".repeat(32)).unwrap(), after_revision: 0 },
    };
    let (endpoint, handle) = one_response("HTTP/1.1 200 OK\r\nContent-Length: 999999999999\r\nConnection: close\r\n\r\n".into());
    assert!(Transport::new(None).unwrap().call(&endpoint, &store.credential().unwrap(), &request).is_err());
    handle.join().unwrap();
}

#[cfg(windows)]
#[test]
fn real_windows_credential_manager_round_trip_and_cleanup() {
    let vault = photocraft_room_client::WindowsCredentials;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).unwrap();
    let namespace = format!("test-{}", random.iter().map(|v| format!("{v:02x}")).collect::<String>());
    let secret = "c".repeat(64);
    vault.set(&namespace, "roundtrip", &secret).unwrap();
    let read = photocraft_room_client::WindowsCredentials.get(&namespace, "roundtrip").unwrap();
    let result = read.as_deref() == Some(&secret);
    vault.remove(&namespace, "roundtrip").unwrap();
    assert!(result);
    assert!(vault.get(&namespace, "roundtrip").unwrap().is_none());
}

#[test]
fn transfer_freeze_is_prepended_once_and_survives_client_restart() {
    use photocraft_party::room::{Assignment, AssignmentView, Phase};
    use photocraft_party::{AssetHash, AssetRef, AssignmentId, CheckpointId, CheckpointRef, GameId, ProjectId, ReplayId, ReplayRef, Submission, SubmissionId};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rooms.sqlite3");
    let vault = MemoryCredentials::default();
    let mut store = paired(&path, vault.clone());
    let profile = store.status().profile.unwrap();
    let room = Room::new(RoomId::new(&"a".repeat(32)).unwrap(), profile.clone(), Mode::Animation { rotations: 1 }, Some(1000)).unwrap();
    let mut view = room.view(profile.id).unwrap();
    view.phase = Phase::Transfer;
    view.epoch = 3;
    view.revision = 7;
    let assigned = Assignment {
        id: AssignmentId { game: GameId::new(&"b".repeat(32)).unwrap(), chain: 0, round: 0 },
        player: profile.id,
        project: ProjectId::new(&"c".repeat(32)).unwrap(),
    };
    view.assignment = Some(AssignmentView { assignment: assigned.clone(), submitted: false, reference: None });
    let checkpoint_asset = AssetRef { hash: AssetHash::new(&"d".repeat(64)).unwrap(), bytes: 4 };
    let image = AssetRef { hash: AssetHash::new(&"e".repeat(64)).unwrap(), bytes: 4 };
    let checkpoint = CheckpointRef { id: CheckpointId::new(&"f".repeat(32)).unwrap(), asset: checkpoint_asset };
    let submission = Submission {
        id: SubmissionId::new(&"1".repeat(32)).unwrap(),
        assignment: assigned.id,
        author: profile.id,
        project: assigned.project,
        revision: 2,
        image,
        checkpoint,
        replay: ReplayRef {
            id: ReplayId::new(&"2".repeat(32)).unwrap(),
            manifest: None,
            baseline: checkpoint,
            end_event: None,
            event_count: 0,
            duration_ms: 0,
        },
        assets: vec![checkpoint_asset, image],
        note: String::new(),
        seam: None,
    };
    store.update_poll(view.clone()).unwrap();
    let pin = photocraft_room_client::FreezePin { room: view.id, epoch: view.epoch, assignment: assigned.id, project: assigned.project, revision: 2 };
    store.pin_freeze(pin.clone()).unwrap();
    assert!(store.status().submission_project.is_none());
    drop(store);
    let mut store = Outbox::open(&path, Box::new(vault.clone())).unwrap();
    assert_eq!(store.status().freeze_pin, Some(pin.clone()), "first deadline revision survives reopening before heavy preparation");
    assert!(store.pin_freeze(photocraft_room_client::FreezePin { revision: 3, ..pin }).is_err());
    let submit_id = store
        .enqueue(Operation::Apply {
            room: view.id,
            expected_revision: 5,
            action: Box::new(Action::Submit { epoch: view.epoch, submission: submission.clone() }),
        })
        .unwrap();
    assert_eq!(store.status().submission_project, Some(assigned.project));
    store.mark_error(&Error::Rejected { code: "phase".into(), message: "The timer froze".into() }, Some(submit_id), true).unwrap();
    store.update_poll(view.clone()).unwrap();
    store.ensure_transfer_freeze().unwrap();
    assert!(!store.status().blocked);
    assert_eq!(store.status().pending, 2);
    let freeze = store.next().unwrap().unwrap();
    assert!(
        matches!(&freeze.operation,Operation::Apply {action,..} if matches!(action.as_ref(),Action::FreezeReport {submission:record,..} if record == &submission))
    );
    store.ensure_transfer_freeze().unwrap();
    assert_eq!(store.status().pending, 2);
    store.acknowledge(&freeze, &RpcResponse::Room { view: Box::new(view), outcome: None }).unwrap();
    drop(store);
    let mut store = Outbox::open(&path, Box::new(vault)).unwrap();
    store.ensure_transfer_freeze().unwrap();
    assert_eq!(store.status().pending, 1);
    assert_eq!(store.next().unwrap().unwrap().request_id, submit_id);
    assert_eq!(store.status().submission_project, Some(assigned.project));
}

#[test]
fn pending_room_metadata_does_not_mark_a_drawing_as_submitted() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("rooms.sqlite3");
    let vault = MemoryCredentials::default();
    let mut store = paired(&path, vault.clone());
    store.enqueue(Operation::Apply { room: RoomId::new(&"a".repeat(32)).unwrap(), expected_revision: 0, action: Box::new(Action::Pause) }).unwrap();
    assert!(store.status().submission_project.is_none());
    drop(store);
    let store = Outbox::open(path, Box::new(vault)).unwrap();
    assert_eq!(store.status().pending, 1);
    assert!(store.status().submission_project.is_none());
}
