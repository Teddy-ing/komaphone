use super::*;
use egui::{Color32, RawInput, ViewportId, pos2, vec2};
use photocraft_engine::{
    scene::{
        shared::{DeviceId, WorldState},
        world_protocol::CanvasSummary,
    },
    worlds::{Canvas, Viewer},
};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::mpsc::{self, Receiver},
};

const CANVAS: &str = "11111111111111111111111111111111";
const OWNER: &str = "22222222222222222222222222222222";
const SENDER: &str = "33333333333333333333333333333333";
const STROKE: &str = "44444444444444444444444444444444";

fn fixture() -> (PhotocraftApp, Context) {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    let canvas = CanvasId::new(CANVAS).unwrap();
    let owner = OwnerId::new(OWNER).unwrap();
    let world = Arc::new(WorldState::new(canvas, owner, "Probe fixture".into()).unwrap());
    app.ui.world.open = true;
    app.session.worlds.account =
        Some(Account { endpoint: "http://127.0.0.1:43127".into(), profile: owner, device: DeviceId::new("55555555555555555555555555555555").unwrap() });
    app.session.worlds.active = Some(Canvas {
        info: CanvasSummary { canvas, owner, name: "Probe fixture".into(), initial_name: "Probe fixture".into(), revision: 0, epoch: 1, import_origin: None },
        canonical: world.clone(),
        display: world,
        pending: 0,
        viewer: Viewer { camera: scene::coordinates::home(canvas), bookmarks: vec![], review: None },
        notice: None,
    });
    let ctx = Context::default();
    ctx.run_ui(raw(), |_| {}).textures_delta.clear();
    (app, ctx)
}
fn raw() -> RawInput {
    let mut input = RawInput { screen_rect: Some(Rect::from_min_size(pos2(0., 0.), vec2(100., 100.))), ..Default::default() };
    let viewport = input.viewports.get_mut(&ViewportId::ROOT).unwrap();
    viewport.inner_rect = input.screen_rect;
    viewport.native_pixels_per_point = Some(1.);
    viewport.minimized = Some(false);
    input
}
fn arm_value() -> Value {
    json!({"action":"arm","canvas":CANVAS,"sender":SENDER,"x":50,"y":50,"excludedStrokes":[]})
}
fn request(app: &mut PhotocraftApp, ctx: &Context, value: Value) -> (Outcome, Receiver<Value>) {
    let (tx, rx) = mpsc::channel();
    (control(app, ctx, &value, tx), rx)
}
fn admitted(app: &mut PhotocraftApp, ctx: &Context) -> Receiver<Value> {
    let (outcome, rx) = request(app, ctx, arm_value());
    assert!(matches!(outcome, Outcome::Deferred));
    rx
}
fn token(app: &PhotocraftApp) -> u64 {
    app.world_probe.probe.as_ref().unwrap().token
}
fn stroke() -> SharedStroke {
    SharedStroke {
        stroke: scene::Stroke {
            id: StrokeId::new(STROKE).unwrap(),
            canvas: CanvasId::new(CANVAS).unwrap(),
            owner: OwnerId::new(SENDER).unwrap(),
            revision: 0,
            anchor: Default::default(),
            points: vec![[-12., 0.], [12., 0.]],
            width: 3.,
            color: scene::Srgba { rgba: [0., 0., 0., 1.] },
            erase: false,
        },
        device: DeviceId::new("66666666666666666666666666666666").unwrap(),
        provenance: None,
        visible: true,
        created_revision: 0,
        changed_revision: 0,
    }
}
fn callback(app: &mut PhotocraftApp, ctx: &Context, preview: Option<SharedStroke>) -> egui::ScreenshotCallback {
    let probe = app.world_probe.probe.as_mut().unwrap();
    assert!(probe.flight.is_none());
    let (slot, callback) = CaptureSlot::new(ctx, ViewportId::ROOT, MAX_IMAGE_BYTES);
    probe.flight = Some(Flight { slot, witness: Witness { frame: app.frame, preview }, epoch: probe.epoch });
    callback
}
fn image(dark: bool) -> Arc<ColorImage> {
    let mut image = ColorImage::filled([100, 100], Color32::WHITE);
    if dark {
        for x in 48..=52 {
            image.pixels[50 * 100 + x] = Color32::BLACK;
        }
    }
    Arc::new(image)
}
fn armed(app: &mut PhotocraftApp, ctx: &Context) -> Value {
    let rx = admitted(app, ctx);
    callback(app, ctx, None).complete(image(false));
    drain(app, ctx);
    let result = rx.try_recv().unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["result"]["armed"], true);
    result["result"].clone()
}
fn failure(outcome: Outcome) {
    let Outcome::Done(reply) = outcome else { panic!("expected refusal") };
    assert_eq!(reply["ok"], false);
}

