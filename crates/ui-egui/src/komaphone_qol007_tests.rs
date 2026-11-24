//! QOL-007 UI & visual verification: bottom-edge drag handle, accessible left-panel numeric
//! height control, non-destructive shrink bound + explicit undoable crop, top-seam anchoring,
//! QOL-004 live dimensions update, post-submit lock, and variable-height sequential reveal
//! (preserving QOL-002 reset on creature switch).
use crate::{PhotocraftApp, Services, komaphone_game_view, komaphone_workspace};
use egui::{Event, PointerButton, Pos2, RawInput, Rect, Vec2, pos2, vec2};
use photocraft_engine::party::room::{Assignment, AssignmentView, MemberView, Phase, Reference, RevealedEntry, RoomView};
use photocraft_engine::party::{
    AssetHash, AssetRef, AssignmentId, CheckpointId, CheckpointRef, DeviceId, GameId, ImageExtent, Mode, Participant, ParticipantId, ProjectId, ReplayId,
    ReplayRef, RoomId,
};
use photocraft_engine::rooms::corpse::{BAND_HEIGHT, SEAM_HEIGHT, WIDTH};
use photocraft_engine::rooms::media::RoomImage;
use serde_json::json;
use std::sync::Arc;

fn participant(seed: u8, name: &str) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{seed:032x}")).unwrap(),
        device: DeviceId::new(&format!("{:032x}", u16::from(seed) + 100)).unwrap(),
        display_name: name.into(),
    }
}

fn asset(seed: u16, bytes: u64) -> AssetRef {
    AssetRef { hash: AssetHash::new(&format!("{seed:064x}")).unwrap(), bytes }
}

fn collect_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
    fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
            egui::Shape::Vec(v) => {
                for s in v {
                    walk(s, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for s in shapes {
        walk(&s.shape, &mut out);
    }
    out
}

fn synthetic_seam_strip() -> RoomImage {
    let w = WIDTH as usize;
    let h = SEAM_HEIGHT as usize;
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) * 4;
            let stripe = (x / 64) % 2 == 0;
            rgba[idx] = if stripe { 226 } else { 58 };
            rgba[idx + 1] = if y > 14 { 92 } else { 154 };
            rgba[idx + 2] = if stripe { 84 } else { 208 };
            rgba[idx + 3] = 255;
        }
    }
    RoomImage { width: WIDTH, height: SEAM_HEIGHT, rgba }
}

fn synthetic_corpse_part(height: u32, bg: [u8; 3], accent: [u8; 3]) -> RoomImage {
    let w = WIDTH as usize;
    let h = height as usize;
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) * 4;
            let edge = y < 12 || y >= h.saturating_sub(12);
            let neck = (x as i32 - 512).abs() < 110;
            let color = if neck && (edge || y % 48 < 24) { accent } else { bg };
            rgba[idx] = color[0];
            rgba[idx + 1] = color[1];
            rgba[idx + 2] = color[2];
            rgba[idx + 3] = 255;
        }
    }
    RoomImage { width: WIDTH, height, rgba }
}

