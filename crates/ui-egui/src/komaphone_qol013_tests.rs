//! QOL-013 catch-up playback stability tests, GPU oracle parity, measurements, and visual captures.
#![cfg(test)]
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable as _;
use photocraft_engine::Session;
use photocraft_engine::scene::{
    CanvasId, OwnerId, RegionKey, Srgba, Stroke, StrokeId, Viewport,
    brush::StrokeSample,
    coordinates,
    shared::{
        AcceptedOp, DeviceId, Edit, MediaId, OpId, ReviewState, SourceId, WorldOp, WorldState,
        media::{AssetHash as SceneAssetHash, Credit, MediaFrame, MediaPlacement, MediaSource, Provenance},
    },
    spatial::TileKey,
    world_protocol::CanvasSummary,
};
use photocraft_engine::world_brush::to_stroke_brush;
use photocraft_engine::world_media::{WorldRenderer, render_view, rgba8};
use photocraft_engine::world_tiles::{TileBudget, WorldTileManager};
use photocraft_engine::worlds::{Account, Canvas, Viewer};
use serde_json::{Value, json};

use crate::komaphone_world_gpu::{CpuTilePresenter, WorldGpu, tests::readback};
use crate::{PhotocraftApp, Tool};

fn hex(n: u128) -> String {
    format!("{n:032x}")
}

fn no_assets(_: &MediaFrame) -> photocraft_engine::Result<Vec<u8>> {
    Err(photocraft_engine::EngineError::Other("no media in synthetic test".into()))
}

struct DenseFixture {
    canvas: CanvasId,
    owners: [OwnerId; 3],
    devices: [DeviceId; 3],
    world: Arc<WorldState>,
    zoom_view: Viewport,
    default_view: Viewport,
    deep_rev: u64,
}

fn build_stroke(canvas: CanvasId, owner: OwnerId, rev: u64, depth: u8, pts_xy: &[[f64; 2]], style: ([f32; 4], f32, bool)) -> Stroke {
    let (color, width, erase) = style;
    let mut brush = photocraft_paint::presets::builtin().into_iter().find(|p| p.name == "Soft Round").unwrap().brush;
    brush.size = width;
    brush.hardness = 0.35;
    brush.opacity = color[3];
    brush.color = [color[0], color[1], color[2], 1.0];
    brush.seed = 400 + rev;
    let sb = to_stroke_brush(&brush, 1.0, false).unwrap();
    let samples: Vec<StrokeSample> = pts_xy
        .iter()
        .enumerate()
        .map(|(i, _)| StrokeSample { pressure: 1.0, tilt_x: 0.0, tilt_y: 0.0, rotation: 0.0, wheel: 0.0, time_ms: (i as f64) * 16.0 })
        .collect();
    Stroke {
        id: StrokeId::new(&hex(0xE400 + u128::from(rev))).unwrap(),
        canvas,
        owner,
        revision: 0,
        anchor: RegionKey { depth, ..Default::default() },
        points: pts_xy.to_vec(),
        width: f64::from(width),
        color: Srgba { rgba: color },
        erase,
        brush: Some(Box::new(sb)),
        samples: Some(samples),
    }
}

fn apply_op(world: &mut WorldState, author: OwnerId, device: DeviceId, rev: u64, action: Edit) {
    let seq = world.next_sequence(device).unwrap();
    world
        .apply(&AcceptedOp {
            revision: rev,
            op: WorldOp {
                id: OpId::new(&hex(0xE300 + u128::from(rev))).unwrap(),
                canvas: world.canvas,
                author,
                device,
                sequence: seq,
                authority_epoch: 1,
                action,
            },
        })
        .unwrap();
}

