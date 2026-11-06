//! Client durability only. Network/service acceptance is independently exercised by the
//! real HTTP harness; this fixture targets process termination around SQLite boundaries.
#![cfg(not(target_arch = "wasm32"))]
use photocraft_party::Mode;
use photocraft_party::protocol::{Operation, RpcResponse};
use photocraft_room_client::{CredentialStore, Outbox, Result};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct FixtureCredentials;
impl CredentialStore for FixtureCredentials {
    fn get(&self, _: &str, _: &str) -> Result<Option<String>> {
        Ok(Some("a".repeat(64)))
    }
    fn set(&self, _: &str, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
    fn remove(&self, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
}

#[test]
fn process_child() {
    let Ok(root) = std::env::var("KOMAPHONE_CLIENT_CRASH_FIXTURE") else { return };
    let root = Path::new(&root);
    let mut store = Outbox::open(root.join("room.sqlite3"), Box::new(FixtureCredentials)).unwrap();
    store.configure("http://127.0.0.1:43127", "Crash fixture").unwrap();
    let request = store.next().unwrap().unwrap();
    let Operation::Pair { participant, .. } = &request.operation else { panic!("pair") };
    store.acknowledge(&request, &RpcResponse::Paired { participant: participant.clone() }).unwrap();
    let mode = std::env::var("KOMAPHONE_CLIENT_CRASH_STAGE").unwrap();
    if mode == "after_pin" {
        use photocraft_party::room::{Assignment, AssignmentView, Phase, Room};
        use photocraft_party::{AssignmentId, GameId, ProjectId, RoomId};
        let room = Room::new(RoomId::new(&"1".repeat(32)).unwrap(), participant.clone(), Mode::Animation { rotations: 1 }, Some(1000)).unwrap();
        let mut view = room.view(participant.id).unwrap();
        let assignment = Assignment {
            id: AssignmentId { game: GameId::new(&"2".repeat(32)).unwrap(), chain: 0, round: 0 },
            player: participant.id,
            project: ProjectId::new(&"3".repeat(32)).unwrap(),
        };
        view.phase = Phase::Transfer;
        view.epoch = 2;
        view.assignment = Some(AssignmentView { assignment: assignment.clone(), submitted: false, reference: None });
        let pin = photocraft_room_client::FreezePin { room: view.id, epoch: view.epoch, assignment: assignment.id, project: assignment.project, revision: 1 };
        store.update_poll(view).unwrap();
        store.pin_freeze(pin.clone()).unwrap();
        std::fs::write(root.join("ready"), serde_json::to_vec(&pin).unwrap()).unwrap();
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    let id = store.enqueue(Operation::Create { mode: Mode::ExquisiteCorpse, timer_ms: None }).unwrap();
    if mode == "during_write" {
        let db = rusqlite::Connection::open(root.join("room.sqlite3")).unwrap();
        db.execute_batch("BEGIN IMMEDIATE; UPDATE outbox SET body=x'7b7d';").unwrap();
        std::fs::write(root.join("ready"), id.as_str()).unwrap();
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    std::fs::write(root.join("ready"), id.as_str()).unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn launch(root: &Path, stage: &str) -> Child {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "process_child", "--nocapture"])
        .env("KOMAPHONE_CLIENT_CRASH_FIXTURE", root)
        .env("KOMAPHONE_CLIENT_CRASH_STAGE", stage)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command.spawn().unwrap()
}
fn kill_after_marker(root: &Path, child: &mut Child) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !root.join("ready").exists() {
        if Instant::now() > deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Crash helper did not reach the observation boundary");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let request = std::fs::read_to_string(root.join("ready")).unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    request
}
fn assert_pending_survives(stage: &str) {
    let root = tempfile::tempdir().unwrap();
    let mut child = launch(root.path(), stage);
    let request_id = kill_after_marker(root.path(), &mut child);
    let store = Outbox::open(root.path().join("room.sqlite3"), Box::new(FixtureCredentials)).unwrap();
    let recovered = store.next().unwrap().unwrap();
    assert_eq!(recovered.request_id.as_str(), request_id);
    assert!(matches!(recovered.operation, Operation::Create { mode: Mode::ExquisiteCorpse, timer_ms: None }));
    assert_eq!(store.status().pending, 1);
    assert!(store.status().paired);
}
#[test]
fn kill_immediately_after_local_pending_ack_restores_exact_request() {
    assert_pending_survives("after_commit");
}
#[test]
fn kill_during_transaction_preserves_preceding_pending_request() {
    assert_pending_survives("during_write");
}

#[test]
fn kill_after_deadline_pin_ack_before_preparation_preserves_first_revision() {
    let root = tempfile::tempdir().unwrap();
    let mut child = launch(root.path(), "after_pin");
    let raw = kill_after_marker(root.path(), &mut child);
    let pin: photocraft_room_client::FreezePin = serde_json::from_str(&raw).unwrap();
    let mut store = Outbox::open(root.path().join("room.sqlite3"), Box::new(FixtureCredentials)).unwrap();
    assert_eq!(store.status().freeze_pin, Some(pin.clone()));
    assert_eq!(store.status().pending, 0, "No artifact preparation or submission had started");
    assert!(store.pin_freeze(photocraft_room_client::FreezePin { revision: 2, ..pin.clone() }).is_err());
    assert_eq!(store.status().freeze_pin, Some(pin));
}