#[cfg(not(target_arch = "wasm32"))]
fn active_corpse_app() -> (tempfile::TempDir, PhotocraftApp) {
    let dir = tempfile::tempdir().unwrap();
    let alice = participant(1, "Alice");
    let bob = participant(2, "Bob");
    let room_id = RoomId::new(&"71".repeat(16)).unwrap();
    let game_id = GameId::new(&"81".repeat(16)).unwrap();
    let project = ProjectId::new(&"91".repeat(16)).unwrap();
    let seam_ref = asset(42, 1024);

    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
    app.ui.komaphone.enabled = true;
    app.session.enable_local_projects(dir.path().join("drafts.sqlite3"), Default::default()).unwrap();
    app.session.flush_local_projects(std::time::Duration::from_secs(20)).unwrap();
    app.session.rooms.status.connected = true;
    app.session.rooms.status.paired = true;
    app.session.rooms.status.polling = true;
    app.session.rooms.status.profile = Some(alice.clone());
    app.session.rooms.status.room = Some(RoomView {
        id: room_id,
        revision: 4,
        epoch: 1,
        phase: Phase::Drawing,
        round: 1,
        mode: Mode::ExquisiteCorpse,
        timer_ms: None,
        moderator: alice.id,
        members: vec![MemberView { participant: alice.clone(), connected: true, ready: true }, MemberView { participant: bob, connected: true, ready: true }],
        waiting: vec![],
        paused: None,
        deadline_ms: None,
        assignment: Some(AssignmentView {
            assignment: Assignment { id: AssignmentId { game: game_id, chain: 1, round: 1 }, player: alice.id, project },
            submitted: false,
            reference: Some(Reference::Seam { image: seam_ref, width: WIDTH, height: SEAM_HEIGHT }),
        }),
        reveal: vec![],
    });
    app.session.open_room_draft().unwrap();
    app.session.flush_local_projects(std::time::Duration::from_secs(20)).unwrap();
    app.session.rooms.media.reconcile(&app.session.rooms.status);
    app.session.rooms.media.images.insert(seam_ref.hash, Arc::new(synthetic_seam_strip()));
    (dir, app)
}