fn build_dense_multi_author_canvas(include_media: bool) -> DenseFixture {
    let canvas = CanvasId::new(&hex(0xE001)).unwrap();
    let owners = [OwnerId::new(&hex(0xE101)).unwrap(), OwnerId::new(&hex(0xE102)).unwrap(), OwnerId::new(&hex(0xE103)).unwrap()];
    let devices = [DeviceId::new(&hex(0xE201)).unwrap(), DeviceId::new(&hex(0xE202)).unwrap(), DeviceId::new(&hex(0xE203)).unwrap()];
    let mut world = WorldState::new(canvas, owners[0], "Dense Catch-Up Canvas".into()).unwrap();
    let palette = [[0.86, 0.24, 0.20, 0.78], [0.14, 0.48, 0.88, 0.78], [0.16, 0.66, 0.38, 0.80], [0.88, 0.56, 0.14, 0.78], [0.58, 0.24, 0.76, 0.76]];
    let mut rev = 0u64;
    for i in 1..=168u64 {
        rev += 1;
        let who = (i as usize) % 3;
        let col = palette[(i as usize) % palette.len()];
        let x0 = 60.0 + f64::from(((i * 53) % 780) as u32);
        let y0 = 60.0 + f64::from(((i * 79) % 420) as u32);
        let x1 = (x0 + 115.0).min(920.0);
        let y1 = (y0 + 72.0).min(530.0);
        let erase = i % 29 == 0;
        let stroke = build_stroke(canvas, owners[who], rev, 0, &[[x0, y0], [x1, y1]], (col, 44.0, erase));
        apply_op(&mut world, owners[who], devices[who], rev, Edit::AddStroke { stroke });
    }
    // Undo & redo visibility toggles on an existing stroke.
    let toggle_target = StrokeId::new(&hex(0xE400 + 25)).unwrap();
    rev += 1;
    apply_op(&mut world, owners[1], devices[1], rev, Edit::SetStrokeVisible { stroke: toggle_target, visible: false });
    rev += 1;
    apply_op(&mut world, owners[1], devices[1], rev, Edit::SetStrokeVisible { stroke: toggle_target, visible: true });

    // Optional uncached placed media to exercise offline media resilience during review.
    if include_media {
        rev += 1;
        let media = MediaPlacement {
            id: MediaId::new(&hex(0xE701)).unwrap(),
            canvas,
            owner: owners[2],
            anchor: RegionKey::default(),
            offset: [160.0, 120.0],
            width: 140.0,
            height: 90.0,
            frames: vec![MediaFrame { hash: SceneAssetHash::new(&"a".repeat(64)).unwrap(), byte_len: 32, width: 32, height: 32, duration_ms: 100 }],
            looping: false,
            provenance: Provenance {
                source: MediaSource::Gallery { entry: SourceId::new(&hex(0xE801)).unwrap(), room: None, game: None, chain: 0, frame: None },
                source_revision: 1,
                artifact_hash: SceneAssetHash::new(&"b".repeat(64)).unwrap(),
                authors: vec![Credit { profile: owners[2], label: "Charlie".into() }],
            },
        };
        apply_op(&mut world, owners[2], devices[2], rev, Edit::PlaceMedia { media: Box::new(media) });
    }

    // Deep-zoom cluster at depth=4 (exact coordinates centered at [512, 320] in depth-4 region).
    for d in 0..8u64 {
        rev += 1;
        let who = (d as usize) % 3;
        let y = 240.0 + (d as f64) * 20.0;
        let stroke = build_stroke(canvas, owners[who], rev, 4, &[[340.0, y], [680.0, y + 18.0]], (palette[(d as usize) % palette.len()], 24.0, false));
        apply_op(&mut world, owners[who], devices[who], rev, Edit::AddStroke { stroke });
    }

    let center_d0 = coordinates::point(&RegionKey::default(), [480.0, 285.0]).unwrap();
    let default_view = Viewport { camera: coordinates::target(canvas, center_d0, 0, 1.0).unwrap(), width: 960, height: 570 };
    let center_d4 = coordinates::point(&RegionKey { depth: 4, ..Default::default() }, [512.0, 320.0]).unwrap();
    let zoom_view = Viewport { camera: coordinates::target(canvas, center_d4, 4, 1.0).unwrap(), width: 960, height: 570 };
    DenseFixture { canvas, owners, devices, world: Arc::new(world), zoom_view, default_view, deep_rev: rev }
}

