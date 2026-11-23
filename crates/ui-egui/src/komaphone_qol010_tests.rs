//! QOL-010 Gallery managed-export UI, prefill/validation/reveal, and visual tests.
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{Pos2, Rect, Vec2};
use photocraft_engine::Session;
use photocraft_engine::gallery::{GalleryItem, LastExport};
use photocraft_engine::gallery_media::{ReplayInfo, ReplayMoment};
use photocraft_engine::party::gallery::GalleryManifest;
use photocraft_engine::party::room::{Assignment, MemberView, Phase, RevealedEntry, RoomView};
use photocraft_engine::party::{
    AssetHash, AssetRef, AssignmentId, CheckpointId, CheckpointRef, DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, ReplayId, ReplayRef, RoomId,
};
use photocraft_engine::rooms::media::RoomImage;
use serde_json::json;

use crate::komaphone_gallery::{self, GallerySection};
use crate::{PhotocraftApp, Services, komaphone_workspace};

fn project_id(seed: u64) -> ProjectId {
    ProjectId::new(&format!("{seed:032x}")).unwrap()
}

fn asset_ref(seed: u64) -> AssetRef {
    AssetRef { hash: AssetHash::new(&format!("{seed:064x}")).unwrap(), bytes: 128 }
}

fn participant(seed: u64, name: &str) -> Participant {
    Participant { id: ParticipantId::new(&format!("{seed:032x}")).unwrap(), device: DeviceId::new(&format!("{seed:032x}")).unwrap(), display_name: name.into() }
}

fn collect_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
    fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) => out.push(t.galley.text().into()),
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

fn wait_gallery(app: &mut PhotocraftApp) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while app.session.gallery.busy() {
        assert!(std::time::Instant::now() < deadline, "gallery timed out");
        app.session.poll_gallery();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn paint_shell(app: &mut PhotocraftApp, ctx: &egui::Context, now: f64, events: Vec<egui::Event>) -> Vec<String> {
    app.session.poll_local_projects();
    app.session.poll_gallery();
    let mut out = ctx.run_ui(
        egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 720.0))), time: Some(now), events, ..Default::default() },
        |ui| {
            komaphone_workspace::chrome(app, ui);
            let t = crate::theme::Tokens::get(ctx);
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ui, |ui| {
                if app.ui.gallery.open {
                    komaphone_gallery::show(app, ui);
                } else if app.session.documents().is_empty() {
                    komaphone_workspace::home(app, ui);
                } else {
                    crate::canvas::document_area(app, ui);
                }
            });
        },
    );
    let texts = collect_texts(&out.shapes);
    out.textures_delta.clear();
    texts
}

fn click_control(app: &mut PhotocraftApp, ctx: &egui::Context, name: &str, t: &mut f64) -> Vec<String> {
    wait_gallery(app);
    paint_shell(app, ctx, *t, vec![]);
    wait_gallery(app);
    *t += 0.05;
    let rect = komaphone_workspace::control_rect(ctx, name).unwrap_or_else(|| panic!("missing control {name}"));
    let pos = rect.center();
    paint_shell(
        app,
        ctx,
        *t,
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() },
        ],
    );
    *t += 0.05;
    paint_shell(app, ctx, *t, vec![egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() }]);
    wait_gallery(app);
    *t += 0.05;
    let texts = paint_shell(app, ctx, *t, vec![]);
    *t += 0.05;
    texts
}

fn make_game_manifest(seed: u64, title: &str, mode: Mode) -> GalleryManifest {
    let p1 = participant(seed * 10 + 1, "Alice");
    let p2 = participant(seed * 10 + 2, "Bob");
    let rounds = mode.rounds(2).unwrap();
    let game_id = GameId::new(&format!("{seed:032x}")).unwrap();
    let mut reveal = Vec::new();
    for chain in 0..2u8 {
        for round in 0..rounds {
            let proj = project_id(seed * 100 + u64::from(chain) * u64::from(rounds) + u64::from(round) + 1);
            let cp = CheckpointRef { id: CheckpointId::new(proj.as_str()).unwrap(), asset: asset_ref(seed * 100 + 2) };
            reveal.push(RevealedEntry::Artwork {
                assignment: Assignment {
                    id: AssignmentId { game: game_id, chain, round },
                    player: if (chain + round) % 2 == 0 { p1.id } else { p2.id },
                    project: proj,
                },
                revision: 2,
                image: asset_ref(seed * 100 + 1),
                checkpoint: cp,
                replay: Box::new(ReplayRef {
                    id: ReplayId::new(proj.as_str()).unwrap(),
                    manifest: None,
                    baseline: cp,
                    end_event: None,
                    event_count: 0,
                    duration_ms: 0,
                }),
                assets: vec![asset_ref(seed * 100 + 1), asset_ref(seed * 100 + 2)],
            });
        }
    }
    let room = RoomView {
        id: RoomId::new(&format!("{:032x}", seed + 5000)).unwrap(),
        revision: 12,
        epoch: 1,
        phase: Phase::Reveal,
        round: rounds,
        mode,
        timer_ms: None,
        moderator: p1.id,
        members: vec![MemberView { participant: p1.clone(), connected: true, ready: true }, MemberView { participant: p2, connected: true, ready: true }],
        waiting: vec![],
        paused: None,
        deadline_ms: None,
        assignment: None,
        reveal,
    };
    let mut manifest = GalleryManifest::from_reveal(ProjectId::new(game_id.as_str()).unwrap(), &room, p1.id, "2026-11-22T12:00:00Z".into()).unwrap();
    manifest.title = title.into();
    manifest
}

