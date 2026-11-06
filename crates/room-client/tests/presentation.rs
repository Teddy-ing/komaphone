//! Independent persistence checks for local handoff text and reveal navigation.
//! These use real SQLite and process termination with fixture credentials, not a service.
#![cfg(not(target_arch = "wasm32"))]
use photocraft_party::protocol::{Operation, RpcResponse};
use photocraft_party::room::{Action, Phase, Room};
use photocraft_party::{DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, RoomId};
use photocraft_room_client::{CredentialStore, NoteDraft, Outbox, Result, RevealPosition};
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

fn setup(path: &Path, mode: Mode) -> (Outbox, Room) {
    let mut store = Outbox::open(path, Box::new(FixtureCredentials)).unwrap();
    store.configure("http://127.0.0.1:43127", "Presentation fixture").unwrap();
    let pair = store.next().unwrap().unwrap();
    let Operation::Pair { participant, .. } = &pair.operation else { panic!("Pairing required") };
    store.acknowledge(&pair, &RpcResponse::Paired { participant: participant.clone() }).unwrap();
    let guest = Participant { id: ParticipantId::new(&"2".repeat(32)).unwrap(), device: DeviceId::new(&"3".repeat(32)).unwrap(), display_name: "Guest".into() };
    let mut room = Room::new(RoomId::new(&"1".repeat(32)).unwrap(), participant.clone(), mode, None).unwrap();
    room.apply(guest.id, room.revision(), 0, Action::Join { participant: guest.clone() }).unwrap();
    for actor in [participant.id, guest.id] {
        room.apply(actor, room.revision(), 0, Action::Ready { ready: true }).unwrap();
    }
    room.apply(
        participant.id,
        room.revision(),
        0,
        Action::Start {
            game: GameId::new(&"4".repeat(32)).unwrap(),
            projects: (10..10 + usize::from(mode.rounds(2).unwrap()) * 2).map(|n| ProjectId::new(&format!("{n:032x}")).unwrap()).collect(),
        },
    )
    .unwrap();
    store.update_poll(room.view(participant.id).unwrap()).unwrap();
    (store, room)
}
fn note(store: &Outbox) -> NoteDraft {
    NoteDraft { assignment: store.status().room.unwrap().assignment.unwrap().assignment.id, note: "Move the little square left.\nKeep its color.".into() }
}
fn finish(store: &mut Outbox, room: &mut Room) -> RevealPosition {
    let actor = store.status().profile.unwrap().id;
    room.apply(actor, room.revision(), 0, Action::End { reason: "Fixture explicitly ends incomplete game".into() }).unwrap();
    store.update_poll(room.view(actor).unwrap()).unwrap();
    RevealPosition { game: GameId::new(&"4".repeat(32)).unwrap(), chain: 1, frame: 1, fps: 13, looping: false, visible_parts: 2 }
}

#[test]
fn note_is_durable_without_a_service_request_and_rejects_wrong_or_frozen_assignment() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("room.sqlite3");
    let (mut store, _) = setup(&path, Mode::Animation { rotations: 2 });
    let expected = note(&store);
    store.save_note(expected.clone()).unwrap();
    assert_eq!(store.status().pending, 0);
    for invalid in [
        NoteDraft { note: "x".repeat(1025), ..expected.clone() },
        NoteDraft { note: "hidden\u{0000}control".into(), ..expected.clone() },
        NoteDraft { assignment: photocraft_party::AssignmentId { round: 1, ..expected.assignment }, ..expected.clone() },
    ] {
        assert!(store.save_note(invalid).is_err());
        assert_eq!(store.status().note_draft, Some(expected.clone()));
    }
    let mut frozen = store.status().room.unwrap();
    frozen.phase = Phase::Transfer;
    store.update_poll(frozen).unwrap();
    assert!(store.save_note(NoteDraft { note: "too late".into(), ..expected.clone() }).is_err());
    drop(store);
    let reopened = Outbox::open(path, Box::new(FixtureCredentials)).unwrap();
    assert_eq!(reopened.status().note_draft, Some(expected));
    assert_eq!(reopened.status().pending, 0);
}