fn spawn_eframe_harness(fx: &DenseFixture, review: Option<ReviewState>, view: &Viewport) -> (egui_kittest::Harness<'static, PhotocraftApp>, tempfile::TempDir) {
    let assets_dir = tempfile::tempdir().unwrap();
    let assets_path = assets_dir.path().to_path_buf();
    let world = Arc::clone(&fx.world);
    let canvas = fx.canvas;
    let owner = fx.owners[0];
    let device = fx.devices[0];
    let camera = view.camera.clone();
    let h = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1280.0, 720.0))
        .with_pixels_per_point(1.0)
        .with_max_steps(32)
        .wgpu()
        .wgpu_setup(eframe::egui_wgpu::WgpuSetup::without_display_handle())
        .build_eframe(move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, crate::theme::ThemeKind::Studio);
            let mut session = Session::new();
            session.enable_world_assets(assets_path.clone()).unwrap();
            session.worlds.account = Some(Account { endpoint: "https://localhost:43195".into(), profile: owner, device });
            session.worlds.active = Some(Canvas {
                info: CanvasSummary {
                    canvas,
                    owner,
                    name: world.name.clone(),
                    initial_name: world.name.clone(),
                    revision: world.revision,
                    epoch: 1,
                    import_origin: None,
                },
                canonical: Arc::clone(&world),
                display: Arc::clone(&world),
                pending: 0,
                viewer: Viewer { camera: camera.clone(), bookmarks: vec![], review: review.clone() },
                notice: None,
            });
            let mut app = PhotocraftApp::new(session, crate::Services::default());
            app.ui.komaphone.enabled = true;
            app.ui.world.open = true;
            app.ui.tool = Tool::Brush;
            if let Some(state) = cc.wgpu_render_state.as_ref() {
                app.set_wgpu(state.clone());
            }
            app
        });
    (h, assets_dir)
}

