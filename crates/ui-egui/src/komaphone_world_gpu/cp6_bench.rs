//! Restored CP1 §7.2 `--release` W1–W8 acceptance benchmark suite & visual PNG audit generator.
//!
//! Run with:
//! `cargo test --release -p photocraft-ui-egui --lib --offline -- komaphone_world_gpu::cp6_bench::run_cp1_restored_w1_to_w8_release_acceptance --ignored --nocapture`
//!
//! Writes raw JSON receipt to `log/k7/qol-009-redesign-dev/cp7-async-and-e2e/cp1-restored-w1-w8-release-bench.json`
//! and visual audit PNGs to `log/k7/qol-009-redesign-dev/cp7-async-and-e2e/renders/`.
//! Preserves earlier `log/k7/qol-009-redesign-dev/cp6/` evidence untouched.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::too_many_arguments)]
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use photocraft_color::BlendMode;
use photocraft_engine::scene::world_protocol::CanvasSummary;
use photocraft_engine::{
    paint::{BrushSettings, StrokePoint, dynamics::DabGenerator},
    scene::{
        CanvasId, OwnerId, RegionKey, Srgba, Stroke, StrokeId, Viewport, Wide,
        brush::{BrushBlendMode, StrokeBrush, StrokeSample},
        coordinates,
        shared::{
            AcceptedOp, DeviceId, Edit, MediaId, OpId, SharedStroke, TourId, WorldOp, WorldState,
            archive_import::ArchiveManifest,
            media::{AssetHash, Credit, MediaFrame, MediaPlacement, MediaSource, Provenance},
            tours::{Tour, TourStop},
        },
        spatial::select_target_lod,
    },
    world_brush::{LiveTileBrush, stroke_points_in_brush_space, to_paint_settings, to_stroke_brush},
    world_media::{WorldImage, WorldRenderer, encode_png, media_asset_hash},
    world_tiles::{RgbaFrame, ScopeKey, TileBudget, WorldSceneIndex, WorldTileManager},
};
use photocraft_room_client::world_queue::{WorldOutbox, WorldScope};
use photocraft_testkit::perf::RssSampler;
use serde::Serialize;

use super::tests::readback;
use super::{CpuTilePresenter, WorldGpu};

fn hex(n: u128) -> String {
    format!("{n:032x}")
}
fn owner(n: u128) -> OwnerId {
    OwnerId::new(&hex(10 + n)).unwrap()
}
fn device(n: u128) -> DeviceId {
    DeviceId::new(&hex(1000 + n)).unwrap()
}
fn canvas(n: u128) -> CanvasId {
    CanvasId::new(&hex(5000 + n)).unwrap()
}
fn preset_brush(name: &str) -> BrushSettings {
    photocraft_paint::presets::builtin().into_iter().find(|p| p.name == name).unwrap_or_else(|| panic!("missing builtin preset {name}")).brush
}
fn pt(x: f64, y: f64) -> StrokePoint {
    StrokePoint::new(x, y, 1.0)
}
fn pt_t(x: f64, y: f64, t: f64) -> StrokePoint {
    let mut p = StrokePoint::new(x, y, 1.0);
    p.time = t;
    p
}
fn view_at(c: CanvasId, w: u32, h: u32, depth: u8, center_local: [f64; 2], zoom: f64) -> Viewport {
    let p = coordinates::point(&RegionKey { depth, ..Default::default() }, center_local).unwrap();
    Viewport { camera: coordinates::target(c, p, depth, zoom).unwrap(), width: w, height: h }
}

fn make_brush_stroke(c: CanvasId, seq: u128, author: u128, pts: &[StrokePoint], brush: &BrushSettings, depth: u8, anchor_xy: [i128; 2]) -> Stroke {
    let mut b = brush.clone();
    b.seed = (seq as u64).max(1);
    let sb = to_stroke_brush(&b, 1.0, false).unwrap();
    let local_pts: Vec<[f64; 2]> = pts.iter().map(|p| [p.x, p.y]).collect();
    let samples: Vec<StrokeSample> = pts
        .iter()
        .map(|p| StrokeSample { pressure: p.pressure, tilt_x: p.tilt_x, tilt_y: p.tilt_y, rotation: p.rotation, wheel: p.wheel, time_ms: p.time })
        .collect();
    let need_samples = sb.build_up || !sb.spacing_enabled || sb.smoothing.is_some();
    Stroke {
        id: StrokeId::new(&hex(0xC000_0000 + seq)).unwrap(),
        canvas: c,
        owner: owner(author),
        revision: 0,
        anchor: RegionKey { depth, x: Wide(anchor_xy[0]), y: Wide(anchor_xy[1]) },
        points: local_pts,
        width: f64::from(b.size),
        color: Srgba { rgba: [b.color[0], b.color[1], b.color[2], b.opacity] },
        erase: b.erase,
        brush: Some(Box::new(sb)),
        samples: need_samples.then_some(samples),
    }
}

fn append_action(world: &mut WorldState, author_idx: u128, action: Edit) {
    let rev = world.revision + 1;
    let dev = device(author_idx);
    let seq = world.next_sequence(dev).unwrap();
    world
        .apply(&AcceptedOp {
            revision: rev,
            op: WorldOp {
                id: OpId::new(&hex(10_000_000 + u128::from(rev))).unwrap(),
                canvas: world.canvas,
                author: owner(author_idx),
                device: dev,
                sequence: seq,
                authority_epoch: 1,
                action,
            },
        })
        .unwrap();
}

fn append_stroke(world: &mut WorldState, author_idx: u128, stroke: Stroke) {
    append_action(world, author_idx, Edit::AddStroke { stroke });
}

fn count_world_dabs(world: &WorldState) -> usize {
    let mut total = 0_usize;
    for shared in world.strokes.values() {
        if !shared.visible {
            continue;
        }
        if let Some(brush) = shared.stroke.brush.as_deref()
            && let Ok(paint) = to_paint_settings(&shared.stroke, brush)
        {
            let sz = brush.smoothing.as_ref().map_or(1.0, |sm| sm.zoom);
            let mut dab_gen = DabGenerator::new(&paint, sz);
            let pts = stroke_points_in_brush_space(&shared.stroke, brush);
            let mut dabs = Vec::new();
            let mut duals = Vec::new();
            dab_gen.push(&pts, &mut dabs, &mut duals);
            dab_gen.finish(&mut dabs, &mut duals);
            total = total.saturating_add(dabs.len());
        }
    }
    total
}

#[derive(Clone, Debug, Serialize)]
struct FixtureMeta {
    revisions: u64,
    strokes: usize,
    visible_strokes: usize,
    points: usize,
    dabs: Option<usize>,
    media: usize,
    tours: usize,
}

fn fixture_meta(world: &WorldState, include_dabs: bool) -> FixtureMeta {
    FixtureMeta {
        revisions: world.revision,
        strokes: world.strokes.len(),
        visible_strokes: world.strokes.values().filter(|s| s.visible).count(),
        points: world.point_count(),
        dabs: include_dabs.then(|| count_world_dabs(world)),
        media: world.media.len(),
        tours: world.tours.len(),
    }
}

#[derive(Clone, Debug, Serialize)]
struct FrameStats {
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    max_ms: f64,
    worst_trial_max_ms: f64,
    gpu_wait_p50_ms: f64,
    gpu_wait_p95_ms: f64,
}

#[derive(Clone, Debug, Serialize)]
struct SettleStats {
    trials: usize,
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
}

#[derive(Clone, Debug, Serialize)]
struct CoarseCrispStats {
    coarse: SettleStats,
    crisp: SettleStats,
}

#[derive(Clone, Debug, Serialize)]
struct ParityCheck {
    max_channel_delta_u8: u8,
    p99_channel_delta_u8: u8,
    pixels_over_2: usize,
    pixels_over_3: usize,
    total_pixels: usize,
}

#[derive(Clone, Default)]
struct MediaBank {
    frames: BTreeMap<AssetHash, Arc<RgbaFrame>>,
    pngs: BTreeMap<AssetHash, Vec<u8>>,
}

