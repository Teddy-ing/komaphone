//! Independent K4 native command/worker acceptance. These exercise real durable
//! stores and portable files, not UI mocks; native visual/network gates are separate.
#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;
use std::time::{Duration, Instant};

use photocraft_engine::{Session, gallery_media};
use photocraft_local_store::{Store, StoreOptions};
use photocraft_party::room::{Action, Room};
use photocraft_party::{DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, RoomId};
use serde_json::{Value, json};

fn session(root: &Path) -> Session {
    let mut session = Session::new();
    session.enable_local_projects(root.join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    session
}

fn gallery(session: &mut Session, command: &str, params: Value) -> Result<(), String> {
    // Native setup and post-command refreshes use the same real worker. Drain
    // those before dispatching the next user action, as the UI does each frame.
    let start = Instant::now();
    loop {
        session.poll_local_projects();
        session.poll_gallery();
        if !session.gallery.busy() {
            break;
        }
        if start.elapsed() >= Duration::from_secs(30) {
            return Err(format!("gallery did not become idle before: {command}"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let result = session.execute(command, params).map_err(|error| error.to_string())?;
    assert_eq!(result["pending"], true);
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        session.poll_local_projects();
        session.poll_gallery();
        if !session.gallery.busy() {
            return session.gallery.error.clone().map_or(Ok(()), Err);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err(format!("gallery operation timed out: {command}"))
}

fn drawing(session: &mut Session) -> ProjectId {
    session.execute("komaphone.project.new", json!({"width":64,"height":48,"name":"Independent original"})).unwrap();
    session.execute("layer.new.layer", json!({"name":"Ink"})).unwrap();
    session.execute("paint.stroke", json!({"points":[[8,20],[50,20]],"size":8,"hardness":1,"color":"#2060c0"})).unwrap();
    session.execute("layer.setProps", json!({"opacity":0.4})).unwrap();
    session.execute("edit.undo", json!({})).unwrap();
    session.execute("edit.redo", json!({})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    session.local_projects.status(session.active().unwrap().doc.id).project.unwrap()
}

fn assert_import_list_matches_durable_state(app: &Session, root: &Path) {
    let active = app.gallery.active.as_ref().unwrap();
    assert!(!app.gallery.active_draft);
    let saved = photocraft_local_store::gallery::GalleryStore::open(root.join("gallery.sqlite3"), StoreOptions::default()).unwrap();
    let expected = saved.list(true).unwrap().into_iter().find(|entry| entry.id == active.id).unwrap();
    let item = app
        .gallery
        .items
        .iter()
        .find(|item| (item.id, item.draft) == (active.id, false))
        .expect("completed import must publish its original gallery row before another command");
    assert_eq!(item.title, expected.title);
    assert_eq!(item.title, active.title);
    assert_eq!(item.kind, "project");
    assert_eq!(item.authors, expected.authors.iter().map(|author| author.display_name.clone()).collect::<Vec<_>>());
    assert_eq!(item.updated_at, expected.updated_at);
    assert_eq!(item.favorite, expected.favorite);
    assert_eq!(item.trashed, expected.trashed);
    assert_eq!(item.complete, expected.complete);
    assert_eq!(item.source_projects, expected.source_projects);
    let local = Store::open(root.join("drafts.sqlite3"), StoreOptions::default()).unwrap().usage().unwrap();
    let saved = saved.usage().unwrap();
    assert!(saved.stored_bytes > 0);
    assert_eq!(app.gallery.usage_bytes, local.stored_bytes + saved.stored_bytes, "import completion must refresh the displayed storage usage");
    assert_eq!(app.gallery.quota_bytes, local.quota_bytes + saved.quota_bytes);
}

#[test]
fn command_replay_archive_remix_and_backup_restore_keep_exact_edit_history() {
    let dir = tempfile::tempdir().unwrap();
    let source_root = dir.path().join("source");
    let mut source = session(&source_root);
    let original = drawing(&mut source);
    let before = source.active().unwrap().doc.clone();
    let canonical = gallery_media::render_document(&before).unwrap();
    gallery(&mut source, "komaphone.gallery.open", json!({"entry":original,"draft":true})).unwrap();
    assert_eq!(source.gallery.image.as_deref(), Some(&canonical));
    let replay = source.gallery.replay.as_ref().unwrap().clone();
    assert!(replay.moments.iter().any(|moment| moment.kind == "undo"));
    assert!(replay.moments.iter().any(|moment| moment.kind == "redo"));
    let native = Store::open(source_root.join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    for (index, moment) in replay.moments.iter().enumerate() {
        gallery(&mut source, "komaphone.gallery.render", json!({"chain":0,"frame":0,"step":index})).unwrap();
        let (document, _) = native.load_revision(original, moment.revision).unwrap();
        assert_eq!(source.gallery.image.as_deref(), Some(&gallery_media::render_document(&document).unwrap()), "checkpoint {index} differs");
    }
    drop(native);
    let archive = dir.path().join("original.komarchive");
    gallery(&mut source, "komaphone.gallery.export", json!({"format":"archive","path":archive,"chain":0,"fps":6,"looping":true})).unwrap();
    let portable = photocraft_local_store::gallery::read_archive(&archive).unwrap();
    gallery_media::validate_originals(&portable).unwrap();
    assert_eq!(gallery_media::replay_info(&portable, 0, 0).unwrap(), replay);
    assert_eq!(gallery_media::replay_step(&portable, 0, 0, replay.moments.len() - 1).unwrap(), canonical);

    let fresh_root = dir.path().join("fresh");
    let mut fresh = session(&fresh_root);
    gallery(&mut fresh, "komaphone.gallery.import", json!({"path":archive})).unwrap();
    // No list, playback, export or other command may be needed to make a freshly
    // imported original discoverable in the gallery or update its quota display.
    assert_import_list_matches_durable_state(&fresh, &fresh_root);
    let saved_id = fresh.gallery.active.as_ref().unwrap().id;
    gallery(&mut fresh, "komaphone.gallery.playback", json!({"fps":12,"looping":false})).unwrap();
    gallery(&mut fresh, "komaphone.gallery.open", json!({"entry":saved_id})).unwrap();
    assert_eq!(fresh.gallery.active.as_ref().unwrap().fps, 12);
    assert!(!fresh.gallery.active.as_ref().unwrap().looping);
    gallery(&mut fresh, "komaphone.gallery.remix", json!({"chain":0})).unwrap();
    fresh.flush_local_projects(Duration::from_secs(20)).unwrap();
    let remix = fresh.local_projects.status(fresh.active().unwrap().doc.id).project.unwrap();
    assert_ne!(remix, original);
    assert_ne!(fresh.active().unwrap().doc.id, before.id);
    assert_eq!(gallery_media::render_document(&fresh.active().unwrap().doc).unwrap(), canonical);
    let provenance = fresh.active().unwrap().doc.metadata.text.iter().find(|(key, _)| key == "Komaphone remix source").unwrap();
    let provenance: photocraft_party::gallery::RemixProvenance = serde_json::from_str(&provenance.1).unwrap();
    assert_eq!(provenance.source_entry, saved_id);
    fresh.execute("paint.stroke", json!({"points":[[10,35],[40,35]],"size":7,"color":"#ff0000"})).unwrap();
    fresh.flush_local_projects(Duration::from_secs(20)).unwrap();
    assert_ne!(gallery_media::render_document(&fresh.active().unwrap().doc).unwrap(), canonical);
    gallery(&mut fresh, "komaphone.gallery.open", json!({"entry":saved_id})).unwrap();
    assert_eq!(fresh.gallery.image.as_deref(), Some(&canonical), "remixing mutated the source");
    let original_local = Store::open(source_root.join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    assert_eq!(*before, original_local.load(original).unwrap().document);

    let backup = dir.path().join("portable-backup");
    gallery(&mut fresh, "komaphone.gallery.backup", json!({"path":backup})).unwrap();
    let restored_root = dir.path().join("restored");
    gallery(&mut fresh, "komaphone.gallery.restore", json!({"path":backup,"destination":restored_root})).unwrap();
    let mut restored = session(&restored_root);
    gallery(&mut restored, "komaphone.gallery.open", json!({"entry":saved_id})).unwrap();
    assert_eq!(restored.gallery.image.as_deref(), Some(&canonical));
    assert_eq!(restored.gallery.active.as_ref().unwrap().fps, 12);
    let from = Store::open(fresh_root.join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    let to = Store::open(restored_root.join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    assert_eq!(from.replay_page(remix, 0, 128).unwrap(), to.replay_page(remix, 0, 128).unwrap());
    assert_eq!(
        gallery_media::render_document(&from.load(remix).unwrap().document).unwrap(),
        gallery_media::render_document(&to.load(remix).unwrap().document).unwrap()
    );
}

#[test]
fn gallery_preferences_reopen_and_failed_export_preserve_the_exact_original() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    let mut app = session(&root);
    let id = drawing(&mut app);
    gallery(&mut app, "komaphone.gallery.update", json!({"entry":id,"draft":true,"title":"Renamed draft","favorite":true})).unwrap();
    assert!(app.gallery.items.iter().any(|item| item.id == id && item.favorite && item.title == "Renamed draft"));
    gallery(&mut app, "komaphone.gallery.open", json!({"entry":id,"draft":true})).unwrap();
    assert_eq!(app.gallery.active.as_ref().unwrap().title, "Renamed draft");
    let before = app.gallery.image.as_ref().unwrap().clone();
    let occupied = dir.path().join("user-original.png");
    std::fs::write(&occupied, b"User data must survive export failure").unwrap();
    assert!(gallery(&mut app, "komaphone.gallery.export", json!({"format":"png","path":occupied,"chain":0,"frame":0,"fps":6,"looping":true})).is_err());
    assert_eq!(std::fs::read(&occupied).unwrap(), b"User data must survive export failure");
    assert_eq!(app.gallery.image.as_deref(), Some(before.as_ref()));
    gallery(&mut app, "komaphone.gallery.update", json!({"entry":id,"draft":true,"trashed":true})).unwrap();
    drop(app);
    let mut reopened = session(&root);
    gallery(&mut reopened, "komaphone.gallery.list", json!({})).unwrap();
    let info = reopened.gallery.items.iter().find(|item| item.id == id).unwrap();
    assert!(info.favorite && info.trashed);
    assert_eq!(info.title, "Renamed draft");
    gallery(&mut reopened, "komaphone.gallery.update", json!({"entry":id,"draft":true,"trashed":false})).unwrap();
    gallery(&mut reopened, "komaphone.gallery.open", json!({"entry":id,"draft":true})).unwrap();
    assert_eq!(reopened.gallery.image.as_deref(), Some(before.as_ref()));
}

#[test]
fn live_draft_and_imported_original_with_same_id_keep_separate_timing_and_thumbnails() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = session(dir.path());
    let id = drawing(&mut app);
    gallery(&mut app, "komaphone.gallery.open", json!({"entry":id,"draft":true})).unwrap();
    let archive = dir.path().join("same-id.komarchive");
    gallery(&mut app, "komaphone.gallery.export", json!({"format":"archive","path":archive,"chain":0,"fps":6,"looping":true})).unwrap();
    assert!(app.gallery.items.iter().any(|item| item.id == id && item.draft));
    gallery(&mut app, "komaphone.gallery.import", json!({"path":archive})).unwrap();
    assert_eq!(app.gallery.active.as_ref().unwrap().id, id);
    assert!(!app.gallery.active_draft);
    assert_import_list_matches_durable_state(&app, dir.path());
    assert_eq!(app.gallery.items.iter().filter(|item| item.id == id).count(), 2, "same-id draft and imported original must each remain listed");
    assert!(app.gallery.items.iter().any(|item| item.id == id && item.draft));
    gallery(&mut app, "komaphone.gallery.playback", json!({"fps":9,"looping":true})).unwrap();
    gallery(&mut app, "komaphone.gallery.open", json!({"entry":id,"draft":true})).unwrap();
    gallery(&mut app, "komaphone.gallery.playback", json!({"fps":17,"looping":false})).unwrap();
    gallery(&mut app, "komaphone.gallery.open", json!({"entry":id,"draft":false})).unwrap();
    assert_eq!(app.gallery.active.as_ref().unwrap().fps, 9);
    assert!(app.gallery.active.as_ref().unwrap().looping);
    gallery(&mut app, "komaphone.gallery.open", json!({"entry":id,"draft":true})).unwrap();
    assert_eq!(app.gallery.active.as_ref().unwrap().fps, 17);
    assert!(!app.gallery.active.as_ref().unwrap().looping);
    app.execute("paint.stroke", json!({"points":[[8,35],[50,35]],"size":8,"color":"#ff0000"})).unwrap();
    app.flush_local_projects(Duration::from_secs(20)).unwrap();
    gallery(&mut app, "komaphone.gallery.thumbnail", json!({"entry":id,"draft":true})).unwrap();
    gallery(&mut app, "komaphone.gallery.thumbnail", json!({"entry":id,"draft":false})).unwrap();
    let draft = app.gallery.thumbnails.get(&(id, true)).unwrap();
    let original = app.gallery.thumbnails.get(&(id, false)).unwrap();
    assert_ne!(draft.rgba, original.rgba, "thumbnail caches must include editable/original identity");
}

#[test]
fn original_metadata_exports_are_identical_before_and_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = session(dir.path());
    let id = drawing(&mut app);
    gallery(&mut app, "komaphone.gallery.open", json!({"entry":id,"draft":true})).unwrap();
    let original_archive = dir.path().join("initial.komarchive");
    gallery(&mut app, "komaphone.gallery.export", json!({"format":"archive","path":original_archive,"chain":0,"fps":6,"looping":true})).unwrap();
    gallery(&mut app, "komaphone.gallery.import", json!({"path":original_archive})).unwrap();
    assert!(!app.gallery.active_draft);
    for (label, command, params) in [
        ("rename", "komaphone.gallery.update", json!({"entry":id,"title":"A renamed original"})),
        ("favorite", "komaphone.gallery.update", json!({"entry":id,"favorite":true})),
        ("playback", "komaphone.gallery.playback", json!({"fps":19,"looping":false})),
    ] {
        let before = app.gallery.active.as_ref().unwrap().clone();
        gallery(&mut app, command, params).unwrap();
        let updated = app.gallery.active.as_ref().unwrap().clone();
        assert_eq!(updated.title, "A renamed original");
        assert_ne!(updated.updated_at, before.updated_at);
        let immediate = dir.path().join(format!("{label}-immediate.komarchive"));
        gallery(&mut app, "komaphone.gallery.export", json!({"format":"archive","path":immediate,"chain":0,"fps":updated.fps,"looping":updated.looping}))
            .unwrap();
        let exported = photocraft_local_store::gallery::read_archive(&immediate).unwrap();
        assert_eq!(
            serde_json::to_value(&exported.manifest).unwrap(),
            serde_json::to_value(updated.as_ref()).unwrap(),
            "UI selection and exported metadata must describe the same durable version"
        );
        gallery(&mut app, "komaphone.gallery.open", json!({"entry":id})).unwrap();
        assert_eq!(serde_json::to_value(app.gallery.active.as_ref().unwrap().as_ref()).unwrap(), serde_json::to_value(updated.as_ref()).unwrap());
        let reopened = dir.path().join(format!("{label}-reopened.komarchive"));
        gallery(&mut app, "komaphone.gallery.export", json!({"format":"archive","path":reopened,"chain":0,"fps":updated.fps,"looping":updated.looping}))
            .unwrap();
        assert_eq!(std::fs::read(&immediate).unwrap(), std::fs::read(&reopened).unwrap(), "reopening after {label} must not silently change an archive");
    }
    assert_eq!(app.gallery.active.as_ref().unwrap().fps, 19);
    assert!(!app.gallery.active.as_ref().unwrap().looping);
    assert!(app.gallery.items.iter().any(|item| item.id == id && !item.draft && item.favorite));
}

fn participant(n: u16) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{n:032x}")).unwrap(),
        device: DeviceId::new(&format!("{:032x}", n + 100)).unwrap(),
        display_name: format!("Player {n}"),
    }
}

#[test]
fn revoking_a_previously_open_draft_blocks_stale_worker_replay_export_and_remix() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = session(dir.path());
    let old = drawing(&mut app);
    gallery(&mut app, "komaphone.gallery.open", json!({"entry":old,"draft":true})).unwrap();
    let host = participant(1);
    let guest = participant(2);
    let room_id = RoomId::new("11111111111111111111111111111111").unwrap();
    let game_id = GameId::new("22222222222222222222222222222222").unwrap();
    let mut room = Room::new(room_id, host.clone(), Mode::ExquisiteCorpse, None).unwrap();
    room.apply(guest.id, room.revision(), 0, Action::Join { participant: guest.clone() }).unwrap();
    for player in [&host, &guest] {
        room.apply(player.id, room.revision(), 0, Action::Ready { ready: true }).unwrap();
    }
    let projects: Vec<_> = (0..6u16).map(|index| ProjectId::new(&format!("{:032x}", 1000 + index)).unwrap()).collect();
    room.apply(host.id, room.revision(), 0, Action::Start { game: game_id, projects }).unwrap();
    let view = room.view(host.id).unwrap();
    let current = view.assignment.as_ref().unwrap().assignment.project;
    app.rooms.status.profile = Some(host);
    app.rooms.status.room = Some(view);
    app.rooms.status.game_drafts = Some(photocraft_room_client::GameDrafts { room: room_id, game: game_id, projects: vec![old, current] });
    app.poll_gallery();
    assert!(app.gallery.active.is_none());
    assert!(app.gallery.image.is_none());
    let denied_export = dir.path().join("forbidden.png");
    for (command, params) in [
        ("komaphone.gallery.render", json!({"chain":0,"frame":0,"step":0})),
        ("komaphone.gallery.export", json!({"format":"png","path":denied_export,"chain":0,"frame":0,"fps":6,"looping":true})),
        ("komaphone.gallery.remix", json!({"chain":0})),
        ("komaphone.gallery.thumbnail", json!({"entry":old,"draft":true})),
    ] {
        assert!(app.execute(command, params).is_err(), "stale authorized original bypassed phase revocation: {command}");
    }
    app.poll_gallery();
    assert!(app.gallery.image.is_none());
    assert!(!denied_export.exists());
    let status = app.execute("komaphone.gallery.status", json!({})).unwrap();
    assert!(status["active"].is_null());
    assert!(status["image"].is_null());
    assert!(status["items"].as_array().unwrap().iter().all(|item| item["id"] != old.as_str()));
    // The player's own current assignment may be viewed, but it is not a
    // finished/remixable source until the authoritative game reveals.
    app.rooms.status.room.as_mut().unwrap().assignment.as_mut().unwrap().assignment.project = old;
    gallery(&mut app, "komaphone.gallery.open", json!({"entry":old,"draft":true})).unwrap();
    assert!(app.execute("komaphone.gallery.remix", json!({"chain":0})).unwrap_err().to_string().contains("Finish the game"));
}

#[test]
fn imported_current_assignment_cannot_bypass_remix_or_previous_assignment_privacy() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = session(dir.path());
    let original = drawing(&mut app);
    gallery(&mut app, "komaphone.gallery.open", json!({"entry":original,"draft":true})).unwrap();
    let archive = dir.path().join("assigned-copy.komarchive");
    gallery(&mut app, "komaphone.gallery.export", json!({"format":"archive","path":archive,"chain":0,"fps":6,"looping":true})).unwrap();
    gallery(&mut app, "komaphone.gallery.import", json!({"path":archive})).unwrap();
    assert_eq!(app.gallery.active.as_ref().unwrap().id, original);
    assert!(!app.gallery.active_draft);

    // A later ordinary draft export conflicts with the first saved original and
    // receives a fresh gallery ID. Its retained replay still names the source.
    app.execute("paint.stroke", json!({"points":[[8,35],[50,35]],"size":8,"color":"#ff0000"})).unwrap();
    app.flush_local_projects(Duration::from_secs(20)).unwrap();
    gallery(&mut app, "komaphone.gallery.open", json!({"entry":original,"draft":true})).unwrap();
    let later_archive = dir.path().join("assigned-later-copy.komarchive");
    gallery(&mut app, "komaphone.gallery.export", json!({"format":"archive","path":later_archive,"chain":0,"fps":6,"looping":true})).unwrap();
    gallery(&mut app, "komaphone.gallery.import", json!({"path":later_archive})).unwrap();
    let imported_copy = app.gallery.active.as_ref().unwrap().id;
    assert_ne!(imported_copy, original);
    let photocraft_party::gallery::GalleryContent::Project { replay: Some(replay), .. } = &app.gallery.active.as_ref().unwrap().content else {
        panic!("normal archive import must retain its replay source");
    };
    assert_eq!(replay.project, original);

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
    projects[0] = original;
    room.apply(host.id, room.revision(), 0, Action::Start { game: game_id, projects: projects.clone() }).unwrap();
    let view = room.view(host.id).unwrap();
    assert_eq!(view.assignment.as_ref().unwrap().assignment.project, original);
    app.rooms.status.profile = Some(host);
    app.rooms.status.room = Some(view);
    app.rooms.status.game_drafts = Some(photocraft_room_client::GameDrafts { room: room_id, game: game_id, projects: projects.clone() });
    app.poll_gallery();
    let mut violations = Vec::new();
    for source in [original, imported_copy] {
        let _ = gallery(&mut app, "komaphone.gallery.open", json!({"entry":source,"draft":false}));
        if gallery(&mut app, "komaphone.gallery.remix", json!({"chain":0})).is_ok() {
            violations.push(format!("current source {source:?}: remix succeeded"));
        }
    }

    // Reopen the current source, then project the next owned assignment. Its old
    // identity must remain restricted regardless of whether the source is an
    // editable draft or an immutable imported copy created by normal commands.
    app.rooms.status.room.as_mut().unwrap().assignment.as_mut().unwrap().assignment.project = projects[1];
    app.poll_gallery();
    if app.gallery.active.is_some() || app.gallery.image.is_some() {
        violations.push("previous imported source remains selected/visible".into());
    }
    for source in [original, imported_copy] {
        let leaked_png = dir.path().join(format!("previous-assignment-{}.png", source.as_str()));
        for (command, params) in [
            ("komaphone.gallery.open", json!({"entry":source,"draft":false})),
            ("komaphone.gallery.render", json!({"chain":0,"frame":0,"step":0})),
            ("komaphone.gallery.export", json!({"format":"png","path":leaked_png,"chain":0,"frame":0,"fps":6,"looping":true})),
            ("komaphone.gallery.thumbnail", json!({"entry":source,"draft":false})),
        ] {
            if gallery(&mut app, command, params).is_ok() {
                violations.push(format!("previous source {source:?}: {command} succeeded"));
            }
        }
        if leaked_png.exists() {
            violations.push(format!("previous source {source:?}: PNG was published"));
        }
    }
    assert!(violations.is_empty(), "imported source bypass: {violations:#?}");
}

#[test]
fn truncated_backup_and_existing_restore_destination_never_replace_live_work() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("live");
    let mut app = session(&root);
    let id = drawing(&mut app);
    let before = app.active().unwrap().doc.clone();
    let backup = dir.path().join("backup");
    gallery(&mut app, "komaphone.gallery.backup", json!({"path":backup})).unwrap();
    assert!(gallery(&mut app, "komaphone.gallery.restore", json!({"path":backup,"destination":root})).is_err());
    let bad_file = backup.join(format!("{}.komarchive", id.as_str()));
    let bytes = std::fs::read(&bad_file).unwrap();
    std::fs::write(&bad_file, &bytes[..bytes.len() / 2]).unwrap();
    let failed = dir.path().join("failed-restoration");
    assert!(gallery(&mut app, "komaphone.gallery.restore", json!({"path":backup,"destination":failed})).is_err());
    assert!(!failed.exists(), "a failed restoration must not publish a partial data directory");
    let store = Store::open(root.join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    assert_eq!(store.load(id).unwrap().document, *before);
    assert!(root.join("drafts.sqlite3").exists());
}

#[test]
fn corrupted_latest_history_cannot_be_archived_as_a_complete_recovered_prefix() {
    use photocraft_doc::{Color, ColorMode, Document, SampleType, Size};
    use photocraft_local_store::{ReplayKind, ReplayRecord, SaveRequest, fresh_event_id, fresh_project_id};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("damaged");
    std::fs::create_dir(&root).unwrap();
    let path = root.join("drafts.sqlite3");
    let mut store = Store::open(&path, StoreOptions::default()).unwrap();
    let original = SaveRequest {
        project: fresh_project_id().unwrap(),
        expected_revision: 0,
        request_id: fresh_event_id().unwrap(),
        title: "Retained damaged history".into(),
        document: Document::with_background("Recovery", Size { width: 16, height: 12 }, ColorMode::Rgb, SampleType::U8, Color::WHITE),
        event: ReplayRecord { schema_version: 1, command: "file.new".into(), params: json!({}), elapsed_ms: 0, kind: ReplayKind::Baseline },
    };
    store.save(&original).unwrap();
    let marker = "fixture.corrupt_latest_record.unique_key";
    let mut latest = original.clone();
    latest.expected_revision = 1;
    latest.request_id = fresh_event_id().unwrap();
    latest.event.command = marker.into();
    latest.event.kind = ReplayKind::Resolved;
    latest.event.elapsed_ms = 100;
    store.save(&latest).unwrap();
    drop(store);

    // Change only the known JSON payload bytes, preserving the SQLite page and
    // row framing. The production checksum path must detect the damaged tail.
    let mut bytes = std::fs::read(&path).unwrap();
    let offsets: Vec<_> = bytes.windows(marker.len()).enumerate().filter_map(|(index, chunk)| (chunk == marker.as_bytes()).then_some(index)).collect();
    assert_eq!(offsets.len(), 1, "fixture marker must identify exactly one stored replay payload");
    bytes[offsets[0]] ^= 1;
    std::fs::write(&path, &bytes).unwrap();
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    let recovered = store.load(original.project).unwrap();
    assert_eq!((recovered.info.revision, recovered.head_revision), (1, 2));
    assert!(recovered.recovery_warning.is_some());
    drop(store);

    let mut app = session(&root);
    let error = gallery(&mut app, "komaphone.gallery.open", json!({"entry":original.project,"draft":true})).unwrap_err();
    assert!(error.contains("recovered earlier revision"), "recovery must be actionable: {error}");
    assert!(app.gallery.active.is_none());
    let backup = dir.path().join("must-not-publish-partial-backup");
    let error = gallery(&mut app, "komaphone.gallery.backup", json!({"path":backup})).unwrap_err();
    assert!(error.contains("recovered earlier revision"));
    assert!(!backup.exists(), "incomplete history must not be represented as a complete backup");
    drop(app);
    assert_eq!(std::fs::read(&path).unwrap(), bytes, "failed gallery operations must retain the damaged history for recovery");
}
