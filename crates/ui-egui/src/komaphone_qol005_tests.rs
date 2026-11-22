//! Comprehensive acceptance & visual-capture tests for QOL-005:
//! "Reuse PhotoCraft's full layer interface" inside Komaphone's minimalist workspace.

use std::sync::Arc;

use egui::{Event, Key, Modifiers, PointerButton, Pos2, Vec2, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use photocraft_color::BlendMode;
use photocraft_doc::{Document, LabelColor, LayerContent, LayerId};
use photocraft_engine::Session;
use photocraft_engine::party::room::{Assignment, AssignmentView, MemberView, Phase, Reference, RoomView};
use photocraft_engine::party::{AssetHash, AssetRef, AssignmentId, DeviceId as PartyDeviceId, GameId, Mode, Participant, ParticipantId, RoomId};
use photocraft_engine::scene::{
    CanvasId, OwnerId, coordinates,
    shared::{DeviceId, WorldState},
    world_protocol::{CanvasSummary, Member, Person},
};
use photocraft_engine::worlds::{Account, Canvas, Viewer};
use serde_json::json;

use crate::{PhotocraftApp, Services, komaphone_workspace};

fn komaphone_harness(session: Session, size: Vec2, ppp: f32) -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(size).with_pixels_per_point(ppp).with_step_dt(1.0 / 60.0).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, crate::theme::ThemeKind::ProMedium);
        let mut app = PhotocraftApp::new(session, Services::default());
        app.ui.komaphone.enabled = true;
        app.session.prefs.edit(|prefs| prefs.interface.language = "en".into());
        app
    });
    h.run_steps(6);
    h
}

fn click_at(h: &mut Harness<'_, PhotocraftApp>, pos: Pos2, button: PointerButton, modifiers: Modifiers) {
    h.hover_at(pos);
    h.step();
    h.event(Event::ModifiersChanged(modifiers));
    h.event(Event::PointerButton { pos, button, pressed: true, modifiers });
    h.step();
    h.event(Event::PointerButton { pos, button, pressed: false, modifiers });
    h.step();
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(3);
}

fn double_click_at(h: &mut Harness<'_, PhotocraftApp>, pos: Pos2) {
    h.hover_at(pos);
    for _ in 0..40 {
        h.step();
    }
    for _ in 0..2 {
        h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        h.step();
        h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
        h.step();
    }
    h.run_steps(3);
}

fn drag_at(h: &mut Harness<'_, PhotocraftApp>, from: Pos2, to: Pos2, release_mods: Modifiers) {
    h.hover_at(from);
    h.step();
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    for k in 1..=8 {
        h.event(Event::PointerMoved(from + (to - from) * (k as f32 / 8.0)));
        h.step();
    }
    h.event(Event::ModifiersChanged(release_mods));
    h.step();
    h.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: release_mods });
    h.step();
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(3);
}

fn composite_pixel(doc: &Document, x: i32, y: i32) -> [f32; 4] {
    photocraft_compose::render(doc, photocraft_geom::Rect::new(x, y, x + 1, y + 1)).px[0]
}

fn row_of(ctx: &egui::Context, id: LayerId) -> crate::layer_row_ui::RowRects {
    crate::layer_row_ui::recorded(ctx).into_iter().find(|r| r.layer == id.0).unwrap_or_else(|| panic!("row for layer {} not drawn", id.0))
}

fn party_participant(seed: u8, name: &str) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{seed:02x}").repeat(16)).unwrap(),
        device: PartyDeviceId::new(&format!("{seed:02x}").repeat(16)).unwrap(),
        display_name: name.to_owned(),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn new_corpse_party_session(submitted: bool) -> (Session, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("party-qol005.sqlite3");
    let mut session = Session::new();
    session.enable_local_projects(db, Default::default()).unwrap();
    session.execute("komaphone.project.new", json!({"name": "Corpse Band", "width": 1024, "height": 512})).unwrap();
    session.flush_local_projects(std::time::Duration::from_secs(5)).unwrap();
    let doc_id = session.active().unwrap().doc.id;
    let project = session.local_projects.status(doc_id).project.expect("persisted local project for party assignment");
    let alice = party_participant(1, "Alice");
    let room_id = RoomId::new(&"77".repeat(16)).unwrap();
    let game_id = GameId::new(&"88".repeat(16)).unwrap();
    session.rooms.status.profile = Some(alice.clone());
    session.rooms.status.room = Some(RoomView {
        id: room_id,
        revision: 4,
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
            assignment: Assignment { id: AssignmentId { game: game_id, chain: 0, round: 1 }, player: alice.id, project },
            submitted,
            reference: Some(Reference::Seam { image: AssetRef { hash: AssetHash::new(&"33".repeat(32)).unwrap(), bytes: 64 }, width: 1024, height: 24 }),
        }),
        reveal: vec![],
    });
    (session, dir)
}

