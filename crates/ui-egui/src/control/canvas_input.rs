//! Screen-space input for the continuous canvas. Each request is one event, so a
//! down/move/up sequence traverses actual egui frames rather than the finite editor's
//! document-coordinate tool shortcut. Synthetic-input authorization remains unchanged.

use super::params::{BUTTONS, KEY_MODIFIERS, err, input_choice, input_modifiers, input_number, input_object, modifiers_from};
use super::{Outcome, PhotocraftApp};
use serde_json::Value;

const MAX_COORDINATE: f64 = 1_000_000.0;
const MAX_WHEEL_DELTA: f64 = 4096.0;
pub(super) const MAX_QUEUED_EVENTS: usize = 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Target {
    canvas: photocraft_engine::scene::CanvasId,
    activation: u64,
}

fn target(app: &PhotocraftApp) -> Option<Target> {
    if !app.ui.canvas.open {
        return None;
    }
    app.session.canvases.active.as_ref().map(|canvas| Target { canvas: canvas.id, activation: app.session.canvases.activation })
}

fn target_key() -> egui::Id {
    egui::Id::unique("canvas-input-target")
}

/// Check before the host injects queued events. Opening another canvas (including
/// switching away and back) invalidates input queued for the earlier selection.
pub(super) fn validate(app: &PhotocraftApp, ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp::<Target>(target_key())).is_none_or(|queued| target(app) == Some(queued))
}

/// Clear after the synthetic queue settles or is cancelled.
pub(super) fn clear(ctx: &egui::Context) {
    ctx.data_mut(|data| data.remove::<Target>(target_key()));
}

/// Queue only after validating the complete request. The dispatcher owns whether the
/// continuous-canvas workspace is active; this adapter never invokes an engine edit.
pub(super) fn input(app: &mut PhotocraftApp, ctx: &egui::Context, params: &Value) -> Outcome {
    let events = match parse(params) {
        Ok(events) => events,
        Err(error) => return err(error),
    };
    let Some(target) = target(app) else { return err("Open a canvas before sending canvas input") };
    let pending = app.synthetic.len().saturating_add(app.synthetic_batch.as_ref().map_or(0, Vec::len));
    if pending.saturating_add(events.len()) > MAX_QUEUED_EVENTS {
        return err("Canvas input queue is full; wait for the previous input to finish");
    }
    if pending > 0 && !validate(app, ctx) {
        return err("Canvas changed while input was queued; wait for cancellation before sending more input");
    }
    ctx.data_mut(|data| data.insert_temp(target_key(), target));
    app.synthetic.extend(events);
    ctx.request_repaint();
    Outcome::AfterInput
}

