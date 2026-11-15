#![cfg(not(target_arch = "wasm32"))]

use photocraft_local_store::canvas::CanvasStore;
use photocraft_local_store::{Error, StoreOptions};
use photocraft_scene::{Bookmark, CanvasId, CanvasInfo, RegionKey, Srgba, Stroke, StrokeId, Viewport, coordinates};

fn stroke(info: &CanvasInfo, n: u64) -> Stroke {
    Stroke {
        id: StrokeId::new(&format!("{n:032x}")).unwrap(),
        canvas: info.id,
        owner: info.owner,
        revision: 0,
        anchor: RegionKey::default(),
        points: vec![[480.0, 500.0], [540.0, 520.0]],
        width: 8.0,
        color: Srgba { rgba: [0.1, 0.4, 0.7, 1.0] },
        erase: false,
    }
}
fn view(info: &CanvasInfo) -> Viewport {
    Viewport { camera: info.camera.clone(), width: 1024, height: 768 }
}

#[test]
fn four_same_named_canvases_keep_owner_artwork_camera_bookmarks_and_history_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("canvases.sqlite3");
    let mut store = CanvasStore::open(&path, StoreOptions::default()).unwrap();
    let owner = store.owner();
    let mut saved = Vec::new();
    for index in 1..=4 {
        let mut info = store.create(if index <= 2 { "Sketchbook" } else { "Another canvas" }).unwrap();
        let first = stroke(&info, index * 10);
        info = store.append(&first, info.revision).unwrap();
        let second = stroke(&info, index * 10 + 1);
        info = store.append(&second, info.revision).unwrap();
        let mut camera = info.camera.clone();
        camera.zoom = 0.5 + index as f64 / 20.0;
        info = store.save_camera(camera.clone()).unwrap();
        info = store.set_bookmarks(info.id, info.revision, vec![Bookmark { name: format!("Place {index}"), camera }]).unwrap();
        if index % 2 == 0 {
            info = store.undo(info.id, info.revision).unwrap();
        }
        saved.push(info);
    }
    assert_eq!(store.list().unwrap().len(), 4);
    assert_ne!(saved[0].id, saved[1].id);
    assert_eq!(saved[0].name, saved[1].name);
    drop(store);
    let mut store = CanvasStore::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(store.owner(), owner);
    assert_eq!(store.active().unwrap().unwrap().id, saved[3].id);
    for (index, expected) in saved.iter().enumerate() {
        let info = store.switch(expected.id, None).unwrap();
        assert_eq!(&info, expected);
        let loaded = store.view(&view(&info)).unwrap();
        assert_eq!(loaded.strokes.len(), if index % 2 == 0 { 2 } else { 1 });
        assert!(loaded.strokes.iter().all(|s| s.canvas == info.id && s.owner == owner));
        if index % 2 == 1 {
            let redone = store.redo(info.id, info.revision).unwrap();
            assert_eq!(store.view(&view(&redone)).unwrap().strokes.len(), 2);
        }
    }
}

#[test]
fn failed_switch_and_foreign_camera_bookmark_owner_do_not_change_either_canvas() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = CanvasStore::open(dir.path().join("c.sqlite3"), StoreOptions::default()).unwrap();
    let a = store.create("A").unwrap();
    let b = store.create("B").unwrap();
    assert!(store.switch(a.id, Some(a.camera.clone())).is_err());
    let absent = CanvasId::new("ffffffffffffffffffffffffffffffff").unwrap();
    assert!(store.switch(absent, Some(b.camera.clone())).is_err());
    assert!(store.set_bookmarks(a.id, a.revision, vec![Bookmark { name: "Foreign".into(), camera: b.camera.clone() }]).is_err());
    let other = CanvasStore::open(dir.path().join("other.sqlite3"), StoreOptions::default()).unwrap();
    let mut mark = stroke(&a, 1);
    mark.owner = other.owner();
    assert!(store.append(&mark, a.revision).is_err());
    assert_eq!(store.info(a.id).unwrap(), a);
    assert_eq!(store.info(b.id).unwrap(), b);
    assert_eq!(store.active().unwrap().unwrap().id, b.id);
}

