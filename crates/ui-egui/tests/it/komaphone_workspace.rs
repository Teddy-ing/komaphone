//! Mouse interaction with the curated chrome, dispatching the real editor commands.

use egui::{Event, PointerButton, Pos2, Rect, Vec2};
use photocraft_engine::Session;
use photocraft_ui_egui::canvas::{self, ViewXform};
use photocraft_ui_egui::{PhotocraftApp, Tool, komaphone_workspace};
use serde_json::json;

struct Workspace {
    app: PhotocraftApp,
    ctx: egui::Context,
    time: f64,
}

impl Workspace {
    fn new() -> Self {
        let mut app = PhotocraftApp::new(Session::new(), Default::default());
        app.run("file.new", json!({"width":1536,"height":1024,"background":"transparent"})).unwrap();
        app.ui.komaphone.enabled = true;
        let ctx = egui::Context::default();
        PhotocraftApp::setup_context(&ctx, app.ui.theme);
        let mut this = Self { app, ctx, time: 0.0 };
        this.frame(vec![]);
        this.frame(vec![]);
        this
    }

    fn frame(&mut self, mut events: Vec<Event>) {
        self.time += 0.1;
        let modifiers = events
            .iter()
            .rev()
            .find_map(|event| match event {
                Event::ModifiersChanged(modifiers) | Event::Key { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        events.insert(0, Event::ModifiersChanged(modifiers));
        let input = egui::RawInput {
            time: Some(self.time),
            events,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 720.0))),
            ..Default::default()
        };
        let mut output = self.ctx.run_ui(input, |ui| {
            if self.app.ui.komaphone.enabled {
                komaphone_workspace::chrome(&mut self.app, ui);
            } else {
                komaphone_workspace::advanced_return(&mut self.app, ui);
            }
            egui::CentralPanel::default().show(ui, |ui| canvas::document_area(&mut self.app, ui));
        });
        output.textures_delta.clear();
    }

    fn click(&mut self, name: &str) {
        self.frame(vec![]);
        let rect = komaphone_workspace::control_rect(&self.ctx, name).unwrap_or_else(|| panic!("missing control {name}"));
        assert!(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 720.0)).contains_rect(rect), "control clipped: {name} {rect:?}");
        let pos = rect.center();
        let pointer = |pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        self.frame(vec![Event::PointerMoved(pos), pointer(true)]);
        self.frame(vec![pointer(false)]);
        self.frame(vec![]);
    }

    fn draw(&mut self, start: [f32; 2], end: [f32; 2]) {
        self.draw_path(&[start, [(start[0] + end[0]) * 0.5, (start[1] + end[1]) * 0.5], end]);
    }

    fn draw_path(&mut self, points: &[[f32; 2]]) {
        let transform = ViewXform::active(&self.app).unwrap();
        let start = transform.to_screen(points[0][0], points[0][1]);
        let end = transform.to_screen(points.last().unwrap()[0], points.last().unwrap()[1]);
        let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        self.frame(vec![Event::PointerMoved(start), button(start, true)]);
        for point in &points[1..] {
            self.frame(vec![Event::PointerMoved(transform.to_screen(point[0], point[1]))]);
        }
        self.frame(vec![button(end, false)]);
        self.frame(vec![]);
    }

    fn pixel(&self, x: i32, y: i32) -> [f32; 4] {
        let state = self.app.session.active().unwrap();
        let surface = state.doc.layer(state.active_layer.unwrap()).unwrap().surface().unwrap();
        let mut pixel = [[0.0; 4]];
        surface.read_rgba_into(photocraft_geom::Rect::new(x, y, x + 1, y + 1), &mut pixel);
        pixel[0]
    }
}

#[test]
fn every_essential_tool_is_one_mouse_click_away_at_1280() {
    let mut workspace = Workspace::new();
    for &(tool, label) in komaphone_workspace::TOOLS {
        workspace.click(&format!("tool.{label}"));
        assert_eq!(workspace.app.ui.tool, tool, "{label}");
    }
    assert!(komaphone_workspace::TOOLS.iter().any(|(tool, _)| *tool == Tool::Type));
}

