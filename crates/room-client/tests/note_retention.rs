//! Locally acknowledged notes remain recoverable when the game advances without them.
#![cfg(not(target_arch = "wasm32"))]
use photocraft_party::protocol::{Operation, RpcResponse};
use photocraft_party::room::{Action, Room};
use photocraft_party::{AssignmentId, DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, RoomId};
use photocraft_room_client::{CredentialStore, NoteDraft, Outbox, Result};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::path::Path;

const ENDPOINT: &str = "http://127.0.0.1:43127";
fn room_id() -> RoomId {
    RoomId::new(&"1".repeat(32)).unwrap()
}
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
fn setup(path: &Path) -> (Outbox, Room) {
    let mut store = Outbox::open(path, Box::new(FixtureCredentials)).unwrap();
    store.configure("http://127.0.0.1:43127", "Note recovery fixture").unwrap();
    let pair = store.next().unwrap().unwrap();
    let Operation::Pair { participant, .. } = &pair.operation else { panic!("Pairing required") };
    store.acknowledge(&pair, &RpcResponse::Paired { participant: participant.clone() }).unwrap();
    let guest = Participant { id: ParticipantId::new(&"2".repeat(32)).unwrap(), device: DeviceId::new(&"3".repeat(32)).unwrap(), display_name: "Guest".into() };
    let mut room = Room::new(RoomId::new(&"1".repeat(32)).unwrap(), participant.clone(), Mode::Animation { rotations: 2 }, None).unwrap();
    room.apply(guest.id, room.revision(), 0, Action::Join { participant: guest }).unwrap();
    start(&mut room, participant.id, 4);
    store.update_poll(room.view(participant.id).unwrap()).unwrap();
    (store, room)
}
fn start(room: &mut Room, moderator: ParticipantId, number: u8) {
    for actor in room.participants().iter().map(|p| p.id).collect::<Vec<_>>() {
        room.apply(actor, room.revision(), 0, Action::Ready { ready: true }).unwrap();
    }
    room.apply(
        moderator,
        room.revision(),
        0,
        Action::Start {
            game: GameId::new(&format!("{number:032x}")).unwrap(),
            projects: (0..8).map(|index| ProjectId::new(&format!("{:032x}", 100 * u32::from(number) + index)).unwrap()).collect(),
        },
    )
    .unwrap();
}
fn note(store: &Outbox, text: &str) -> NoteDraft {
    NoteDraft { assignment: store.status().room.unwrap().assignment.unwrap().assignment.id, note: text.into() }
}
fn skip_round(store: &mut Outbox, room: &mut Room) {
    let actor = store.status().profile.unwrap().id;
    for participant in room.participants().iter().map(|p| p.id).collect::<Vec<_>>() {
        room.apply(actor, room.revision(), 0, Action::Skip { participant, reason: "Explicit fixture skip".into() }).unwrap();
    }
    store.update_poll(room.view(actor).unwrap()).unwrap();
}
fn encoded(note: &NoteDraft) -> (Vec<u8>, String) {
    let body = serde_json::to_vec(note).unwrap();
    let checksum = format!("{:x}", Sha256::digest(&body));
    (body, checksum)
}
fn seed(db: &Connection, note: &NoteDraft) {
    let (body, checksum) = encoded(note);
    db.execute(
        "INSERT INTO local_assignment_notes(endpoint,room,game,chain,round,body,digest) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![ENDPOINT, room_id().as_str(), note.assignment.game.as_str(), note.assignment.chain, note.assignment.round, body, checksum],
    )
    .unwrap();
}