impl MediaBank {
    fn register_in(&self, mgr: &mut WorldTileManager) {
        for (h, f) in &self.frames {
            mgr.register_media_frame(*h, Arc::clone(f));
        }
    }
    fn make_frame(&mut self, width: u32, height: u32, rgba: Vec<u8>, duration_ms: u32) -> MediaFrame {
        let img = WorldImage {
            width,
            height,
            pixels: rgba.chunks_exact(4).map(|p| [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0, p[3] as f32 / 255.0]).collect(),
        };
        let png = encode_png(&img).unwrap();
        let hash = media_asset_hash(&png).unwrap();
        let byte_len = png.len() as u64;
        self.frames.insert(hash, Arc::new(RgbaFrame { width, height, rgba }));
        self.pngs.insert(hash, png);
        MediaFrame { hash, byte_len, width, height, duration_ms }
    }
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn summarize_trials(cpu_trials: &[Vec<f64>], gpu_trials: &[Vec<f64>]) -> FrameStats {
    let mut trial_summaries: Vec<(f64, f64, f64, f64, f64, f64)> = Vec::new();
    let mut worst_max = 0.0_f64;
    for (cpu, gpu) in cpu_trials.iter().zip(gpu_trials.iter()) {
        let mut c = cpu.clone();
        c.sort_by(|a, b| a.total_cmp(b));
        let mut g = gpu.clone();
        g.sort_by(|a, b| a.total_cmp(b));
        let m = *c.last().unwrap_or(&0.0);
        worst_max = worst_max.max(m);
        trial_summaries.push((pct(&c, 0.50), pct(&c, 0.95), pct(&c, 0.99), m, pct(&g, 0.50), pct(&g, 0.95)));
    }
    trial_summaries.sort_by(|a, b| a.1.total_cmp(&b.1));
    let med = trial_summaries[trial_summaries.len() / 2];
    FrameStats { p50_ms: med.0, p95_ms: med.1, p99_ms: med.2, max_ms: med.3, worst_trial_max_ms: worst_max, gpu_wait_p50_ms: med.4, gpu_wait_p95_ms: med.5 }
}

fn summarize_settle(mut times: Vec<f64>) -> SettleStats {
    times.sort_by(|a, b| a.total_cmp(b));
    SettleStats { trials: times.len(), p50_ms: pct(&times, 0.50), p95_ms: pct(&times, 0.95), max_ms: *times.last().unwrap_or(&0.0) }
}

fn save_gpu_png(gpu: &WorldGpu, path: &std::path::Path) {
    let pixels = readback(gpu);
    let [w, h] = gpu.targets.as_ref().unwrap().size;
    let img = WorldImage {
        width: w,
        height: h,
        pixels: pixels.into_iter().map(|[r, g, b, a]| [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a as f32 / 255.0]).collect(),
    };
    std::fs::write(path, encode_png(&img).unwrap()).unwrap();
}

fn measure_gpu_vs_cpu_parity(gpu: &WorldGpu, index: &Arc<WorldSceneIndex>, view: &Viewport, elapsed_ms: u64, media: &MediaBank) -> ParityCheck {
    let gpu_px = readback(gpu);
    let mut renderer = WorldRenderer::from_index(Arc::clone(index));
    let mut resolve_png = |frame: &MediaFrame| -> photocraft_engine::Result<Vec<u8>> {
        media.pngs.get(&frame.hash).cloned().ok_or_else(|| photocraft_engine::EngineError::Other("missing media png".into()))
    };
    let cpu_img = renderer.render_with(view, elapsed_ms, &mut resolve_png, &mut || true).unwrap();
    assert_eq!(gpu_px.len(), cpu_img.pixels.len());
    let mut deltas = Vec::with_capacity(gpu_px.len());
    let mut max_d = 0_u8;
    let mut over_2 = 0_usize;
    let mut over_3 = 0_usize;
    for (g, c) in gpu_px.iter().zip(cpu_img.pixels.iter()) {
        // WorldGpu composites tiles over white paper [255,255,255,255] just like render_view!
        let cr = (c[0].clamp(0.0, 1.0) * 255.0).round() as u8;
        let cg = (c[1].clamp(0.0, 1.0) * 255.0).round() as u8;
        let cb = (c[2].clamp(0.0, 1.0) * 255.0).round() as u8;
        let ca = (c[3].clamp(0.0, 1.0) * 255.0).round() as u8;
        let d = g[0].abs_diff(cr).max(g[1].abs_diff(cg)).max(g[2].abs_diff(cb)).max(g[3].abs_diff(ca));
        max_d = max_d.max(d);
        if d > 2 {
            over_2 += 1;
        }
        if d > 3 {
            over_3 += 1;
        }
        deltas.push(d);
    }
    deltas.sort_unstable();
    let p99 = deltas[((deltas.len() - 1) as f64 * 0.99).round() as usize];
    ParityCheck { max_channel_delta_u8: max_d, p99_channel_delta_u8: p99, pixels_over_2: over_2, pixels_over_3: over_3, total_pixels: gpu_px.len() }
}

fn wait_until_crisp_pair(gpu: &mut WorldGpu, mgr: &mut WorldTileManager, view: &Viewport, density: f32, elapsed_ms: u64, media: &MediaBank) -> (f64, f64) {
    let mut resolve = |frame: &MediaFrame| -> photocraft_engine::Result<Option<Arc<RgbaFrame>>> { Ok(media.frames.get(&frame.hash).cloned()) };
    let t0 = Instant::now();
    let mut coarse_ms: Option<f64> = None;
    loop {
        gpu.present_interactive(mgr, view, density, elapsed_ms, &[], None, &mut resolve, false).unwrap();
        let _ = gpu.health.wait(&gpu.rs.device, None);
        if coarse_ms.is_none() && gpu.stats.visible_tiles > 0 && gpu.stats.resident_tiles + gpu.stats.fallback_tiles >= gpu.stats.visible_tiles {
            coarse_ms = Some(t0.elapsed().as_secs_f64() * 1000.0);
        }
        if !gpu.stats.is_refining && gpu.stats.anim_pending_tiles == 0 {
            let crisp = t0.elapsed().as_secs_f64() * 1000.0;
            return (coarse_ms.unwrap_or(crisp), crisp);
        }
        if t0.elapsed() > Duration::from_secs(20) {
            panic!("crisp settle timed out after 20s (pending_tiles={}, anim_pending={})", gpu.stats.pending_tiles, gpu.stats.anim_pending_tiles);
        }
        std::thread::sleep(Duration::from_micros(250));
    }
}

fn bench_settle_10(
    gpu: &mut WorldGpu,
    mgr: &mut WorldTileManager,
    index: &Arc<WorldSceneIndex>,
    scope_base: u64,
    view: &Viewport,
    density: f32,
    media: &MediaBank,
) -> CoarseCrispStats {
    let mut coarse = Vec::with_capacity(10);
    let mut crisp = Vec::with_capacity(10);
    for t in 0..10_u64 {
        gpu.purge_slots();
        mgr.set_scope(ScopeKey::new(scope_base + t, "bench", "u1", "d1", index.world().canvas, None), Arc::clone(index));
        media.register_in(mgr);
        let (co, cr) = wait_until_crisp_pair(gpu, mgr, view, density, 0, media);
        coarse.push(co);
        crisp.push(cr);
    }
    CoarseCrispStats { coarse: summarize_settle(coarse), crisp: summarize_settle(crisp) }
}

fn measure_pan_zoom(
    gpu: &mut WorldGpu,
    mgr: &mut WorldTileManager,
    base_view: &Viewport,
    density: f32,
    pan_span: f64,
    zoom_sweep: bool,
    peers: &[SharedStroke],
    media: &MediaBank,
) -> FrameStats {
    let mut resolve = |frame: &MediaFrame| -> photocraft_engine::Result<Option<Arc<RgbaFrame>>> { Ok(media.frames.get(&frame.hash).cloned()) };
    let mut cpu_trials = Vec::new();
    let mut gpu_trials = Vec::new();
    for trial in 0..3_usize {
        let mut cpu_ms = Vec::with_capacity(120);
        let mut gpu_ms = Vec::with_capacity(120);
        for frame in 0..130_usize {
            let mut v = base_view.clone();
            let phase = (frame as f64) * 0.08 + (trial as f64) * 0.7;
            let drag = [phase.sin() * pan_span, (phase * 0.7).cos() * (pan_span * 0.6)];
            v.camera = coordinates::pan(&base_view.camera, drag).unwrap();
            if zoom_sweep {
                let factor = 1.0 + 0.35 * (phase * 0.5).sin();
                v.camera = coordinates::zoom_at(&v, [f64::from(v.width) * 0.5, f64::from(v.height) * 0.5], factor).unwrap();
            }
            let elapsed_ms = (frame as u64) * 16;
            let t0 = Instant::now();
            gpu.present_interactive(mgr, &v, density, elapsed_ms, peers, None, &mut resolve, false).unwrap();
            let t_cpu = t0.elapsed().as_secs_f64() * 1000.0;
            let tg = Instant::now();
            let _ = gpu.health.wait(&gpu.rs.device, None);
            let t_gpu = tg.elapsed().as_secs_f64() * 1000.0;
            if frame >= 10 {
                cpu_ms.push(t_cpu);
                gpu_ms.push(t_gpu);
            }
        }
        cpu_trials.push(cpu_ms);
        gpu_trials.push(gpu_ms);
    }
    summarize_trials(&cpu_trials, &gpu_trials)
}

fn measure_live_draw(
    gpu: &mut WorldGpu,
    mgr: &mut WorldTileManager,
    view: &Viewport,
    density: f32,
    template_stroke: &Stroke,
    author_idx: u128,
    peer_previews: &[SharedStroke],
    media: &MediaBank,
) -> FrameStats {
    let mut resolve = |frame: &MediaFrame| -> photocraft_engine::Result<Option<Arc<RgbaFrame>>> { Ok(media.frames.get(&frame.hash).cloned()) };
    let mut cpu_trials = Vec::new();
    let mut gpu_trials = Vec::new();
    let target_lod = gpu.target_lod(view, density).unwrap();

    for trial in 0..3_usize {
        let mut cpu_ms = Vec::with_capacity(120);
        let mut gpu_ms = Vec::with_capacity(120);
        let mut live_stroke = template_stroke.clone();
        live_stroke.id = StrokeId::new(&hex(99_000_000 + trial as u128)).unwrap();
        let all_pts = template_stroke.points.clone();
        let all_samples = template_stroke.samples.clone();
        live_stroke.points = vec![all_pts[0]];
        live_stroke.samples = all_samples.as_ref().map(|s| vec![s[0]]);
        let mut live_brush = LiveTileBrush::new(&live_stroke, target_lod).unwrap();

        for frame in 0..130_usize {
            // For 10s 60 Hz timestamped strokes (600–625 samples @ ~16ms each), advance 1 real-time
            // 60 Hz sample per frame instead of compressing 5x wall-clock time into each 16ms frame.
            let p_idx = if all_pts.len() > 131 { (frame + 1).min(all_pts.len()) } else { ((frame + 1) * all_pts.len() / 131).max(1).min(all_pts.len()) };
            live_stroke.points.clear();
            live_stroke.points.extend_from_slice(&all_pts[..p_idx]);
            if let (Some(dst), Some(src)) = (&mut live_stroke.samples, &all_samples) {
                dst.clear();
                dst.extend_from_slice(&src[..p_idx]);
            }
            let mut previews = Vec::with_capacity(1 + peer_previews.len());
            for (pi, peer) in peer_previews.iter().enumerate() {
                let mut p = peer.clone();
                let plen = ((frame + 1 + pi * 7) * peer.stroke.points.len() / 131).max(2).min(peer.stroke.points.len());
                p.stroke.points.truncate(plen);
                if let Some(s) = &mut p.stroke.samples {
                    s.truncate(plen);
                }
                previews.push(p);
            }
            previews.push(SharedStroke {
                stroke: live_stroke.clone(),
                provenance: None,
                device: device(author_idx),
                visible: true,
                created_revision: 999_999,
                changed_revision: 999_999,
            });
            let t0 = Instant::now();
            live_brush.sync_points(&live_stroke.points, live_stroke.samples.as_deref()).unwrap();
            gpu.present_interactive(mgr, view, density, (frame as u64) * 16, &previews, Some(&mut live_brush), &mut resolve, false).unwrap();
            let t_cpu = t0.elapsed().as_secs_f64() * 1000.0;
            let tg = Instant::now();
            let _ = gpu.health.wait(&gpu.rs.device, None);
            let t_gpu = tg.elapsed().as_secs_f64() * 1000.0;
            if frame >= 10 {
                cpu_ms.push(t_cpu);
                gpu_ms.push(t_gpu);
            }
        }
        cpu_trials.push(cpu_ms);
        gpu_trials.push(gpu_ms);
    }
    summarize_trials(&cpu_trials, &gpu_trials)
}

fn measure_tour_flight(gpu: &mut WorldGpu, mgr: &mut WorldTileManager, tour: &Tour, width: u32, height: u32, density: f32, media: &MediaBank) -> FrameStats {
    let mut resolve = |frame: &MediaFrame| -> photocraft_engine::Result<Option<Arc<RgbaFrame>>> { Ok(media.frames.get(&frame.hash).cloned()) };
    let duration = tour.duration_ms().unwrap().max(1);
    let mut cpu_trials = Vec::new();
    let mut gpu_trials = Vec::new();
    for trial in 0..3_usize {
        let mut cpu_ms = Vec::with_capacity(120);
        let mut gpu_ms = Vec::with_capacity(120);
        for frame in 0..130_usize {
            let frac = ((frame + trial * 11) % 130) as u64;
            let elapsed = frac * duration / 129;
            let tf = tour.frame_at(elapsed).unwrap();
            let v = Viewport { camera: tf.camera, width, height };
            let t0 = Instant::now();
            gpu.present_interactive(mgr, &v, density, (frame as u64) * 16, &[], None, &mut resolve, false).unwrap();
            let t_cpu = t0.elapsed().as_secs_f64() * 1000.0;
            let tg = Instant::now();
            let _ = gpu.health.wait(&gpu.rs.device, None);
            let t_gpu = tg.elapsed().as_secs_f64() * 1000.0;
            if frame >= 10 {
                cpu_ms.push(t_cpu);
                gpu_ms.push(t_gpu);
            }
        }
        cpu_trials.push(cpu_ms);
        gpu_trials.push(gpu_ms);
    }
    summarize_trials(&cpu_trials, &gpu_trials)
}

// =============================================================================
// Exact CP1 §7.2 Point Generators (1:1 with baseline world_perf_bench/src/main.rs)
// =============================================================================

fn ten_sec_airbrush_points() -> Vec<StrokePoint> {
    let mut pts = Vec::with_capacity(625);
    let mut x = 520.0;
    let mut y = 360.0;
    for step in 0..625_usize {
        let t_ms = (step as f64) * 16.0;
        let phase = (t_ms % 2500.0) / 2500.0;
        if phase < 0.4 {
            let cycle = (t_ms / 2500.0).floor();
            let dir = if (cycle as usize).is_multiple_of(2) { 1.0 } else { -1.0 };
            x += dir * 1.8;
            y = 360.0 + (t_ms * 0.002).sin() * 45.0;
        }
        pts.push(pt_t(x, y, t_ms));
    }
    pts
}

fn ten_sec_watercolor_vigorous_points() -> Vec<StrokePoint> {
    (0..600)
        .map(|i| {
            let t = i as f64 / 60.0;
            let x = 640.0 + (t * 5.5).sin() * 280.0 + (t * 11.3).cos() * 60.0;
            let y = 360.0 + (t * 7.1).sin() * 170.0 + (t * 9.7).sin() * 45.0;
            pt_t(x, y, t * 1000.0)
        })
        .collect()
}

fn twenty_low_flow_points(i: usize) -> Vec<StrokePoint> {
    let angle = (i as f64) * 0.314_159;
    let ca = angle.cos();
    let sa = angle.sin();
    (0..40)
        .map(|k| {
            let u = (k as f64 / 39.0 - 0.5) * 480.0;
            let v = ((k as f64 / 39.0) * std::f64::consts::TAU).sin() * 35.0 + ((i as f64) - 9.5) * 8.0;
            pt(640.0 + u * ca - v * sa, 360.0 + u * sa + v * ca)
        })
        .collect()
}

fn ten_hard_round_points(i: usize) -> Vec<StrokePoint> {
    (0..40)
        .map(|k| {
            let t = k as f64 / 39.0;
            let x = 440.0 + t * 400.0;
            let y = 220.0 + (i as f64) * 32.0 + (t * std::f64::consts::TAU).sin() * 24.0;
            pt(x, y)
        })
        .collect()
}

// =============================================================================
// Exact Restored CP1 §7.2 Fixture Builders
// =============================================================================

fn build_cp1_w1_airbrush(c: CanvasId, stroke_count: usize) -> (WorldState, Stroke) {
    let mut world = WorldState::new(c, owner(1), format!("CP1 W1 Airbrush Soft x{stroke_count}")).unwrap();
    let mut ab = preset_brush("Airbrush Soft");
    ab.size = 80.0;
    ab.hardness = 0.0;
    ab.flow = 0.10;
    ab.build_up = true;
    ab.color = [0.18, 0.42, 0.88, 1.0];
    let pts = ten_sec_airbrush_points();
    let mut last = None;
    for i in 0..stroke_count {
        let s = make_brush_stroke(c, 100 + i as u128, 1, &pts, &ab, 0, [0, 0]);
        append_stroke(&mut world, 1, s.clone());
        last = Some(s);
    }
    (world, last.unwrap())
}

fn build_cp1_w2_watercolor(c: CanvasId) -> (WorldState, Stroke) {
    let mut world = WorldState::new(c, owner(1), "CP1 W2 Watercolor Wet Edges".into()).unwrap();
    let mut wc = preset_brush("Watercolor Wet Edges");
    wc.size = 60.0;
    wc.hardness = 0.3;
    wc.wet_edges = true;
    wc.color = [0.12, 0.58, 0.52, 1.0];
    let pts = ten_sec_watercolor_vigorous_points();
    let s = make_brush_stroke(c, 200, 1, &pts, &wc, 0, [0, 0]);
    append_stroke(&mut world, 1, s.clone());
    (world, s)
}

fn build_cp1_w3_lowflow(c: CanvasId, stroke_count: usize) -> (WorldState, Stroke) {
    let mut world = WorldState::new(c, owner(1), format!("CP1 W3 Low-Flow 3% Soft Round x{stroke_count}")).unwrap();
    let mut lf = preset_brush("Soft Round");
    lf.size = 45.0;
    lf.hardness = 0.0;
    lf.flow = 0.03;
    lf.opacity = 1.0;
    lf.color = [0.20, 0.40, 0.90, 1.0];
    let mut last = None;
    for i in 0..stroke_count {
        let pts = twenty_low_flow_points(i % 20);
        let author = (i % 2 + 1) as u128;
        let s = make_brush_stroke(c, 300 + i as u128, author, &pts, &lf, 0, [0, 0]);
        append_stroke(&mut world, author, s.clone());
        last = Some(s);
    }
    (world, last.unwrap())
}

fn build_cp1_w4_hard_round(c: CanvasId) -> (WorldState, Stroke) {
    let mut world = WorldState::new(c, owner(1), "CP1 W4 Hard Round 10".into()).unwrap();
    let mut hr = preset_brush("Hard Round");
    hr.size = 30.0;
    hr.hardness = 1.0;
    hr.spacing = 0.05;
    hr.opacity = 1.0;
    hr.flow = 1.0;
    hr.color = [0.15, 0.35, 0.85, 1.0];
    let mut last = None;
    for i in 0..10 {
        let pts = ten_hard_round_points(i);
        let author = (i % 2 + 1) as u128;
        let s = make_brush_stroke(c, 400 + i as u128, author, &pts, &hr, 0, [0, 0]);
        append_stroke(&mut world, author, s.clone());
        last = Some(s);
    }
    (world, last.unwrap())
}

fn make_checker_rgba(w: u32, h: u32, m_idx: u128, f_idx: u8) -> Vec<u8> {
    let mut rgba = vec![0_u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let checker = ((x / 8 + y / 8 + u32::from(f_idx)) % 2) == 0;
            rgba[i] = match m_idx % 3 {
                0 => {
                    if checker {
                        235
                    } else {
                        45
                    }
                }
                1 => {
                    if checker {
                        55
                    } else {
                        215
                    }
                }
                _ => {
                    if checker {
                        220
                    } else {
                        90
                    }
                }
            };
            rgba[i + 1] = match m_idx % 3 {
                0 => {
                    if checker {
                        95
                    } else {
                        180
                    }
                }
                1 => {
                    if checker {
                        220
                    } else {
                        75
                    }
                }
                _ => {
                    if checker {
                        170
                    } else {
                        225
                    }
                }
            };
            rgba[i + 2] = if checker { 240 } else { 110 };
            rgba[i + 3] = 220;
        }
    }
    rgba
}