fn demo_still_image(w: u32, h: u32, bg: [u8; 4], fg: [u8; 4]) -> Arc<RoomImage> {
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            let dx = x as i32 - (w as i32 / 2);
            let dy = y as i32 - (h as i32 / 2);
            let ring = (dx * dx + dy * dy) < ((h as i32 / 3) * (h as i32 / 3));
            let c = if ring { fg } else { bg };
            rgba[idx..idx + 4].copy_from_slice(&c);
        }
    }
    Arc::new(RoomImage { width: w, height: h, rgba })
}

fn gallery_item_for(manifest: &GalleryManifest, kind: &str, authors: Vec<&str>) -> GalleryItem {
    GalleryItem {
        id: manifest.id,
        title: manifest.title.clone(),
        kind: kind.into(),
        authors: authors.into_iter().map(str::to_owned).collect(),
        updated_at: manifest.updated_at.clone(),
        favorite: false,
        trashed: false,
        complete: true,
        draft: false,
        revision: 1,
        state: "Completed game".into(),
        source_projects: vec![manifest.id],
        saved_matches_head: false,
        has_saved_copy: false,
        has_unsaved_edits: false,
        untouched_blank: false,
        covered_by_game: false,
    }
}

#[test]
fn gallery_export_ui_prefills_validates_dispatches_and_reveals_without_folder_picker() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let exports_dir = dir.path().join("Exports");
    std::fs::create_dir_all(&data_dir).unwrap();

    let revealed: Arc<Mutex<Vec<(String, bool)>>> = Arc::new(Mutex::new(Vec::new()));
    let revealed_clone = revealed.clone();
    let services = Services {
        reveal_in_folder: Some(Box::new(move |path: &str, is_dir: bool| {
            revealed_clone.lock().unwrap().push((path.to_owned(), is_dir));
            Ok(())
        })),
        ..Default::default()
    };

    let mut session = Session::new();
    session.enable_local_projects_with_exports(data_dir.join("drafts.sqlite3"), Default::default(), Some(exports_dir.clone())).unwrap();
    while session.gallery.busy() {
        session.poll_gallery();
        std::thread::sleep(Duration::from_millis(5));
    }
    session.execute("komaphone.project.new", json!({"width": 96, "height": 64})).unwrap();
    session.execute("paint.stroke", json!({"points": [[8.0, 8.0], [80.0, 52.0]], "size": 6.0, "color": "#d96528"})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    while session.gallery.busy() {
        session.poll_gallery();
        std::thread::sleep(Duration::from_millis(5));
    }
    let drawing_id = session.local_projects.status(session.active().unwrap().doc.id).project.unwrap();
    session.execute("komaphone.gallery.save", json!({"entry": drawing_id, "title": "Forest: Fox / 1?"})).unwrap();
    while session.gallery.busy() {
        session.poll_gallery();
        std::thread::sleep(Duration::from_millis(5));
    }

    let mut app = PhotocraftApp::new(session, services);
    app.ui.komaphone.enabled = true;
    app.ui.gallery.open = true;
    app.ui.gallery.section = GallerySection::Saved;

    let ctx = egui::Context::default();
    let mut t = 1.0;
    paint_shell(&mut app, &ctx, t, vec![]);
    wait_gallery(&mut app);

    // Open saved drawing.
    app.session.execute("komaphone.gallery.open", json!({"entry": drawing_id, "draft": false})).unwrap();
    wait_gallery(&mut app);
    let texts = paint_shell(&mut app, &ctx, t, vec![]);
    wait_gallery(&mut app);
    assert_eq!(app.ui.gallery.export_name, "Forest Fox 1");
    assert!(texts.iter().any(|s| s.contains("Saves automatically to your Komaphone Exports folder")));
    assert!(!texts.iter().any(|s| s.contains("Browse") || s.contains("Choose folder")));

    // Drawing shows archive + png only (not sequence or gif).
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.exportName").is_some());
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.export.archive").is_some());
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.export.png").is_some());
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.export.sequence").is_none());
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.export.gif").is_none());

    // Renaming while unedited updates export_name automatically.
    app.session.execute("komaphone.gallery.update", json!({"entry": drawing_id, "draft": false, "title": "Morning Fox"})).unwrap();
    wait_gallery(&mut app);
    paint_shell(&mut app, &ctx, t, vec![]);
    wait_gallery(&mut app);
    assert_eq!(app.ui.gallery.export_name, "Morning Fox");

    // Once user edits export_name, later item rename preserves user text.
    app.ui.gallery.export_name = "Custom Name".into();
    app.session.execute("komaphone.gallery.update", json!({"entry": drawing_id, "draft": false, "title": "Evening Fox"})).unwrap();
    wait_gallery(&mut app);
    paint_shell(&mut app, &ctx, t, vec![]);
    wait_gallery(&mut app);
    assert_eq!(app.ui.gallery.export_name, "Custom Name");

    // Live validation error disables export buttons and prevents export.
    app.ui.gallery.export_name = "bad/name".into();
    let err_texts = paint_shell(&mut app, &ctx, t, vec![]);
    assert!(err_texts.iter().any(|s| s.contains("path separator") || s.contains('/')));
    click_control(&mut app, &ctx, "gallery.export.png", &mut t);
    assert!(app.session.gallery.last_export.is_none());

    // Valid name exports PNG via real engine command and shows "Show in folder".
    app.ui.gallery.export_name = "Morning Study.png".into();
    let exported_texts = click_control(&mut app, &ctx, "gallery.export.png", &mut t);
    let last = app.session.gallery.last_export.clone().unwrap();
    assert!(!last.is_directory);
    assert!(last.path.ends_with("Morning Study.png"));
    assert!(Path::new(&last.path).is_file());
    assert!(exported_texts.iter().any(|s| s.contains("Saved to ") && s.contains("Morning Study.png")));
    assert!(exported_texts.iter().any(|s| s == "Show in folder"));

    click_control(&mut app, &ctx, "gallery.export.reveal", &mut t);
    assert_eq!(revealed.lock().unwrap().as_slice(), &[(last.path.clone(), false)]);

    verify_game_export_ui(&mut app, &ctx, &exports_dir, &revealed, &last.path, &mut t);
}

