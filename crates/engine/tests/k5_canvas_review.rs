//! Independent regressions for the local-canvas worker's durable acknowledgment seam.
#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;
use std::time::Duration;

use photocraft_engine::Session;
use photocraft_engine::scene::{CanvasId, coordinates};
use photocraft_local_store::canvas::CanvasStore;
use rusqlite::params;
use serde_json::{Value, json};

fn finish(session: &mut Session) {
    session.flush_canvases(Duration::from_secs(10)).unwrap();
}
fn session(path: &Path) -> Session {
    let mut session = Session::new();
    session.enable_canvases(path, Default::default()).unwrap();
    finish(&mut session);
    session
}
fn command(session: &mut Session, name: &str, args: Value) {
    session.execute(&format!("komaphone.canvas.{name}"), args).unwrap();
    finish(session);
}
fn create(session: &mut Session, name: &str) -> CanvasId {
    command(session, "create", json!({"name":name}));
    session.canvases.active.as_ref().unwrap().id
}
fn draw(session: &mut Session) {
    let camera = session.canvases.camera.as_ref().unwrap().clone();
    command(
        session,
        "stroke",
        json!({"canvas":camera.canvas,"anchor":camera.region,"points":[[480.0,500.0],[540.0,520.0]],"width":5.0,"color":{"rgba":[0.1,0.3,0.7,1.0]}}),
    );
}

fn committed_receipt_survives_refresh_failure(operation: &str) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("canvases.sqlite3");
    let mut session = session(&path);
    let unrelated = create(&mut session, "Unrelated readable canvas");
    let active = create(&mut session, "Active canvas");
    draw(&mut session);
    draw(&mut session);
    if operation == "redo" {
        command(&mut session, "undo", json!({}));
    }
    let before = session.canvases.active.as_ref().unwrap().clone();
    let database = rusqlite::Connection::open(&path).unwrap();
    let original_hash: String = database.query_row("SELECT hash FROM canvases WHERE id=?1", [unrelated.as_str()], |row| row.get(0)).unwrap();
    database.execute("UPDATE canvases SET hash='synthetic-unrelated-corruption' WHERE id=?1", [unrelated.as_str()]).unwrap();
    let request = if operation == "create" { json!({"name":"Created exactly once"}) } else { json!({}) };
    let enqueued = session.execute(&format!("komaphone.canvas.{operation}"), request).unwrap();
    assert_eq!(enqueued.get("saved"), Some(&Value::Bool(false)), "queue acceptance is not a durable acknowledgment");
    finish(&mut session);
    assert!(session.canvases.error.is_none(), "committed receipt must not become a retryable mutation failure");
    assert!(session.canvases.notice.is_some(), "failed list refresh remains visible rather than silently disappearing");
    let accepted = session.canvases.active.as_ref().unwrap().clone();
    if operation == "create" {
        assert_ne!(accepted.id, active);
        assert_eq!(accepted.name, "Created exactly once");
    } else {
        assert_eq!(accepted.id, active);
        assert_eq!(accepted.revision, before.revision + 1);
        assert_eq!(accepted.undo_count, if operation == "undo" { 1 } else { 2 });
        assert_eq!(accepted.redo_count, if operation == "undo" { 1 } else { 0 });
    }
    let store = CanvasStore::open(&path, Default::default()).unwrap();
    assert_eq!(store.info(accepted.id).unwrap(), accepted);
    let count: u32 = database.query_row("SELECT count(*) FROM canvases", [], |row| row.get(0)).unwrap();
    assert_eq!(count, if operation == "create" { 3 } else { 2 });
    command(&mut session, "retry", json!({}));
    assert_eq!(session.canvases.active.as_ref(), Some(&accepted));
    // An explicit refresh may still fail; repairing that failure must retry only
    // the read, never the already committed Create/Undo/Redo operation.
    session.execute("komaphone.canvas.list", json!({})).unwrap();
    assert!(session.flush_canvases(Duration::from_secs(10)).is_err());
    database.execute("UPDATE canvases SET hash=?2 WHERE id=?1", params![unrelated.as_str(), original_hash]).unwrap();
    command(&mut session, "retry", json!({}));
    assert_eq!(session.canvases.active.as_ref(), Some(&accepted));
    let after_count: u32 = database.query_row("SELECT count(*) FROM canvases", [], |row| row.get(0)).unwrap();
    assert_eq!(after_count, count);
    assert_eq!(store.info(accepted.id).unwrap(), accepted);
    assert!(store.info(unrelated).is_ok());
}