#[test]
fn skipped_notes_survive_later_empty_autosaves_new_games_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("room.sqlite3");
    let (mut store, mut room) = setup(&path);
    let first = note(&store, "Never submitted: keep the moon blue");
    store.save_note(first.clone()).unwrap();
    skip_round(&mut store, &mut room);
    let second = note(&store, "");
    store.save_note(second.clone()).unwrap();
    assert_eq!(store.saved_note(ENDPOINT, room_id(), first.assignment).unwrap(), Some(first.clone()));
    let actor = store.status().profile.unwrap().id;
    room.apply(actor, room.revision(), 0, Action::End { reason: "Incomplete game".into() }).unwrap();
    assert!(room.notes(actor).unwrap().is_empty(), "The service has no submitted note to recover");
    room.apply(actor, room.revision(), 0, Action::NextGame).unwrap();
    start(&mut room, actor, 5);
    store.update_poll(room.view(actor).unwrap()).unwrap();
    let third = note(&store, "");
    store.save_note(third.clone()).unwrap();
    drop(store);
    let reopened = Outbox::open(&path, Box::new(FixtureCredentials)).unwrap();
    for expected in [first.clone(), second, third.clone()] {
        assert_eq!(reopened.saved_note(ENDPOINT, room_id(), expected.assignment).unwrap(), Some(expected));
    }
    assert_eq!(reopened.status().note_draft, Some(third));
    assert!(!serde_json::to_string(&reopened.status()).unwrap().contains(&first.note), "Recovery history must not leak through the current party projection");
    assert_eq!(reopened.status().pending, 0);
}

#[test]
fn explicit_empty_edit_replaces_only_that_assignments_latest_text() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("room.sqlite3");
    let (mut store, _) = setup(&path);
    let original = note(&store, "An intentional edit may clear this note");
    store.save_note(original.clone()).unwrap();
    let cleared = NoteDraft { note: String::new(), ..original };
    store.save_note(cleared.clone()).unwrap();
    drop(store);
    let reopened = Outbox::open(&path, Box::new(FixtureCredentials)).unwrap();
    assert_eq!(reopened.saved_note(ENDPOINT, room_id(), cleared.assignment).unwrap(), Some(cleared.clone()));
    assert_eq!(reopened.status().note_draft, Some(cleared));
    let db = Connection::open(&path).unwrap();
    assert_eq!(db.query_row("SELECT count(*) FROM local_assignment_notes", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
}

#[test]
fn old_singleton_migrates_idempotently_without_changing_current_status() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("room.sqlite3");
    let (mut store, _) = setup(&path);
    let expected = note(&store, "Acknowledged before the additive recovery table existed");
    store.save_note(expected.clone()).unwrap();
    drop(store);
    let db = Connection::open(&path).unwrap();
    db.execute("DROP TABLE local_assignment_notes", []).unwrap();
    for _ in 0..2 {
        let reopened = Outbox::open(&path, Box::new(FixtureCredentials)).unwrap();
        assert_eq!(reopened.saved_note(ENDPOINT, room_id(), expected.assignment).unwrap(), Some(expected.clone()));
        assert_eq!(reopened.status().note_draft, Some(expected.clone()));
    }
    assert_eq!(db.query_row("SELECT count(*) FROM local_assignment_notes", [], |row| row.get::<_, i64>(0)).unwrap(), 1);
}

#[test]
fn failed_state_write_rolls_back_both_recovery_upsert_and_acknowledged_projection() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("room.sqlite3");
    let (mut store, mut room) = setup(&path);
    let expected = note(&store, "Last acknowledged text");
    store.save_note(expected.clone()).unwrap();
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TRIGGER reject_state BEFORE INSERT ON state BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;").unwrap();
    assert!(store.save_note(NoteDraft { note: "Must roll back".into(), ..expected.clone() }).is_err());
    assert_eq!(store.saved_note(ENDPOINT, room_id(), expected.assignment).unwrap(), Some(expected.clone()));
    assert_eq!(store.status().note_draft, Some(expected.clone()));
    db.execute("DROP TRIGGER reject_state", []).unwrap();
    skip_round(&mut store, &mut room);
    let next = note(&store, "New row must also roll back");
    db.execute_batch("CREATE TRIGGER reject_state BEFORE INSERT ON state BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;").unwrap();
    assert!(store.save_note(next.clone()).is_err());
    assert_eq!(store.saved_note(ENDPOINT, room_id(), next.assignment).unwrap(), None);
    assert_eq!(store.status().note_draft, Some(expected.clone()));
    db.execute("DROP TRIGGER reject_state", []).unwrap();
    drop(store);
    let reopened = Outbox::open(&path, Box::new(FixtureCredentials)).unwrap();
    assert_eq!(reopened.saved_note(ENDPOINT, room_id(), expected.assignment).unwrap(), Some(expected.clone()));
    assert_eq!(reopened.saved_note(ENDPOINT, room_id(), next.assignment).unwrap(), None);
    assert_eq!(reopened.status().note_draft, Some(expected));
}