#[test]
fn layer_buttons_preserve_real_undo_and_visibility() {
    use egui_kittest::kittest::{NodeT, Queryable};

    let mut harness = native_harness(Default::default());
    let original_count = harness.state().session.active().unwrap().doc.layer_count();
    let new_layer = harness.get_by_label("Create a new layer").rect().center();
    native_press(&mut harness, new_layer, true);
    native_press(&mut harness, new_layer, false);
    harness.run_steps(3);
    assert_eq!(harness.state().session.active().unwrap().doc.layer_count(), original_count + 1);

    let first_added = harness.state().session.active().unwrap().active_layer.unwrap();
    let row = photocraft_ui_egui::layer_row_ui::recorded(&harness.ctx).into_iter().find(|r| r.layer == first_added.0).expect("row drawn");
    let new_layer = harness.get_by_label("Create a new layer").rect().center();
    native_drag(&mut harness, row.row.center(), new_layer);
    assert_eq!(harness.state().session.active().unwrap().doc.layer_count(), original_count + 2);

    native_click(&mut harness, "edit.undo");
    assert_eq!(harness.state().session.active().unwrap().doc.layer_count(), original_count + 1);
    native_click(&mut harness, "edit.redo");
    assert_eq!(harness.state().session.active().unwrap().doc.layer_count(), original_count + 2);

    let layer = harness.state().session.active().unwrap().active_layer.unwrap();
    let row = photocraft_ui_egui::layer_row_ui::recorded(&harness.ctx).into_iter().find(|r| r.layer == layer.0).expect("active row drawn");
    let eye = egui::pos2(row.row.left() + 15.0, row.row.center().y);
    native_press(&mut harness, eye, true);
    native_press(&mut harness, eye, false);
    harness.run_steps(3);
    assert!(!harness.state().session.active().unwrap().doc.layer(layer).unwrap().visible);
    native_click(&mut harness, "edit.undo");
    assert!(harness.state().session.active().unwrap().doc.layer(layer).unwrap().visible);

    harness.get_by_label("Lock all").click();
    harness.run_steps(3);
    assert!(harness.state().session.active().unwrap().doc.layer(layer).unwrap().locks.all);
    harness.get_by_label("Lock all").click();
    harness.run_steps(3);
    assert!(!harness.state().session.active().unwrap().doc.layer(layer).unwrap().locks.any());

    let opacity = harness
        .query_all_by_role(egui::accesskit::Role::SpinButton)
        .find(|node| node.accesskit_node().label().as_deref() == Some("Opacity") && node.rect().left() > 900.0)
        .expect("Layers Opacity field")
        .rect()
        .center();
    native_press(&mut harness, opacity, true);
    native_press(&mut harness, opacity, false);
    harness.run_steps(2);
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::A);
    harness.event(Event::Text("50".into()));
    harness.key_press(egui::Key::Enter);
    harness.run_steps(3);
    assert!((harness.state().session.active().unwrap().doc.layer(layer).unwrap().opacity - 0.5).abs() < 0.01);

    let trash = harness.get_by_label("Delete layer").rect().center();
    native_press(&mut harness, trash, true);
    native_press(&mut harness, trash, false);
    harness.run_steps(3);
    assert!(harness.state().session.active().unwrap().doc.layer(layer).is_some(), "trash click prompts before deleting layer");
    harness.get_by_label("Delete").click();
    harness.run_steps(3);
    assert!(harness.state().session.active().unwrap().doc.layer(layer).is_none());
}

#[test]
fn advanced_roundtrip_keeps_document_and_tool_intact() {
    let mut workspace = Workspace::new();
    workspace.click("tool.Pencil");
    let document = workspace.app.session.active().unwrap().doc.clone();
    workspace.click("workspace.advanced");
    assert!(!workspace.app.ui.komaphone.enabled);
    workspace.click("workspace.drawing");
    assert!(workspace.app.ui.komaphone.enabled);
    assert_eq!(workspace.app.ui.tool, Tool::Pencil);
    assert_eq!(workspace.app.session.active().unwrap().doc, document);
}