#[test]
fn qol005_drag_reorder_changes_real_compositing_and_roundtrips_undo_redo() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48, "background": "white"})).unwrap();
    let bg = s.active().unwrap().doc.layers[0].id;
    let blue = LayerId(s.execute("layer.new.layer", json!({"name": "Blue"})).unwrap()["layer"].as_u64().unwrap());
    s.execute("edit.fill", json!({"color": "#1144ee"})).unwrap();
    let red = LayerId(s.execute("layer.new.layer", json!({"name": "Red"})).unwrap()["layer"].as_u64().unwrap());
    s.execute("edit.fill", json!({"color": "#ee2211"})).unwrap();

    let mut h = komaphone_harness(s, vec2(1280.0, 720.0), 1.0);
    assert!(h.state().ui.komaphone.enabled, "stays inside Komaphone minimalist workspace");

    // Before drag: Red is on top of Blue -> center pixel composites Red.
    let before = composite_pixel(&h.state().session.active().unwrap().doc, 32, 24);
    assert!(before[0] > 0.85 && before[2] < 0.15, "expected red on top, got {before:?}");

    // Drag Red row below Blue row inside PhotoCraft's reused Layers panel.
    let red_row = row_of(&h.ctx, red).row;
    let blue_row = row_of(&h.ctx, blue).row;
    let below_blue = pos2(blue_row.center().x, blue_row.top() + blue_row.height() * 0.8);
    drag_at(&mut h, red_row.center(), below_blue, Modifiers::NONE);

    let order: Vec<LayerId> = h.state().session.active().unwrap().doc.layers.iter().map(|l| l.id).collect();
    assert_eq!(order, vec![bg, red, blue], "Red dragged below Blue");
    let after = composite_pixel(&h.state().session.active().unwrap().doc, 32, 24);
    assert!(after[2] > 0.85 && after[0] < 0.15, "expected blue on top after drag reorder, got {after:?}");

    // Undo restores Red on top; Redo puts Blue back on top.
    h.state_mut().run("edit.undo", json!({})).unwrap();
    h.run_steps(2);
    let undone = composite_pixel(&h.state().session.active().unwrap().doc, 32, 24);
    assert!(undone[0] > 0.85 && undone[2] < 0.15, "undo restores red on top: {undone:?}");
    h.state_mut().run("edit.redo", json!({})).unwrap();
    h.run_steps(2);
    let redone = composite_pixel(&h.state().session.active().unwrap().doc, 32, 24);
    assert!(redone[2] > 0.85 && redone[0] < 0.15, "redo restores blue on top: {redone:?}");

    // Alt-drag duplicates a layer to the dropped position without moving the original.
    let red_row = row_of(&h.ctx, red).row;
    let blue_row = row_of(&h.ctx, blue).row;
    let above_blue = pos2(blue_row.center().x, blue_row.top() + blue_row.height() * 0.2);
    drag_at(&mut h, red_row.center(), above_blue, Modifiers::ALT);
    let names: Vec<String> = h.state().session.active().unwrap().doc.layers.iter().map(|l| l.name.clone()).collect();
    assert_eq!(names, ["Background", "Red", "Blue", "Red copy"]);
    let after_alt_copy = composite_pixel(&h.state().session.active().unwrap().doc, 32, 24);
    assert!(after_alt_copy[0] > 0.85 && after_alt_copy[2] < 0.15, "Red copy on top restores red composite: {after_alt_copy:?}");

    // Drag into a group: create an empty group, then drag Red copy into its middle band.
    let group_id = LayerId(h.state_mut().run("layer.new.group", json!({"name": "Folder"})).unwrap()["layer"].as_u64().unwrap());
    h.run_steps(3);
    let red_copy_id = h.state().session.active().unwrap().doc.layers.iter().find(|l| l.name == "Red copy").unwrap().id;
    let copy_row = row_of(&h.ctx, red_copy_id).row;
    let group_row = row_of(&h.ctx, group_id).row;
    drag_at(&mut h, copy_row.center(), group_row.center(), Modifiers::NONE);
    let group = h.state().session.active().unwrap().doc.layer(group_id).unwrap();
    let LayerContent::Group(g) = &group.content else { panic!("expected group") };
    assert!(g.children.iter().any(|c| c.id == red_copy_id), "Red copy dropped inside Folder group");
}