#[test]
fn committed_create_is_not_repeated_when_unrelated_gallery_refresh_fails() {
    committed_receipt_survives_refresh_failure("create");
}
#[test]
fn committed_undo_is_not_repeated_when_unrelated_gallery_refresh_fails() {
    committed_receipt_survives_refresh_failure("undo");
}
#[test]
fn committed_redo_is_not_repeated_when_unrelated_gallery_refresh_fails() {
    committed_receipt_survives_refresh_failure("redo");
}

#[test]
fn selecting_or_creating_during_flight_cancels_before_the_next_tick() {
    for operation in ["open", "create"] {
        let directory = tempfile::tempdir().unwrap();
        let mut session = session(&directory.path().join("canvases.sqlite3"));
        let first = create(&mut session, "First");
        command(&mut session, "bookmark", json!({"name":"Start"}));
        let second = create(&mut session, "Second");
        command(&mut session, "open", json!({"canvas":first}));
        let center = coordinates::center(session.canvases.camera.as_ref().unwrap()).unwrap();
        let deep = coordinates::target(first, center, 6, 1.0).unwrap();
        command(&mut session, "camera", json!({"camera":deep}));
        session.execute("komaphone.canvas.go", json!({"name":"Start","animate":true})).unwrap();
        assert!(session.canvases.flight.is_some());
        let args = if operation == "open" { json!({"canvas":second}) } else { json!({"name":"New destination"}) };
        session.execute(&format!("komaphone.canvas.{operation}"), args).unwrap();
        assert!(session.canvases.flight.is_none(), "selection acceptance must cancel flight before the worker reply");
        session.tick_canvas_flight(0.1);
        assert!(session.canvases.error.is_none());
        finish(&mut session);
        assert_ne!(session.canvases.active.as_ref().unwrap().id, first);
        assert!(session.canvases.error.is_none());
    }
}

#[test]
fn canvas_queries_and_mutations_preserve_a_finite_floating_selection() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = session(&directory.path().join("canvases.sqlite3"));
    let canvas = create(&mut session, "Independent canvas");
    session.execute("file.new", json!({"width":80,"height":60,"background":"transparent"})).unwrap();
    let layer = session.active().unwrap().active_layer.unwrap();
    session
        .edit("Synthetic finite drawing", |doc, _| {
            doc.layer_mut(layer).unwrap().surface_mut().unwrap().fill_rect(photocraft_geom::Rect::new(10, 10, 30, 30), &[1.0, 0.0, 0.0, 1.0]);
            Ok(())
        })
        .unwrap();
    session.execute("select.rect", json!({"x":10,"y":10,"width":20,"height":20})).unwrap();
    session.execute("select.float", json!({"dx":15,"dy":5})).unwrap();
    let before = session.active().unwrap();
    let document = before.doc.clone();
    let revision = before.revision;
    let history = before.history.past_len();
    let floating = photocraft_engine::float_cmds::floating(before).unwrap().clone();
    let assert_finite_unchanged = |session: &Session| {
        let current = session.active().unwrap();
        assert!(std::sync::Arc::ptr_eq(&current.doc, &document), "canvas command changed the finite document snapshot");
        assert_eq!(current.revision, revision);
        assert_eq!(current.history.past_len(), history);
        let current_float = photocraft_engine::float_cmds::floating(current).expect("canvas command committed the finite floating selection");
        assert_eq!(current_float.offset, floating.offset);
        assert_eq!(current_float.layer, floating.layer);
        assert_eq!(current_float.revision, floating.revision);
        assert!(std::sync::Arc::ptr_eq(&current_float.parts, &floating.parts));
    };
    // Even status previously reached float_cmds::before_command and committed the
    // moved finite pixels before this independent canvas's command body ran.
    for (id, params) in [("status", json!({})), ("list", json!({})), ("rename", json!({"canvas":canvas,"name":"Still independent"}))] {
        command(&mut session, id, params);
        assert_finite_unchanged(&session);
    }
    draw(&mut session);
    assert_finite_unchanged(&session);
    command(&mut session, "undo", json!({}));
    assert_finite_unchanged(&session);
    command(&mut session, "redo", json!({}));
    assert_finite_unchanged(&session);
    let second = create(&mut session, "Second independent canvas");
    assert_ne!(canvas, second);
    assert_finite_unchanged(&session);
    command(&mut session, "open", json!({"canvas":canvas}));
    assert_finite_unchanged(&session);
}
