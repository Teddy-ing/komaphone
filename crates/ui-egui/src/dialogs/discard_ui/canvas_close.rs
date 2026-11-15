//! A separate close decision for local canvas work. Finite-document approval
//! never authorizes discarding an unacknowledged canvas drawing.

use crate::{PhotocraftApp, komaphone_canvas, theme::Tokens};
use photocraft_engine::scene::{Camera, StrokeId};
use serde_json::json;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Approval {
    drawing: komaphone_canvas::DrawingSnapshot,
    camera: Option<Camera>,
    activation: u64,
    pending: Vec<StrokeId>,
}

impl Approval {
    fn capture(app: &PhotocraftApp) -> Self {
        Self {
            drawing: app.ui.canvas.drawing_snapshot(),
            camera: app.session.canvases.camera.clone(),
            activation: app.session.canvases.activation,
            pending: app.session.canvases.pending_strokes.iter().map(|stroke| stroke.id).collect(),
        }
    }

    fn permits(&self, app: &PhotocraftApp) -> bool {
        self.drawing == app.ui.canvas.drawing_snapshot()
            && self.camera == app.session.canvases.camera
            && self.activation == app.session.canvases.activation
            && app.session.canvases.pending_strokes.iter().all(|stroke| self.pending.contains(&stroke.id))
    }
}

fn pending(app: &PhotocraftApp) -> bool {
    app.ui.canvas.has_uncommitted_drawing() || app.session.canvases.has_pending()
}

pub(crate) fn has_unsaved_work(app: &PhotocraftApp) -> bool {
    pending(app) && app.ui.canvas.close_approval.as_ref().is_none_or(|approval| !approval.permits(app))
}

/// Called for a window/application close, independently of finite allow_close.
pub(crate) fn intercept(app: &mut PhotocraftApp) -> bool {
    if !pending(app) {
        app.ui.canvas.close_prompt = false;
        app.ui.canvas.close_approval = None;
        return false;
    }
    if app.window_lost {
        // Match the existing finite-editor fallback: a lost window cannot draw
        // a modal. Recovery is attempted by the host, but quitting must remain
        // possible if it fails rather than trapping an invisible window.
        log::warn!("quitting a window with pending canvas work after its GPU device was lost");
        app.ui.canvas.close_prompt = false;
        return false;
    }
    app.ui.canvas.prepare_close();
    if app.session.canvases.flight.is_some()
        && let Err(error) = app.run("komaphone.canvas.flight", json!({"state":"cancel"}))
    {
        app.ui.canvas.message = Some(error);
    }
    if !has_unsaved_work(app) {
        return false;
    }
    app.ui.canvas.close_approval = None;
    app.ui.canvas.close_prompt = true;
    app.allow_close = false;
    true
}

fn keep_drawing(app: &mut PhotocraftApp, ctx: &egui::Context) {
    app.ui.canvas.close_prompt = false;
    app.ui.canvas.close_approval = None;
    app.allow_close = false;
    app.discard = None;
    app.jobs.close_after_saves = false;
    ctx.input_mut(|input| {
        input.pointer = egui::PointerState::default();
        input.events.clear();
        input.raw.events.clear();
    });
    ctx.stop_dragging();
}

fn discard(app: &mut PhotocraftApp, ctx: &egui::Context) {
    app.ui.canvas.close_approval = Some(Approval::capture(app));
    app.ui.canvas.close_prompt = false;
    // A canvas answer does not grant permission to discard finite documents.
    app.allow_close = false;
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
}