#[test]
fn qol005_background_lock_rules_hold_through_drag_drop_zones_menus_footer_and_convert() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48, "background": "white"})).unwrap();
    let bg = s.active().unwrap().doc.layers[0].id;
    let mut h = komaphone_harness(s, vec2(1280.0, 720.0), 1.0);

    // Lone Background: Delete layer, Add a mask, Add a layer style, Lock all are all disabled.
    assert!(h.get_by_label("Delete layer").accesskit_node().is_disabled(), "lone Background cannot be deleted");
    assert!(h.get_by_label("Add a layer style").accesskit_node().is_disabled(), "Background cannot take layer styles");
    let mask_label = format!("Add a mask  (from the selection; {} inverts)", crate::shortcuts::pretty("Alt"));
    assert!(h.get_by_label(&mask_label).accesskit_node().is_disabled(), "Background cannot take a layer mask");
    assert!(h.get_by_label("Lock all").accesskit_node().is_disabled(), "Pro lock toggles disabled on locked Background");

    // Open flyout menu on lone Background: Flatten Image, Lock Layers…, Rename Layer… disabled; New Layer… enabled.
    let menu_pos = komaphone_workspace::control_rect(&h.ctx, "layers.menu").expect("layers flyout button").center();
    click_at(&mut h, menu_pos, PointerButton::Primary, Modifiers::NONE);
    assert!(!h.get_by_label("New Layer…").accesskit_node().is_disabled());
    assert!(h.get_by_label("Flatten Image").accesskit_node().is_disabled());
    assert!(h.get_by_label("Lock Layers…").accesskit_node().is_disabled());
    assert!(h.get_by_label("Rename Layer…").accesskit_node().is_disabled());
    h.key_press(Key::Escape);
    h.run_steps(2);

    // Add a layer above Background so Paint is active and Background is inactive.
    let paint = LayerId(h.state_mut().run("layer.new.layer", json!({"name": "Paint"})).unwrap()["layer"].as_u64().unwrap());
    h.run_steps(3);

    // Drag Paint onto bottom half of Background row -> refused; Background stays at index 0.
    let paint_row = row_of(&h.ctx, paint).row;
    let bg_row = row_of(&h.ctx, bg).row;
    drag_at(&mut h, paint_row.center(), pos2(bg_row.center().x, bg_row.bottom() - 2.0), Modifiers::NONE);
    assert_eq!(h.state().session.active().unwrap().doc.layers[0].id, bg);

    // Drag Paint into empty drop zone BELOW Background row (`layer_drop_zone`) -> refused!
    let paint_row = row_of(&h.ctx, paint).row;
    let bg_row = row_of(&h.ctx, bg).row;
    drag_at(&mut h, paint_row.center(), pos2(bg_row.center().x, bg_row.bottom() + 18.0), Modifiers::NONE);
    assert_eq!(h.state().session.active().unwrap().doc.layers[0].id, bg, "empty space below Background must not accept drop below Background");

    // Drag Background above Paint without Alt -> refused!
    let paint_row = row_of(&h.ctx, paint).row;
    let bg_row = row_of(&h.ctx, bg).row;
    drag_at(&mut h, bg_row.center(), pos2(paint_row.center().x, paint_row.top() + 3.0), Modifiers::NONE);
    assert_eq!(h.state().session.active().unwrap().doc.layers[0].id, bg, "Background cannot be moved off bottom");

    // Right-click INACTIVE Background row -> context menu must still enforce Background blocks!
    let bg_row = row_of(&h.ctx, bg).row;
    click_at(&mut h, pos2(bg_row.left() + 60.0, bg_row.center().y), PointerButton::Secondary, Modifiers::NONE);
    for disabled_label in ["Blending Options…", "Add Layer Mask", "Create Clipping Mask", "Merge Down", "Rename Layer…"] {
        assert!(h.get_by_label(disabled_label).accesskit_node().is_disabled(), "inactive Background context menu must disable {disabled_label}");
    }
    h.key_press(Key::Escape);
    h.run_steps(2);

    // Clicking the Background row's lock icon converts it to Layer 0 and unlocks drop-below.
    let bg_lock = row_of(&h.ctx, bg).indicators.into_iter().find(|(k, _)| *k == crate::layer_row_ui::Indicator::Lock).expect("lock icon").1;
    click_at(&mut h, bg_lock.center(), PointerButton::Primary, Modifiers::NONE);
    assert!(!h.state().session.active().unwrap().doc.is_background(bg), "clicking row lock unlocks Background");
    assert_eq!(h.state().session.active().unwrap().doc.layer(bg).unwrap().name, "Layer 0");

    // Now dragging Paint below Layer 0 into the bottom drop zone succeeds!
    let paint_row = row_of(&h.ctx, paint).row;
    let l0_row = row_of(&h.ctx, bg).row;
    drag_at(&mut h, paint_row.center(), pos2(l0_row.center().x, l0_row.bottom() + 18.0), Modifiers::NONE);
    assert_eq!(h.state().session.active().unwrap().doc.layers[0].id, paint, "Paint moves below converted Layer 0");
}