#[test]
fn workspace_view_state_roundtrips_without_claiming_durability() {
    let state = komaphone_workspace::WorkspaceState {
        enabled: true,
        drafts_open: true,
        rename_layer: Some(27),
        rename_document: Some(2),
        layer_name: "Ink".into(),
        properties_open: false,
    };
    let value = serde_json::to_value(&state).unwrap();
    assert_eq!(serde_json::from_value::<komaphone_workspace::WorkspaceState>(value.clone()).unwrap(), state);
    assert!(value.get("saved").is_none());
    assert!(!komaphone_workspace::WorkspaceState::default().enabled);
    let mut workspace = Workspace::new();
    workspace.app.ui.komaphone = state;
    let (request, _) = photocraft_ui_egui::control::ControlRequest::new("ui.inspect", json!({}));
    let photocraft_ui_egui::control::Outcome::Done(reply) = photocraft_ui_egui::control::handle(&mut workspace.app, &workspace.ctx, &request) else {
        panic!("inspection must complete synchronously")
    };
    assert_eq!(reply["result"]["komaphone"], value);
}

#[test]
fn mouse_tools_change_real_pixels_selection_and_shapes() {
    let mut workspace = Workspace::new();
    workspace.app.run("tools.setColors", json!({"foreground":[1.0,0.0,0.0,1.0]})).unwrap();
    workspace.click("tool.Brush");
    workspace.app.run("tools.setBrush", json!({"brush":{"size":40.0,"hardness":1.0,"smoothing":{"enabled":false}}})).unwrap();
    workspace.draw([600.0, 500.0], [900.0, 500.0]);
    assert!(workspace.pixel(750, 500)[0] > 0.99 && workspace.pixel(750, 500)[3] > 0.99, "brush paints red through canvas mouse events");
    workspace.click("tool.Eraser");
    workspace.app.run("tools.setBrush", json!({"brush":{"size":40.0,"hardness":1.0,"smoothing":{"enabled":false}}})).unwrap();
    workspace.draw([600.0, 500.0], [900.0, 500.0]);
    assert!(workspace.pixel(750, 500)[3] < 0.01, "eraser removes the stroke");
    workspace.click("tool.Fill");
    workspace.draw([750.0, 500.0], [750.0, 500.0]);
    assert!(workspace.pixel(750, 500)[0] > 0.99 && workspace.pixel(750, 500)[3] > 0.99, "fill paints the empty layer");
    workspace.click("tool.Select");
    workspace.draw([650.0, 450.0], [850.0, 550.0]);
    assert!(workspace.app.session.active().unwrap().doc.selection.is_some(), "marquee selection is created");
    workspace.click("select.deselect");
    assert!(workspace.app.session.active().unwrap().doc.selection.is_none());
    let layers = workspace.app.session.active().unwrap().doc.layer_count();
    workspace.click("tool.Rectangle");
    workspace.draw([650.0, 400.0], [850.0, 600.0]);
    assert_eq!(workspace.app.session.active().unwrap().doc.layer_count(), layers + 1, "shape drag creates a new editable shape layer");
}

