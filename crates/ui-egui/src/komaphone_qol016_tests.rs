//! QOL-016 UI, recovery-deduplication, and visual verification tests.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::{Pos2, Rect, Vec2};
use photocraft_engine::gallery::GalleryItem;
use photocraft_engine::gallery_media::{ReplayInfo, ReplayMoment};
use photocraft_engine::party::gallery::{GALLERY_SCHEMA, GalleryContent, GalleryManifest};
use photocraft_engine::party::room::{Assignment, MemberView, Phase, RevealedEntry, RoomView};
use photocraft_engine::party::{
    AssetHash, AssetRef, AssignmentId, CheckpointId, CheckpointRef, DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, ReplayId, ReplayRef, RoomId,
};
use photocraft_engine::rooms::media::RoomImage;
use serde_json::json;

use crate::komaphone_gallery::{self, GallerySection};
use crate::{PhotocraftApp, Services, komaphone_time, komaphone_workspace};

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

fn wait_gallery(app: &mut PhotocraftApp) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while app.session.gallery.busy() {
        assert!(std::time::Instant::now() < deadline, "gallery timed out");
        app.session.poll_gallery();
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn qol016_komaphone_local_drafts_skip_legacy_recovery_autosave_and_discard_stuck_recovery_once_deduped() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    let drafts_db = root.join("drafts.sqlite3");

    // 1. Session A: create a Komaphone draft, paint a stroke, flush, trigger autosave.
    let autosaved_docs = Arc::new(Mutex::new(Vec::<(u64, Option<String>)>::new()));
    let autosaved_docs_cb = Arc::clone(&autosaved_docs);
    let services = Services {
        autosave: Some(Box::new(move |doc, _rev, path| {
            autosaved_docs_cb.lock().unwrap().push((doc.id.0, path.map(str::to_owned)));
            Ok(())
        })),
        ..Default::default()
    };
    let mut session = photocraft_engine::Session::new();
    session.enable_local_projects(drafts_db.clone(), Default::default()).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();

    let mut app = PhotocraftApp::new(session, services);
    app.ui.komaphone.enabled = true;
    app.run("komaphone.project.new", json!({"width": 64, "height": 64, "name": "Sunset"})).unwrap();
    app.run("paint.stroke", json!({"points": [[8, 8], [48, 48]], "size": 5, "color": "#cc4422"})).unwrap();
    app.session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let komaphone_doc = app.session.active().unwrap().doc.as_ref().clone();
    let original_project = app.session.local_projects.status(komaphone_doc.id).project.unwrap();
    assert_eq!(app.session.local_projects.status(komaphone_doc.id).local_revision, 2);

    let ctx = egui::Context::default();
    crate::prefs_ui::autosave_now(&mut app);
    crate::prefs_ui::tick(&mut app, &ctx);
    assert!(autosaved_docs.lock().unwrap().is_empty(), "Komaphone-managed local draft must NOT write config/Recovery/");

    // External disk-backed document still gets upstream PhotoCraft recovery autosave.
    let ext_doc = photocraft_doc::Document::with_background(
        "External.pcraft",
        photocraft_doc::Size { width: 32, height: 32 },
        photocraft_doc::ColorMode::Rgb,
        photocraft_doc::SampleType::U8,
        photocraft_doc::Color::WHITE,
    );
    app.session.add_document(ext_doc, Some("C:\\tmp\\external.pcraft".into()));
    app.run("paint.stroke", json!({"points": [[4, 4], [24, 24]], "size": 3, "color": "#1155aa"})).unwrap();
    crate::prefs_ui::autosave_now(&mut app);
    crate::prefs_ui::tick(&mut app, &ctx);
    let calls = autosaved_docs.lock().unwrap().clone();
    assert_eq!(calls.len(), 1, "external disk-backed document retains upstream recovery autosave");
    assert_eq!(calls[0].1.as_deref(), Some("C:\\tmp\\external.pcraft"));
    drop(app);

    // 2. Session B: simulate relaunch with a stuck legacy config/Recovery/ file matching `komaphone_doc`.
    let discarded = Arc::new(Mutex::new(Vec::<u64>::new()));
    let discarded_cb = Arc::clone(&discarded);
    let adopted = Arc::new(Mutex::new(Vec::<(u64, String)>::new()));
    let adopted_cb = Arc::clone(&adopted);
    let recovered_payload = komaphone_doc.clone();
    let services_b = Services {
        autosave: Some(Box::new(|_doc, _rev, _path| Ok(()))),
        adopt_autosave: Some(Box::new(move |id, key| {
            adopted_cb.lock().unwrap().push((id, key.to_owned()));
        })),
        discard_autosave: Some(Box::new(move |id| {
            discarded_cb.lock().unwrap().push(id);
        })),
        recover: Some(Box::new(move || {
            let payload = recovered_payload.clone();
            vec![crate::services::Recoverable { key: "stuck-legacy-id".into(), name: "Sunset".into(), path: None, load: Box::new(move || Ok(payload)) }]
        })),
        ..Default::default()
    };

    let mut session_b = photocraft_engine::Session::new();
    session_b.enable_local_projects(drafts_db.clone(), Default::default()).unwrap();
    session_b.flush_local_projects(Duration::from_secs(20)).unwrap();
    let mut app_b = PhotocraftApp::new(session_b, services_b);
    app_b.ui.komaphone.enabled = true;

    let ctx_b = egui::Context::default();
    // Tick startup recovery, poll jobs, flush local projects, then tick again to discard stale recovery entry.
    for _ in 0..20 {
        crate::prefs_ui::tick(&mut app_b, &ctx_b);
        for event in app_b.session.poll_jobs() {
            crate::prefs_ui::on_recovery_event(&mut app_b, &event);
        }
        app_b.session.poll_local_projects();
        if !discarded.lock().unwrap().is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    app_b.session.flush_local_projects(Duration::from_secs(20)).unwrap();
    crate::prefs_ui::tick(&mut app_b, &ctx_b);

    assert_eq!(app_b.session.documents().len(), 1, "recovered document opened");
    let rec_id = app_b.session.active().unwrap().doc.id;
    let status = app_b.session.local_projects.status(rec_id);
    assert_eq!(status.project, Some(original_project), "recovery dedup rebinds to original ProjectId");
    assert_eq!(status.local_revision, 2, "recovery dedup keeps existing revision history");
    assert!(app_b.session.local_document_saved(rec_id));
    assert!(discarded.lock().unwrap().contains(&rec_id.0), "stuck config/Recovery/ entry discarded once drafts.sqlite3 confirmed");

    app_b.session.list_local_projects().unwrap();
    app_b.session.flush_local_projects(Duration::from_secs(20)).unwrap();
    assert_eq!(app_b.session.local_projects.drafts.len(), 1, "no duplicate clone created on relaunch");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn qol016_workroom_save_to_gallery_still_cleanup_and_two_step_trash_delete_work_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    let mut session = photocraft_engine::Session::new();
    session.enable_local_projects(root.join("drafts.sqlite3"), Default::default()).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let mut app = PhotocraftApp::new(session, Services::default());
    wait_gallery(&mut app);
    app.ui.komaphone.enabled = true;

    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, app.ui.theme);
    let mut t = 0.0;

    // 1. Create drawing in Workroom and verify Save to gallery button fits cleanly in 1280x720 header.
    app.run("komaphone.project.new", json!({"width": 64, "height": 64, "name": "My Painting"})).unwrap();
    app.run("paint.stroke", json!({"points": [[6, 6], [56, 56]], "size": 6, "color": "#2255dd"})).unwrap();
    app.session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let saved_id = app.session.local_projects.status(app.session.active().unwrap().doc.id).project.unwrap();
    paint_shell(&mut app, &ctx, t, vec![]);
    t += 0.1;
    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 720.0));
    let save_rect = komaphone_workspace::control_rect(&ctx, "workspace.saveToGallery").expect("Save to gallery button visible in Workroom");
    assert!(viewport.contains_rect(save_rect), "Save to gallery within viewport: {save_rect:?}");
    assert!(komaphone_workspace::workroom_save_to_gallery_state(&app).is_ok());

    // Click Save to gallery from Workroom and wait for worker completion.
    click_control(&mut app, &ctx, "workspace.saveToGallery", &mut t);
    wait_gallery(&mut app);
    paint_shell(&mut app, &ctx, t, vec![]);
    t += 0.1;
    assert!(app.ui.status.contains("Saved to gallery"), "Workroom status confirms save: {}", app.ui.status);

    // Close the Workroom tab and also create a closed unsaved Temp draft.
    app.session.close(0).unwrap();
    app.session.flush_local_projects(Duration::from_secs(20)).unwrap();
    app.run("komaphone.project.new", json!({"width": 64, "height": 64, "name": "Scratch Note"})).unwrap();
    app.run("paint.stroke", json!({"points": [[10, 40], [50, 12]], "size": 5, "color": "#11aa55"})).unwrap();
    app.session.flush_local_projects(Duration::from_secs(20)).unwrap();
    app.session.close(0).unwrap();
    app.session.flush_local_projects(Duration::from_secs(20)).unwrap();

    // 2. Open Gallery -> Saved, verify left margin, absence of removed controls, and clean still-drawing detail pane.
    komaphone_gallery::open(&mut app);
    wait_gallery(&mut app);
    paint_shell(&mut app, &ctx, t, vec![]);
    t += 0.1;
    let saved_tab = komaphone_workspace::control_rect(&ctx, "gallery.section.saved").expect("Saved tab visible");
    assert!(saved_tab.min.x >= 10.0, "left-edge padding prevents clipping: {saved_tab:?}");
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.draw").is_none());
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.favorites").is_none());
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.trashFilter").is_none());
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.kind").is_none());

    click_control(&mut app, &ctx, &format!("gallery.item.{}.original", saved_id.as_str()), &mut t);
    wait_gallery(&mut app);
    let detail_texts = paint_shell(&mut app, &ctx, t, vec![]);
    t += 0.1;

    // Still drawing hides animation clutter while keeping Drawing replay, Edit in Workroom, and Archive/PNG exports.
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.chain.0").is_none(), "no Animation 1 pill on still drawing");
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.animationPlay").is_none(), "no Play animation on still drawing");
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.fps").is_none(), "no fps control on still drawing");
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.loop").is_none(), "no Loop checkbox on still drawing");
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.gifPreview").is_none(), "no GIF preview on still drawing");
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.remixStill").is_none(), "no duplicate Remix selected still on 1-frame still");
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.export.sequence").is_none(), "no PNG frames on 1-frame still");
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.export.gif").is_none(), "no Animated GIF on 1-frame still");
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.replayMode").is_some(), "Drawing replay remains accessible");
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.editInWorkroom").is_some(), "Edit in Workroom present on Saved drawing");
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.export.archive").is_some());
    assert!(komaphone_workspace::control_rect(&ctx, "gallery.export.png").is_some());
    assert!(!detail_texts.iter().any(|txt| txt.contains("GIF uses up to 255")));

    // 3. Move Saved item to Trash -> Trash tab -> two-step Delete permanently (Cancel keeps it, Confirm removes it).
    click_control(&mut app, &ctx, "gallery.trash", &mut t);
    wait_gallery(&mut app);
    click_control(&mut app, &ctx, "gallery.section.trash", &mut t);
    paint_shell(&mut app, &ctx, t, vec![]);
    t += 0.1;
    click_control(&mut app, &ctx, "gallery.delete", &mut t);
    assert!(komaphone_workspace::control_rect(&ctx, "komaphone.gallery.delete").is_some(), "Confirm delete appears");
    click_control(&mut app, &ctx, "gallery.delete.cancel", &mut t);
    assert!(app.session.gallery.items.iter().any(|i| i.id == saved_id && i.trashed), "Cancel keeps trashed item");
    click_control(&mut app, &ctx, "gallery.delete", &mut t);
    click_control(&mut app, &ctx, "komaphone.gallery.delete", &mut t);
    wait_gallery(&mut app);
    assert!(!app.session.gallery.items.iter().any(|i| i.id == saved_id), "Confirm permanently deletes item");

    // 4. Temp tab -> Clear temp -> moves Scratch Note to Trash -> Trash -> Empty trash two-step confirmation.
    click_control(&mut app, &ctx, "gallery.section.temp", &mut t);
    click_control(&mut app, &ctx, "komaphone.gallery.clearTemp", &mut t);
    wait_gallery(&mut app);
    click_control(&mut app, &ctx, "gallery.section.trash", &mut t);
    click_control(&mut app, &ctx, "gallery.emptyTrash", &mut t);
    assert!(komaphone_workspace::control_rect(&ctx, "komaphone.gallery.emptyTrash").is_some(), "Confirm empty appears");
    click_control(&mut app, &ctx, "komaphone.gallery.emptyTrash", &mut t);
    wait_gallery(&mut app);
    assert!(app.session.gallery.items.is_empty(), "Empty trash removes all closed trashed items");
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
    h.run_steps(4);
    let img = h.render().unwrap_or_else(|e| panic!("wgpu render failed for {name}: {e}"));
    img.save(dir.join(name)).unwrap_or_else(|e| panic!("save {name}: {e}"));
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

