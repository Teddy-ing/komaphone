use std::time::{Duration, Instant};

use super::*;

#[allow(clippy::unwrap_used)]
fn local_app(database: std::path::PathBuf) -> PhotocraftApp {
    let mut session = photocraft_engine::Session::new();
    session.enable_local_projects(database, Default::default()).unwrap();
    let mut app = PhotocraftApp::new(session, crate::Services::default());
    app.run("file.new", json!({"width":16,"height":16})).unwrap();
    app.run("layer.new.layer", json!({"name":"Kept locally"})).unwrap();
    app.sync_views();
    app
}

#[allow(clippy::unwrap_used)]
fn flush(app: &mut PhotocraftApp) {
    app.session.flush_local_projects(Duration::from_secs(10)).unwrap();
}

fn show_frame(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let mut output = ctx.run_ui(Default::default(), |ui| show(app, ui.ctx()));
    output.textures_delta.clear();
}

#[test]
fn acknowledged_local_drawing_closes_without_an_external_save_dialog() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = local_app(directory.path().join("drafts.sqlite3"));
    flush(&mut app);
    let document = app.session.active().unwrap().doc.id;
    let project = app.session.local_projects.status(document).project.unwrap();
    assert!(app.session.active().unwrap().is_dirty(), "external-file dirty state remains separate");
    assert!(app.session.local_document_saved(document));
    assert!(!app.has_unsaved_work());
    let ctx = egui::Context::default();
    crate::menus::invoke(&mut app, &ctx, "file.close", json!({})).unwrap();
    assert!(app.discard.is_none());
    assert!(app.session.documents().is_empty());
    assert!(!app.file_dialog_open());
    app.session.open_local_project(project).unwrap();
    flush(&mut app);
    assert_eq!(app.session.active().unwrap().doc.layer_count(), 2, "closing never removes the durable draft");
}

#[test]
fn pending_local_write_keeps_the_window_open_then_closes_after_acknowledgment() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = local_app(directory.path().join("drafts.sqlite3"));
    let ctx = egui::Context::default();
    assert!(intercept(&mut app, EXIT, &Value::Null));
    show_frame(&mut app, &ctx);
    assert!(!app.allow_close);
    assert!(app.discard.is_some());
    assert!(!app.file_dialog_open());
    assert_eq!(inspect(&app).unwrap()["fields"]["localSave"], true);
    flush(&mut app);
    show_frame(&mut app, &ctx);
    assert!(app.allow_close, "the exact local save ACK releases the parked close");
    assert!(app.discard.is_none());
}

#[test]
fn newly_created_clean_document_still_waits_for_its_first_local_save() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = photocraft_engine::Session::new();
    session.enable_local_projects(directory.path().join("drafts.sqlite3"), Default::default()).unwrap();
    let mut app = PhotocraftApp::new(session, crate::Services::default());
    app.run("file.new", json!({"width":8,"height":8})).unwrap();
    assert!(!app.session.active().unwrap().is_dirty());
    assert!(intercept(&mut app, "file.close", &Value::Null));
    assert!(app.has_unsaved_work(), "an unacknowledged new drawing is not durable yet");
}

#[test]
fn local_failure_remains_visible_and_retry_can_finish_the_parked_close() {
    let directory = tempfile::tempdir().unwrap();
    let blocked_parent = directory.path().join("blocked-parent");
    std::fs::write(&blocked_parent, b"a file cannot contain the database").unwrap();
    let mut app = local_app(blocked_parent.join("drafts.sqlite3"));
    let ctx = egui::Context::default();
    assert!(intercept(&mut app, EXIT, &Value::Null));
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.session.local_projects.error.is_none() {
        app.session.poll_local_projects();
        assert!(Instant::now() < deadline, "the real filesystem failure should reach the UI");
        std::thread::sleep(Duration::from_millis(5));
    }
    show_frame(&mut app, &ctx);
    assert!(!app.allow_close);
    assert!(inspect(&app).unwrap()["fields"]["localStatus"].as_str().unwrap().starts_with("Save failed:"));
    assert_eq!(inspect(&app).unwrap()["fields"]["default"], "cancel");
    std::fs::remove_file(&blocked_parent).unwrap();
    std::fs::create_dir(&blocked_parent).unwrap();
    answer_control(&mut app, &ctx, "save").unwrap();
    assert!(app.discard.is_some(), "Retry starts an asynchronous save; it does not claim success");
    assert!(!app.file_dialog_open(), "local Retry never requests an external file path");
    flush(&mut app);
    show_frame(&mut app, &ctx);
    assert!(app.allow_close);
}

