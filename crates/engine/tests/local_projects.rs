#![cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::time::{Duration, Instant};

use photocraft_engine::Session;
use photocraft_local_store::{ReplayKind, Store, StoreOptions};
use serde_json::json;

fn open(path: &Path) -> Session {
    let mut session = Session::new();
    session.enable_local_projects(path.to_path_buf(), StoreOptions::default()).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    session
}

fn fixture(session: &mut Session) {
    session.execute("komaphone.project.new", json!({"width":64,"height":64,"name":"Recovery fixture"})).unwrap();
    session.execute("layer.new.layer", json!({"name":"Ink"})).unwrap();
    session.execute("paint.stroke", json!({"points":[[10,20],[45,20]],"size":8,"hardness":1,"color":"#2060c0"})).unwrap();
    session.execute("layer.setProps", json!({"opacity":0.4})).unwrap();
    session.execute("edit.undo", json!({})).unwrap();
}

#[test]
fn acknowledged_edits_reopen_with_layers_pixels_and_retained_history() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("drafts.sqlite3");
    let mut first = open(&path);
    fixture(&mut first);
    let original = first.active().unwrap().doc.clone();
    assert_eq!(first.local_projects.status(original.id).local_revision, 0, "enqueue is not acknowledgment");
    first.flush_local_projects(Duration::from_secs(20)).unwrap();
    let status = first.local_projects.status(original.id);
    assert_eq!(status.local_revision, 5);
    assert_eq!(status.pending, 0);
    let project = status.project.unwrap();
    drop(first);

    let mut second = open(&path);
    assert_eq!(second.local_projects.drafts.len(), 1);
    second.execute("komaphone.project.open", json!({"project":project.as_str()})).unwrap();
    second.flush_local_projects(Duration::from_secs(20)).unwrap();
    assert_eq!(*original, *second.active().unwrap().doc);
    assert_eq!(second.local_replay.pending_len(), 0, "reopen invents no baseline");
    assert_eq!(second.local_projects.status(second.active().unwrap().doc.id).local_revision, 5);
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    let replay = store.replay_page(project, 0, 128).unwrap();
    assert_eq!(replay.len(), 5);
    // Every saved event has its own native checkpoint, including the final undo pixels.
    assert!(format!("{replay:?}").contains("Undo"));
    drop(store);
    second.execute("layer.renameLayer", json!({"name":"Reopened ink"})).unwrap();
    second.flush_local_projects(Duration::from_secs(20)).unwrap();
    assert_eq!(second.local_projects.status(second.active().unwrap().doc.id).local_revision, 6);
}