#[test]
fn qol005_full_photocraft_layer_options_work_in_komaphone_workspace() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48, "background": "white"})).unwrap();
    let a = LayerId(s.execute("layer.new.layer", json!({"name": "Alpha"})).unwrap()["layer"].as_u64().unwrap());
    s.execute("edit.fill", json!({"color": "#22aa44"})).unwrap();
    let b = LayerId(s.execute("layer.new.layer", json!({"name": "Beta"})).unwrap()["layer"].as_u64().unwrap());
    s.execute("edit.fill", json!({"color": "#3366cc"})).unwrap();
    let mut h = komaphone_harness(s, vec2(1280.0, 720.0), 1.0);

    // 1. Alt-click eye solo (`layer.showOnly`) & restore.
    let b_row = row_of(&h.ctx, b).row;
    let b_eye = pos2(b_row.left() + 15.0, b_row.center().y);
    click_at(&mut h, b_eye, PointerButton::Primary, Modifiers::ALT);
    assert!(h.state().session.active().unwrap().doc.layer(b).unwrap().visible);
    assert!(!h.state().session.active().unwrap().doc.layer(a).unwrap().visible, "Alt-click eye hides sibling layers");
    click_at(&mut h, b_eye, PointerButton::Primary, Modifiers::ALT);
    assert!(h.state().session.active().unwrap().doc.layer(a).unwrap().visible, "second Alt-click restores siblings");

    // 2. Right-click eye column -> label color Red.
    click_at(&mut h, b_eye, PointerButton::Secondary, Modifiers::NONE);
    let red_item = h.get_by_role_and_label(egui::accesskit::Role::Button, "Red").rect().center();
    click_at(&mut h, red_item, PointerButton::Primary, Modifiers::NONE);
    assert_eq!(h.state().session.active().unwrap().doc.layer(b).unwrap().label, LabelColor::Red);

    // 3. Multi-select via Ctrl-click, link layers via footer button, then group via footer button.
    let a_row = row_of(&h.ctx, a).row;
    click_at(&mut h, pos2(a_row.left() + 80.0, a_row.center().y), PointerButton::Primary, Modifiers::COMMAND);
    assert_eq!(h.state().session.active().unwrap().selected_layers().len(), 2);
    let link_btn = h.get_by_label("Link layers");
    assert!(!link_btn.accesskit_node().is_disabled());
    let link_pos = link_btn.rect().center();
    click_at(&mut h, link_pos, PointerButton::Primary, Modifiers::NONE);
    let doc = &h.state().session.active().unwrap().doc;
    assert!(doc.layer(a).unwrap().link_group.is_some() && doc.layer(a).unwrap().link_group == doc.layer(b).unwrap().link_group);

    let group_pos = h.get_by_label("Create a new group").rect().center();
    click_at(&mut h, group_pos, PointerButton::Primary, Modifiers::NONE);
    let group_id = h.state().session.active().unwrap().active_layer.unwrap();
    assert!(h.state().session.active().unwrap().doc.layer(group_id).unwrap().is_group());

    // 4. Flyout Collapse All Groups collapses the new group.
    let menu_pos = komaphone_workspace::control_rect(&h.ctx, "layers.menu").unwrap().center();
    click_at(&mut h, menu_pos, PointerButton::Primary, Modifiers::NONE);
    let collapse_pos = h.get_by_label("Collapse All Groups").rect().center();
    click_at(&mut h, collapse_pos, PointerButton::Primary, Modifiers::NONE);
    let LayerContent::Group(g) = &h.state().session.active().unwrap().doc.layer(group_id).unwrap().content else {
        panic!("expected group");
    };
    assert!(!g.expanded, "Collapse All Groups collapsed the group");

    // 5. Add a mask on a normal layer via footer -> double-click mask obeys PhotoCraft's preference
    //    (Select and Mask by default, or on-demand Layer Properties when disabled) -> delete mask via footer.
    let c = LayerId(h.state_mut().run("layer.new.layer", json!({"name": "Masked"})).unwrap()["layer"].as_u64().unwrap());
    h.run_steps(3);
    let mask_label = format!("Add a mask  (from the selection; {} inverts)", crate::shortcuts::pretty("Alt"));
    let mask_pos = h.get_by_label(&mask_label).rect().center();
    click_at(&mut h, mask_pos, PointerButton::Primary, Modifiers::NONE);
    assert!(h.state().session.active().unwrap().doc.layer(c).unwrap().mask.is_some());
    assert!(!h.state().ui.komaphone.properties_open, "properties window stays hidden until requested");
    let mask_thumb = crate::mask_thumbs_ui::recorded(&h.ctx, c.0).expect("mask thumb").0[0].1;
    // With default PhotoCraft preference, double-clicking a pixel mask opens Select and Mask.
    double_click_at(&mut h, mask_thumb.center());
    assert!(!h.state().distort.describe()["selectAndMask"].is_null(), "default double-click on mask opens Select and Mask workspace");
    crate::select_mask_ui::cancel(h.state_mut());
    h.run_steps(3);
    // With Select-and-Mask double-click disabled, double-clicking opens Komaphone's Layer Properties window.
    h.state_mut().session.prefs.edit(|p| p.tools.double_click_layer_mask_launches_select_and_mask = false);
    h.run_steps(2);
    let mask_thumb = crate::mask_thumbs_ui::recorded(&h.ctx, c.0).expect("mask thumb").0[0].1;
    double_click_at(&mut h, mask_thumb.center());
    assert!(h.state().ui.komaphone.properties_open, "double-clicking mask opens Layer Properties window in Komaphone mode");
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(!h.state().ui.komaphone.properties_open, "Esc closes on-demand Layer Properties window");
    let del_mask = h.get_by_label("Delete Layer Mask").rect().center();
    click_at(&mut h, del_mask, PointerButton::Primary, Modifiers::NONE);
    assert!(h.state().session.active().unwrap().doc.layer(c).unwrap().mask.is_none());
    assert!(h.state().session.active().unwrap().doc.layer(c).is_some(), "layer itself kept");

    // 6. Add an Adjustment layer via footer press-menu -> creates adjustment layer & opens Layer Properties window.
    let adj_pos = h.get_by_label("Create new fill or adjustment layer").rect().center();
    click_at(&mut h, adj_pos, PointerButton::Primary, Modifiers::NONE);
    let bc_pos = h.get_by_label("Brightness/Contrast").rect().center();
    click_at(&mut h, bc_pos, PointerButton::Primary, Modifiers::NONE);
    let adj_id = h.state().session.active().unwrap().active_layer.unwrap();
    assert!(matches!(h.state().session.active().unwrap().doc.layer(adj_id).unwrap().content, LayerContent::Adjustment(_)));
    assert!(h.state().ui.komaphone.properties_open, "creating adjustment layer opens Layer Properties in Komaphone mode");
    h.key_press(Key::Escape);
    h.run_steps(2);

    // 7. Add a Drop Shadow layer style via footer fx menu on Masked layer.
    h.state_mut().run("layer.select", json!({"layer": c.0})).unwrap();
    h.run_steps(3);
    let fx_pos = h.get_by_label("Add a layer style").rect().center();
    click_at(&mut h, fx_pos, PointerButton::Primary, Modifiers::NONE);
    let ds_pos = h.get_by_label("Drop Shadow…").rect().center();
    click_at(&mut h, ds_pos, PointerButton::Primary, Modifiers::NONE);
    let dialog_id = h.state().ui.dialogs.iter().find(|d| d.kind == crate::state::DialogKind::LayerStyle).expect("Layer Style dialog").id;
    crate::dialogs::confirm(h.state_mut(), dialog_id).unwrap();
    h.run_steps(3);
    assert!(!h.state().session.active().unwrap().doc.layer(c).unwrap().effects.items.is_empty(), "Drop Shadow applied");

    // 8. Flyout Filter Options > Show enables filter bar; Panel Options changes thumbnail size to Large.
    let menu_pos = komaphone_workspace::control_rect(&h.ctx, "layers.menu").unwrap().center();
    click_at(&mut h, menu_pos, PointerButton::Primary, Modifiers::NONE);
    let panel_opts = h.get_by_label("Panel Options…").rect().center();
    click_at(&mut h, panel_opts, PointerButton::Primary, Modifiers::NONE);
    assert!(matches!(h.state().ui.layers_panel_dialog, Some(crate::state::LayersPanelDialog::PanelOptions(_))));
    if let Some(crate::state::LayersPanelDialog::PanelOptions(ref mut opts)) = h.state_mut().ui.layers_panel_dialog {
        opts.thumbnail_size = crate::state::LayerThumbnailSize::Large;
    }
    let ok_pos = h.get_by_label("OK").rect().center();
    click_at(&mut h, ok_pos, PointerButton::Primary, Modifiers::NONE);
    assert_eq!(h.state().ui.layers_panel_options.thumbnail_size, crate::state::LayerThumbnailSize::Large);
    assert!((row_of(&h.ctx, c).row.height() - 56.0).abs() < 0.5, "Large thumbnail option resizes real row height");

    // 9. Blend mode change (Multiply) changes real compositing and stays in Komaphone workspace.
    h.state_mut().run("layer.setProps", json!({"layer": c.0, "blend": "Multiply", "opacity": 0.75, "fill": 0.8})).unwrap();
    h.run_steps(2);
    let l = h.state().session.active().unwrap().doc.layer(c).unwrap();
    assert_eq!(l.blend, BlendMode::Multiply);
    assert!((l.opacity - 0.75).abs() < 0.01);
    assert!((l.fill_opacity - 0.8).abs() < 0.01);
    assert!(h.state().ui.komaphone.enabled, "never expanded workspace into Advanced mode");
}

