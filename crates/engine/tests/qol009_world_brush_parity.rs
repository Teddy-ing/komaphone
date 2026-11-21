#![cfg(not(target_arch = "wasm32"))]

use photocraft_color::{BlendMode, PixelFormat};
use photocraft_engine::{
    paint::{self, BrushSettings, GrayTile, Pattern, StrokePoint, TipShape, presets},
    scene::{
        CanvasId, OwnerId, RegionKey, Srgba, Stroke, StrokeId, Viewport,
        brush::{BrushTip, StrokeSample},
        coordinates,
        shared::{
            AcceptedOp, DeviceId, Edit, OpId, SHARED_SCHEMA_BRUSH, Tour, TourStop, WorldOp, WorldState,
            media::MediaFrame,
            tours::{ExportSettings, FrozenTour},
        },
    },
    world_brush::{LiveWorldBrush, MAX_BRUSH_WORK_SAMPLES, composite_brush_onto_layer, rasterize_world_stroke, to_stroke_brush, validate_stroke_brush_assets},
    world_media::{WorldImage, WorldRenderer, export_archive, export_tour_gif, export_view_png, read_archive, render_view},
};
use photocraft_raster::Surface;

fn hex(n: u128) -> String {
    format!("{n:032x}")
}
fn canvas() -> CanvasId {
    CanvasId::new(&hex(1)).unwrap()
}
fn owner(n: u128) -> OwnerId {
    OwnerId::new(&hex(10 + n)).unwrap()
}
fn device(n: u128) -> DeviceId {
    DeviceId::new(&hex(100 + n)).unwrap()
}
fn no_assets(_: &MediaFrame) -> photocraft_engine::Result<Vec<u8>> {
    panic!("Synthetic brush test has no media")
}

/// View of `[0, w) × [0, h)` in region `(depth, 0, 0)` at 1:1 scale so local `(x, y)` maps directly to screen `(x, y)`.
fn pixel_view(w: u32, h: u32, depth: u8) -> Viewport {
    let center = coordinates::point(&RegionKey { depth, ..Default::default() }, [f64::from(w) * 0.5, f64::from(h) * 0.5]).unwrap();
    Viewport { camera: coordinates::target(canvas(), center, depth, 1.0).unwrap(), width: w, height: h }
}

fn append(world: &mut WorldState, writer: u128, stroke: Stroke) {
    let rev = world.revision + 1;
    let seq = world.next_sequence(device(writer)).unwrap();
    world
        .apply(&AcceptedOp {
            revision: rev,
            op: WorldOp {
                id: OpId::new(&hex(10_000 + u128::from(rev))).unwrap(),
                canvas: world.canvas,
                author: owner(writer),
                device: device(writer),
                sequence: seq,
                authority_epoch: 1,
                action: Edit::AddStroke { stroke },
            },
        })
        .unwrap();
}

fn make_stroke(id: u128, writer: u128, depth: u8, brush: &BrushSettings, pts: &[(f64, f64, f64)]) -> Stroke {
    let sb = to_stroke_brush(brush, 1.0, false).unwrap();
    let has_times = pts.iter().any(|&(_, _, t)| t > 0.0);
    let samples = has_times.then(|| pts.iter().map(|&(_, _, t)| StrokeSample { time_ms: t, ..Default::default() }).collect());
    Stroke {
        id: StrokeId::new(&hex(id)).unwrap(),
        canvas: canvas(),
        owner: owner(writer),
        revision: 0,
        anchor: RegionKey { depth, ..Default::default() },
        points: pts.iter().map(|&(x, y, _)| [x, y]).collect(),
        width: f64::from(brush.size),
        color: Srgba { rgba: brush.color },
        erase: brush.erase,
        brush: Some(Box::new(sb)),
        samples,
    }
}

/// Render the same brush strokes via Party's `photocraft_paint::render_stroke` onto a
/// transparent RGBA32F layer and composite over opaque white (exact party equivalent).
fn render_party(w: u32, h: u32, strokes: &[(&BrushSettings, Vec<StrokePoint>)]) -> Vec<[f32; 4]> {
    let mut layer = Surface::new(PixelFormat::RGBA32F);
    for (brush, pts) in strokes {
        paint::render_stroke(&mut layer, brush, pts, None, false, 1.0);
    }
    let mut out = vec![[1.0f32; 4]; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let p = layer.rgba(x as i32, y as i32);
            let a = p[3].clamp(0.0, 1.0);
            let dst = &mut out[(y * w + x) as usize];
            for ch in 0..3 {
                dst[ch] = (p[ch] * a + 1.0 * (1.0 - a)).clamp(0.0, 1.0);
            }
            dst[3] = 1.0;
        }
    }
    out
}