#[test]
fn switch_atomically_saves_outgoing_camera_and_rename_does_not_clear_undo() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.sqlite3");
    let mut store = CanvasStore::open(&path, StoreOptions::default()).unwrap();
    let mut a = store.create("A").unwrap();
    a = store.append(&stroke(&a, 1), 0).unwrap();
    a = store.rename(a.id, a.revision, "Renamed").unwrap();
    let b = store.create("B").unwrap();
    let mut camera = b.camera.clone();
    camera.zoom = 1.25;
    store.switch(a.id, Some(camera.clone())).unwrap();
    drop(store);
    let mut store = CanvasStore::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(store.info(b.id).unwrap().camera, camera);
    assert_eq!(store.active().unwrap().unwrap().name, "Renamed");
    a = store.undo(a.id, a.revision).unwrap();
    assert_eq!(a.redo_count, 1);
    assert!(store.view(&view(&a)).unwrap().strokes.is_empty());
}

#[test]
fn exact_retries_are_idempotent_conflicts_and_stale_edits_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = CanvasStore::open(dir.path().join("c.sqlite3"), StoreOptions::default()).unwrap();
    let a = store.create("A").unwrap();
    let mark = stroke(&a, 1);
    let saved = store.append(&mark, 0).unwrap();
    assert_eq!(store.append(&mark, 0).unwrap(), saved);
    let mut changed = mark.clone();
    changed.width = 12.0;
    assert!(matches!(store.append(&changed, 0), Err(Error::Conflict)));
    assert!(matches!(store.rename(a.id, 0, "Stale"), Err(Error::Conflict)));
    let undone = store.undo(a.id, saved.revision).unwrap();
    assert_eq!(store.append(&mark, 0).unwrap(), undone);
    let next = store.append(&stroke(&a, 2), undone.revision).unwrap();
    assert_eq!(next.redo_count, 0);
    assert_eq!(next.stroke_count, 2);
    assert!(store.redo(a.id, next.revision).is_err());
    assert_eq!(store.view(&view(&next)).unwrap().strokes.len(), 1);
}

#[test]
fn boundary_strokes_deduplicate_and_offscreen_payloads_are_never_loaded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.sqlite3");
    let mut store = CanvasStore::open(&path, StoreOptions::default()).unwrap();
    let mut info = store.create("Viewport").unwrap();
    let mut cross = stroke(&info, 1);
    cross.points = vec![[900.0, 500.0], [1200.0, 500.0]];
    info = store.append(&cross, 0).unwrap();
    let mut viewport = view(&info);
    viewport.camera = coordinates::target(info.id, coordinates::point(&RegionKey::default(), [1024.0, 512.0]).unwrap(), 0, 1.0).unwrap();
    let before = store.view(&viewport).unwrap();
    assert_eq!(before.strokes.len(), 1);
    for n in 2..=120 {
        let mut far = stroke(&info, n);
        far.anchor.x = photocraft_scene::Wide(n as i128 * 3);
        info = store.append(&far, info.revision).unwrap();
    }
    let after = store.view(&viewport).unwrap();
    assert_eq!(after.strokes, before.strokes);
    assert_eq!(after.stats, before.stats);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE strokes SET payload=zeroblob(2000000),hash='broken' WHERE id!=?1", [cross.id.as_str()]).unwrap();
    assert_eq!(store.view(&viewport).unwrap().strokes, before.strokes, "corrupt offscreen payload is not fetched");
}