fn verify_game_export_ui(
    app: &mut PhotocraftApp,
    ctx: &egui::Context,
    exports_dir: &Path,
    revealed: &Mutex<Vec<(String, bool)>>,
    last_file_path: &str,
    t: &mut f64,
) {
    // Animation game UI shows all 4 formats and renders "Open folder" when sequence exported.
    let anim_manifest = make_game_manifest(40, "Bouncing Lantern", Mode::Animation { rotations: 1 });
    let corpse_manifest = make_game_manifest(41, "Midnight Robot", Mode::ExquisiteCorpse);
    app.ui.gallery.section = GallerySection::Completed;
    app.session.gallery.items.push(gallery_item_for(&anim_manifest, "animation", vec!["Alice", "Bob"]));
    app.session.gallery.items.push(gallery_item_for(&corpse_manifest, "creature", vec!["Alice", "Bob"]));
    app.session.gallery.thumbnails.insert((anim_manifest.id, false), demo_still_image(64, 64, [245, 245, 245, 255], [70, 130, 210, 255]));
    app.session.gallery.thumbnails.insert((corpse_manifest.id, false), demo_still_image(64, 64, [245, 245, 245, 255], [130, 76, 175, 255]));
    app.session.gallery.active = Some(Arc::new(anim_manifest));
    app.session.gallery.active_draft = false;
    app.session.gallery.frame_count = 2;
    app.session.gallery.image = Some(demo_still_image(160, 120, [232, 240, 250, 255], [52, 118, 206, 255]));
    app.session.gallery.last_export = None;

    paint_shell(app, ctx, *t, vec![]);
    assert_eq!(app.ui.gallery.export_name, "Bouncing Lantern");
    for id in ["gallery.export.archive", "gallery.export.png", "gallery.export.sequence", "gallery.export.gif"] {
        assert!(komaphone_workspace::control_rect(ctx, id).is_some(), "missing animation button {id}");
    }

    let seq_dir = exports_dir.join("Bouncing Lantern");
    std::fs::create_dir_all(&seq_dir).unwrap();
    app.session.gallery.last_export = Some(LastExport { path: seq_dir.to_string_lossy().into_owned(), is_directory: true, format: "sequence".into() });
    let seq_texts = paint_shell(app, ctx, *t, vec![]);
    assert!(seq_texts.iter().any(|s| s == "Open folder"));
    click_control(app, ctx, "gallery.export.reveal", t);
    assert_eq!(revealed.lock().unwrap().last().unwrap(), &(seq_dir.to_string_lossy().into_owned(), true));

    // Exquisite Corpse shows archive + png + sequence, NOT gif.
    app.session.gallery.active = Some(Arc::new(corpse_manifest));
    app.session.gallery.frame_count = 1;
    app.session.gallery.last_export = None;
    paint_shell(app, ctx, *t, vec![]);
    assert!(komaphone_workspace::control_rect(ctx, "gallery.export.archive").is_some());
    assert!(komaphone_workspace::control_rect(ctx, "gallery.export.png").is_some());
    assert!(komaphone_workspace::control_rect(ctx, "gallery.export.sequence").is_some());
    assert!(komaphone_workspace::control_rect(ctx, "gallery.export.gif").is_none());

    // Error from reveal callback is displayed inline cleanly and closing returns to Workroom.
    app.services.reveal_in_folder = Some(Box::new(|_, _| Err("Permission denied".into())));
    app.session.gallery.last_export = Some(LastExport { path: last_file_path.to_owned(), is_directory: false, format: "png".into() });
    click_control(app, ctx, "gallery.export.reveal", t);
    assert_eq!(app.session.gallery.error.as_deref(), Some("Permission denied"));
    app.ui.gallery.open = false;
    paint_shell(app, ctx, *t, vec![]);
    assert!(!app.ui.gallery.open);
}

