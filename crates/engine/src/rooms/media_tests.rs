//! Consumer-side note authorization tests. A queue may outlive a room transition;
//! room IDs and phase epochs alone cannot identify a game.
use super::*;
use photocraft_party::room::{Assignment, MemberView, Phase, RevealedEntry, RoomView};
use photocraft_party::{CheckpointId, CheckpointRef, DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, ReplayId, ReplayRef};

fn participant(index: u8) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{:032x}", index + 1)).unwrap(),
        device: DeviceId::new(&format!("{:032x}", index + 11)).unwrap(),
        display_name: format!("Artist {}", index + 1),
    }
}
fn game(number: u8) -> GameId {
    GameId::new(&format!("{number:032x}")).unwrap()
}
fn assignment(game: GameId, round: u8) -> Assignment {
    Assignment {
        id: AssignmentId { game, chain: 0, round },
        player: participant(round % 2).id,
        project: ProjectId::new(&format!("{:032x}", 100 + u16::from(round))).unwrap(),
    }
}
fn status(game: GameId) -> photocraft_room_client::ClientStatus {
    let checkpoint =
        CheckpointRef { id: CheckpointId::new(&"1".repeat(32)).unwrap(), asset: AssetRef { hash: AssetHash::new(&"1".repeat(64)).unwrap(), bytes: 100 } };
    photocraft_room_client::ClientStatus {
        profile: Some(participant(0)),
        room: Some(RoomView {
            id: RoomId::new(&"b".repeat(32)).unwrap(),
            revision: 30,
            epoch: 4,
            phase: Phase::Reveal,
            round: 3,
            mode: Mode::Animation { rotations: 2 },
            timer_ms: None,
            moderator: participant(0).id,
            members: (0..2).map(|index| MemberView { participant: participant(index), connected: true, ready: true }).collect(),
            waiting: vec![],
            paused: None,
            deadline_ms: None,
            assignment: None,
            reveal: vec![
                RevealedEntry::Artwork {
                    assignment: assignment(game, 0),
                    revision: 1,
                    image: AssetRef { hash: AssetHash::new(&"2".repeat(64)).unwrap(), bytes: 100 },
                    checkpoint,
                    replay: Box::new(ReplayRef {
                        id: ReplayId::new(&"3".repeat(32)).unwrap(),
                        manifest: None,
                        baseline: checkpoint,
                        end_event: None,
                        event_count: 0,
                        duration_ms: 0,
                    }),
                    assets: vec![checkpoint.asset],
                },
                RevealedEntry::Missing { assignment: assignment(game, 1), reason: "Explicit skip".into() },
            ],
        }),
        ..Default::default()
    }
}

#[test]
fn next_game_same_room_and_epoch_prunes_previously_loaded_note_metadata() {
    let first = status(game(1));
    let room = first.room.as_ref().unwrap();
    let mut media = RoomMedia::default();
    media.accept_notes(&first, room.id, game(1), room.epoch, vec![(assignment(game(1), 0).id, "First game's instruction".into())]);
    assert!(media.notes_loaded(room.id, room.epoch));
    assert_eq!(media.notes.len(), 1);
    let second = status(game(2));
    assert_eq!(first.room.as_ref().unwrap().id, second.room.as_ref().unwrap().id);
    assert_eq!(first.room.as_ref().unwrap().epoch, second.room.as_ref().unwrap().epoch);
    media.reconcile(&second);
    assert!(media.notes.is_empty());
    assert!(!media.notes_loaded(room.id, room.epoch));
}

#[test]
fn delayed_previous_game_events_including_empty_notes_cannot_acknowledge_new_game() {
    let current = status(game(2));
    let room = current.room.as_ref().unwrap();
    for delayed in [vec![], vec![(assignment(game(1), 0).id, "Old event".into())]] {
        let mut media = RoomMedia::default();
        media.accept_notes(&current, room.id, game(1), room.epoch, delayed);
        assert!(media.notes.is_empty());
        assert!(!media.notes_loaded(room.id, room.epoch), "Even an empty old response cannot mark this game loaded");
    }
    let mut media = RoomMedia::default();
    let expected = vec![(assignment(game(2), 0).id, "Current instruction".into())];
    media.accept_notes(&current, room.id, game(2), room.epoch, expected.clone());
    media.accept_notes(&current, room.id, game(1), room.epoch, vec![]);
    assert_eq!(media.notes, expected);
    assert!(media.notes_loaded(room.id, room.epoch));
}

#[test]
fn missing_parts_foreign_assignments_and_invalid_text_cannot_enter_notes_cache() {
    let current = status(game(2));
    let room = current.room.as_ref().unwrap();
    for invalid in [
        vec![(assignment(game(1), 0).id, "Another game".into())],
        vec![(assignment(game(2), 1).id, "A missing part has no note".into())],
        vec![(assignment(game(2), 0).id, "x".repeat(photocraft_party::MAX_NOTE_BYTES + 1))],
        vec![(assignment(game(2), 0).id, "embedded\u{0000}control".into())],
    ] {
        let mut media = RoomMedia::default();
        media.accept_notes(&current, room.id, game(2), room.epoch, invalid);
        assert!(media.notes.is_empty());
        assert!(!media.notes_loaded(room.id, room.epoch));
    }
    let mut media = RoomMedia::default();
    media.accept_notes(&current, room.id, game(2), room.epoch, vec![]);
    assert!(media.notes.is_empty());
    assert!(media.notes_loaded(room.id, room.epoch), "An explicitly fetched empty current response is a valid completed load");
}

#[test]
fn membership_or_phase_loss_clears_loaded_notes_and_rejects_queued_responses() {
    let original = status(game(2));
    let room = original.room.as_ref().unwrap();
    for leave_membership in [false, true] {
        let mut media = RoomMedia::default();
        let notes = vec![(assignment(game(2), 0).id, "Private revealed metadata".into())];
        media.accept_notes(&original, room.id, game(2), room.epoch, notes.clone());
        let mut changed = original.clone();
        if leave_membership {
            changed.profile = Some(participant(2));
        } else {
            changed.room.as_mut().unwrap().phase = Phase::Drawing;
        }
        media.reconcile(&changed);
        assert!(media.notes.is_empty());
        assert!(!media.notes_loaded(room.id, room.epoch));
        media.accept_notes(&changed, room.id, game(2), room.epoch, notes);
        assert!(media.notes.is_empty());
        assert!(!media.notes_loaded(room.id, room.epoch));
    }
}