fn place_media_op(
    world: &mut WorldState,
    bank: &mut MediaBank,
    m_idx: u128,
    author: u128,
    offset: [f64; 2],
    size: [f64; 2],
    animated_frames: u8,
    duration_ms: u32,
) {
    let mut frames = Vec::new();
    for f_idx in 0..animated_frames.max(1) {
        let rgba = make_checker_rgba(64, 64, m_idx, f_idx);
        frames.push(bank.make_frame(64, 64, rgba, duration_ms));
    }
    let placement = MediaPlacement {
        id: MediaId::new(&hex(8800 + m_idx)).unwrap(),
        canvas: world.canvas,
        owner: owner(author),
        anchor: RegionKey::default(),
        offset,
        width: size[0],
        height: size[1],
        frames,
        looping: animated_frames > 1,
        provenance: Provenance {
            source: MediaSource::Canvas { canvas: canvas(999), media: MediaId::new(&hex(8900 + m_idx)).unwrap() },
            source_revision: 1,
            artifact_hash: AssetHash::new(&format!("{:064x}", 900 + m_idx)).unwrap(),
            authors: vec![Credit { profile: owner(author), label: format!("Artist {author}") }],
        },
    };
    append_action(world, author, Edit::PlaceMedia { media: Box::new(placement) });
}

/// Exact CP1 §7.2 W5: 120 revs = 90 strokes + 6 media (3 overlapping animated + 3 static) + 24 undo/redo,
/// across 6 devices covering Normal, Erase, Behind, Multiply, Screen, Overlay, Dissolve,
/// plus 2 overlapping peer preview strokes + 1 local template stroke!
fn build_cp1_w5_dynamic_overlap(c: CanvasId) -> (WorldState, MediaBank, Stroke, Vec<SharedStroke>) {
    let mut world = WorldState::new(c, owner(1), "CP1 W5 Multi-Author Dynamic Overlap".into()).unwrap();
    let mut bank = MediaBank::default();
    let palette =
        [[0.85, 0.22, 0.20, 1.0], [0.18, 0.52, 0.85, 1.0], [0.16, 0.66, 0.36, 1.0], [0.85, 0.56, 0.12, 1.0], [0.58, 0.26, 0.78, 1.0], [0.12, 0.62, 0.68, 1.0]];
    let modes = [
        (BlendMode::Normal, false, false),
        (BlendMode::Multiply, false, false),
        (BlendMode::Screen, false, false),
        (BlendMode::Overlay, false, false),
        (BlendMode::Dissolve, false, false),
        (BlendMode::Normal, false, true), // Behind
        (BlendMode::Normal, true, false), // Erase
    ];

    let mut stroke_ids: Vec<(u128, StrokeId)> = Vec::new();
    // Interleave 90 strokes, 6 media (3 overlapping animated at 120/180/250ms + 3 static), and 24 undo/redo = 120 revs exact!
    for i in 0..90_usize {
        if i == 15 {
            place_media_op(&mut world, &mut bank, 1, 1, [520.0, 270.0], [150.0, 150.0], 3, 120);
        } else if i == 30 {
            place_media_op(&mut world, &mut bank, 2, 2, [590.0, 310.0], [150.0, 150.0], 3, 180);
        } else if i == 45 {
            place_media_op(&mut world, &mut bank, 3, 3, [550.0, 240.0], [140.0, 140.0], 2, 250);
        } else if i == 60 {
            place_media_op(&mut world, &mut bank, 4, 4, [380.0, 220.0], [90.0, 90.0], 1, 100);
        } else if i == 70 {
            place_media_op(&mut world, &mut bank, 5, 5, [760.0, 220.0], [90.0, 90.0], 1, 100);
        } else if i == 80 {
            place_media_op(&mut world, &mut bank, 6, 6, [740.0, 410.0], [90.0, 90.0], 1, 100);
        }
        let author = (i % 6 + 1) as u128;
        let (mode, erase, behind) = modes[i % modes.len()];
        let mut b = preset_brush(if i % 3 == 0 { "Soft Round" } else { "Hard Round" });
        b.size = 24.0 + (i % 5) as f32 * 6.0;
        b.opacity = 0.72;
        b.flow = 0.55;
        b.mode = mode;
        b.erase = erase;
        b.behind = behind;
        b.color = palette[(author - 1) as usize];
        let angle = (i as f64) * 0.27;
        let pts: Vec<StrokePoint> = (0..20)
            .map(|k| {
                let u = (k as f64 / 19.0 - 0.5) * 320.0;
                let v = ((i % 9) as f64 - 4.0) * 18.0 + (u * 0.03).sin() * 14.0;
                pt(640.0 + u * angle.cos() - v * angle.sin(), 360.0 + u * angle.sin() + v * angle.cos())
            })
            .collect();
        let s = make_brush_stroke(c, 5000 + i as u128, author, &pts, &b, 0, [0, 0]);
        let sid = s.id;
        append_stroke(&mut world, author, s);
        stroke_ids.push((author, sid));
    }
    // 16 undos + 8 redos = 24 undo/redo ops -> total 90 + 6 + 24 = 120 revisions exact!
    for k in 0..16_usize {
        let (a, sid) = stroke_ids[k * 5];
        append_action(&mut world, a, Edit::SetStrokeVisible { stroke: sid, visible: false });
    }
    for k in 0..8_usize {
        let (a, sid) = stroke_ids[k * 5];
        append_action(&mut world, a, Edit::SetStrokeVisible { stroke: sid, visible: true });
    }
    assert_eq!(world.revision, 120);

    // Local live stroke + 2 simultaneous overlapping peer previews (authors 2 & 3).
    let mut local_b = preset_brush("Soft Round");
    local_b.size = 32.0;
    local_b.opacity = 0.75;
    local_b.flow = 0.35;
    local_b.color = [0.90, 0.25, 0.25, 1.0];
    let local_pts: Vec<StrokePoint> = (0..60).map(|k| pt(480.0 + k as f64 * 5.2, 340.0 + (k as f64 * 0.2).sin() * 45.0)).collect();
    let local_template = make_brush_stroke(c, 5990, 1, &local_pts, &local_b, 0, [0, 0]);

    let mut peer_previews = Vec::new();
    for peer_idx in 0..2_u128 {
        let mut pb = preset_brush("Soft Round");
        pb.size = 28.0;
        pb.opacity = 0.65;
        pb.mode = if peer_idx == 0 { BlendMode::Multiply } else { BlendMode::Normal };
        pb.color = palette[(peer_idx + 1) as usize];
        let ppts: Vec<StrokePoint> = (0..40).map(|k| pt(500.0 + k as f64 * 7.0, 290.0 + (peer_idx as f64) * 90.0 + (k as f64 * 0.25).cos() * 28.0)).collect();
        let ps = make_brush_stroke(c, 5991 + peer_idx, 2 + peer_idx, &ppts, &pb, 0, [0, 0]);
        peer_previews.push(SharedStroke {
            stroke: ps,
            provenance: None,
            device: device(2 + peer_idx),
            visible: true,
            created_revision: 999_001 + peer_idx as u64,
            changed_revision: 999_001 + peer_idx as u64,
        });
    }
    (world, bank, local_template, peer_previews)
}

/// Prior CP6 stress workload (4,500 strokes + 2 overlapping animated media) retained separately for direct
/// apples-to-apples before/after verification of the async animated-media fix.
fn build_extra_w5_stress_4500_media(c: CanvasId) -> (WorldState, MediaBank) {
    let mut world = WorldState::new(c, owner(1), "EXTRA W5 Stress 4,500 Strokes + 2 Animated Media".into()).unwrap();
    let mut bank = MediaBank::default();
    let palette = [
        [0.85, 0.22, 0.20, 0.85],
        [0.18, 0.52, 0.85, 0.85],
        [0.16, 0.66, 0.36, 0.85],
        [0.85, 0.56, 0.12, 0.85],
        [0.58, 0.26, 0.78, 0.85],
        [0.12, 0.62, 0.68, 0.85],
    ];
    place_media_op(&mut world, &mut bank, 51, 1, [-110.0, -70.0], [120.0, 120.0], 2, 250);
    place_media_op(&mut world, &mut bank, 52, 2, [30.0, -40.0], [120.0, 120.0], 2, 250);
    for i in 0..4_500_u64 {
        let author = (i % 6 + 1) as u128;
        let angle = (i as f64) * 0.13;
        let radius = 20.0 + ((i % 250) as f64) * 1.4;
        let cx = angle.cos() * radius;
        let cy = angle.sin() * radius * 0.65;
        let pts: Vec<[f64; 2]> = (0..12).map(|k| [cx + (k as f64 * 2.5) * angle.cos(), cy + (k as f64 * 2.5) * angle.sin()]).collect();
        let erase = i.is_multiple_of(37);
        let mode = match i % 29 {
            0 => BrushBlendMode::Multiply,
            1 => BrushBlendMode::Screen,
            _ => BrushBlendMode::Normal,
        };
        let behind = !erase && i.is_multiple_of(43);
        let brush = StrokeBrush {
            authored_size: 10.0 + (i % 12) as f32,
            hardness: if i.is_multiple_of(3) { 0.25 } else { 0.85 },
            spacing: 0.2,
            opacity: 0.75,
            flow: 0.6,
            mode,
            behind,
            seed: i,
            ..Default::default()
        };
        let s = Stroke {
            id: StrokeId::new(&hex(500_000 + u128::from(i))).unwrap(),
            canvas: c,
            owner: owner(author),
            revision: 0,
            anchor: RegionKey { depth: if i.is_multiple_of(20) { 6 } else { 0 }, ..Default::default() },
            points: pts,
            width: f64::from(brush.authored_size),
            color: Srgba { rgba: palette[(author - 1) as usize] },
            erase,
            brush: Some(Box::new(brush)),
            samples: None,
        };
        let sid = s.id;
        append_stroke(&mut world, author, s);
        if i % 300 == 150 {
            append_action(&mut world, author, Edit::SetStrokeVisible { stroke: sid, visible: false });
        }
    }
    (world, bank)
}

