#![cfg(not(target_arch = "wasm32"))]
use photocraft_engine::{
    Session,
    scene::{self, Camera, CanvasId, coordinates},
};
use serde_json::{Value, json};
use std::time::Duration;

fn finish(s: &mut Session) {
    s.flush_canvases(Duration::from_secs(15)).unwrap();
}
fn session(path: &std::path::Path) -> Session {
    let mut s = Session::new();
    s.enable_canvases(path, Default::default()).unwrap();
    finish(&mut s);
    s
}
fn command(s: &mut Session, id: &str, params: Value) {
    s.execute(&format!("komaphone.canvas.{id}"), params).unwrap();
    finish(s);
}
fn create(s: &mut Session, name: &str) -> CanvasId {
    command(s, "create", json!({"name":name}));
    s.canvases.active.as_ref().unwrap().id
}
fn draw(s: &mut Session, camera: &Camera) {
    command(
        s,
        "stroke",
        json!({"canvas":camera.canvas,"anchor":camera.region,"points":[[-20.0,0.0],[0.0,-20.0],[20.0,0.0],[20.0,20.0],[-20.0,20.0],[-20.0,0.0]],"width":3.0,"color":{"rgba":[0.1,0.3,0.7,1.0]},"erase":false}),
    );
}

#[test]
fn four_canvas_commands_keep_names_history_camera_bookmarks_and_reopen_separate() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("canvas.sqlite3");
    let mut s = session(&db);
    let mut records = Vec::new();
    for index in 0..4 {
        let canvas = create(&mut s, if index < 2 { "Same name" } else { "Other canvas" });
        let home = coordinates::home(canvas);
        let origin = coordinates::center(&home).unwrap();
        let camera = coordinates::target(canvas, origin, index, 1.0).unwrap();
        command(&mut s, "camera", json!({"camera":camera}));
        draw(&mut s, &camera);
        command(&mut s, "bookmark", json!({"name":format!("Place {index}")}));
        records.push((canvas, camera));
    }
    assert_eq!(s.canvases.items.len(), 4);
    assert_ne!(records[0].0, records[1].0);
    let owner = s.canvases.owner;
    drop(s);
    let mut s = session(&db);
    assert_eq!(s.canvases.owner, owner);
    assert_eq!(s.canvases.active.as_ref().unwrap().id, records[3].0);
    for (index, (id, camera)) in records.iter().enumerate() {
        command(&mut s, "open", json!({"canvas":id}));
        assert_eq!(s.canvases.camera.as_ref(), Some(camera));
        let active = s.canvases.active.as_ref().unwrap();
        assert_eq!(active.stroke_count, 1);
        assert_eq!(active.bookmarks.len(), 1);
        assert_eq!(active.bookmarks[0].name, format!("Place {index}"));
        command(&mut s, "undo", json!({}));
        assert_eq!(s.canvases.active.as_ref().unwrap().undo_count, 0);
        command(&mut s, "redo", json!({}));
        assert_eq!(s.canvases.active.as_ref().unwrap().undo_count, 1);
    }
    assert!(s.documents().is_empty(), "world operations must not mutate finite documents");
}

#[test]
fn seven_direct_depths_reopen_and_view_loads_saved_strokes() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("canvas.sqlite3");
    let mut s = session(&db);
    let id = create(&mut s, "Nested drawings");
    let home = coordinates::home(id);
    let center = coordinates::center(&home).unwrap();
    for depth in 0..=6 {
        let camera = coordinates::target(id, center, depth, 1.0).unwrap();
        command(&mut s, "camera", json!({"camera":camera}));
        draw(&mut s, &camera);
        command(&mut s, "bookmark", json!({"name":format!("Depth {depth}")}));
    }
    drop(s);
    let mut s = session(&db);
    assert_eq!(s.canvases.active.as_ref().unwrap().undo_count, 7);
    for depth in (0..=6).rev() {
        command(&mut s, "go", json!({"name":format!("Depth {depth}"),"animate":false}));
        s.set_canvas_viewport(1000, 600);
        finish(&mut s);
        let view = s.canvases.view.as_ref().unwrap();
        assert_eq!(view.viewport.camera.region.depth, depth);
        assert!(view.strokes.iter().any(|stroke| stroke.anchor.depth == depth));
        assert!(view.strokes.iter().all(|stroke| stroke.canvas == id));
        assert!(view.stats.points <= scene::MAX_VIEW_POINTS);
    }
}

#[test]
fn canvas_commands_reject_foreign_and_malformed_requests_without_touching_art() {
    let root = tempfile::tempdir().unwrap();
    let mut s = session(&root.path().join("canvas.sqlite3"));
    let first = create(&mut s, "First");
    let first_camera = coordinates::home(first);
    let second = create(&mut s, "Second");
    let before = s.canvases.active.clone();
    assert!(s.execute("komaphone.canvas.camera", json!({"camera":first_camera})).is_err());
    assert!(
        s.execute("komaphone.canvas.stroke", json!({"canvas":first,"anchor":first_camera.region,"points":[[0,0]],"width":3,"color":{"rgba":[0,0,0,1]}}))
            .is_err()
    );
    for spec in photocraft_engine::commands::command_specs().iter().filter(|spec| spec.id.starts_with("komaphone.canvas.")) {
        for params in [json!({"unexpected":true}), json!([]), json!({"points":[]}), json!({"name":"\u{0}"})] {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.execute(spec.id, params)));
            assert!(result.is_ok(), "{} panicked", spec.id);
            assert!(result.unwrap().is_err(), "{} accepted malformed input", spec.id);
        }
    }
    assert_eq!(s.canvases.active, before);
    assert_eq!(s.canvases.active.as_ref().unwrap().id, second);
}

#[test]
fn flight_pause_cancel_and_new_canvas_preserve_outgoing_exact_camera() {
    let root = tempfile::tempdir().unwrap();
    let mut s = session(&root.path().join("canvas.sqlite3"));
    let first = create(&mut s, "First");
    command(&mut s, "bookmark", json!({"name":"Home"}));
    let center = coordinates::center(&coordinates::home(first)).unwrap();
    let deep = coordinates::target(first, center, 6, 2.0).unwrap();
    command(&mut s, "camera", json!({"camera":deep}));
    s.execute("komaphone.canvas.go", json!({"name":"Home","animate":true})).unwrap();
    s.tick_canvas_flight(0.1);
    s.execute("komaphone.canvas.flight", json!({"state":"pause"})).unwrap();
    let paused = s.canvases.camera.clone();
    s.tick_canvas_flight(0.1);
    assert_eq!(s.canvases.camera, paused);
    s.execute("komaphone.canvas.flight", json!({"state":"cancel"})).unwrap();
    assert!(s.canvases.flight.is_none());
    // Create immediately, before the ordinary camera debounce; its transaction must retain it.
    create(&mut s, "Second");
    command(&mut s, "open", json!({"canvas":first}));
    assert_eq!(s.canvases.camera, paused);
    assert_eq!(s.canvases.active.as_ref().unwrap().stroke_count, 0);
}