/// Returns true for the action frame too, so another close modal cannot consume
/// the same click or key after this one has just closed.
pub(crate) fn show(app: &mut PhotocraftApp, ctx: &egui::Context) -> bool {
    if app.discard.is_some() || app.file_dialog_open() || app.save_options_open() || app.saving() || app.printing() {
        // A different close/save question can hold the application open while
        // new work is accepted. Any still-pending canvas needs fresh consent
        // after that sequence, including if the finite question is cancelled.
        app.ui.canvas.close_approval = None;
    }
    if !app.ui.canvas.close_prompt || app.window_lost {
        return false;
    }
    let mut keep = false;
    let mut close = false;
    let mut retry = false;
    let has_draft = app.ui.canvas.has_uncommitted_drawing();
    let can_retry = komaphone_canvas::can_retry_retained(app) || (app.session.canvases.error.is_some() && app.session.is_enabled("komaphone.canvas.retry"));
    let modal = egui::Modal::new(egui::Id::unique("canvas-close-prompt")).show(ctx, |ui| {
        ui.set_max_width(430.0);
        ui.heading("Canvas work is not finished");
        ui.label(if has_draft {
            "An unfinished stroke has not been saved. Keep drawing to return to its canvas and retry it, or explicitly close without saving it."
        } else if pending(app) {
            "Canvas saving or loading is still pending. Keep this window open until it finishes. Closing now may discard changes that have not been saved."
        } else {
            "The pending canvas work has finished saving. You can close the window or keep drawing."
        });
        if let Some(error) = &app.session.canvases.error {
            ui.colored_label(Tokens::get(ctx).danger, error);
        }
        if let Some(message) = &app.ui.canvas.message {
            ui.label(message);
        }
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            let response = ui.button("Keep drawing");
            crate::komaphone_workspace::remember(ui, "canvas.close.keep", &response);
            keep = response.clicked();
            let response = ui.add_enabled(can_retry, egui::Button::new("Retry saving"));
            crate::komaphone_workspace::remember(ui, "canvas.close.retry", &response);
            retry = response.clicked();
            let response = ui.button(if pending(app) { "Close without saving" } else { "Close" });
            crate::komaphone_workspace::remember(ui, "canvas.close.discard", &response);
            close = response.clicked();
        });
    });
    if keep || modal.should_close() {
        keep_drawing(app, ctx);
    } else if close {
        discard(app, ctx);
    } else if retry {
        if komaphone_canvas::can_retry_retained(app) {
            komaphone_canvas::retry_retained(app);
        } else if let Err(error) = app.run("komaphone.canvas.retry", json!({})) {
            app.ui.canvas.message = Some(error);
        }
        app.ui.canvas.close_approval = None;
    }
    true
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;
    use std::time::Duration;

    fn app() -> (tempfile::TempDir, PhotocraftApp) {
        let directory = tempfile::tempdir().unwrap();
        let mut session = photocraft_engine::Session::new();
        session.enable_canvases(directory.path().join("canvases.sqlite3"), Default::default()).unwrap();
        session.flush_canvases(Duration::from_secs(15)).unwrap();
        session.execute("komaphone.canvas.create", json!({"name":"Close fixture"})).unwrap();
        session.flush_canvases(Duration::from_secs(15)).unwrap();
        let mut app = PhotocraftApp::new(session, crate::Services::default());
        app.ui.canvas.open = true;
        (directory, app)
    }

    fn unfinished(app: &mut PhotocraftApp, ctx: &egui::Context) {
        let raw = |time, events| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024.0, 768.0))),
            time: Some(time),
            events,
            ..Default::default()
        };
        ctx.run_ui(raw(0.0, vec![]), |ui| komaphone_canvas::show(app, ui)).textures_delta.clear();
        app.session.flush_canvases(Duration::from_secs(15)).unwrap();
        let point = crate::komaphone_workspace::control_rect(ctx, "canvas.drawing").unwrap().center();
        ctx.run_ui(
            raw(
                0.1,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
                ],
            ),
            |ui| komaphone_canvas::show(app, ui),
        )
        .textures_delta
        .clear();
        ctx.run_ui(raw(0.2, vec![egui::Event::PointerMoved(point + egui::vec2(30.0, 20.0))]), |ui| komaphone_canvas::show(app, ui)).textures_delta.clear();
        assert!(app.ui.canvas.has_uncommitted_drawing());
    }

    #[test]
    fn finite_allow_close_cannot_drop_active_drawing_and_keep_then_retry_saves_it() {
        let (_directory, mut app) = app();
        let ctx = egui::Context::default();
        unfinished(&mut app, &ctx);
        app.allow_close = true;
        assert!(intercept(&mut app));
        assert!(!app.allow_close);
        assert!(app.ui.canvas.close_prompt);
        let retained = app.ui.canvas.drawing_snapshot();
        assert!(intercept(&mut app));
        assert_eq!(app.ui.canvas.drawing_snapshot(), retained);
        assert_eq!(serde_json::to_value(&app.ui.canvas).unwrap()["close_prompt"], true);
        let restored: komaphone_canvas::CanvasUi = serde_json::from_value(serde_json::to_value(&app.ui.canvas).unwrap()).unwrap();
        assert!(!restored.close_prompt, "A previous process's close decision must not be restored");
        keep_drawing(&mut app, &ctx);
        assert!(!app.ui.canvas.close_prompt && !app.allow_close);
        assert!(!ctx.input(|input| input.pointer.any_down()));
        assert_eq!(app.ui.canvas.drawing_snapshot(), retained);
        assert!(komaphone_canvas::retry_retained(&mut app));
        app.session.flush_canvases(Duration::from_secs(15)).unwrap();
        assert_eq!(app.session.canvases.active.as_ref().unwrap().stroke_count, 1);
        assert!(!has_unsaved_work(&app));
        assert!(!intercept(&mut app));
    }

    #[test]
    fn a_new_accepted_nonstroke_command_revokes_discard_but_read_queries_do_not() {
        for command in ["rename", "undo"] {
            let (_directory, mut app) = app();
            let ctx = egui::Context::default();
            let camera = app.session.canvases.camera.clone().unwrap();
            app.run(
                "komaphone.canvas.stroke",
                json!({"canvas":camera.canvas,"anchor":camera.region,"points":[[0.0,0.0],[10.0,10.0]],"width":3.0,"color":{"rgba":[0.1,0.2,0.3,1.0]}}),
            )
            .unwrap();
            app.session.flush_canvases(Duration::from_secs(15)).unwrap();
            unfinished(&mut app, &ctx);
            assert!(intercept(&mut app));
            discard(&mut app, &ctx);
            assert!(!has_unsaved_work(&app));
            app.run("komaphone.canvas.status", json!({})).unwrap();
            app.run("komaphone.canvas.list", json!({})).unwrap();
            app.session.flush_canvases(Duration::from_secs(15)).unwrap();
            assert!(!has_unsaved_work(&app), "Read queries preserve this exact explicit discard decision");
            let params = if command == "rename" { json!({"canvas":camera.canvas,"name":"New unsaved intent"}) } else { json!({}) };
            app.run(&format!("komaphone.canvas.{command}"), params).unwrap();
            assert!(app.ui.canvas.close_approval.is_none(), "{command} must revoke the old decision immediately");
            assert!(intercept(&mut app));
        }
    }

    #[test]
    fn finite_exit_prompt_and_cancel_revoke_canvas_consent_before_another_ui_pass() {
        let (_directory, mut app) = app();
        let ctx = egui::Context::default();
        unfinished(&mut app, &ctx);
        app.run("file.new", json!({"width":32,"height":32})).unwrap();
        app.run("layer.new.layer", json!({"name":"Unsaved finite layer"})).unwrap();
        assert!(intercept(&mut app));
        discard(&mut app, &ctx);
        assert!(super::super::intercept(&mut app, "file.exit", &serde_json::Value::Null));
        assert!(app.discard.is_some());
        assert!(app.ui.canvas.close_approval.is_none());
        super::super::answer_control(&mut app, &ctx, "cancel").unwrap();
        assert!(app.discard.is_none());
        assert!(intercept(&mut app), "Later quit requires fresh canvas consent after finite Cancel");
    }

    #[test]
    fn failed_store_initialization_and_lost_gpu_do_not_trap_the_window() {
        let directory = tempfile::tempdir().unwrap();
        let blocked = directory.path().join("not-a-directory");
        std::fs::write(&blocked, b"owned synthetic filesystem failure").unwrap();
        let mut session = photocraft_engine::Session::new();
        session.enable_canvases(blocked.join("canvases.sqlite3"), Default::default()).unwrap();
        assert!(session.flush_canvases(Duration::from_secs(15)).is_err());
        let mut app = PhotocraftApp::new(session, crate::Services::default());
        assert!(app.session.canvases.error.is_some());
        let ctx = egui::Context::default();
        if intercept(&mut app) {
            discard(&mut app, &ctx);
            assert!(!intercept(&mut app));
        }
        let (_other_directory, mut app) = self::app();
        unfinished(&mut app, &ctx);
        app.window_lost = true;
        assert!(!intercept(&mut app), "An invisible GPU-lost modal must not prevent the existing quit fallback");
        assert!(app.ui.canvas.has_uncommitted_drawing(), "Fallback does not pretend the draft was saved");
    }
}
