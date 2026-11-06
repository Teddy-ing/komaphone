//! Independent SQLite assignment-scope tests with synthetic authorized projections.
//! These use real SQLite/reopen and fixture credentials, not a service or native UI.
#![cfg(not(target_arch = "wasm32"))]
use photocraft_party::protocol::{Operation, RpcResponse};
use photocraft_party::room::{Action, Phase, Room};
use photocraft_party::{DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, RoomId};
use photocraft_room_client::{CredentialStore, Outbox, Result};
use std::path::Path;

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

fn setup(path: &Path, mode: Mode, players: usize) -> (Outbox, Room) {
    let mut store = Outbox::open(path, Box::new(FixtureCredentials)).unwrap();
    store.configure("http://127.0.0.1:43127", "Presentation fixture").unwrap();
    let pair = store.next().unwrap().unwrap();
    let Operation::Pair { participant, .. } = &pair.operation else { panic!("Pairing required") };
    store.acknowledge(&pair, &RpcResponse::Paired { participant: participant.clone() }).unwrap();
    let mut room = Room::new(RoomId::new(&"1".repeat(32)).unwrap(), participant.clone(), mode, None).unwrap();
    for number in 2..=players {
        let guest = Participant {
            id: ParticipantId::new(&format!("{number:032x}")).unwrap(),
            device: DeviceId::new(&format!("{:032x}", 100 + number)).unwrap(),
            display_name: format!("Guest {number}"),
        };
        room.apply(guest.id, room.revision(), 0, Action::Join { participant: guest }).unwrap();
    }
    let actors: Vec<_> = room.view(participant.id).unwrap().members.iter().map(|member| member.participant.id).collect();
    for actor in actors {
        room.apply(actor, room.revision(), 0, Action::Ready { ready: true }).unwrap();
    }
    room.apply(
        participant.id,
        room.revision(),
        0,
        Action::Start {
            game: GameId::new(&"4".repeat(32)).unwrap(),
            projects: (10..10 + usize::from(mode.rounds(players).unwrap()) * players).map(|n| ProjectId::new(&format!("{n:032x}")).unwrap()).collect(),
        },
    )
    .unwrap();
    store.update_poll(room.view(participant.id).unwrap()).unwrap();
    (store, room)
}

fn next_projection(store: &Outbox, index: u8) -> photocraft_party::room::RoomView {
    let mut view = store.status().room.unwrap();
    view.round = index;
    view.epoch += 1;
    view.revision += 1;
    let assignment = &mut view.assignment.as_mut().unwrap().assignment;
    assignment.id.round = index;
    assignment.id.chain = ((view.members.len() - usize::from(index) % view.members.len()) % view.members.len()) as u8;
    assignment.project = ProjectId::new(&format!("{:032x}", 100 + u16::from(index))).unwrap();
    view
}

#[test]
fn own_turn_scope_is_atomic_durable_and_retained_during_reveal() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rooms.sqlite3");
    let (mut store, _) = setup(&path, Mode::Animation { rotations: 2 }, 2);
    let original = store.status().room.unwrap().assignment.unwrap().assignment.project;
    assert_eq!(store.status().game_drafts.as_ref().unwrap().projects, vec![original]);
    for index in 1..=3 {
        store.update_poll(next_projection(&store, index)).unwrap();
    }
    let expected = store.status().game_drafts.unwrap();
    assert_eq!(expected.projects.len(), 4);
    assert_eq!(expected.projects[0], original);
    drop(store);
    let mut reopened = Outbox::open(&path, Box::new(FixtureCredentials)).unwrap();
    assert_eq!(reopened.status().game_drafts, Some(expected.clone()));
    let mut reveal = reopened.status().room.unwrap();
    reveal.phase = Phase::Reveal;
    reveal.assignment = None;
    reveal.revision += 1;
    reopened.update_poll(reveal).unwrap();
    drop(reopened);
    let mut reopened = Outbox::open(&path, Box::new(FixtureCredentials)).unwrap();
    assert_eq!(reopened.status().game_drafts, Some(expected));
    let mut lobby = reopened.status().room.unwrap();
    lobby.phase = Phase::Lobby;
    lobby.revision += 1;
    reopened.update_poll(lobby).unwrap();
    drop(reopened);
    assert!(Outbox::open(&path, Box::new(FixtureCredentials)).unwrap().status().game_drafts.is_none());
}

#[test]
fn rejected_or_failed_projection_never_changes_the_prior_saved_scope() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rooms.sqlite3");
    let (mut store, _) = setup(&path, Mode::ExquisiteCorpse, 2);
    let expected = store.status();
    let mut foreign = next_projection(&store, 1);
    foreign.assignment.as_mut().unwrap().assignment.player = ParticipantId::new(&"2".repeat(32)).unwrap();
    assert!(store.update_poll(foreign).is_err());
    assert_eq!(store.status().room, expected.room);
    assert_eq!(store.status().game_drafts, expected.game_drafts);
    let lock = rusqlite::Connection::open(&path).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(store.update_poll(next_projection(&store, 1)).is_err());
    assert_eq!(store.status().room, expected.room);
    assert_eq!(store.status().game_drafts, expected.game_drafts);
    lock.execute_batch("ROLLBACK").unwrap();
    drop(store);
    assert_eq!(Outbox::open(&path, Box::new(FixtureCredentials)).unwrap().status().game_drafts, expected.game_drafts);
}

#[test]
fn a_waiting_profile_cannot_acquire_assignment_scope_from_a_malformed_projection() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, _) = setup(&directory.path().join("rooms.sqlite3"), Mode::ExquisiteCorpse, 2);
    let expected = store.status();
    let actor = expected.profile.as_ref().unwrap().id;
    let mut waiting = next_projection(&store, 1);
    let member = waiting.members.iter().position(|member| member.participant.id == actor).unwrap();
    waiting.waiting.push(waiting.members.remove(member));
    assert!(store.update_poll(waiting).is_err());
    assert_eq!(store.status().room, expected.room);
    assert_eq!(store.status().game_drafts, expected.game_drafts);
}

#[test]
fn projection_scope_rejects_a_thirteenth_distinct_project_and_new_game_resets_it() {
    let directory = tempfile::tempdir().unwrap();
    let (mut store, _) = setup(&directory.path().join("rooms.sqlite3"), Mode::Animation { rotations: 4 }, 3);
    // The maximum schedule is three players × four rotations = twelve own frames.
    for index in 1..12 {
        store.update_poll(next_projection(&store, index)).unwrap();
    }
    let expected = store.status().game_drafts;
    assert_eq!(expected.as_ref().unwrap().projects.len(), 12);
    assert!(store.update_poll(next_projection(&store, 12)).is_err());
    assert_eq!(store.status().game_drafts, expected);
    let mut fresh = next_projection(&store, 0);
    fresh.assignment.as_mut().unwrap().assignment.id.game = GameId::new(&"5".repeat(32)).unwrap();
    let project = fresh.assignment.as_ref().unwrap().assignment.project;
    store.update_poll(fresh).unwrap();
    assert_eq!(store.status().game_drafts.unwrap().projects, vec![project]);
}