#[test]
fn malformed_oversized_and_wrong_scope_requests_have_no_effect() {
    let (mut app, ctx) = fixture();
    for value in [
        Value::Null,
        json!([]),
        json!({"action":"oops"}),
        json!({"action":"wait","token":-1}),
        json!({"action":"cancel","token":1.5}),
        json!({"action":"save","token":1,"path":7}),
    ] {
        failure(request(&mut app, &ctx, value).0);
        assert!(!app.world_probe.active());
    }
    for (key, value) in [
        ("x", json!("1")),
        ("y", json!(-1)),
        ("x", json!(1e10)),
        ("sender", json!(OWNER)),
        ("canvas", json!("77777777777777777777777777777777")),
        ("unknown", json!(true)),
        ("excludedStrokes", json!(vec![STROKE; 257])),
    ] {
        let mut arm = arm_value();
        arm[key] = value;
        failure(request(&mut app, &ctx, arm).0);
        assert!(!app.world_probe.active());
    }
    app.session.worlds.account = None;
    failure(request(&mut app, &ctx, arm_value()).0);
}

#[test]
fn baseline_requires_actual_blank_pixels_and_replies_once() {
    let (mut app, ctx) = fixture();
    let rx = admitted(&mut app, &ctx);
    assert!(rx.try_recv().is_err());
    callback(&mut app, &ctx, None).complete(image(false));
    drain(&mut app, &ctx);
    let reply = rx.try_recv().unwrap();
    assert_eq!(reply["result"]["baseline"]["pixels"].as_array().unwrap().len(), 25);
    assert_eq!(reply["result"]["baseline"]["darkPixels"], 0);
    assert!(rx.try_recv().is_err());
    assert_eq!(reply["result"]["baseline"]["matched"], false);
    assert!(app.world_probe.probe.as_ref().unwrap().image.is_none());
    app.world_probe.cancel("test");
    let rx = admitted(&mut app, &ctx);
    callback(&mut app, &ctx, None).complete(image(true));
    drain(&mut app, &ctx);
    assert_eq!(rx.try_recv().unwrap()["ok"], false);
    assert!(!app.world_probe.active());
}

#[test]
fn metadata_without_pixels_does_not_match_and_issued_witness_is_retained() {
    let (mut app, ctx) = fixture();
    let baseline = armed(&mut app, &ctx);
    let id = token(&app);
    let (outcome, rx) = request(&mut app, &ctx, json!({"action":"wait","token":id}));
    assert!(matches!(outcome, Outcome::Deferred));
    app.frame = 8;
    callback(&mut app, &ctx, Some(stroke())).complete(image(false));
    drain(&mut app, &ctx);
    assert!(rx.try_recv().is_err());
    let callback = callback(&mut app, &ctx, Some(stroke()));
    app.frame = 9;
    app.world_probe.probe.as_mut().unwrap().rendered = None;
    callback.complete(image(true));
    drain(&mut app, &ctx);
    let reply = rx.try_recv().unwrap();
    let result = &reply["result"];
    assert_eq!(result["frame"], 8);
    assert_eq!(result["preview"]["id"], STROKE);
    assert_eq!(result["darkPixels"], 5);
    assert_eq!(result["scope"], baseline["baseline"]["scope"]);
    assert_eq!(result["geometryEpoch"], baseline["baseline"]["geometryEpoch"]);
    assert_eq!(app.world_probe.probe.as_ref().unwrap().image.as_ref().unwrap().pixels[5050], Color32::BLACK);
    assert!(rx.try_recv().is_err());
}

#[test]
fn cancellation_timeout_and_late_callbacks_release_the_single_slot() {
    for timeout in [false, true] {
        let (mut app, ctx) = fixture();
        armed(&mut app, &ctx);
        let id = token(&app);
        let (_, rx) = request(&mut app, &ctx, json!({"action":"wait","token":id}));
        failure(request(&mut app, &ctx, json!({"action":"wait","token":id})).0);
        failure(request(&mut app, &ctx, arm_value()).0);
        let late = callback(&mut app, &ctx, Some(stroke()));
        if timeout {
            app.world_probe.probe.as_mut().unwrap().deadline = Instant::now() - Duration::from_secs(1);
            drain(&mut app, &ctx);
        } else {
            let _ = request(&mut app, &ctx, json!({"action":"cancel","token":id}));
        }
        assert_eq!(rx.try_recv().unwrap()["ok"], false);
        assert!(!app.world_probe.active());
        let image = image(true);
        let weak = Arc::downgrade(&image);
        late.complete(image);
        assert!(weak.upgrade().is_none());
        drain(&mut app, &ctx);
        assert!(rx.try_recv().is_err());
    }
}