#[test]
fn qol005_empty_locked_multi_layer_and_save_reopen_persistence() {
    // 1. No-document empty state in Komaphone workspace.
    {
        let mut h = komaphone_harness(Session::new(), vec2(1280.0, 720.0), 1.0);
        assert!(crate::layer_row_ui::recorded(&h.ctx).is_empty());
        let menu_pos = komaphone_workspace::control_rect(&h.ctx, "layers.menu").expect("flyout visible").center();
        click_at(&mut h, menu_pos, PointerButton::Primary, Modifiers::NONE);
        assert!(h.get_by_label("New Layer…").accesskit_node().is_disabled());
        assert!(h.get_by_label("Flatten Image").accesskit_node().is_disabled());
        h.key_press(Key::Escape);
        h.run_steps(2);
    }

    // 2. Durable local-store save & reopen across sessions with multi-layer state, drag order,
    //    locks, opacity, blend mode, label color, mask, visibility, and group nesting.
    #[cfg(not(target_arch = "wasm32"))]
    {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("qol005-layers.sqlite3");
        let project_id = {
            let mut s = Session::new();
            s.enable_local_projects(db.clone(), Default::default()).unwrap();
            s.execute("komaphone.project.new", json!({"name": "Layered Sketch", "width": 64, "height": 48})).unwrap();
            let bg = s.active().unwrap().doc.layers[0].id;
            let sky = LayerId(s.execute("layer.new.layer", json!({"name": "Sky"})).unwrap()["layer"].as_u64().unwrap());
            s.execute("edit.fill", json!({"color": "#3388ee"})).unwrap();
            s.execute("layer.setProps", json!({"layer": sky.0, "opacity": 0.65, "blend": "Multiply", "locks": {"position": true}})).unwrap();
            s.execute("layer.setLabelColor", json!({"layer": sky.0, "color": "blue"})).unwrap();
            s.execute("layer.layerMask.revealAll", json!({"layer": sky.0})).unwrap();
            let ink = LayerId(s.execute("layer.new.layer", json!({"name": "Ink"})).unwrap()["layer"].as_u64().unwrap());
            s.execute("edit.fill", json!({"color": "#dd3322"})).unwrap();
            s.execute("layer.setProps", json!({"layer": ink.0, "visible": false})).unwrap();
            s.execute("layer.groupLayers", json!({"name": "Lines"})).unwrap();
            // Reorder Sky above Lines so drag order is also persisted.
            let group = s.active().unwrap().active_layer.unwrap();
            s.execute("layer.moveTo", json!({"layer": sky.0, "target": group.0, "position": "above"})).unwrap();
            assert_eq!(s.active().unwrap().doc.layers.iter().map(|l| l.id).collect::<Vec<_>>(), vec![bg, group, sky]);
            s.flush_local_projects(std::time::Duration::from_secs(5)).unwrap();
            let doc_id = s.active().unwrap().doc.id;
            assert!(s.local_document_saved(doc_id), "edits durably acknowledged");
            let saved_px = composite_pixel(&s.active().unwrap().doc, 32, 24);
            let project = s.local_projects.status(doc_id).project.expect("persisted project id");
            (project, saved_px)
        };

        let mut reopened = Session::new();
        reopened.enable_local_projects(db, Default::default()).unwrap();
        reopened.flush_local_projects(std::time::Duration::from_secs(5)).unwrap();
        reopened.open_local_project(project_id.0).unwrap();
        reopened.flush_local_projects(std::time::Duration::from_secs(5)).unwrap();
        let doc = &reopened.active().unwrap().doc;
        let top_names: Vec<&str> = doc.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(top_names, ["Background", "Lines", "Sky"], "layer stack order persisted");
        let sky = doc.layers.iter().find(|l| l.name == "Sky").unwrap();
        assert!((sky.opacity - 0.65).abs() < 0.01);
        assert_eq!(sky.blend, BlendMode::Multiply);
        assert!(sky.locks.position);
        assert_eq!(sky.label, LabelColor::Blue);
        assert!(sky.mask.is_some());
        let lines = doc.layers.iter().find(|l| l.name == "Lines").unwrap();
        let LayerContent::Group(g) = &lines.content else { panic!("group persisted") };
        assert_eq!(g.children.len(), 1);
        assert_eq!(g.children[0].name, "Ink");
        assert!(!g.children[0].visible);
        let reopened_px = composite_pixel(doc, 32, 24);
        for ch in 0..4 {
            assert!((reopened_px[ch] - project_id.1[ch]).abs() < 0.01, "composited pixels survived save/reopen: {reopened_px:?} vs {:?}", project_id.1);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn qol005_party_assignment_boundaries_active_frozen_and_hidden_references() {
    // 1. Active editable Exquisite Corpse assignment (1024x512) with hidden Reference::Seam.
    let (mut s, _dir) = new_corpse_party_session(false);
    let sketch = LayerId(s.execute("layer.new.layer", json!({"name": "Band Sketch"})).unwrap()["layer"].as_u64().unwrap());
    let ink = LayerId(s.execute("layer.new.layer", json!({"name": "Band Ink"})).unwrap()["layer"].as_u64().unwrap());

    let mut h = komaphone_harness(s, vec2(1280.0, 720.0), 1.0);
    // Hidden seam reference is NOT a document layer.
    assert_eq!(h.state().session.active().unwrap().doc.layers.len(), 3);
    assert!(crate::layer_row_ui::recorded(&h.ctx).iter().all(|r| r.layer != 0));
    assert!(!h.state().session.active().unwrap().doc.layers.iter().any(|l| l.name.to_ascii_lowercase().contains("seam")));

    // Drag reorder works in active editable party assignment.
    let ink_row = row_of(&h.ctx, ink).row;
    let sketch_row = row_of(&h.ctx, sketch).row;
    drag_at(&mut h, ink_row.center(), pos2(sketch_row.center().x, sketch_row.bottom() - 2.0), Modifiers::NONE);
    assert_eq!(h.state().session.active().unwrap().doc.layers.last().unwrap().id, sketch, "reorder works in active party assignment");

    // Flyout New Artboard… and context menu Artboard from Layers… are disabled in fixed-size party assignment.
    let menu_pos = komaphone_workspace::control_rect(&h.ctx, "layers.menu").unwrap().center();
    click_at(&mut h, menu_pos, PointerButton::Primary, Modifiers::NONE);
    assert!(h.get_by_label("New Artboard…").accesskit_node().is_disabled(), "New Artboard… disabled in fixed-size party assignment");
    assert!(!h.get_by_label("New Layer…").accesskit_node().is_disabled());
    h.key_press(Key::Escape);
    h.run_steps(2);

    let sketch_row = row_of(&h.ctx, sketch).row;
    click_at(&mut h, pos2(sketch_row.left() + 80.0, sketch_row.center().y), PointerButton::Secondary, Modifiers::NONE);
    assert!(h.get_by_label("Artboard from Layers…").accesskit_node().is_disabled(), "Artboard from Layers… disabled in party assignment");
    h.key_press(Key::Escape);
    h.run_steps(2);

    // 2. Freeze assignment (`submitted = true`): all mutating UI controls, drags, context menu & flyout refuse edits.
    h.state_mut().session.rooms.status.room.as_mut().unwrap().assignment.as_mut().unwrap().submitted = true;
    h.run_steps(4);
    let frozen_before = h.state().session.active().unwrap().doc.clone();

    // Footer buttons disabled.
    for label in ["Delete layer", "Create a new layer", "Create a new group", "Create new fill or adjustment layer", "Add a layer style", "Link layers"] {
        assert!(h.get_by_label(label).accesskit_node().is_disabled(), "frozen assignment disables footer button {label}");
    }
    assert!(h.get_by_label("Lock all").accesskit_node().is_disabled(), "frozen assignment disables locks");

    // Drag reorder attempt does nothing.
    let sketch_row = row_of(&h.ctx, sketch).row;
    let ink_row = row_of(&h.ctx, ink).row;
    drag_at(&mut h, sketch_row.center(), pos2(ink_row.center().x, ink_row.bottom() - 2.0), Modifiers::NONE);
    assert_eq!(h.state().session.active().unwrap().doc.as_ref(), frozen_before.as_ref(), "drag reorder blocked when frozen");

    // Eye click attempt is refused and records status message without changing visibility.
    let sketch_eye = pos2(sketch_row.left() + 15.0, sketch_row.center().y);
    click_at(&mut h, sketch_eye, PointerButton::Primary, Modifiers::NONE);
    assert!(h.state().ui.status.contains("frozen for submission"), "status explains freeze: {}", h.state().ui.status);
    assert_eq!(h.state().session.active().unwrap().doc.as_ref(), frozen_before.as_ref());

    // Double-click rename attempt does not open rename editor.
    let name_pos = row_of(&h.ctx, sketch).name.expect("name rect").center();
    double_click_at(&mut h, name_pos);
    assert_eq!(crate::layer_row_ui::renaming(&h.ctx), None, "in-place rename blocked when frozen");

    // Flyout & context menu disable all mutating items.
    let menu_pos = komaphone_workspace::control_rect(&h.ctx, "layers.menu").unwrap().center();
    click_at(&mut h, menu_pos, PointerButton::Primary, Modifiers::NONE);
    for item in ["New Layer…", "New Group…", "Flatten Image", "Lock Layers…", "Rename Layer…", "Hide Layers", "New Artboard…"] {
        assert!(h.get_by_label(item).accesskit_node().is_disabled(), "frozen flyout disables {item}");
    }
    h.key_press(Key::Escape);
    h.run_steps(2);

    click_at(&mut h, pos2(sketch_row.left() + 80.0, sketch_row.center().y), PointerButton::Secondary, Modifiers::NONE);
    for item in ["Blending Options…", "Duplicate Layer…", "Delete Layer", "Rename Layer…", "Add Layer Mask"] {
        assert!(h.get_by_label(item).accesskit_node().is_disabled(), "frozen context menu disables {item}");
    }
    h.key_press(Key::Escape);
    h.run_steps(2);

    // Read-only selection still works so the player can inspect layers on their submitted drawing.
    let ink_row = row_of(&h.ctx, ink).row;
    click_at(&mut h, pos2(ink_row.left() + 80.0, ink_row.center().y), PointerButton::Primary, Modifiers::NONE);
    assert_eq!(h.state().session.active().unwrap().active_layer, Some(ink), "read-only selection still works when frozen");
    assert_eq!(h.state().session.active().unwrap().doc.as_ref(), frozen_before.as_ref(), "document untouched");
}

#[test]
fn qol005_shared_world_and_local_canvas_isolate_finite_layer_ui_and_permissions() {
    let me = OwnerId::new(&"01".repeat(16)).unwrap();
    let my_dev = DeviceId::new(&"02".repeat(16)).unwrap();
    let alice = OwnerId::new(&"03".repeat(16)).unwrap();
    let cid = CanvasId::new(&"10".repeat(16)).unwrap();
    let world = Arc::new(WorldState::new(cid, me, "Shared Murals".into()).unwrap());
    let home = coordinates::home(cid);

    let mut s = Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48, "background": "white"})).unwrap();
    s.execute("layer.new.layer", json!({"name": "Finite Layer"})).unwrap();
    s.worlds.account = Some(Account { endpoint: "https://komaphone.tedsoftware.org".into(), profile: me, device: my_dev });
    let summary = CanvasSummary {
        canvas: cid,
        owner: me,
        name: "Shared Murals".into(),
        initial_name: "Shared Murals".into(),
        revision: 1,
        epoch: 1,
        import_origin: None,
    };
    s.worlds.items = vec![summary.clone()];
    s.worlds.people = vec![Person { profile: me, display_name: "You".into() }, Person { profile: alice, display_name: "Alice".into() }];
    s.worlds.members = vec![Member { profile: me, owner: true }, Member { profile: alice, owner: false }];
    s.worlds.active = Some(Canvas {
        info: summary,
        canonical: Arc::clone(&world),
        display: world,
        pending: 0,
        viewer: Viewer { camera: home, bookmarks: vec![], review: None },
        notice: None,
    });
    let world_hash_before = s.worlds.active.as_ref().unwrap().canonical.canonical_hash().unwrap();

    // Open Shared World in Komaphone: finite PhotoCraft Layers panel must NOT be rendered.
    let mut h = komaphone_harness(s, vec2(1280.0, 720.0), 1.0);
    h.state_mut().ui.world.open = true;
    h.run_steps(4);
    assert!(komaphone_workspace::control_rect(&h.ctx, "layers.menu").is_none(), "finite Layers flyout must not render inside World");
    assert!(h.query_by_label("Create a new layer").is_none(), "finite Layers footer must not render inside World");
    assert_eq!(h.state().session.worlds.active.as_ref().unwrap().canonical.canonical_hash().unwrap(), world_hash_before);

    // Open Local Continuous Canvas: finite PhotoCraft Layers panel must NOT be rendered.
    h.state_mut().ui.world.open = false;
    h.state_mut().ui.canvas.open = true;
    h.run_steps(4);
    assert!(komaphone_workspace::control_rect(&h.ctx, "layers.menu").is_none(), "finite Layers flyout must not render inside Canvases");
    assert!(h.query_by_label("Create a new layer").is_none(), "finite Layers footer must not render inside Canvases");

    // Return to Workroom: finite Layers panel returns intact and WorldState stays untouched.
    assert!(komaphone_workspace::open_workroom(h.state_mut()));
    h.run_steps(4);
    assert!(komaphone_workspace::control_rect(&h.ctx, "layers.menu").is_some(), "Layers flyout returns in Workroom");
    assert!(h.query_by_label("Create a new layer").is_some(), "Layers footer returns in Workroom");
    assert_eq!(h.state().session.worlds.active.as_ref().unwrap().canonical.canonical_hash().unwrap(), world_hash_before);
}

