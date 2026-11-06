#![cfg(not(target_arch = "wasm32"))]

use photocraft_doc::{Color, ColorMode, Document, Layer, SampleType, Size};
use photocraft_local_store::{Error, ReplayKind, ReplayRecord, SaveRequest, SaveStage, Store, StoreOptions, fresh_event_id, fresh_project_id};
use serde_json::json;

fn request(depth: SampleType) -> SaveRequest {
    let mut document = Document::with_background("Drawing", Size { width: 32, height: 24 }, ColorMode::Rgb, depth, Color::WHITE);
    document.layers.push(Layer::raster("Ink", document.pixel_format()));
    SaveRequest {
        project: fresh_project_id().unwrap(),
        expected_revision: 0,
        request_id: fresh_event_id().unwrap(),
        title: "Local drawing".into(),
        document,
        event: ReplayRecord {
            schema_version: 1,
            command: "file.new".into(),
            params: json!({"width":32,"height":24}),
            elapsed_ms: 0,
            kind: ReplayKind::Baseline,
        },
    }
}

fn next(request: &SaveRequest, command: &str) -> SaveRequest {
    let mut result = request.clone();
    result.expected_revision += 1;
    result.request_id = fresh_event_id().unwrap();
    result.event.command = command.into();
    result.event.elapsed_ms += 100;
    result.event.kind = ReplayKind::Resolved;
    result
}

#[test]
fn durable_roundtrip_all_depths_and_replay_pages() {
    for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drafts.sqlite3");
        let mut store = Store::open(&path, StoreOptions::default()).unwrap();
        let a = request(depth);
        let first = store.save(&a).unwrap();
        assert_eq!(first.revision, 1);
        assert_eq!(first.device_sequence, 1);
        let mut b = next(&a, "edit.undo");
        b.event.kind = ReplayKind::Undo;
        b.document.layers[1].name = "Undo result".into();
        assert_eq!(store.save(&b).unwrap().revision, 2);
        let mut c = next(&b, "edit.redo");
        c.event.kind = ReplayKind::Redo;
        c.document.layers[1].name = "Redo result".into();
        assert_eq!(store.save(&c).unwrap().revision, 3);
        drop(store);
        let reopened = Store::open(&path, StoreOptions::default()).unwrap();
        let loaded = reopened.load(a.project).unwrap();
        assert_eq!(loaded.document, c.document);
        assert_eq!(loaded.info.revision, 3);
        assert_eq!(loaded.info.replay_elapsed_ms, 200);
        assert!(loaded.recovery_warning.is_none());
        assert_eq!(reopened.list().unwrap().len(), 1);
        assert_eq!(reopened.replay_page(a.project, 0, 2).unwrap(), vec![a.event, b.event]);
        assert_eq!(reopened.replay_page(a.project, 2, 2).unwrap(), vec![c.event]);
        assert!(reopened.usage().unwrap().stored_bytes > 0);
    }
}

#[test]
fn retries_do_not_allocate_again_and_conflicting_retries_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("drafts.sqlite3");
    let mut store = Store::open(&path, StoreOptions::default()).unwrap();
    let a = request(SampleType::U8);
    let ack = store.save(&a).unwrap();
    assert_eq!(store.save(&a).unwrap(), ack);
    let mut changed = a.clone();
    changed.title = "Changed request".into();
    assert!(matches!(store.save(&changed), Err(Error::Conflict)));
    drop(store);
    let mut store = Store::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(store.save(&a).unwrap(), ack);
    let b = next(&a, "layer.rename");
    assert_eq!(store.save(&b).unwrap().device_sequence, 2);
    assert_eq!(store.replay_page(a.project, 0, 128).unwrap().len(), 2);
}

#[test]
fn stale_writer_and_regressing_time_cannot_replace_current_draft() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("drafts.sqlite3");
    let mut first = Store::open(&path, StoreOptions::default()).unwrap();
    let mut second = Store::open(&path, StoreOptions::default()).unwrap();
    let a = request(SampleType::U8);
    first.save(&a).unwrap();
    let b = next(&a, "paint.stroke");
    first.save(&b).unwrap();
    let competing = next(&a, "paint.erase");
    assert!(matches!(second.save(&competing), Err(Error::Conflict)));
    let mut regressing = next(&b, "paint.stroke");
    regressing.event.elapsed_ms = 0;
    assert!(matches!(second.save(&regressing), Err(Error::Conflict)));
    assert_eq!(second.load(a.project).unwrap().info.revision, 2);
}