fn max_channel_diff(a: &[[f32; 4]], b: &[[f32; 4]]) -> (f32, f32) {
    assert_eq!(a.len(), b.len());
    let mut max_d = 0.0f32;
    let mut sum_d = 0.0f64;
    for (pa, pb) in a.iter().zip(b) {
        for ch in 0..3 {
            let d = (pa[ch] - pb[ch]).abs();
            max_d = max_d.max(d);
            sum_d += f64::from(d);
        }
    }
    (max_d, (sum_d / (a.len() * 3) as f64) as f32)
}

#[test]
fn all_36_builtin_presets_encode_symbolically_and_compactly() {
    let all = presets::builtin();
    assert!(all.len() >= 36, "expected at least 36 built-in presets, got {}", all.len());
    for preset in &all {
        let mut b = preset.brush.clone();
        b.seed = 42;
        let sb = to_stroke_brush(&b, 1.0, false).unwrap_or_else(|e| panic!("preset {} failed: {e}", preset.name));
        assert!(!matches!(sb.tip, BrushTip::Sampled { .. }), "built-in preset {} should encode symbolically, not as raw PNG", preset.name);
        if let Some(dual) = &sb.dual_brush {
            assert!(!matches!(dual.tip, BrushTip::Sampled { .. }), "built-in dual preset {} should encode symbolically", preset.name);
        }
        let json = serde_json::to_vec(&sb).unwrap();
        assert!(json.len() < 1200, "built-in preset {} header too large: {} bytes", preset.name, json.len());
        validate_stroke_brush_assets(&sb).unwrap();
    }
}

#[test]
fn custom_sampled_tip_and_pattern_round_trip_and_reject_corrupt_png() {
    let custom_tip = GrayTile::from_fn(48, 32, |x, y| if (x + y) % 3 == 0 { 1.0 } else { 0.25 });
    let custom_pat = GrayTile::from_fn(24, 24, |x, y| if x < 12 && y < 12 { 0.8 } else { 0.2 });
    let mut brush = BrushSettings { size: 32.0, tip: TipShape::Sampled(custom_tip), seed: 7, ..Default::default() };
    brush.texture.enabled = true;
    brush.texture.pattern = Pattern::Tile(custom_pat);

    let sb = to_stroke_brush(&brush, 1.0, false).unwrap();
    assert!(matches!(sb.tip, BrushTip::Sampled { width: 48, height: 32, .. }));
    validate_stroke_brush_assets(&sb).unwrap();

    // Preview downscales oversized tips cleanly.
    let big_tip = GrayTile::from_fn(300, 300, |x, y| ((x ^ y) & 255) as f32 / 255.0);
    let big_brush = BrushSettings { size: 64.0, tip: TipShape::Sampled(big_tip), ..Default::default() };
    let sb_full = to_stroke_brush(&big_brush, 1.0, false).unwrap();
    let sb_prev = to_stroke_brush(&big_brush, 1.0, true).unwrap();
    validate_stroke_brush_assets(&sb_full).unwrap();
    validate_stroke_brush_assets(&sb_prev).unwrap();

    // Corrupt PNG payload with valid PNG magic prefix is rejected.
    let mut bad = sb;
    bad.tip = BrushTip::Sampled { width: 16, height: 16, png_b64: "iVBORw0KGgoAAAAA".into() };
    assert!(validate_stroke_brush_assets(&bad).is_err());
}

