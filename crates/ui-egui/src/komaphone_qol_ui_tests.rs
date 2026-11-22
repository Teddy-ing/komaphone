//! Regression and visual-capture tests for the Small UI QOL batch:
//! QOL-001 (Safe GPU Komaphone copy), QOL-003 (collapsible shared-canvas sidebar),
//! QOL-004 (bottom-left dimensions & zoom), QOL-011 (persistent Workroom navigation),
//! QOL-012 (local collaborator name labels toggle), and QOL-015 (readable local timestamps).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use egui::{Event, PointerButton, Pos2, Rect, Vec2};
use photocraft_engine::gallery::GalleryItem;
use photocraft_engine::party::gallery::{GALLERY_SCHEMA, GalleryContent, GalleryManifest};
use photocraft_engine::party::room::{Assignment, AssignmentView, MemberView, Phase, Reference, RevealedEntry, RoomView};
use photocraft_engine::party::{AssetHash, AssetRef, AssignmentId, DeviceId as PartyDeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, RoomId};
use photocraft_engine::rooms::media::RoomImage;
use photocraft_engine::scene::{
    self, Bookmark, CanvasId, OwnerId, Srgba, StrokeId, coordinates,
    shared::{DeviceId, WorldState},
    world_protocol::{CanvasSummary, Member, Peer, Person, PreviewStyle, TrashedCanvas},
};
use photocraft_engine::worlds::{Account, Canvas, Viewer};
use serde_json::json;

use crate::{PhotocraftApp, Services, komaphone_time, komaphone_workspace};

fn owner_id(seed: u8) -> OwnerId {
    OwnerId::new(&format!("{seed:02x}").repeat(16)).unwrap()
}

fn device_id(seed: u8) -> DeviceId {
    DeviceId::new(&format!("{seed:02x}").repeat(16)).unwrap()
}

fn canvas_id(seed: u8) -> CanvasId {
    CanvasId::new(&format!("{seed:02x}").repeat(16)).unwrap()
}

fn project_id(seed: u8) -> ProjectId {
    ProjectId::new(&format!("{seed:02x}").repeat(16)).unwrap()
}

fn party_participant(seed: u8, name: &str) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{seed:02x}").repeat(16)).unwrap(),
        device: PartyDeviceId::new(&format!("{seed:02x}").repeat(16)).unwrap(),
        display_name: name.to_owned(),
    }
}

fn collect_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
    fn visit(shape: &egui::Shape, out: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(text) => out.push(text.galley.text().to_owned()),
            egui::Shape::Vec(list) => {
                for item in list {
                    visit(item, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for clipped in shapes {
        visit(&clipped.shape, &mut out);
    }
    out
}

fn has_peer_cursor_circle(shapes: &[egui::epaint::ClippedShape], rect: Rect) -> bool {
    fn visit(shape: &egui::Shape, rect: Rect) -> bool {
        match shape {
            egui::Shape::Circle(circle) => rect.contains(circle.center) && (circle.radius - 5.0).abs() < 0.25,
            egui::Shape::Vec(list) => list.iter().any(|item| visit(item, rect)),
            _ => false,
        }
    }
    shapes.iter().any(|clipped| visit(&clipped.shape, rect))
}

fn run_world_frame_with_shapes(app: &mut PhotocraftApp, ctx: &egui::Context, size: Vec2, ppp: f32, time: f64, events: Vec<Event>) -> (Vec<String>, bool) {
    let mut raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), time: Some(time), events, ..Default::default() };
    raw.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(ppp);
    let mut out = ctx.run_ui(raw, |ui| crate::komaphone_world::show(app, ui));
    let texts = collect_texts(&out.shapes);
    let draw_rect = komaphone_workspace::control_rect(ctx, "world.drawing").unwrap_or(Rect::NOTHING);
    let has_cursor = has_peer_cursor_circle(&out.shapes, draw_rect);
    out.textures_delta.clear();
    (texts, has_cursor)
}

fn run_world_frame(app: &mut PhotocraftApp, ctx: &egui::Context, size: Vec2, ppp: f32, time: f64, events: Vec<Event>) -> Vec<String> {
    run_world_frame_with_shapes(app, ctx, size, ppp, time, events).0
}

fn click_world_control(app: &mut PhotocraftApp, ctx: &egui::Context, size: Vec2, ppp: f32, time: &mut f64, control: &str) -> Rect {
    *time += 0.1;
    run_world_frame(app, ctx, size, ppp, *time, vec![]);
    let rect = komaphone_workspace::control_rect(ctx, control).unwrap_or_else(|| panic!("missing control {control}"));
    assert!(Rect::from_min_size(Pos2::ZERO, size).contains_rect(rect), "control {control} clipped: {rect:?} in {size:?}");
    let pos = rect.center();
    *time += 0.1;
    run_world_frame(
        app,
        ctx,
        size,
        ppp,
        *time,
        vec![Event::PointerMoved(pos), Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Default::default() }],
    );
    *time += 0.1;
    run_world_frame(
        app,
        ctx,
        size,
        ppp,
        *time,
        vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Default::default() }],
    );
    *time += 0.1;
    run_world_frame(app, ctx, size, ppp, *time, vec![]);
    rect
}