// ------------------------------------------------------------------ Visual wgpu capture suite (opt-in via env var)

#[cfg(not(target_arch = "wasm32"))]
fn build_rich_workroom_session() -> (Session, LayerId, LayerId, LayerId) {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 320, "height": 240, "background": "white"})).unwrap();
    let sky = LayerId(s.execute("layer.new.layer", json!({"name": "Sky Wash"})).unwrap()["layer"].as_u64().unwrap());
    s.execute("select.rect", json!({"x": 20, "y": 20, "width": 220, "height": 150})).unwrap();
    s.execute("edit.fill", json!({"color": "#3a86ff"})).unwrap();
    s.execute("select.deselect", json!({})).unwrap();
    s.execute("layer.setLabelColor", json!({"layer": sky.0, "color": "blue"})).unwrap();
    s.execute("layer.layerMask.revealAll", json!({"layer": sky.0})).unwrap();

    let sun = LayerId(s.execute("layer.new.layer", json!({"name": "Warm Sun"})).unwrap()["layer"].as_u64().unwrap());
    s.execute("select.rect", json!({"x": 110, "y": 55, "width": 170, "height": 140})).unwrap();
    s.execute("edit.fill", json!({"color": "#ff595e"})).unwrap();
    s.execute("select.deselect", json!({})).unwrap();
    s.execute("layer.setProps", json!({"layer": sun.0, "blend": "Multiply", "opacity": 0.85})).unwrap();
    s.execute("layer.setLabelColor", json!({"layer": sun.0, "color": "red"})).unwrap();

    let group_child = LayerId(s.execute("layer.new.layer", json!({"name": "Ink Detail"})).unwrap()["layer"].as_u64().unwrap());
    s.execute("select.rect", json!({"x": 60, "y": 140, "width": 200, "height": 60})).unwrap();
    s.execute("edit.fill", json!({"color": "#2b2d42"})).unwrap();
    s.execute("select.deselect", json!({})).unwrap();
    s.execute("layer.groupLayers", json!({"name": "Foreground"})).unwrap();
    s.execute("layer.select", json!({"layer": sun.0})).unwrap();
    (s, sky, sun, group_child)
}