/// Exact CP1 §7.2 W6: 10,500 revs = 9,100 60-pt strokes (546,000 pts) across 6 authors & mixed builtin presets
/// spanning depths 0..=12 (including negative-coordinate seam strokes and depth-0/6/12 pockets for CP1 W7),
/// plus 910 undos (10%), 455 redos (5%), 25 media placements, and 10 multi-depth tours (0 <-> 6 <-> 12).
fn build_cp1_w6_rev10500(c: CanvasId) -> (WorldState, MediaBank, Stroke, Tour) {
    let mut world = WorldState::new(c, owner(1), "CP1 W6 Rev 10,500 Mixed-Depth Mural".into()).unwrap();
    let mut bank = MediaBank::default();
    let preset_specs = [
        ("Hard Round", 22.0_f32, 1.0_f32),
        ("Hard Round", 18.0, 0.85),
        ("Soft Round", 32.0, 0.55),
        ("Watercolor Wet Edges", 38.0, 0.65),
        ("Airbrush Soft", 42.0, 0.50),
        ("Chalk", 26.0, 0.85),
    ];
    let brushes: Vec<BrushSettings> = preset_specs
        .iter()
        .map(|&(name, sz, op)| {
            let mut b = preset_brush(name);
            b.size = sz;
            b.opacity = op;
            b
        })
        .collect();
    let colors = [
        [0.12_f32, 0.36, 0.86, 1.0],
        [0.86, 0.22, 0.24, 1.0],
        [0.16, 0.68, 0.42, 1.0],
        [0.88, 0.52, 0.14, 1.0],
        [0.54, 0.24, 0.78, 1.0],
        [0.20, 0.20, 0.24, 1.0],
    ];
    let depths: [u8; 10] = [0, 0, 0, 0, 1, 2, 4, 6, 9, 12];
    let mut undo_candidates: Vec<(u128, StrokeId)> = Vec::with_capacity(910);
    let mut draw_template = None;

    for i in 0..9_100_usize {
        let author = (i % 6 + 1) as u128;
        let mut b = brushes[i % brushes.len()].clone();
        b.color = colors[(author - 1) as usize];
        b.erase = i > 120 && i.is_multiple_of(47);
        let depth = depths[i % depths.len()];

        // First 90 strokes establish dense negative-coord seam + depth-0 ancestor + depth-6 & depth-12 pockets
        // right around (-120..-40, -80..-30) and (0,0) for CP1 W7 deep detail verification!
        let (anchor_xy, ox, oy) = if i < 30 {
            ([0_i128, 0_i128], -180.0 + ((i % 5) as f64) * 35.0, -120.0 + ((i / 5) as f64) * 38.0)
        } else if i < 60 && depth == 6 {
            ([0_i128, 0_i128], -160.0 + ((i % 6) as f64) * 28.0, -100.0 + ((i / 6) as f64) * 22.0)
        } else if i < 90 && depth == 12 {
            ([0_i128, 0_i128], -90.0 + ((i % 6) as f64) * 24.0, -70.0 + ((i / 6) as f64) * 18.0)
        } else if depth == 0 {
            let cx = ((i / 25) % 8) as i128 - 4;
            let cy = ((i / 200) % 8) as i128 - 4;
            ([cx, cy], -240.0 + ((i % 5) as f64) * 120.0, -180.0 + (((i / 5) % 5) as f64) * 90.0)
        } else {
            let cx = ((i / 10) % 8) as i128 - 4;
            let cy = ((i / 80) % 8) as i128 - 4;
            ([cx, cy], -120.0 + ((i % 5) as f64) * 65.0, -90.0 + (((i / 5) % 4) as f64) * 55.0)
        };
        let depth_used = if i < 30 {
            0
        } else if i < 45 {
            6
        } else if i < 60 {
            12
        } else {
            depth
        };
        let pts: Vec<StrokePoint> = (0..60)
            .map(|k| {
                let t = k as f64 / 59.0;
                pt_t(ox + t * 140.0, oy + (t * std::f64::consts::TAU).sin() * 22.0, (k as f64) * 16.0)
            })
            .collect();
        let s = make_brush_stroke(c, 800_000 + i as u128, author, &pts, &b, depth_used, anchor_xy);
        let sid = s.id;
        if i == 5 {
            draw_template = Some(s.clone());
        }
        append_stroke(&mut world, author, s);
        if i % 10 == 9 {
            undo_candidates.push((author, sid));
        }
    }
    // 910 undos (10%) + 455 redos (5%)
    for &(a, sid) in &undo_candidates {
        append_action(&mut world, a, Edit::SetStrokeVisible { stroke: sid, visible: false });
    }
    for &(a, sid) in undo_candidates.iter().take(455) {
        append_action(&mut world, a, Edit::SetStrokeVisible { stroke: sid, visible: true });
    }
    // 25 media placements (including 3 animated)
    for m in 0..25_u128 {
        let off = [-300.0 + (m % 5) as f64 * 150.0, -200.0 + (m / 5) as f64 * 90.0];
        place_media_op(&mut world, &mut bank, 200 + m, m % 6 + 1, off, [96.0, 96.0], if m < 3 { 2 } else { 1 }, 240);
    }
    // 10 tours spanning depths 0 <-> 6 <-> 12!
    let mut main_tour = None;
    for t in 0..10_u128 {
        let p0 = coordinates::point(&RegionKey::default(), [-80.0 + (t as f64) * 10.0, -50.0]).unwrap();
        let p6 = coordinates::point(&RegionKey { depth: 6, ..Default::default() }, [-120.0, -80.0]).unwrap();
        let p12 = coordinates::point(&RegionKey { depth: 12, ..Default::default() }, [-40.0, -30.0]).unwrap();
        let tour = Tour {
            id: TourId::new(&hex(9_000 + t)).unwrap(),
            canvas: c,
            name: format!("Deep Flight {t}"),
            revision: 0,
            stops: vec![
                TourStop { camera: coordinates::target(c, p0, 0, 1.0).unwrap(), travel_ms: 0, pause_ms: 100, target: None },
                TourStop { camera: coordinates::target(c, p6, 6, 1.37).unwrap(), travel_ms: 1200, pause_ms: 100, target: None },
                TourStop { camera: coordinates::target(c, p12, 12, 2.23).unwrap(), travel_ms: 1200, pause_ms: 100, target: None },
                TourStop { camera: coordinates::target(c, p0, 0, 0.5).unwrap(), travel_ms: 1200, pause_ms: 100, target: None },
            ],
        };
        if t == 0 {
            main_tour = Some(tour.clone());
        }
        append_action(&mut world, t % 6 + 1, Edit::PutTour { tour, expected_revision: 0 });
    }
    assert_eq!(world.revision, 10_500);
    (world, bank, draw_template.unwrap(), main_tour.unwrap())
}

#[derive(Clone, Debug, Serialize)]
struct CombinedReopenStats {
    total_ms: SettleStats,
    outbox_load_ms: SettleStats,
    index_build_ms: SettleStats,
    tile_crisp_ms: SettleStats,
    disk_hits_last_trial: u64,
}

fn bench_w8_combined_reopen(
    gpu: &mut WorldGpu,
    w6_world: &WorldState,
    w6_media: &MediaBank,
    view: &Viewport,
) -> (CombinedReopenStats, CombinedReopenStats, u64, u64) {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("w8-client-v4.sqlite3");
    let tile_dir = tmp.path().join("tile-cache");
    let scope = WorldScope::new("http://127.0.0.1:1", owner(1), device(1)).unwrap();

    // Seed real WorldOutbox v4 DB with a rev-10,240 checkpoint + 260 incremental tail events (revs 10,241..=10,500),
    // matching exact production steady-state storage on a 10,500-rev canvas!
    {
        let mut q = WorldOutbox::open(&db_path).unwrap();
        let lease = q.activate(scope.clone()).unwrap();
        q.grant(&lease, w6_world.canvas, 1, 1).unwrap();
        let prefix = w6_world.replay_to(10_240).unwrap();
        q.save_snapshot(&lease, &prefix).unwrap();
        let info = CanvasSummary {
            canvas: w6_world.canvas,
            owner: w6_world.owner,
            name: w6_world.name.clone(),
            initial_name: w6_world.initial_name().to_owned(),
            revision: w6_world.revision,
            epoch: 1,
            import_origin: None,
        };
        let mut after = 10_240_u64;
        let mut appended = 0_usize;
        while after < w6_world.revision {
            let page = w6_world.events_after(after, 256).unwrap();
            assert!(!page.is_empty());
            after = page.last().unwrap().revision;
            appended += page.len();
            q.append_accepted_events(&lease, &info, page, None).unwrap();
        }
        assert_eq!(appended, 260);
    }
    let sqlite_bytes = std::fs::metadata(&db_path).unwrap().len();
    let archive_v3_bytes = ArchiveManifest { schema: 3, world: w6_world.clone(), assets: Vec::new() }.encode_v3().unwrap().len() as u64;

    let run_reopen = |gpu: &mut WorldGpu, scope_id: u64, purge_disk: bool| -> (f64, f64, f64, f64, u64) {
        if purge_disk {
            let _ = std::fs::remove_dir_all(&tile_dir);
        }
        gpu.purge_slots();
        let t_total = Instant::now();
        let t0 = Instant::now();
        let mut q = WorldOutbox::open(&db_path).unwrap();
        let lease = q.activate(scope.clone()).unwrap();
        let loaded = q.load_snapshot(&lease, w6_world.canvas).unwrap().unwrap();
        assert_eq!(loaded.revision, 10_500);
        let outbox_ms = t0.elapsed().as_secs_f64() * 1000.0;

        let t1 = Instant::now();
        let idx = Arc::new(WorldSceneIndex::new(&loaded).unwrap());
        let idx_ms = t1.elapsed().as_secs_f64() * 1000.0;

        let t2 = Instant::now();
        let mut mgr = WorldTileManager::new(TileBudget::desktop(), Some(tile_dir.clone()));
        mgr.set_scope(ScopeKey::new(scope_id, "bench", "u1", "d1", loaded.canvas, None), idx);
        w6_media.register_in(&mut mgr);
        let _ = wait_until_crisp_pair(gpu, &mut mgr, view, 1.0, 0, w6_media);
        let tile_ms = t2.elapsed().as_secs_f64() * 1000.0;
        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;
        (total_ms, outbox_ms, idx_ms, tile_ms, mgr.stats().disk_hits)
    };

    let mut cold_tot = Vec::with_capacity(10);
    let mut cold_db = Vec::with_capacity(10);
    let mut cold_idx = Vec::with_capacity(10);
    let mut cold_tile = Vec::with_capacity(10);
    let mut cold_hits = 0_u64;
    for t in 0..10_u64 {
        let (tot, db, ix, ti, h) = run_reopen(gpu, 8000 + t, true);
        cold_tot.push(tot);
        cold_db.push(db);
        cold_idx.push(ix);
        cold_tile.push(ti);
        cold_hits = h;
    }

    // 3 warmups + 10 warm disk-cache trials (per CP1 §7.1 repetitions).
    for t in 0..3_u64 {
        let _ = run_reopen(gpu, 8100 + t, false);
    }
    let mut warm_tot = Vec::with_capacity(10);
    let mut warm_db = Vec::with_capacity(10);
    let mut warm_idx = Vec::with_capacity(10);
    let mut warm_tile = Vec::with_capacity(10);
    let mut warm_hits = 0_u64;
    for t in 0..10_u64 {
        let (tot, db, ix, ti, h) = run_reopen(gpu, 8200 + t, false);
        warm_tot.push(tot);
        warm_db.push(db);
        warm_idx.push(ix);
        warm_tile.push(ti);
        warm_hits = h;
    }
    (
        CombinedReopenStats {
            total_ms: summarize_settle(cold_tot),
            outbox_load_ms: summarize_settle(cold_db),
            index_build_ms: summarize_settle(cold_idx),
            tile_crisp_ms: summarize_settle(cold_tile),
            disk_hits_last_trial: cold_hits,
        },
        CombinedReopenStats {
            total_ms: summarize_settle(warm_tot),
            outbox_load_ms: summarize_settle(warm_db),
            index_build_ms: summarize_settle(warm_idx),
            tile_crisp_ms: summarize_settle(warm_tile),
            disk_hits_last_trial: warm_hits,
        },
        sqlite_bytes,
        archive_v3_bytes,
    )
}