fn world_fixture() -> PhotocraftApp {
    let me = owner_id(1);
    let my_dev = device_id(2);
    let alice = owner_id(3);
    let alice_dev = device_id(4);
    let cid = canvas_id(10);
    let second_cid = canvas_id(11);
    let world = Arc::new(WorldState::new(cid, me, "Studio Canvas".into()).unwrap());
    let home = coordinates::home(cid);
    let zoomed = coordinates::target(cid, coordinates::center(&home).unwrap(), 1, 2.5).unwrap();
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
    app.ui.komaphone.enabled = true;
    app.ui.world.open = true;
    app.session.worlds.account = Some(Account { endpoint: "https://komaphone.tedsoftware.org".into(), profile: me, device: my_dev });
    let summary = CanvasSummary {
        canvas: cid,
        owner: me,
        name: "Studio Canvas".into(),
        initial_name: "Studio Canvas".into(),
        revision: 4,
        epoch: 1,
        import_origin: None,
    };
    let second_summary = CanvasSummary {
        canvas: second_cid,
        owner: alice,
        name: "Side Sketch".into(),
        initial_name: "Side Sketch".into(),
        revision: 2,
        epoch: 1,
        import_origin: None,
    };
    app.session.worlds.items = vec![summary.clone(), second_summary];
    app.session.worlds.people = vec![Person { profile: me, display_name: "You".into() }, Person { profile: alice, display_name: "Alice".into() }];
    app.session.worlds.people.sort_by_key(|p| p.profile);
    app.session.worlds.members = vec![Member { profile: me, owner: true }, Member { profile: alice, owner: false }];
    app.session.worlds.trash = vec![TrashedCanvas {
        canvas: canvas_id(12),
        name: "Old Doodle".into(),
        revision: 9,
        deleted_at_ms: 1_795_063_414_946, // 2026-11-19T04:43:34.946Z
        owner_epoch: 1,
    }];
    app.session.worlds.active = Some(Canvas {
        info: summary,
        canonical: Arc::clone(&world),
        display: world,
        pending: 0,
        viewer: Viewer { camera: home.clone(), bookmarks: vec![Bookmark { name: "Corner Detail".into(), camera: zoomed }], review: None },
        notice: None,
    });
    let peer_cursor = coordinates::center(&home).unwrap();
    let peer_local = coordinates::local_at(&home.region, peer_cursor).unwrap();
    app.session.worlds.peers = vec![Peer {
        profile: alice,
        device: alice_dev,
        camera: home.clone(),
        preview: vec![[peer_local[0] - 80.0, peer_local[1] + 20.0], [peer_local[0] + 60.0, peer_local[1] + 20.0]],
        cursor: Some(peer_cursor),
        stroke: Some(PreviewStyle {
            id: StrokeId::new(&"aa".repeat(16)).unwrap(),
            anchor: home.region,
            width: 8.0,
            color: Srgba { rgba: [0.85, 0.15, 0.2, 1.0] },
            erase: false,
            brush: None,
            samples: None,
        }),
        expires_at_ms: u64::MAX,
    }];
    app
}

fn app_with_prefs_store(saved: Option<String>) -> (PhotocraftApp, Rc<RefCell<Option<String>>>) {
    let store = Rc::new(RefCell::new(saved));
    let reader = Rc::clone(&store);
    let writer = Rc::clone(&store);
    let services = Services {
        load_prefs: Some(Box::new(move || reader.borrow().clone())),
        save_prefs: Some(Box::new(move |text: &str| {
            *writer.borrow_mut() = Some(text.to_owned());
            Ok(())
        })),
        ..Default::default()
    };
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
    app.ui.komaphone.enabled = true;
    crate::prefs_ui::tick(&mut app, &egui::Context::default());
    (app, store)
}

// ------------------------------------------------------------------ QOL-003

#[test]
fn qol003_sidebar_collapse_expands_canvas_preserves_state_and_avoids_spurious_strokes() {
    let mut app = world_fixture();
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, app.ui.theme);
    let mut time = 0.0;
    let size = Vec2::new(1280.0, 720.0);

    run_world_frame(&mut app, &ctx, size, 1.0, time, vec![]);
    let expanded_draw_rect = komaphone_workspace::control_rect(&ctx, "world.drawing").unwrap();
    let initial_camera = app.session.worlds.active.as_ref().unwrap().viewer.camera.clone();
    let initial_hash = app.session.worlds.active.as_ref().unwrap().canonical.canonical_hash().unwrap();
    let initial_bookmarks = app.session.worlds.active.as_ref().unwrap().viewer.bookmarks.clone();
    assert!(!app.ui.world.sidebar_collapsed);
    assert!(komaphone_workspace::control_rect(&ctx, "world.name").is_some());
    assert!(komaphone_workspace::control_rect(&ctx, "world.section.restore").is_some());

    // Collapse via the top chevron button.
    let collapse_rect = click_world_control(&mut app, &ctx, size, 1.0, &mut time, "world.sidebar.toggle");
    assert!(app.ui.world.sidebar_collapsed);
    assert!(!app.ui.world.unfinished(), "clicking collapse chevron must never start a stroke");
    assert_eq!(app.session.worlds.active.as_ref().unwrap().viewer.camera, initial_camera);
    assert_eq!(app.session.worlds.active.as_ref().unwrap().canonical.canonical_hash().unwrap(), initial_hash);
    assert_eq!(app.session.worlds.active.as_ref().unwrap().viewer.bookmarks, initial_bookmarks);
    let collapsed_draw_rect = komaphone_workspace::control_rect(&ctx, "world.drawing").unwrap();
    assert!(
        collapsed_draw_rect.width() >= expanded_draw_rect.width() + 200.0,
        "collapsing sidebar must widen drawing area: expanded={:?} collapsed={:?}",
        expanded_draw_rect,
        collapsed_draw_rect
    );
    assert!(collapse_rect.right() <= collapsed_draw_rect.left() + 260.0);
    assert!(komaphone_workspace::control_rect(&ctx, "world.name").is_none());
    assert!(komaphone_workspace::control_rect(&ctx, "world.section.restore").is_none());

    // Navigate (change camera) while collapsed, then reopen and confirm contents + new camera stay intact.
    let navigated = coordinates::target(initial_camera.canvas, coordinates::center(&initial_camera).unwrap(), 1, 3.0).unwrap();
    app.session.worlds.active.as_mut().unwrap().viewer.camera = navigated.clone();
    click_world_control(&mut app, &ctx, size, 1.0, &mut time, "world.sidebar.toggle");
    assert!(!app.ui.world.sidebar_collapsed);
    assert!(!app.ui.world.unfinished(), "clicking expand chevron must never start a stroke");
    assert_eq!(app.session.worlds.active.as_ref().unwrap().viewer.camera, navigated);
    assert_eq!(app.session.worlds.active.as_ref().unwrap().viewer.bookmarks, initial_bookmarks);
    assert!(komaphone_workspace::control_rect(&ctx, "world.name").is_some());
    assert!(komaphone_workspace::control_rect(&ctx, "world.section.restore").is_some());

    // Narrow window (760x480) and High DPI (2.0x): toggle remains unclipped and interactive in both states.
    for (win, ppp) in [(Vec2::new(760.0, 480.0), 1.0), (Vec2::new(1280.0, 720.0), 2.0)] {
        click_world_control(&mut app, &ctx, win, ppp, &mut time, "world.sidebar.toggle");
        assert!(app.ui.world.sidebar_collapsed, "collapsed at {win:?} @{ppp}x");
        assert!(komaphone_workspace::control_rect(&ctx, "world.peerLabels").is_some(), "peer labels toggle stays discoverable when collapsed");
        click_world_control(&mut app, &ctx, win, ppp, &mut time, "world.sidebar.toggle");
        assert!(!app.ui.world.sidebar_collapsed, "expanded at {win:?} @{ppp}x");
    }
}