#[test]
fn six_nested_scales_use_summaries_and_keep_fine_original_samples() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = CanvasStore::open(dir.path().join("c.sqlite3"), StoreOptions::default()).unwrap();
    let mut info = store.create("Nested").unwrap();
    let center = coordinates::center(&info.camera).unwrap();
    let mut deepest = None;
    for depth in 0..=6 {
        let camera = coordinates::target(info.id, center, depth, 1.0).unwrap();
        let local = coordinates::local_at(&camera.region, center).unwrap();
        let mut mark = stroke(&info, u64::from(depth) + 1);
        mark.anchor = camera.region;
        mark.points = (0..100).map(|n| [local[0] + f64::from(n) / 10.0, local[1] + (f64::from(n) / 7.0).sin()]).collect();
        info = store.append(&mark, info.revision).unwrap();
        deepest = Some(camera);
    }
    let outer = store.view(&view(&info)).unwrap();
    assert!(outer.stats.summary_strokes >= 5);
    assert!(outer.stats.full_strokes >= 1);
    let inner = store.view(&Viewport { camera: deepest.unwrap(), width: 1024, height: 768 }).unwrap();
    let fine = inner.strokes.iter().find(|s| s.anchor.depth == 6).unwrap();
    assert_eq!(fine.points.len(), 100);
}

#[test]
fn corrupt_hash_and_schema_are_errors_and_other_canvas_remains_readable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.sqlite3");
    let mut store = CanvasStore::open(&path, StoreOptions::default()).unwrap();
    let a = store.create("A").unwrap();
    let mark = stroke(&a, 1);
    let a = store.append(&mark, 0).unwrap();
    let b = store.create("B").unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE strokes SET hash='broken' WHERE canvas=?1", [a.id.as_str()]).unwrap();
    assert!(store.view(&view(&a)).is_err());
    assert!(store.append(&mark, 0).is_err());
    assert_eq!(store.info(b.id).unwrap(), b);
    db.pragma_update(None, "user_version", 999).unwrap();
    drop(store);
    drop(db);
    let bytes = std::fs::read(&path).unwrap();
    assert!(matches!(CanvasStore::open(&path, StoreOptions::default()), Err(Error::Version(999))));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn exhausted_quota_preserves_prior_canvas_and_rejects_invalid_requests() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.sqlite3");
    let mut store = CanvasStore::open(&path, StoreOptions { quota_bytes: 96 * 1024 }).unwrap();
    let info = store.create("Preserved").unwrap();
    let mut large = stroke(&info, 1);
    large.points = (0..8192).map(|n| [480.0 + f64::from(n) / 100.12345, 500.0 + (f64::from(n) / 7.54321).sin()]).collect();
    assert!(matches!(store.append(&large, 0), Err(Error::Full)));
    assert_eq!(store.info(info.id).unwrap(), info);
    assert!(store.view(&view(&info)).unwrap().strokes.is_empty());
    let mut invalid = stroke(&info, 2);
    invalid.width = f64::NAN;
    assert!(store.append(&invalid, 0).is_err());
    let mut bad_view = view(&info);
    bad_view.width = u32::MAX;
    assert!(store.view(&bad_view).is_err());
    drop(store);
    let reopened = CanvasStore::open(&path, StoreOptions { quota_bytes: 96 * 1024 }).unwrap();
    assert_eq!(reopened.info(info.id).unwrap(), info);
}

#[test]
fn new_canvas_commits_outgoing_camera_and_rejects_foreign_context_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.sqlite3");
    let mut store = CanvasStore::open(&path, StoreOptions::default()).unwrap();
    let a = store.create("A").unwrap();
    let mut camera = a.camera.clone();
    camera.zoom = 1.5;
    let b = store.create_with_previous("B", Some(camera.clone())).unwrap();
    assert_eq!(store.info(a.id).unwrap().camera, camera);
    assert!(store.create_with_previous("Not created", Some(camera.clone())).is_err());
    assert_eq!(store.list().unwrap().len(), 2);
    drop(store);
    let reopened = CanvasStore::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(reopened.active().unwrap().unwrap(), b);
    assert_eq!(reopened.info(a.id).unwrap().camera, camera);
}

