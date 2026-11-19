//! QOL-009 visual parity matrix, UI verification, and payload/latency measurement suite.
//! Always asserts Party vs World CPU vs World GPU pixel parity and UI controls; when
//! `KOMAPHONE_QOL009_EVIDENCE_DIR` is set, writes all rendered comparison sheets, UI
//! screenshots, and `measurements.json` for visual inspection and archiving.
#![cfg(test)]
#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use photocraft_codecs::{ChannelLayout, EncodeOptions, Format, Image};
use photocraft_color::{BlendMode, PixelFormat};
use photocraft_engine::scene::{
    self, CanvasId, OwnerId, RegionKey, Srgba, Stroke, StrokeId, Viewport,
    brush::StrokeSample,
    coordinates,
    shared::{
        AcceptedOp, DeviceId, Edit, OpId, SHARED_SCHEMA_BRUSH, Tour, TourId, TourStop, WorldOp, WorldState,
        media::MediaFrame,
        tours::{ExportSettings, FrozenTour},
    },
    world_protocol::{CanvasSummary, Peer, PreviewStyle},
};
use photocraft_engine::world_brush::{LiveWorldBrush, to_stroke_brush};
use photocraft_engine::world_media::{WorldRenderer, export_archive, export_tour_gif, export_view_png, read_archive, render_view, rgba8};
use photocraft_engine::worlds::{Account, Canvas, Viewer};
use photocraft_paint::{BrushSettings, GrayTile, StrokePoint, TipShape, render_stroke};
use photocraft_raster::Surface;
use serde_json::{Value, json};

use crate::komaphone_world_gpu::{WorldGpu, tests::readback};
use crate::{PhotocraftApp, Tool};

fn hex(n: u128) -> String {
    format!("{n:032x}")
}
fn canvas() -> CanvasId {
    CanvasId::new(&hex(0x9001)).unwrap()
}
fn owner(n: u128) -> OwnerId {
    OwnerId::new(&hex(0x9100 + n)).unwrap()
}
fn device(n: u128) -> DeviceId {
    DeviceId::new(&hex(0x9200 + n)).unwrap()
}
fn no_assets(_: &MediaFrame) -> photocraft_engine::Result<Vec<u8>> {
    panic!("Brush evidence fixture has no media assets")
}
fn pixel_view(w: u32, h: u32, depth: u8) -> Viewport {
    let center = coordinates::point(&RegionKey { depth, ..Default::default() }, [f64::from(w) * 0.5, f64::from(h) * 0.5]).unwrap();
    Viewport { camera: coordinates::target(canvas(), center, depth, 1.0).unwrap(), width: w, height: h }
}
fn preset_brush(name: &str) -> BrushSettings {
    photocraft_paint::presets::builtin().into_iter().find(|p| p.name == name).unwrap_or_else(|| panic!("missing builtin preset {name}")).brush
}
fn append_stroke(world: &mut WorldState, writer: u128, stroke: Stroke) {
    let rev = world.revision + 1;
    world
        .apply(&AcceptedOp {
            revision: rev,
            op: WorldOp {
                id: OpId::new(&hex(0x9300 + u128::from(rev))).unwrap(),
                canvas: world.canvas,
                author: owner(writer),
                device: device(writer),
                sequence: world.next_sequence(device(writer)).unwrap(),
                authority_epoch: 1,
                action: Edit::AddStroke { stroke },
            },
        })
        .unwrap();
}
fn make_world_stroke(id: u128, writer: u128, _w: u32, _h: u32, pts: &[StrokePoint], b: &BrushSettings, depth: u8) -> Stroke {
    let sb = to_stroke_brush(b, 1.0, false).unwrap();
    let local_pts: Vec<[f64; 2]> = pts.iter().map(|p| [p.x, p.y]).collect();
    let samples: Vec<StrokeSample> = pts
        .iter()
        .map(|p| StrokeSample { pressure: p.pressure, tilt_x: p.tilt_x, tilt_y: p.tilt_y, rotation: p.rotation, wheel: p.wheel, time_ms: p.time })
        .collect();
    let need_samples = sb.build_up || !sb.spacing_enabled || sb.smoothing.is_some();
    Stroke {
        id: StrokeId::new(&hex(0x9400 + id)).unwrap(),
        canvas: canvas(),
        owner: owner(writer),
        revision: 0,
        anchor: RegionKey { depth, ..Default::default() },
        points: local_pts,
        width: f64::from(b.size),
        color: Srgba { rgba: [b.color[0], b.color[1], b.color[2], b.opacity] },
        erase: b.erase,
        brush: Some(Box::new(sb)),
        samples: need_samples.then_some(samples),
    }
}

