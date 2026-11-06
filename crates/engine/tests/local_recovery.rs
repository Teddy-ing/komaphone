//! A corrupt durable head opens as an independently saved recovery draft.
#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;
use std::time::Duration;

use photocraft_doc::{Color, ColorMode, Document, Layer, SampleType, Size};
use photocraft_engine::Session;
use photocraft_local_store::{ReplayKind, ReplayRecord, SaveRequest, Store, StoreOptions, fresh_event_id, fresh_project_id};
use serde_json::json;

fn open(path: &Path) -> Session {
    let mut session = Session::new();
    session.enable_local_projects(path.to_path_buf(), StoreOptions::default()).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    session
}

#[test]
fn corrupt_head_opens_as_durable_independent_copy_without_rewriting_source() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("drafts.sqlite3");
    let source = fresh_project_id().unwrap();
    let mut document = Document::with_background("Recovery source", Size::new(32, 24), ColorMode::Rgb, SampleType::U16, Color::rgb(0.2, 0.4, 0.8));
    document.layers.push(Layer::raster("Readable ink layer", document.pixel_format()));
    let original = document.clone();
    let mut store = Store::open(&path, StoreOptions::default()).unwrap();
    store
        .save(&SaveRequest {
            project: source,
            expected_revision: 0,
            request_id: fresh_event_id().unwrap(),
            title: "Recovery source".into(),
            document,
            event: ReplayRecord { schema_version: 1, command: "legacy.import".into(), params: json!({}), elapsed_ms: 0, kind: ReplayKind::Baseline },
        })
        .unwrap();
    let unique_command = "recovery.fixture.uniqueDamagedTailCommand";
    let mut tail = original.clone();
    tail.layers[1].name = "Unrecoverable later layer name".into();
    store
        .save(&SaveRequest {
            project: source,
            expected_revision: 1,
            request_id: fresh_event_id().unwrap(),
            title: "Recovery source".into(),
            document: tail,
            event: ReplayRecord {
                schema_version: 1,
                command: unique_command.into(),
                params: json!({"onlyThisRevision":true}),
                elapsed_ms: 100,
                kind: ReplayKind::Checkpoint,
            },
        })
        .unwrap();
    drop(store);

    // Damage one payload byte in the CLOSED, isolated real database. A same-length
    // replacement leaves SQLite structure intact while invalidating the object hash.
    // The unique marker must exist exactly once, so the earlier checkpoint is untouched.
    let mut bytes = std::fs::read(&path).unwrap();
    let positions: Vec<_> =
        bytes.windows(unique_command.len()).enumerate().filter_map(|(offset, value)| (value == unique_command.as_bytes()).then_some(offset)).collect();
    assert_eq!(positions.len(), 1, "the fixture must identify only the tail replay payload");
    bytes[positions[0]] = b'x';
    std::fs::write(&path, bytes).unwrap();

    let mut session = open(&path);
    session.execute("komaphone.project.open", json!({"project":source.as_str()})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let recovered = session.active().unwrap().doc.clone();
    assert_eq!(*recovered, original);
    assert!(session.local_projects.notice.as_deref().unwrap().contains("Recovered revision 1"));
    assert!(session.local_projects.error.is_none(), "recovery notice must not block the new baseline save");
    let status = session.local_projects.status(recovered.id);
    let copy = status.project.unwrap();
    assert_ne!(copy, source, "the recovery must own an independent permanent project ID");
    assert_eq!((status.local_revision, status.pending), (1, 0));
    assert!(status.error.is_none());
    assert!(session.local_document_saved(recovered.id));
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    let events = store.replay_page(copy, 0, 16).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, ReplayKind::Baseline);
    assert_eq!(store.load(copy).unwrap().document, original);
    drop(store);

    session.execute("layer.renameLayer", json!({"name":"Edited independent recovery"})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let edited_copy = session.active().unwrap().doc.clone();
    assert_eq!(session.local_projects.status(edited_copy.id).local_revision, 2);
    assert!(session.local_document_saved(edited_copy.id));
    drop(session);

    let store = Store::open(&path, StoreOptions::default()).unwrap();
    let retained_source = store.load(source).unwrap();
    assert_eq!((retained_source.info.revision, retained_source.head_revision), (1, 2));
    assert_eq!(retained_source.document, original);
    assert!(retained_source.recovery_warning.is_some());
    assert!(store.load_revision(source, 2).is_err(), "the damaged source tail must remain preserved, not replaced");
    let drafts = store.list().unwrap();
    assert_eq!(drafts.len(), 2);
    assert_eq!(drafts.iter().find(|draft| draft.project == source).unwrap().revision, 2);
    assert_eq!(drafts.iter().find(|draft| draft.project == copy).unwrap().revision, 2);
    assert_eq!(store.replay_page(copy, 0, 16).unwrap().len(), 2);
    drop(store);

    let mut reopened = open(&path);
    reopened.execute("komaphone.project.open", json!({"project":copy.as_str()})).unwrap();
    reopened.flush_local_projects(Duration::from_secs(20)).unwrap();
    assert_eq!(*reopened.active().unwrap().doc, *edited_copy);
    assert!(reopened.local_projects.notice.is_none());
    assert_eq!(reopened.local_replay.pending_len(), 0, "reopening the healthy recovery must not invent another baseline");
    assert!(reopened.local_document_saved(reopened.active().unwrap().doc.id));
}