#[test]
fn pencil_pick_move_lasso_ellipse_and_line_produce_real_document_edits() {
    let mut workspace = Workspace::new();
    workspace.app.run("tools.setColors", json!({"foreground":[1.0,0.0,0.0,1.0]})).unwrap();
    workspace.click("tool.Pencil");
    workspace.app.run("tools.setBrush", json!({"brush":{"size":24.0,"hardness":1.0,"smoothing":{"enabled":false}}})).unwrap();
    workspace.draw([600.0, 500.0], [900.0, 500.0]);
    assert!(workspace.pixel(750, 500)[0] > 0.99 && workspace.pixel(750, 500)[3] > 0.99, "Pencil writes opaque red pixels");
    workspace.app.run("tools.setColors", json!({"foreground":[0.0,0.0,1.0,1.0]})).unwrap();
    workspace.click("tool.Pick color");
    workspace.draw([750.0, 500.0], [750.0, 500.0]);
    let picked = workspace.app.session.tools.foreground;
    assert!(picked[0] > 0.99 && picked[2] < 0.01, "Pick color samples the actual pencil stroke: {picked:?}");
    workspace.click("tool.Move");
    workspace.draw([750.0, 500.0], [810.0, 580.0]);
    assert!(workspace.pixel(810, 580)[3] > 0.99 && workspace.pixel(750, 500)[3] < 0.01, "Move relocates the raster stroke");
    workspace.click("tool.Lasso");
    workspace.draw_path(&[[600.0, 400.0], [900.0, 400.0], [900.0, 650.0], [600.0, 650.0], [600.0, 400.0]]);
    assert!(workspace.app.session.active().unwrap().doc.selection.is_some(), "Lasso creates an actual selection mask");
    workspace.click("select.deselect");
    workspace.click("tool.Ellipse");
    workspace.draw([650.0, 400.0], [850.0, 600.0]);
    let state = workspace.app.session.active().unwrap();
    let shape = match &state.doc.layer(state.active_layer.unwrap()).unwrap().content {
        photocraft_doc::LayerContent::Shape(shape) => shape,
        _ => panic!("ellipse must create an editable shape"),
    };
    assert!(matches!(shape.live, Some(photocraft_doc::LiveShape::Ellipse { rect }) if rect[2] > 190.0 && rect[3] > 190.0));
    workspace.click("tool.Line");
    workspace.draw([650.0, 420.0], [850.0, 580.0]);
    let state = workspace.app.session.active().unwrap();
    let shape = match &state.doc.layer(state.active_layer.unwrap()).unwrap().content {
        photocraft_doc::LayerContent::Shape(shape) => shape,
        _ => panic!("line must create an editable shape"),
    };
    // The Line UI sends its arrow options even when both ends are disabled, so the engine
    // preserves those editable options in ArrowLine. Both representations draw a plain line.
    let (from, to, weight) = match shape.live.as_ref() {
        Some(photocraft_doc::LiveShape::Line { from, to, weight })
        | Some(photocraft_doc::LiveShape::ArrowLine { from, to, weight, start: false, end: false, .. }) => (*from, *to, *weight),
        other => panic!("expected an editable plain line, got {other:?}"),
    };
    assert!(to[0] - from[0] > 190.0 && to[1] - from[1] > 150.0 && weight > 0.0, "line retains the mouse drag's geometry: {from:?} to {to:?}");
    assert!(shape.path.control_bounds().is_some(), "the line has a concrete drawable path");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn another_documents_pending_edits_do_not_hide_the_active_drafts_acknowledgment() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::new();
    session.enable_local_projects(directory.path().join("drafts.sqlite3"), Default::default()).unwrap();
    let mut app = PhotocraftApp::new(session, Default::default());
    app.run("file.new", json!({"width":16,"height":16})).unwrap();
    app.run("layer.new.layer", json!({})).unwrap();
    app.session.flush_local_projects(std::time::Duration::from_secs(10)).unwrap();
    let saved = app.session.active().unwrap().doc.id;
    app.run("file.new", json!({"width":16,"height":16})).unwrap();
    app.run("layer.new.layer", json!({})).unwrap();
    let pending = app.session.active().unwrap().doc.id;
    assert!(app.session.local_replay.pending_for(pending) > 0);
    app.session.set_active(0);
    assert!(app.session.local_document_saved(saved));
    assert!(komaphone_workspace::local_save_label(&app).starts_with("Saved locally"));
}

#[test]
fn live_gradient_preview_is_reported_as_unsaved_before_mouse_release() {
    let mut workspace = Workspace::new();
    workspace.app.ui.tool = Tool::Gradient;
    workspace.app.ui.tool_options.gradient_classic = false;
    workspace.frame(vec![]);
    let transform = ViewXform::active(&workspace.app).unwrap();
    let start = transform.to_screen(650.0, 450.0);
    let end = transform.to_screen(850.0, 550.0);
    workspace.frame(vec![
        Event::PointerMoved(start),
        Event::PointerButton { pos: start, button: PointerButton::Primary, pressed: true, modifiers: Default::default() },
    ]);
    workspace.frame(vec![Event::PointerMoved(end)]);
    assert_eq!(komaphone_workspace::local_save_label(&workspace.app), "Drawing · Unsaved");
}

type NativeHarness = egui_kittest::Harness<'static, PhotocraftApp>;

fn native_harness(services: photocraft_ui_egui::Services) -> NativeHarness {
    let mut harness =
        egui_kittest::Harness::builder().with_size(egui::vec2(1280.0, 720.0)).with_step_dt(1.0 / 60.0).with_max_steps(64).build_eframe(move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            let mut app = PhotocraftApp::new(Session::new(), services);
            app.ui.komaphone.enabled = true;
            app.session.prefs.edit(|prefs| prefs.interface.language = "en".into());
            app.run("file.new", json!({"width":1536,"height":1024,"background":"transparent"})).unwrap();
            app
        });
    harness.run_steps(5);
    harness
}