#[test]
fn qol003_and_qol012_local_view_preferences_persist_across_reopen_and_tolerate_corrupt_json() {
    let ctx = egui::Context::default();
    let (mut app, store) = app_with_prefs_store(None);
    assert!(!app.ui.world.sidebar_collapsed);
    assert!(app.ui.world.show_peer_labels);

    // Defaults keep `komaphoneView` null.
    crate::komaphone_world::persist_view_prefs(&mut app, &ctx);
    assert!(app.session.prefs().komaphone_view.is_null());

    // Flip both local view preferences and persist.
    app.ui.world.sidebar_collapsed = true;
    app.ui.world.show_peer_labels = false;
    crate::komaphone_world::persist_view_prefs(&mut app, &ctx);
    crate::prefs_ui::tick(&mut app, &ctx);
    let persisted = store.borrow().clone().expect("preferences written");
    assert!(persisted.contains("worldSidebarCollapsed"));
    assert!(persisted.contains("worldShowPeerLabels"));

    // Fresh app instance restores both preferences cleanly.
    let (reopened, _) = app_with_prefs_store(Some(persisted));
    assert!(reopened.ui.world.sidebar_collapsed);
    assert!(!reopened.ui.world.show_peer_labels);

    // Malformed `komaphoneView` JSON never panics and falls back to defaults.
    for bad in [json!("not-an-object"), json!(42), json!({"worldSidebarCollapsed": "yes", "worldShowPeerLabels": 123})] {
        let raw = json!({"komaphoneView": bad}).to_string();
        let (safe, _) = app_with_prefs_store(Some(raw));
        assert!(!safe.ui.world.sidebar_collapsed);
        assert!(safe.ui.world.show_peer_labels);
    }
}

// ------------------------------------------------------------------ QOL-012

#[test]
fn qol012_collaborator_name_labels_toggle_hides_only_labels_and_keeps_cursors_previews_and_credits() {
    let mut client_a = world_fixture();
    let mut client_b = world_fixture();
    client_b.ui.world.show_peer_labels = false;
    client_b.ui.world.sidebar_collapsed = true;

    let ctx_a = egui::Context::default();
    let ctx_b = egui::Context::default();
    PhotocraftApp::setup_context(&ctx_a, client_a.ui.theme);
    PhotocraftApp::setup_context(&ctx_b, client_b.ui.theme);
    let size = Vec2::new(1280.0, 720.0);

    // Client A (default visible): "Alice" name label AND cursor ring are drawn on canvas.
    let (texts_a, cursor_a) = run_world_frame_with_shapes(&mut client_a, &ctx_a, size, 1.0, 0.1, vec![]);
    assert!(cursor_a, "client A draws peer cursor ring");
    assert!(texts_a.iter().any(|t| t == "Alice"), "client A shows peer name label: {texts_a:?}");

    // Client B (hidden + collapsed sidebar): "Alice" name label is NOT drawn, cursor ring IS still drawn, and toggle stays visible.
    let (texts_b, cursor_b) = run_world_frame_with_shapes(&mut client_b, &ctx_b, size, 1.0, 0.1, vec![]);
    assert!(cursor_b, "client B still draws peer cursor ring when name labels are hidden");
    assert!(!texts_b.iter().any(|t| t == "Alice"), "client B hides peer name label: {texts_b:?}");
    assert!(komaphone_workspace::control_rect(&ctx_b, "world.peerLabels").is_some(), "toggle discoverable while sidebar collapsed");

    // A newly arriving second peer ("Bob") respects each client's local preference immediately.
    let bob = owner_id(5);
    let bob_dev = device_id(6);
    for app in [&mut client_a, &mut client_b] {
        app.session.worlds.people.push(Person { profile: bob, display_name: "Bob".into() });
        app.session.worlds.people.sort_by_key(|p| p.profile);
        let cam = app.session.worlds.active.as_ref().unwrap().viewer.camera.clone();
        app.session.worlds.peers.push(Peer {
            profile: bob,
            device: bob_dev,
            camera: cam.clone(),
            preview: vec![],
            cursor: Some(coordinates::center(&cam).unwrap()),
            stroke: None,
            expires_at_ms: u64::MAX,
        });
    }
    let texts_a2 = run_world_frame(&mut client_a, &ctx_a, size, 1.0, 0.2, vec![]);
    let texts_b2 = run_world_frame(&mut client_b, &ctx_b, size, 1.0, 0.2, vec![]);
    assert!(texts_a2.iter().any(|t| t == "Bob"));
    assert!(!texts_b2.iter().any(|t| t == "Bob"));

    // Authorship, members, canonical hash, and canvas switches keep preference intact.
    assert_eq!(
        client_a.session.worlds.active.as_ref().unwrap().canonical.canonical_hash().unwrap(),
        client_b.session.worlds.active.as_ref().unwrap().canonical.canonical_hash().unwrap()
    );
    // Simulate reconnect (peers cleared then repopulated) and canvas switch on client B.
    client_b.session.worlds.peers.clear();
    run_world_frame(&mut client_b, &ctx_b, size, 1.0, 0.3, vec![]);
    assert!(!client_b.ui.world.show_peer_labels, "reconnect does not reset preference");

    // Clicking the toggle on client B re-enables peer labels immediately.
    let mut t = 0.4;
    click_world_control(&mut client_b, &ctx_b, size, 1.0, &mut t, "world.peerLabels");
    assert!(client_b.ui.world.show_peer_labels);
}