#[test]
fn assignment_quota_preserves_all_existing_notes_and_allows_existing_note_edits() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("room.sqlite3");
    let (mut store, mut room) = setup(&path);
    let expected = note(&store, "Keep this acknowledged note");
    store.save_note(expected.clone()).unwrap();
    let mut db = Connection::open(&path).unwrap();
    let tx = db.transaction().unwrap();
    for index in 0..1023 {
        seed(
            &tx,
            &NoteDraft {
                assignment: AssignmentId { game: GameId::new(&format!("{:032x}", index + 100)).unwrap(), chain: 0, round: 0 },
                note: "Retained".into(),
            },
        );
    }
    tx.commit().unwrap();
    let edited = NoteDraft { note: "Existing row is still editable".into(), ..expected };
    store.save_note(edited.clone()).unwrap();
    skip_round(&mut store, &mut room);
    let next = note(&store, "");
    assert!(store.save_note(next.clone()).unwrap_err().to_string().contains("storage is full"));
    assert_eq!(store.status().note_draft, Some(edited.clone()));
    assert_eq!(store.saved_note(ENDPOINT, room_id(), next.assignment).unwrap(), None);
    assert_eq!(store.saved_note(ENDPOINT, room_id(), edited.assignment).unwrap(), Some(edited));
    assert_eq!(db.query_row("SELECT count(*) FROM local_assignment_notes", [], |row| row.get::<_, i64>(0)).unwrap(), 1024);
}

#[test]
fn encoded_byte_quota_refuses_growth_without_pruning_other_assignments() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("room.sqlite3");
    let (mut store, mut room) = setup(&path);
    let expected = note(&store, "Retained original");
    store.save_note(expected.clone()).unwrap();
    let mut db = Connection::open(&path).unwrap();
    let tx = db.transaction().unwrap();
    let mut used = encoded(&expected).0.len();
    let mut count = 1;
    loop {
        let seed_note = NoteDraft {
            assignment: AssignmentId { game: GameId::new(&format!("{:032x}", count + 100)).unwrap(), chain: 0, round: 0 },
            note: "\"".repeat(1024),
        };
        let length = encoded(&seed_note).0.len();
        if used + length > 2 * 1024 * 1024 {
            break;
        }
        seed(&tx, &seed_note);
        used += length;
        count += 1;
    }
    assert!(count < 1024, "This fixture isolates the byte limit from the assignment limit");
    tx.commit().unwrap();
    skip_round(&mut store, &mut room);
    let next = note(&store, &"\"".repeat(1024));
    assert!(store.save_note(next.clone()).unwrap_err().to_string().contains("storage is full"));
    assert_eq!(store.saved_note(ENDPOINT, room_id(), next.assignment).unwrap(), None);
    assert_eq!(store.saved_note(ENDPOINT, room_id(), expected.assignment).unwrap(), Some(expected.clone()));
    assert_eq!(store.status().note_draft, Some(expected));
    assert_eq!(db.query_row("SELECT sum(length(body)) FROM local_assignment_notes", [], |row| row.get::<_, i64>(0)).unwrap(), i64::try_from(used).unwrap());
}

#[test]
fn corrupt_recovery_records_are_bounded_and_rejected_without_repair_or_overwrite() {
    for corruption in 0..4 {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("room.sqlite3");
        let (mut store, _) = setup(&path);
        let expected = note(&store, "Keep the last readable database for recovery");
        store.save_note(expected.clone()).unwrap();
        let db = Connection::open(&path).unwrap();
        let (body, checksum) = match corruption {
            0 => (b"corrupt".to_vec(), "0".repeat(64)),
            1 => {
                let body = vec![b'x'; 9000];
                let checksum = format!("{:x}", Sha256::digest(&body));
                (body, checksum)
            }
            2 => encoded(&NoteDraft { assignment: AssignmentId { round: 11, ..expected.assignment }, ..expected.clone() }),
            _ => encoded(&NoteDraft { note: "x".repeat(1025), ..expected.clone() }),
        };
        db.execute("UPDATE local_assignment_notes SET body=?1,digest=?2", params![body, checksum]).unwrap();
        assert!(store.saved_note(ENDPOINT, room_id(), expected.assignment).is_err());
        assert!(store.save_note(NoteDraft { note: "Must not overwrite corruption".into(), ..expected.clone() }).is_err());
        assert_eq!(store.status().note_draft, Some(expected));
        drop(store);
        assert!(Outbox::open(&path, Box::new(FixtureCredentials)).is_err());
        assert_eq!(db.query_row("SELECT body FROM local_assignment_notes", [], |row| row.get::<_, Vec<u8>>(0)).unwrap(), body);
    }
}

