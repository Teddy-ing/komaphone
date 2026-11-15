use super::*;
use crate::komaphone_world::{CanvasId, Sense, TourStop, WorldUi, can_retry_unfinished};
use photocraft_engine::worlds::{Account, Canvas, Viewer};
use scene::{
    OwnerId,
    shared::{DeviceId, WorldState},
    world_protocol::CanvasSummary,
};
use std::sync::Arc;

fn fixture() -> (PhotocraftApp, egui::Context, Viewport) {
    let canvas = CanvasId::new("11111111111111111111111111111111").unwrap();
    let owner = OwnerId::new("22222222222222222222222222222222").unwrap();
    let world = Arc::new(WorldState::new(canvas, owner, "Input fixture".into()).unwrap());
    let camera = coordinates::home(canvas);
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.session.worlds.account =
        Some(Account { endpoint: "http://127.0.0.1:41234".into(), profile: owner, device: DeviceId::new("33333333333333333333333333333333").unwrap() });
    app.session.worlds.active = Some(Canvas {
        info: CanvasSummary { canvas, owner, name: world.name.clone(), initial_name: world.name.clone(), revision: 0, epoch: 1, import_origin: None },
        canonical: world.clone(),
        display: world,
        pending: 0,
        viewer: Viewer { camera: camera.clone(), bookmarks: vec![], review: None },
        notice: None,
    });
    (app, egui::Context::default(), Viewport { camera, width: 800, height: 600 })
}
fn frame(app: &mut PhotocraftApp, ctx: &egui::Context, view: &Viewport, time: f64, events: Vec<egui::Event>) -> Rect {
    let mut rect = Rect::NOTHING;
    ctx.run_ui(
        egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))), time: Some(time), events, ..Default::default() },
        |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let (area, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
                rect = area;
                pointer(app, ui, area, &response, view);
            });
        },
    )
    .textures_delta
    .clear();
    rect
}
fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
}

#[test]
fn world_pointer_keeps_all_measured_points_and_release_when_save_is_refused() {
    let (mut app, ctx, view) = fixture();
    let rect = frame(&mut app, &ctx, &view, 0.0, vec![]);
    let a = rect.min + egui::vec2(40.0, 40.0);
    let b = a + egui::vec2(20.0, 30.0);
    let c = a + egui::vec2(50.0, -10.0);
    let d = a + egui::vec2(80.0, 15.0);
    frame(
        &mut app,
        &ctx,
        &view,
        0.1,
        vec![egui::Event::PointerMoved(a), button(a, true), egui::Event::PointerMoved(b), egui::Event::PointerMoved(c), button(d, false)],
    );
    let draft = app.ui.world.draft.as_ref().unwrap();
    assert!(!draft.drawing && !draft.submitted);
    assert_eq!(draft.stroke.points.len(), 4);
    for (actual, position) in draft.stroke.points.iter().zip([a, b, c, d]) {
        let expected = coordinates::screen_to_point(&view, [f64::from(position.x - rect.left()), f64::from(position.y - rect.top())])
            .and_then(|point| coordinates::local_at(&draft.stroke.anchor, point))
            .unwrap();
        assert_eq!(*actual, expected);
    }
    assert!(app.ui.world.message.is_some());
    assert!(app.ui.world.unfinished());
}
#[test]
fn world_pointer_account_switch_retains_original_prefix_and_never_finishes_as_new_owner() {
    let (mut app, ctx, view) = fixture();
    let rect = frame(&mut app, &ctx, &view, 0.0, vec![]);
    let a = rect.center();
    frame(&mut app, &ctx, &view, 0.1, vec![egui::Event::PointerMoved(a), button(a, true)]);
    let before = app.ui.world.draft.as_ref().unwrap().clone();
    app.session.worlds.account.as_mut().unwrap().profile = OwnerId::new("44444444444444444444444444444444").unwrap();
    frame(&mut app, &ctx, &view, 0.2, vec![egui::Event::PointerMoved(a + egui::vec2(50.0, 0.0)), button(a + egui::vec2(60.0, 0.0), false)]);
    let after = app.ui.world.draft.as_ref().unwrap();
    assert_eq!(after.stroke.points, before.stroke.points);
    assert_eq!(after.account, before.account);
    assert!(!after.drawing && !after.submitted);
    assert!(!can_retry_unfinished(&app));
}
#[test]
fn world_pointer_focus_loss_retains_unsaved_prefix_and_blocks_new_stroke() {
    let (mut app, ctx, view) = fixture();
    let rect = frame(&mut app, &ctx, &view, 0.0, vec![]);
    let a = rect.center();
    frame(
        &mut app,
        &ctx,
        &view,
        0.1,
        vec![egui::Event::PointerMoved(a), button(a, true), egui::Event::PointerMoved(a + egui::vec2(25.0, 25.0)), egui::Event::WindowFocused(false)],
    );
    let before = app.ui.world.draft.as_ref().unwrap().clone();
    assert_eq!(before.stroke.points.len(), 2);
    assert!(!before.drawing);
    frame(&mut app, &ctx, &view, 0.2, vec![button(a + egui::vec2(50.0, 50.0), true)]);
    assert_eq!(app.ui.world.draft.as_ref().unwrap(), &before);
}