// ------------------------------------------------------------------ QOL-004

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn qol004_bottom_left_dimensions_and_zoom_update_across_finite_party_and_deep_canvases() {
    use egui_kittest::kittest::Queryable;

    let mut h = egui_kittest::Harness::builder().with_size(Vec2::new(1280.0, 720.0)).with_step_dt(1.0 / 60.0).with_max_steps(64).build_eframe(|cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
        app.ui.komaphone.enabled = true;
        app.session.prefs.edit(|p| p.interface.language = "en".into());
        app.run("file.new", json!({"width": 1536, "height": 1024, "background": "white"})).unwrap();
        app.run("file.new", json!({"width": 800, "height": 600, "background": "white"})).unwrap();
        app.session.set_active(0);
        app
    });
    h.run_steps(5);

    // 1. Finite drawing shows exact dimensions and interactive zoom field on bottom-left.
    assert!(h.query_by_label("1536 px x 1024 px (72 ppi)").is_some(), "active 1536x1024 dimensions visible");
    let zoom_rect = komaphone_workspace::control_rect(&h.ctx, "workspace.zoom").expect("workspace.zoom present");
    let dim_rect = komaphone_workspace::control_rect(&h.ctx, "workspace.dimensions").expect("workspace.dimensions present");
    assert!(zoom_rect.left() < dim_rect.left(), "zoom sits left of dimensions");
    assert!(zoom_rect.bottom() > 670.0 && dim_rect.bottom() > 670.0, "controls live in bottom-left status bar");

    // 2. Typing a zoom percentage in the reused PhotoCraft zoom field updates the real active View zoom.
    h.get_by_role_and_label(egui::accesskit::Role::SpinButton, "Zoom").click();
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::A);
    h.event(Event::Text("200".into()));
    h.key_press(egui::Key::Enter);
    h.run_steps(6);
    assert!((h.state().current_zoom() - 2.0).abs() < 0.02, "zoom field controls real view zoom: {}", h.state().current_zoom());

    // 3. Switching drawings updates dimensions and zoom immediately.
    h.state_mut().session.set_active(1);
    h.run_steps(4);
    assert!(h.query_by_label("800 px x 600 px (72 ppi)").is_some(), "switching drawing updates dimensions");

    // 4. Resizing the active drawing updates dimensions immediately.
    h.state_mut().run("image.canvasSize", json!({"width": 1024, "height": 512})).unwrap();
    h.run_steps(4);
    assert!(h.query_by_label("1024 px x 512 px (72 ppi)").is_some(), "resizing updates dimensions");

    // 5. Deep-zoom Shared Canvas shows accurate scale and explicit Unbounded canvas label (never finite px or %).
    let mut world_app = world_fixture();
    let cid = world_app.session.worlds.active.as_ref().unwrap().info.canvas;
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, world_app.ui.theme);
    let texts_d0 = run_world_frame(&mut world_app, &ctx, Vec2::new(1280.0, 720.0), 1.0, 0.1, vec![]);
    assert!(texts_d0.iter().any(|t| t == "Zoom 1.00×"), "{texts_d0:?}");
    assert!(texts_d0.iter().any(|t| t == "Unbounded canvas"), "{texts_d0:?}");
    assert!(!texts_d0.iter().any(|t| t.contains("ppi")), "deep canvas must not invent finite ppi/dimensions");

    let deep_cam = coordinates::target(cid, coordinates::center(&coordinates::home(cid)).unwrap(), 6, 1.0).unwrap();
    world_app.session.worlds.active.as_mut().unwrap().viewer.camera = deep_cam;
    let texts_d6 = run_world_frame(&mut world_app, &ctx, Vec2::new(1280.0, 720.0), 1.0, 0.2, vec![]);
    assert!(texts_d6.iter().any(|t| t == "Zoom 1.0e12×"), "deep zoom retains exact scientific scale: {texts_d6:?}");
    assert!(texts_d6.iter().any(|t| t == "Unbounded canvas"));

    // 6. Narrow layout (760x480) and High DPI (2.0x): bottom-left zoom/dimensions never overlap tools or canvas.
    for (win, ppp) in [(Vec2::new(760.0, 480.0), 1.0), (Vec2::new(1000.0, 640.0), 2.0)] {
        let mut narrow = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
        narrow.ui.komaphone.enabled = true;
        narrow.run("file.new", json!({"width": 1024, "height": 512, "background": "white"})).unwrap();
        let nctx = egui::Context::default();
        PhotocraftApp::setup_context(&nctx, narrow.ui.theme);
        let mut raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, win)), time: Some(0.1), ..Default::default() };
        raw.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(ppp);
        nctx.run_ui(raw, |ui| {
            komaphone_workspace::chrome(&mut narrow, ui);
            egui::CentralPanel::default().show(ui, |ui| crate::canvas::document_area(&mut narrow, ui));
        })
        .textures_delta
        .clear();
        let z = komaphone_workspace::control_rect(&nctx, "workspace.zoom").unwrap();
        let d = komaphone_workspace::control_rect(&nctx, "workspace.dimensions").unwrap();
        let tool = komaphone_workspace::control_rect(&nctx, "tool.Brush").unwrap();
        assert!(z.right() <= d.left() + 1.0, "zoom and dimensions do not overlap at {win:?} @{ppp}x");
        assert!(tool.bottom() <= z.top() + 1.0, "tools panel does not overlap bottom status bar at {win:?} @{ppp}x");
        assert!(narrow.last_canvas_rect.bottom() <= z.top() + 1.0, "canvas does not overlap bottom status bar");
    }
}