#[test]
#[ignore = "CP7 restored CP1 §7.2 --release W1-W8 acceptance benchmark and visual PNG audit"]
#[allow(clippy::cognitive_complexity)]
fn run_cp1_restored_w1_to_w8_release_acceptance() {
    let _lock = photocraft_testkit::gpu_lock();
    let rss = RssSampler::start(Duration::from_millis(5));
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out_dir = repo_root.join("log/k7/qol-009-redesign-dev/cp7-async-and-e2e");
    let renders_dir = out_dir.join("renders");
    std::fs::create_dir_all(&renders_dir).unwrap();

    let rs = egui_kittest::wgpu::create_render_state(crate::gpu_canvas::wgpu_setup(), Default::default());
    let adapter_info = rs.adapter.get_info();
    let health = photocraft_gpu::DeviceHealth::watch(&rs.device);
    let mut gpu = WorldGpu::new_with_budget(&rs, health.clone(), TileBudget::desktop()).unwrap();
    let mut mgr = WorldTileManager::new(TileBudget::desktop(), None);
    let empty_media = MediaBank::default();

    // =========================================================================
    // CP1 W1: 10s Airbrush Soft (size=80, hard=0, flow=0.10, build_up, 625 pts / 337 dabs)
    //         Both 1 stroke AND 2 overlapping strokes @ 1x, 4x, 16x
    // =========================================================================
    let (w1_1_world, w1_stroke) = build_cp1_w1_airbrush(canvas(1), 1);
    let w1_1_meta = fixture_meta(&w1_1_world, true);
    let w1_1_idx = Arc::new(WorldSceneIndex::new(&w1_1_world).unwrap());
    let w1_v1x = view_at(w1_1_world.canvas, 1280, 720, 0, [640.0, 360.0], 1.0);
    let w1_v4x = view_at(w1_1_world.canvas, 1280, 720, 0, [640.0, 360.0], 4.0);
    let w1_v16x = view_at(w1_1_world.canvas, 1280, 720, 0, [640.0, 360.0], 16.0);
    let w1_1_settle_1x = bench_settle_10(&mut gpu, &mut mgr, &w1_1_idx, 100, &w1_v1x, 1.0, &empty_media);
    let w1_1_settle_4x = bench_settle_10(&mut gpu, &mut mgr, &w1_1_idx, 120, &w1_v4x, 1.0, &empty_media);
    let w1_1_settle_16x = bench_settle_10(&mut gpu, &mut mgr, &w1_1_idx, 140, &w1_v16x, 1.0, &empty_media);
    let w1_1_parity = measure_gpu_vs_cpu_parity(&gpu, &w1_1_idx, &w1_v16x, 0, &empty_media);
    save_gpu_png(&gpu, &renders_dir.join("01_cp1_w1_airbrush_1xstroke_16x.png"));
    let w1_1_pan_1x = measure_pan_zoom(&mut gpu, &mut mgr, &w1_v1x, 1.0, 40.0, true, &[], &empty_media);
    let w1_1_pan_4x = measure_pan_zoom(&mut gpu, &mut mgr, &w1_v4x, 1.0, 25.0, true, &[], &empty_media);
    let w1_1_pan_16x = measure_pan_zoom(&mut gpu, &mut mgr, &w1_v16x, 1.0, 15.0, true, &[], &empty_media);
    let w1_1_pan_16x_1080 = measure_pan_zoom(&mut gpu, &mut mgr, &w1_v16x, 1.5, 15.0, true, &[], &empty_media);
    let w1_1_pan_16x_1440 = measure_pan_zoom(&mut gpu, &mut mgr, &w1_v16x, 2.0, 15.0, true, &[], &empty_media);
    let w1_1_draw_16x = measure_live_draw(&mut gpu, &mut mgr, &w1_v16x, 1.0, &w1_stroke, 1, &[], &empty_media);
    let w1_1_draw_16x_1080 = measure_live_draw(&mut gpu, &mut mgr, &w1_v16x, 1.5, &w1_stroke, 1, &[], &empty_media);

    let (w1_2_world, _) = build_cp1_w1_airbrush(canvas(11), 2);
    let w1_2_meta = fixture_meta(&w1_2_world, true);
    let w1_2_idx = Arc::new(WorldSceneIndex::new(&w1_2_world).unwrap());
    let w1_2_v16x = view_at(w1_2_world.canvas, 1280, 720, 0, [640.0, 360.0], 16.0);
    let w1_2_settle_16x = bench_settle_10(&mut gpu, &mut mgr, &w1_2_idx, 160, &w1_2_v16x, 1.0, &empty_media);
    let w1_2_pan_16x = measure_pan_zoom(&mut gpu, &mut mgr, &w1_2_v16x, 1.0, 15.0, true, &[], &empty_media);
    let w1_2_pan_16x_1080 = measure_pan_zoom(&mut gpu, &mut mgr, &w1_2_v16x, 1.5, 15.0, true, &[], &empty_media);
    eprintln!(
        "[CP7] CP1 W1: 1-stroke ({:?} dabs) 16x settle_p95={:.2}ms pan_p95={:.2}ms draw_p95={:.2}ms; 2-stroke ({:?} dabs) 16x settle_p95={:.2}ms",
        w1_1_meta.dabs, w1_1_settle_16x.crisp.p95_ms, w1_1_pan_16x.p95_ms, w1_1_draw_16x.p95_ms, w1_2_meta.dabs, w1_2_settle_16x.crisp.p95_ms
    );

    // =========================================================================
    // CP1 W2: 10s Watercolor Wet Edges vigorous (size=60, hard=0.3, Paper(256), 600 pts / ~926 dabs) @ 1x, 4x, 16x
    // =========================================================================
    let (w2_world, w2_stroke) = build_cp1_w2_watercolor(canvas(2));
    let w2_meta = fixture_meta(&w2_world, true);
    let w2_idx = Arc::new(WorldSceneIndex::new(&w2_world).unwrap());
    let w2_v1x = view_at(w2_world.canvas, 1280, 720, 0, [640.0, 360.0], 1.0);
    let w2_v4x = view_at(w2_world.canvas, 1280, 720, 0, [640.0, 360.0], 4.0);
    let w2_v16x = view_at(w2_world.canvas, 1280, 720, 0, [640.0, 360.0], 16.0);
    let w2_settle_1x = bench_settle_10(&mut gpu, &mut mgr, &w2_idx, 200, &w2_v1x, 1.0, &empty_media);
    let w2_settle_4x = bench_settle_10(&mut gpu, &mut mgr, &w2_idx, 220, &w2_v4x, 1.0, &empty_media);
    let w2_parity_4x = measure_gpu_vs_cpu_parity(&gpu, &w2_idx, &w2_v4x, 0, &empty_media);
    save_gpu_png(&gpu, &renders_dir.join("02_cp1_w2_watercolor_4x.png"));
    let w2_settle_16x = bench_settle_10(&mut gpu, &mut mgr, &w2_idx, 240, &w2_v16x, 1.0, &empty_media);
    let w2_pan_1x = measure_pan_zoom(&mut gpu, &mut mgr, &w2_v1x, 1.0, 60.0, false, &[], &empty_media);
    let w2_pan_4x = measure_pan_zoom(&mut gpu, &mut mgr, &w2_v4x, 1.0, 30.0, true, &[], &empty_media);
    let w2_pan_16x = measure_pan_zoom(&mut gpu, &mut mgr, &w2_v16x, 1.0, 15.0, true, &[], &empty_media);
    let w2_pan_4x_1080 = measure_pan_zoom(&mut gpu, &mut mgr, &w2_v4x, 1.5, 30.0, true, &[], &empty_media);
    let w2_draw_4x = measure_live_draw(&mut gpu, &mut mgr, &w2_v4x, 1.0, &w2_stroke, 1, &[], &empty_media);
    let w2_draw_16x = measure_live_draw(&mut gpu, &mut mgr, &w2_v16x, 1.0, &w2_stroke, 1, &[], &empty_media);
    eprintln!(
        "[CP7] CP1 W2 ({:?} dabs): settle 1x={:.2}ms 4x={:.2}ms 16x={:.2}ms | pan4x_p95={:.2}ms draw4x_p95={:.2}ms parity_max_delta={}/255",
        w2_meta.dabs,
        w2_settle_1x.crisp.p95_ms,
        w2_settle_4x.crisp.p95_ms,
        w2_settle_16x.crisp.p95_ms,
        w2_pan_4x.p95_ms,
        w2_draw_4x.p95_ms,
        w2_parity_4x.max_channel_delta_u8
    );

    // =========================================================================
    // CP1 W3: Low-flow 3% Soft Round (size=45, hard=0) — 20 AND 40 overlapping strokes @ 1x, 4x, 16x
    // =========================================================================
    let (w3_20_world, w3_stroke) = build_cp1_w3_lowflow(canvas(3), 20);
    let w3_20_meta = fixture_meta(&w3_20_world, true);
    let w3_20_idx = Arc::new(WorldSceneIndex::new(&w3_20_world).unwrap());
    let w3_20_v1x = view_at(w3_20_world.canvas, 1280, 720, 0, [640.0, 360.0], 1.0);
    let w3_20_v4x = view_at(w3_20_world.canvas, 1280, 720, 0, [640.0, 360.0], 4.0);
    let w3_20_v16x = view_at(w3_20_world.canvas, 1280, 720, 0, [640.0, 360.0], 16.0);
    let w3_20_settle_1x = bench_settle_10(&mut gpu, &mut mgr, &w3_20_idx, 300, &w3_20_v1x, 1.0, &empty_media);
    let w3_20_settle_4x = bench_settle_10(&mut gpu, &mut mgr, &w3_20_idx, 320, &w3_20_v4x, 1.0, &empty_media);
    let w3_20_settle_16x = bench_settle_10(&mut gpu, &mut mgr, &w3_20_idx, 340, &w3_20_v16x, 1.0, &empty_media);
    let w3_20_parity = measure_gpu_vs_cpu_parity(&gpu, &w3_20_idx, &w3_20_v16x, 0, &empty_media);
    save_gpu_png(&gpu, &renders_dir.join("03_cp1_w3_lowflow_20_16x.png"));
    let w3_20_pan_16x = measure_pan_zoom(&mut gpu, &mut mgr, &w3_20_v16x, 1.0, 15.0, true, &[], &empty_media);
    let w3_20_pan_16x_1080 = measure_pan_zoom(&mut gpu, &mut mgr, &w3_20_v16x, 1.5, 15.0, true, &[], &empty_media);
    let w3_20_draw_16x = measure_live_draw(&mut gpu, &mut mgr, &w3_20_v16x, 1.0, &w3_stroke, 1, &[], &empty_media);

    let (w3_40_world, w3_40_stroke) = build_cp1_w3_lowflow(canvas(31), 40);
    let w3_40_meta = fixture_meta(&w3_40_world, true);
    let w3_40_idx = Arc::new(WorldSceneIndex::new(&w3_40_world).unwrap());
    let w3_40_v16x = view_at(w3_40_world.canvas, 1280, 720, 0, [640.0, 360.0], 16.0);
    let w3_40_settle_16x = bench_settle_10(&mut gpu, &mut mgr, &w3_40_idx, 360, &w3_40_v16x, 1.0, &empty_media);
    let w3_40_pan_16x = measure_pan_zoom(&mut gpu, &mut mgr, &w3_40_v16x, 1.0, 15.0, true, &[], &empty_media);
    let w3_40_draw_16x = measure_live_draw(&mut gpu, &mut mgr, &w3_40_v16x, 1.0, &w3_40_stroke, 1, &[], &empty_media);
    eprintln!(
        "[CP7] CP1 W3: 20x ({:?} dabs) 16x settle_p95={:.2}ms pan_p95={:.2}ms draw_p95={:.2}ms; 40x ({:?} dabs) 16x settle_p95={:.2}ms",
        w3_20_meta.dabs, w3_20_settle_16x.crisp.p95_ms, w3_20_pan_16x.p95_ms, w3_20_draw_16x.p95_ms, w3_40_meta.dabs, w3_40_settle_16x.crisp.p95_ms
    );

    // =========================================================================
    // CP1 W4: Hard Round check (10 plain opaque strokes, size=30, hard=1.0, spacing=0.05) @ 1x, 4x, 16x + d6/d12 ancestor
    // =========================================================================
    let (w4_world, w4_stroke) = build_cp1_w4_hard_round(canvas(4));
    let w4_meta = fixture_meta(&w4_world, true);
    let w4_idx = Arc::new(WorldSceneIndex::new(&w4_world).unwrap());
    let w4_v1x = view_at(w4_world.canvas, 1280, 720, 0, [640.0, 360.0], 1.0);
    let w4_v4x = view_at(w4_world.canvas, 1280, 720, 0, [640.0, 360.0], 4.0);
    let w4_v16x = view_at(w4_world.canvas, 1280, 720, 0, [640.0, 360.0], 16.0);
    let pt_d0_center = coordinates::point(&RegionKey::default(), [640.0, 360.0]).unwrap();
    let w4_v_anc_d6 = Viewport { camera: coordinates::target(w4_world.canvas, pt_d0_center, 6, 1.0).unwrap(), width: 1280, height: 720 };
    let w4_v_anc_d12 = Viewport { camera: coordinates::target(w4_world.canvas, pt_d0_center, 12, 1.0).unwrap(), width: 1280, height: 720 };
    let w4_settle_1x = bench_settle_10(&mut gpu, &mut mgr, &w4_idx, 400, &w4_v1x, 1.0, &empty_media);
    let w4_settle_4x = bench_settle_10(&mut gpu, &mut mgr, &w4_idx, 420, &w4_v4x, 1.0, &empty_media);
    let w4_parity_4x = measure_gpu_vs_cpu_parity(&gpu, &w4_idx, &w4_v4x, 0, &empty_media);
    let w4_settle_16x = bench_settle_10(&mut gpu, &mut mgr, &w4_idx, 440, &w4_v16x, 1.0, &empty_media);
    let w4_settle_anc_d6 = bench_settle_10(&mut gpu, &mut mgr, &w4_idx, 460, &w4_v_anc_d6, 1.0, &empty_media);
    let w4_settle_anc_d12 = bench_settle_10(&mut gpu, &mut mgr, &w4_idx, 480, &w4_v_anc_d12, 1.0, &empty_media);
    let w4_pan_4x = measure_pan_zoom(&mut gpu, &mut mgr, &w4_v4x, 1.0, 30.0, true, &[], &empty_media);
    let w4_draw_4x = measure_live_draw(&mut gpu, &mut mgr, &w4_v4x, 1.0, &w4_stroke, 1, &[], &empty_media);
    let w4_pan_anc_d12 = measure_pan_zoom(&mut gpu, &mut mgr, &w4_v_anc_d12, 1.0, 30.0, true, &[], &empty_media);
    eprintln!(
        "[CP7] CP1 W4 ({:?} dabs): settle 4x={:.2}ms d6_anc={:.2}ms d12_anc={:.2}ms | pan4x_p95={:.2}ms draw4x_p95={:.2}ms parity={}/255",
        w4_meta.dabs,
        w4_settle_4x.crisp.p95_ms,
        w4_settle_anc_d6.crisp.p95_ms,
        w4_settle_anc_d12.crisp.p95_ms,
        w4_pan_4x.p95_ms,
        w4_draw_4x.p95_ms,
        w4_parity_4x.max_channel_delta_u8
    );

    // =========================================================================
    // CP1 W5: Multi-author combined dynamic overlap (120 revs, 6 devices, all blend modes,
    //         3 overlapping animated media + 3 static media, 24 undo/redo, simultaneous local + 2 peer previews)
    // =========================================================================
    let (w5_world, w5_media, w5_local_stroke, w5_peers) = build_cp1_w5_dynamic_overlap(canvas(5));
    let w5_meta = fixture_meta(&w5_world, true);
    let w5_idx = Arc::new(WorldSceneIndex::new(&w5_world).unwrap());
    let w5_v720 = view_at(w5_world.canvas, 1280, 720, 0, [640.0, 360.0], 1.0);
    let w5_settle = bench_settle_10(&mut gpu, &mut mgr, &w5_idx, 500, &w5_v720, 1.0, &w5_media);
    let w5_parity = measure_gpu_vs_cpu_parity(&gpu, &w5_idx, &w5_v720, 0, &w5_media);
    save_gpu_png(&gpu, &renders_dir.join("04_cp1_w5_dynamic_overlap.png"));
    let w5_pan_720 = measure_pan_zoom(&mut gpu, &mut mgr, &w5_v720, 1.0, 50.0, true, &w5_peers, &w5_media);
    let w5_pan_1080 = measure_pan_zoom(&mut gpu, &mut mgr, &w5_v720, 1.5, 50.0, true, &w5_peers, &w5_media);
    let w5_draw_720 = measure_live_draw(&mut gpu, &mut mgr, &w5_v720, 1.0, &w5_local_stroke, 1, &w5_peers, &w5_media);
    let w5_draw_1080 = measure_live_draw(&mut gpu, &mut mgr, &w5_v720, 1.5, &w5_local_stroke, 1, &w5_peers, &w5_media);
    eprintln!(
        "[CP7] CP1 W5 ({} revs): settle_p95={:.2}ms pan720_p95={:.2}ms max={:.2}ms draw720_p95={:.2}ms max={:.2}ms parity={}/255",
        w5_meta.revisions,
        w5_settle.crisp.p95_ms,
        w5_pan_720.p95_ms,
        w5_pan_720.worst_trial_max_ms,
        w5_draw_720.p95_ms,
        w5_draw_720.worst_trial_max_ms,
        w5_parity.max_channel_delta_u8
    );

    // EXTRA: Prior CP6 W5 stress workload (4,500 strokes + 2 overlapping animated media) — apples-to-apples async anim proof!
    let (w5s_world, w5s_media) = build_extra_w5_stress_4500_media(canvas(55));
    let w5s_meta = fixture_meta(&w5s_world, false);
    let w5s_idx = Arc::new(WorldSceneIndex::new(&w5s_world).unwrap());
    let w5s_v720 = Viewport { camera: coordinates::home(w5s_world.canvas), width: 1280, height: 720 };
    let w5s_settle = bench_settle_10(&mut gpu, &mut mgr, &w5s_idx, 550, &w5s_v720, 1.0, &w5s_media);
    let w5s_pan_720 = measure_pan_zoom(&mut gpu, &mut mgr, &w5s_v720, 1.0, 80.0, true, &[], &w5s_media);
    let w5s_pan_1080 = measure_pan_zoom(&mut gpu, &mut mgr, &w5s_v720, 1.5, 80.0, true, &[], &w5s_media);
    let w5s_pan_1440 = measure_pan_zoom(&mut gpu, &mut mgr, &w5s_v720, 2.0, 80.0, true, &[], &w5s_media);
    eprintln!(
        "[CP7] EXTRA W5-stress (4,500 strokes + 2 anim media): pan720 p95={:.2}ms max={:.2}ms worst={:.2}ms (was 13.14ms before async anim!)",
        w5s_pan_720.p95_ms, w5s_pan_720.max_ms, w5s_pan_720.worst_trial_max_ms
    );

    // =========================================================================
    // CP1 W6: Dense 10,500-rev mixed-depth (0..=12) overview (6 authors, mixed presets, 60 pts/stroke,
    //         10% undo + 5% redo + 25 media + 10 tours) @ depth 0 (0.5x & 1x)
    // =========================================================================
    let (w6_world, w6_media, w6_draw_stroke, w6_tour) = build_cp1_w6_rev10500(canvas(6));
    let w6_meta = fixture_meta(&w6_world, false);
    let t_idx0 = Instant::now();
    let w6_idx = Arc::new(WorldSceneIndex::new(&w6_world).unwrap());
    let w6_index_build_ms = t_idx0.elapsed().as_secs_f64() * 1000.0;
    let w6_v1x = view_at(w6_world.canvas, 1280, 720, 0, [0.0, 0.0], 1.0);
    let w6_v05x = view_at(w6_world.canvas, 1280, 720, 0, [0.0, 0.0], 0.5);
    let w6_settle_1x = bench_settle_10(&mut gpu, &mut mgr, &w6_idx, 600, &w6_v1x, 1.0, &w6_media);
    save_gpu_png(&gpu, &renders_dir.join("05_cp1_w6_rev10500_overview_1x.png"));
    let w6_settle_05x = bench_settle_10(&mut gpu, &mut mgr, &w6_idx, 620, &w6_v05x, 1.0, &w6_media);
    let w6_pan_1x_720 = measure_pan_zoom(&mut gpu, &mut mgr, &w6_v1x, 1.0, 120.0, true, &[], &w6_media);
    let w6_pan_05x_720 = measure_pan_zoom(&mut gpu, &mut mgr, &w6_v05x, 1.0, 120.0, true, &[], &w6_media);
    let w6_pan_1x_1080 = measure_pan_zoom(&mut gpu, &mut mgr, &w6_v1x, 1.5, 120.0, true, &[], &w6_media);
    let w6_pan_1x_1440 = measure_pan_zoom(&mut gpu, &mut mgr, &w6_v1x, 2.0, 120.0, true, &[], &w6_media);
    let w6_draw_1x_720 = measure_live_draw(&mut gpu, &mut mgr, &w6_v1x, 1.0, &w6_draw_stroke, 1, &[], &w6_media);
    eprintln!(
        "[CP7] CP1 W6 ({} revs, {} pts): idx={:.2}ms | 1x coarse_p95={:.2}ms crisp_p95={:.2}ms | 0.5x crisp_p95={:.2}ms | pan1x_p95={:.2}ms draw_p95={:.2}ms",
        w6_meta.revisions,
        w6_meta.points,
        w6_index_build_ms,
        w6_settle_1x.coarse.p95_ms,
        w6_settle_1x.crisp.p95_ms,
        w6_settle_05x.crisp.p95_ms,
        w6_pan_1x_720.p95_ms,
        w6_draw_1x_720.p95_ms
    );

    // =========================================================================
    // CP1 W7: Sparse deep detail INSIDE CP1 W6 (depths 6 & 12 + d0 ancestors + negative coords + fractional zoom + tour flight 0 <-> 6 <-> 12)
    // =========================================================================
    let w7_v_d6 = view_at(w6_world.canvas, 1280, 720, 6, [-120.0, -80.0], 1.37);
    let w7_v_d12 = view_at(w6_world.canvas, 1280, 720, 12, [-40.0, -30.0], 2.23);
    let w7_settle_d6 = bench_settle_10(&mut gpu, &mut mgr, &w6_idx, 700, &w7_v_d6, 1.0, &w6_media);
    let w7_pan_d6 = measure_pan_zoom(&mut gpu, &mut mgr, &w7_v_d6, 1.0, 40.0, true, &[], &w6_media);
    let w7_settle_d12 = bench_settle_10(&mut gpu, &mut mgr, &w6_idx, 720, &w7_v_d12, 1.0, &w6_media);
    save_gpu_png(&gpu, &renders_dir.join("06_cp1_w7_deep_d12_inside_w6.png"));
    let w7_pan_d12 = measure_pan_zoom(&mut gpu, &mut mgr, &w7_v_d12, 1.0, 40.0, true, &[], &w6_media);
    let mut w7_d12_stroke = w6_draw_stroke.clone();
    w7_d12_stroke.anchor = RegionKey { depth: 12, ..Default::default() };
    let w7_draw_d12 = measure_live_draw(&mut gpu, &mut mgr, &w7_v_d12, 1.0, &w7_d12_stroke, 1, &[], &w6_media);
    let w7_tour_flight = measure_tour_flight(&mut gpu, &mut mgr, &w6_tour, 1280, 720, 1.0, &w6_media);
    eprintln!(
        "[CP7] CP1 W7 (inside W6): d6_settle={:.2}ms d12_settle={:.2}ms d12_pan_p95={:.2}ms d12_draw_p95={:.2}ms tour_flight_p95={:.2}ms",
        w7_settle_d6.crisp.p95_ms, w7_settle_d12.crisp.p95_ms, w7_pan_d12.p95_ms, w7_draw_d12.p95_ms, w7_tour_flight.p95_ms
    );

    // =========================================================================
    // CP1 W8: Combined End-to-End Cold Reopen on CP1 W6 (10,500 revs)
    //         WorldOutbox::open + activate + load_snapshot + WorldSceneIndex::new + WorldTileManager + crisp settle
    // =========================================================================
    let (w8_cold_reopen, w8_warm_reopen, w8_client_sqlite_bytes, w8_archive_v3_bytes) = bench_w8_combined_reopen(&mut gpu, &w6_world, &w6_media, &w6_v1x);
    eprintln!(
        "[CP7] CP1 W8 combined reopen: cold_p95={:.2}ms (db={:.2} idx={:.2} tile={:.2}) | warm_p95={:.2}ms (db={:.2} idx={:.2} tile={:.2} hits={}) | client_sqlite={:.2}MiB archive_v3={:.2}MiB",
        w8_cold_reopen.total_ms.p95_ms,
        w8_cold_reopen.outbox_load_ms.p95_ms,
        w8_cold_reopen.index_build_ms.p95_ms,
        w8_cold_reopen.tile_crisp_ms.p95_ms,
        w8_warm_reopen.total_ms.p95_ms,
        w8_warm_reopen.outbox_load_ms.p95_ms,
        w8_warm_reopen.index_build_ms.p95_ms,
        w8_warm_reopen.tile_crisp_ms.p95_ms,
        w8_warm_reopen.disk_hits_last_trial,
        w8_client_sqlite_bytes as f64 / (1024.0 * 1024.0),
        w8_archive_v3_bytes as f64 / (1024.0 * 1024.0),
    );

    // =========================================================================
    // Constrained Desktop Proxy (TileBudget::conservative, 128 GPU layers, <= 4 workers)
    // Explicitly labeled as desktop proxy; real laptop hardware remains NOT VERIFIED.
    // =========================================================================
    let mut cons_budget = TileBudget::conservative();
    cons_budget.worker_threads = cons_budget.worker_threads.min(4);
    let mut gpu_cons = WorldGpu::new_with_budget(&rs, health.clone(), cons_budget).unwrap();
    let mut mgr_cons = WorldTileManager::new(cons_budget, None);

    let cons_w1_settle = bench_settle_10(&mut gpu_cons, &mut mgr_cons, &w1_1_idx, 9100, &w1_v16x, 1.0, &empty_media);
    let cons_w2_settle = bench_settle_10(&mut gpu_cons, &mut mgr_cons, &w2_idx, 9120, &w2_v4x, 1.0, &empty_media);
    let cons_w3_settle = bench_settle_10(&mut gpu_cons, &mut mgr_cons, &w3_20_idx, 9140, &w3_20_v16x, 1.0, &empty_media);
    let cons_w5_settle = bench_settle_10(&mut gpu_cons, &mut mgr_cons, &w5_idx, 9160, &w5_v720, 1.0, &w5_media);
    let cons_w5_pan = measure_pan_zoom(&mut gpu_cons, &mut mgr_cons, &w5_v720, 1.0, 50.0, true, &w5_peers, &w5_media);
    let cons_w5s_settle = bench_settle_10(&mut gpu_cons, &mut mgr_cons, &w5s_idx, 9180, &w5s_v720, 1.0, &w5s_media);
    save_gpu_png(&gpu_cons, &renders_dir.join("07_conservative_w5_stress.png"));
    let cons_w5s_pan = measure_pan_zoom(&mut gpu_cons, &mut mgr_cons, &w5s_v720, 1.0, 80.0, true, &[], &w5s_media);
    let cons_w6_settle = bench_settle_10(&mut gpu_cons, &mut mgr_cons, &w6_idx, 9200, &w6_v1x, 1.0, &w6_media);
    let cons_w6_pan = measure_pan_zoom(&mut gpu_cons, &mut mgr_cons, &w6_v1x, 1.0, 120.0, true, &[], &w6_media);

    // Pure-CPU fallback presenter timing spot check on CP1 W5!
    let mut cpu_presenter = CpuTilePresenter::default();
    let cpu_ctx = egui::Context::default();
    mgr_cons.set_scope(ScopeKey::new(9250, "bench", "u1", "d1", w5_world.canvas, None), Arc::clone(&w5_idx));
    w5_media.register_in(&mut mgr_cons);
    let _ = wait_until_crisp_pair(&mut gpu_cons, &mut mgr_cons, &w5_v720, 1.0, 0, &w5_media);
    let lod_w5 = select_target_lod(&w5_v720, 1.0, cons_budget.max_visible_tiles, None).unwrap();
    let vis_w5 = photocraft_engine::scene::spatial::visible_tiles(&w5_v720, lod_w5, cons_budget.max_visible_tiles).unwrap();
    let uv = [2.0_f32 / 260.0, 2.0_f32 / 260.0, 258.0_f32 / 260.0, 258.0_f32 / 260.0];
    let tile_bufs: Vec<_> = vis_w5.iter().filter_map(|&k| mgr_cons.get_valid_cpu_tile(k).map(|c| (k.logical_rect(), uv, c.rgba8))).collect();
    let tile_refs: Vec<_> = tile_bufs.iter().map(|(r, u, b)| (*r, *u, b.as_slice())).collect();
    let t_cpu_fb = Instant::now();
    let _cpu_tid = cpu_presenter.present_tiles(&cpu_ctx, &w5_v720, 1.0, &tile_refs).unwrap();
    let cpu_fallback_frame_ms = t_cpu_fb.elapsed().as_secs_f64() * 1000.0;

    let peak_rss_bytes = rss.peak().unwrap_or(0);
    let desktop_budget = TileBudget::desktop();
    let gpu_compute_triggered = w1_1_settle_16x.crisp.p95_ms > 250.0
        || w1_2_settle_16x.crisp.p95_ms > 250.0
        || w2_settle_16x.crisp.p95_ms > 250.0
        || w3_20_settle_16x.crisp.p95_ms > 400.0;

    let report = serde_json::json!({
        "profile": "release",
        "measurement_layer": "headless_wgpu_component_harness (NOT native OS window frame pacing — see native-e2e-verification.json for real native app frame pacing)",
        "adapter": format!("{} ({:?}, {:?})", adapter_info.name, adapter_info.device_type, adapter_info.backend),
        "process_peak_rss_mib": peak_rss_bytes as f64 / (1024.0 * 1024.0),
        "desktop_budget": {
            "gpu_layers": desktop_budget.gpu_layers,
            "tile_array_mib": (u64::from(desktop_budget.gpu_layers) * 260 * 260 * 4) as f64 / (1024.0 * 1024.0),
            "staging_ring_mib": desktop_budget.staging_ring_bytes as f64 / (1024.0 * 1024.0),
            "cpu_cache_mib": desktop_budget.cpu_cache_bytes as f64 / (1024.0 * 1024.0),
            "worker_threads": desktop_budget.worker_threads,
        },
        "conservative_budget": {
            "gpu_layers": cons_budget.gpu_layers,
            "tile_array_mib": (u64::from(cons_budget.gpu_layers) * 260 * 260 * 4) as f64 / (1024.0 * 1024.0),
            "staging_ring_mib": cons_budget.staging_ring_bytes as f64 / (1024.0 * 1024.0),
            "cpu_cache_mib": cons_budget.cpu_cache_bytes as f64 / (1024.0 * 1024.0),
            "worker_threads": cons_budget.worker_threads,
        },
        "laptop_real_hardware": "NOT VERIFIED (only desktop constrained proxy measured locally)",
        "gpu_compute_gate_evaluation": {
            "w1_1stroke_16x_crisp_p95_ms": w1_1_settle_16x.crisp.p95_ms,
            "w1_2stroke_16x_crisp_p95_ms": w1_2_settle_16x.crisp.p95_ms,
            "w2_16x_crisp_p95_ms": w2_settle_16x.crisp.p95_ms,
            "w3_20stroke_16x_crisp_p95_ms": w3_20_settle_16x.crisp.p95_ms,
            "w3_40stroke_16x_crisp_p95_ms": w3_40_settle_16x.crisp.p95_ms,
            "threshold_triggered": gpu_compute_triggered,
            "note": "Do not add GPU compute per Codex checkpoint directive; report trigger metrics transparently.",
        },
        "pixel_parity_gpu_vs_cpu_export": {
            "W1_1stroke_16x": w1_1_parity,
            "W2_watercolor_4x": w2_parity_4x,
            "W3_20_lowflow_16x": w3_20_parity,
            "W4_hard_round_4x": w4_parity_4x,
            "W5_dynamic_overlap": w5_parity,
        },
        "cp1_restored_workloads": {
            "W1_airbrush_soft": {
                "1_stroke": {
                    "fixture": w1_1_meta,
                    "settle_1x": w1_1_settle_1x,
                    "settle_4x": w1_1_settle_4x,
                    "settle_16x": w1_1_settle_16x,
                    "pan_zoom_1x_720p": w1_1_pan_1x,
                    "pan_zoom_4x_720p": w1_1_pan_4x,
                    "pan_zoom_16x_720p": w1_1_pan_16x,
                    "pan_zoom_16x_1080p": w1_1_pan_16x_1080,
                    "pan_zoom_16x_1440p": w1_1_pan_16x_1440,
                    "live_draw_16x_720p": w1_1_draw_16x,
                    "live_draw_16x_1080p": w1_1_draw_16x_1080,
                },
                "2_overlapping_strokes": {
                    "fixture": w1_2_meta,
                    "settle_16x": w1_2_settle_16x,
                    "pan_zoom_16x_720p": w1_2_pan_16x,
                    "pan_zoom_16x_1080p": w1_2_pan_16x_1080,
                },
            },
            "W2_watercolor_wet_edges_vigorous": {
                "fixture": w2_meta,
                "settle_1x": w2_settle_1x,
                "settle_4x": w2_settle_4x,
                "settle_16x": w2_settle_16x,
                "pan_1x_720p": w2_pan_1x,
                "pan_zoom_4x_720p": w2_pan_4x,
                "pan_zoom_16x_720p": w2_pan_16x,
                "pan_zoom_4x_1080p": w2_pan_4x_1080,
                "live_draw_4x_720p": w2_draw_4x,
                "live_draw_16x_720p": w2_draw_16x,
            },
            "W3_lowflow_3pct_soft_round": {
                "20_overlapping_strokes": {
                    "fixture": w3_20_meta,
                    "settle_1x": w3_20_settle_1x,
                    "settle_4x": w3_20_settle_4x,
                    "settle_16x": w3_20_settle_16x,
                    "pan_zoom_16x_720p": w3_20_pan_16x,
                    "pan_zoom_16x_1080p": w3_20_pan_16x_1080,
                    "live_draw_16x_720p": w3_20_draw_16x,
                },
                "40_overlapping_strokes": {
                    "fixture": w3_40_meta,
                    "settle_16x": w3_40_settle_16x,
                    "pan_zoom_16x_720p": w3_40_pan_16x,
                    "live_draw_16x_720p": w3_40_draw_16x,
                },
            },
            "W4_hard_round_10_and_ancestors": {
                "fixture": w4_meta,
                "settle_1x": w4_settle_1x,
                "settle_4x": w4_settle_4x,
                "settle_16x": w4_settle_16x,
                "settle_d6_ancestor": w4_settle_anc_d6,
                "settle_d12_ancestor": w4_settle_anc_d12,
                "pan_zoom_4x_720p": w4_pan_4x,
                "live_draw_4x_720p": w4_draw_4x,
                "pan_zoom_d12_ancestor_720p": w4_pan_anc_d12,
            },
            "W5_multi_author_dynamic_overlap": {
                "fixture": w5_meta,
                "settle_720p": w5_settle,
                "pan_zoom_with_2peers_anim_720p": w5_pan_720,
                "pan_zoom_with_2peers_anim_1080p": w5_pan_1080,
                "live_draw_with_local_plus_2peers_anim_720p": w5_draw_720,
                "live_draw_with_local_plus_2peers_anim_1080p": w5_draw_1080,
            },
            "W6_rev10500_mixed_depth_overview": {
                "fixture": w6_meta,
                "index_build_ms": w6_index_build_ms,
                "settle_1x_720p": w6_settle_1x,
                "settle_0_5x_720p": w6_settle_05x,
                "pan_zoom_1x_720p": w6_pan_1x_720,
                "pan_zoom_0_5x_720p": w6_pan_05x_720,
                "pan_zoom_1x_1080p": w6_pan_1x_1080,
                "pan_zoom_1x_1440p": w6_pan_1x_1440,
                "live_draw_1x_720p": w6_draw_1x_720,
            },
            "W7_deep_detail_inside_W6": {
                "depth6_fractional_1_37x_settle": w7_settle_d6,
                "depth6_pan_zoom_720p": w7_pan_d6,
                "depth12_fractional_2_23x_settle": w7_settle_d12,
                "depth12_pan_zoom_720p": w7_pan_d12,
                "depth12_live_draw_720p": w7_draw_d12,
                "tour_flight_0_6_12_720p": w7_tour_flight,
            },
            "W8_combined_reopen_on_W6_rev10500": {
                "purged_disk_cache": w8_cold_reopen,
                "warm_disk_cache": w8_warm_reopen,
                "client_sqlite_v4_mib": w8_client_sqlite_bytes as f64 / (1024.0 * 1024.0),
                "archive_v3_mib": w8_archive_v3_bytes as f64 / (1024.0 * 1024.0),
            },
        },
        "extra_stress_workloads": {
            "W5_stress_4500_strokes_2_animated_media_before_after_async_anim": {
                "fixture": w5s_meta,
                "settle_720p": w5s_settle,
                "pan_zoom_720p": w5s_pan_720,
                "pan_zoom_1080p": w5s_pan_1080,
                "pan_zoom_1440p": w5s_pan_1440,
            },
        },
        "constrained_desktop_proxy_NOT_real_laptop": {
            "W1_1stroke_16x_settle": cons_w1_settle,
            "W2_watercolor_4x_settle": cons_w2_settle,
            "W3_20_lowflow_16x_settle": cons_w3_settle,
            "W5_cp1_settle": cons_w5_settle,
            "W5_cp1_pan_zoom_720p": cons_w5_pan,
            "W5_stress_4500_settle": cons_w5s_settle,
            "W5_stress_4500_pan_zoom_720p": cons_w5s_pan,
            "W6_rev10500_settle": cons_w6_settle,
            "W6_rev10500_pan_zoom_720p": cons_w6_pan,
            "cpu_fallback_presenter_w5_ms": cpu_fallback_frame_ms,
        },
    });

    let receipt_path = out_dir.join("cp1-restored-w1-w8-release-bench.json");
    let json = serde_json::to_string_pretty(&report).unwrap();
    std::fs::write(&receipt_path, &json).unwrap();
    eprintln!("=== CP7 RESTORED CP1 W1-W8 --release RECEIPT WRITTEN TO {} ===\n{}", receipt_path.display(), json);
}