fn gallery_fixture_items() -> (Vec<GalleryItem>, GalleryManifest, GalleryManifest) {
    let saved_id = project_id(1);
    let anim_id = project_id(2);
    let corpse_id = project_id(3);
    let temp_id = project_id(4);
    let trash_id = project_id(5);

    let items = vec![
        GalleryItem {
            id: saved_id,
            title: "Forest Fox".into(),
            kind: "project".into(),
            authors: vec![],
            updated_at: "2026-11-22T22:10:00Z".into(),
            favorite: false,
            trashed: false,
            complete: true,
            draft: false,
            revision: 4,
            state: "Saved drawing".into(),
            source_projects: vec![saved_id],
            saved_matches_head: false,
            has_saved_copy: false,
            has_unsaved_edits: false,
            untouched_blank: false,
            covered_by_game: false,
        },
        GalleryItem {
            id: anim_id,
            title: "Bouncing Lantern".into(),
            kind: "animation".into(),
            authors: vec!["Alice".into(), "Bob".into()],
            updated_at: "2026-11-21T04:43:00Z".into(),
            favorite: false,
            trashed: false,
            complete: true,
            draft: false,
            revision: 1,
            state: "Completed game".into(),
            source_projects: vec![anim_id],
            saved_matches_head: false,
            has_saved_copy: false,
            has_unsaved_edits: false,
            untouched_blank: false,
            covered_by_game: false,
        },
        GalleryItem {
            id: corpse_id,
            title: "Midnight Robot".into(),
            kind: "creature".into(),
            authors: vec!["Alice".into(), "Bob".into(), "Charlie".into()],
            updated_at: "2026-11-19T04:43:00Z".into(),
            favorite: false,
            trashed: false,
            complete: true,
            draft: false,
            revision: 1,
            state: "Completed game".into(),
            source_projects: vec![corpse_id],
            saved_matches_head: false,
            has_saved_copy: false,
            has_unsaved_edits: false,
            untouched_blank: false,
            covered_by_game: false,
        },
        GalleryItem {
            id: temp_id,
            title: "Rough Cloud Study".into(),
            kind: "draft".into(),
            authors: vec!["You".into()],
            updated_at: "2026-11-22T21:30:00Z".into(),
            favorite: false,
            trashed: false,
            complete: true,
            draft: true,
            revision: 6,
            state: "Temporary draft".into(),
            source_projects: vec![temp_id],
            saved_matches_head: false,
            has_saved_copy: false,
            has_unsaved_edits: false,
            untouched_blank: false,
            covered_by_game: false,
        },
        GalleryItem {
            id: trash_id,
            title: "Old Test Doodle".into(),
            kind: "project".into(),
            authors: vec![],
            updated_at: "2026-11-18T19:15:00Z".into(),
            favorite: false,
            trashed: true,
            complete: true,
            draft: false,
            revision: 2,
            state: "Saved drawing".into(),
            source_projects: vec![trash_id],
            saved_matches_head: false,
            has_saved_copy: false,
            has_unsaved_edits: false,
            untouched_blank: false,
            covered_by_game: false,
        },
    ];

    let still_manifest = GalleryManifest {
        schema_version: GALLERY_SCHEMA,
        id: saved_id,
        title: "Forest Fox".into(),
        created_at: "2026-11-22T22:10:00Z".into(),
        updated_at: "2026-11-22T22:10:00Z".into(),
        authors: vec![],
        content: GalleryContent::Project { checkpoint: asset_ref(1), direct_assets: vec![], replay: None },
        provenance: None,
        notes: vec![],
        fps: 6,
        looping: true,
    };

    let game_id = GameId::new(anim_id.as_str()).unwrap();
    let p1 = participant(1, "Alice");
    let p2 = participant(2, "Bob");
    let mut reveal = Vec::new();
    for chain in 0..2u8 {
        for round in 0..2u8 {
            let proj = project_id(100 + u64::from(chain) * 2 + u64::from(round));
            let cp = CheckpointRef { id: CheckpointId::new(proj.as_str()).unwrap(), asset: asset_ref(2) };
            reveal.push(RevealedEntry::Artwork {
                assignment: Assignment {
                    id: AssignmentId { game: game_id, chain, round },
                    player: if (chain + round) % 2 == 0 { p1.id } else { p2.id },
                    project: proj,
                },
                revision: 2,
                image: asset_ref(1),
                checkpoint: cp,
                replay: Box::new(ReplayRef {
                    id: ReplayId::new(proj.as_str()).unwrap(),
                    manifest: None,
                    baseline: cp,
                    end_event: None,
                    event_count: 0,
                    duration_ms: 0,
                }),
                assets: vec![asset_ref(1), asset_ref(2)],
            });
        }
    }
    let room = RoomView {
        id: RoomId::new(&"aa".repeat(16)).unwrap(),
        revision: 12,
        epoch: 1,
        phase: Phase::Reveal,
        round: 1,
        mode: Mode::Animation { rotations: 1 },
        timer_ms: None,
        moderator: p1.id,
        members: vec![MemberView { participant: p1.clone(), connected: true, ready: true }, MemberView { participant: p2, connected: true, ready: true }],
        waiting: vec![],
        paused: None,
        deadline_ms: None,
        assignment: None,
        reveal,
    };
    let mut anim_manifest = GalleryManifest::from_reveal(anim_id, &room, p1.id, "2026-11-21T04:43:00Z".into()).unwrap();
    anim_manifest.title = "Bouncing Lantern".into();

    (items, still_manifest, anim_manifest)
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "opt-in visual capture: set KOMAPHONE_QOL016_RENDER_DIR to output directory"]
fn qol016_visual_wgpu_captures() {
    let dir = std::path::PathBuf::from(std::env::var("KOMAPHONE_QOL016_RENDER_DIR").expect("set KOMAPHONE_QOL016_RENDER_DIR"));
    std::fs::create_dir_all(&dir).unwrap();

    // 1. Workroom header with Save to gallery button at 1280x720.
    {
        let tmp = tempfile::tempdir().unwrap();
        let mut session = photocraft_engine::Session::new();
        session.enable_local_projects(tmp.path().join("drafts.sqlite3"), Default::default()).unwrap();
        session.flush_local_projects(Duration::from_secs(20)).unwrap();
        let mut app = PhotocraftApp::new(session, Services::default());
        wait_gallery(&mut app);
        app.ui.komaphone.enabled = true;
        app.run("komaphone.project.new", json!({"width": 1024, "height": 768, "name": "Forest Fox"})).unwrap();
        app.run("paint.stroke", json!({"points": [[220, 220], [500, 380], [760, 240]], "size": 28, "color": "#d96528"})).unwrap();
        app.session.flush_local_projects(Duration::from_secs(20)).unwrap();
        save_wgpu_png(&dir, "01-workroom-save-to-gallery-1280x720.png", Vec2::new(1280.0, 720.0), 1.0, false, app, |ui, app| {
            komaphone_workspace::chrome(app, ui);
            let t = crate::theme::Tokens::get(ui.ctx());
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ui, |ui| crate::canvas::document_area(app, ui));
        });
    }

    let make_gallery_app = |tmp: &std::path::Path,
                            section: GallerySection,
                            active: GalleryManifest,
                            active_draft: bool,
                            frames: usize,
                            bg: [u8; 4],
                            fg: [u8; 4]|
     -> PhotocraftApp {
        let (items, _, _) = gallery_fixture_items();
        let mut session = photocraft_engine::Session::new();
        session.enable_local_projects(tmp.join("drafts.sqlite3"), Default::default()).unwrap();
        session.flush_local_projects(Duration::from_secs(20)).unwrap();
        let mut app = PhotocraftApp::new(session, Services::default());
        wait_gallery(&mut app);
        app.ui.komaphone.enabled = true;
        app.ui.gallery.open = true;
        app.ui.gallery.section = section;
        app.session.gallery.items = items;
        app.session.gallery.usage_bytes = 12 * 1_048_576;
        app.session.gallery.quota_bytes = 4096 * 1_048_576;
        app.session.gallery.active = Some(Arc::new(active));
        app.session.gallery.active_draft = active_draft;
        app.session.gallery.frame_count = frames;
        app.session.gallery.image = Some(demo_still_image(480, 320, bg, fg));
        app.session.gallery.replay = Some(ReplayInfo {
            duration_ms: 800,
            baseline_only: false,
            missing: false,
            moments: vec![
                ReplayMoment { revision: 1, elapsed_ms: 0, command: "file.new".into(), kind: "baseline".into() },
                ReplayMoment { revision: 2, elapsed_ms: 800, command: "paint.stroke".into(), kind: "resolved".into() },
            ],
        });
        for item in &app.session.gallery.items {
            app.session.gallery.thumbnails.insert((item.id, item.draft), demo_still_image(64, 64, [245, 245, 245, 255], [70, 130, 210, 255]));
        }
        app
    };

    komaphone_time::with_viewer_timezone(komaphone_time::fixture_pacific_12h, || {
        let (_, still_manifest, anim_manifest) = gallery_fixture_items();
        let tmp_g = tempfile::tempdir().unwrap();

        // 2. Gallery -> Saved (clean still-drawing detail pane, zero animation clutter, crisp margins).
        let app_saved =
            make_gallery_app(&tmp_g.path().join("saved"), GallerySection::Saved, still_manifest.clone(), false, 1, [246, 242, 234, 255], [217, 101, 40, 255]);
        save_wgpu_png(&dir, "02-gallery-saved-still-1280x720.png", Vec2::new(1280.0, 720.0), 1.0, false, app_saved, |ui, app| {
            komaphone_workspace::chrome(app, ui);
            let t = crate::theme::Tokens::get(ui.ctx());
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ui, |ui| komaphone_gallery::show(app, ui));
        });

        // 3. Gallery -> Completed (Animation game with All/Animations/Creatures pills & animation toolbar).
        let app_completed =
            make_gallery_app(&tmp_g.path().join("completed"), GallerySection::Completed, anim_manifest, false, 2, [232, 240, 250, 255], [52, 118, 206, 255]);
        save_wgpu_png(&dir, "03-gallery-completed-games-1280x720.png", Vec2::new(1280.0, 720.0), 1.0, false, app_completed, |ui, app| {
            komaphone_workspace::chrome(app, ui);
            let t = crate::theme::Tokens::get(ui.ctx());
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ui, |ui| komaphone_gallery::show(app, ui));
        });

        // 4. Gallery -> Temp (Clear temp + Continue drawing / Save to gallery).
        let mut temp_manifest = still_manifest.clone();
        temp_manifest.id = project_id(4);
        temp_manifest.title = "Rough Cloud Study".into();
        let app_temp = make_gallery_app(&tmp_g.path().join("temp"), GallerySection::Temp, temp_manifest, true, 1, [240, 248, 244, 255], [52, 160, 108, 255]);
        save_wgpu_png(&dir, "04-gallery-temp-drafts-1280x720.png", Vec2::new(1280.0, 720.0), 1.0, false, app_temp, |ui, app| {
            komaphone_workspace::chrome(app, ui);
            let t = crate::theme::Tokens::get(ui.ctx());
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ui, |ui| komaphone_gallery::show(app, ui));
        });

        // 5. Gallery -> Trash (two-step Empty trash + Restore + two-step Delete permanently).
        let mut trash_manifest = still_manifest.clone();
        trash_manifest.id = project_id(5);
        trash_manifest.title = "Old Test Doodle".into();
        let mut app_trash =
            make_gallery_app(&tmp_g.path().join("trash"), GallerySection::Trash, trash_manifest, false, 1, [246, 236, 236, 255], [186, 68, 68, 255]);
        app_trash.ui.gallery.confirm_empty_trash = true;
        save_wgpu_png(&dir, "05-gallery-trash-confirm-1280x720.png", Vec2::new(1280.0, 720.0), 1.0, false, app_trash, |ui, app| {
            komaphone_workspace::chrome(app, ui);
            let t = crate::theme::Tokens::get(ui.ctx());
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ui, |ui| komaphone_gallery::show(app, ui));
            app.ui.gallery.confirm_delete = Some((project_id(5), false));
        });

        // 6. Gallery -> Saved at 2x DPI HiDPI verifying clean margins & no overlap.
        let app_hidpi =
            make_gallery_app(&tmp_g.path().join("hidpi"), GallerySection::Saved, still_manifest, false, 1, [246, 242, 234, 255], [217, 101, 40, 255]);
        save_wgpu_png(&dir, "06-gallery-saved-2x-dpi-1280x720.png", Vec2::new(1280.0, 720.0), 2.0, false, app_hidpi, |ui, app| {
            komaphone_workspace::chrome(app, ui);
            let t = crate::theme::Tokens::get(ui.ctx());
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ui, |ui| komaphone_gallery::show(app, ui));
        });
    });
}