#[test]
fn failed_worker_write_retains_exact_retry_and_never_acknowledges() {
    let temp = tempfile::tempdir().unwrap();
    let unavailable = temp.path().join("occupied");
    std::fs::write(&unavailable, b"not a directory").unwrap();
    let mut session = Session::new();
    session.enable_local_projects(unavailable.join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    fixture(&mut session);
    assert!(session.flush_local_projects(Duration::from_secs(20)).is_err());
    let doc = session.active().unwrap().doc.clone();
    assert_eq!(session.local_projects.status(doc.id).local_revision, 0);
    assert!(session.local_projects.status_text(doc.id).contains("failed"));
    std::fs::remove_file(&unavailable).unwrap();
    session.execute("komaphone.project.retry", json!({})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    assert_eq!(session.local_projects.status(doc.id).local_revision, 5);
    assert_eq!(*doc, *session.active().unwrap().doc);
}

#[test]
fn subprocess_writer() {
    let Some(root) = std::env::var_os("KOMAPHONE_TEST_WRITER_ROOT") else { return };
    let root = std::path::PathBuf::from(root);
    let mut session = open(&root.join("drafts.sqlite3"));
    fixture(&mut session);
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let state = session.active().unwrap();
    let project = session.local_projects.status(state.doc.id).project.unwrap();
    // Only signal after the same acknowledgment used by the real workspace.
    std::fs::write(root.join("ack"), project.as_str()).unwrap();
    loop {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn killing_app_worker_immediately_after_ack_preserves_final_drawing() {
    let temp = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "subprocess_writer", "--nocapture"])
        .env("KOMAPHONE_TEST_WRITER_ROOT", temp.path())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(40);
    while !temp.path().join("ack").exists() && Instant::now() < deadline {
        assert!(child.try_wait().unwrap().is_none(), "writer exited before acknowledgment");
        std::thread::sleep(Duration::from_millis(2));
    }
    let acknowledged = temp.path().join("ack").exists();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(acknowledged, "writer must acknowledge before kill");
    let project = photocraft_party::contracts::ProjectId::new(&std::fs::read_to_string(temp.path().join("ack")).unwrap()).unwrap();
    let store = Store::open(temp.path().join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    let loaded = store.load(project).unwrap();
    assert_eq!(loaded.info.revision, 5);
    assert_eq!(loaded.document.layers.len(), 2);
    let flat = photocraft_compose::flatten(&loaded.document);
    let expected = flat.px.get(20 * 64 + 20).unwrap();
    assert!(expected[2] > expected[0], "blue stroke survives undo and process kill");
    let events = store.replay_page(project, 0, 128).unwrap();
    assert!(format!("{events:?}").contains(&format!("{:?}", ReplayKind::Undo)));
}

#[test]
fn ten_layer_ordinary_strokes_are_durable_within_one_second() {
    let temp = tempfile::tempdir().unwrap();
    let mut session = open(&temp.path().join("drafts.sqlite3"));
    session.execute("komaphone.project.new", json!({})).unwrap();
    for n in 1..10 {
        session.execute("layer.new.layer", json!({"name":format!("Paint {n}")})).unwrap();
    }
    session.flush_local_projects(Duration::from_secs(30)).unwrap();
    let mut timings = Vec::new();
    for n in 0..20 {
        let started = Instant::now();
        session.execute("paint.stroke", json!({"points":[[100,100+n*25],[400,180+n*25],[800,120+n*25]],"size":24,"hardness":0.8,"color":"#284b80"})).unwrap();
        session.flush_local_projects(Duration::from_secs(10)).unwrap();
        timings.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    timings.sort_by(f64::total_cmp);
    let p50 = timings[10];
    let p95 = timings[18];
    println!("1536x1024, 10 layers, 20 ordinary strokes: edit+durable ack p50={p50:.2}ms p95={p95:.2}ms max={:.2}ms", timings[19]);
    assert!(timings.iter().all(|ms| *ms <= 1000.0), "ordinary stroke exceeded durable acknowledgment target: {timings:?}");
}

#[test]
fn repeated_refresh_and_open_cannot_create_competing_editors() {
    let temp = tempfile::tempdir().unwrap();
    let mut session = open(&temp.path().join("drafts.sqlite3"));
    fixture(&mut session);
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let project = session.local_projects.status(session.active().unwrap().doc.id).project.unwrap();
    session.close(0);
    for _ in 0..20 {
        session.list_local_projects().unwrap();
    }
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    session.open_local_project(project).unwrap();
    for _ in 0..20 {
        session.list_local_projects().unwrap();
    }
    assert!(session.open_local_project(project).is_err());
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    session.open_local_project(project).unwrap();
    assert_eq!(session.documents().len(), 1);
}

#[test]
fn reopening_legacy_copy_creates_independent_project_and_utf8_title() {
    let temp = tempfile::tempdir().unwrap();
    let mut session = open(&temp.path().join("drafts.sqlite3"));
    fixture(&mut session);
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let original = session.active().unwrap().doc.clone();
    let project = session.local_projects.status(original.id).project.unwrap();
    session.close(0);
    let mut imported = original.as_ref().clone();
    imported.name = "絵".repeat(80);
    session.add_document(imported, None);
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let status = session.local_projects.status(session.active().unwrap().doc.id);
    assert_ne!(status.project, Some(project));
    assert_eq!(status.local_revision, 1);
    session.list_local_projects().unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    assert_eq!(session.local_projects.drafts.len(), 2);
    assert!(session.local_projects.drafts.iter().all(|draft| draft.title.len() <= 96));
}

#[test]
fn nested_status_cannot_persist_a_rolled_back_compound_edit() {
    let temp = tempfile::tempdir().unwrap();
    let mut session = open(&temp.path().join("drafts.sqlite3"));
    session.execute("komaphone.project.new", json!({"width":32,"height":32})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let original = session.active().unwrap().doc.clone();
    let context = session.begin_local_replay("compound.fixture", &json!({})).unwrap();
    session
        .edit("tentative", |doc, _| {
            // File identity is intentionally preserved through undo/rollback; use
            // an actual undoable drawing edit to exercise complete rollback.
            doc.layers[0].opacity = 0.25;
            Ok(())
        })
        .unwrap();
    session.execute("komaphone.project.status", json!({})).unwrap();
    assert!(session.rollback());
    session.end_local_replay(context, false).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let status = session.local_projects.status(original.id);
    assert_eq!(status.local_revision, 1);
    let loaded = Store::open(temp.path().join("drafts.sqlite3"), StoreOptions::default()).unwrap().load(status.project.unwrap()).unwrap();
    assert_eq!(*original, loaded.document);
}