#[derive(Clone, Debug, Serialize)]
struct ColdWarmLiveStats {
    cold_render_fn_p50_ms: f64,
    cold_render_fn_p95_ms: f64,
    cold_render_fn_max_ms: f64,
    cold_split_settle_ms: f64,
    warm_render_fn_p50_ms: f64,
    warm_render_fn_p95_ms: f64,
    warm_render_fn_max_ms: f64,
}

#[allow(clippy::too_many_arguments)]
fn measure_cold_vs_warm_live_draw(
    gpu: &mut WorldGpu,
    world: &WorldState,
    index: &Arc<WorldSceneIndex>,
    view: &Viewport,
    density: f32,
    template_stroke: &Stroke,
    author_idx: u128,
    peer_previews: &[SharedStroke],
    media: &MediaBank,
) -> ColdWarmLiveStats {
    let mut resolve = |frame: &MediaFrame| -> photocraft_engine::Result<Option<Arc<RgbaFrame>>> { Ok(media.frames.get(&frame.hash).cloned()) };
    let mut mgr = WorldTileManager::new(TileBudget::desktop(), None);
    media.register_in(&mut mgr);
    gpu.purge_slots();
    let scope = ScopeKey::new(901, "https://cp8-cold-warm.invalid", owner(author_idx).as_str(), device(author_idx).as_str(), world.canvas, None);
    mgr.set_scope(scope, Arc::clone(index));
    // Settle static/animated base tiles first while keeping DynamicSplitCache 100% COLD!
    let _ = wait_until_crisp_pair(gpu, &mut mgr, view, density, 0, media);

    let target_lod = gpu.target_lod(view, density).unwrap();
    let mut run_pass = |gpu: &mut WorldGpu, mgr: &mut WorldTileManager, pass_id: u128| -> (Vec<f64>, f64) {
        let mut live_stroke = template_stroke.clone();
        live_stroke.id = StrokeId::new(&hex(88_000_000 + pass_id)).unwrap();
        let all_pts = template_stroke.points.clone();
        let all_samples = template_stroke.samples.clone();
        live_stroke.points = vec![all_pts[0]];
        live_stroke.samples = all_samples.as_ref().map(|s| vec![s[0]]);
        let mut live_brush = LiveTileBrush::new(&live_stroke, target_lod).unwrap();
        let mut samples_ms = Vec::with_capacity(60);
        let mut last_previews = Vec::new();
        for frame in 0..60_usize {
            let p_idx = if all_pts.len() > 61 { (frame + 1).min(all_pts.len()) } else { ((frame + 1) * all_pts.len() / 61).max(1).min(all_pts.len()) };
            live_stroke.points.clear();
            live_stroke.points.extend_from_slice(&all_pts[..p_idx]);
            if let (Some(dst), Some(src)) = (&mut live_stroke.samples, &all_samples) {
                dst.clear();
                dst.extend_from_slice(&src[..p_idx]);
            }
            let mut previews = Vec::with_capacity(1 + peer_previews.len());
            for (pi, peer) in peer_previews.iter().enumerate() {
                let mut p = peer.clone();
                let plen = ((frame + 1 + pi * 5) * peer.stroke.points.len() / 61).max(2).min(peer.stroke.points.len());
                p.stroke.points.truncate(plen);
                if let Some(s) = &mut p.stroke.samples {
                    s.truncate(plen);
                }
                previews.push(p);
            }
            previews.push(SharedStroke {
                stroke: live_stroke.clone(),
                provenance: None,
                device: device(author_idx),
                visible: true,
                created_revision: 999_999,
                changed_revision: 999_999,
            });
            let t0 = Instant::now();
            live_brush.sync_points(&live_stroke.points, live_stroke.samples.as_deref()).unwrap();
            gpu.present_interactive(mgr, view, density, (frame as u64) * 16, &previews, Some(&mut live_brush), &mut resolve, false).unwrap();
            let _ = gpu.health.wait(&gpu.rs.device, None);
            samples_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
            last_previews = previews;
        }
        // Measure remaining refinement delay until any pending WarmSplitBands/anim ticks converge.
        let t_settle = Instant::now();
        while gpu.stats.is_refining || gpu.stats.anim_pending_tiles > 0 {
            if t_settle.elapsed() > Duration::from_secs(10) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
            gpu.present_interactive(mgr, view, density, 60 * 16, &last_previews, Some(&mut live_brush), &mut resolve, false).unwrap();
            let _ = gpu.health.wait(&gpu.rs.device, None);
        }
        let settle_ms = t_settle.elapsed().as_secs_f64() * 1000.0;
        (samples_ms, settle_ms)
    };

    let (mut cold_ms, cold_settle_ms) = run_pass(gpu, &mut mgr, 1);
    let (mut warm_ms, _warm_settle_ms) = run_pass(gpu, &mut mgr, 2);
    cold_ms.sort_by(|a, b| a.total_cmp(b));
    warm_ms.sort_by(|a, b| a.total_cmp(b));
    ColdWarmLiveStats {
        cold_render_fn_p50_ms: pct(&cold_ms, 0.50),
        cold_render_fn_p95_ms: pct(&cold_ms, 0.95),
        cold_render_fn_max_ms: cold_ms.last().copied().unwrap_or(0.0),
        cold_split_settle_ms: cold_settle_ms,
        warm_render_fn_p50_ms: pct(&warm_ms, 0.50),
        warm_render_fn_p95_ms: pct(&warm_ms, 0.95),
        warm_render_fn_max_ms: warm_ms.last().copied().unwrap_or(0.0),
    }
}

