//! Engine validation at the timed-room boundary. These synthetic room projections test
//! local revision/tab guards; actual network acceptance is tested by the native K2 harness.
#![cfg(not(target_arch = "wasm32"))]

use photocraft_engine::{Session, rooms::FreezePin};
use photocraft_party::{
    AssetHash, AssetRef, CheckpointId, CheckpointRef, DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, ReplayId, ReplayRef, RoomId, Submission,
    SubmissionId,
    room::{Action, Room},
};
use serde_json::json;
use std::time::Duration;

fn person(n: u16) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{n:032x}")).unwrap(),
        device: DeviceId::new(&format!("{:032x}", n + 100)).unwrap(),
        display_name: format!("Player {n}"),
    }
}
fn apply(room: &mut Room, actor: ParticipantId, action: Action) {
    room.apply(actor, room.revision(), 0, action).unwrap();
}
fn fixture(root: &std::path::Path) -> (Session, Room, Participant) {
    let host = person(1);
    let guest = person(2);
    let mut room = Room::new(RoomId::new(&format!("{:032x}", 1000)).unwrap(), host.clone(), Mode::ExquisiteCorpse, Some(1000)).unwrap();
    apply(&mut room, guest.id, Action::Join { participant: guest.clone() });
    for participant in [&host, &guest] {
        apply(&mut room, participant.id, Action::Ready { ready: true });
    }
    apply(
        &mut room,
        host.id,
        Action::Start {
            game: GameId::new(&format!("{:032x}", 2000)).unwrap(),
            projects: (0..6).map(|n| ProjectId::new(&format!("{:032x}", 3000 + n)).unwrap()).collect(),
        },
    );
    let mut session = Session::new();
    session.enable_local_projects(root.join("drafts.sqlite3"), Default::default()).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    session.rooms.status.profile = Some(host.clone());
    session.rooms.status.room = Some(room.view(host.id).unwrap());
    session.open_room_draft().unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    (session, room, host)
}
fn frozen_baseline(room: &Room, author: ParticipantId) -> Submission {
    let assignment = room.view(author).unwrap().assignment.unwrap().assignment;
    let asset = |n| AssetRef { hash: AssetHash::new(&format!("{n:064x}")).unwrap(), bytes: 4 };
    let checkpoint = CheckpointRef { id: CheckpointId::new(&format!("{:032x}", 50)).unwrap(), asset: asset(1) };
    Submission {
        id: SubmissionId::new(&format!("{:032x}", 60)).unwrap(),
        assignment: assignment.id,
        author,
        project: assignment.project,
        revision: 1,
        image: asset(2),
        checkpoint,
        replay: ReplayRef {
            id: ReplayId::new(&format!("{:032x}", 70)).unwrap(),
            manifest: None,
            baseline: checkpoint,
            end_event: None,
            event_count: 0,
            duration_ms: 0,
        },
        assets: vec![asset(1), asset(2), asset(3)],
        note: String::new(),
        seam: Some(asset(3)),
    }
}