#[test]
fn cancel_and_explicit_close_without_waiting_are_distinct_actions() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = local_app(directory.path().join("drafts.sqlite3"));
    let ctx = egui::Context::default();
    assert!(intercept(&mut app, EXIT, &Value::Null));
    answer_control(&mut app, &ctx, "cancel").unwrap();
    assert!(app.discard.is_none());
    assert!(!app.allow_close);
    assert_eq!(app.session.documents().len(), 1);
    assert!(intercept(&mut app, EXIT, &Value::Null));
    answer_control(&mut app, &ctx, "dontSave").unwrap();
    assert!(app.allow_close, "only this explicit answer releases an unacknowledged close");
}

#[test]
fn acknowledged_external_files_keep_the_existing_save_confirmation() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = local_app(directory.path().join("drafts.sqlite3"));
    app.session.active_mut().unwrap().path = Some("drawing.pcraft".into());
    flush(&mut app);
    assert!(app.session.local_document_saved(app.session.active().unwrap().doc.id));
    assert!(intercept(&mut app, "file.close", &Value::Null));
    assert_eq!(inspect(&app).unwrap()["fields"]["localSave"], false);
    assert_eq!(inspect(&app).unwrap()["fields"]["default"], "save");
}

#[test]
fn an_active_type_preview_cannot_be_certified_by_a_previous_save() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = local_app(directory.path().join("drafts.sqlite3"));
    flush(&mut app);
    let doc = app.session.active().unwrap().doc.id;
    app.ui.text_edit = Some(crate::state::TextEdit {
        layer: 1,
        document: Some(doc.0),
        caret: 0,
        anchor: 0,
        session: "close-guard-test".into(),
        created: false,
        mask: false,
        dragging: false,
        resize: None,
        preedit: None,
    });
    assert!(app.session.local_document_saved(doc));
    assert!(intercept(&mut app, EXIT, &Value::Null));
    let ctx = egui::Context::default();
    show_frame(&mut app, &ctx);
    assert!(!app.allow_close);
    assert!(app.discard.is_some());
    assert_eq!(crate::komaphone_workspace::local_save_label(&app), "Drawing · Unsaved");
    assert_eq!(inspect(&app).unwrap()["fields"]["localStatus"], "Drawing · Unsaved", "automation sees the same unacknowledged preview as the close modal");
    answer_control(&mut app, &ctx, "cancel").unwrap();
    assert!(app.ui.text_edit.is_some(), "Keep drawing leaves the active edit available");
}

#[test]
fn excluded_advanced_document_uses_external_save_instead_of_waiting_for_local_ack() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = local_app(directory.path().join("drafts.sqlite3"));
    app.run("file.new", json!({"width":9000,"height":1})).unwrap();
    app.run("layer.new.layer", json!({})).unwrap();
    let document = app.session.active().unwrap().doc.id;
    assert!(app.session.local_replay.excluded_reason(document).is_some());
    assert!(crate::komaphone_workspace::local_save_label(&app).starts_with("External document · use File > Save:"));
    assert!(intercept(&mut app, "file.close", &Value::Null));
    assert_eq!(inspect(&app).unwrap()["fields"]["localSave"], false);
}