#[test]
fn every_subject_change_and_geometry_aba_invalidates_inflight_pixels() {
    for change in 0..7 {
        let (mut app, ctx) = fixture();
        armed(&mut app, &ctx);
        let id = token(&app);
        let (_, rx) = request(&mut app, &ctx, json!({"action":"wait","token":id}));
        let late = callback(&mut app, &ctx, Some(stroke()));
        match change {
            0 => app.session.worlds.activation += 2,
            1 => app.session.worlds.account.as_mut().unwrap().endpoint.push_str("/changed"),
            2 => app.session.worlds.active.as_mut().unwrap().info.epoch += 1,
            3 => app.session.worlds.active.as_mut().unwrap().viewer.camera.zoom = 2.,
            4 => app.ui.world.open = false,
            5 => {
                let active = app.session.worlds.active.as_mut().unwrap();
                Arc::make_mut(&mut active.canonical).revision += 1;
            }
            _ => {
                let subject = Subject::current(&app);
                let original = CaptureGeometry::current(&ctx, None);
                let mut resized = original;
                resized.logical_size.x += 1.;
                app.world_probe.observe(subject.clone(), resized, Instant::now());
                app.world_probe.observe(subject, original, Instant::now());
            }
        }
        observe(&mut app, &ctx);
        late.complete(image(true));
        drain(&mut app, &ctx);
        assert_eq!(rx.try_recv().unwrap()["ok"], false);
        assert!(!app.world_probe.active());
    }
}

#[test]
fn malformed_image_fails_without_a_match() {
    let (mut app, ctx) = fixture();
    let rx = admitted(&mut app, &ctx);
    callback(&mut app, &ctx, None).complete(Arc::new(ColorImage::filled([101, 100], Color32::WHITE)));
    drain(&mut app, &ctx);
    assert_eq!(rx.try_recv().unwrap()["ok"], false);
    assert!(!app.world_probe.active());
}

#[test]
fn exact_matched_image_is_encoded_only_by_post_timing_save() {
    let (mut app, ctx) = fixture();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let encoded = calls.clone();
    let written = calls.clone();
    app.services.encode_png = Some(Box::new(move |_, _, rgba| {
        encoded.borrow_mut().push(("encode".to_owned(), rgba.to_vec()));
        Ok(vec![1, 2, 3])
    }));
    app.services.automation_write = Some(Box::new(move |path, bytes| {
        written.borrow_mut().push((path.to_owned(), bytes.to_vec()));
        Ok(())
    }));
    armed(&mut app, &ctx);
    let id = token(&app);
    failure(request(&mut app, &ctx, json!({"action":"save","token":id,"path":"probe.png"})).0);
    let (_, rx) = request(&mut app, &ctx, json!({"action":"wait","token":id}));
    callback(&mut app, &ctx, Some(stroke())).complete(image(true));
    drain(&mut app, &ctx);
    assert_eq!(rx.try_recv().unwrap()["result"]["matched"], true);
    assert!(calls.borrow().is_empty());
    let (outcome, _) = request(&mut app, &ctx, json!({"action":"save","token":id,"path":"probe.png"}));
    let Outcome::Done(reply) = outcome else { panic!("save replies immediately") };
    assert_eq!(reply["ok"], true);
    assert_eq!(calls.borrow().len(), 2);
    assert_eq!(&calls.borrow()[0].1[5050 * 4..5050 * 4 + 4], &[0, 0, 0, 255]);
    assert!(!app.world_probe.active());
}

