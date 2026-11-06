//! Real subprocess termination at SQLite's transaction boundary. No mock disk adapter.
#![cfg(not(target_arch = "wasm32"))]

use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use photocraft_doc::{Color, ColorMode, Document, Layer, SampleType, Size};
use photocraft_local_store::{ReplayKind, ReplayRecord, SaveRequest, SaveStage, Store, StoreOptions};
use photocraft_party::contracts::{EventId, ProjectId};
use serde_json::json;

fn fixture(revision: u64) -> SaveRequest {
    let mut document =
        Document::with_background("Recovery fixture", Size { width: 64, height: 48 }, ColorMode::Rgb, SampleType::U16, Color::rgb(0.2, 0.4, 0.6));
    document.layers.push(Layer::raster(format!("Layer revision {revision}"), document.pixel_format()));
    SaveRequest {
        project: ProjectId::new("11111111111111111111111111111111").unwrap(),
        expected_revision: revision - 1,
        request_id: EventId::new(&format!("{revision:032x}")).unwrap(),
        title: "Crash recovery fixture".into(),
        document,
        event: ReplayRecord {
            schema_version: 1,
            command: if revision == 1 { "file.new" } else { "edit.undo" }.into(),
            params: json!({"revision":revision}),
            elapsed_ms: revision * 100,
            kind: if revision == 1 { ReplayKind::Baseline } else { ReplayKind::Undo },
        },
    }
}

fn signal_and_wait(message: &str) {
    println!("{message}");
    std::io::stdout().flush().unwrap();
    // Parent terminates the process. Keeping stdin open avoids a graceful shutdown path.
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
}

#[test]
fn crash_child() {
    let Some(root) = std::env::var_os("KOMAPHONE_CRASH_CHILD_ROOT") else { return };
    let phase = std::env::var("KOMAPHONE_CRASH_CHILD_PHASE").unwrap();
    let mut store = Store::open(std::path::Path::new(&root).join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    let mut update = fixture(2);
    if phase == "during" {
        // Unique bytes exceed SQLite's page cache, forcing actual uncommitted disk
        // writes before the kill. Repeated bytes would deduplicate and weaken this.
        let mut asset = vec![0_u8; 1024 * 1024];
        getrandom::fill(&mut asset).unwrap();
        update.event.params = json!({"during_write_asset":asset});
        store
            .save_observed(&update, |stage| {
                if stage == SaveStage::BeforeCommit {
                    let journal = std::path::Path::new(&root).join("drafts.sqlite3-journal");
                    assert!(std::fs::metadata(journal).unwrap().len() > 512, "rollback journal must exist on disk");
                    assert!(
                        std::fs::metadata(std::path::Path::new(&root).join("drafts.sqlite3")).unwrap().len() > 2 * 1024 * 1024,
                        "transaction must spill uncommitted pages to disk"
                    );
                    signal_and_wait("KOMAPHONE_DURING_WRITE");
                }
            })
            .unwrap();
    } else {
        let ack = store.save(&update).unwrap();
        assert_eq!(ack.revision, 2);
        signal_and_wait("KOMAPHONE_SAVE_ACKNOWLEDGED");
    }
}

fn kill_at(phase: &str, marker: &str) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("drafts.sqlite3");
    let first = fixture(1);
    let mut store = Store::open(&path, StoreOptions::default()).unwrap();
    store.save(&first).unwrap();
    drop(store);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_child", "--nocapture"])
        .env("KOMAPHONE_CRASH_CHILD_ROOT", directory.path())
        .env("KOMAPHONE_CRASH_CHILD_PHASE", phase)
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
    let started = std::time::Instant::now();
    let mut observed = false;
    while started.elapsed() < Duration::from_secs(30) {
        match receiver.recv_timeout(Duration::from_secs(1)) {
            Ok(line) if line.contains(marker) => {
                observed = true;
                break;
            }
            Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let killed = child.kill();
    let status = child.wait().unwrap();
    reader.join().unwrap();
    assert!(observed, "child never reached {marker}; status {status}");
    killed.unwrap();
    assert!(!status.success(), "termination must not be a normal close");
    let mut reopened = Store::open(&path, StoreOptions::default()).unwrap();
    let loaded = reopened.load(first.project).unwrap();
    let expected = if phase == "during" { 1 } else { 2 };
    assert_eq!(loaded.info.revision, expected);
    assert_eq!(loaded.document.layers.len(), 2);
    assert_eq!(loaded.document.layers[1].name, format!("Layer revision {expected}"));
    assert!(loaded.recovery_warning.is_none());
    assert_eq!(reopened.replay_page(first.project, 0, 128).unwrap().len(), expected as usize);
    if phase == "during" {
        let ack = reopened.save(&fixture(2)).unwrap();
        assert_eq!(ack.device_sequence, 2, "rolled-back transaction must not consume sequence");
    }
}

#[test]
fn kill_immediately_after_save_ack_preserves_edit_layers_and_replay() {
    kill_at("ack", "KOMAPHONE_SAVE_ACKNOWLEDGED");
}

#[test]
fn kill_during_sqlite_write_preserves_previous_acknowledged_revision() {
    kill_at("during", "KOMAPHONE_DURING_WRITE");
}
