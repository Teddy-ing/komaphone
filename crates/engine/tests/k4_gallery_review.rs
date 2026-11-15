//! Independent regressions for asynchronous source authorization and gallery identity.
#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;
use std::time::{Duration, Instant};

use photocraft_engine::Session;
use photocraft_local_store::StoreOptions;
use photocraft_party::room::{Action, Room};
use photocraft_party::{DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, RoomId};
use serde_json::{Value, json};

fn session(root: &Path) -> Session {
    let mut session = Session::new();
    session.enable_local_projects(root.join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    session
}

fn finish(session: &mut Session) -> Result<(), String> {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(20) {
        session.poll_local_projects();
        session.poll_gallery();
        if !session.gallery.busy() {
            return session.gallery.error.clone().map_or(Ok(()), Err);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err("Gallery worker did not finish".into())
}

fn gallery(session: &mut Session, command: &str, params: Value) {
    let _ = finish(session);
    assert!(!session.gallery.busy(), "previous gallery operation did not finish");
    assert_eq!(session.execute(command, params).unwrap()["pending"], true);
    finish(session).unwrap();
}

fn drawing(session: &mut Session, name: &str, color: &str) -> ProjectId {
    session.execute("komaphone.project.new", json!({"width":32,"height":32,"name":name})).unwrap();
    session.execute("paint.stroke", json!({"points":[[5,16],[27,16]],"size":8,"color":color})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    session.local_projects.status(session.active().unwrap().doc.id).project.unwrap()
}

fn participant(n: u16) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{n:032x}")).unwrap(),
        device: DeviceId::new(&format!("{:032x}", n + 100)).unwrap(),
        display_name: format!("Player {n}"),
    }
}

#[test]
fn replay_status_is_bounded_and_can_page_without_losing_edit_boundaries() {
    let mut app = Session::new();
    app.gallery.replay = Some(photocraft_engine::gallery_media::ReplayInfo {
        duration_ms: 1000,
        moments: (0..1000)
            .map(|revision| photocraft_engine::gallery_media::ReplayMoment {
                revision,
                elapsed_ms: revision,
                command: "paint.stroke".into(),
                kind: "checkpoint".into(),
            })
            .collect(),
        baseline_only: false,
        missing: false,
    });
    let first = app.execute("komaphone.gallery.status", json!({})).unwrap();
    assert_eq!(first["replay"]["moment_count"], 1000);
    assert_eq!(first["replay"]["moments"].as_array().unwrap().len(), 128);
    assert_eq!(first["replay"]["next"], 128);
    let second = app.execute("komaphone.gallery.status", json!({"replay_after":128,"replay_limit":512})).unwrap();
    assert_eq!(second["replay"]["moments"][0]["revision"], 128);
    assert_eq!(second["replay"]["next"], 640);
    let last = app.execute("komaphone.gallery.status", json!({"replay_after":640,"replay_limit":512})).unwrap();
    assert_eq!(last["replay"]["moments"].as_array().unwrap().len(), 360);
    assert!(last["replay"]["next"].is_null());
    for params in [json!({"replay_limit":0}), json!({"replay_limit":513}), json!({"replay_after":-1}), json!({"replay_after":"all"})] {
        assert!(app.execute("komaphone.gallery.status", params).is_err());
    }
}

#[test]
fn revoked_async_open_cannot_reuse_the_previous_authorized_source() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = session(dir.path());
    let safe = drawing(&mut app, "Independent drawing", "#0000ff");
    let assigned = drawing(&mut app, "Assigned drawing", "#ff0000");
    gallery(&mut app, "komaphone.gallery.open", json!({"entry":safe,"draft":true}));
    assert_eq!(app.gallery.active.as_ref().unwrap().id, safe);

    let host = participant(1);
    let guest = participant(2);
    let room_id = RoomId::new("11111111111111111111111111111111").unwrap();
    let game_id = GameId::new("22222222222222222222222222222222").unwrap();
    let mut room = Room::new(room_id, host.clone(), Mode::ExquisiteCorpse, None).unwrap();
    room.apply(guest.id, room.revision(), 0, Action::Join { participant: guest.clone() }).unwrap();
    for player in [&host, &guest] {
        room.apply(player.id, room.revision(), 0, Action::Ready { ready: true }).unwrap();
    }
    let mut projects: Vec<_> = (0..6u16).map(|index| ProjectId::new(&format!("{:032x}", 1000 + index)).unwrap()).collect();
    projects[0] = assigned;
    let next = projects[1];
    room.apply(host.id, room.revision(), 0, Action::Start { game: game_id, projects }).unwrap();
    let mut view = room.view(host.id).unwrap();
    view.assignment.as_mut().unwrap().assignment.project = assigned;
    app.rooms.status.profile = Some(host);
    app.rooms.status.room = Some(view);
    app.rooms.status.game_drafts = Some(photocraft_room_client::GameDrafts { room: room_id, game: game_id, projects: vec![assigned, next] });

    // The request is authorized now. Do not poll its response until the room has
    // advanced: the worker may finish at any time, but Session cannot install it yet.
    app.execute("komaphone.gallery.open", json!({"entry":assigned,"draft":true})).unwrap();
    app.rooms.status.room.as_mut().unwrap().assignment.as_mut().unwrap().assignment.project = next;
    assert!(finish(&mut app).unwrap_err().contains("private"));
    assert!(app.gallery.active.is_none(), "the prior source must not authorize actions on the worker's rejected source");
    assert!(app.gallery.image.is_none());
    assert!(app.gallery.replay.is_none());
    let export = dir.path().join("forbidden.png");
    for (command, params) in [
        ("komaphone.gallery.render", json!({"chain":0,"frame":0})),
        ("komaphone.gallery.export", json!({"format":"png","path":export,"chain":0,"frame":0,"fps":6,"looping":true})),
        ("komaphone.gallery.remix", json!({"chain":0})),
        ("komaphone.gallery.previewGif", json!({"chain":0,"frame":0,"fps":6,"looping":true})),
    ] {
        assert!(app.execute(command, params).is_err(), "rejected source leaked through {command}");
    }
    assert!(!export.exists());
    // A subsequent explicitly authorized open synchronizes both sides again.
    gallery(&mut app, "komaphone.gallery.open", json!({"entry":safe,"draft":true}));
    gallery(&mut app, "komaphone.gallery.render", json!({"chain":0,"frame":0}));
    assert_eq!(app.gallery.active.as_ref().unwrap().id, safe);
}