#[cfg(not(target_arch = "wasm32"))]
fn wgpu_eframe_harness(session: Session, size: Vec2, ppp: f32) -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder()
        .with_size(size)
        .with_pixels_per_point(ppp)
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(64)
        .with_accessibility_check(false)
        .wgpu()
        .build_eframe(move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, crate::theme::ThemeKind::ProMedium);
            let mut app = PhotocraftApp::new(session, Services::default());
            app.ui.komaphone.enabled = true;
            app.session.prefs.edit(|prefs| prefs.interface.language = "en".into());
            app
        });
    h.run_steps(6);
    h
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn capture_qol005_visual_evidence() {
    let Some(dir) = std::env::var_os("KOMAPHONE_QOL005_RENDERS").map(std::path::PathBuf::from) else {
        return;
    };
    let _gpu = photocraft_testkit::gpu_lock();
    std::fs::create_dir_all(&dir).unwrap();

    // 1. Normal DPI (1x) multi-layer Komaphone workroom with PhotoCraft's full Layers panel.
    {
        let (s, ..) = build_rich_workroom_session();
        let mut h = wgpu_eframe_harness(s, vec2(1280.0, 720.0), 1.0);
        h.render().unwrap().save(dir.join("01-workroom-layers-multi-1280x720-1x.png")).unwrap();
    }

    // 2. High DPI (2x) layout & crispness verification.
    {
        let (s, ..) = build_rich_workroom_session();
        let mut h = wgpu_eframe_harness(s, vec2(1280.0, 720.0), 2.0);
        h.render().unwrap().save(dir.join("02-workroom-layers-highdpi-2x.png")).unwrap();
    }

    // 3. Layer right-click context menu open in Komaphone workspace.
    {
        let (s, _, sun, _) = build_rich_workroom_session();
        let mut h = wgpu_eframe_harness(s, vec2(1280.0, 720.0), 1.0);
        let row = row_of(&h.ctx, sun).row;
        click_at(&mut h, pos2(row.left() + 80.0, row.center().y), PointerButton::Secondary, Modifiers::NONE);
        h.run_steps(15);
        h.render().unwrap().save(dir.join("03-workroom-layers-context-menu.png")).unwrap();
    }

    // 4. Layers panel flyout menu open in Komaphone workspace.
    {
        let (s, ..) = build_rich_workroom_session();
        let mut h = wgpu_eframe_harness(s, vec2(1280.0, 720.0), 1.0);
        let menu_pos = komaphone_workspace::control_rect(&h.ctx, "layers.menu").unwrap().center();
        click_at(&mut h, menu_pos, PointerButton::Primary, Modifiers::NONE);
        h.run_steps(15);
        h.render().unwrap().save(dir.join("04-workroom-layers-flyout-menu.png")).unwrap();
    }

    // 5. Locked lone Background state.
    {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 320, "height": 240, "background": "white"})).unwrap();
        let mut h = wgpu_eframe_harness(s, vec2(1280.0, 720.0), 1.0);
        h.render().unwrap().save(dir.join("05-workroom-layers-locked-bg.png")).unwrap();
    }

    // 6. Frozen Exquisite Corpse party assignment showing restriction notice & disabled controls.
    {
        let (mut s, _dir) = new_corpse_party_session(false);
        s.execute("layer.new.layer", json!({"name": "Torso Sketch"})).unwrap();
        s.execute("select.rect", json!({"x": 320, "y": 80, "width": 380, "height": 340})).unwrap();
        s.execute("edit.fill", json!({"color": "#457b9d"})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        s.rooms.status.room.as_mut().unwrap().assignment.as_mut().unwrap().submitted = true;
        let mut h = wgpu_eframe_harness(s, vec2(1280.0, 720.0), 1.0);
        h.render().unwrap().save(dir.join("06-workroom-layers-party-frozen.png")).unwrap();
    }

    // 7. After drag-reorder (Sun moved below Sky) + Adjustment Layer with on-demand Layer Properties window.
    {
        let (s, sky, sun, _) = build_rich_workroom_session();
        let mut h = wgpu_eframe_harness(s, vec2(1280.0, 720.0), 1.0);
        let sun_row = row_of(&h.ctx, sun).row;
        let sky_row = row_of(&h.ctx, sky).row;
        drag_at(&mut h, sun_row.center(), pos2(sky_row.center().x, sky_row.bottom() - 2.0), Modifiers::NONE);
        let adj_pos = h.get_by_label("Create new fill or adjustment layer").rect().center();
        click_at(&mut h, adj_pos, PointerButton::Primary, Modifiers::NONE);
        let bc_pos = h.get_by_label("Brightness/Contrast").rect().center();
        click_at(&mut h, bc_pos, PointerButton::Primary, Modifiers::NONE);
        h.run_steps(15);
        h.render().unwrap().save(dir.join("07-workroom-layers-reordered-and-properties.png")).unwrap();
    }
}