pub(super) fn parse(params: &Value) -> Result<Vec<egui::Event>, String> {
    let params = input_object(params, None, &["kind", "x", "y", "button", "deltaX", "deltaY", "unit", "modifiers", "shift", "alt", "command", "ctrl"])?;
    let kind = input_choice(&params, "kind", "kind", &["down", "move", "up", "wheel"])?.ok_or("kind is required")?;
    let coordinate = |key: &str| -> Result<f32, String> {
        input_number(&params, key, key, Some((-MAX_COORDINATE, MAX_COORDINATE)))?.map(|value| value as f32).ok_or_else(|| format!("{key} is required"))
    };
    let position = egui::pos2(coordinate("x")?, coordinate("y")?);
    let modifiers = modifiers_from(&input_modifiers(&params, &KEY_MODIFIERS)?);
    let mut events = vec![egui::Event::PointerMoved(position)];
    if kind == "wheel" {
        if params.get("button").is_some() {
            return Err("button is unavailable for wheel input".into());
        }
        let delta =
            |key: &str| -> Result<f32, String> { Ok(input_number(&params, key, key, Some((-MAX_WHEEL_DELTA, MAX_WHEEL_DELTA)))?.unwrap_or(0.0) as f32) };
        let unit = match input_choice(&params, "unit", "unit", &["point", "line"])?.unwrap_or("point") {
            "line" => egui::MouseWheelUnit::Line,
            _ => egui::MouseWheelUnit::Point,
        };
        events.push(egui::Event::MouseWheel {
            unit,
            delta: egui::vec2(delta("deltaX")?, delta("deltaY")?),
            modifiers,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
        });
    } else {
        if let Some(key) = ["deltaX", "deltaY", "unit"].into_iter().find(|key| params.get(*key).is_some()) {
            return Err(format!("{key} is available only for wheel input"));
        }
        let button = match input_choice(&params, "button", "button", &BUTTONS)?.unwrap_or("left") {
            "right" | "secondary" => egui::PointerButton::Secondary,
            "middle" => egui::PointerButton::Middle,
            _ => egui::PointerButton::Primary,
        };
        if kind == "move" {
            if params.get("button").is_some() {
                return Err("button is unavailable for move input; the preceding down holds it".into());
            }
        } else {
            events.push(egui::Event::PointerButton { pos: position, button, pressed: kind == "down", modifiers });
        }
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn app() -> PhotocraftApp {
        use photocraft_engine::scene::{CanvasId, CanvasInfo, OwnerId, coordinates};
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let id = CanvasId::new("11111111111111111111111111111111").unwrap();
        app.ui.canvas.open = true;
        app.session.canvases.active = Some(CanvasInfo {
            id,
            owner: OwnerId::new("22222222222222222222222222222222").unwrap(),
            name: "Input fixture".into(),
            created_at: String::new(),
            updated_at: String::new(),
            revision: 0,
            camera: coordinates::home(id),
            bookmarks: Vec::new(),
            stroke_count: 0,
            undo_count: 0,
            redo_count: 0,
        });
        app
    }

    #[test]
    fn invalid_fields_never_enqueue_partial_input() {
        let mut app = app();
        let ctx = egui::Context::default();
        for params in [
            json!(false),
            json!({}),
            json!({"kind":"down","x":20}),
            json!({"kind":"down","x":20,"y":30,"path":"outside.png"}),
            json!({"kind":"drag","x":20,"y":30}),
            json!({"kind":"down","x":1e300,"y":30}),
            json!({"kind":"down","x":"20","y":30}),
            json!({"kind":"down","x":20,"y":30,"button":"extra"}),
            json!({"kind":"down","x":20,"y":30,"deltaY":1}),
            json!({"kind":"move","x":20,"y":30,"button":"left"}),
            json!({"kind":"wheel","x":20,"y":30,"button":"left"}),
            json!({"kind":"wheel","x":20,"y":30,"deltaY":4097}),
            json!({"kind":"wheel","x":20,"y":30,"unit":"page"}),
            json!({"kind":"wheel","x":20,"y":30,"modifiers":{"space":true}}),
            json!({"kind":"up","x":20,"y":30,"modifiers":{"ctrl":"yes"}}),
            json!({"kind":"up","x":20,"y":30,"shift":"yes","modifiers":{"ctrl":true}}),
        ] {
            let Outcome::Done(reply) = input(&mut app, &ctx, &params) else { panic!("Invalid input queued: {params}") };
            assert_eq!(reply["ok"], false, "{params}");
            assert!(app.synthetic.is_empty(), "{params}");
        }
    }

    #[test]
    fn down_move_up_are_screen_events_without_a_finite_document() {
        let mut app = app();
        let ctx = egui::Context::default();
        assert!(app.session.documents().is_empty());
        for (kind, expected_count) in [("down", 2), ("move", 1), ("up", 2)] {
            assert!(matches!(input(&mut app, &ctx, &json!({"kind":kind,"x":123.5,"y":234.25})), Outcome::AfterInput));
            let events = app.take_synthetic_step();
            assert_eq!(events.len(), expected_count);
            assert_eq!(events.first(), Some(&egui::Event::PointerMoved(egui::pos2(123.5, 234.25))));
            if kind != "move" {
                assert!(matches!(events.get(1), Some(egui::Event::PointerButton { pos, button, pressed, .. })
                    if *pos == egui::pos2(123.5, 234.25) && *button == egui::PointerButton::Primary && *pressed == (kind == "down")));
            }
            assert!(app.automation_input, "Existing synthetic authorization must remain active");
        }
    }

    #[test]
    fn wheel_keeps_cursor_units_and_modifiers() {
        let events = parse(&json!({"kind":"wheel","x":25,"y":40,"deltaX":-2,"deltaY":3,"unit":"line","modifiers":{"ctrl":true}})).unwrap();
        assert_eq!(events.first(), Some(&egui::Event::PointerMoved(egui::pos2(25.0, 40.0))));
        assert!(matches!(events.get(1), Some(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta, modifiers, .. })
            if *delta == egui::vec2(-2.0, 3.0) && modifiers.ctrl));
    }

    #[test]
    fn queue_limit_preserves_already_queued_events() {
        let mut app = app();
        let ctx = egui::Context::default();
        app.synthetic = vec![egui::Event::PointerGone; MAX_QUEUED_EVENTS - 1];
        app.synthetic_batch = Some(vec![egui::Event::PointerGone]);
        let before = app.synthetic.clone();
        let Outcome::Done(reply) = input(&mut app, &ctx, &json!({"kind":"move","x":0,"y":0})) else { panic!("Full queue accepted input") };
        assert_eq!(reply["ok"], false);
        assert_eq!(app.synthetic, before);
        assert_eq!(app.synthetic_batch, Some(vec![egui::Event::PointerGone]));
    }

    #[test]
    fn queued_input_binds_to_canvas_activation_before_first_injection() {
        let mut app = app();
        let ctx = egui::Context::default();
        assert!(matches!(input(&mut app, &ctx, &json!({"kind":"down","x":20,"y":30})), Outcome::AfterInput));
        assert!(validate(&app, &ctx));
        app.session.canvases.activation += 1;
        assert!(!validate(&app, &ctx), "Even an away-and-back switch invalidates queued input");
        let count = app.synthetic.len();
        let Outcome::Done(reply) = input(&mut app, &ctx, &json!({"kind":"move","x":40,"y":50})) else { panic!("Stale queue accepted new input") };
        assert_eq!(reply["ok"], false);
        assert_eq!(app.synthetic.len(), count);
        clear(&ctx);
        assert!(validate(&app, &ctx));
    }

    #[test]
    fn a_closed_workspace_rejects_input_without_enqueuing() {
        let mut app = app();
        app.ui.canvas.open = false;
        let ctx = egui::Context::default();
        let Outcome::Done(reply) = input(&mut app, &ctx, &json!({"kind":"down","x":20,"y":30})) else { panic!("Closed workspace accepted input") };
        assert_eq!(reply["ok"], false);
        assert!(app.synthetic.is_empty());
    }

    #[test]
    fn switching_away_and_back_before_the_raw_hook_clears_an_already_held_pointer() {
        let mut app = app();
        let ctx = egui::Context::default();
        assert!(matches!(input(&mut app, &ctx, &json!({"kind":"down","x":20,"y":30})), Outcome::AfterInput));
        let mut down = egui::RawInput::default();
        app.inject_synthetic(&ctx, &mut down);
        ctx.run_ui(down, |ui| {
            app.begin_synthetic_pass(ui.ctx());
            assert!(ui.input(|input| input.pointer.primary_down()));
            app.finish_synthetic_pass();
        })
        .textures_delta
        .clear();
        assert!(ctx.input(|input| input.pointer.primary_down()));
        assert!(matches!(input(&mut app, &ctx, &json!({"kind":"up","x":20,"y":30})), Outcome::AfterInput));
        let (reply, cancelled) = std::sync::mpsc::channel();
        app.input_waiters.push(reply);
        app.session.canvases.activation += 2; // A -> B -> A, same ID but another activation.
        let mut next = egui::RawInput::default();
        app.inject_synthetic(&ctx, &mut next);
        assert!(next.events.is_empty(), "Stale release must be withdrawn before injection");
        assert!(!ctx.input(|input| input.pointer.any_down()), "Cancelling the queued up also clears the previously delivered down");
        assert!(app.synthetic.is_empty() && app.synthetic_batch.is_none());
        let result = cancelled.try_recv().unwrap();
        assert_eq!(result["ok"], false);
        assert!(result["error"].as_str().unwrap().contains("canvas changed"));
        assert!(app.canvas_input_cancelled);
        let mut host = eframe::Frame::_new_kittest();
        ctx.run_ui(next, |ui| app.ui_frame(ui, &mut host)).textures_delta.clear();
        assert!(!app.canvas_input_cancelled);
    }

    #[test]
    fn a_generic_toolbar_click_injected_before_a_canvas_switch_never_toggles_the_new_canvas() {
        let mut app = app();
        let ctx = egui::Context::default();
        let raw = |time| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024.0, 768.0))),
            time: Some(time),
            ..Default::default()
        };
        ctx.run_ui(raw(0.0), |ui| crate::komaphone_canvas::show(&mut app, ui)).textures_delta.clear();
        let point = crate::komaphone_workspace::control_rect(&ctx, "canvas.tool.eraser").unwrap().center();
        let click = || {
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
                egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE },
            ]
        };
        // A generic ui.click has no canvas-input-target stamp. The host's in-flight
        // target snapshot must protect its cached egui click response as well.
        app.synthetic.extend(click());
        let mut stale = raw(0.1);
        app.inject_synthetic(&ctx, &mut stale);
        let mut host = eframe::Frame::_new_kittest();
        ctx.run_ui(stale, |ui| {
            app.session.canvases.activation += 1;
            app.ui_frame(ui, &mut host);
        })
        .textures_delta
        .clear();
        assert!(!app.ui.canvas.erase);
        ctx.run_ui(raw(0.2), |ui| crate::komaphone_canvas::show(&mut app, ui)).textures_delta.clear();
        assert!(!app.ui.canvas.erase, "The stale cached click must not leak into the following pass");

        app.synthetic.extend(click());
        let mut fresh = raw(0.3);
        app.inject_synthetic(&ctx, &mut fresh);
        ctx.run_ui(fresh, |ui| {
            assert!(!app.cancel_stale_canvas_pass(ui.ctx()));
            app.begin_synthetic_pass(ui.ctx());
            crate::komaphone_canvas::show(&mut app, ui);
            app.finish_synthetic_pass();
        })
        .textures_delta
        .clear();
        assert!(app.ui.canvas.erase, "A fresh toolbar click must still work after cancellation");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn an_injected_canvas_switch_clears_held_input_and_fresh_input_targets_the_new_canvas() {
        use std::time::Duration;
        let temp = tempfile::tempdir().unwrap();
        let mut session = photocraft_engine::Session::new();
        session.enable_canvases(temp.path().join("canvas.sqlite3"), Default::default()).unwrap();
        session.flush_canvases(Duration::from_secs(15)).unwrap();
        session.execute("komaphone.canvas.create", json!({"name":"A"})).unwrap();
        session.flush_canvases(Duration::from_secs(15)).unwrap();
        let a = session.canvases.active.as_ref().unwrap().id;
        session.execute("komaphone.canvas.create", json!({"name":"B"})).unwrap();
        session.flush_canvases(Duration::from_secs(15)).unwrap();
        let b = session.canvases.active.as_ref().unwrap().id;
        session.execute("komaphone.canvas.open", json!({"canvas":a})).unwrap();
        session.flush_canvases(Duration::from_secs(15)).unwrap();
        let mut app = PhotocraftApp::new(session, crate::Services::default());
        app.run("file.new", json!({"width":32,"height":32})).unwrap();
        let finite_revision = app.session.active().unwrap().revision;
        app.ui.canvas.open = true;
        let ctx = egui::Context::default();
        let raw = |time| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024.0, 768.0))),
            time: Some(time),
            ..Default::default()
        };
        ctx.run_ui(raw(0.0), |ui| crate::komaphone_canvas::show(&mut app, ui)).textures_delta.clear();
        let point = crate::komaphone_workspace::control_rect(&ctx, "canvas.drawing").unwrap().center();
        assert!(matches!(input(&mut app, &ctx, &json!({"kind":"down","x":point.x,"y":point.y})), Outcome::AfterInput));
        let mut injected = raw(0.1);
        app.inject_synthetic(&ctx, &mut injected);
        assert_eq!(injected.events.len(), 2);
        let mut host = eframe::Frame::_new_kittest();
        ctx.run_ui(injected, |ui| {
            assert!(ui.input(|input| input.pointer.primary_down()), "egui consumed A's raw down event");
            // Match the host race exactly: raw input has been injected/consumed,
            // then the control/worker phase selects B before the UI handles it.
            app.session.execute("komaphone.canvas.open", json!({"canvas":b})).unwrap();
            app.session.flush_canvases(Duration::from_secs(15)).unwrap();
            assert!(app.cancel_stale_canvas_pass(ui.ctx()));
            assert!(!ui.input(|input| input.pointer.any_down()));
            assert!(ui.input(|input| input.events.is_empty() && input.raw.events.is_empty()));
            assert!(app.synthetic.is_empty() && app.synthetic_batch.is_none());
            assert!(!app.automation_input);
            // The real UI entry point must skip this stale pass and release the
            // cancellation flag, so subsequent B input can run normally.
            app.ui_frame(ui, &mut host);
            assert!(!app.canvas_input_cancelled);
        })
        .textures_delta
        .clear();
        assert!(app.session.canvases.items.iter().all(|canvas| canvas.stroke_count == 0));
        assert_eq!(app.session.active().unwrap().revision, finite_revision);

        ctx.run_ui(raw(0.2), |ui| crate::komaphone_canvas::show(&mut app, ui)).textures_delta.clear();
        let point = crate::komaphone_workspace::control_rect(&ctx, "canvas.drawing").unwrap().center();
        for (index, kind) in ["down", "move", "up"].into_iter().enumerate() {
            let x = point.x + index as f32 * 20.0;
            assert!(matches!(input(&mut app, &ctx, &json!({"kind":kind,"x":x,"y":point.y})), Outcome::AfterInput));
            let mut fresh = raw(0.3 + index as f64 * 0.1);
            app.inject_synthetic(&ctx, &mut fresh);
            ctx.run_ui(fresh, |ui| {
                assert!(!app.cancel_stale_canvas_pass(ui.ctx()));
                app.begin_synthetic_pass(ui.ctx());
                crate::komaphone_canvas::show(&mut app, ui);
                app.finish_synthetic_pass();
            })
            .textures_delta
            .clear();
        }
        app.session.flush_canvases(Duration::from_secs(15)).unwrap();
        assert_eq!(app.session.canvases.items.iter().find(|canvas| canvas.id == a).unwrap().stroke_count, 0);
        assert_eq!(app.session.canvases.items.iter().find(|canvas| canvas.id == b).unwrap().stroke_count, 1);
        assert_eq!(app.session.active().unwrap().revision, finite_revision);
    }
}