#[test]
fn pinned_durable_revision_remains_valid_after_later_ack_and_tab_switch() {
    let directory = tempfile::tempdir().unwrap();
    let (mut session, mut room, host) = fixture(directory.path());
    let frozen = frozen_baseline(&room, host.id);
    let assigned_document = session.active().unwrap().doc.id;
    session.execute("layer.new.layer", json!({"name":"Later acknowledged local work"})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    assert_eq!(session.local_projects.status(assigned_document).local_revision, 2);
    session.execute("komaphone.project.new", json!({"width":32,"height":32,"name":"Another tab"})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    assert_ne!(session.active().unwrap().doc.id, assigned_document);
    room.tick(1000).unwrap();
    session.rooms.status.room = Some(room.view(host.id).unwrap());
    session.rooms.status.freeze_pin =
        Some(FreezePin { room: room.id(), epoch: room.epoch(), assignment: frozen.assignment, project: frozen.project, revision: frozen.revision });
    session.enable_rooms(directory.path().join("rooms.sqlite3"), Default::default()).unwrap();
    assert!(session.queue_room_submission(frozen.clone(), Vec::new(), false).is_err(), "Normal manual submissions still require their current saved revision");
    let mut future = frozen.clone();
    future.revision = 3;
    assert!(session.queue_room_submission(future, Vec::new(), true).is_err(), "Future/unacknowledged revisions cannot be frozen");
    let mut later_ack = frozen.clone();
    later_ack.revision = 2;
    assert!(session.queue_room_submission(later_ack, Vec::new(), true).is_err(), "The first pin cannot be replaced by a later acknowledged revision");
    // This proves only the validation seam can queue the pinned revision. No credentials
    // or remote submission are created by this synthetic worker with no Connect command.
    session.queue_room_submission(frozen, Vec::new(), true).unwrap();
}

#[test]
fn startup_draft_listing_can_validate_frozen_revision_without_opening_its_tab() {
    let directory = tempfile::tempdir().unwrap();
    let (session, mut room, host) = fixture(directory.path());
    let frozen = frozen_baseline(&room, host.id);
    drop(session);
    let mut restarted = Session::new();
    restarted.enable_local_projects(directory.path().join("drafts.sqlite3"), Default::default()).unwrap();
    restarted.flush_local_projects(Duration::from_secs(20)).unwrap();
    assert!(restarted.documents().is_empty());
    assert!(restarted.local_projects.drafts.iter().any(|draft| draft.project == frozen.project && draft.revision == 1));
    room.tick(1000).unwrap();
    restarted.rooms.status.profile = Some(host.clone());
    restarted.rooms.status.room = Some(room.view(host.id).unwrap());
    restarted.rooms.status.freeze_pin =
        Some(FreezePin { room: room.id(), epoch: room.epoch(), assignment: frozen.assignment, project: frozen.project, revision: frozen.revision });
    restarted.enable_rooms(directory.path().join("rooms.sqlite3"), Default::default()).unwrap();
    restarted.queue_room_submission(frozen, Vec::new(), true).unwrap();
}

#[test]
fn transfer_and_accepted_assignment_block_editor_commands_but_keep_other_drafts_editable() {
    let directory = tempfile::tempdir().unwrap();
    let (mut session, mut room, host) = fixture(directory.path());
    let original = session.active().unwrap().doc.clone();
    room.tick(1000).unwrap();
    session.rooms.status.room = Some(room.view(host.id).unwrap());
    for command in ["paint.stroke", "layer.new.layer", "edit.undo"] {
        let error = session.execute(command, json!({})).unwrap_err();
        assert!(error.to_string().contains("frozen"), "Expected room freeze guard for {command}: {error}");
    }
    assert_eq!(*session.active().unwrap().doc, *original);
    let accepted = session.rooms.status.room.as_mut().unwrap();
    accepted.phase = photocraft_party::room::Phase::Drawing;
    accepted.assignment.as_mut().unwrap().submitted = true;
    assert!(session.execute("layer.new.layer", json!({})).unwrap_err().to_string().contains("frozen"));
    session.execute("document.inspect", json!({})).unwrap();
    session.execute("komaphone.project.new", json!({"width":32,"height":32,"name":"Private offline work"})).unwrap();
    session.execute("layer.new.layer", json!({"name":"Still editable"})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    assert_eq!(session.active().unwrap().doc.layers.len(), 2);
}

#[test]
fn deadline_pin_can_only_queue_an_existing_acknowledged_local_revision() {
    let directory = tempfile::tempdir().unwrap();
    let (mut session, mut room, host) = fixture(directory.path());
    room.tick(1000).unwrap();
    let assignment = room.view(host.id).unwrap().assignment.unwrap().assignment;
    session.rooms.status.room = Some(room.view(host.id).unwrap());
    session.enable_rooms(directory.path().join("rooms.sqlite3"), Default::default()).unwrap();
    let pin = FreezePin { room: room.id(), epoch: room.epoch(), assignment: assignment.id, project: assignment.project, revision: 1 };
    for revision in [0, 2, u64::MAX] {
        assert!(session.pin_room_freeze(FreezePin { revision, ..pin.clone() }).is_err());
    }
    let unknown = ProjectId::new(&format!("{:032x}", 9999)).unwrap();
    assert!(session.pin_room_freeze(FreezePin { project: unknown, ..pin.clone() }).is_err());
    session.pin_room_freeze(pin).unwrap();
}