#[test]
fn world_scope_switch_clears_private_fields_but_preserves_unfinished_art() {
    let (mut app, ctx, view) = fixture();
    super::super::prune(&mut app, &ctx);
    let rect = frame(&mut app, &ctx, &view, 0.0, vec![]);
    let point = rect.center();
    frame(&mut app, &ctx, &view, 0.1, vec![egui::Event::PointerMoved(point), button(point, true)]);
    let draft = app.ui.world.draft.as_ref().unwrap().clone();
    app.ui.world.tour_name = "Private tour title".into();
    app.ui.world.rename = "Private canvas title".into();
    app.ui.world.export_path = "private/path.png".into();
    app.ui.world.bookmark = "Private destination".into();
    app.ui.world.tour_stops.push(TourStop { camera: view.camera.clone(), travel_ms: 0, pause_ms: 1000, target: None });
    let serialized = serde_json::to_string(&app.ui.world).unwrap();
    for private in ["Private", "private/path", view.camera.canvas.as_str()] {
        assert!(!serialized.contains(private), "unscoped preferences retained {private}");
    }
    let restored: WorldUi = serde_json::from_str(&serialized).unwrap();
    assert!(restored.tour_stops.is_empty() && restored.export_path.is_empty());
    app.session.worlds.account.as_mut().unwrap().profile = OwnerId::new("44444444444444444444444444444444").unwrap();
    super::super::prune(&mut app, &ctx);
    assert_eq!(app.ui.world.tour_name, WorldUi::default().tour_name);
    assert!(app.ui.world.rename.is_empty() && app.ui.world.export_path.is_empty() && app.ui.world.tour_stops.is_empty());
    assert_eq!(app.ui.world.bookmark, WorldUi::default().bookmark);
    let retained = app.ui.world.draft.as_ref().unwrap();
    assert_eq!(retained.account, draft.account);
    assert_eq!(retained.stroke.points, draft.stroke.points);
    assert!(!retained.drawing && !retained.submitted);
    assert!(app.ui.world.message.as_ref().unwrap().contains("unfinished stroke"));
}

#[test]
fn same_stroke_identity_on_another_canvas_never_consumes_the_retained_original_draft() {
    fn sync_frame(app: &mut PhotocraftApp, ctx: &egui::Context, time: f64) {
        ctx.run_ui(egui::RawInput { time: Some(time), ..Default::default() }, |ui| super::super::sync(app, ui)).textures_delta.clear();
    }
    fn accepted_canvas(canvas: CanvasId, draft: &Draft) -> Canvas {
        use scene::shared::{AcceptedOp, Edit, OpId, WorldOp};
        let mut world = WorldState::new(canvas, draft.account.profile, "Accepted artwork".into()).unwrap();
        world
            .apply(&AcceptedOp {
                revision: 1,
                op: WorldOp {
                    id: OpId::new("55555555555555555555555555555555").unwrap(),
                    canvas,
                    author: draft.account.profile,
                    device: draft.account.device,
                    sequence: 1,
                    authority_epoch: 1,
                    action: Edit::AddStroke {
                        stroke: scene::Stroke {
                            id: draft.id,
                            canvas,
                            owner: draft.account.profile,
                            revision: 0,
                            anchor: draft.stroke.anchor,
                            points: draft.stroke.points.clone(),
                            width: draft.stroke.width,
                            color: draft.stroke.color,
                            erase: draft.stroke.erase,
                        },
                    },
                },
            })
            .unwrap();
        // Exercise a real accepted/reopened scene, not a manually inserted ID.
        // The same stroke ID is valid independently in both canvas namespaces.
        let world: WorldState = serde_json::from_slice(&serde_json::to_vec(&world).unwrap()).unwrap();
        world.validate().unwrap();
        let world = Arc::new(world);
        Canvas {
            info: CanvasSummary {
                canvas,
                owner: draft.account.profile,
                name: world.name.clone(),
                initial_name: world.name.clone(),
                revision: 1,
                epoch: 1,
                import_origin: None,
            },
            canonical: Arc::clone(&world),
            display: world,
            pending: 0,
            viewer: Viewer { camera: coordinates::home(canvas), bookmarks: vec![], review: None },
            notice: None,
        }
    }

    let (mut app, ctx, view) = fixture();
    let rect = frame(&mut app, &ctx, &view, 0.0, vec![]);
    let first = rect.center();
    frame(&mut app, &ctx, &view, 0.1, vec![egui::Event::PointerMoved(first), button(first, true), button(first + egui::vec2(30.0, 20.0), false)]);
    let original = app.ui.world.draft.as_ref().unwrap().clone();
    assert!(!original.drawing && !original.submitted);
    assert_eq!(original.stroke.points.len(), 2);
    let a = original.stroke.viewport.camera.canvas;
    let b = CanvasId::new("44444444444444444444444444444444").unwrap();
    assert_eq!(app.session.worlds.account.as_ref(), Some(&original.account));

    app.session.worlds.durable_strokes.insert((b, original.id));
    sync_frame(&mut app, &ctx, 0.2);
    assert_eq!(app.ui.world.draft.as_ref(), Some(&original), "Canvas B's durable receipt must retain every field of canvas A's original draft");
    app.session.worlds.durable_strokes.insert((a, original.id));
    sync_frame(&mut app, &ctx, 0.3);
    assert!(app.ui.world.draft.is_none(), "The matching canvas A receipt completes the retained draft");

    app.session.worlds.durable_strokes.clear();
    app.ui.world.draft = Some(original.clone());
    app.session.worlds.active = Some(accepted_canvas(b, &original));
    sync_frame(&mut app, &ctx, 0.4);
    assert_eq!(app.ui.world.draft.as_ref(), Some(&original), "Canvas B's accepted display must not consume canvas A's draft with the same ID");
    // A mismatched summary cannot lend its canvas identity to a foreign display.
    app.session.worlds.active.as_mut().unwrap().info.canvas = a;
    sync_frame(&mut app, &ctx, 0.5);
    assert_eq!(app.ui.world.draft.as_ref(), Some(&original));
    app.session.worlds.active = Some(accepted_canvas(a, &original));
    sync_frame(&mut app, &ctx, 0.6);
    assert!(app.ui.world.draft.is_none(), "Only canvas A's matching accepted display completes the retained draft");
}
