//! One invalid/conflicting local project must not stop independent projects saving.
#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use photocraft_doc::DocId;
use photocraft_engine::Session;
use photocraft_local_store::{ReplayKind, ReplayRecord, SaveRequest, Store, StoreOptions, fresh_event_id, fresh_project_id};
use serde_json::json;

fn open(path: &Path) -> Session {
    let mut session = Session::new();
    session.enable_local_projects(path.to_path_buf(), StoreOptions::default()).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    session
}

fn drawing(session: &mut Session, name: &str) -> DocId {
    session.execute("komaphone.project.new", json!({"width":32,"height":32,"name":name})).unwrap();
    session.active().unwrap().doc.id
}

fn independent_drawing_saves(session: &mut Session, path: &Path, failed: DocId) -> DocId {
    let healthy = drawing(session, "Independent healthy draft");
    session.execute("paint.stroke", json!({"points":[[6,12],[24,12]],"size":5,"color":"#2860b0"})).unwrap();
    // Flush drains runnable projects before reporting the retained failure.
    let error = session.flush_local_projects(Duration::from_secs(30)).unwrap_err();
    assert!(!error.to_string().contains("still pending"), "failure should be actionable, not a timeout: {error}");
    assert!(session.local_projects.error.is_none(), "one bad project must not set the global storage error");
    assert!(session.local_projects.status(failed).error.is_some());
    assert!(!session.local_document_saved(failed));
    let status = session.local_projects.status(healthy);
    assert_eq!(status.local_revision, 2);
    assert_eq!(status.pending, 0);
    assert!(status.error.is_none());
    assert!(session.local_document_saved(healthy));
    let store = Store::open(path, StoreOptions::default()).unwrap();
    assert_eq!(store.load(status.project.unwrap()).unwrap().document, *session.active().unwrap().doc);
    healthy
}

#[test]
fn conflicting_writer_retains_failed_request_and_following_events_without_blocking_other_projects() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("drafts.sqlite3");
    let mut session = open(&path);
    let first = drawing(&mut session, "Conflicting draft");
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let project = session.local_projects.status(first).project.unwrap();

    let mut competing = Store::open(&path, StoreOptions::default()).unwrap();
    let saved = competing.load(project).unwrap();
    let mut competing_document = saved.document;
    competing_document.layers[0].name = "Other writer's durable edit".into();
    competing
        .save(&SaveRequest {
            project,
            expected_revision: 1,
            request_id: fresh_event_id().unwrap(),
            title: "Conflicting draft".into(),
            document: competing_document.clone(),
            event: ReplayRecord {
                schema_version: 1,
                command: "layer.renameLayer".into(),
                params: json!({"name":"Other writer's durable edit"}),
                elapsed_ms: saved.info.replay_elapsed_ms.saturating_add(1000),
                kind: ReplayKind::Checkpoint,
            },
        })
        .unwrap();
    session.execute("layer.renameLayer", json!({"name":"My first retained edit"})).unwrap();
    session.execute("layer.renameLayer", json!({"name":"My following retained edit"})).unwrap();
    let retained_document = session.active().unwrap().doc.clone();
    assert!(session.flush_local_projects(Duration::from_secs(20)).is_err());
    let failed = session.local_projects.status(first);
    assert_eq!(failed.local_revision, 1);
    assert_eq!(failed.pending, 2, "failed request and following replay event must both remain retained");
    assert_eq!(session.local_replay.pending_for(first), 1);
    assert!(failed.error.as_deref().unwrap().contains("changed since"));
    assert!(session.execute("layer.renameLayer", json!({"name":"Blocked edit"})).is_err());
    assert!(Arc::ptr_eq(&retained_document, &session.active().unwrap().doc));

    let healthy = independent_drawing_saves(&mut session, &path, first);
    session.set_active(0);
    session.retry_local_projects().unwrap();
    assert!(session.flush_local_projects(Duration::from_secs(20)).is_err());
    assert_eq!(session.local_projects.status(first).pending, 2, "retry cannot replace or drop the failed request");
    assert_eq!(session.local_replay.pending_for(first), 1);
    assert!(session.local_projects.status(healthy).error.is_none());
    assert_eq!(*session.active().unwrap().doc, *retained_document);
    let stored = competing.load(project).unwrap();
    assert_eq!(stored.info.revision, 2);
    assert_eq!(stored.document, competing_document, "a stale local retry must not overwrite the other writer");
    assert_eq!(competing.replay_page(project, 0, 128).unwrap().len(), 2);
}

#[test]
fn small_canvas_with_oversized_encoded_checkpoint_keeps_original_and_allows_other_drafts() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("drafts.sqlite3");
    let mut session = open(&path);
    let first = drawing(&mut session, "Oversized embedded metadata");
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let project = session.local_projects.status(first).project.unwrap();
    let original = session.active().unwrap().doc.clone();
    // A tiny canvas can carry large lossless imported metadata. Use incompressible
    // bytes so this exercises the actual encoded checkpoint limit, not a mock error.
    let mut metadata = vec![0_u8; photocraft_local_store::MAX_CHECKPOINT_BYTES + 4096];
    getrandom::fill(&mut metadata).unwrap();
    let metadata = Arc::new(metadata);
    let state = session.active_mut().unwrap();
    Arc::make_mut(&mut state.doc).metadata.exif = Some(metadata.clone());
    state.revision = state.revision.saturating_add(1);
    assert!(session.flush_local_projects(Duration::from_secs(90)).is_err());
    let failed = session.local_projects.status(first);
    assert!(failed.error.as_deref().unwrap().contains("64 MiB"));
    assert_eq!((failed.local_revision, failed.pending), (1, 1));
    assert!(Arc::ptr_eq(session.active().unwrap().doc.metadata.exif.as_ref().unwrap(), &metadata));
    independent_drawing_saves(&mut session, &path, first);
    let store = Store::open(&path, StoreOptions::default()).unwrap();
    let preserved = store.load(project).unwrap();
    assert_eq!(preserved.info.revision, 1);
    assert_eq!(preserved.document, *original);
}

#[test]
fn invalid_document_encoding_is_local_and_missing_open_is_a_nonblocking_notice() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("drafts.sqlite3");
    let mut session = open(&path);
    let first = drawing(&mut session, "Invalid checkpoint");
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let state = session.active_mut().unwrap();
    Arc::make_mut(&mut state.doc).resolution_dpi = f32::NAN;
    state.revision = state.revision.saturating_add(1);
    assert!(session.flush_local_projects(Duration::from_secs(20)).is_err());
    assert!(session.local_projects.status(first).error.is_some());
    independent_drawing_saves(&mut session, &path, first);

    session.open_local_project(fresh_project_id().unwrap()).unwrap();
    assert!(session.flush_local_projects(Duration::from_secs(20)).is_err());
    assert!(session.local_projects.notice.as_deref().unwrap().contains("not found"));
    assert!(session.local_projects.error.is_none());
    session.execute("layer.renameLayer", json!({"name":"Healthy after missing open"})).unwrap();
    assert!(session.flush_local_projects(Duration::from_secs(20)).is_err());
    assert_eq!(session.local_projects.status(session.active().unwrap().doc.id).local_revision, 3);
    assert!(session.local_document_saved(session.active().unwrap().doc.id));
}