#[test]
fn migration_reports_a_conflicting_current_recovery_record_without_overwriting_it() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("room.sqlite3");
    let (mut store, _) = setup(&path);
    let original = note(&store, "Current acknowledged projection");
    store.save_note(original.clone()).unwrap();
    drop(store);
    let db = Connection::open(&path).unwrap();
    let conflicting = NoteDraft { note: "Different checksummed recovery text".into(), ..original };
    let (body, checksum) = encoded(&conflicting);
    db.execute("UPDATE local_assignment_notes SET body=?1,digest=?2", params![body, checksum]).unwrap();
    assert!(Outbox::open(&path, Box::new(FixtureCredentials)).is_err());
    assert_eq!(db.query_row("SELECT body FROM local_assignment_notes", [], |row| row.get::<_, Vec<u8>>(0)).unwrap(), body);
}

#[test]
fn identical_assignment_ids_in_different_rooms_and_services_keep_independent_notes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("room.sqlite3");
    let (mut store, _) = setup(&path);
    let first = note(&store, "First room, first service");
    store.save_note(first.clone()).unwrap();
    let second_room = RoomId::new(&"2".repeat(32)).unwrap();
    let mut second_view = store.status().room.unwrap();
    second_view.id = second_room;
    store.update_poll(second_view.clone()).unwrap();
    assert!(store.status().note_draft.is_none(), "Changing rooms clears only the projected note after archiving");
    let second = note(&store, "Second room, first service");
    assert_eq!(first.assignment, second.assignment);
    store.save_note(second.clone()).unwrap();
    let other_endpoint = "http://127.0.0.1:43128";
    store.configure(other_endpoint, "Note recovery fixture").unwrap();
    assert!(store.status().note_draft.is_none());
    let pair = store.next().unwrap().unwrap();
    let Operation::Pair { participant, .. } = &pair.operation else { panic!("Expected service pairing") };
    store.acknowledge(&pair, &RpcResponse::Paired { participant: participant.clone() }).unwrap();
    store.update_poll(second_view).unwrap();
    let third = note(&store, "Second room, second service");
    store.save_note(third.clone()).unwrap();
    drop(store);
    let reopened = Outbox::open(&path, Box::new(FixtureCredentials)).unwrap();
    assert_eq!(reopened.saved_note(ENDPOINT, room_id(), first.assignment).unwrap(), Some(first));
    assert_eq!(reopened.saved_note(ENDPOINT, second_room, second.assignment).unwrap(), Some(second));
    assert_eq!(reopened.saved_note(other_endpoint, second_room, third.assignment).unwrap(), Some(third));
    assert!(reopened.saved_note(other_endpoint, room_id(), note(&reopened, "").assignment).unwrap().is_none());
}

#[test]
fn legacy_singleton_with_ambiguous_game_scope_is_preserved_and_reported() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("room.sqlite3");
    let (mut store, mut room) = setup(&path);
    let original = note(&store, "Legacy note whose room cannot be proven");
    store.save_note(original).unwrap();
    let actor = store.status().profile.unwrap().id;
    room.apply(actor, room.revision(), 0, Action::End { reason: "Incomplete game".into() }).unwrap();
    room.apply(actor, room.revision(), 0, Action::NextGame).unwrap();
    start(&mut room, actor, 5);
    store.update_poll(room.view(actor).unwrap()).unwrap();
    drop(store);
    let db = Connection::open(&path).unwrap();
    let original_state: Vec<u8> = db.query_row("SELECT body FROM state", [], |row| row.get(0)).unwrap();
    db.execute("DROP TABLE local_assignment_notes", []).unwrap();
    assert!(Outbox::open(&path, Box::new(FixtureCredentials)).is_err());
    assert_eq!(db.query_row("SELECT body FROM state", [], |row| row.get::<_, Vec<u8>>(0)).unwrap(), original_state);
    assert_eq!(db.query_row("SELECT count(*) FROM local_assignment_notes", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
}