fn native_click(harness: &mut NativeHarness, name: &str) {
    harness.run_steps(2);
    let rect = komaphone_workspace::control_rect(&harness.ctx, name).unwrap_or_else(|| panic!("missing control {name}"));
    assert!(Rect::from_min_size(Pos2::ZERO, egui::vec2(1280.0, 720.0)).contains_rect(rect), "control clipped: {name} {rect:?}");
    let position = rect.center();
    native_press(harness, position, true);
    native_press(harness, position, false);
    harness.run_steps(2);
}

fn native_press(harness: &mut NativeHarness, position: Pos2, pressed: bool) {
    harness.event(Event::PointerMoved(position));
    harness.event(Event::PointerButton { pos: position, button: PointerButton::Primary, pressed, modifiers: Default::default() });
    harness.step();
}

fn native_drag(harness: &mut NativeHarness, start: Pos2, end: Pos2) {
    native_press(harness, start, true);
    harness.event(Event::PointerMoved(start.lerp(end, 0.5)));
    harness.step();
    harness.event(Event::PointerMoved(end));
    harness.step();
    native_press(harness, end, false);
    harness.run_steps(3);
}

#[test]
fn mouse_text_transform_pan_and_wheel_use_the_full_application() {
    use egui_kittest::kittest::Queryable;

    let mut harness = native_harness(Default::default());
    native_click(&mut harness, "tool.Text");
    let transform = ViewXform::active(harness.state()).unwrap();
    let position = transform.to_screen(650.0, 450.0);
    native_press(&mut harness, position, true);
    native_press(&mut harness, position, false);
    harness.run_steps(2);
    assert!(harness.state().ui.text_edit.is_some());
    assert_eq!(komaphone_workspace::local_save_label(harness.state()), "Drawing · Unsaved");
    harness.event(Event::Text("Komaphone".into()));
    harness.run_steps(3);
    native_click(&mut harness, "tool.Move");
    assert!(harness.state().ui.text_edit.is_none(), "switching tools commits type editing");
    let layer = harness.state().session.active().unwrap().active_layer.unwrap();
    let text = match &harness.state().session.active().unwrap().doc.layer(layer).unwrap().content {
        photocraft_doc::LayerContent::Text(text) => text,
        other => panic!("expected text, got {other:?}"),
    };
    assert_eq!(text.text, "Komaphone");
    let original = text.transform;
    native_click(&mut harness, "edit.freeTransform");
    assert!(harness.state().ui.transform.is_some());
    harness.get_by_role_and_label(egui::accesskit::Role::SpinButton, "Rotate").click();
    harness.run_steps(2);
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::A);
    harness.event(Event::Text("15".into()));
    harness.run_steps(2);
    harness.get_by_label("Commit transform").click();
    harness.run_steps(4);
    assert!(harness.state().ui.transform.is_none());
    let transformed = match &harness.state().session.active().unwrap().doc.layer(layer).unwrap().content {
        photocraft_doc::LayerContent::Text(text) => text.transform,
        _ => panic!("type layer lost"),
    };
    assert_ne!(transformed, original, "mouse-accessible rotation edits the real type transform");
    native_click(&mut harness, "edit.freeTransform");
    harness.get_by_role_and_label(egui::accesskit::Role::SpinButton, "Width").click();
    harness.run_steps(2);
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::A);
    harness.event(Event::Text("150%".into()));
    harness.run_steps(2);
    harness.get_by_label("Commit transform").click();
    harness.run_steps(4);
    let scaled = match &harness.state().session.active().unwrap().doc.layer(layer).unwrap().content {
        photocraft_doc::LayerContent::Text(text) => text.transform,
        _ => panic!("type layer lost"),
    };
    assert_ne!(scaled, transformed, "mouse-accessible width changes the real type scale");
    native_click(&mut harness, "tool.Pan");
    let center = harness.state().last_canvas_rect.center();
    let before = ViewXform::active(harness.state()).unwrap().center;
    native_drag(&mut harness, center, center + egui::vec2(75.0, 30.0));
    assert_ne!(ViewXform::active(harness.state()).unwrap().center, before);
    let before = harness.state().current_zoom();
    harness.event(Event::PointerMoved(center));
    harness.event(Event::MouseWheel {
        source: egui::MouseWheelSource::Unknown,
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, 1.0),
        phase: egui::TouchPhase::Move,
        modifiers: Default::default(),
    });
    harness.run_steps(12);
    assert!(harness.state().current_zoom() > before, "plain wheel zooms in the curated workspace");
    native_click(&mut harness, "view.actualPixels");
    harness.run_steps(12);
    assert_eq!(harness.state().current_zoom(), 1.0);
}