#[test]
fn rendered_candidate_must_intersect_and_not_be_another_preview_or_label() {
    let (mut app, ctx) = fixture();
    armed(&mut app, &ctx);
    let view = Viewport { camera: scene::coordinates::home(CanvasId::new(CANVAS).unwrap()), width: 100, height: 100 };
    let mut candidate = stroke();
    let paint = |app: &mut PhotocraftApp, previews: &[SharedStroke]| {
        ctx.run_ui(raw(), |ui| rendered(app, ui, Rect::from_min_size(pos2(0., 0.), vec2(100., 100.)), &view, previews)).textures_delta.clear();
    };
    paint(&mut app, &[candidate.clone()]);
    assert!(app.world_probe.probe.as_ref().unwrap().rendered.as_ref().unwrap().preview.is_some());
    app.world_probe.obscured(Rect::from_center_size(pos2(50., 50.), vec2(8., 8.)));
    assert!(app.world_probe.probe.as_ref().unwrap().rendered.is_none());
    candidate.stroke.points = vec![[20., 20.], [30., 20.]];
    paint(&mut app, &[candidate.clone()]);
    assert!(app.world_probe.probe.as_ref().unwrap().rendered.as_ref().unwrap().preview.is_none());
    let mut interference = stroke();
    interference.stroke.owner = OwnerId::new(OWNER).unwrap();
    paint(&mut app, &[stroke(), interference.clone()]);
    assert!(app.world_probe.probe.as_ref().unwrap().rendered.is_none());
    interference.stroke.points = vec![[0., 10.]];
    interference.stroke.width = 30.;
    paint(&mut app, &[stroke(), interference.clone()]);
    assert!(app.world_probe.probe.as_ref().unwrap().rendered.is_none());
    interference.stroke.points = vec![[-12., 10.], [12., 10.]];
    paint(&mut app, &[stroke(), interference]);
    assert!(app.world_probe.probe.as_ref().unwrap().rendered.is_none());
    paint(&mut app, &[stroke()]);
    assert!(app.world_probe.probe.as_ref().unwrap().rendered.is_some());
    app.world_probe.begin_pass();
    assert!(app.world_probe.probe.as_ref().unwrap().rendered.is_none(), "a second UI pass at the same frame cannot reuse evidence");
}

#[test]
fn real_issuance_waits_for_settling_and_enforces_one_slot_and_capture_bound() {
    let (mut app, ctx) = fixture();
    let _rx = admitted(&mut app, &ctx);
    let mut found = None;
    for pass in 1..=3 {
        app.frame = pass;
        app.world_probe.probe.as_mut().unwrap().rendered = Some(Witness { frame: pass, preview: None });
        let mut output = ctx.run_ui(raw(), |ui| issue(&mut app, ui.ctx()));
        let callback = output.viewport_output.values().flat_map(|v| &v.commands).find_map(|command| match command {
            egui::ViewportCommand::Screenshot(callback) => Some(callback.clone()),
            _ => None,
        });
        output.textures_delta.clear();
        if pass < 3 {
            assert!(callback.is_none());
        } else {
            found = callback;
        }
    }
    assert!(found.is_some());
    assert_eq!(app.world_probe.probe.as_ref().unwrap().issued, 1);
    issue(&mut app, &ctx);
    assert_eq!(app.world_probe.probe.as_ref().unwrap().issued, 1);
    found.unwrap().complete(image(false));
    drain(&mut app, &ctx);
    let id = token(&app);
    let (_, rx) = request(&mut app, &ctx, json!({"action":"wait","token":id}));
    let probe = app.world_probe.probe.as_mut().unwrap();
    probe.issued = MAX_CAPTURES;
    probe.rendered = Some(Witness { frame: app.frame, preview: Some(stroke()) });
    issue(&mut app, &ctx);
    assert_eq!(rx.try_recv().unwrap()["ok"], false);
    assert!(!app.world_probe.active());
}

#[test]
fn ordinary_screenshot_reservation_prevents_probe_admission() {
    let (mut app, ctx) = fixture();
    let (tx, _) = mpsc::channel();
    app.captures.note_geometry(CaptureGeometry::current(&ctx, None));
    app.captures.admit(999, Some("ordinary.png".into()), tx, None, 0., Instant::now()).unwrap();
    failure(request(&mut app, &ctx, arm_value()).0);
    assert!(!app.world_probe.active());
}

#[test]
fn hidden_paint_is_requested_only_for_unissued_baseline_or_armed_probe() {
    let (mut app, ctx) = fixture();
    let requests_paint = |app: &mut PhotocraftApp| {
        let mut output = ctx.run_ui(raw(), |ui| drain(app, ui.ctx()));
        let requested = output
            .viewport_output
            .values()
            .flat_map(|viewport| &viewport.commands)
            .any(|command| matches!(command, egui::ViewportCommand::RequestPaintWhileHidden));
        output.textures_delta.clear();
        requested
    };
    assert!(!requests_paint(&mut app));
    let rx = admitted(&mut app, &ctx);
    assert!(requests_paint(&mut app), "baseline settling needs real UI passes while hidden");
    let baseline = callback(&mut app, &ctx, None);
    assert!(!requests_paint(&mut app), "the issued screenshot owns its paint until readback completes");
    baseline.complete(image(false));
    drain(&mut app, &ctx);
    assert_eq!(rx.try_recv().unwrap()["result"]["armed"], true);
    assert!(requests_paint(&mut app), "an armed observer must draw incoming previews while hidden");
    callback(&mut app, &ctx, Some(stroke())).complete(image(true));
    drain(&mut app, &ctx);
    assert!(!requests_paint(&mut app), "a retained match must not keep painting");
    app.world_probe.cancel("done");
    assert!(!requests_paint(&mut app));
}
