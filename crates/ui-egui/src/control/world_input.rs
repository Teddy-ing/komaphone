//! Bounded screen input through real egui frames for the selected shared canvas.
//! An account, endpoint, device, grant or activation change cancels queued events.
use super::{Outcome, PhotocraftApp, params::err};
use serde_json::Value;

#[derive(Clone, PartialEq, Eq)]
struct Target {
    account: photocraft_engine::worlds::Account,
    canvas: photocraft_engine::scene::CanvasId,
    epoch: u64,
    activation: u64,
}
fn target(app: &PhotocraftApp) -> Option<Target> {
    if !app.ui.world.open {
        return None;
    }
    Some(Target {
        account: app.session.worlds.account.clone()?,
        canvas: app.session.worlds.active.as_ref()?.info.canvas,
        epoch: app.session.worlds.active.as_ref()?.info.epoch,
        activation: app.session.worlds.activation,
    })
}
fn key() -> egui::Id {
    egui::Id::unique("world-input-target")
}
pub(super) fn validate(app: &PhotocraftApp, ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp::<Target>(key())).is_none_or(|queued| target(app) == Some(queued))
}
pub(super) fn clear(ctx: &egui::Context) {
    ctx.data_mut(|data| data.remove::<Target>(key()));
}

pub(super) fn input(app: &mut PhotocraftApp, ctx: &egui::Context, params: &Value) -> Outcome {
    let events = match super::canvas_input::parse(params) {
        Ok(events) => events,
        Err(error) => return err(error),
    };
    let Some(target) = target(app) else { return err("Open an authenticated shared canvas before sending world input") };
    let pending = app.synthetic.len().saturating_add(app.synthetic_batch.as_ref().map_or(0, Vec::len));
    if pending.saturating_add(events.len()) > super::canvas_input::MAX_QUEUED_EVENTS {
        return err("World input queue is full; wait for the previous input to finish");
    }
    if pending > 0 && !super::validate_canvas_input(app, ctx) {
        return err("Canvas or account changed while input was queued; wait for cancellation");
    }
    ctx.data_mut(|data| data.insert_temp(key(), target));
    app.synthetic.extend(events);
    ctx.request_repaint();
    Outcome::AfterInput
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_engine::scene::{
        CanvasId, OwnerId, coordinates,
        shared::{DeviceId, WorldState},
        world_protocol::CanvasSummary,
    };
    use photocraft_engine::worlds::{Account, Canvas, Viewer};
    use serde_json::json;
    use std::sync::Arc;

    fn app() -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let canvas = CanvasId::new("11111111111111111111111111111111").unwrap();
        let owner = OwnerId::new("22222222222222222222222222222222").unwrap();
        let world = Arc::new(WorldState::new(canvas, owner, "Synthetic world".into()).unwrap());
        app.ui.world.open = true;
        app.session.worlds.account =
            Some(Account { endpoint: "http://127.0.0.1:43127".into(), profile: owner, device: DeviceId::new("33333333333333333333333333333333").unwrap() });
        app.session.worlds.active = Some(Canvas {
            info: CanvasSummary {
                canvas,
                owner,
                name: "Synthetic world".into(),
                initial_name: "Synthetic world".into(),
                revision: 0,
                epoch: 1,
                import_origin: None,
            },
            canonical: world.clone(),
            display: world,
            pending: 0,
            viewer: Viewer { camera: coordinates::home(canvas), bookmarks: vec![], review: None },
            notice: None,
        });
        app
    }
    fn down() -> Value {
        json!({"kind":"down","x":20,"y":30})
    }
    fn refused(outcome: Outcome) {
        let Outcome::Done(reply) = outcome else { panic!("invalid world input was queued") };
        assert_eq!(reply["ok"], false);
    }

    #[test]
    fn malformed_world_input_never_partially_enqueues() {
        let mut app = app();
        let ctx = egui::Context::default();
        for params in [
            json!(false),
            json!({}),
            json!({"kind":"down","x":20}),
            json!({"kind":"down","x":20,"y":30,"path":"not-an-input-field"}),
            json!({"kind":"move","x":1e300,"y":30}),
            json!({"kind":"move","x":20,"y":30,"button":"left"}),
            json!({"kind":"wheel","x":20,"y":30,"deltaY":4097}),
            json!({"kind":"wheel","x":20,"y":30,"unit":"page"}),
        ] {
            refused(input(&mut app, &ctx, &params));
            assert!(app.synthetic.is_empty() && app.synthetic_batch.is_none());
        }
    }

    #[test]
    fn world_input_requires_current_workspace_account_and_canvas() {
        for missing in 0..3 {
            let mut app = app();
            match missing {
                0 => app.ui.world.open = false,
                1 => app.session.worlds.account = None,
                _ => app.session.worlds.active = None,
            }
            refused(input(&mut app, &egui::Context::default(), &down()));
            assert!(app.synthetic.is_empty());
        }
    }

    #[test]
    fn every_world_identity_boundary_cancels_before_first_injection() {
        for changed in 0..7 {
            let mut app = app();
            let ctx = egui::Context::default();
            assert!(matches!(input(&mut app, &ctx, &down()), Outcome::AfterInput));
            match changed {
                0 => app.session.worlds.activation += 2,
                1 => app.session.worlds.account.as_mut().unwrap().profile = OwnerId::new("44444444444444444444444444444444").unwrap(),
                2 => app.session.worlds.account.as_mut().unwrap().device = DeviceId::new("44444444444444444444444444444444").unwrap(),
                3 => app.session.worlds.account.as_mut().unwrap().endpoint = "http://127.0.0.1:43128".into(),
                4 => app.session.worlds.active.as_mut().unwrap().info.canvas = CanvasId::new("44444444444444444444444444444444").unwrap(),
                5 => app.session.worlds.active.as_mut().unwrap().info.epoch += 1,
                _ => app.ui.world.open = false,
            }
            assert!(!validate(&app, &ctx));
            let count = app.synthetic.len();
            refused(input(&mut app, &ctx, &json!({"kind":"up","x":20,"y":30})));
            assert_eq!(app.synthetic.len(), count);
            let (reply, result) = std::sync::mpsc::channel();
            app.input_waiters.push(reply);
            let mut raw = egui::RawInput::default();
            app.inject_synthetic(&ctx, &mut raw);
            assert!(raw.events.is_empty());
            assert!(app.synthetic.is_empty() && app.synthetic_batch.is_none());
            assert_eq!(result.try_recv().unwrap()["ok"], false);
            assert!(app.session.worlds.active.as_ref().unwrap().display.strokes.is_empty());
        }
    }

    #[test]
    fn account_switch_clears_an_already_delivered_button_down_and_stale_release() {
        let mut app = app();
        let ctx = egui::Context::default();
        assert!(matches!(input(&mut app, &ctx, &down()), Outcome::AfterInput));
        let mut raw = egui::RawInput::default();
        app.inject_synthetic(&ctx, &mut raw);
        ctx.run_ui(raw, |ui| {
            app.begin_synthetic_pass(ui.ctx());
            assert!(ui.input(|input| input.pointer.primary_down()));
            app.finish_synthetic_pass();
        })
        .textures_delta
        .clear();
        assert!(matches!(input(&mut app, &ctx, &json!({"kind":"up","x":20,"y":30})), Outcome::AfterInput));
        app.session.worlds.account = None;
        let mut raw = egui::RawInput::default();
        app.inject_synthetic(&ctx, &mut raw);
        assert!(raw.events.is_empty());
        assert!(!ctx.input(|input| input.pointer.any_down()));
        assert!(app.canvas_input_cancelled);
    }

    #[test]
    fn queue_budget_counts_inflight_events_and_preserves_previous_input() {
        let mut app = app();
        let ctx = egui::Context::default();
        app.synthetic = vec![egui::Event::PointerGone; super::super::canvas_input::MAX_QUEUED_EVENTS - 1];
        app.synthetic_batch = Some(vec![egui::Event::PointerGone]);
        let prior = app.synthetic.clone();
        refused(input(&mut app, &ctx, &down()));
        assert_eq!(app.synthetic, prior);
        assert_eq!(app.synthetic_batch, Some(vec![egui::Event::PointerGone]));
    }

    #[test]
    fn inspection_reports_canonical_scene_without_invitation_capabilities() {
        let mut app = app();
        let private = "capability-that-must-never-enter-ordinary-inspection";
        app.ui.world.accept_code = private.into();
        app.session.worlds.invitation = Some(json!({"secret":private}));
        let inspection = super::super::inspect::inspect(&app, &egui::Context::default());
        assert_eq!(inspection["world"]["open"], true);
        assert_eq!(inspection["worldScene"]["revision"], 0);
        assert_eq!(inspection["worldScene"]["sampleLimit"], 128);
        assert_eq!(inspection["worldScene"]["canonicalHash"], app.session.worlds.active.as_ref().unwrap().canonical.canonical_hash().unwrap());
        assert!(!serde_json::to_string(&inspection).unwrap().contains(private));
    }
}