fn run_workspace_frame(app: &mut PhotocraftApp, ctx: &egui::Context, events: Vec<Event>, time: f64) -> Vec<String> {
    let mut out = ctx.run_ui(
        RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 720.0))), time: Some(time), events, ..Default::default() },
        |ui| {
            komaphone_game_view::sync(app, ui.ctx());
            komaphone_workspace::chrome(app, ui);
            komaphone_game_view::reference_panel(app, ui);
            egui::CentralPanel::default().show(ui, |ui| crate::canvas::document_area(app, ui));
        },
    );
    let texts = collect_texts(&out.shapes);
    out.textures_delta.clear();
    texts
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn corpse_bottom_drag_handle_expands_stops_at_artwork_anchors_top_seam_and_supports_explicit_crop() {
    let (_dir, mut app) = active_corpse_app();
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, app.ui.theme);

    let mut t = 0.0;
    let mut texts = Vec::new();
    for _ in 0..4 {
        t += 0.05;
        texts = run_workspace_frame(&mut app, &ctx, vec![], t);
    }
    assert_eq!(app.session.active().unwrap().doc.size.height, BAND_HEIGHT);
    assert!(texts.iter().any(|s| s == "1024 px x 512 px (72 ppi)"), "QOL-004 default corpse dimensions: {texts:?}");

    let handle_rect = komaphone_workspace::control_rect(&ctx, "corpse.height.handle").expect("bottom-edge handle rect registered");
    let before_view = app.ui.views.first().cloned().expect("active canvas view");
    let before_history_len = app.session.active().unwrap().history.entries().len();

    // Drag the bottom-edge handle downward to expand canvas height without painting a stray brush stroke.
    let start = handle_rect.center();
    let target_down = pos2(start.x, start.y + 80.0);
    t += 0.05;
    run_workspace_frame(&mut app, &ctx, vec![Event::PointerMoved(start)], t);
    t += 0.05;
    run_workspace_frame(
        &mut app,
        &ctx,
        vec![Event::PointerButton { pos: start, button: PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }],
        t,
    );
    assert!(app.ui.rooms.corpse_view.height_drag.is_some(), "pressing bottom handle starts height drag");
    assert!(app.drag.is_none(), "bottom handle press must not start a canvas paint stroke");
    t += 0.05;
    let mid_texts = run_workspace_frame(&mut app, &ctx, vec![Event::PointerMoved(target_down)], t);
    let preview_h = app.ui.rooms.corpse_view.height_drag.as_ref().unwrap().preview_height;
    assert!(preview_h > BAND_HEIGHT, "dragging down expands preview height (got {preview_h})");
    assert!(mid_texts.iter().any(|s| s == "Drawing · Unsaved"), "active drag reports unsaved work: {mid_texts:?}");
    t += 0.05;
    run_workspace_frame(
        &mut app,
        &ctx,
        vec![Event::PointerButton { pos: target_down, button: PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }],
        t,
    );
    t += 0.05;
    let after_texts = run_workspace_frame(&mut app, &ctx, vec![], t);

    let expanded_h = app.session.active().unwrap().doc.size.height;
    assert_eq!(expanded_h, preview_h);
    assert!(expanded_h > BAND_HEIGHT);
    assert_eq!(app.session.active().unwrap().doc.size.width, WIDTH);
    assert_eq!(app.session.active().unwrap().history.entries().len(), before_history_len + 1, "one clean resize history entry, no stray brush stroke");
    let after_view = app.ui.views.first().cloned().unwrap();
    assert!(
        (after_view.center[1] - before_view.center[1]).abs() < 0.01 && (after_view.center[0] - before_view.center[0]).abs() < 0.01,
        "top seam camera anchor must not shift on segment height change: before={:?} after={:?}",
        before_view.center,
        after_view.center
    );
    let expected_dim = format!("1024 px x {expanded_h} px (72 ppi)");
    assert!(after_texts.iter().any(|s| s == &expected_dim), "QOL-004 dimensions updated to {expected_dim}: {after_texts:?}");

    // Paint artwork reaching y ~ 430 so dragging up past artwork stops non-destructively.
    app.run("paint.stroke", json!({"points": [[240, 220], [520, 420]], "size": 24, "color": [0.1, 0.25, 0.7, 1.0], "seed": 5})).unwrap();
    let safe_min = photocraft_engine::rooms::corpse::safe_min_height(&app.session.active().unwrap().doc);
    assert!(safe_min >= 420 && safe_min < expanded_h, "safe_min={safe_min}, expanded_h={expanded_h}");
    t += 0.05;
    run_workspace_frame(&mut app, &ctx, vec![], t);
    let handle2 = komaphone_workspace::control_rect(&ctx, "corpse.height.handle").unwrap();
    let start2 = handle2.center();
    let way_up = pos2(start2.x, start2.y - 320.0);
    t += 0.05;
    run_workspace_frame(
        &mut app,
        &ctx,
        vec![
            Event::PointerMoved(start2),
            Event::PointerButton { pos: start2, button: PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
        ],
        t,
    );
    t += 0.05;
    run_workspace_frame(&mut app, &ctx, vec![Event::PointerMoved(way_up)], t);
    let drag_state = app.ui.rooms.corpse_view.height_drag.as_ref().unwrap();
    assert!(drag_state.clamped_by_artwork, "shrinking drag past painted marks clamps at artwork bound");
    assert_eq!(drag_state.preview_height, safe_min);
    t += 0.05;
    run_workspace_frame(
        &mut app,
        &ctx,
        vec![Event::PointerButton { pos: way_up, button: PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }],
        t,
    );
    t += 0.05;
    run_workspace_frame(&mut app, &ctx, vec![], t);
    assert_eq!(app.session.active().unwrap().doc.size.height, safe_min, "non-destructive shrink stopped exactly at safe_min");
    assert!(app.ui.rooms.corpse_view.height_notice.is_some(), "bound explanation shown after release");

    // Numeric control explicit crop flow: request 240 px -> confirmation appears without mutating doc -> confirm crop -> undo restores!
    app.ui.rooms.corpse_view.draft_height = 240;
    t += 0.05;
    run_workspace_frame(&mut app, &ctx, vec![], t);
    let apply_rect = komaphone_workspace::control_rect(&ctx, "corpse.height.apply").expect("apply button rect");
    let click = |app: &mut PhotocraftApp, t: &mut f64, rect: Rect| {
        *t += 0.05;
        let p = rect.center();
        run_workspace_frame(
            app,
            &ctx,
            vec![
                Event::PointerMoved(p),
                Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
                Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE },
            ],
            *t,
        );
        *t += 0.05;
        run_workspace_frame(app, &ctx, vec![], *t)
    };
    let confirm_texts = click(&mut app, &mut t, apply_rect);
    assert_eq!(app.session.active().unwrap().doc.size.height, safe_min, "clicking Apply below artwork does NOT silently crop");
    assert_eq!(app.ui.rooms.corpse_view.pending_crop_height, Some(240));
    assert!(confirm_texts.iter().any(|s| s == "Confirm crop"), "explicit crop prompt visible: {confirm_texts:?}");
    let crop_rect = komaphone_workspace::control_rect(&ctx, "corpse.height.confirmCrop").expect("confirm crop rect");
    let cropped_texts = click(&mut app, &mut t, crop_rect);
    assert_eq!(app.session.active().unwrap().doc.size.height, 240, "confirmed crop resized to 240 px");
    assert!(cropped_texts.iter().any(|s| s == "1024 px x 240 px (72 ppi)"));

    // Undo restores exact prior height and painted artwork cleanly.
    app.run("edit.undo", json!({})).unwrap();
    t += 0.05;
    let undone_texts = run_workspace_frame(&mut app, &ctx, vec![], t);
    assert_eq!(app.session.active().unwrap().doc.size.height, safe_min);
    assert_eq!(photocraft_engine::rooms::corpse::safe_min_height(&app.session.active().unwrap().doc), safe_min);
    assert!(undone_texts.iter().any(|s| s == &format!("1024 px x {safe_min} px (72 ppi)")));

    // Submitted segment locks handle and disables numeric resize command.
    app.session.rooms.status.room.as_mut().unwrap().assignment.as_mut().unwrap().submitted = true;
    t += 0.05;
    run_workspace_frame(&mut app, &ctx, vec![], t);
    assert!(komaphone_workspace::control_rect(&ctx, "corpse.height.handle").is_none(), "submitted segment hides bottom drag handle");
    assert!(!app.session.is_enabled("komaphone.corpse.setSegmentHeight"));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn variable_height_corpse_reveal_displays_exact_dimensions_and_preserves_qol002_reset() {
    use egui_kittest::kittest::Queryable;

    let alice = participant(1, "Alice");
    let bob = participant(2, "Bob");
    let cara = participant(3, "Cara");
    let members =
        [alice.clone(), bob.clone(), cara.clone()].into_iter().map(|p| MemberView { participant: p, connected: true, ready: true }).collect::<Vec<_>>();
    let game_id = GameId::new(&"c7".repeat(16)).unwrap();
    let heights = [[192u32, 512, 1024], [768, 256, 640], [512, 1280, 192]];
    let palette = [([236, 212, 200], [186, 72, 62]), ([206, 228, 214], [52, 134, 88]), ([208, 216, 238], [62, 86, 178])];
    let mut reveal = Vec::new();
    let mut images = Vec::new();
    for chain in 0..3u8 {
        for round in 0..3u8 {
            let h = heights[usize::from(chain)][usize::from(round)];
            let project = ProjectId::new(&format!("{:032x}", 300 + u16::from(chain) * 10 + u16::from(round))).unwrap();
            let img_ref = asset(400 + u16::from(chain) * 10 + u16::from(round), 2048);
            let cp_ref = asset(500 + u16::from(chain) * 10 + u16::from(round), 4096);
            let ckpt = CheckpointRef { id: CheckpointId::new(project.as_str()).unwrap(), asset: cp_ref };
            let author = members[(usize::from(chain) + usize::from(round)) % 3].participant.id;
            reveal.push(RevealedEntry::Artwork {
                assignment: Assignment { id: AssignmentId { game: game_id, chain, round }, player: author, project },
                revision: 1,
                image: img_ref,
                checkpoint: ckpt,
                replay: Box::new(ReplayRef {
                    id: ReplayId::new(project.as_str()).unwrap(),
                    manifest: None,
                    baseline: ckpt,
                    end_event: None,
                    event_count: 0,
                    duration_ms: 0,
                }),
                assets: vec![img_ref, cp_ref],
                extent: Some(ImageExtent { width: WIDTH, height: h }),
            });
            let (bg, acc) = palette[usize::from(round)];
            images.push((img_ref.hash, Arc::new(synthetic_corpse_part(h, bg, acc))));
        }
    }
    let room = RoomView {
        id: RoomId::new(&"d7".repeat(16)).unwrap(),
        revision: 30,
        epoch: 4,
        phase: Phase::Reveal,
        round: 2,
        mode: Mode::ExquisiteCorpse,
        timer_ms: None,
        moderator: alice.id,
        members,
        waiting: vec![],
        paused: None,
        deadline_ms: None,
        assignment: None,
        reveal,
    };

    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
    app.ui.komaphone.enabled = true;
    app.session.rooms.status.profile = Some(alice);
    app.session.rooms.status.room = Some(room);
    app.session.rooms.media.reconcile(&app.session.rooms.status);
    for (hash, img) in images {
        app.session.rooms.media.images.insert(hash, img);
    }

    let mut h = egui_kittest::Harness::builder().with_size(vec2(1280.0, 720.0)).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            komaphone_game_view::sync(app, ui.ctx());
            egui::CentralPanel::default().show(ui, |ui| komaphone_game_view::reveal(app, ui));
        },
        app,
    );
    h.run_steps(3);
    assert_eq!((h.state().ui.rooms.corpse_view.chain, h.state().ui.rooms.corpse_view.visible_parts), (0, 1));
    assert!(h.query_by_label("Head · Alice").is_some());
    assert!(h.query_by_label("1024 × 1728 (192 + 512 + 1024) · credits stay outside artwork").is_some());

    h.get_by_label("Full creature").click();
    h.run_steps(2);
    assert_eq!(h.state().ui.rooms.corpse_view.visible_parts, 3);
    assert!(h.query_by_label("Torso and arms · Bob").is_some());
    assert!(h.query_by_label("Legs and feet · Cara").is_some());

    // QOL-002 recheck: switching to Creature 2 resets sequential reveal back to Head and updates total height.
    h.get_by_label("Creature 2").click();
    h.run_steps(2);
    assert_eq!((h.state().ui.rooms.corpse_view.chain, h.state().ui.rooms.corpse_view.visible_parts), (1, 1));
    assert!(h.query_by_label("Head · Bob").is_some());
    assert!(h.query_by_label("Torso and arms · Cara").is_none());
    assert!(h.query_by_label("1024 × 1664 (768 + 256 + 640) · credits stay outside artwork").is_some());
}