#[test]
fn party_vs_world_pixel_parity_across_opacity_overlap_presets_and_modes() {
    let (w, h) = (160, 120);
    let view = pixel_view(w, h, 0);
    let all = presets::builtin();
    let by_name = |name: &str| presets::find(&all, name).unwrap().brush.clone();

    // 1. Single-stroke self-overlap at 50% opacity must NOT double-darken (exact 50% ceiling).
    let mut half_hard = by_name("Hard Round");
    half_hard.size = 24.0;
    half_hard.opacity = 0.5;
    half_hard.color = [0.0, 0.0, 1.0, 1.0];
    half_hard.seed = 101;
    let pts = [(30.0, 60.0, 0.0), (130.0, 60.0, 0.0), (30.0, 60.0, 0.0)];
    let party = render_party(w, h, &[(&half_hard, pts.iter().map(|&(x, y, _)| StrokePoint::new(x, y, 1.0)).collect())]);
    let mut world = WorldState::new(canvas(), owner(1), "Opacity test".into()).unwrap();
    append(&mut world, 1, make_stroke(1, 1, 0, &half_hard, &pts));
    let world_img: WorldImage = render_view(&world, &view, 0, &mut no_assets).unwrap();
    let (max_d, mean_d) = max_channel_diff(&party, &world_img.pixels);
    assert!(max_d < 0.015 && mean_d < 0.002, "self-overlap parity drift: max={max_d}, mean={mean_d}");
    // Verify center pixel is ~50% blue over white: (0.5, 0.5, 1.0), NOT 75% (0.25, 0.25, 1.0).
    let mid = world_img.pixels[(60 * w + 80) as usize];
    assert!((mid[0] - 0.5).abs() < 0.02 && (mid[2] - 1.0).abs() < 0.02, "expected 50% blue, got {mid:?}");

    // 2. Second separate 50% stroke crossing the first compounds to 75% (0.25, 0.25, 1.0).
    let pts2 = [(80.0, 20.0, 0.0), (80.0, 100.0, 0.0)];
    let party2 = render_party(
        w,
        h,
        &[
            (&half_hard, pts.iter().map(|&(x, y, _)| StrokePoint::new(x, y, 1.0)).collect()),
            (&half_hard, pts2.iter().map(|&(x, y, _)| StrokePoint::new(x, y, 1.0)).collect()),
        ],
    );
    append(&mut world, 1, make_stroke(2, 1, 0, &half_hard, &pts2));
    let world_img2 = render_view(&world, &view, 0, &mut no_assets).unwrap();
    let (max_d2, mean_d2) = max_channel_diff(&party2, &world_img2.pixels);
    assert!(max_d2 < 0.015 && mean_d2 < 0.002, "two-stroke overlap parity drift: max={max_d2}, mean={mean_d2}");
    let cross = world_img2.pixels[(60 * w + 80) as usize];
    assert!((cross[0] - 0.25).abs() < 0.02, "expected 75% compound overlap (r=0.25), got {cross:?}");

    // 3. Partial-opacity soft eraser removes half of author 1's paint while leaving author 2 untouched.
    let mut author2_brush = by_name("Hard Round");
    author2_brush.size = 24.0;
    author2_brush.opacity = 1.0;
    author2_brush.color = [1.0, 0.0, 0.0, 1.0];
    append(&mut world, 2, make_stroke(3, 2, 0, &author2_brush, &[(80.0, 60.0, 0.0), (140.0, 60.0, 0.0)]));
    let mut soft_eraser = by_name("Soft Round");
    soft_eraser.size = 30.0;
    soft_eraser.opacity = 0.5;
    soft_eraser.erase = true;
    append(&mut world, 1, make_stroke(4, 1, 0, &soft_eraser, &[(50.0, 60.0, 0.0), (110.0, 60.0, 0.0)]));
    let after_erase = render_view(&world, &view, 0, &mut no_assets).unwrap();
    // At (50, 60): only author 1 was there (50% blue); 50% erase reduces blue alpha to ~25% -> r ≈ 0.75.
    let own_erased = after_erase.pixels[(60 * w + 50) as usize];
    assert!(own_erased[0] > 0.70 && own_erased[0] < 0.82, "expected partial own erase (r~0.75), got {own_erased:?}");
    // At (110, 60): author 2 painted opaque red over author 1; author 1's eraser MUST NOT touch author 2!
    let other_author = after_erase.pixels[(60 * w + 110) as usize];
    assert!(other_author[0] > 0.99 && other_author[1] < 0.01 && other_author[2] < 0.01, "author 1 eraser touched author 2: {other_author:?}");

    // 4. Representative preset suite: Soft Round, Hard Pencil, Calligraphy Flat, Airbrush Soft (stationary hold),
    //    Chalk, Charcoal, Watercolor Wet Edges, Ink Pen, Oil Bristle, Spatter, Dual Grain.
    type BrushParityCase = (&'static str, BrushSettings, Vec<(f64, f64, f64)>);
    let cases: Vec<BrushParityCase> = vec![
        (
            "Soft Round",
            BrushSettings { opacity: 0.6, color: [0.1, 0.3, 0.8, 1.0], seed: 11, ..by_name("Soft Round") },
            vec![(30.0, 60.0, 0.0), (130.0, 60.0, 0.0)],
        ),
        (
            "Hard Pencil",
            BrushSettings { size: 3.0, opacity: 0.5, color: [0.0, 0.0, 0.0, 1.0], seed: 12, ..by_name("Hard Pencil") },
            vec![(20.0, 40.0, 0.0), (140.0, 80.0, 0.0)],
        ),
        (
            "Calligraphy Flat",
            BrushSettings { color: [0.2, 0.1, 0.5, 1.0], seed: 13, ..by_name("Calligraphy Flat") },
            vec![(30.0, 80.0, 0.0), (80.0, 40.0, 0.0), (130.0, 80.0, 0.0)],
        ),
        (
            "Airbrush Soft hold",
            BrushSettings { size: 60.0, opacity: 0.7, color: [0.0, 0.5, 0.2, 1.0], seed: 14, ..by_name("Airbrush Soft") },
            vec![(80.0, 60.0, 0.0), (80.0, 60.0, 200.0), (80.0, 60.0, 400.0)],
        ),
        ("Chalk", BrushSettings { color: [0.7, 0.3, 0.1, 1.0], seed: 15, ..by_name("Chalk") }, vec![(30.0, 60.0, 0.0), (130.0, 60.0, 0.0)]),
        ("Charcoal", BrushSettings { color: [0.1, 0.1, 0.1, 1.0], seed: 16, ..by_name("Charcoal") }, vec![(30.0, 60.0, 0.0), (130.0, 60.0, 0.0)]),
        (
            "Watercolor Wet Edges",
            BrushSettings { opacity: 0.7, color: [0.1, 0.4, 0.9, 1.0], seed: 17, ..by_name("Watercolor Wet Edges") },
            vec![(35.0, 60.0, 0.0), (125.0, 60.0, 0.0)],
        ),
        (
            "Ink Pen",
            BrushSettings { color: [0.05, 0.05, 0.1, 1.0], seed: 18, ..by_name("Ink Pen") },
            vec![(20.0, 60.0, 0.0), (60.0, 35.0, 16.0), (100.0, 85.0, 32.0), (140.0, 60.0, 48.0)],
        ),
        (
            "Oil Bristle",
            BrushSettings { color: [0.8, 0.2, 0.1, 1.0], background: [1.0, 0.8, 0.2, 1.0], seed: 19, ..by_name("Oil Bristle") },
            vec![(35.0, 60.0, 0.0), (125.0, 60.0, 0.0)],
        ),
        ("Spatter", BrushSettings { color: [0.4, 0.1, 0.6, 1.0], seed: 20, ..by_name("Spatter") }, vec![(40.0, 60.0, 0.0), (120.0, 60.0, 0.0)]),
        ("Dual Grain", BrushSettings { color: [0.2, 0.5, 0.3, 1.0], seed: 21, ..by_name("Dual Grain") }, vec![(35.0, 60.0, 0.0), (125.0, 60.0, 0.0)]),
    ];

    for (idx, (label, brush, pts)) in cases.iter().enumerate() {
        let party_pts: Vec<StrokePoint> = pts.iter().map(|&(x, y, t)| StrokePoint { x, y, time: t, ..Default::default() }).collect();
        let party_px = render_party(w, h, &[(brush, party_pts)]);
        let mut wstate = WorldState::new(canvas(), owner(1), format!("Case {label}")).unwrap();
        let st = make_stroke(100 + idx as u128, 1, 0, brush, pts);
        append(&mut wstate, 1, st.clone());
        let world_px = render_view(&wstate, &view, 0, &mut no_assets).unwrap().pixels;
        let (max_d, mean_d) = max_channel_diff(&party_px, &world_px);
        // Textured/noisy brushes align seamlessly to `p0`, matching party within tight tolerance.
        let max_tol = if brush.texture.enabled || brush.noise || brush.dual_brush.enabled { 0.08 } else { 0.03 };
        let mean_tol = if brush.texture.enabled || brush.noise || brush.dual_brush.enabled { 0.01 } else { 0.004 };
        assert!(max_d <= max_tol && mean_d <= mean_tol, "{label}: party vs world drift max={max_d:.4} (tol {max_tol}), mean={mean_d:.4} (tol {mean_tol})");

        // Verify LiveWorldBrush incremental push matches one-shot rasterize_world_stroke bit-for-bit!
        let mut live =
            LiveWorldBrush::new(&Stroke { points: vec![st.points[0]], samples: st.samples.as_ref().map(|s| vec![s[0]]), ..st.clone() }, &view, 1.0).unwrap();
        for k in 2..=st.points.len() {
            live.sync_points(&st.points[..k], st.samples.as_deref().map(|s| &s[..k])).unwrap();
        }
        let live_raster = live.preview_raster().unwrap();
        let mut work = 0u64;
        let full_raster = rasterize_world_stroke(&st, &view, 1.0, &mut work, MAX_BRUSH_WORK_SAMPLES).unwrap();
        let mut live_layer = vec![[0.0f32; 4]; (w * h) as usize];
        let mut full_layer = vec![[0.0f32; 4]; (w * h) as usize];
        if let Some(r) = &live_raster {
            composite_brush_onto_layer(r, &mut live_layer, w, |_| {});
        }
        if let Some(r) = &full_raster {
            composite_brush_onto_layer(r, &mut full_layer, w, |_| {});
        }
        let (inc_max, _) = max_channel_diff(&live_layer, &full_layer);
        assert!(inc_max < 1e-5, "{label}: LiveWorldBrush diverged from one-shot raster: {inc_max}");
    }

    // 5. Behind and Multiply modes on same device layer match Party mode.
    let mut base = by_name("Hard Round");
    base.size = 30.0;
    base.color = [1.0, 0.8, 0.2, 1.0];
    let mut behind_b = base.clone();
    behind_b.color = [0.1, 0.2, 0.9, 1.0];
    behind_b.behind = true;
    let mut mult_b = base.clone();
    mult_b.color = [0.2, 0.6, 1.0, 1.0];
    mult_b.mode = BlendMode::Multiply;

    for (label, second) in [("Behind", behind_b), ("Multiply", mult_b)] {
        let p1 = vec![(50.0, 60.0, 0.0), (110.0, 60.0, 0.0)];
        let p2 = vec![(80.0, 30.0, 0.0), (80.0, 90.0, 0.0)];
        let party_px = render_party(
            w,
            h,
            &[
                (&base, p1.iter().map(|&(x, y, _)| StrokePoint::new(x, y, 1.0)).collect()),
                (&second, p2.iter().map(|&(x, y, _)| StrokePoint::new(x, y, 1.0)).collect()),
            ],
        );
        let mut wstate = WorldState::new(canvas(), owner(1), label.into()).unwrap();
        append(&mut wstate, 1, make_stroke(300, 1, 0, &base, &p1));
        append(&mut wstate, 1, make_stroke(301, 1, 0, &second, &p2));
        let world_px = render_view(&wstate, &view, 0, &mut no_assets).unwrap().pixels;
        let (max_d, mean_d) = max_channel_diff(&party_px, &world_px);
        assert!(max_d < 0.02 && mean_d < 0.003, "{label}: max={max_d}, mean={mean_d}");
    }
}

#[test]
fn airbrush_stationary_hold_builds_up_over_time_while_single_dab_stays_light() {
    let (w, h) = (128, 128);
    let view = pixel_view(w, h, 0);
    let all = presets::builtin();
    let mut airbrush = presets::find(&all, "Airbrush Soft").unwrap().brush.clone();
    airbrush.size = 60.0;
    airbrush.opacity = 0.8;
    airbrush.color = [0.0, 0.0, 0.0, 1.0];

    let mut short_world = WorldState::new(canvas(), owner(1), "Short airbrush".into()).unwrap();
    append(&mut short_world, 1, make_stroke(1, 1, 0, &airbrush, &[(64.0, 64.0, 0.0)]));
    let short_img = render_view(&short_world, &view, 0, &mut no_assets).unwrap();

    let mut held_world = WorldState::new(canvas(), owner(1), "Held airbrush".into()).unwrap();
    let hold_pts: Vec<(f64, f64, f64)> = (0..=40).map(|i| (64.0, 64.0, f64::from(i) * 16.0)).collect();
    append(&mut held_world, 1, make_stroke(2, 1, 0, &airbrush, &hold_pts));
    let held_img = render_view(&held_world, &view, 0, &mut no_assets).unwrap();

    let short_lum = short_img.pixels[(64 * w + 64) as usize][0];
    let held_lum = held_img.pixels[(64 * w + 64) as usize][0];
    // Single dab at flow=0.1, opacity=0.8 gives darkness ~0.08 (lum ~0.92).
    // 640 ms hold at 25 dabs/sec (~17 dabs) builds up smoothly toward opacity ceiling 0.8 (lum ~0.20..0.35), never exceeding 0.8 (lum >= 0.19).
    assert!(short_lum > 0.85, "single airbrush dab too dark: {short_lum}");
    assert!((0.19..0.45).contains(&held_lum), "held airbrush failed to build up within opacity ceiling: {held_lum}");
}

#[test]
fn deep_zoom_catchup_tour_and_schema2_archive_agree_on_brush_strokes() {
    let dir = tempfile::tempdir().unwrap();
    let all = presets::builtin();
    let mut soft = presets::find(&all, "Soft Round").unwrap().brush.clone();
    soft.size = 36.0;
    soft.opacity = 0.5;
    soft.color = [0.1, 0.6, 0.3, 1.0];

    let mut world = WorldState::new(canvas(), owner(1), "Deep zoom brushes".into()).unwrap();
    // Depth-0 stroke and depth-6 fine detail stroke!
    append(&mut world, 1, make_stroke(1, 1, 0, &soft, &[(40.0, 60.0, 0.0), (120.0, 60.0, 0.0)]));
    append(&mut world, 1, make_stroke(2, 1, 6, &soft, &[(40.0, 60.0, 0.0), (120.0, 60.0, 0.0)]));
    assert_eq!(world.schema, SHARED_SCHEMA_BRUSH);

    let view_d6 = pixel_view(160, 120, 6);
    let live_d6 = render_view(&world, &view_d6, 0, &mut no_assets).unwrap();
    assert!(live_d6.pixels[(60 * 160 + 80) as usize][1] > 0.7 && live_d6.pixels[(60 * 160 + 80) as usize][0] < 0.75);

    // Catch-up replay to revision 1 hides the depth-6 stroke cleanly.
    let replay1 = world.replay_to(1).unwrap();
    let r1_d6 = render_view(&replay1, &view_d6, 0, &mut no_assets).unwrap();
    assert!(r1_d6.pixels[(60 * 160 + 80) as usize][0] > 0.99, "depth-6 stroke should not exist at rev 1");

    // Archive schema-2 round-trip preserves exact render pixels.
    let archive_path = dir.path().join("brush-world.komworld");
    export_archive(&world, &archive_path, &mut no_assets, |_, _| true).unwrap();
    let (reopened, _) = read_archive(&archive_path).unwrap();
    assert_eq!(reopened, world);
    let reopened_d6 = render_view(&reopened, &view_d6, 0, &mut no_assets).unwrap();
    assert_eq!(reopened_d6.pixels, live_d6.pixels);

    // PNG view export and Tour GIF export both succeed cleanly with brush strokes.
    let png_path = dir.path().join("view.png");
    export_view_png(&world, &view_d6, 0, &png_path, &mut no_assets).unwrap();
    assert!(std::fs::metadata(&png_path).unwrap().len() > 100);

    let tour = Tour {
        id: photocraft_scene::shared::TourId::new(&hex(77)).unwrap(),
        canvas: canvas(),
        name: "Deep brush tour".into(),
        revision: 0,
        stops: vec![
            TourStop { camera: pixel_view(160, 120, 0).camera, pause_ms: 250, travel_ms: 0, target: None },
            TourStop { camera: view_d6.camera.clone(), pause_ms: 250, travel_ms: 500, target: None },
        ],
    };
    let tour_id = tour.id;
    let rev = world.revision + 1;
    world
        .apply(&AcceptedOp {
            revision: rev,
            op: WorldOp {
                id: OpId::new(&hex(1000 + u128::from(rev))).unwrap(),
                canvas: canvas(),
                author: owner(0),
                device: device(0),
                sequence: world.next_sequence(device(0)).unwrap(),
                authority_epoch: 1,
                action: Edit::PutTour { tour, expected_revision: 0 },
            },
        })
        .unwrap();
    let frozen = FrozenTour::new(&world, tour_id, ExportSettings { width: 160, height: 120, fps: 4 }).unwrap();
    let gif_path = dir.path().join("tour.gif");
    export_tour_gif(&frozen, &gif_path, true, &mut no_assets, |_, _| true).unwrap();
    assert!(std::fs::metadata(&gif_path).unwrap().len() > 100);

    // Offscreen depth-0 stroke at (40, 60) is cleanly culled from `view_d6` near (0, 0);
    // once a depth-0 ancestor actually intersects (0, 0), `visible_content(&view_d6)` includes both!
    let mut renderer = WorldRenderer::new(&world).unwrap();
    assert_eq!(renderer.visible_content(&view_d6).unwrap().len(), 1);
    append(&mut world, 1, make_stroke(3, 1, 0, &soft, &[(-10.0, 0.0, 0.0), (10.0, 0.0, 0.0)]));
    let mut renderer2 = WorldRenderer::new(&world).unwrap();
    assert_eq!(renderer2.visible_content(&view_d6).unwrap().len(), 2);
}

#[test]
fn archive_v3_ten_thousand_five_hundred_revisions_roundtrip_and_size_bound() {
    let mut world = WorldState::new(canvas(), owner(0), "Archive 10500".into()).unwrap();
    for rev in 1..=10_500_u64 {
        let author = (rev % 6) as u128;
        let mut points = Vec::with_capacity(60);
        let mut samples = Vec::with_capacity(60);
        let bx = ((rev % 80) as f64 - 40.0) * 10.0;
        let by = (((rev / 80) % 80) as f64 - 40.0) * 10.0;
        for i in 0..60 {
            let t = i as f64;
            points.push([bx + t * 1.2, by + (t * 0.2).sin() * 4.0]);
            samples.push(StrokeSample { time_ms: t * 8.0, ..Default::default() });
        }
        let stroke = Stroke {
            id: StrokeId::new(&hex(500_000 + u128::from(rev))).unwrap(),
            canvas: canvas(),
            owner: owner(author),
            revision: 0,
            anchor: RegionKey {
                depth: if rev % 10 == 0 {
                    12
                } else if rev % 5 == 0 {
                    6
                } else {
                    0
                },
                ..Default::default()
            },
            points,
            width: 12.0,
            color: Srgba { rgba: [0.2, 0.4, 0.7, 0.8] },
            erase: false,
            brush: Some(Box::new(photocraft_scene::brush::StrokeBrush { authored_size: 12.0, hardness: 0.5, opacity: 0.8, seed: rev, ..Default::default() })),
            samples: Some(samples),
        };
        append(&mut world, author, stroke);
    }
    assert_eq!(world.revision, 10_500);

    let dir = tempfile::tempdir().unwrap();
    let archive_path = dir.path().join("scale-10500.komworld");
    export_archive(&world, &archive_path, &mut no_assets, |_, _| true).unwrap();

    let world_json_bytes = std::fs::metadata(archive_path.join("world.json")).unwrap().len();
    eprintln!("[CP5 10,500-rev ARCHIVE v3] world.json={:.2} MiB ({} bytes)", world_json_bytes as f64 / (1024.0 * 1024.0), world_json_bytes);
    assert!(world_json_bytes <= 12 * 1024 * 1024, "10,500-rev .komworld world.json exceeded 12 MiB target: {} bytes", world_json_bytes);

    let (restored, assets) = read_archive(&archive_path).unwrap();
    assert!(assets.is_empty());
    assert_eq!(restored.revision, 10_500);
    assert_eq!(restored.strokes.len(), 10_500);
    assert_eq!(restored.canonical_hash().unwrap(), world.canonical_hash().unwrap());

    let upload = photocraft_engine::world_media::inspect_archive_for_upload(&archive_path, &mut || true).unwrap();
    assert_eq!(upload.world.revision, 10_500);
    assert_eq!(upload.manifest_bytes, world_json_bytes);
}