// ------------------------------------------------------------------ QOL-011

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn qol011_workroom_button_returns_directly_from_gallery_canvases_worlds_party_and_reveal() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
    app.ui.komaphone.enabled = true;
    app.session.enable_local_projects(dir.path().join("drafts.sqlite3"), Default::default()).unwrap();
    app.run("komaphone.project.new", json!({"width": 1536, "height": 1024, "name": "Sketch A"})).unwrap();
    app.run("komaphone.project.new", json!({"width": 1024, "height": 512, "name": "Assigned Segment"})).unwrap();
    // Create an active marquee selection and paint an unsaved stroke on Document 1.
    app.run("select.rect", json!({"x": 40, "y": 40, "width": 120, "height": 80})).unwrap();
    let active_id = app.session.active().unwrap().doc.id;
    let doc_count_before = app.session.documents().len();
    assert_eq!(doc_count_before, 2);
    assert!(app.session.active().unwrap().doc.selection.is_some());

    // Set up an active Drawing phase room assignment so the reference panel is active.
    let alice = party_participant(1, "Alice");
    let room_id = RoomId::new(&"77".repeat(16)).unwrap();
    let game_id = GameId::new(&"88".repeat(16)).unwrap();
    let assign_proj = project_id(9);
    app.session.rooms.status.profile = Some(alice.clone());
    app.session.rooms.status.room = Some(RoomView {
        id: room_id,
        revision: 3,
        epoch: 1,
        phase: Phase::Drawing,
        round: 1,
        mode: Mode::ExquisiteCorpse,
        timer_ms: None,
        moderator: alice.id,
        members: vec![MemberView { participant: alice.clone(), connected: true, ready: true }],
        waiting: vec![],
        paused: None,
        deadline_ms: None,
        assignment: Some(AssignmentView {
            assignment: Assignment { id: AssignmentId { game: game_id, chain: 0, round: 1 }, player: alice.id, project: assign_proj },
            submitted: false,
            reference: Some(Reference::Missing),
        }),
        reveal: vec![],
    });

    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, app.ui.theme);
    let size = Vec2::new(1280.0, 720.0);
    let run_full = |app: &mut PhotocraftApp, time: f64, events: Vec<Event>| {
        let mut out =
            ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), time: Some(time), events, ..Default::default() }, |ui| {
                if app.ui.world.open {
                    crate::komaphone_world::show(app, ui);
                } else if app.ui.canvas.open {
                    crate::komaphone_canvas::show(app, ui);
                } else {
                    komaphone_workspace::chrome(app, ui);
                    if !app.ui.gallery.open && !crate::komaphone_game_view::has_reveal(app) {
                        crate::komaphone_game_view::reference_panel(app, ui);
                    }
                    egui::CentralPanel::default().show(ui, |ui| {
                        if app.ui.gallery.open {
                            crate::komaphone_gallery::show(app, ui);
                        } else if crate::komaphone_game_view::has_reveal(app) {
                            crate::komaphone_game_view::reveal(app, ui);
                        } else if app.session.documents().is_empty() {
                            komaphone_workspace::home(app, ui);
                        } else {
                            crate::canvas::document_area(app, ui);
                        }
                    });
                }
            });
        out.textures_delta.clear();
    };
    let mut time = 0.0;
    let mut click = |app: &mut PhotocraftApp, id: &str| {
        for _ in 0..2 {
            time += 0.1;
            run_full(app, time, vec![]);
        }
        let rect = komaphone_workspace::control_rect(&ctx, id).unwrap_or_else(|| panic!("missing {id}"));
        let pos = rect.center();
        time += 0.1;
        run_full(
            app,
            time,
            vec![Event::PointerMoved(pos), Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Default::default() }],
        );
        time += 0.1;
        run_full(app, time, vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Default::default() }]);
        time += 0.1;
        run_full(app, time, vec![]);
    };

    // 1. Gallery -> Workroom directly without cycling Canvases/Party and without creating a draft.
    click(&mut app, "workspace.gallery");
    assert!(app.ui.gallery.open);
    click(&mut app, "workspace.workroom");
    assert!(!app.ui.gallery.open);
    assert_eq!(app.session.documents().len(), doc_count_before, "Workroom must never create a new draft");
    assert_eq!(app.session.active().unwrap().doc.id, active_id);
    assert!(app.session.active().unwrap().doc.selection.is_some(), "selection preserved across Gallery roundtrip");

    // 2. Local Canvases -> Workroom.
    click(&mut app, "workspace.canvases");
    assert!(app.ui.canvas.open);
    click(&mut app, "workspace.workroom");
    assert!(!app.ui.canvas.open);
    assert_eq!(app.session.documents().len(), doc_count_before);
    assert_eq!(app.session.active().unwrap().doc.id, active_id);

    // 3. Shared Canvases -> Workroom.
    click(&mut app, "workspace.worlds");
    assert!(app.ui.world.open);
    click(&mut app, "workspace.workroom");
    assert!(!app.ui.world.open);
    assert_eq!(app.session.documents().len(), doc_count_before);
    assert_eq!(app.session.active().unwrap().doc.id, active_id);

    // 4. Party (Private room) opened over Gallery -> Workroom button in Private room closes BOTH and returns to Workroom.
    click(&mut app, "workspace.gallery");
    click(&mut app, "workspace.room");
    assert!(app.ui.gallery.open && app.ui.rooms.open);
    click(&mut app, "rooms.workroom");
    assert!(!app.ui.rooms.open && !app.ui.gallery.open);
    assert_eq!(app.session.documents().len(), doc_count_before);
    assert_eq!(app.session.active().unwrap().doc.id, active_id);

    // 5. Reveal phase -> Workroom dismisses reveal cleanly while keeping open documents intact and not unlocking submitted content.
    let room = app.session.rooms.status.room.as_mut().unwrap();
    room.phase = Phase::Reveal;
    room.reveal = vec![RevealedEntry::Missing {
        assignment: Assignment { id: AssignmentId { game: game_id, chain: 0, round: 0 }, player: alice.id, project: assign_proj },
        reason: "Fixture".into(),
    }];
    app.ui.rooms.view_reveal = true;
    assert!(crate::komaphone_game_view::has_reveal(&app));
    click(&mut app, "workspace.workroom");
    assert!(!crate::komaphone_game_view::has_reveal(&app));
    assert_eq!(app.session.active().unwrap().doc.id, active_id);

    // 6. New drawing from Gallery creates a new drawing and brings the user to Workroom.
    click(&mut app, "workspace.gallery");
    assert!(app.ui.gallery.open);
    click(&mut app, "komaphone.project.new");
    assert!(!app.ui.gallery.open, "New drawing switches to Workroom");
    assert_eq!(app.session.documents().len(), doc_count_before + 1);

    // 7. Empty-state Workroom (no open documents) renders Home with New drawing action.
    while !app.session.documents().is_empty() {
        app.run("file.close", json!({"force": true})).unwrap();
    }
    app.ui.gallery.open = true;
    click(&mut app, "workspace.workroom");
    assert!(!app.ui.gallery.open);
    assert!(komaphone_workspace::control_rect(&ctx, "home.new").is_some(), "empty Workroom offers New drawing");
}