/// Render a sequence of `(device_layer, brush, points)` through Party's exact `photocraft_paint::render_stroke`
/// engine over opaque white paper, preserving per-device layer isolation identically to Shared Canvas.
fn render_party_layers(w: u32, h: u32, strokes: &[(u128, BrushSettings, Vec<StrokePoint>)]) -> Vec<u8> {
    let mut layers: std::collections::BTreeMap<u128, Surface> = std::collections::BTreeMap::new();
    for (dev, brush, pts) in strokes {
        let surf = layers.entry(*dev).or_insert_with(|| Surface::new(PixelFormat::RGBA32F));
        render_stroke(surf, brush, pts, None, false, 1.0);
    }
    let mut out = vec![[1.0f32; 4]; (w * h) as usize];
    for surf in layers.values() {
        for y in 0..h {
            for x in 0..w {
                let p = surf.rgba(x as i32, y as i32);
                let a = p[3].clamp(0.0, 1.0);
                if a > 0.0 {
                    let dst = &mut out[(y * w + x) as usize];
                    for ch in 0..3 {
                        dst[ch] = (p[ch] * a + dst[ch] * (1.0 - a)).clamp(0.0, 1.0);
                    }
                }
            }
        }
    }
    let mut bytes = Vec::with_capacity((w * h * 4) as usize);
    for px in out {
        for ch in px {
            bytes.push((ch.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    bytes
}

fn next_stroke_seq() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    u128::from(SEQ.fetch_add(1, Ordering::Relaxed))
}
fn render_world_cpu_and_gpu(w: u32, h: u32, strokes: &[(u128, BrushSettings, Vec<StrokePoint>)], gpu: &mut WorldGpu) -> (Vec<u8>, Vec<u8>) {
    let mut world = WorldState::new(canvas(), owner(0), "Parity cell".into()).unwrap();
    for (dev, b, pts) in strokes {
        let s = make_world_stroke(next_stroke_seq(), *dev, w, h, pts, b, 0);
        append_stroke(&mut world, *dev, s);
    }
    let view = pixel_view(w, h, 0);
    let cpu_img = render_view(&world, &view, 0, &mut no_assets).unwrap();
    let cpu = rgba8(&cpu_img).unwrap();

    let mut renderer = WorldRenderer::new(&world).unwrap();
    gpu.render(&mut renderer, &view, 1.0, 0, &mut |_| panic!("No media")).unwrap();
    let gpu_pixels = readback(gpu);
    let gpu_bytes: Vec<u8> = gpu_pixels.into_iter().flatten().collect();
    (cpu, gpu_bytes)
}

fn diff_stats(a: &[u8], b: &[u8]) -> (u8, f64) {
    assert_eq!(a.len(), b.len());
    let mut max_d = 0u8;
    let mut sum = 0u64;
    for (&x, &y) in a.iter().zip(b.iter()) {
        let d = x.abs_diff(y);
        max_d = max_d.max(d);
        sum += u64::from(d);
    }
    (max_d, sum as f64 / (a.len().max(1) as f64 * 255.0))
}

fn save_png(path: &Path, w: u32, h: u32, rgba: &[u8]) {
    let img = Image::from_u8(w, h, ChannelLayout::Rgba, rgba.to_vec()).unwrap();
    let encoded = photocraft_codecs::encode(&img, Format::Png, &EncodeOptions::default()).unwrap();
    std::fs::write(path, encoded).unwrap();
}

struct GridSheet {
    cols: u32,
    rows: u32,
    cell_w: u32,
    cell_h: u32,
    pad: u32,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl GridSheet {
    fn new(cols: u32, rows: u32, cell_w: u32, cell_h: u32, pad: u32) -> Self {
        let width = cols * cell_w + (cols + 1) * pad;
        let height = rows * cell_h + (rows + 1) * pad;
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for px in rgba.chunks_exact_mut(4) {
            px.copy_from_slice(&[42, 45, 52, 255]);
        }
        Self { cols, rows, cell_w, cell_h, pad, width, height, rgba }
    }
    fn blit(&mut self, col: u32, row: u32, src: &[u8]) {
        assert!(col < self.cols && row < self.rows);
        let ox = self.pad + col * (self.cell_w + self.pad);
        let oy = self.pad + row * (self.cell_h + self.pad);
        for y in 0..self.cell_h {
            let s0 = (y * self.cell_w * 4) as usize;
            let s1 = s0 + (self.cell_w * 4) as usize;
            let d0 = (((oy + y) * self.width + ox) * 4) as usize;
            let d1 = d0 + (self.cell_w * 4) as usize;
            self.rgba[d0..d1].copy_from_slice(&src[s0..s1]);
        }
    }
}

fn custom_diamond_tip(side: u32) -> TipShape {
    let c = side as f32 * 0.5;
    let mut data = vec![0u16; (side * side) as usize];
    for y in 0..side {
        for x in 0..side {
            let d = ((x as f32 + 0.5 - c).abs() + (y as f32 + 0.5 - c).abs()) / c;
            data[(y * side + x) as usize] = ((1.0 - d).clamp(0.0, 1.0) * 65535.0).round() as u16;
        }
    }
    TipShape::Sampled(GrayTile { width: side, height: side, data: data.into() })
}

fn pt(x: f64, y: f64) -> StrokePoint {
    StrokePoint::new(x, y, 1.0)
}
fn pt_t(x: f64, y: f64, time: f64) -> StrokePoint {
    let mut p = StrokePoint::new(x, y, 1.0);
    p.time = time;
    p
}
fn wave_points(w: u32, h: u32, count: usize, dt_ms: f64) -> Vec<StrokePoint> {
    (0..count)
        .map(|i| {
            let t = i as f64 / (count.saturating_sub(1).max(1)) as f64;
            let x = 18.0 + t * (f64::from(w) - 36.0);
            let y = f64::from(h) * 0.5 + (t * std::f64::consts::TAU).sin() * (f64::from(h) * 0.24);
            pt_t(x, y, i as f64 * dt_ms)
        })
        .collect()
}

#[test]
fn qol009_visual_parity_matrix_and_measurements() {
    let _gpu_lock = photocraft_testkit::gpu_lock();
    let rs = egui_kittest::wgpu::create_render_state(crate::gpu_canvas::wgpu_setup(), Default::default());
    let health = photocraft_gpu::DeviceHealth::watch(&rs.device);
    let mut gpu = WorldGpu::new(&rs, health).unwrap();
    let out_dir = std::env::var_os("KOMAPHONE_QOL009_EVIDENCE_DIR").map(std::path::PathBuf::from);
    if let Some(dir) = &out_dir {
        std::fs::create_dir_all(dir.join("renders")).unwrap();
    }

    let mut parity_rows: Vec<Value> = Vec::new();

    // =========================================================================
    // Sheet 1: Opacity, Overlap, Device-Layer Soft Eraser, Airbrush, Modes
    // Columns: [0] Party reference | [1] World CPU export | [2] World GPU wgpu
    // =========================================================================
    let (cw, ch) = (220u32, 108u32);
    let mut sheet1 = GridSheet::new(3, 5, cw, ch, 4);

    // Row 0: 25%, 50%, 100% opacity bars (Soft Round, 25% hardness, cobalt blue).
    let row0_strokes: Vec<_> = [(0.25f32, 24.0), (0.50, 54.0), (1.00, 84.0)]
        .into_iter()
        .enumerate()
        .map(|(i, (op, y))| {
            let mut b = preset_brush("Soft Round");
            b.size = 24.0;
            b.hardness = 0.25;
            b.opacity = op;
            b.color = [0.12, 0.38, 0.88, 1.0];
            b.seed = 10 + i as u64;
            (0u128, b, vec![pt(26.0, y), pt(194.0, y)])
        })
        .collect();

    // Row 1: Single-stroke self-overlap (50% stays 50%, left) vs two-stroke crossing (50% + 50% = 75%, right).
    let mut b_self = preset_brush("Hard Round");
    b_self.size = 20.0;
    b_self.hardness = 0.85;
    b_self.opacity = 0.5;
    b_self.color = [0.85, 0.18, 0.22, 1.0];
    b_self.seed = 21;
    let self_pts = vec![pt(22.0, 26.0), pt(92.0, 82.0), pt(92.0, 26.0), pt(22.0, 82.0)];
    let mut b_sep1 = b_self.clone();
    b_sep1.seed = 22;
    let mut b_sep2 = b_self.clone();
    b_sep2.seed = 23;
    let row1_strokes = vec![(0, b_self, self_pts), (0, b_sep1, vec![pt(128.0, 26.0), pt(198.0, 82.0)]), (0, b_sep2, vec![pt(198.0, 26.0), pt(128.0, 82.0)])];

    // Row 2: Partial-opacity Soft Eraser on Author 2 preserving Author 1's red band.
    let mut a1 = preset_brush("Hard Round");
    a1.size = 34.0;
    a1.opacity = 1.0;
    a1.color = [0.90, 0.20, 0.16, 1.0];
    a1.seed = 31;
    let mut a2 = preset_brush("Hard Round");
    a2.size = 34.0;
    a2.opacity = 1.0;
    a2.color = [0.12, 0.42, 0.88, 1.0];
    a2.seed = 32;
    let mut er = preset_brush("Soft Round");
    er.size = 36.0;
    er.hardness = 0.15;
    er.opacity = 0.6;
    er.erase = true;
    er.seed = 33;
    let row2_strokes =
        vec![(0, a1, vec![pt(24.0, 54.0), pt(196.0, 54.0)]), (1, a2, vec![pt(110.0, 14.0), pt(110.0, 94.0)]), (1, er, vec![pt(72.0, 54.0), pt(148.0, 54.0)])];

    // Row 3: Airbrush Soft stationary hold (400 ms build-up) vs quick tap vs moving sweep.
    let mut air = preset_brush("Airbrush Soft");
    air.size = 34.0;
    air.opacity = 0.9;
    air.flow = 0.08;
    air.build_up = true;
    air.color = [0.10, 0.12, 0.18, 1.0];
    let mut air_tap = air.clone();
    air_tap.seed = 41;
    let mut air_hold = air.clone();
    air_hold.seed = 42;
    let hold_pts: Vec<StrokePoint> = (0..26).map(|k| pt_t(96.0, 54.0, k as f64 * 16.0)).collect();
    let mut air_sweep = air.clone();
    air_sweep.seed = 43;
    let sweep_pts: Vec<StrokePoint> = (0..16).map(|k| pt_t(138.0 + k as f64 * 4.0, 24.0 + k as f64 * 4.0, k as f64 * 16.0)).collect();
    let row3_strokes = vec![(0, air_tap, vec![pt_t(36.0, 54.0, 0.0)]), (0, air_hold, hold_pts), (0, air_sweep, sweep_pts)];

    // Row 4: Behind mode (green behind blue, left) & Multiply mode (magenta * cyan, right).
    let mut base_blue = preset_brush("Hard Round");
    base_blue.size = 24.0;
    base_blue.color = [0.15, 0.40, 0.90, 1.0];
    base_blue.seed = 51;
    let mut behind_green = preset_brush("Hard Round");
    behind_green.size = 24.0;
    behind_green.behind = true;
    behind_green.color = [0.15, 0.78, 0.30, 1.0];
    behind_green.seed = 52;
    let mut base_cyan = preset_brush("Hard Round");
    base_cyan.size = 24.0;
    base_cyan.color = [0.15, 0.85, 0.92, 1.0];
    base_cyan.seed = 53;
    let mut mul_mag = preset_brush("Hard Round");
    mul_mag.size = 24.0;
    mul_mag.mode = BlendMode::Multiply;
    mul_mag.color = [0.92, 0.20, 0.70, 1.0];
    mul_mag.seed = 54;
    let row4_strokes = vec![
        (0, base_blue, vec![pt(22.0, 54.0), pt(98.0, 54.0)]),
        (0, behind_green, vec![pt(60.0, 18.0), pt(60.0, 90.0)]),
        (0, base_cyan, vec![pt(124.0, 54.0), pt(200.0, 54.0)]),
        (0, mul_mag, vec![pt(162.0, 18.0), pt(162.0, 90.0)]),
    ];

    for (row_idx, (label, strokes)) in [
        ("opacity_25_50_100", row0_strokes),
        ("self_vs_separate_overlap_50pct", row1_strokes),
        ("soft_eraser_author_layer_isolation", row2_strokes),
        ("airbrush_tap_vs_400ms_hold_vs_sweep", row3_strokes),
        ("behind_and_multiply_modes", row4_strokes),
    ]
    .into_iter()
    .enumerate()
    {
        let party = render_party_layers(cw, ch, &strokes);
        let (w_cpu, w_gpu) = render_world_cpu_and_gpu(cw, ch, &strokes, &mut gpu);
        let (max_pc, mean_pc) = diff_stats(&party, &w_cpu);
        let (max_cg, mean_cg) = diff_stats(&w_cpu, &w_gpu);
        assert!(max_pc <= 2, "{label}: Party vs World CPU max diff {max_pc}");
        assert!(max_cg <= 2, "{label}: World CPU vs World GPU max diff {max_cg}");
        sheet1.blit(0, row_idx as u32, &party);
        sheet1.blit(1, row_idx as u32, &w_cpu);
        sheet1.blit(2, row_idx as u32, &w_gpu);
        parity_rows.push(json!({
            "sheet": "01-opacity-overlap-matrix",
            "case": label,
            "party_vs_world_cpu_max_u8": max_pc,
            "party_vs_world_cpu_mean": mean_pc,
            "world_cpu_vs_gpu_max_u8": max_cg,
            "world_cpu_vs_gpu_mean": mean_cg,
        }));
    }

    // =========================================================================
    // Sheet 2: 14 Presets across all 4 groups + Pencil + Eraser + Custom Tip
    // Layout: 7 rows × 6 cols ([Left 3: Party|CPU|GPU] + [Right 3: Party|CPU|GPU])
    // =========================================================================
    let (pw, ph) = (150u32, 68u32);
    let mut sheet2 = GridSheet::new(6, 7, pw, ph, 3);
    type PresetCase = (&'static str, Vec<(u128, BrushSettings, Vec<StrokePoint>)>);
    let preset_cases: Vec<PresetCase> = vec![
        ("Hard Round", {
            let mut b = preset_brush("Hard Round");
            b.size = 18.0;
            b.color = [0.12, 0.18, 0.30, 1.0];
            b.seed = 101;
            vec![(0, b, wave_points(pw, ph, 14, 16.0))]
        }),
        ("Soft Round", {
            let mut b = preset_brush("Soft Round");
            b.size = 24.0;
            b.opacity = 0.75;
            b.color = [0.16, 0.42, 0.85, 1.0];
            b.seed = 102;
            vec![(0, b, wave_points(pw, ph, 14, 16.0))]
        }),
        ("Hard Pencil (Pencil tool)", {
            let mut b = preset_brush("Hard Pencil");
            b.size = 10.0;
            b.aliased = true;
            b.hardness = 1.0;
            b.flow = 1.0;
            b.opacity = 0.7;
            b.color = [0.22, 0.14, 0.45, 1.0];
            b.seed = 103;
            vec![(0, b, wave_points(pw, ph, 14, 16.0))]
        }),
        ("Soft Eraser (Eraser tool)", {
            let mut bg = preset_brush("Hard Round");
            bg.size = 38.0;
            bg.color = [0.20, 0.62, 0.48, 1.0];
            bg.seed = 1040;
            let mut b = preset_brush("Soft Round");
            b.size = 22.0;
            b.opacity = 0.7;
            b.erase = true;
            b.seed = 104;
            vec![(0, bg, vec![pt(16.0, 34.0), pt(134.0, 34.0)]), (0, b, wave_points(pw, ph, 14, 16.0))]
        }),
        ("Airbrush Soft", {
            let mut b = preset_brush("Airbrush Soft");
            b.size = 26.0;
            b.color = [0.15, 0.15, 0.22, 1.0];
            b.seed = 105;
            vec![(0, b, wave_points(pw, ph, 18, 20.0))]
        }),
        ("Calligraphy Flat", {
            let mut b = preset_brush("Calligraphy Flat");
            b.size = 22.0;
            b.color = [0.55, 0.12, 0.18, 1.0];
            b.seed = 106;
            vec![(0, b, wave_points(pw, ph, 14, 16.0))]
        }),
        ("Ink Pen (smoothing)", {
            let mut b = preset_brush("Ink Pen");
            b.size = 12.0;
            b.color = [0.06, 0.08, 0.14, 1.0];
            b.seed = 107;
            vec![(0, b, wave_points(pw, ph, 16, 16.0))]
        }),
        ("Chalk", {
            let mut b = preset_brush("Chalk");
            b.size = 24.0;
            b.color = [0.76, 0.36, 0.12, 1.0];
            b.seed = 108;
            vec![(0, b, wave_points(pw, ph, 14, 16.0))]
        }),
        ("Charcoal", {
            let mut b = preset_brush("Charcoal");
            b.size = 24.0;
            b.color = [0.18, 0.18, 0.20, 1.0];
            b.seed = 109;
            vec![(0, b, wave_points(pw, ph, 14, 16.0))]
        }),
        ("Watercolor Wet Edges", {
            let mut b = preset_brush("Watercolor Wet Edges");
            b.size = 26.0;
            b.color = [0.14, 0.56, 0.78, 1.0];
            b.seed = 110;
            vec![(0, b, wave_points(pw, ph, 14, 16.0))]
        }),
        ("Oil Bristle", {
            let mut b = preset_brush("Oil Bristle");
            b.size = 24.0;
            b.color = [0.82, 0.22, 0.16, 1.0];
            b.background = [0.95, 0.78, 0.16, 1.0];
            b.seed = 111;
            vec![(0, b, wave_points(pw, ph, 14, 16.0))]
        }),
        ("Spatter", {
            let mut b = preset_brush("Spatter");
            b.size = 20.0;
            b.color = [0.58, 0.16, 0.68, 1.0];
            b.seed = 112;
            vec![(0, b, wave_points(pw, ph, 14, 16.0))]
        }),
        ("Dual Grain", {
            let mut b = preset_brush("Dual Grain");
            b.size = 26.0;
            b.color = [0.18, 0.48, 0.28, 1.0];
            b.seed = 113;
            vec![(0, b, wave_points(pw, ph, 14, 16.0))]
        }),
        ("Custom 32x32 Diamond Tip", {
            let b = BrushSettings {
                tip: custom_diamond_tip(32),
                size: 24.0,
                spacing: 0.35,
                opacity: 0.75,
                color: [0.85, 0.32, 0.12, 1.0],
                seed: 114,
                ..Default::default()
            };
            vec![(0, b, wave_points(pw, ph, 14, 16.0))]
        }),
    ];

    for (idx, (label, strokes)) in preset_cases.into_iter().enumerate() {
        let party = render_party_layers(pw, ph, &strokes);
        let (w_cpu, w_gpu) = render_world_cpu_and_gpu(pw, ph, &strokes, &mut gpu);
        let (max_pc, mean_pc) = diff_stats(&party, &w_cpu);
        let (max_cg, mean_cg) = diff_stats(&w_cpu, &w_gpu);
        assert!(max_pc <= 2, "{label}: Party vs World CPU max diff {max_pc}");
        assert!(max_cg <= 2, "{label}: World CPU vs World GPU max diff {max_cg}");
        let row = (idx / 2) as u32;
        let base_col = ((idx % 2) * 3) as u32;
        sheet2.blit(base_col, row, &party);
        sheet2.blit(base_col + 1, row, &w_cpu);
        sheet2.blit(base_col + 2, row, &w_gpu);
        parity_rows.push(json!({
            "sheet": "02-preset-parity-matrix",
            "case": label,
            "party_vs_world_cpu_max_u8": max_pc,
            "party_vs_world_cpu_mean": mean_pc,
            "world_cpu_vs_gpu_max_u8": max_cg,
            "world_cpu_vs_gpu_mean": mean_cg,
        }));
    }

    // =========================================================================
    // Sheet 3: Deep Zoom (Depth 0 & Depth 6), Export, Replay, Archive, Thumbnail
    // =========================================================================
    let (dw, dh) = (180u32, 140u32);
    let mut sheet3 = GridSheet::new(4, 1, dw, dh, 4);
    let mut deep_world = WorldState::new(canvas(), owner(0), "Deep brush evidence".into()).unwrap();
    let mut b_d0 = preset_brush("Soft Round");
    b_d0.size = 110.0;
    b_d0.opacity = 0.35;
    b_d0.color = [0.92, 0.65, 0.12, 1.0];
    b_d0.seed = 201;
    append_stroke(&mut deep_world, 0, make_world_stroke(1, 0, dw, dh, &[pt(-10.0, 0.0), pt(140.0, 90.0)], &b_d0, 0));

    let mut b_d6_a = preset_brush("Chalk");
    b_d6_a.size = 24.0;
    b_d6_a.opacity = 0.8;
    b_d6_a.color = [0.14, 0.32, 0.78, 1.0];
    b_d6_a.seed = 202;
    append_stroke(&mut deep_world, 0, make_world_stroke(2, 0, dw, dh, &wave_points(dw, dh, 14, 16.0), &b_d6_a, 6));

    let mut b_d6_b = preset_brush("Soft Round");
    b_d6_b.size = 30.0;
    b_d6_b.opacity = 0.5;
    b_d6_b.color = [0.86, 0.18, 0.30, 1.0];
    b_d6_b.seed = 203;
    append_stroke(&mut deep_world, 1, make_world_stroke(3, 1, dw, dh, &[pt(40.0, 26.0), pt(140.0, 114.0)], &b_d6_b, 6));
    assert_eq!(deep_world.schema, SHARED_SCHEMA_BRUSH);

    let view_d0 = pixel_view(dw, dh, 0);
    let view_d6 = pixel_view(dw, dh, 6);
    let img_d0 = rgba8(&render_view(&deep_world, &view_d0, 0, &mut no_assets).unwrap()).unwrap();
    let img_d6_cpu = rgba8(&render_view(&deep_world, &view_d6, 0, &mut no_assets).unwrap()).unwrap();

    let mut deep_renderer = WorldRenderer::new(&deep_world).unwrap();
    gpu.render(&mut deep_renderer, &view_d6, 1.0, 0, &mut |_| panic!("No media")).unwrap();
    let img_d6_gpu: Vec<u8> = readback(&gpu).into_iter().flatten().collect();
    let (deep_max, deep_mean) = diff_stats(&img_d6_cpu, &img_d6_gpu);
    // 3 stacked translucent layers (depth-0 ancestor + depth-6 Chalk + depth-6 Soft Round)
    // undergo 3 sequential 8-bit Rgba8Unorm GPU blends vs f32 CPU blends (<= 3/255 quantization).
    assert!(deep_max <= 3, "Depth 6 CPU vs GPU max diff {deep_max}");

    // Archive export/import (schema 2) + PNG export + Tour GIF + Thumbnail check.
    let tmp = tempfile::tempdir().unwrap();
    let arch_dir = tmp.path().join("deep.komworld");
    export_archive(&deep_world, &arch_dir, &mut no_assets, |_, _| true).unwrap();
    let (imported_world, _) = read_archive(&arch_dir).unwrap();
    assert_eq!(imported_world.schema, SHARED_SCHEMA_BRUSH);
    assert_eq!(imported_world.canonical_hash().unwrap(), deep_world.canonical_hash().unwrap());
    let img_d6_reopened = rgba8(&render_view(&imported_world, &view_d6, 0, &mut no_assets).unwrap()).unwrap();
    assert_eq!(img_d6_cpu, img_d6_reopened, "reopened archive depth-6 pixels must be bit-identical");

    let png_export_path = tmp.path().join("depth6.png");
    export_view_png(&deep_world, &view_d6, 0, &png_export_path, &mut no_assets).unwrap();
    assert!(std::fs::metadata(&png_export_path).unwrap().len() > 200);

    let tour = Tour {
        id: TourId::new(&hex(0x9500)).unwrap(),
        canvas: canvas(),
        name: "Deep brush tour".into(),
        revision: 0,
        stops: vec![
            TourStop { camera: view_d0.camera.clone(), travel_ms: 0, pause_ms: 250, target: None },
            TourStop { camera: view_d6.camera.clone(), travel_ms: 500, pause_ms: 250, target: None },
        ],
    };
    let tid = tour.id;
    let rev = deep_world.revision + 1;
    deep_world
        .apply(&AcceptedOp {
            revision: rev,
            op: WorldOp {
                id: OpId::new(&hex(0x9501)).unwrap(),
                canvas: canvas(),
                author: owner(0),
                device: device(0),
                sequence: deep_world.next_sequence(device(0)).unwrap(),
                authority_epoch: 1,
                action: Edit::PutTour { tour, expected_revision: 0 },
            },
        })
        .unwrap();
    let frozen = FrozenTour::new(&deep_world, tid, ExportSettings { width: dw, height: dh, fps: 4 }).unwrap();
    let gif_path = tmp.path().join("tour.gif");
    export_tour_gif(&frozen, &gif_path, true, &mut no_assets, |_, _| true).unwrap();
    assert!(std::fs::metadata(&gif_path).unwrap().len() > 200);

    // Canvas Gallery thumbnail (160×100) renders brush strokes cleanly without dropping translucent marks.
    let thumb_view = Viewport { camera: view_d0.camera.clone(), width: 160, height: 100 };
    let thumb = rgba8(&render_view(&deep_world, &thumb_view, 0, &mut no_assets).unwrap()).unwrap();
    assert!(thumb.chunks_exact(4).any(|px| px[0] < 250 || px[1] < 250 || px[2] < 250));

    sheet3.blit(0, 0, &img_d0);
    sheet3.blit(1, 0, &img_d6_cpu);
    sheet3.blit(2, 0, &img_d6_gpu);
    sheet3.blit(3, 0, &img_d6_reopened);
    parity_rows.push(json!({
        "sheet": "03-deep-zoom-and-export-parity",
        "case": "depth6_cpu_vs_gpu_and_reopened_archive",
        "world_cpu_vs_gpu_max_u8": deep_max,
        "world_cpu_vs_gpu_mean": deep_mean,
        "reopened_archive_exact_match": img_d6_cpu == img_d6_reopened,
    }));

    // =========================================================================
    // Measurements: Serialized payload bytes & CPU / GPU / Live-drag latencies
    // =========================================================================
    let sample_pts = wave_points(800, 600, 20, 16.0);
    let legacy_stroke = Stroke {
        id: StrokeId::new(&hex(0x9600)).unwrap(),
        canvas: canvas(),
        owner: owner(0),
        revision: 0,
        anchor: RegionKey::default(),
        points: sample_pts.iter().map(|p| [p.x, p.y]).collect(),
        width: 24.0,
        color: Srgba { rgba: [0.1, 0.2, 0.3, 1.0] },
        erase: false,
        brush: None,
        samples: None,
    };
    let mut payload_bytes = serde_json::Map::new();
    payload_bytes.insert("legacy_opaque_20pts".into(), json!(serde_json::to_vec(&legacy_stroke).unwrap().len()));
    for name in [
        "Hard Round",
        "Soft Round",
        "Hard Pencil",
        "Airbrush Soft",
        "Calligraphy Flat",
        "Ink Pen",
        "Chalk",
        "Charcoal",
        "Watercolor Wet Edges",
        "Oil Bristle",
        "Spatter",
        "Dual Grain",
    ] {
        let b = preset_brush(name);
        let s = make_world_stroke(1, 0, 800, 600, &sample_pts, &b, 0);
        payload_bytes.insert(name.into(), json!(serde_json::to_vec(&s).unwrap().len()));
    }
    for side in [32u32, 128u32] {
        let b = BrushSettings { tip: custom_diamond_tip(side), ..Default::default() };
        let s = make_world_stroke(2, 0, 800, 600, &sample_pts, &b, 0);
        payload_bytes.insert(format!("custom_sampled_{side}x{side}_20pts"), json!(serde_json::to_vec(&s).unwrap().len()));
    }

    // Latency measurements at 800x600.
    let bench_view = pixel_view(800, 600, 0);
    let mut w1 = WorldState::new(canvas(), owner(0), "Bench 1".into()).unwrap();
    let mut sb1 = preset_brush("Soft Round");
    sb1.size = 32.0;
    sb1.opacity = 0.5;
    sb1.color = [0.2, 0.4, 0.85, 1.0];
    append_stroke(&mut w1, 0, make_world_stroke(1, 0, 800, 600, &sample_pts, &sb1, 0));

    let t0 = Instant::now();
    let _ = render_view(&w1, &bench_view, 0, &mut no_assets).unwrap();
    let cpu_1_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let mut r1 = WorldRenderer::new(&w1).unwrap();
    let t0 = Instant::now();
    gpu.render(&mut r1, &bench_view, 1.0, 0, &mut |_| panic!("No media")).unwrap();
    let gpu_1_cold_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let t0 = Instant::now();
    gpu.render(&mut r1, &bench_view, 1.0, 0, &mut |_| panic!("No media")).unwrap();
    let gpu_1_cached_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let mut w20 = WorldState::new(canvas(), owner(0), "Bench 20".into()).unwrap();
    let preset_cycle = ["Soft Round", "Hard Round", "Chalk", "Calligraphy Flat", "Spatter"];
    for i in 0..20usize {
        let mut b = preset_brush(preset_cycle[i % preset_cycle.len()]);
        b.size = 24.0;
        b.opacity = 0.6;
        b.seed = 500 + i as u64;
        let pts: Vec<StrokePoint> = (0..16).map(|k| pt(60.0 + k as f64 * 42.0, 40.0 + i as f64 * 26.0 + (k as f64).sin() * 8.0)).collect();
        append_stroke(&mut w20, (i % 3) as u128, make_world_stroke(i as u128 + 1, (i % 3) as u128, 800, 600, &pts, &b, 0));
    }
    let t0 = Instant::now();
    let _ = render_view(&w20, &bench_view, 0, &mut no_assets).unwrap();
    let cpu_20_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let mut r20 = WorldRenderer::new(&w20).unwrap();
    let t0 = Instant::now();
    gpu.render(&mut r20, &bench_view, 1.0, 0, &mut |_| panic!("No media")).unwrap();
    let gpu_20_cold_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let t0 = Instant::now();
    gpu.render(&mut r20, &bench_view, 1.0, 0, &mut |_| panic!("No media")).unwrap();
    let gpu_20_cached_ms = t0.elapsed().as_secs_f64() * 1000.0;

    // 200-point incremental live drag measurement.
    let drag_pts = wave_points(800, 600, 200, 8.0);
    let mut live_stroke = make_world_stroke(99, 0, 800, 600, &drag_pts[..1], &sb1, 0);
    let mut live = LiveWorldBrush::new(&live_stroke, &bench_view, 1.0).unwrap();
    let mut step_ms_sum = 0.0f64;
    let mut step_ms_max = 0.0f64;
    let mut steps = 0u32;
    for chunk in drag_pts[1..].chunks(4) {
        let t0 = Instant::now();
        for pt in chunk {
            live_stroke.points.push([pt.x, pt.y]);
        }
        live.sync_points(&live_stroke.points, None).unwrap();
        let raster = live.preview_raster().unwrap();
        let shared = scene::shared::SharedStroke {
            stroke: live_stroke.clone(),
            device: device(0),
            provenance: None,
            visible: true,
            created_revision: 0,
            changed_revision: 0,
        };
        gpu.render_with_live(&mut r20, &bench_view, 1.0, 0, &mut |_| panic!("No media"), &[shared], raster.as_ref().map(|r| (live_stroke.id, r))).unwrap();
        let dt = t0.elapsed().as_secs_f64() * 1000.0;
        step_ms_sum += dt;
        step_ms_max = step_ms_max.max(dt);
        steps += 1;
    }

    if let Some(dir) = &out_dir {
        save_png(&dir.join("renders/01-opacity-overlap-matrix.png"), sheet1.width, sheet1.height, &sheet1.rgba);
        save_png(&dir.join("renders/02-preset-parity-matrix.png"), sheet2.width, sheet2.height, &sheet2.rgba);
        save_png(&dir.join("renders/03-deep-zoom-and-export-parity.png"), sheet3.width, sheet3.height, &sheet3.rgba);
        let measurements = json!({
            "viewport": {"width": 800, "height": 600, "density": 1.0},
            "payload_json_bytes_20pts": payload_bytes,
            "latencies_ms": {
                "single_stroke_cpu": cpu_1_ms,
                "single_stroke_gpu_cold": gpu_1_cold_ms,
                "single_stroke_gpu_cached": gpu_1_cached_ms,
                "twenty_strokes_cpu": cpu_20_ms,
                "twenty_strokes_gpu_cold": gpu_20_cold_ms,
                "twenty_strokes_gpu_cached": gpu_20_cached_ms,
                "live_200pt_drag_step_avg": step_ms_sum / f64::from(steps.max(1)),
                "live_200pt_drag_step_max": step_ms_max,
                "live_drag_steps": steps,
            },
            "parity_rows": parity_rows,
        });
        std::fs::write(dir.join("measurements.json"), serde_json::to_string_pretty(&measurements).unwrap()).unwrap();
    }
}

#[test]
fn qol009_ui_screenshots_and_controls() {
    let _gpu_lock = photocraft_testkit::gpu_lock();
    let out_dir = std::env::var_os("KOMAPHONE_QOL009_EVIDENCE_DIR").map(std::path::PathBuf::from);
    if let Some(dir) = &out_dir {
        std::fs::create_dir_all(dir.join("renders")).unwrap();
    }

    // Build a multi-author shared canvas with rich translucent brush artwork + live remote peer brush preview.
    let c = canvas();
    let a1 = owner(0);
    let a2 = owner(1);
    let d1 = device(0);
    let d2 = device(1);
    let mut world = WorldState::new(c, a1, "Studio Shared Canvas".into()).unwrap();
    let (w, h) = (960u32, 570u32);

    let mut soft_blue = preset_brush("Soft Round");
    soft_blue.size = 56.0;
    soft_blue.hardness = 0.2;
    soft_blue.opacity = 0.45;
    soft_blue.color = [0.12, 0.40, 0.90, 1.0];
    soft_blue.seed = 301;
    append_stroke(&mut world, 0, make_world_stroke(1, 0, w, h, &[pt(140.0, 200.0), pt(560.0, 200.0)], &soft_blue, 0));

    let mut soft_coral = preset_brush("Soft Round");
    soft_coral.size = 56.0;
    soft_coral.hardness = 0.2;
    soft_coral.opacity = 0.45;
    soft_coral.color = [0.92, 0.24, 0.28, 1.0];
    soft_coral.seed = 302;
    append_stroke(&mut world, 0, make_world_stroke(2, 0, w, h, &[pt(260.0, 110.0), pt(420.0, 340.0)], &soft_coral, 0));

    let mut chalk = preset_brush("Chalk");
    chalk.size = 34.0;
    chalk.opacity = 0.85;
    chalk.color = [0.88, 0.50, 0.12, 1.0];
    chalk.seed = 303;
    append_stroke(&mut world, 1, make_world_stroke(3, 1, w, h, &wave_points(w, h, 24, 16.0), &chalk, 0));

    let mut soft_er = preset_brush("Soft Round");
    soft_er.size = 44.0;
    soft_er.hardness = 0.15;
    soft_er.opacity = 0.65;
    soft_er.erase = true;
    soft_er.seed = 304;
    append_stroke(&mut world, 1, make_world_stroke(4, 1, w, h, &[pt(320.0, 260.0), pt(420.0, 260.0)], &soft_er, 0));

    let mut peer_brush = preset_brush("Calligraphy Flat");
    peer_brush.size = 28.0;
    peer_brush.opacity = 0.7;
    peer_brush.color = [0.14, 0.68, 0.42, 1.0];
    peer_brush.seed = 305;
    let peer_sb = to_stroke_brush(&peer_brush, 1.0, true).unwrap();
    let ui_camera = pixel_view(w, h, 0).camera;
    let peer_preview = Peer {
        profile: a2,
        device: d2,
        camera: ui_camera.clone(),
        cursor: Some(coordinates::point(&RegionKey::default(), [620.0, 200.0]).unwrap()),
        preview: vec![[480.0, 320.0], [550.0, 240.0], [620.0, 200.0]],
        stroke: Some(PreviewStyle {
            id: StrokeId::new(&hex(0x9700)).unwrap(),
            anchor: RegionKey::default(),
            width: 28.0,
            color: Srgba { rgba: [0.14, 0.68, 0.42, 0.7] },
            erase: false,
            brush: Some(Box::new(peer_sb)),
            samples: None,
        }),
        expires_at_ms: 100_000,
    };

    let world = Arc::new(world);
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1280.0, 720.0))
        .with_pixels_per_point(1.0)
        .with_max_steps(32)
        .wgpu()
        .wgpu_setup(eframe::egui_wgpu::WgpuSetup::without_display_handle())
        .build_eframe(move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, crate::theme::ThemeKind::Studio);
            let mut session = photocraft_engine::Session::new();
            session.worlds.account = Some(Account { endpoint: "https://localhost:43195".into(), profile: a1, device: d1 });
            session.worlds.active = Some(Canvas {
                info: CanvasSummary {
                    canvas: c,
                    owner: a1,
                    name: world.name.clone(),
                    initial_name: world.name.clone(),
                    revision: world.revision,
                    epoch: 1,
                    import_origin: None,
                },
                canonical: Arc::clone(&world),
                display: Arc::clone(&world),
                pending: 0,
                viewer: Viewer { camera: ui_camera, bookmarks: vec![], review: None },
                notice: None,
            });
            session.worlds.peers = vec![peer_preview];
            session.tools.foreground = [0.16, 0.44, 0.90, 1.0];
            session.tools.background = [0.95, 0.72, 0.16, 1.0];
            session.tools.brush.size = 32.0;
            session.tools.brush.opacity = 0.55;
            session.tools.brush.flow = 0.80;
            session.tools.brush.hardness = 0.25;
            session.tools.brush.smoothing.enabled = true;
            session.tools.brush.smoothing.amount = 0.20;
            let mut app = PhotocraftApp::new(session, crate::Services::default());
            app.ui.komaphone.enabled = true;
            app.ui.world.open = true;
            app.ui.tool = Tool::Brush;
            if let Some(state) = cc.wgpu_render_state.as_ref() {
                app.set_wgpu(state.clone());
            }
            app
        });

    // Wait for background preparation worker to install the WorldRenderer and draw on wgpu.
    for _ in 0..30 {
        harness.run_steps(2);
        if harness.state().world_render.rendered_revision.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    harness.run_steps(4);
    assert!(harness.state().world_render.rendered_revision.is_some(), "shared canvas must finish GPU render");

    // Verify all QOL-009 toolbar controls are exposed and visible in 1280×720.
    for control in [
        "world.tool.brush",
        "world.tool.pencil",
        "world.tool.eraser",
        "world.tool.pan",
        "world.width",
        "world.opacity",
        "world.flow",
        "world.hardness",
        "world.smoothing",
        "world.brush.presets",
        "world.brush.settings",
        "world.color",
        "world.color.swap",
        "world.color.bg",
        "world.colors.panel",
        "world.lockWidth",
        "komaphone.world.undo",
        "komaphone.world.redo",
        "komaphone.world.home",
    ] {
        assert!(crate::komaphone_workspace::control_rect(&harness.ctx, control).is_some(), "missing control {control} at 1280x720");
    }

    if let Some(dir) = &out_dir {
        harness.render().unwrap().save(dir.join("renders/04-ui-world-brush-toolbar-1280x720.png")).unwrap();
    }

    // 2. Open Brush Presets picker from toolbar button.
    let presets_rect = crate::komaphone_workspace::control_rect(&harness.ctx, "world.brush.presets").unwrap();
    let ctx_clone = harness.ctx.clone();
    crate::brush_picker::open(&mut harness.state_mut().ui, &ctx_clone, [presets_rect.left(), presets_rect.bottom()]);
    harness.run_steps(4);
    assert!(harness.state().ui.brush_picker.is_some());
    if let Some(dir) = &out_dir {
        harness.render().unwrap().save(dir.join("renders/05-ui-world-brush-presets-picker.png")).unwrap();
    }
    harness.state_mut().ui.brush_picker = None;
    harness.run_steps(2);

    // 3. Open Colors popover by clicking the Colors toggle button.
    egui_kittest::kittest::Queryable::get_by_label(&harness, "Colors").click();
    harness.run_steps(4);
    if let Some(dir) = &out_dir {
        harness.render().unwrap().save(dir.join("renders/06-ui-world-colors-popover.png")).unwrap();
    }
    // Close Colors popover by clicking toggle button again.
    egui_kittest::kittest::Queryable::get_by_label(&harness, "Colors").click();
    harness.run_steps(2);

    // 4. Open Brush Settings (F5) window alongside Shared Canvas.
    harness.state_mut().ui.panels.brush_settings = true;
    harness.run_steps(5);
    assert!(harness.state().ui.panels.brush_settings);
    if let Some(dir) = &out_dir {
        harness.render().unwrap().save(dir.join("renders/07-ui-world-brush-settings-f5.png")).unwrap();
    }
}