#[test]
fn sqlite_full_rolls_back_checkpoint_event_sequence_and_head() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("drafts.sqlite3");
    let mut store = Store::open(&path, StoreOptions { quota_bytes: 128 * 1024 }).unwrap();
    let a = request(SampleType::U8);
    store.save(&a).unwrap();
    let mut b = next(&a, "file.place");
    let mut bytes = vec![0_u8; 400_000];
    getrandom::fill(&mut bytes).unwrap();
    b.event.params = json!({"embedded_asset":bytes});
    let mut stages = Vec::new();
    assert!(matches!(store.save_observed(&b, |stage| stages.push(stage)), Err(Error::Full)));
    assert!(!stages.contains(&SaveStage::AfterCommit));
    assert_eq!(store.load(a.project).unwrap().document, a.document);
    assert_eq!(store.replay_page(a.project, 0, 128).unwrap().len(), 1);
    drop(store);
    let mut store = Store::open(&path, StoreOptions::default()).unwrap();
    let ack = store.save(&b).unwrap();
    assert_eq!((ack.revision, ack.device_sequence), (2, 2));
    assert_eq!(store.load(a.project).unwrap().info.revision, 2);
}

#[test]
fn encoded_checkpoints_that_exceed_reopen_limits_never_acknowledge() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("drafts.sqlite3");
    let mut store = Store::open(&path, StoreOptions::default()).unwrap();
    let original = request(SampleType::U8);
    store.save(&original).unwrap();
    let before = std::fs::read(&path).unwrap();
    for oversized_blob in [true, false] {
        let mut rejected = next(&original, "checkpoint.externalEdit");
        rejected.event.kind = ReplayKind::Checkpoint;
        if oversized_blob {
            // Highly compressible metadata fits the encoded checkpoint cap while its
            // decoded blob exceeds the limit used when the acknowledged draft reopens.
            rejected.document.metadata.exif = Some(std::sync::Arc::new(vec![7; 64 * 1024 * 1024 + 1]));
        } else {
            // Native format saves deliberately have no manifest byte cap. Local drafts
            // must enforce their tighter reopening limit before publishing this result.
            rejected.document.name = "x".repeat(8 * 1024 * 1024 + 1);
        }
        let encoded = photocraft_format::save_to_bytes(&rejected.document, &photocraft_format::SaveOptions::default()).unwrap();
        assert!(encoded.len() < photocraft_local_store::MAX_CHECKPOINT_BYTES, "fixture must pass the encoded-size check");
        drop(encoded);
        let mut stages = Vec::new();
        let error = store.save_observed(&rejected, |stage| stages.push(stage)).unwrap_err();
        assert!(matches!(error, Error::Invalid(_)), "expected a project-specific limit failure: {error}");
        assert!(error.to_string().contains("cannot be reopened"));
        assert!(stages.is_empty(), "an unreadable checkpoint must be rejected before starting a commit");
        assert_eq!(std::fs::read(&path).unwrap(), before, "the prior database must remain untouched");
        assert_eq!(store.load(original.project).unwrap().document, original.document);
        assert_eq!(store.replay_page(original.project, 0, 128).unwrap().len(), 1);
    }
    drop(store);
    let mut reopened = Store::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(reopened.load(original.project).unwrap().document, original.document);
    let next_valid = next(&original, "layer.renameLayer");
    let ack = reopened.save(&next_valid).unwrap();
    assert_eq!((ack.revision, ack.device_sequence), (2, 2), "rejected checkpoints consume neither revision nor device sequence");
}

#[test]
fn corrupted_latest_record_recovers_previous_without_overwriting_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("drafts.sqlite3");
    let mut store = Store::open(&path, StoreOptions::default()).unwrap();
    let a = request(SampleType::U8);
    store.save(&a).unwrap();
    let mut b = next(&a, "paint.stroke");
    b.document.layers[1].name = "Damaged later version".into();
    store.save(&b).unwrap();
    drop(store);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE chunks SET data=zeroblob(length(data)) WHERE hash IN (SELECT chunk FROM object_chunks WHERE object IN (SELECT payload FROM revisions WHERE revision=2))", []).unwrap();
    drop(db);
    let mut store = Store::open(&path, StoreOptions::default()).unwrap();
    let loaded = store.load(a.project).unwrap();
    assert_eq!(loaded.document, a.document);
    assert_eq!((loaded.info.revision, loaded.head_revision), (1, 2));
    assert!(loaded.recovery_warning.unwrap().contains("Recovered revision 1"));
    assert!(matches!(store.save(&b), Err(Error::Corrupt(_))));
    let mut overwrite = next(&a, "paint.stroke");
    overwrite.document = a.document.clone();
    assert!(matches!(store.save(&overwrite), Err(Error::Conflict)));
    assert_eq!(store.list().unwrap()[0].revision, 2);
}

#[test]
fn future_schema_is_not_rewritten_and_unversioned_foreign_db_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("future.sqlite3");
    drop(Store::open(&path, StoreOptions::default()).unwrap());
    let db = rusqlite::Connection::open(&path).unwrap();
    db.pragma_update(None, "user_version", 99).unwrap();
    drop(db);
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(Store::open(&path, StoreOptions::default()), Err(Error::Version(99))));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let other = dir.path().join("other.sqlite3");
    let db = rusqlite::Connection::open(&other).unwrap();
    db.execute("CREATE TABLE personal(data TEXT)", []).unwrap();
    drop(db);
    let before = std::fs::read(&other).unwrap();
    assert!(matches!(Store::open(&other, StoreOptions::default()), Err(Error::Corrupt(_))));
    assert_eq!(std::fs::read(&other).unwrap(), before);
}

