//! Shared-world drawing survives close requests until local durability or an
//! explicit decision tied to this exact draft and queue state.
use crate::{PhotocraftApp, komaphone_world, theme::Tokens};
use photocraft_engine::scene::CanvasId;
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Approval {
    drawing: komaphone_world::DrawingSnapshot,
    account: Option<photocraft_engine::worlds::Account>,
    canvas: Option<CanvasId>,
    revision: Option<u64>,
    viewer: Option<Value>,
    activation: u64,
    pending: Vec<u64>,
}
impl Approval {
    fn capture(app: &PhotocraftApp) -> Self {
        Self {
            drawing: app.ui.world.drawing_snapshot(),
            account: app.session.worlds.account.clone(),
            canvas: app.session.worlds.active.as_ref().map(|a| a.info.canvas),
            revision: app.session.worlds.active.as_ref().map(|a| a.display.revision),
            viewer: app.session.worlds.active.as_ref().and_then(|a| serde_json::to_value(&a.viewer).ok()),
            activation: app.session.worlds.activation,
            pending: app.session.worlds.pending_local_sequences(),
        }
    }
    fn permits(&self, app: &PhotocraftApp) -> bool {
        let current = Self::capture(app);
        self.drawing == current.drawing
            && self.account == current.account
            && self.canvas == current.canvas
            && self.revision == current.revision
            && self.viewer == current.viewer
            && self.activation == current.activation
            && current.pending.iter().all(|sequence| self.pending.contains(sequence))
    }
}
fn pending(app: &PhotocraftApp) -> bool {
    app.ui.world.unfinished() || app.session.worlds.busy() || app.session.worlds.view_needs_save()
}
pub(crate) fn has_unsaved_work(app: &PhotocraftApp) -> bool {
    pending(app) && app.ui.world.close_approval.as_ref().is_none_or(|approval| !approval.permits(app))
}
pub(crate) fn intercept(app: &mut PhotocraftApp) -> bool {
    if !pending(app) {
        app.ui.world.close_prompt = false;
        app.ui.world.close_approval = None;
        return false;
    }
    if app.window_lost {
        log::warn!("quitting after device loss with unacknowledged shared-canvas work");
        app.ui.world.close_prompt = false;
        return false;
    }
    if !has_unsaved_work(app) {
        return false;
    }
    app.ui.world.prepare_close();
    app.session.worlds.tour_playback = None;
    if app.session.worlds.active.as_ref().is_some_and(|a| a.viewer.review.as_ref().and_then(|r| r.interval.as_ref()).is_some_and(|i| i.playing))
        && let Err(error) = app.run("komaphone.world.review.control", json!({"state":"pause"}))
    {
        app.ui.world.message = Some(error);
    }
    if app.session.worlds.view_needs_save()
        && let Err(error) = app.session.world_save_view()
    {
        app.ui.world.message = Some(error.to_string());
    }
    if !has_unsaved_work(app) {
        return false;
    }
    app.ui.world.close_approval = None;
    app.ui.world.close_prompt = true;
    app.allow_close = false;
    true
}
fn keep(app: &mut PhotocraftApp, ctx: &egui::Context) {
    app.ui.world.close_prompt = false;
    app.ui.world.close_approval = None;
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
    app.ui.world.close_approval = Some(Approval::capture(app));
    app.ui.world.close_prompt = false;
    app.allow_close = false;
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
}
pub(crate) fn show(app: &mut PhotocraftApp, ctx: &egui::Context) -> bool {
    if app.discard.is_some() || app.ui.canvas.close_prompt || app.file_dialog_open() || app.save_options_open() || app.saving() || app.printing() {
        app.ui.world.close_approval = None;
        return false;
    }
    if !app.ui.world.close_prompt || app.window_lost {
        return false;
    }
    let mut keep_open = false;
    let mut close = false;
    let mut retry = false;
    let can_retry = komaphone_world::can_retry_unfinished(app) || app.session.worlds.view_needs_save();
    let modal = egui::Modal::new(egui::Id::unique("world-close-prompt")).show(ctx, |ui| {
        ui.set_max_width(440.);
        ui.heading("Shared canvas work is not saved yet");
        ui.label(if app.ui.world.unfinished() {
            "The current stroke has not received a local save acknowledgment. Keep drawing or retry saving it before closing."
        } else if pending(app) {
            "Local saving is still pending. Closing now may lose edits that have not reached your local recovery queue."
        } else {
            "The local save has finished. Changes waiting for the service remain in your durable queue and can sync after reopening."
        });
        if let Some(message) = app.ui.world.message.as_ref().or(app.session.worlds.error.as_ref()) {
            ui.colored_label(Tokens::get(ctx).danger, message);
        }
        ui.add_space(10.);
        ui.horizontal_wrapped(|ui| {
            let response = ui.button("Keep drawing");
            crate::komaphone_workspace::remember(ui, "world.close.keep", &response);
            keep_open = response.clicked();
            let response = ui.add_enabled(can_retry, egui::Button::new("Retry saving"));
            crate::komaphone_workspace::remember(ui, "world.close.retry", &response);
            retry = response.clicked();
            let response = ui.button(if pending(app) { "Close without saving" } else { "Close" });
            crate::komaphone_workspace::remember(ui, "world.close.discard", &response);
            close = response.clicked();
        });
    });
    if keep_open || modal.should_close() {
        keep(app, ctx);
    } else if close {
        discard(app, ctx);
    } else if retry {
        komaphone_world::retry_unfinished(app);
        if app.session.worlds.view_needs_save()
            && let Err(error) = app.session.world_save_view()
        {
            app.ui.world.message = Some(error.to_string());
        }
        app.ui.world.close_approval = None;
    }
    true
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;
    use photocraft_engine::scene::{
        self, OwnerId,
        shared::{DeviceId, WorldState},
        world_protocol::CanvasSummary,
    };
    use std::sync::Arc;
    fn fixture() -> (tempfile::TempDir, PhotocraftApp) {
        let dir = tempfile::tempdir().unwrap();
        let mut session = photocraft_engine::Session::new();
        session.enable_worlds(dir.path().join("world.sqlite3"), Default::default()).unwrap();
        let canvas = CanvasId::new(&format!("{:032x}", 1)).unwrap();
        let profile = OwnerId::new(&format!("{:032x}", 2)).unwrap();
        session.worlds.account = Some(photocraft_engine::worlds::Account {
            endpoint: "https://localhost:43195".into(),
            profile,
            device: DeviceId::new(&format!("{:032x}", 3)).unwrap(),
        });
        let world = Arc::new(WorldState::new(canvas, profile, "Close fixture".into()).unwrap());
        session.worlds.active = Some(photocraft_engine::worlds::Canvas {
            info: CanvasSummary {
                canvas,
                owner: profile,
                name: world.name.clone(),
                initial_name: world.name.clone(),
                revision: 0,
                epoch: 1,
                import_origin: None,
            },
            canonical: world.clone(),
            display: world,
            pending: 0,
            viewer: photocraft_engine::worlds::Viewer { camera: scene::coordinates::home(canvas), bookmarks: vec![], review: None },
            notice: None,
        });
        // The configured queue has not acknowledged this save. Do not poll it.
        session.world_save_view().unwrap();
        (dir, PhotocraftApp::new(session, crate::Services::default()))
    }
    #[test]
    fn finite_close_approval_cannot_bypass_unacknowledged_world_save() {
        let (_dir, mut app) = fixture();
        let ctx = egui::Context::default();
        app.allow_close = true;
        assert!(intercept(&mut app));
        assert!(!app.allow_close);
        assert!(app.ui.world.close_prompt);
        let before = app.ui.world.drawing_snapshot();
        keep(&mut app, &ctx);
        assert!(!app.ui.world.close_prompt);
        assert_eq!(app.ui.world.drawing_snapshot(), before);
        assert!(has_unsaved_work(&app));
        assert!(app.session.worlds.busy());
    }
    #[test]
    fn explicit_discard_only_covers_the_captured_queue_and_account() {
        let (_dir, mut app) = fixture();
        let ctx = egui::Context::default();
        assert!(intercept(&mut app));
        discard(&mut app, &ctx);
        assert!(!has_unsaved_work(&app));
        app.session.worlds.active.as_mut().unwrap().viewer.camera.zoom = 2.0;
        app.session.world_save_view().unwrap();
        assert!(has_unsaved_work(&app));
        assert!(intercept(&mut app));
        discard(&mut app, &ctx);
        app.session.worlds.activation += 1;
        assert!(has_unsaved_work(&app));
        let restored: komaphone_world::WorldUi = serde_json::from_value(serde_json::to_value(&app.ui.world).unwrap()).unwrap();
        assert!(!restored.close_prompt && restored.close_approval.is_none());
    }
    #[test]
    fn lost_device_fallback_never_claims_pending_world_work_was_saved() {
        let (_dir, mut app) = fixture();
        app.window_lost = true;
        assert!(!intercept(&mut app));
        assert!(app.session.worlds.busy());
        assert!(has_unsaved_work(&app));
    }
    #[test]
    fn identical_view_checkpoints_coalesce_but_changed_camera_requires_an_ack() {
        let (_dir, mut app) = fixture();
        let pending = app.session.worlds.pending_local_sequences();
        assert_eq!(pending.len(), 1);
        for _ in 0..10 {
            app.session.world_save_view().unwrap();
        }
        assert_eq!(app.session.worlds.pending_local_sequences(), pending);
        assert!(!app.session.worlds.view_needs_save());
        app.session.worlds.active.as_mut().unwrap().viewer.camera.zoom = 2.0;
        assert!(app.session.worlds.view_needs_save());
        assert!(intercept(&mut app));
        assert_eq!(app.session.worlds.pending_local_sequences().len(), 2);
        assert!(!app.session.worlds.view_needs_save());
        assert!(has_unsaved_work(&app));
    }
}