// ------------------------------------------------------------------ QOL-015 UI integration

#[test]
fn qol015_gallery_card_and_drafts_render_viewer_local_timestamps_without_mutating_stored_values() {
    const SUPPLIED: &str = "2026-11-19T04:43:34.946Z";
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
    app.ui.komaphone.enabled = true;
    app.ui.gallery.open = true;
    let id1 = project_id(1);
    let id2 = project_id(2);
    app.session.gallery.items = vec![
        GalleryItem {
            id: id1,
            title: "Evening Cat".into(),
            kind: "creature".into(),
            authors: vec!["Alice".into(), "Bob".into()],
            updated_at: SUPPLIED.into(),
            favorite: false,
            trashed: false,
            complete: true,
            draft: false,
            revision: 1,
            state: "Saved locally".into(),
            source_projects: vec![id1],
        },
        GalleryItem {
            id: id2,
            title: "Broken Date Item".into(),
            kind: "draft".into(),
            authors: vec!["Alice".into()],
            updated_at: "not-a-timestamp".into(),
            favorite: false,
            trashed: false,
            complete: true,
            draft: true,
            revision: 1,
            state: "Saved locally".into(),
            source_projects: vec![id2],
        },
    ];
    app.session.gallery.active = Some(Arc::new(GalleryManifest {
        schema_version: GALLERY_SCHEMA,
        id: id1,
        title: "Evening Cat".into(),
        created_at: SUPPLIED.into(),
        updated_at: SUPPLIED.into(),
        authors: vec![party_participant(1, "Alice")],
        content: GalleryContent::Project {
            checkpoint: AssetRef { hash: AssetHash::new(&"11".repeat(32)).unwrap(), bytes: 64 },
            direct_assets: vec![],
            replay: None,
        },
        provenance: None,
        notes: vec![],
        fps: 6,
        looping: true,
    }));
    app.session.gallery.image = Some(Arc::new(RoomImage { width: 64, height: 64, rgba: [200, 120, 90, 255].repeat(64 * 64) }));

    let ctx = egui::Context::default();
    let render_gallery = |app: &mut PhotocraftApp| -> Vec<String> {
        let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 720.0))), ..Default::default() }, |ui| {
            egui::CentralPanel::default().show(ui, |ui| crate::komaphone_gallery::show(app, ui));
        });
        let texts = collect_texts(&out.shapes);
        out.textures_delta.clear();
        texts
    };

    // Pacific viewer (12-hour): date rolls over Nov 19 UTC -> Nov 18 PST.
    let pacific_texts = komaphone_time::with_viewer_timezone(komaphone_time::fixture_pacific_12h, || render_gallery(&mut app));
    assert!(pacific_texts.iter().any(|t| t == "Nov 18, 2026, 8:43 PM PST"), "card shows readable Pacific time: {pacific_texts:?}");
    assert!(pacific_texts.iter().any(|t| t == "Updated Nov 18, 2026, 8:43 PM PST"), "detail shows readable Pacific time");
    assert!(pacific_texts.iter().any(|t| t == komaphone_time::UNAVAILABLE_LABEL), "invalid timestamp shows honest unavailable label");
    assert!(!pacific_texts.iter().any(|t| t.contains(SUPPLIED)), "raw ISO string must not appear directly on card");

    // Tokyo viewer (24-hour): same stored item renders in JST without altering stored data.
    let tokyo_texts = komaphone_time::with_viewer_timezone(komaphone_time::fixture_tokyo_24h, || render_gallery(&mut app));
    assert!(tokyo_texts.iter().any(|t| t == "Nov 19, 2026, 13:43 JST"), "card follows Tokyo viewer: {tokyo_texts:?}");

    // Original stored timestamps remain completely unchanged.
    assert_eq!(app.session.gallery.items[0].updated_at, SUPPLIED);
    assert_eq!(app.session.gallery.active.as_ref().unwrap().updated_at, SUPPLIED);

    // Local drafts window and Shared-canvas Trash reuse the exact same formatter.
    app.ui.gallery.open = false;
    app.ui.komaphone.drafts_open = true;
    app.session.local_projects.drafts.push(photocraft_engine::local_projects::DraftInfo {
        project: id1,
        title: "Night Sketch".into(),
        revision: 7,
        updated_at: SUPPLIED.into(),
        replay_elapsed_ms: 0,
    });
    let drafts_texts = komaphone_time::with_viewer_timezone(komaphone_time::fixture_pacific_12h, || {
        let mut texts = Vec::new();
        for pass in 0..3 {
            let mut out = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 720.0))),
                    time: Some(pass as f64 * 0.1),
                    ..Default::default()
                },
                |ui| {
                    komaphone_workspace::chrome(&mut app, ui);
                },
            );
            texts = collect_texts(&out.shapes);
            out.textures_delta.clear();
        }
        texts
    });
    assert!(drafts_texts.iter().any(|t| t == "Revision 7 · Nov 18, 2026, 8:43 PM PST"), "drafts window formats local time: {drafts_texts:?}");
    assert_eq!(app.session.local_projects.drafts[0].updated_at, SUPPLIED);

    // Local canvases sidebar formats `CanvasInfo.updated_at` via the same formatter.
    let mut canvas_app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
    canvas_app.ui.canvas.open = true;
    let cid = canvas_id(20);
    canvas_app.session.canvases.items.push(scene::CanvasInfo {
        id: cid,
        owner: owner_id(1),
        name: "Field Notes".into(),
        created_at: SUPPLIED.into(),
        updated_at: SUPPLIED.into(),
        revision: 3,
        camera: coordinates::home(cid),
        bookmarks: vec![],
        stroke_count: 3,
        undo_count: 0,
        redo_count: 0,
    });
    let canvas_texts = komaphone_time::with_viewer_timezone(komaphone_time::fixture_pacific_12h, || {
        let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 720.0))), ..Default::default() }, |ui| {
            crate::komaphone_canvas::show(&mut canvas_app, ui);
        });
        let texts = collect_texts(&out.shapes);
        out.textures_delta.clear();
        texts
    });
    assert!(canvas_texts.iter().any(|t| t == "3 strokes · Nov 18, 2026, 8:43 PM PST"), "canvas list formats local time: {canvas_texts:?}");
    assert_eq!(canvas_app.session.canvases.items[0].updated_at, SUPPLIED);

    // Shared-canvas Trash formats `deleted_at_ms` via the same formatter.
    let mut world_app = world_fixture();
    let wctx = egui::Context::default();
    PhotocraftApp::setup_context(&wctx, world_app.ui.theme);
    let mut t = 0.0;
    let trash_texts = komaphone_time::with_viewer_timezone(komaphone_time::fixture_pacific_12h, || {
        click_world_control(&mut world_app, &wctx, Vec2::new(1280.0, 720.0), 1.0, &mut t, "world.section.trash");
        t += 0.1;
        run_world_frame(&mut world_app, &wctx, Vec2::new(1280.0, 720.0), 1.0, t, vec![])
    });
    assert!(trash_texts.iter().any(|text| text == "Deleted Nov 18, 2026, 8:43 PM PST"), "world trash formats deleted_at_ms: {trash_texts:?}");
}

