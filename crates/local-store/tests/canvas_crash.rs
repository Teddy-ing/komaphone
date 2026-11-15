//! Real process termination around canvas commit and switch boundaries.
#![cfg(not(target_arch = "wasm32"))]

use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use photocraft_local_store::StoreOptions;
use photocraft_local_store::canvas::{CanvasStage, CanvasStore};
use photocraft_scene::{CanvasId, Srgba, Stroke, StrokeId, Viewport};

fn signal_and_wait(marker: &str) {
    println!("{marker}");
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
}

#[test]
fn canvas_crash_child() {
    let Some(root) = std::env::var_os("KOMAPHONE_CANVAS_CRASH_ROOT") else { return };
    let action = std::env::var("KOMAPHONE_CANVAS_CRASH_ACTION").unwrap();
    let phase = std::env::var("KOMAPHONE_CANVAS_CRASH_PHASE").unwrap();
    let path = std::path::Path::new(&root).join("canvases.sqlite3");
    let mut store = CanvasStore::open(&path, StoreOptions::default()).unwrap();
    let info = store.active().unwrap().unwrap();
    let original_database_bytes = std::fs::metadata(&path).unwrap().len();
    let mut observe = |stage| {
        if stage == CanvasStage::BeforeCommit && phase == "before" {
            assert!(std::fs::metadata(path.with_extension("sqlite3-journal")).unwrap().len() > 512);
            if action == "append" {
                let uncommitted_database_bytes = std::fs::metadata(&path).unwrap().len();
                assert!(
                    uncommitted_database_bytes > original_database_bytes,
                    "uncommitted pages actually spill before kill: {original_database_bytes} -> {uncommitted_database_bytes}"
                );
                println!("CANVAS_SPILL_BYTES {original_database_bytes} {uncommitted_database_bytes}");
            }
            signal_and_wait("CANVAS_BEFORE_COMMIT");
        }
    };
    if action == "append" {
        let stroke = Stroke {
            id: StrokeId::new("11111111111111111111111111111111").unwrap(),
            canvas: info.id,
            owner: info.owner,
            revision: 0,
            anchor: info.camera.region,
            points: (0..8192).map(|n| [480.0 + f64::from(n) / 100.12345, 500.0 + (f64::from(n) / 7.54321).sin()]).collect(),
            width: 5.0,
            color: Srgba { rgba: [0.2, 0.4, 0.6, 1.0] },
            erase: false,
        };
        let ack = store.append_observed(&stroke, info.revision, &mut observe).unwrap();
        assert_eq!(ack.stroke_count, 1);
    } else if action == "create" {
        let mut camera = info.camera.clone();
        camera.zoom = 1.25;
        assert_eq!(store.create_observed("New after switch", Some(camera), &mut observe).unwrap().name, "New after switch");
    } else {
        let destination = CanvasId::new(&std::env::var("KOMAPHONE_CANVAS_CRASH_DEST").unwrap()).unwrap();
        let mut camera = info.camera.clone();
        camera.zoom = 1.25;
        assert_eq!(store.switch_observed(destination, Some(camera), &mut observe).unwrap().id, destination);
    }
    signal_and_wait("CANVAS_DURABLE_ACK");
}

fn kill_at(action: &str, phase: &str) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("canvases.sqlite3");
    let mut store = CanvasStore::open(&path, StoreOptions::default()).unwrap();
    let a = store.create("Same name").unwrap();
    let b = store.create("Same name").unwrap();
    store.switch(a.id, None).unwrap();
    drop(store);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "canvas_crash_child", "--nocapture"])
        .env("KOMAPHONE_CANVAS_CRASH_ROOT", dir.path())
        .env("KOMAPHONE_CANVAS_CRASH_ACTION", action)
        .env("KOMAPHONE_CANVAS_CRASH_PHASE", phase)
        .env("KOMAPHONE_CANVAS_CRASH_DEST", b.id.as_str())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let output = child.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in std::io::BufReader::new(output).lines() {
            if sender.send(line.unwrap_or_default()).is_err() {
                break;
            }
        }
    });
    let marker = if phase == "before" { "CANVAS_BEFORE_COMMIT" } else { "CANVAS_DURABLE_ACK" };
    let start = std::time::Instant::now();
    let mut observed = false;
    while start.elapsed() < Duration::from_secs(45) {
        match receiver.recv_timeout(Duration::from_secs(1)) {
            Ok(line) if line.contains(marker) => {
                observed = true;
                break;
            }
            Ok(line) if line.starts_with("CANVAS_SPILL_BYTES") => println!("{line}"),
            Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let killed = child.kill();
    let status = child.wait().unwrap();
    reader.join().unwrap();
    assert!(observed, "child missed {action}/{marker}: {status}");
    killed.unwrap();
    assert!(!status.success());
    let reopened = CanvasStore::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(reopened.owner(), a.owner);
    assert_eq!(reopened.list().unwrap().len(), if action == "create" && phase == "after" { 3 } else { 2 });
    assert_eq!(reopened.info(b.id).unwrap(), b);
    let restored = reopened.info(a.id).unwrap();
    if phase == "before" {
        assert_eq!(restored, a);
        assert_eq!(reopened.active().unwrap().unwrap().id, a.id);
    } else if action == "append" {
        assert_eq!(restored.stroke_count, 1);
        assert_eq!(restored.revision, 1);
        assert_eq!(reopened.active().unwrap().unwrap().id, a.id);
        let view = reopened.view(&Viewport { camera: restored.camera, width: 1024, height: 768 }).unwrap();
        assert_eq!(view.strokes.len(), 1);
        assert_eq!(view.strokes[0].points.len(), 8192);
    } else if action == "create" {
        assert_eq!(restored.camera.zoom, 1.25);
        assert_eq!(restored.revision, 1);
        let active = reopened.active().unwrap().unwrap();
        assert_eq!(active.name, "New after switch");
        assert_ne!(active.id, a.id);
        assert_ne!(active.id, b.id);
    } else {
        assert_eq!(restored.camera.zoom, 1.25);
        assert_eq!(restored.revision, 1);
        assert_eq!(reopened.active().unwrap().unwrap().id, b.id);
    }
}

#[test]
fn kill_after_stroke_ack_keeps_every_committed_sample() {
    kill_at("append", "after")
}
#[test]
fn kill_before_stroke_commit_keeps_previous_readable_canvas() {
    kill_at("append", "before")
}
#[test]
fn kill_after_switch_ack_keeps_active_identity_and_outgoing_camera() {
    kill_at("switch", "after")
}
#[test]
fn kill_before_switch_commit_keeps_both_canvases_and_previous_active_identity() {
    kill_at("switch", "before")
}

#[test]
fn kill_after_create_ack_keeps_new_identity_and_outgoing_camera() {
    kill_at("create", "after")
}
#[test]
fn kill_before_create_commit_keeps_original_list_and_camera() {
    kill_at("create", "before")
}