#[test]
fn import_button_decodes_selected_png_without_replacing_the_drawing() {
    use photocraft_ui_egui::{FileDialogAnswer, FileDialogRequest, Services};

    let mut source = Session::new();
    source.execute("file.new", json!({"width":24,"height":16,"background":"white"})).unwrap();
    source.execute("edit.fill", json!({"color":"#204080"})).unwrap();
    let bytes = photocraft_io::export(&source.active().unwrap().doc, "png", &Default::default()).unwrap().bytes;
    let services = Services {
        file_dialog: Some(Box::new(move |request, _, reply| {
            assert!(matches!(request, FileDialogRequest::Open { multiple: false, .. }));
            reply.send(Some(FileDialogAnswer::Contents("generated-fixture.png".into(), bytes.clone())));
        })),
        import: Some(Box::new(|name, bytes, options| {
            photocraft_io::import_with_options(name, bytes, options).map(|result| (result.document, result.warnings)).map_err(|error| error.to_string())
        })),
        ..Default::default()
    };
    let mut harness = native_harness(services);
    let before = harness.state().session.active().unwrap().doc.clone();
    native_click(&mut harness, "file.placeEmbedded");
    harness.run_steps(8);
    let document = &harness.state().session.active().unwrap().doc;
    assert_eq!(document.id, before.id);
    assert_eq!(document.layer_count(), before.layer_count() + 1, "import creates a separate editable layer");
    assert_eq!(document.size, before.size);
    assert!(!harness.state().file_dialog_open());
}

#[test]
fn layer_rename_and_reorder_work_with_mouse_controls() {
    use egui_kittest::kittest::Queryable;

    let mut harness = native_harness(Default::default());
    for _ in 0..2 {
        let new_layer = harness.get_by_label("Create a new layer").rect().center();
        native_press(&mut harness, new_layer, true);
        native_press(&mut harness, new_layer, false);
        harness.run_steps(3);
    }
    let layer = harness.state().session.active().unwrap().active_layer.unwrap();
    let row = photocraft_ui_egui::layer_row_ui::recorded(&harness.ctx).into_iter().find(|r| r.layer == layer.0).expect("row drawn");
    let name_pos = row.name.expect("layer name rect").center();
    // Double-click the layer name in PhotoCraft's reused Layers panel to rename in place.
    for _ in 0..40 {
        harness.step();
    }
    for _ in 0..2 {
        native_press(&mut harness, name_pos, true);
        native_press(&mut harness, name_pos, false);
    }
    harness.run_steps(3);
    harness.event(Event::Text("Foreground ink".into()));
    harness.key_press(egui::Key::Enter);
    harness.run_steps(3);
    assert_eq!(harness.state().session.active().unwrap().doc.layer(layer).unwrap().name, "Foreground ink");
    assert_eq!(harness.state().session.active().unwrap().doc.layers.last().unwrap().id, layer);

    // Drag the top row below the row underneath it to reorder in the document stack.
    let rows = photocraft_ui_egui::layer_row_ui::recorded(&harness.ctx);
    assert!(rows.len() >= 2);
    let from = rows[0].row.center();
    let below_second = egui::pos2(rows[1].row.center().x, rows[1].row.top() + rows[1].row.height() * 0.8);
    native_drag(&mut harness, from, below_second);
    assert_ne!(harness.state().session.active().unwrap().doc.layers.last().unwrap().id, layer);

    // Drag it back above the top row.
    let rows = photocraft_ui_egui::layer_row_ui::recorded(&harness.ctx);
    let moved_row = rows.iter().find(|r| r.layer == layer.0).expect("moved row").row;
    let above_top = egui::pos2(rows[0].row.center().x, rows[0].row.top() + rows[0].row.height() * 0.2);
    native_drag(&mut harness, moved_row.center(), above_top);
    assert_eq!(harness.state().session.active().unwrap().doc.layers.last().unwrap().id, layer);
}