#[test]
fn qol004_party_corpse_editing_reports_segment_dimensions_excluding_seam_strip() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
    app.ui.komaphone.enabled = true;
    app.run("file.new", json!({"width": 1024, "height": 512, "background": "white"})).unwrap();
    let alice = party_participant(1, "Alice");
    let room_id = RoomId::new(&"77".repeat(16)).unwrap();
    let game_id = GameId::new(&"88".repeat(16)).unwrap();
    app.session.rooms.status.profile = Some(alice.clone());
    app.session.rooms.status.room = Some(RoomView {
        id: room_id,
        revision: 2,
        epoch: 1,
        phase: Phase::Drawing,
        round: 1,
        mode: Mode::ExquisiteCorpse,
        timer_ms: None,
        moderator: alice.id,
        members: vec![MemberView { participant: alice.clone(), connected: true, ready: true }],
        waiting: vec![],
        paused: None,
        deadline_ms: None,
        assignment: Some(AssignmentView {
            assignment: Assignment { id: AssignmentId { game: game_id, chain: 0, round: 1 }, player: alice.id, project: project_id(5) },
            submitted: false,
            reference: Some(Reference::Seam { image: AssetRef { hash: AssetHash::new(&"22".repeat(32)).unwrap(), bytes: 64 }, width: 1024, height: 24 }),
        }),
        reveal: vec![],
    });
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, app.ui.theme);
    let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 720.0))), ..Default::default() }, |ui| {
        komaphone_workspace::chrome(&mut app, ui);
        crate::komaphone_game_view::reference_panel(&mut app, ui);
        egui::CentralPanel::default().show(ui, |ui| crate::canvas::document_area(&mut app, ui));
    });
    let texts = collect_texts(&out.shapes);
    out.textures_delta.clear();
    assert!(texts.iter().any(|t| t == "1024 px x 512 px (72 ppi)"), "corpse editing shows editable segment size, not seam+segment: {texts:?}");
    assert!(!texts.iter().any(|t| t.contains("536 px")), "must not include 24 px seam reference strip");
}

// ------------------------------------------------------------------ Visual wgpu capture suite (opt-in via env var)