fn settle_review(h: &mut egui_kittest::Harness<'_, PhotocraftApp>) -> u32 {
    let mut max_held = h.state().world_render.gpu_held_review_frames();
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        h.run_steps(1);
        max_held = max_held.max(h.state().world_render.gpu_held_review_frames());
        let s = h.state();
        let rev = s.session.worlds.active.as_ref().and_then(|a| a.viewer.review.as_ref()).and_then(|r| r.interval.as_ref());
        let settled = match rev {
            Some(interval) => s.world_render.review_settled_for(interval.end_inclusive, interval.position),
            None => !s.world_render.preparing && !s.world_render.stats.is_refining && s.world_render.rendered_revision.is_some(),
        };
        if settled {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(2);
    max_held.max(h.state().world_render.gpu_held_review_frames())
}

fn center_non_white_pixels(h: &mut egui_kittest::Harness<'_, PhotocraftApp>) -> usize {
    let shot = h.render().unwrap();
    let (w, h_px) = (shot.width(), shot.height());
    let mut count = 0usize;
    for y in (h_px / 2 - 40)..(h_px / 2 + 40) {
        for x in (w / 2 - 40)..(w / 2 + 40) {
            let p = shot.get_pixel(x, y);
            if p[0] < 252 || p[1] < 252 || p[2] < 252 {
                count += 1;
            }
        }
    }
    count
}

#[derive(Default)]
struct StepMetrics {
    white_flashes: u32,
    preparing_flashes: u32,
    saving_flips: u32,
    partial_frames: u32,
    max_held: u32,
}

fn step_and_observe(h: &mut egui_kittest::Harness<'_, PhotocraftApp>, positions: &[u64], metrics: &mut StepMetrics) {
    for &pos in positions {
        {
            let active = h.state_mut().session.worlds.active.as_mut().unwrap();
            active.viewer.review.as_mut().unwrap().scrub(pos).unwrap();
        }
        for _ in 0..3 {
            h.run_steps(1);
            let s = h.state();
            if !s.world_render.can_render_current || center_non_white_pixels(h) == 0 {
                metrics.white_flashes += 1;
            }
            if h.state().world_render.shows_preparing_overlay() {
                metrics.preparing_flashes += 1;
            }
            if h.query_by_label("Saving locally…").is_some() {
                metrics.saving_flips += 1;
            }
            metrics.max_held = metrics.max_held.max(h.state().world_render.gpu_held_review_frames());
            let s = h.state();
            if s.world_render.stats.fallback_tiles > 0 && s.world_render.gpu_held_review_frames() == 0 && !s.world_render.stats.is_refining {
                metrics.partial_frames += 1;
            }
            if s.world_render.gpu_held_review_frames() >= 20 {
                metrics.max_held = metrics.max_held.max(s.world_render.gpu_held_review_frames());
            }
            let interval = s.session.worlds.active.as_ref().unwrap().viewer.review.as_ref().unwrap().interval.as_ref().unwrap();
            if s.world_render.review_settled_for(interval.end_inclusive, interval.position) {
                break;
            }
            std::thread::sleep(Duration::from_millis(4));
        }
        metrics.max_held = metrics.max_held.max(settle_review(h));
        let rendered_rev = h.state().world_render.rendered_revision;
        assert_eq!(rendered_rev, Some(pos), "review frame must settle cleanly at {pos}");
    }
}

#[test]
fn dense_multi_author_review_playback_scrub_filter_peer_edit_and_reopen_have_zero_flicker() {
    let _gpu_lock = photocraft_testkit::gpu_lock();
    let fx = build_dense_multi_author_canvas(true);
    let mut review = ReviewState::first_visit(fx.canvas, fx.owners[0], 119).unwrap();
    review.begin(fx.world.revision).unwrap();
    review.scrub(120).unwrap();

    let (mut h, _assets_dir) = spawn_eframe_harness(&fx, Some(review.clone()), &fx.default_view);
    settle_review(&mut h);
    let mut metrics = StepMetrics::default();

    // Forward stepping, backward scrubbing, pause/play gate, and undo/redo range (`170`).
    let forward: Vec<u64> = (121..=132).collect();
    step_and_observe(&mut h, &forward, &mut metrics);
    let backward: Vec<u64> = (124..=131).rev().collect();
    step_and_observe(&mut h, &backward, &mut metrics);
    step_and_observe(&mut h, &[168, 169, 170, 171], &mut metrics);

    // Author filter toggle and play/pause settlement gating.
    h.state_mut().ui.world.highlights.author = Some(fx.owners[1]);
    h.state_mut().ui.world.highlights.include_own = false;
    {
        let r = h.state_mut().session.worlds.active.as_mut().unwrap().viewer.review.as_mut().unwrap();
        r.interval.as_mut().unwrap().playing = true;
    }
    for _ in 0..8 {
        h.run_steps(1);
        assert!(h.state().world_render.can_render_current);
        assert!(!h.state().world_render.shows_preparing_overlay());
        std::thread::sleep(Duration::from_millis(5));
    }
    {
        let r = h.state_mut().session.worlds.active.as_mut().unwrap().viewer.review.as_mut().unwrap();
        r.interval.as_mut().unwrap().playing = false;
    }
    let settled_pos = settle_review(&mut h);
    metrics.max_held = metrics.max_held.max(settled_pos);

    // Concurrent peer edit arriving mid-review updates live canonical/display while review stays isolated on `end`.
    let mut extended = (*fx.world).clone();
    let new_rev = extended.revision + 1;
    let peer_stroke = build_stroke(fx.canvas, fx.owners[2], new_rev, 0, &[[100.0, 100.0], [500.0, 420.0]], ([0.95, 0.12, 0.12, 0.9], 28.0, false));
    apply_op(&mut extended, fx.owners[2], fx.devices[2], new_rev, Edit::AddStroke { stroke: peer_stroke });
    let extended_arc = Arc::new(extended);
    {
        let active = h.state_mut().session.worlds.active.as_mut().unwrap();
        active.canonical = Arc::clone(&extended_arc);
        active.display = Arc::clone(&extended_arc);
        active.info.revision = new_rev;
    }
    step_and_observe(&mut h, &[175], &mut metrics);
    assert_eq!(h.state().world_render.rendered_revision, Some(175));

    // Exit back to Live view cleanly reveals rev `new_rev`.
    h.state_mut().session.worlds.active.as_mut().unwrap().viewer.review.as_mut().unwrap().cancel();
    settle_review(&mut h);
    assert_eq!(h.state().world_render.rendered_revision, Some(new_rev));
    assert!(!h.state().session.worlds.has_pending_edits());

    assert_eq!(metrics.white_flashes, 0, "expected 0 white-flash frames");
    assert_eq!(metrics.preparing_flashes, 0, "expected 0 Preparing overlay flashes");
    assert_eq!(metrics.saving_flips, 0, "expected 0 Saving locally status flips");
    assert_eq!(metrics.partial_frames, 0, "expected 0 partial fallback frames");
    assert!(metrics.max_held < 20, "held review frames {} exceeded 20-frame cap", metrics.max_held);

    // Simulate offline reopen preserving exact review interval at rev 175.
    let reopened_review = h.state().session.worlds.active.as_ref().unwrap().viewer.review.clone();
    drop(h);
    let mut fx2 = fx;
    fx2.world = extended_arc;
    let mut reopen_rev = reopened_review.unwrap();
    reopen_rev.begin(fx2.world.revision).unwrap();
    reopen_rev.scrub(175).unwrap();
    let (mut h2, _assets_dir2) = spawn_eframe_harness(&fx2, Some(reopen_rev), &fx2.default_view);
    settle_review(&mut h2);
    assert_eq!(h2.state().world_render.rendered_revision, Some(175));
    assert!(center_non_white_pixels(&mut h2) > 0);
}

#[test]
fn cpu_tile_presenter_holds_incomplete_review_frame_until_settled() {
    let canvas = CanvasId::new(&hex(0xF100)).unwrap();
    let view = default_view_for(canvas);
    let ctx = egui::Context::default();
    let mut presenter = CpuTilePresenter::default();
    let red_tile = vec![220u8; 256 * 256 * 4];
    let blue_tile = vec![90u8; 256 * 256 * 4];
    let rect = TileKey { lod: 0, tx: 0, ty: 0 }.logical_rect();

    // Frame 1 (settled complete frame) establishes baseline texture.
    let base_id = presenter.present_tiles_for_review(&ctx, &view, 1.0, 1, false, true, &[(rect, [0.0, 0.0, 1.0, 1.0], &red_tile)]).unwrap();

    // Up to 20 incomplete frames during review hold the intact texture without overwriting pixels.
    for step in 0..20u32 {
        let held = presenter.present_tiles_for_review(&ctx, &view, 1.0, 1, true, true, &[(rect, [0.0, 0.0, 1.0, 1.0], &blue_tile)]).unwrap();
        assert_eq!(held, base_id, "step {step} should keep intact review texture");
    }

    // 21st frame exceeds 20-frame cap and swaps forward cleanly without indefinite deadlock; complete swaps immediately too.
    let swapped = presenter.present_tiles_for_review(&ctx, &view, 1.0, 1, false, true, &[(rect, [0.0, 0.0, 1.0, 1.0], &blue_tile)]).unwrap();
    assert_eq!(swapped, base_id);

    // Scope reset drops cached texture and allocates fresh handle.
    presenter.reset();
    let fresh = presenter.present_tiles_for_review(&ctx, &view, 1.0, 2, true, true, &[(rect, [0.0, 0.0, 1.0, 1.0], &blue_tile)]).unwrap();
    assert_ne!(fresh, base_id);
}

fn default_view_for(canvas: CanvasId) -> Viewport {
    let center = coordinates::point(&RegionKey::default(), [128.0, 128.0]).unwrap();
    Viewport { camera: coordinates::target(canvas, center, 0, 1.0).unwrap(), width: 256, height: 256 }
}

fn present_settled_gpu(world: &WorldState, view: &Viewport, gpu: &mut WorldGpu, mgr: &mut WorldTileManager) -> (Vec<u8>, u32) {
    let renderer = WorldRenderer::new(world).unwrap();
    let scope = photocraft_engine::world_tiles::ScopeKey::new(1, "https://localhost", "owner", "dev", world.canvas, Some(world.revision));
    mgr.set_scope(scope, Arc::clone(renderer.index()));
    let mut no_media = |_: &MediaFrame| Ok(None);
    let mut steps = 0u32;
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        steps += 1;
        gpu.present_interactive(mgr, view, 1.0, 0, &[], None, &mut no_media, true).unwrap();
        if !gpu.stats.is_refining && gpu.stats.pending_tiles == 0 && gpu.stats.fallback_tiles == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    assert!(!gpu.stats.is_refining && gpu.stats.pending_tiles == 0, "GPU failed to settle within timeout (steps={steps})");
    let bytes = readback(gpu).into_iter().flatten().collect();
    (bytes, gpu.held_review_frames())
}

#[test]
fn gpu_readback_matches_cpu_oracle_across_review_and_deep_zoom() {
    let _gpu_lock = photocraft_testkit::gpu_lock();
    let fx = build_dense_multi_author_canvas(false);
    let (h, _assets_dir) = spawn_eframe_harness(&fx, None, &fx.default_view);
    let rs = h.state().gpu_render_state.clone().expect("wgpu render state required");
    let health = h.state().gpu_health().expect("gpu health");
    let mut gpu = WorldGpu::new(&rs, health).unwrap();
    let mut mgr = WorldTileManager::new(TileBudget::conservative(), None);

    for (label, rev, view) in [
        ("start_120", 120u64, &fx.default_view),
        ("forward_150", 150u64, &fx.default_view),
        ("backward_132", 132u64, &fx.default_view),
        ("deep_zoom_d4", fx.deep_rev, &fx.zoom_view),
    ] {
        let sub = fx.world.replay_to(rev).unwrap();
        let cpu_img = render_view(&sub, view, 0, &mut no_assets).unwrap();
        let cpu = rgba8(&cpu_img).unwrap();
        let (gpu_bytes, _) = present_settled_gpu(&sub, view, &mut gpu, &mut mgr);
        assert_eq!(cpu.len(), gpu_bytes.len(), "{label}: byte length mismatch");
        let max_diff = cpu.iter().zip(gpu_bytes.iter()).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
        assert!(max_diff <= 2, "{label}: max channel diff {max_diff} exceeded 2/255 oracle bound");
    }
    drop(h);
}

#[test]
#[ignore = "writes QOL-013 AFTER visual PNGs and before/after measurements.json"]
fn qol013_visual_wgpu_captures() {
    let _gpu_lock = photocraft_testkit::gpu_lock();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../log/k7/qol-013-catchup-stability-20261122");
    let renders_dir = root.join("renders");
    std::fs::create_dir_all(&renders_dir).unwrap();

    let fx = build_dense_multi_author_canvas(false);
    let mut review = ReviewState::first_visit(fx.canvas, fx.owners[0], 119).unwrap();
    review.begin(fx.world.revision).unwrap();
    review.scrub(120).unwrap();

    let (mut h, _assets_dir) = spawn_eframe_harness(&fx, Some(review), &fx.default_view);
    settle_review(&mut h);
    if h.query_by_label("Since your last review").is_some() {
        h.get_by_label("Since your last review").click();
        settle_review(&mut h);
    }
    h.render().unwrap().save(renders_dir.join("after-01-review-start.png")).unwrap();

    // Capture `after-02-review-mid-playback.png` on the exact immediate frame right after `scrub(121)`!
    let mut after_metrics = StepMetrics::default();
    {
        let active = h.state_mut().session.worlds.active.as_mut().unwrap();
        active.viewer.review.as_mut().unwrap().scrub(121).unwrap();
    }
    h.run_steps(1);
    let imm_shot = h.render().unwrap();
    assert!(h.state().world_render.can_render_current);
    assert!(!h.state().world_render.shows_preparing_overlay());
    assert!(center_non_white_pixels(&mut h) > 0, "immediate frame after step must show artwork, not white flash");
    imm_shot.save(renders_dir.join("after-02-review-mid-playback.png")).unwrap();
    after_metrics.max_held = after_metrics.max_held.max(settle_review(&mut h));

    step_and_observe(&mut h, &(122..=130).collect::<Vec<_>>(), &mut after_metrics);

    // 3. Author filter active.
    h.state_mut().ui.world.highlights.author = Some(fx.owners[1]);
    h.state_mut().ui.world.highlights.include_own = false;
    settle_review(&mut h);
    h.render().unwrap().save(renders_dir.join("after-03-review-author-filter.png")).unwrap();

    // 4. Deep zoom `depth=4` review frame.
    {
        let active = h.state_mut().session.worlds.active.as_mut().unwrap();
        active.viewer.camera = fx.zoom_view.camera.clone();
        active.viewer.review.as_mut().unwrap().scrub(fx.deep_rev).unwrap();
    }
    settle_review(&mut h);
    h.render().unwrap().save(renders_dir.join("after-04-review-deep-zoom.png")).unwrap();

    // 5. Exit back to Live view cleanly showing all revisions.
    {
        let active = h.state_mut().session.worlds.active.as_mut().unwrap();
        active.viewer.camera = fx.default_view.camera.clone();
        active.viewer.review.as_mut().unwrap().cancel();
    }
    settle_review(&mut h);
    h.render().unwrap().save(renders_dir.join("after-05-review-end-and-exit-live.png")).unwrap();

    // Measure GPU oracle max diff for measurements.json too.
    let rs = h.state().gpu_render_state.clone().expect("wgpu render state");
    let health = h.state().gpu_health().expect("gpu health");
    let mut gpu = WorldGpu::new(&rs, health).unwrap();
    let mut mgr = WorldTileManager::new(TileBudget::conservative(), None);
    let sub130 = fx.world.replay_to(130).unwrap();
    let cpu130 = rgba8(&render_view(&sub130, &fx.default_view, 0, &mut no_assets).unwrap()).unwrap();
    let (gpu130, _) = present_settled_gpu(&sub130, &fx.default_view, &mut gpu, &mut mgr);
    let gpu_max_diff = cpu130.iter().zip(gpu130.iter()).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);

    let before_json: Value = serde_json::from_str(&std::fs::read_to_string(root.join("before-measurements.json")).unwrap()).unwrap_or_else(|_| json!({}));
    let combined = json!({
        "before": before_json,
        "after": {
            "steps_tested": 10,
            "white_flash_frames": after_metrics.white_flashes,
            "partial_tile_frames": after_metrics.partial_frames,
            "preparing_overlay_flashes": after_metrics.preparing_flashes,
            "saving_locally_flips": after_metrics.saving_flips,
            "max_held_review_frames": after_metrics.max_held,
            "gpu_vs_cpu_max_channel_diff": gpu_max_diff,
        },
        "observed_author_filter": BTreeSet::from([fx.owners[1].as_str()]),
    });
    std::fs::write(root.join("measurements.json"), serde_json::to_string_pretty(&combined).unwrap()).unwrap();
}
