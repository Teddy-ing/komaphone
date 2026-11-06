//! Independent K3 engine boundary tests. The real HTTP/native suites separately
//! prove authority and UI; these use explicit room projections to attack dispatch.
#![cfg(not(target_arch = "wasm32"))]

use photocraft_engine::Session;
use photocraft_party::{
    DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, RoomId,
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
fn fixture(root: &std::path::Path, mode: Mode) -> Session {
    let host = person(1);
    let guest = person(2);
    let mut room = Room::new(RoomId::new(&format!("{:032x}", 1000)).unwrap(), host.clone(), mode, None).unwrap();
    apply(&mut room, guest.id, Action::Join { participant: guest.clone() });
    for participant in [&host, &guest] {
        apply(&mut room, participant.id, Action::Ready { ready: true });
    }
    apply(
        &mut room,
        host.id,
        Action::Start {
            game: GameId::new(&format!("{:032x}", 2000)).unwrap(),
            projects: (0..usize::from(mode.rounds(2).unwrap()) * 2).map(|n| ProjectId::new(&format!("{:032x}", 3000 + n)).unwrap()).collect(),
        },
    );
    let mut session = Session::new();
    session.enable_local_projects(root.join("drafts.sqlite3"), Default::default()).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    session.rooms.status.profile = Some(host.clone());
    session.rooms.status.room = Some(room.view(host.id).unwrap());
    session.open_room_draft().unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    session
}

#[test]
fn current_assignment_canvas_cannot_be_resized_by_advanced_commands() {
    for mode in [Mode::Animation { rotations: 2 }, Mode::ExquisiteCorpse] {
        let directory = tempfile::tempdir().unwrap();
        let mut session = fixture(directory.path(), mode);
        let original = session.active().unwrap().doc.clone();
        for (command, params) in [
            ("image.canvasSize", json!({"width":64,"height":64})),
            ("image.imageSize", json!({"width":64,"height":64})),
            ("image.crop", json!({"x":0,"y":0,"width":64,"height":64})),
            ("image.rotation.arbitrary", json!({"angle":90,"interpolation":"nearest"})),
        ] {
            let error = session.execute(command, params).unwrap_err();
            assert!(error.to_string().to_ascii_lowercase().contains("assign"), "Expected assignment guard: {command}: {error}");
            assert_eq!(*session.active().unwrap().doc, *original, "A refused {command} changed the document");
        }
        // Local artwork outside the assigned frame stays compatible with the editor.
        session.execute("komaphone.project.new", json!({"width":32,"height":32,"name":"Private local work"})).unwrap();
        session.execute("image.canvasSize", json!({"width":64,"height":48})).unwrap();
        assert_eq!((session.active().unwrap().doc.size.width, session.active().unwrap().doc.size.height), (64, 48));
    }
}

#[test]
fn explicit_layer_targets_and_background_jobs_cannot_edit_a_submitted_assignment() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = fixture(directory.path(), Mode::ExquisiteCorpse);
    session.execute("layer.new.layer", json!({"name":"Assigned layer"})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let layer = session.active().unwrap().active_layer.unwrap().0;
    session.rooms.status.room.as_mut().unwrap().assignment.as_mut().unwrap().submitted = true;
    let original = session.active().unwrap().doc.clone();
    for (command, params) in [
        ("layer.renameLayer", json!({"layer":layer,"name":"Bypass attempt"})),
        ("layer.setProps", json!({"layer":layer,"visible":false})),
        ("layer.hideLayers", json!({"layer":layer})),
        ("paint.stroke", json!({"layer":layer,"points":[[10,10],[40,10]],"size":12})),
        ("edit.undo", json!({})),
    ] {
        assert!(session.execute(command, params.clone()).is_err(), "{command} bypassed the accepted freeze");
        assert!(!session.is_enabled_with(command, &params));
        assert_eq!(*session.active().unwrap().doc, *original);
    }
    assert!(session.start("image.canvasSize", json!({"width":64,"height":64})).is_err());
    assert_eq!(*session.active().unwrap().doc, *original);
    session.execute("document.inspect", json!({})).unwrap();
}

#[test]
fn a_running_pixel_job_cannot_commit_after_the_assignment_freezes() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = fixture(directory.path(), Mode::Animation { rotations: 1 });
    session.execute("paint.stroke", json!({"points":[[100,100],[300,100]],"size":30,"hardness":1,"color":"#204080"})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let before = session.active().unwrap().doc.clone();
    let job = match session.start("filter.blur.gaussianBlur", json!({"radius":8})).unwrap() {
        photocraft_engine::jobs::Started::Job(job) => job,
        photocraft_engine::jobs::Started::Done(_) => panic!("Fixture must launch a real background pixel job"),
    };
    session.rooms.status.room.as_mut().unwrap().assignment.as_mut().unwrap().submitted = true;
    let error = session.wait_job(job).unwrap_err();
    assert!(error.to_string().contains("frozen"), "Expected apply-time assignment freeze, got {error}");
    assert_eq!(*session.active().unwrap().doc, *before);
}

#[test]
fn previous_turn_drafts_remain_readable_but_frozen_after_local_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = fixture(directory.path(), Mode::ExquisiteCorpse);
    session.execute("paint.stroke", json!({"points":[[100,100],[300,100]],"size":30,"color":"#204080"})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let old_project = session.local_projects.status(session.active().unwrap().doc.id).project.unwrap();
    let original = session.execute("document.pixel", json!({"x":200,"y":100})).unwrap();
    let next_project = ProjectId::new(&format!("{:032x}", 4000)).unwrap();
    let room = session.rooms.status.room.as_mut().unwrap();
    let assignment = room.assignment.as_mut().unwrap();
    let game = assignment.assignment.id.game;
    assignment.assignment.id.round = 1;
    assignment.assignment.id.chain = 1;
    assignment.assignment.project = next_project;
    room.round = 1;
    room.epoch += 1;
    session.rooms.status.game_drafts = Some(photocraft_room_client::GameDrafts { room: room.id, game, projects: vec![old_project, next_project] });
    session.open_room_draft().unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let projection = session.rooms.status.clone();
    drop(session);

    // Real local drawing storage is reopened; this focused engine test restores a
    // projection fixture. assignment_scope.rs independently proves SQLite scope persistence.
    let mut reopened = Session::new();
    reopened.enable_local_projects(directory.path().join("drafts.sqlite3"), Default::default()).unwrap();
    reopened.flush_local_projects(Duration::from_secs(20)).unwrap();
    reopened.rooms.status = projection;
    reopened.open_local_project(old_project).unwrap();
    reopened.flush_local_projects(Duration::from_secs(20)).unwrap();
    assert_eq!(reopened.execute("document.pixel", json!({"x":200,"y":100})).unwrap(), original);
    assert!(reopened.execute("layer.new.layer", json!({"name":"Past-turn mutation"})).is_err());
    assert!(reopened.execute("edit.undo", json!({})).is_err());
    reopened.open_room_draft().unwrap();
    reopened.flush_local_projects(Duration::from_secs(20)).unwrap();
    reopened.execute("layer.new.layer", json!({"name":"Current assignment remains editable"})).unwrap();
    reopened.open_local_project(old_project).unwrap();
    reopened.rooms.status.room.as_mut().unwrap().phase = photocraft_party::room::Phase::Reveal;
    reopened.rooms.status.room.as_mut().unwrap().assignment = None;
    assert!(reopened.execute("layer.new.layer", json!({})).is_err(), "Reveal originals stay frozen");
    assert_eq!(reopened.execute("document.pixel", json!({"x":200,"y":100})).unwrap(), original);
    reopened.execute("komaphone.project.new", json!({"width":32,"height":32,"name":"Independent local drawing"})).unwrap();
    reopened.execute("layer.new.layer", json!({"name":"Local editing remains available"})).unwrap();
}

#[test]
fn host_can_resume_after_explicitly_skipping_an_offline_assignment() {
    let directory = tempfile::tempdir().unwrap();
    let host = person(1);
    let guest = person(2);
    let mut room = Room::new(RoomId::new(&format!("{:032x}", 5000)).unwrap(), host.clone(), Mode::Animation { rotations: 1 }, None).unwrap();
    apply(&mut room, guest.id, Action::Join { participant: guest.clone() });
    for participant in [&host, &guest] {
        apply(&mut room, participant.id, Action::Ready { ready: true });
    }
    apply(
        &mut room,
        host.id,
        Action::Start {
            game: GameId::new(&format!("{:032x}", 6000)).unwrap(),
            projects: (0..4).map(|index| ProjectId::new(&format!("{:032x}", 7000 + index)).unwrap()).collect(),
        },
    );
    let mut session = Session::new();
    // Native worker availability is real, but this fixture never Connects, stores
    // credentials, or claims a remote mutation. Domain assertions below provide
    // the authority check; the native harness separately drives Skip and Resume.
    session.enable_rooms(directory.path().join("rooms.sqlite3"), Default::default()).unwrap();
    session.rooms.status.paired = true;
    session.rooms.status.profile = Some(host.clone());
    session.rooms.status.room = Some(room.view(host.id).unwrap());
    assert!(!session.is_enabled("komaphone.room.resume"), "An unpaused game cannot resume");

    room.set_connected(guest.id, false, 10).unwrap();
    let before = room.snapshot_bytes().unwrap();
    assert!(room.apply(host.id, room.revision(), 10, Action::Resume).is_err(), "Authority must reject an unresolved offline assignment");
    assert_eq!(room.snapshot_bytes().unwrap(), before);
    room.apply(host.id, room.revision(), 10, Action::Skip { participant: guest.id, reason: "Explicitly keep an offline placeholder".into() }).unwrap();
    let skipped = room.view(host.id).unwrap();
    assert!(skipped.paused.is_some());
    assert!(skipped.members.iter().any(|member| member.participant.id == guest.id && !member.connected));
    session.rooms.status.room = Some(skipped);
    assert!(session.is_enabled("komaphone.room.resume"), "The UI must let the host ask authority to resume after Skip");
    session.rooms.status.profile = Some(guest.clone());
    session.rooms.status.room = Some(room.view(guest.id).unwrap());
    assert!(!session.is_enabled("komaphone.room.resume"), "A non-host cannot resume");

    room.apply(host.id, room.revision(), 11, Action::Resume).unwrap();
    let resumed = room.view(host.id).unwrap();
    assert!(resumed.paused.is_none());
    assert!(resumed.members.iter().any(|member| member.participant.id == guest.id && !member.connected));
    session.rooms.status.profile = Some(host);
    session.rooms.status.room = Some(resumed);
    assert!(!session.is_enabled("komaphone.room.resume"), "Resume becomes unavailable after success");
}