#[cfg(not(target_arch = "wasm32"))]
fn save_wgpu_png(dir: &Path, name: &str, size: Vec2, app: PhotocraftApp, mut draw: impl FnMut(&mut egui::Ui, &mut PhotocraftApp) + 'static) {
    let mut styled = false;
    let mut h = egui_kittest::Harness::builder().with_size(size).with_pixels_per_point(1.0).with_accessibility_check(false).wgpu().build_ui_state(
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
    h.run_steps(4);
    let img = h.render().unwrap_or_else(|e| panic!("wgpu render failed for {name}: {e}"));
    img.save(dir.join(name)).unwrap_or_else(|e| panic!("save {name}: {e}"));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "writes QOL-010 visual verification screenshots using headless wgpu"]
fn qol010_visual_wgpu_captures() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../log/k7/qol-010-managed-exports-20261122/renders");
    std::fs::create_dir_all(&dir).unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let exports_dir = tmp.path().join("Exports");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&exports_dir).unwrap();

    let build_drawing_app = |sub: &str| -> PhotocraftApp {
        let sub_data = data_dir.join(sub);
        let sub_exports = exports_dir.join(sub);
        std::fs::create_dir_all(&sub_data).unwrap();
        std::fs::create_dir_all(&sub_exports).unwrap();
        let mut session = Session::new();
        session.enable_local_projects_with_exports(sub_data.join("drafts.sqlite3"), Default::default(), Some(sub_exports)).unwrap();
        while session.gallery.busy() {
            session.poll_gallery();
            std::thread::sleep(Duration::from_millis(5));
        }
        session.execute("komaphone.project.new", json!({"width": 120, "height": 80})).unwrap();
        session.execute("paint.stroke", json!({"points": [[12.0, 14.0], [104.0, 62.0]], "size": 8.0, "color": "#d96528"})).unwrap();
        session.flush_local_projects(Duration::from_secs(20)).unwrap();
        while session.gallery.busy() {
            session.poll_gallery();
            std::thread::sleep(Duration::from_millis(5));
        }
        let drawing_id = session.local_projects.status(session.active().unwrap().doc.id).project.unwrap();
        session.execute("komaphone.gallery.save", json!({"entry": drawing_id, "title": "Forest Fox"})).unwrap();
        while session.gallery.busy() {
            session.poll_gallery();
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut app = PhotocraftApp::new(session, Services::default());
        wait_gallery(&mut app);
        app.ui.komaphone.enabled = true;
        app.ui.gallery.open = true;
        app.ui.gallery.section = GallerySection::Saved;
        app.session.execute("komaphone.gallery.open", json!({"entry": drawing_id, "draft": false})).unwrap();
        wait_gallery(&mut app);
        app.ui.gallery.export_name = "Forest Fox".into();
        for item in &app.session.gallery.items {
            app.session.gallery.thumbnails.insert((item.id, item.draft), demo_still_image(64, 64, [245, 245, 245, 255], [70, 130, 210, 255]));
        }
        app
    };

    let render_gallery = |ui: &mut egui::Ui, app: &mut PhotocraftApp| {
        app.session.poll_local_projects();
        app.session.poll_gallery();
        komaphone_workspace::chrome(app, ui);
        let t = crate::theme::Tokens::get(ui.ctx());
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ui, |ui| komaphone_gallery::show(app, ui));
    };

    // 1. Saved drawing clean Export section.
    let app1 = build_drawing_app("s1");
    save_wgpu_png(&dir, "01-gallery-export-drawing.png", Vec2::new(1280.0, 720.0), app1, render_gallery);

    // 2. Inline validation error + disabled export buttons.
    let mut app2 = build_drawing_app("s2");
    paint_shell(&mut app2, &egui::Context::default(), 1.0, vec![]);
    wait_gallery(&mut app2);
    app2.ui.gallery.export_name = "bad/name".into();
    save_wgpu_png(&dir, "02-gallery-export-validation-error.png", Vec2::new(1280.0, 720.0), app2, render_gallery);

    // 3. After exporting PNG -> "Saved to ..." + "Show in folder".
    let mut app3 = build_drawing_app("s3");
    app3.session
        .execute("komaphone.gallery.exportManaged", json!({"format": "png", "name": "Forest Fox", "chain": 0, "frame": 0, "fps": 6, "looping": true}))
        .unwrap();
    wait_gallery(&mut app3);
    save_wgpu_png(&dir, "03-gallery-export-saved-file.png", Vec2::new(1280.0, 720.0), app3, render_gallery);

    // 4. Animation game with all 4 formats + exported sequence ("Open folder").
    let mut app4 = build_drawing_app("s4");
    let anim = make_game_manifest(91, "Bouncing Lantern", Mode::Animation { rotations: 1 });
    app4.ui.gallery.section = GallerySection::Completed;
    app4.session.gallery.thumbnails.insert((anim.id, false), demo_still_image(64, 64, [245, 245, 245, 255], [70, 130, 210, 255]));
    app4.session.gallery.items = vec![gallery_item_for(&anim, "animation", vec!["Alice", "Bob"])];
    app4.session.gallery.active = Some(Arc::new(anim));
    app4.session.gallery.frame_count = 2;
    app4.session.gallery.image = Some(demo_still_image(360, 240, [232, 240, 250, 255], [52, 118, 206, 255]));
    app4.session.gallery.replay = Some(ReplayInfo {
        duration_ms: 800,
        baseline_only: false,
        missing: false,
        moments: vec![
            ReplayMoment { revision: 1, elapsed_ms: 0, command: "file.new".into(), kind: "baseline".into() },
            ReplayMoment { revision: 2, elapsed_ms: 800, command: "paint.stroke".into(), kind: "resolved".into() },
        ],
    });
    app4.ui.gallery.export_name = "Bouncing Lantern".into();
    app4.session.gallery.last_export =
        Some(LastExport { path: exports_dir.join("Bouncing Lantern").to_string_lossy().into_owned(), is_directory: true, format: "sequence".into() });
    save_wgpu_png(&dir, "04-gallery-export-animation-sequence.png", Vec2::new(1280.0, 720.0), app4, render_gallery);

    // 5. Exquisite Corpse (Archive + PNG + PNG frames; no GIF button).
    let mut app5 = build_drawing_app("s5");
    let corpse = make_game_manifest(92, "Midnight Robot", Mode::ExquisiteCorpse);
    app5.ui.gallery.section = GallerySection::Completed;
    app5.session.gallery.thumbnails.insert((corpse.id, false), demo_still_image(64, 64, [245, 245, 245, 255], [130, 76, 175, 255]));
    app5.session.gallery.items = vec![gallery_item_for(&corpse, "creature", vec!["Alice", "Bob"])];
    app5.session.gallery.active = Some(Arc::new(corpse));
    app5.session.gallery.frame_count = 1;
    app5.session.gallery.image = Some(demo_still_image(240, 320, [242, 236, 248, 255], [130, 76, 175, 255]));
    app5.session.gallery.last_export = None;
    app5.ui.gallery.export_name = "Midnight Robot".into();
    save_wgpu_png(&dir, "05-gallery-export-creature.png", Vec2::new(1280.0, 720.0), app5, render_gallery);
}