#[test]
fn malformed_records_fail_without_ack_or_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    let a = request(SampleType::U8);
    store.save(&a).unwrap();
    let mut b = next(&a, "paint.stroke");
    b.event.schema_version = 99;
    assert!(matches!(store.save(&b), Err(Error::Version(99))));
    b.event.schema_version = 1;
    b.event.params = json!({"nested":null});
    for _ in 0..70 {
        b.event.params = json!([b.event.params]);
    }
    assert!(matches!(store.save(&b), Err(Error::Invalid(_))));
    b.event.params = json!({});
    b.document.size.width = u32::MAX;
    assert!(matches!(store.save(&b), Err(Error::Invalid(_))));
    assert_eq!(store.load(a.project).unwrap().info.revision, 1);
    assert!(store.replay_page(a.project, 0, usize::MAX).is_err());
}

#[test]
fn corrupt_and_future_event_envelopes_cannot_be_ignored_by_load_replay_or_retry() {
    use sha2::{Digest, Sha256};
    for mutation in ["version", "project", "result"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drafts.sqlite3");
        let mut store = Store::open(&path, StoreOptions::default()).unwrap();
        let a = request(SampleType::U8);
        store.save(&a).unwrap();
        let b = next(&a, "paint.stroke");
        store.save(&b).unwrap();
        drop(store);
        let db = rusqlite::Connection::open(&path).unwrap();
        let bytes: Vec<u8> = db.query_row("SELECT envelope FROM revisions WHERE revision=2", [], |r| r.get(0)).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        match mutation {
            "version" => value["schema_version"] = json!(99),
            "project" => value["project_id"] = json!("ffffffffffffffffffffffffffffffff"),
            _ => value["step"]["result"]["asset"]["hash"] = json!("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"),
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        // Recompute checksum deliberately: semantic association checks must also work.
        let digest = format!("{:x}", Sha256::digest(&bytes));
        db.execute("UPDATE revisions SET envelope=?1,envelope_hash=?2 WHERE revision=2", rusqlite::params![bytes, digest]).unwrap();
        drop(db);
        let mut store = Store::open(&path, StoreOptions::default()).unwrap();
        let recovered = store.load(a.project).unwrap();
        assert_eq!((recovered.info.revision, recovered.head_revision), (1, 2));
        assert!(recovered.recovery_warning.is_some());
        assert!(store.replay_page(a.project, 0, 128).is_err());
        assert!(store.save(&b).is_err());
    }
}

#[test]
fn unchanged_native_entries_share_stored_chunks_across_checkpoints() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("drafts.sqlite3");
    let mut store = Store::open(&path, StoreOptions::default()).unwrap();
    let a = request(SampleType::U8);
    store.save(&a).unwrap();
    let mut b = next(&a, "layer.rename");
    b.document.layers[1].name = "Much longer renamed layer makes manifest length differ".into();
    store.save(&b).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    let shared:i64 = db.query_row("SELECT count(*) FROM object_chunks a JOIN object_chunks b ON a.chunk=b.chunk WHERE a.object=(SELECT checkpoint FROM revisions WHERE revision=1) AND b.object=(SELECT checkpoint FROM revisions WHERE revision=2)",[],|r|r.get(0)).unwrap();
    assert!(shared > 0, "unchanged compressed tiles should be shared despite shifting ZIP offsets");
    assert_eq!(store.load_revision(a.project, 1).unwrap().0, a.document);
    assert_eq!(store.load_revision(a.project, 2).unwrap().0, b.document);
}

#[test]
fn missing_replay_rows_are_reported_instead_of_silently_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("drafts.sqlite3");
    let mut store = Store::open(&path, StoreOptions::default()).unwrap();
    let a = request(SampleType::U8);
    store.save(&a).unwrap();
    store.save(&next(&a, "paint.stroke")).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("DELETE FROM revisions WHERE revision=2", []).unwrap();
    assert!(matches!(store.replay_page(a.project, 0, 128), Err(Error::Corrupt(_))));
    assert!(matches!(store.replay_page(a.project, 1, 128), Err(Error::Corrupt(_))));
    let recovered = store.load(a.project).unwrap();
    assert_eq!((recovered.info.revision, recovered.head_revision), (1, 2));
}

#[test]
fn truncated_database_is_retained_and_returns_actionable_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("drafts.sqlite3");
    let mut store = Store::open(&path, StoreOptions::default()).unwrap();
    store.save(&request(SampleType::U8)).unwrap();
    drop(store);
    std::fs::OpenOptions::new().write(true).open(&path).unwrap().set_len(100).unwrap();
    let original = std::fs::read(&path).unwrap();
    let error = match Store::open(&path, StoreOptions::default()) {
        Ok(_) => panic!("truncated database accepted"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("retained") || error.to_string().contains("preserved"));
    assert_eq!(std::fs::read(&path).unwrap(), original);
}