/// Focused CP8 `--release` headless rendering verification (cold vs warm live draw on W1/W2/W3/W5/W5-stress
/// and deep-pocket W7 d6/d12 sharpening delay + pixel parity vs CPU export).
#[test]
#[ignore = "CP8 --release focused headless rendering & deep-pocket refinement benchmark"]
#[allow(clippy::cognitive_complexity)]
fn run_cp8_focused_rendering_bench() {
    let _lock = photocraft_testkit::gpu_lock();
    let rs = egui_kittest::wgpu::create_render_state(crate::gpu_canvas::wgpu_setup(), Default::default());
    let adapter = rs.adapter.get_info();
    let mut gpu = WorldGpu::new_with_budget(&rs, photocraft_gpu::DeviceHealth::watch(&rs.device), TileBudget::desktop()).unwrap();
    let empty_media = MediaBank::default();

    let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../log/k7/qol-009-redesign-dev/cp8-cold-live-and-deep");
    std::fs::create_dir_all(&out_dir).unwrap();

    // 1. CP1 W1 (Airbrush Soft 16x), W2 (Watercolor Wet Edges 4x), W3 (40 low-flow Soft Round 16x)
    let c1 = canvas(1);
    let (w1_world, w1_s1) = build_cp1_w1_airbrush(c1, 1);
    let w1_idx = Arc::new(WorldSceneIndex::new(&w1_world).unwrap());
    let w1_v16 = view_at(c1, 1280, 720, 0, [640.0, 360.0], 16.0);
    let w1_cold_warm_720 = measure_cold_vs_warm_live_draw(&mut gpu, &w1_world, &w1_idx, &w1_v16, 1.0, &w1_s1, 1, &[], &empty_media);
    let w1_cold_warm_1080 = measure_cold_vs_warm_live_draw(&mut gpu, &w1_world, &w1_idx, &w1_v16, 1.5, &w1_s1, 1, &[], &empty_media);

    let c2 = canvas(2);
    let (w2_world, w2_s) = build_cp1_w2_watercolor(c2);
    let w2_idx = Arc::new(WorldSceneIndex::new(&w2_world).unwrap());
    let w2_v4 = view_at(c2, 1280, 720, 0, [640.0, 360.0], 4.0);
    let w2_cold_warm_4x = measure_cold_vs_warm_live_draw(&mut gpu, &w2_world, &w2_idx, &w2_v4, 1.0, &w2_s, 1, &[], &empty_media);

    let c3 = canvas(31);
    let (w3_40_world, w3_40_s) = build_cp1_w3_lowflow(c3, 40);
    let w3_40_idx = Arc::new(WorldSceneIndex::new(&w3_40_world).unwrap());
    let w3_v16 = view_at(c3, 1280, 720, 0, [640.0, 360.0], 16.0);
    let w3_40_cold_warm = measure_cold_vs_warm_live_draw(&mut gpu, &w3_40_world, &w3_40_idx, &w3_v16, 1.0, &w3_40_s, 1, &[], &empty_media);

    // 2. CP1 W5 (multi-author + 3 animated media + 2 peer previews, local_dev = device(1) < device(2..6))
    let c5 = canvas(5);
    let (w5_world, w5_media, w5_live, w5_peers) = build_cp1_w5_dynamic_overlap(c5);
    let w5_idx = Arc::new(WorldSceneIndex::new(&w5_world).unwrap());
    let w5_v = view_at(c5, 1280, 720, 0, [640.0, 360.0], 1.0);
    let w5_cold_warm_720 = measure_cold_vs_warm_live_draw(&mut gpu, &w5_world, &w5_idx, &w5_v, 1.0, &w5_live, 1, &w5_peers, &w5_media);
    let w5_cold_warm_1080 = measure_cold_vs_warm_live_draw(&mut gpu, &w5_world, &w5_idx, &w5_v, 1.5, &w5_live, 1, &w5_peers, &w5_media);

    // 3. EXTRA W5-stress (4,500 strokes across authors + 2 animated media):
    //    local_dev = device(1) (< higher author devices, forcing multi-author W4 split bands on cold tiles!)
    let c5s = canvas(55);
    let (w5s_world, w5s_media) = build_extra_w5_stress_4500_media(c5s);
    let w5s_idx = Arc::new(WorldSceneIndex::new(&w5s_world).unwrap());
    let w5s_v = Viewport { camera: coordinates::home(c5s), width: 1280, height: 720 };
    let mut b_live = preset_brush("Hard Round");
    b_live.size = 22.0;
    b_live.color = [0.10, 0.85, 0.25, 1.0];
    let live_pts: Vec<StrokePoint> = (0..60).map(|i| pt_t(-160.0 + (i as f64) * 5.5, -40.0 + ((i as f64) * 0.2).sin() * 60.0, (i as f64) * 16.0)).collect();
    let w5s_live_stroke = make_brush_stroke(c5s, 990_001, 1, &live_pts, &b_live, 0, [0, 0]);
    let w5s_cold_warm_below_peers = measure_cold_vs_warm_live_draw(&mut gpu, &w5s_world, &w5s_idx, &w5s_v, 1.0, &w5s_live_stroke, 1, &[], &w5s_media);

    // 4. CP1 W7 Deep Pocket inside exact CP1 W6 (10,500 revs, 30 dense depth-0 Watercolor/Airbrush/Chalk ancestors + d6/d12 strokes!)
    let c6 = canvas(6);
    let (w6_world, w6_media, _, w6_tour) = build_cp1_w6_rev10500(c6);
    let w6_idx = Arc::new(WorldSceneIndex::new(&w6_world).unwrap());
    let w7_v_d6 = view_at(c6, 1280, 720, 6, [-120.0, -80.0], 1.37);
    let w7_v_d12 = view_at(c6, 1280, 720, 12, [-40.0, -30.0], 2.23);

    let mut w7_mgr = WorldTileManager::new(TileBudget::desktop(), None);
    let w7_settle_d6 = bench_settle_10(&mut gpu, &mut w7_mgr, &w6_idx, 700, &w7_v_d6, 1.0, &w6_media);
    let w7_pan_d6 = measure_pan_zoom(&mut gpu, &mut w7_mgr, &w7_v_d6, 1.0, 40.0, true, &[], &w6_media);
    let w7_settle_d12 = bench_settle_10(&mut gpu, &mut w7_mgr, &w6_idx, 720, &w7_v_d12, 1.0, &w6_media);
    let w7_d12_parity = measure_gpu_vs_cpu_parity(&gpu, &w6_idx, &w7_v_d12, 0, &w6_media);
    let w7_pan_d12 = measure_pan_zoom(&mut gpu, &mut w7_mgr, &w7_v_d12, 1.0, 40.0, true, &[], &w6_media);
    let w7_tour_flight = measure_tour_flight(&mut gpu, &mut w7_mgr, &w6_tour, 1280, 720, 1.0, &w6_media);

    let mut d12_b = preset_brush("Hard Round");
    d12_b.size = 18.0;
    d12_b.color = [0.9, 0.15, 0.25, 1.0];
    let d12_pts: Vec<StrokePoint> = (0..40).map(|i| pt_t(-55.0 + (i as f64) * 1.4, -30.0 + ((i as f64) * 0.3).sin() * 8.0, (i as f64) * 16.0)).collect();
    let d12_live_stroke = make_brush_stroke(c6, 990_002, 1, &d12_pts, &d12_b, 12, [0, 0]);
    let w7_d12_cold_warm_live = measure_cold_vs_warm_live_draw(&mut gpu, &w6_world, &w6_idx, &w7_v_d12, 1.0, &d12_live_stroke, 1, &[], &w6_media);

    let report = serde_json::json!({
        "profile": "release",
        "measurement_layer": "headless_wgpu_render_function_timings (NOT native OS window frame intervals — see cp8-native-smoke.json for actual native frame intervals)",
        "adapter": format!("{} ({:?}, {:?})", adapter.name, adapter.device_type, adapter.backend),
        "cold_vs_warm_live_draw_render_function_ms": {
            "CP1_W1_airbrush_16x_720p": w1_cold_warm_720,
            "CP1_W1_airbrush_16x_1080p": w1_cold_warm_1080,
            "CP1_W2_watercolor_4x_720p": w2_cold_warm_4x,
            "CP1_W3_40_lowflow_16x_720p": w3_40_cold_warm,
            "CP1_W5_dynamic_overlap_local_plus_2peers_3anim_720p": w5_cold_warm_720,
            "CP1_W5_dynamic_overlap_local_plus_2peers_3anim_1080p": w5_cold_warm_1080,
            "EXTRA_W5_stress_4500_strokes_2anim_author_below_peers_720p": w5s_cold_warm_below_peers,
            "CP1_W7_depth12_inside_W6_720p": w7_d12_cold_warm_live,
        },
        "deep_pocket_W7_inside_W6_rev10500": {
            "depth6_fractional_1_37x_refinement_settle": w7_settle_d6,
            "depth6_pan_zoom_render_fn_720p": w7_pan_d6,
            "depth12_fractional_2_23x_refinement_settle": w7_settle_d12,
            "depth12_pan_zoom_render_fn_720p": w7_pan_d12,
            "depth12_gpu_vs_cpu_export_parity": w7_d12_parity,
            "tour_flight_0_6_12_render_fn_720p": w7_tour_flight,
        },
    });
    let receipt_path = out_dir.join("cp8-headless-rendering-bench.json");
    let json = serde_json::to_string_pretty(&report).unwrap();
    std::fs::write(&receipt_path, &json).unwrap();
    eprintln!("=== CP8 FOCUSED HEADLESS BENCH WRITTEN TO {} ===\n{}", receipt_path.display(), json);
}