#[cfg(not(target_arch = "wasm32"))]
#[inline(never)]
fn save_qol007_shot(out_dir: &std::path::Path, name: &str, size: Vec2, ppp: f32, app: PhotocraftApp, draw: fn(&mut egui::Ui, &mut PhotocraftApp)) {
    let mut styled = false;
    let mut h = egui_kittest::Harness::builder().with_size(size).with_pixels_per_point(ppp).wgpu().build_ui_state(
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
    h.run_steps(6);
    h.render().unwrap().save(out_dir.join(name)).unwrap();
}

#[cfg(not(target_arch = "wasm32"))]
fn draw_qol007_editor(ui: &mut egui::Ui, app: &mut PhotocraftApp) {
    komaphone_game_view::sync(app, ui.ctx());
    komaphone_workspace::chrome(app, ui);
    komaphone_game_view::reference_panel(app, ui);
    egui::CentralPanel::default().show(ui, |ui| crate::canvas::document_area(app, ui));
}

#[cfg(not(target_arch = "wasm32"))]
#[inline(never)]
fn capture_qol007_shot_1(out_dir: &std::path::Path) {
    let (_dir, mut app) = active_corpse_app();
    app.run("paint.stroke", json!({"points": [[360, 12], [380, 180], [640, 180], [660, 12]], "size": 24, "color": [0.16, 0.34, 0.62, 1.0], "seed": 1}))
        .unwrap();
    save_qol007_shot(out_dir, "01-corpse-default-512-with-seam-and-handle.png", vec2(1280.0, 720.0), 1.0, app, draw_qol007_editor);
}

#[cfg(not(target_arch = "wasm32"))]
#[inline(never)]
fn capture_qol007_shot_2(out_dir: &std::path::Path) {
    let (_dir, mut app) = active_corpse_app();
    app.run("paint.stroke", json!({"points": [[360, 12], [440, 320], [620, 260]], "size": 24, "color": [0.16, 0.34, 0.62, 1.0], "seed": 2})).unwrap();
    app.session.flush_local_projects(std::time::Duration::from_secs(20)).unwrap();
    let mut styled = false;
    let mut h = egui_kittest::Harness::builder().with_size(vec2(1280.0, 720.0)).with_pixels_per_point(1.0).wgpu().build_ui_state(
        move |ui, app: &mut PhotocraftApp| {
            if !styled {
                PhotocraftApp::setup_context(ui.ctx(), crate::theme::ThemeKind::ProMedium);
                styled = true;
                return;
            }
            draw_qol007_editor(ui, app);
        },
        app,
    );
    h.run_steps(6);
    let handle = komaphone_workspace::control_rect(&h.ctx, "corpse.height.handle").expect("handle rect");
    let start = handle.center();
    let dragged = pos2(start.x, start.y + 96.0);
    h.input_mut().events.push(Event::PointerMoved(start));
    h.step();
    h.input_mut().events.push(Event::PointerButton { pos: start, button: PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    h.input_mut().events.push(Event::PointerMoved(dragged));
    h.step();
    assert!(h.state().ui.rooms.corpse_view.height_drag.is_some(), "mid-drag preview must remain active while primary button is held");
    h.render().unwrap().save(out_dir.join("02-corpse-dragging-preview.png")).unwrap();
}

#[cfg(not(target_arch = "wasm32"))]
#[inline(never)]
fn capture_qol007_shot_3(out_dir: &std::path::Path) {
    let (_dir, mut app) = active_corpse_app();
    app.run("paint.stroke", json!({"points": [[360, 12], [440, 320], [620, 260]], "size": 24, "color": [0.16, 0.34, 0.62, 1.0], "seed": 3})).unwrap();
    app.run("komaphone.corpse.setSegmentHeight", json!({"height": 832})).unwrap();
    app.run("paint.stroke", json!({"points": [[420, 420], [512, 780], [610, 420]], "size": 26, "color": [0.78, 0.28, 0.22, 1.0], "seed": 4})).unwrap();
    save_qol007_shot(out_dir, "03-corpse-expanded-832-anchored-seam.png", vec2(1280.0, 720.0), 1.0, app, draw_qol007_editor);
}

#[cfg(not(target_arch = "wasm32"))]
#[inline(never)]
fn capture_qol007_shot_4(out_dir: &std::path::Path) {
    let (_dir, mut app) = active_corpse_app();
    app.run("paint.stroke", json!({"points": [[360, 80], [520, 440], [680, 120]], "size": 26, "color": [0.18, 0.45, 0.32, 1.0], "seed": 5})).unwrap();
    let doc_id = app.session.active().unwrap().doc.id;
    app.ui.rooms.corpse_view.draft_height_sync = Some((doc_id, 512));
    app.ui.rooms.corpse_view.draft_height = 256;
    app.ui.rooms.corpse_view.pending_crop_height = Some(256);
    save_qol007_shot(out_dir, "04-corpse-crop-confirmation.png", vec2(1280.0, 720.0), 1.0, app, draw_qol007_editor);
}

#[cfg(not(target_arch = "wasm32"))]
#[inline(never)]
fn capture_qol007_shot_5(out_dir: &std::path::Path) {
    let alice = participant(1, "Alice");
    let bob = participant(2, "Bob");
    let cara = participant(3, "Cara");
    let members = [alice.clone(), bob.clone(), cara].into_iter().map(|p| MemberView { participant: p, connected: true, ready: true }).collect::<Vec<_>>();
    let game_id = GameId::new(&"e7".repeat(16)).unwrap();
    let heights = [192u32, 512, 1024];
    let palette = [([236, 212, 200], [186, 72, 62]), ([206, 228, 214], [52, 134, 88]), ([208, 216, 238], [62, 86, 178])];
    let mut reveal = Vec::new();
    let mut reveal_images = Vec::new();
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
    app.ui.komaphone.enabled = true;
    for chain in 0..3u8 {
        for round in 0..3u8 {
            let h = heights[usize::from(round)];
            let project = ProjectId::new(&format!("{:032x}", 600 + u16::from(chain) * 10 + u16::from(round))).unwrap();
            let img_ref = asset(700 + u16::from(chain) * 10 + u16::from(round), 2048);
            let cp_ref = asset(800 + u16::from(chain) * 10 + u16::from(round), 4096);
            let ckpt = CheckpointRef { id: CheckpointId::new(project.as_str()).unwrap(), asset: cp_ref };
            let author = members[(usize::from(chain) + usize::from(round)) % 3].participant.id;
            reveal.push(RevealedEntry::Artwork {
                assignment: Assignment { id: AssignmentId { game: game_id, chain, round }, player: author, project },
                revision: 1,
                image: img_ref,
                checkpoint: ckpt,
                replay: Box::new(ReplayRef {
                    id: ReplayId::new(project.as_str()).unwrap(),
                    manifest: None,
                    baseline: ckpt,
                    end_event: None,
                    event_count: 0,
                    duration_ms: 0,
                }),
                assets: vec![img_ref, cp_ref],
                extent: Some(ImageExtent { width: WIDTH, height: h }),
            });
            let (bg, acc) = palette[usize::from(round)];
            reveal_images.push((img_ref.hash, Arc::new(synthetic_corpse_part(h, bg, acc))));
        }
    }
    app.session.rooms.status.profile = Some(alice.clone());
    app.session.rooms.status.room = Some(RoomView {
        id: RoomId::new(&"f7".repeat(16)).unwrap(),
        revision: 35,
        epoch: 5,
        phase: Phase::Reveal,
        round: 2,
        mode: Mode::ExquisiteCorpse,
        timer_ms: None,
        moderator: alice.id,
        members,
        waiting: vec![],
        paused: None,
        deadline_ms: None,
        assignment: None,
        reveal,
    });
    app.session.rooms.media.reconcile(&app.session.rooms.status);
    for (hash, img) in reveal_images {
        app.session.rooms.media.images.insert(hash, img);
    }
    app.ui.rooms.view_reveal = true;
    app.ui.rooms.restored_reveal = Some(game_id);
    app.ui.rooms.corpse_view.game = Some(game_id);
    app.ui.rooms.corpse_view.visible_parts = 3;
    save_qol007_shot(out_dir, "05-corpse-variable-height-reveal-full.png", vec2(1280.0, 720.0), 1.0, app, |ui, app| {
        komaphone_game_view::sync(app, ui.ctx());
        komaphone_workspace::chrome(app, ui);
        egui::CentralPanel::default().show(ui, |ui| komaphone_game_view::reveal(app, ui));
    });
}

#[cfg(not(target_arch = "wasm32"))]
#[inline(never)]
fn capture_qol007_shot_6(out_dir: &std::path::Path) {
    let (_dir, mut app) = active_corpse_app();
    app.run("paint.stroke", json!({"points": [[360, 12], [440, 260], [640, 180]], "size": 24, "color": [0.16, 0.34, 0.62, 1.0], "seed": 6})).unwrap();
    save_qol007_shot(out_dir, "06-corpse-hidpi-2x.png", vec2(1280.0, 720.0), 2.0, app, draw_qol007_editor);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "real wgpu visual capture; run with KOMAPHONE_QOL007_SHOTS=<dir>"]
fn capture_qol007_wgpu_visuals() {
    let Ok(out_dir) = std::env::var("KOMAPHONE_QOL007_SHOTS") else {
        return;
    };
    let out_dir = std::path::PathBuf::from(out_dir);
    let _gpu = photocraft_testkit::gpu_lock();
    std::fs::create_dir_all(&out_dir).unwrap();
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            capture_qol007_shot_1(&out_dir);
            capture_qol007_shot_2(&out_dir);
            capture_qol007_shot_3(&out_dir);
            capture_qol007_shot_4(&out_dir);
            capture_qol007_shot_5(&out_dir);
            capture_qol007_shot_6(&out_dir);
        })
        .unwrap()
        .join()
        .unwrap();
}