#[test]
fn summaries_are_checksummed_and_spatial_queries_use_a_covering_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.sqlite3");
    let mut store = CanvasStore::open(&path, StoreOptions::default()).unwrap();
    let mut info = store.create("Summary").unwrap();
    let center = coordinates::center(&info.camera).unwrap();
    let camera = coordinates::target(info.id, center, 3, 0.75).unwrap();
    let local = coordinates::local_at(&camera.region, center).unwrap();
    let mut mark = stroke(&info, 1);
    mark.anchor = camera.region;
    mark.points = (0..100).map(|n| [local[0] + f64::from(n) / 10.0, local[1]]).collect();
    info = store.append(&mark, 0).unwrap();
    let outer = store.view(&view(&info)).unwrap();
    assert_eq!(outer.stats.summary_strokes, 1);
    assert_eq!(outer.strokes[0].points.len(), 24);
    let db = rusqlite::Connection::open(&path).unwrap();
    let mut statement = db
        .prepare("EXPLAIN QUERY PLAN SELECT stroke FROM cells WHERE canvas='x' AND depth=0 AND x='0' AND y='0' AND state=1 AND source_depth BETWEEN 0 AND 12")
        .unwrap();
    let plan = statement.query_map([], |r| r.get::<_, String>(3)).unwrap().map(Result::unwrap).collect::<Vec<_>>().join(" ");
    assert!(plan.contains("COVERING INDEX cells_view"), "{plan}");
    db.execute("UPDATE strokes SET summary_hash='broken' WHERE canvas=?1", [info.id.as_str()]).unwrap();
    assert!(store.view(&view(&info)).is_err());
    let fine = store.view(&Viewport { camera, width: 1024, height: 768 }).unwrap();
    assert_eq!(fine.strokes[0].points.len(), 100, "canonical payload remains independently readable");
}

#[test]
fn finer_view_index_excludes_offscreen_descendants_inside_the_same_parent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.sqlite3");
    let mut store = CanvasStore::open(&path, StoreOptions::default()).unwrap();
    let mut info = store.create("Fine viewport").unwrap();
    let center = coordinates::center(&info.camera).unwrap();
    let fine_camera = coordinates::target(info.id, center, 2, 1.0).unwrap();
    let local = coordinates::local_at(&fine_camera.region, center).unwrap();
    let mut mark = stroke(&info, 1);
    mark.anchor = fine_camera.region;
    mark.points = vec![local, [local[0] + 10.0, local[1] + 10.0]];
    info = store.append(&mark, 0).unwrap();
    let viewport = Viewport { camera: coordinates::target(info.id, center, 0, 50.0).unwrap(), width: 1024, height: 768 };
    let before = store.view(&viewport).unwrap();
    assert_eq!(before.strokes.len(), 1);
    for n in 2..=120 {
        let mut far = mark.clone();
        far.id = StrokeId::new(&format!("{n:032x}")).unwrap();
        far.anchor.x.0 += 1000 + n * 3;
        info = store.append(&far, info.revision).unwrap();
    }
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE strokes SET summary_hash='broken',hash='broken' WHERE id!=?1", [mark.id.as_str()]).unwrap();
    let after = store.view(&viewport).unwrap();
    assert_eq!(after.strokes, before.strokes);
    assert_eq!(after.stats, before.stats);
}

#[test]
fn damaged_index_metadata_cannot_silently_select_a_false_summary() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.sqlite3");
    let mut store = CanvasStore::open(&path, StoreOptions::default()).unwrap();
    let info = store.create("Index integrity").unwrap();
    let mark = stroke(&info, 1);
    let info = store.append(&mark, 0).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE strokes SET extent=0.001 WHERE canvas=?1", [info.id.as_str()]).unwrap();
    assert!(store.view(&view(&info)).is_err());
    assert_eq!(store.info(info.id).unwrap(), info);
}

#[test]
fn exact_float_request_retry_keeps_original_identity_and_counter() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = CanvasStore::open(dir.path().join("c.sqlite3"), StoreOptions::default()).unwrap();
    let info = store.create("Float identity").unwrap();
    let mut mark = stroke(&info, 1);
    mark.points = (0..500).map(|n| [480.0 + f64::from(n) / 123.456789, 500.0 + (f64::from(n) / 7.54321).sin()]).collect();
    let saved = store.append(&mark, 0).unwrap();
    assert_eq!(store.append(&mark, 0).unwrap(), saved);
}