#[cfg(not(target_arch = "wasm32"))]
fn save_wgpu_png(
    dir: &std::path::Path,
    name: &str,
    size: Vec2,
    ppp: f32,
    a11y_check: bool,
    app: PhotocraftApp,
    mut draw: impl FnMut(&mut egui::Ui, &mut PhotocraftApp) + 'static,
) {
    let mut styled = false;
    let mut h = egui_kittest::Harness::builder().with_size(size).with_pixels_per_point(ppp).with_accessibility_check(a11y_check).wgpu().build_ui_state(
        move |ui, app: &mut PhotocraftApp| {
            if !styled {
                PhotocraftApp::setup_context(ui.ctx(), crate::theme::ThemeKind::ProMedium);
                styled = true;
                return;
            }
            draw(ui, app);
        },
        app,
    );
    for _ in 0..25 {
        h.run_steps(2);
        if !h.state().world_render.preparing && !h.state().world_render.stats.is_refining && h.state().world_render.stats.anim_pending_tiles == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    h.run_steps(2);
    h.render().unwrap().save(dir.join(name)).unwrap();
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn capture_qol_small_ui_batch_visual_evidence() {
    let Some(dir) = std::env::var_os("KOMAPHONE_QOL_BATCH_RENDERS").map(std::path::PathBuf::from) else {
        return;
    };
    let _gpu = photocraft_testkit::gpu_lock();
    std::fs::create_dir_all(&dir).unwrap();

    // 1. QOL-001: Safe GPU fallback modal over Komaphone home.
    {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
        app.ui.komaphone.enabled = true;
        crate::gpu_status::queue_fallback_notice(&mut app, "--safe-gpu: CPU renderer for this launch");
        save_wgpu_png(&dir, "01-qol001-safe-gpu-modal.png", Vec2::new(1280.0, 720.0), 1.0, true, app, |ui, app| {
            komaphone_workspace::chrome(app, ui);
            egui::CentralPanel::default().show(ui, |ui| komaphone_workspace::home(app, ui));
            crate::gpu_status::show_fallback(app, ui.ctx());
        });
    }

    // 2 & 3. QOL-003 + QOL-012 + QOL-004 + QOL-011: Shared canvas expanded (labels ON) vs collapsed (labels OFF) + narrow 760x480.
    {
        let app = world_fixture();
        save_wgpu_png(&dir, "02-qol003-012-world-expanded-labels-on.png", Vec2::new(1280.0, 720.0), 1.0, true, app, |ui, app| {
            crate::komaphone_world::show(app, ui);
        });
    }
    {
        let mut app = world_fixture();
        app.ui.world.sidebar_collapsed = true;
        app.ui.world.show_peer_labels = false;
        save_wgpu_png(&dir, "03-qol003-012-world-collapsed-labels-off.png", Vec2::new(1280.0, 720.0), 1.0, true, app, |ui, app| {
            crate::komaphone_world::show(app, ui);
        });
    }
    {
        let mut app = world_fixture();
        app.ui.world.sidebar_collapsed = true;
        save_wgpu_png(&dir, "07-qol003-world-narrow-760x480-2x.png", Vec2::new(760.0, 480.0), 2.0, true, app, |ui, app| {
            crate::komaphone_world::show(app, ui);
        });
    }

    // 4 & 5. QOL-004 + QOL-011: Finite Workroom at 1280x720 (1536x1024) and narrow 760x480 (1024x512).
    {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
        app.ui.komaphone.enabled = true;
        app.run("file.new", json!({"width": 1536, "height": 1024, "background": "white"})).unwrap();
        save_wgpu_png(&dir, "04-qol004-011-workroom-1280x720.png", Vec2::new(1280.0, 720.0), 1.0, true, app, |ui, app| {
            komaphone_workspace::chrome(app, ui);
            egui::CentralPanel::default().show(ui, |ui| crate::canvas::document_area(app, ui));
        });
    }
    {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
        app.ui.komaphone.enabled = true;
        app.run("file.new", json!({"width": 1024, "height": 512, "background": "white"})).unwrap();
        save_wgpu_png(&dir, "05-qol004-workroom-narrow-760x480.png", Vec2::new(760.0, 480.0), 1.0, true, app, |ui, app| {
            komaphone_workspace::chrome(app, ui);
            egui::CentralPanel::default().show(ui, |ui| crate::canvas::document_area(app, ui));
        });
    }

    // 6. QOL-011 + QOL-015: Gallery with supplied 2026-11-19T04:43:34.946Z, summer PDT, invalid date, and top-level Workroom button.
    komaphone_time::with_viewer_timezone(komaphone_time::fixture_pacific_12h, || {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
        app.ui.komaphone.enabled = true;
        app.ui.gallery.open = true;
        let id1 = project_id(1);
        let id2 = project_id(2);
        let id3 = project_id(3);
        app.session.gallery.items = vec![
            GalleryItem {
                id: id1,
                title: "Late Night Creature".into(),
                kind: "creature".into(),
                authors: vec!["Alice".into(), "Bob".into()],
                updated_at: "2026-11-19T04:43:34.946Z".into(),
                favorite: true,
                trashed: false,
                complete: true,
                draft: false,
                revision: 1,
                state: "Saved locally".into(),
                source_projects: vec![id1],
            },
            GalleryItem {
                id: id2,
                title: "Summer Animation".into(),
                kind: "animation".into(),
                authors: vec!["Alice".into()],
                updated_at: "2026-07-15T18:30:00.000Z".into(),
                favorite: false,
                trashed: false,
                complete: true,
                draft: false,
                revision: 1,
                state: "Saved locally".into(),
                source_projects: vec![id2],
            },
            GalleryItem {
                id: id3,
                title: "Corrupt Timestamp Draft".into(),
                kind: "draft".into(),
                authors: vec!["You".into()],
                updated_at: "bad-timestamp".into(),
                favorite: false,
                trashed: false,
                complete: true,
                draft: true,
                revision: 1,
                state: "Saved locally".into(),
                source_projects: vec![id3],
            },
        ];
        app.session.gallery.active = Some(Arc::new(GalleryManifest {
            schema_version: GALLERY_SCHEMA,
            id: id1,
            title: "Late Night Creature".into(),
            created_at: "2026-11-19T04:43:34.946Z".into(),
            updated_at: "2026-11-19T04:43:34.946Z".into(),
            authors: vec![party_participant(1, "Alice"), party_participant(2, "Bob")],
            content: GalleryContent::Project {
                checkpoint: AssetRef { hash: AssetHash::new(&"11".repeat(32)).unwrap(), bytes: 64 },
                direct_assets: vec![],
                replay: None,
            },
            provenance: None,
            notes: vec![],
            fps: 6,
            looping: true,
        }));
        app.session.gallery.image = Some(Arc::new(RoomImage { width: 320, height: 240, rgba: [70, 145, 220, 255].repeat(320 * 240) }));
        save_wgpu_png(&dir, "06-qol011-015-gallery-pacific-12h.png", Vec2::new(1280.0, 720.0), 1.0, false, app, |ui, app| {
            komaphone_workspace::chrome(app, ui);
            egui::CentralPanel::default().show(ui, |ui| crate::komaphone_gallery::show(app, ui));
        });
    });
}
