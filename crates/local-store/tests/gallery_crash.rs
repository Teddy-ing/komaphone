//! Terminate a real writer at the durable boundary, retaining its previous result.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use photocraft_doc::{Color, ColorMode, Document, SampleType, Size};
use photocraft_format::{PcraftWriter, SaveOptions};
use photocraft_local_store::StoreOptions;
use photocraft_local_store::gallery::{GalleryStage, GalleryStore};
use photocraft_party::gallery::{GALLERY_SCHEMA, GalleryBundle, GalleryContent, GalleryManifest};
use photocraft_party::{AssetHash, AssetRef, ProjectId};
use sha2::{Digest, Sha256};

fn fixture(id: u32, large: bool) -> GalleryBundle {
    let mut doc = Document::with_background("Crash fixture", Size { width: 16, height: 16 }, ColorMode::Rgb, SampleType::U8, Color::WHITE);
    if large {
        // Valid editable metadata, incompressible enough to spill SQLite pages.
        let mut bytes = vec![0u8; 6 * 1024 * 1024];
        getrandom::fill(&mut bytes).unwrap();
        let text = bytes.iter().map(|byte| char::from(b'!' + byte % 90)).collect::<String>();
        doc.metadata.xmp = Some(text.into());
    }
    let (bytes, _) = PcraftWriter::new().save_zip(&doc, &SaveOptions::default()).unwrap();
    let hash = AssetHash::new(&format!("{:x}", Sha256::digest(&bytes))).unwrap();
    GalleryBundle {
        manifest: GalleryManifest {
            schema_version: GALLERY_SCHEMA,
            id: ProjectId::new(&format!("{id:032x}")).unwrap(),
            title: format!("Crash fixture {id}"),
            created_at: "2026-11-06T00:00:00.000Z".into(),
            updated_at: "2026-11-06T00:00:00.000Z".into(),
            authors: Vec::new(),
            content: GalleryContent::Project { checkpoint: AssetRef { hash, bytes: bytes.len() as u64 }, replay: None, direct_assets: Vec::new() },
            provenance: None,
            notes: Vec::new(),
            fps: 6,
            looping: true,
        },
        objects: BTreeMap::from([(hash, bytes)]),
    }
}

fn signal_and_wait(message: &str) {
    println!("{message}");
    std::io::stdout().flush().unwrap();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
}

#[test]
fn gallery_crash_child() {
    let Some(root) = std::env::var_os("KOMAPHONE_GALLERY_CRASH_ROOT") else { return };
    let phase = std::env::var("KOMAPHONE_GALLERY_CRASH_PHASE").unwrap();
    let path = std::path::Path::new(&root).join("gallery.sqlite3");
    let mut store = GalleryStore::open(&path, StoreOptions::default()).unwrap();
    let second = fixture(2, phase == "during");
    if phase == "during" {
        store
            .insert_observed(&second, |stage| {
                if stage == GalleryStage::BeforeCommit {
                    let journal = std::path::Path::new(&root).join("gallery.sqlite3-journal");
                    assert!(std::fs::metadata(journal).unwrap().len() > 512);
                    assert!(std::fs::metadata(&path).unwrap().len() > 2 * 1024 * 1024, "uncommitted pages must actually spill");
                    signal_and_wait("GALLERY_DURING_TRANSACTION");
                }
            })
            .unwrap();
    } else {
        let saved = store.insert(&second).unwrap();
        assert_eq!(saved.id, second.manifest.id);
        signal_and_wait("GALLERY_DURABLE_ACK");
    }
}

fn kill_at(phase: &str, marker: &str) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gallery.sqlite3");
    let first = fixture(1, false);
    let mut store = GalleryStore::open(&path, StoreOptions::default()).unwrap();
    store.insert(&first).unwrap();
    drop(store);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "gallery_crash_child", "--nocapture"])
        .env("KOMAPHONE_GALLERY_CRASH_ROOT", dir.path())
        .env("KOMAPHONE_GALLERY_CRASH_PHASE", phase)
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
    let start = std::time::Instant::now();
    let mut observed = false;
    while start.elapsed() < Duration::from_secs(45) {
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
    assert!(observed, "child failed to reach {marker}: {status}");
    killed.unwrap();
    assert!(!status.success());
    let mut reopened = GalleryStore::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(reopened.load(first.manifest.id).unwrap().objects, first.objects);
    let second_id = ProjectId::new(&format!("{:032x}", 2)).unwrap();
    if phase == "during" {
        assert!(reopened.load(second_id).is_err());
        assert_eq!(reopened.list(true).unwrap().len(), 1);
        assert_eq!(reopened.insert(&fixture(2, false)).unwrap().id, second_id);
    } else {
        assert_eq!(reopened.list(true).unwrap().len(), 2);
        assert_eq!(reopened.load(second_id).unwrap().manifest.title, "Crash fixture 2");
    }
}

#[test]
fn kill_immediately_after_gallery_ack_preserves_original_and_archive_dependencies() {
    kill_at("ack", "GALLERY_DURABLE_ACK");
}

#[test]
fn kill_during_gallery_transaction_rolls_back_unacknowledged_entry() {
    kill_at("during", "GALLERY_DURING_TRANSACTION");
}