#[test]
fn reveal_position_is_local_durable_bounded_and_cannot_reference_unrevealed_games() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("room.sqlite3");
    let (mut store, mut room) = setup(&path, Mode::ExquisiteCorpse);
    let position = RevealPosition { game: GameId::new(&"4".repeat(32)).unwrap(), chain: 1, frame: 1, fps: 6, looping: true, visible_parts: 2 };
    assert!(store.save_reveal_position(position).is_err());
    let expected = finish(&mut store, &mut room);
    store.save_reveal_position(expected.clone()).unwrap();
    for invalid in [
        RevealPosition { game: GameId::new(&"5".repeat(32)).unwrap(), ..expected.clone() },
        RevealPosition { chain: 2, ..expected.clone() },
        RevealPosition { frame: 3, ..expected.clone() },
        RevealPosition { fps: 0, ..expected.clone() },
        RevealPosition { fps: 25, ..expected.clone() },
        RevealPosition { visible_parts: 0, ..expected.clone() },
        RevealPosition { visible_parts: 4, ..expected.clone() },
    ] {
        assert!(store.save_reveal_position(invalid).is_err());
        assert_eq!(store.status().reveal_position, Some(expected.clone()));
    }
    assert_eq!(store.status().pending, 0);
    drop(store);
    assert_eq!(Outbox::open(path, Box::new(FixtureCredentials)).unwrap().status().reveal_position, Some(expected));
}

#[test]
fn failed_note_commit_never_reports_new_text_or_overwrites_the_last_acknowledgment() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("room.sqlite3");
    let (mut store, _) = setup(&path, Mode::Animation { rotations: 1 });
    let expected = note(&store);
    store.save_note(expected.clone()).unwrap();
    let lock = rusqlite::Connection::open(&path).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(store.save_note(NoteDraft { note: "This commit must fail".into(), ..expected.clone() }).is_err());
    assert_eq!(store.status().note_draft, Some(expected.clone()));
    lock.execute_batch("ROLLBACK").unwrap();
    drop(store);
    assert_eq!(Outbox::open(path, Box::new(FixtureCredentials)).unwrap().status().note_draft, Some(expected));
}

#[test]
fn presentation_process_child() {
    let Ok(root) = std::env::var("KOMAPHONE_PRESENTATION_CRASH_ROOT") else { return };
    let root = Path::new(&root);
    let path = root.join("room.sqlite3");
    let (mut store, mut room) = setup(&path, Mode::Animation { rotations: 2 });
    let note = note(&store);
    store.save_note(note.clone()).unwrap();
    let position = finish(&mut store, &mut room);
    store.save_reveal_position(position.clone()).unwrap();
    let during_write = std::env::var("KOMAPHONE_PRESENTATION_CRASH_STAGE").unwrap() == "during_write";
    let lock = rusqlite::Connection::open(&path).unwrap();
    if during_write {
        lock.execute_batch("BEGIN IMMEDIATE; UPDATE state SET body=x'7b7d';").unwrap();
        lock.cache_flush().unwrap();
    }
    std::fs::write(root.join("ready"), serde_json::to_vec(&(note, position)).unwrap()).unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn launch(root: &Path, stage: &str) -> Child {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "presentation_process_child", "--nocapture"])
        .env("KOMAPHONE_PRESENTATION_CRASH_ROOT", root)
        .env("KOMAPHONE_PRESENTATION_CRASH_STAGE", stage)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command.spawn().unwrap()
}
fn kill_and_recover(stage: &str) {
    let root = tempfile::tempdir().unwrap();
    let mut child = launch(root.path(), stage);
    let deadline = Instant::now() + Duration::from_secs(20);
    while !root.path().join("ready").exists() {
        if Instant::now() > deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Presentation helper did not acknowledge a durable state");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let expected: (NoteDraft, RevealPosition) = serde_json::from_slice(&std::fs::read(root.path().join("ready")).unwrap()).unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    let store = Outbox::open(root.path().join("room.sqlite3"), Box::new(FixtureCredentials)).unwrap();
    assert_eq!(store.saved_note("http://127.0.0.1:43127", RoomId::new(&"1".repeat(32)).unwrap(), expected.0.assignment).unwrap(), Some(expected.0.clone()));
    assert_eq!(store.status().note_draft, Some(expected.0));
    assert_eq!(store.status().reveal_position, Some(expected.1));
    assert_eq!(store.status().pending, 0);
}

#[test]
fn kill_after_presentation_ack_restores_exact_note_and_reveal_position() {
    kill_and_recover("after_commit");
}

#[test]
fn kill_during_later_write_restores_acknowledged_note_and_reveal_position() {
    kill_and_recover("during_write");
}
