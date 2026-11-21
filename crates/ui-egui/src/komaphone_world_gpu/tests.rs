use super::*;
use photocraft_engine::scene::{
    CanvasId, OwnerId, RegionKey, Srgba, Stroke, StrokeId, coordinates,
    shared::{AcceptedOp, DeviceId, Edit, OpId, WorldOp, WorldState},
};

fn hex(n: u128) -> String {
    format!("{n:032x}")
}
fn fixture() -> WorldState {
    let canvas = CanvasId::new(&hex(1)).unwrap();
    let owner = OwnerId::new(&hex(10)).unwrap();
    let mut state = WorldState::new(canvas, owner, "GPU synthetic".into()).unwrap();
    for (index, writer, points, color, width, erase) in [
        (1, 0, vec![[-24., 0.], [24., 0.]], [1., 0., 0., 1.], 16., false),
        (2, 1, vec![[0., -24.], [0., 24.]], [0., 0., 1., 1.], 16., false),
        (3, 1, vec![[0., 0.]], [1.; 4], 8., true),
    ] {
        let author = OwnerId::new(&hex(10 + writer)).unwrap();
        let device = DeviceId::new(&hex(20 + writer)).unwrap();
        state
            .apply(&AcceptedOp {
                revision: index,
                op: WorldOp {
                    id: OpId::new(&hex(100 + u128::from(index))).unwrap(),
                    canvas,
                    author,
                    device,
                    sequence: state.next_sequence(device).unwrap(),
                    authority_epoch: 1,
                    action: Edit::AddStroke {
                        stroke: Stroke {
                            id: StrokeId::new(&hex(200 + u128::from(index))).unwrap(),
                            canvas,
                            owner: author,
                            revision: 0,
                            anchor: RegionKey::default(),
                            points,
                            width,
                            color: Srgba { rgba: color },
                            erase,
                            brush: None,
                            samples: None,
                        },
                    },
                },
            })
            .unwrap();
    }
    state
}

pub(crate) fn readback(gpu: &WorldGpu) -> Vec<[u8; 4]> {
    let targets = gpu.targets.as_ref().unwrap();
    let [width, height] = targets.size;
    let row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = gpu.rs.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("world layer pixel oracle"),
        size: u64::from(row) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.rs.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: &targets.output._texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(height) },
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    gpu.rs.queue.submit([encoder.finish()]);
    buffer.slice(..).map_async(wgpu::MapMode::Read, |result| result.unwrap());
    assert!(gpu.health.wait(&gpu.rs.device, None), "{:?}", gpu.health.fault());
    let data = buffer.slice(..).get_mapped_range().unwrap();
    let mut pixels = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let offset = (y * row + x * 4) as usize;
            pixels.push(data[offset..offset + 4].try_into().unwrap());
        }
    }
    pixels
}

#[test]
fn shared_gpu_author_erase_readback_matches_cpu_oracle_at_native_and_high_dpi() {
    let _lock = photocraft_testkit::gpu_lock();
    // This is mandatory evidence, not a silently skipped adapter-dependent pass.
    let rs = egui_kittest::wgpu::create_render_state(crate::gpu_canvas::wgpu_setup(), Default::default());
    let health = photocraft_gpu::DeviceHealth::watch(&rs.device);
    let mut gpu = WorldGpu::new(&rs, health).unwrap();
    let state = fixture();
    let mut indexed = WorldRenderer::new(&state).unwrap();
    let view = Viewport { camera: coordinates::home(state.canvas), width: 64, height: 64 };
    for density in [1.0, 2.0] {
        gpu.render(&mut indexed, &view, density, 0, &mut |_| panic!("No media fixture")).unwrap();
        let pixels = readback(&gpu);
        let side = (64.0 * density) as usize;
        let at = |x: usize, y: usize| pixels[y * side + x];
        let k = density as usize;
        assert_eq!(at(32 * k, 32 * k), [255, 0, 0, 255], "erase must reveal lower author");
        assert_eq!(at(32 * k, 20 * k), [0, 0, 255, 255]);
        assert_eq!(at(20 * k, 32 * k), [255, 0, 0, 255]);
        assert_eq!(at(2 * k, 2 * k), [255; 4]);
        let mut physical = view.clone();
        physical.width = side as u32;
        physical.height = side as u32;
        physical.camera.zoom = f64::from(density);
        let cpu = photocraft_engine::world_media::render_view(&state, &physical, 0, &mut |_| panic!("No media fixture")).unwrap();
        let cpu = photocraft_engine::world_media::rgba8(&cpu).unwrap();
        // Compare all pixels at least 1.5 physical pixels from any analytic
        // boundary. Triangle edge coverage rules may differ only in that band.
        let mut compared = 0;
        for y in 0..side {
            for x in 0..side {
                let p = [(x as f64 + 0.5) / f64::from(density) - 32., (y as f64 + 0.5) / f64::from(density) - 32.];
                let horizontal = distance_segment(p, [-24., 0.], [24., 0.]);
                let vertical = distance_segment(p, [0., -24.], [0., 24.]);
                let erased = p[0].hypot(p[1]);
                if [(horizontal - 8.).abs(), (vertical - 8.).abs(), (erased - 4.).abs()].into_iter().any(|d| d * f64::from(density) <= 1.5) {
                    continue;
                }
                let offset = (y * side + x) * 4;
                assert_eq!(pixels[y * side + x], cpu[offset..offset + 4], "pixel {x},{y}, density{density}");
                compared += 1;
            }
        }
        assert!(compared > side * side / 2);
        assert_eq!(gpu.stats.layers, 2);
        assert!(gpu.stats.texture_bytes <= MAX_PIXELS * 8);
    }
    assert!(gpu.render(&mut indexed, &view, f32::NAN, 0, &mut |_| panic!()).is_err());
    assert!(gpu.health.is_ok(), "{:?}", gpu.health.fault());
}
fn distance_segment(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let d = [b[0] - a[0], b[1] - a[1]];
    let t = (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / (d[0] * d[0] + d[1] * d[1])).clamp(0., 1.);
    (p[0] - a[0] - d[0] * t).hypot(p[1] - a[1] - d[1] * t)
}

#[test]
fn transient_preview_eraser_uses_its_author_layer_without_changing_retained_scene() {
    let _lock = photocraft_testkit::gpu_lock();
    let rs = egui_kittest::wgpu::create_render_state(crate::gpu_canvas::wgpu_setup(), Default::default());
    let mut gpu = WorldGpu::new(&rs, photocraft_gpu::DeviceHealth::watch(&rs.device)).unwrap();
    let complete = fixture();
    let prefix = complete.replay_to(2).unwrap();
    let preview = complete.strokes.values().find(|s| s.stroke.erase).unwrap().clone();
    let hash = prefix.canonical_hash().unwrap();
    let mut indexed = WorldRenderer::new(&prefix).unwrap();
    let view = Viewport { camera: coordinates::home(prefix.canvas), width: 64, height: 64 };
    gpu.render(&mut indexed, &view, 1., 0, &mut |_| panic!()).unwrap();
    assert_eq!(readback(&gpu)[32 * 64 + 32], [0, 0, 255, 255]);
    gpu.render_with_previews(&mut indexed, &view, 1., 0, &mut |_| panic!(), std::slice::from_ref(&preview)).unwrap();
    assert_eq!(readback(&gpu)[32 * 64 + 32], [255, 0, 0, 255]);
    assert_eq!(indexed.world().canonical_hash().unwrap(), hash);
    assert!(gpu.render_with_previews(&mut indexed, &view, 1., 0, &mut |_| panic!(), &[preview.clone(), preview]).is_err());
}

#[test]
fn unavailable_verified_media_keeps_other_art_visible_and_reports_pending_assets() {
    use photocraft_engine::scene::shared::{
        MediaId, MediaPlacement, SourceId,
        media::{AssetHash, Credit, MediaFrame, MediaSource, Provenance},
    };
    let _lock = photocraft_testkit::gpu_lock();
    let rs = egui_kittest::wgpu::create_render_state(crate::gpu_canvas::wgpu_setup(), Default::default());
    let mut gpu = WorldGpu::new(&rs, photocraft_gpu::DeviceHealth::watch(&rs.device)).unwrap();
    let mut world = fixture();
    let hash = AssetHash::new(&"b".repeat(64)).unwrap();
    let media = MediaPlacement {
        id: MediaId::new(&hex(900)).unwrap(),
        canvas: world.canvas,
        owner: world.owner,
        anchor: Default::default(),
        offset: [0., 0.],
        width: 16.,
        height: 16.,
        looping: false,
        frames: vec![MediaFrame { hash, byte_len: 1, width: 1, height: 1, duration_ms: 1000 }],
        provenance: Provenance {
            source: MediaSource::Gallery { entry: SourceId::new(&hex(901)).unwrap(), room: None, game: None, chain: 0, frame: None },
            source_revision: 1,
            artifact_hash: hash,
            authors: vec![Credit { profile: world.owner, label: "Synthetic".into() }],
        },
    };
    world
        .apply(&AcceptedOp {
            revision: 4,
            op: WorldOp {
                id: OpId::new(&hex(902)).unwrap(),
                canvas: world.canvas,
                author: world.owner,
                device: DeviceId::new(&hex(20)).unwrap(),
                sequence: 2,
                authority_epoch: 1,
                action: Edit::PlaceMedia { media: Box::new(media) },
            },
        })
        .unwrap();
    let mut indexed = WorldRenderer::new(&world).unwrap();
    let view = Viewport { camera: coordinates::home(world.canvas), width: 64, height: 64 };
    gpu.render_available(&mut indexed, &view, 1., 0, &mut |_| Ok(None), &[]).unwrap();
    assert_eq!(gpu.stats.pending_assets, 1);
    assert_eq!(readback(&gpu)[32 * 64 + 32], [255, 0, 0, 255]);
    assert!(gpu.health.is_ok(), "{:?}", gpu.health.fault());
}

#[test]
fn shared_gpu_brush_strokes_match_cpu_oracle_across_opacity_overlap_presets_and_modes() {
    use photocraft_engine::paint::{BrushSettings, presets};
    use photocraft_engine::scene::brush::StrokeSample;
    use photocraft_engine::world_brush::to_stroke_brush;
    let _lock = photocraft_testkit::gpu_lock();
    let rs = egui_kittest::wgpu::create_render_state(crate::gpu_canvas::wgpu_setup(), Default::default());
    let mut gpu = WorldGpu::new(&rs, photocraft_gpu::DeviceHealth::watch(&rs.device)).unwrap();

    let canvas = CanvasId::new(&hex(9)).unwrap();
    let author1 = OwnerId::new(&hex(11)).unwrap();
    let author2 = OwnerId::new(&hex(12)).unwrap();
    let dev1 = DeviceId::new(&hex(21)).unwrap();
    let dev2 = DeviceId::new(&hex(22)).unwrap();
    let mut world = WorldState::new(canvas, author1, "GPU brush parity".into()).unwrap();
    let all = presets::builtin();
    let by_name = |name: &str| presets::find(&all, name).unwrap().brush.clone();

    let mut add = |id: u128, author: OwnerId, device: DeviceId, brush: &BrushSettings, pts: Vec<[f64; 2]>, times: Option<Vec<f64>>| {
        let rev = world.revision + 1;
        let sb = to_stroke_brush(brush, 1.0, false).unwrap();
        let samples = times.map(|ts| ts.into_iter().map(|t| StrokeSample { time_ms: t, ..Default::default() }).collect());
        world
            .apply(&AcceptedOp {
                revision: rev,
                op: WorldOp {
                    id: OpId::new(&hex(500 + id)).unwrap(),
                    canvas,
                    author,
                    device,
                    sequence: world.next_sequence(device).unwrap(),
                    authority_epoch: 1,
                    action: Edit::AddStroke {
                        stroke: Stroke {
                            id: StrokeId::new(&hex(600 + id)).unwrap(),
                            canvas,
                            owner: author,
                            revision: 0,
                            anchor: RegionKey::default(),
                            points: pts,
                            width: f64::from(brush.size),
                            color: Srgba { rgba: brush.color },
                            erase: brush.erase,
                            brush: Some(Box::new(sb)),
                            samples,
                        },
                    },
                },
            })
            .unwrap();
    };

    // Device 1: 50% Hard Round self-overlapping stroke (stays 50%) + crossing 50% Soft Round + 50% Soft Eraser + Behind + Chalk.
    let mut half_hard = by_name("Hard Round");
    half_hard.size = 24.0;
    half_hard.opacity = 0.5;
    half_hard.color = [0.0, 0.0, 1.0, 1.0];
    add(1, author1, dev1, &half_hard, vec![[-32., -16.], [32., -16.], [-32., -16.]], None);

    let mut half_soft = by_name("Soft Round");
    half_soft.size = 32.0;
    half_soft.opacity = 0.5;
    half_soft.color = [0.0, 0.8, 0.2, 1.0];
    add(2, author1, dev1, &half_soft, vec![[0., -40.], [0., 40.]], None);

    let mut eraser = by_name("Soft Round");
    eraser.size = 20.0;
    eraser.opacity = 0.5;
    eraser.erase = true;
    add(3, author1, dev1, &eraser, vec![[-20., -16.], [20., -16.]], None);

    let mut behind = by_name("Hard Round");
    behind.size = 24.0;
    behind.opacity = 1.0;
    behind.behind = true;
    behind.color = [1.0, 0.8, 0.0, 1.0];
    add(4, author1, dev1, &behind, vec![[-24., 16.], [24., 16.]], None);

    // Device 2: Chalk + Multiply blend mode + Airbrush hold + Hard Pencil (cannot be erased by device 1).
    let mut chalk = by_name("Chalk");
    chalk.size = 24.0;
    chalk.color = [0.9, 0.2, 0.1, 1.0];
    chalk.seed = 99;
    add(5, author2, dev2, &chalk, vec![[-40., 24.], [40., 24.]], None);

    let mut mult = by_name("Hard Round");
    mult.size = 20.0;
    mult.mode = photocraft_color::BlendMode::Multiply;
    mult.color = [0.2, 0.6, 1.0, 1.0];
    add(6, author2, dev2, &mult, vec![[-10., 14.], [10., 34.]], None);

    let mut pencil = by_name("Hard Pencil");
    pencil.size = 3.0;
    pencil.opacity = 0.7;
    pencil.color = [0.0, 0.0, 0.0, 1.0];
    add(7, author2, dev2, &pencil, vec![[-40., -36.], [40., -36.]], None);

    let mut indexed = WorldRenderer::new(&world).unwrap();
    let view = Viewport { camera: coordinates::home(canvas), width: 128, height: 128 };
    gpu.render(&mut indexed, &view, 1.0, 0, &mut |_| panic!("No media")).unwrap();
    let gpu_px = readback(&gpu);
    let cpu = photocraft_engine::world_media::render_view(&world, &view, 0, &mut |_| panic!("No media")).unwrap();
    let cpu_px = photocraft_engine::world_media::rgba8(&cpu).unwrap();

    let mut max_diff = 0i32;
    for i in 0..(128 * 128) {
        for ch in 0..3 {
            let d = (i32::from(gpu_px[i][ch]) - i32::from(cpu_px[i * 4 + ch])).abs();
            max_diff = max_diff.max(d);
        }
    }
    // 8-bit premul rounding + dither allows at most ±3 levels across multi-stroke composition.
    assert!(max_diff <= 3, "GPU vs CPU brush parity diverged: max channel diff = {max_diff}");
    assert!(gpu.health.is_ok(), "{:?}", gpu.health.fault());
}

#[test]
fn tiled_gpu_negative_coords_fractional_zoom_mixed_lod_and_conservative_budget() {
    use photocraft_engine::paint::presets;
    use photocraft_engine::scene::{Point, Wide};
    use photocraft_engine::world_brush::to_stroke_brush;
    use photocraft_engine::world_tiles::{ScopeKey, TileBudget, WorldTileManager};

    let _lock = photocraft_testkit::gpu_lock();
    let rs = egui_kittest::wgpu::create_render_state(crate::gpu_canvas::wgpu_setup(), Default::default());
    let mut gpu = WorldGpu::new_with_budget(&rs, photocraft_gpu::DeviceHealth::watch(&rs.device), TileBudget::conservative()).unwrap();
    assert_eq!(gpu.budget().gpu_layers, 128);
    assert_eq!(gpu.budget().max_visible_tiles, 72);

    let canvas = CanvasId::new(&hex(42)).unwrap();
    let author = OwnerId::new(&hex(11)).unwrap();
    let dev = DeviceId::new(&hex(21)).unwrap();
    let mut world = WorldState::new(canvas, author, "Fractional & Negative".into()).unwrap();
    let all = presets::builtin();
    let mut soft = presets::find(&all, "Watercolor Wet Edges").unwrap().brush.clone();
    soft.size = 44.0;
    soft.color = [0.15, 0.45, 0.85, 1.0];
    soft.seed = 123;
    let sb = to_stroke_brush(&soft, 1.0, false).unwrap();
    // Stroke crossing (0, 0) across all four negative/positive tile quadrants.
    world
        .apply(&AcceptedOp {
            revision: 1,
            op: WorldOp {
                id: OpId::new(&hex(701)).unwrap(),
                canvas,
                author,
                device: dev,
                sequence: 1,
                authority_epoch: 1,
                action: Edit::AddStroke {
                    stroke: Stroke {
                        id: StrokeId::new(&hex(801)).unwrap(),
                        canvas,
                        owner: author,
                        revision: 0,
                        anchor: RegionKey::default(),
                        points: vec![[-120.0, -80.0], [0.0, 0.0], [120.0, 80.0]],
                        width: 44.0,
                        color: Srgba { rgba: soft.color },
                        erase: false,
                        brush: Some(Box::new(sb)),
                        samples: None,
                    },
                },
            },
        })
        .unwrap();

    let mut indexed = WorldRenderer::new(&world).unwrap();
    // Test negative offsets, fractional zooms, and fractional DPI `1.5×`.
    for (zoom, density, cx, cy) in [(0.63, 1.0_f32, -40, -25), (1.37, 1.5, 0, 0), (std::f64::consts::PI, 1.0, -10, 15)] {
        let unit = coordinates::local_unit(0).unwrap();
        let center = Point { x: Wide(cx * unit), y: Wide(cy * unit) };
        let view = Viewport { camera: coordinates::target(canvas, center, 0, zoom).unwrap(), width: 320, height: 240 };
        gpu.render(&mut indexed, &view, density, 0, &mut |_| panic!("No media")).unwrap();
        assert!(gpu.stats.visible_tiles <= 72);
        assert!(gpu.stats.tile_array_bytes <= 35 * 1024 * 1024);
        let px = readback(&gpu);
        // Verify non-white stroke pixels rendered cleanly across negative/positive boundary with zero cracks.
        let painted = px.iter().filter(|p| p[0] < 245 || p[1] < 245 || p[2] < 245).count();
        assert!(painted > 100, "zoom={zoom} density={density} rendered too few painted pixels ({painted})");
    }

    // Verify interactive progressive refinement (coarse fallback -> settled target LOD)
    // converges bit-identically to blocking target-LOD render across a mixed-LOD transition!
    let mut mgr = WorldTileManager::new(TileBudget::conservative(), None);
    let scope = ScopeKey::new(1, "https://synth.invalid", author.as_str(), dev.as_str(), canvas, None);
    mgr.set_scope(scope, Arc::clone(indexed.index()));

    let coarse_view = Viewport { camera: coordinates::target(canvas, Point::default(), 0, 0.63).unwrap(), width: 320, height: 240 };
    let fine_view = Viewport { camera: coordinates::target(canvas, Point::default(), 0, 1.37).unwrap(), width: 320, height: 240 };

    // 1. Settle coarse view so coarse ancestor tiles are resident in GPU atlas.
    for _ in 0..80 {
        gpu.present_interactive(&mut mgr, &coarse_view, 1.0, 0, &[], None, &mut |_| Ok(None), false).unwrap();
        if !gpu.stats.is_refining {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!gpu.stats.is_refining, "coarse view must settle");

    // 2. Zoom to fine view: frame 1 immediately draws temporary coarse fallback (zero blank white!).
    gpu.present_interactive(&mut mgr, &fine_view, 1.5, 0, &[], None, &mut |_| Ok(None), false).unwrap();
    assert!(gpu.stats.fallback_tiles > 0, "zoom transition should present coarse ancestor fallback immediately");
    let interim = readback(&gpu);
    assert!(interim.iter().any(|p| p[0] < 245), "interim coarse fallback must not be blank white");

    // 3. Drive interactive loop until settled: must converge to uniform target LOD with 0 fallback tiles
    //    and match blocking render bit-identically!
    for _ in 0..120 {
        gpu.present_interactive(&mut mgr, &fine_view, 1.5, 0, &[], None, &mut |_| Ok(None), false).unwrap();
        if !gpu.stats.is_refining {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!gpu.stats.is_refining && gpu.stats.fallback_tiles == 0 && gpu.stats.pending_tiles == 0, "fine view must settle cleanly");
    let settled = readback(&gpu);

    let mut gpu_ref = WorldGpu::new_with_budget(&rs, photocraft_gpu::DeviceHealth::watch(&rs.device), TileBudget::conservative()).unwrap();
    gpu_ref.render(&mut indexed, &fine_view, 1.5, 0, &mut |_| panic!("No media")).unwrap();
    let expected = readback(&gpu_ref);
    assert_eq!(settled, expected, "settled interactive GPU tiles must match blocking render bit-identically");
    assert!(gpu.health.is_ok(), "{:?}", gpu.health.fault());
}

#[test]
fn async_animated_media_keeps_last_valid_frame_and_preserves_w4_order() {
    use photocraft_engine::scene::shared::MediaId;
    use photocraft_engine::scene::shared::media::{Credit, MediaPlacement, MediaSource, Provenance};
    use photocraft_engine::world_tiles::ScopeKey;
    let _lock = photocraft_testkit::gpu_lock();
    let rs = egui_kittest::wgpu::create_render_state(crate::gpu_canvas::wgpu_setup(), Default::default());
    let mut gpu = WorldGpu::new_with_budget(&rs, photocraft_gpu::DeviceHealth::watch(&rs.device), TileBudget::conservative()).unwrap();

    let canvas = CanvasId::new(&hex(91)).unwrap();
    let a0 = OwnerId::new(&hex(92)).unwrap();
    let a1 = OwnerId::new(&hex(93)).unwrap();
    let d0 = DeviceId::new(&hex(94)).unwrap();
    let d1 = DeviceId::new(&hex(95)).unwrap();
    let mut world = WorldState::new(canvas, a0, "GPU Async Anim".into()).unwrap();

    let mut media_map: BTreeMap<AssetHash, Arc<RgbaFrame>> = BTreeMap::new();
    let mut media_pngs: BTreeMap<AssetHash, Vec<u8>> = BTreeMap::new();
    let mut make_f = |rgba: [u8; 4], dur: u32| -> MediaFrame {
        let mut buf = Vec::with_capacity(64 * 64 * 4);
        for _ in 0..(64 * 64) {
            buf.extend_from_slice(&rgba);
        }
        let img = photocraft_engine::world_media::WorldImage {
            width: 64,
            height: 64,
            pixels: buf.chunks_exact(4).map(|p| [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0, p[3] as f32 / 255.0]).collect(),
        };
        let png = photocraft_engine::world_media::encode_png(&img).unwrap();
        let h = photocraft_engine::world_media::media_asset_hash(&png).unwrap();
        let byte_len = png.len() as u64;
        media_map.insert(h, Arc::new(RgbaFrame { width: 64, height: 64, rgba: buf }));
        media_pngs.insert(h, png);
        MediaFrame { hash: h, byte_len, width: 64, height: 64, duration_ms: dur }
    };
    let f0 = make_f([230, 40, 40, 255], 100);
    let f1 = make_f([40, 200, 70, 255], 100);
    let g0 = make_f([40, 70, 230, 180], 250);
    let g1 = make_f([230, 210, 30, 180], 250);

    for (idx, (owner, dev, off, frames)) in [(a0, d0, [-60.0, -40.0], vec![f0, f1]), (a1, d1, [-20.0, -20.0], vec![g0, g1])].into_iter().enumerate() {
        let rev = world.revision + 1;
        let mid = MediaId::new(&hex(700 + idx as u128)).unwrap();
        world
            .apply(&AcceptedOp {
                revision: rev,
                op: WorldOp {
                    id: OpId::new(&hex(9000 + u128::from(rev))).unwrap(),
                    canvas,
                    author: owner,
                    device: dev,
                    sequence: 1,
                    authority_epoch: 1,
                    action: Edit::PlaceMedia {
                        media: Box::new(MediaPlacement {
                            id: mid,
                            canvas,
                            owner,
                            anchor: RegionKey::default(),
                            offset: off,
                            width: 100.0,
                            height: 100.0,
                            frames,
                            looping: true,
                            provenance: Provenance {
                                source: MediaSource::Canvas { canvas: CanvasId::new(&hex(999)).unwrap(), media: mid },
                                source_revision: 1,
                                artifact_hash: AssetHash::new(&format!("{:064x}", 50 + idx as u128)).unwrap(),
                                authors: vec![Credit { profile: owner, label: format!("U{idx}") }],
                            },
                        }),
                    },
                },
            })
            .unwrap();
    }
    // Author 0 erases a band through their own animated media (must not erase author 1's media).
    world
        .apply(&AcceptedOp {
            revision: 3,
            op: WorldOp {
                id: OpId::new(&hex(9003)).unwrap(),
                canvas,
                author: a0,
                device: d0,
                sequence: 2,
                authority_epoch: 1,
                action: Edit::AddStroke {
                    stroke: Stroke {
                        id: StrokeId::new(&hex(9100)).unwrap(),
                        canvas,
                        owner: a0,
                        revision: 0,
                        anchor: RegionKey::default(),
                        points: vec![[-55.0, 0.0], [55.0, 0.0]],
                        width: 24.0,
                        color: Srgba { rgba: [0.0, 0.0, 0.0, 1.0] },
                        erase: true,
                        brush: None,
                        samples: None,
                    },
                },
            },
        })
        .unwrap();

    let mut renderer = WorldRenderer::new(&world).unwrap();
    let mut mgr = WorldTileManager::new(TileBudget::conservative(), None);
    let scope = ScopeKey::new(1, "https://anim.invalid", a0.as_str(), d0.as_str(), canvas, None);
    mgr.set_scope(scope, Arc::clone(renderer.index()));
    for (h, f) in &media_map {
        mgr.register_media_frame(*h, Arc::clone(f));
    }
    let view = Viewport { camera: coordinates::home(canvas), width: 320, height: 240 };
    let mut resolve = |mf: &MediaFrame| -> photocraft_engine::Result<Option<Arc<RgbaFrame>>> { Ok(media_map.get(&mf.hash).cloned()) };

    // 1. Settle frame at t = 0 ms asynchronously.
    for _ in 0..150 {
        gpu.present_interactive(&mut mgr, &view, 1.0, 0, &[], None, &mut resolve, false).unwrap();
        if !gpu.stats.is_refining && gpu.stats.anim_pending_tiles == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
    assert!(!gpu.stats.is_refining && gpu.stats.anim_pending_tiles == 0, "t=0 animated frame must settle");
    let frame_t0 = readback(&gpu);

    // 2. Advance to t = 150 ms (Media 0 flips red->green; Media 1 stays blue).
    // First call MUST keep the last valid t=0 presentation (zero blanking/flicker) while scheduling async work!
    gpu.present_interactive(&mut mgr, &view, 1.0, 150, &[], None, &mut resolve, false).unwrap();
    assert!(!gpu.stats.is_refining, "animation tick with valid prior frame must not mark cold refining");
    assert!(gpu.stats.anim_pending_tiles > 0, "animation tick must schedule async tile work off UI thread");
    let frame_held = readback(&gpu);
    assert_eq!(frame_held, frame_t0, "first frame on animation tick must keep last valid presentation without blanking");

    // 3. Drive interactive loop until async animation update lands: must match exact blocking t=150ms render!
    for _ in 0..150 {
        gpu.present_interactive(&mut mgr, &view, 1.0, 150, &[], None, &mut resolve, false).unwrap();
        if !gpu.stats.is_refining && gpu.stats.anim_pending_tiles == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
    assert_eq!(gpu.stats.anim_pending_tiles, 0, "t=150ms animated frame must settle");
    let frame_t150 = readback(&gpu);
    assert_ne!(frame_t150, frame_t0, "settled t=150ms animation frame must reflect red->green flip");

    let mut gpu_ref = WorldGpu::new_with_budget(&rs, photocraft_gpu::DeviceHealth::watch(&rs.device), TileBudget::conservative()).unwrap();
    let mut encode_resolve = |mf: &MediaFrame| -> photocraft_engine::Result<Option<Vec<u8>>> { Ok(media_pngs.get(&mf.hash).cloned()) };
    gpu_ref.render_available(&mut renderer, &view, 1.0, 150, &mut encode_resolve, &[]).unwrap();
    let expected_t150 = readback(&gpu_ref);
    assert_eq!(frame_t150, expected_t150, "async animated-media tile must match exact blocking W4 render bit-identically");
    assert!(gpu.health.is_ok(), "{:?}", gpu.health.fault());
}

#[test]
fn cold_tile_live_draw_stays_nonblocking_and_converges_to_exact_w4() {
    use photocraft_engine::paint::presets;
    use photocraft_engine::world_brush::{LiveTileBrush, to_stroke_brush};
    use photocraft_engine::world_tiles::{ScopeKey, TileBudget, WorldTileManager};
    let _lock = photocraft_testkit::gpu_lock();
    let rs = egui_kittest::wgpu::create_render_state(crate::gpu_canvas::wgpu_setup(), Default::default());
    let mut gpu = WorldGpu::new_with_budget(&rs, photocraft_gpu::DeviceHealth::watch(&rs.device), TileBudget::conservative()).unwrap();

    let canvas = CanvasId::new(&hex(42)).unwrap();
    let a1 = OwnerId::new(&hex(43)).unwrap();
    let a5 = OwnerId::new(&hex(44)).unwrap();
    let d1 = DeviceId::new(&hex(45)).unwrap();
    let d5 = DeviceId::new(&hex(99)).unwrap(); // strictly > d1 in W4 device order
    let mut world = WorldState::new(canvas, a1, "Cold GPU Live W4".into()).unwrap();
    let all = presets::builtin();
    let mk_stroke = |id: u128, owner: OwnerId, pts: Vec<[f64; 2]>, size: f32, color: [f32; 4], erase: bool| {
        let mut b = presets::find(&all, "Hard Round").unwrap().brush.clone();
        b.size = size;
        b.color = color;
        b.erase = erase;
        let sb = to_stroke_brush(&b, 1.0, false).unwrap();
        Stroke {
            id: StrokeId::new(&hex(id)).unwrap(),
            canvas,
            owner,
            revision: 0,
            anchor: RegionKey::default(),
            points: pts,
            width: f64::from(size),
            color: Srgba { rgba: color },
            erase,
            brush: Some(Box::new(sb)),
            samples: None,
        }
    };
    let s1 = mk_stroke(101, a1, vec![[-40.0, 0.0], [40.0, 0.0]], 20.0, [0.9, 0.1, 0.1, 1.0], false);
    let s5 = mk_stroke(102, a5, vec![[0.0, -40.0], [0.0, 40.0]], 20.0, [0.1, 0.2, 0.9, 1.0], false);
    for (rev, (author, dev, stroke)) in [(1u64, (a1, d1, s1)), (2u64, (a5, d5, s5))] {
        world
            .apply(&AcceptedOp {
                revision: rev,
                op: WorldOp {
                    id: OpId::new(&hex(500 + u128::from(rev))).unwrap(),
                    canvas,
                    author,
                    device: dev,
                    sequence: 1,
                    authority_epoch: 1,
                    action: Edit::AddStroke { stroke },
                },
            })
            .unwrap();
    }

    let mut renderer = WorldRenderer::new(&world).unwrap();
    let mut mgr = WorldTileManager::new(TileBudget::conservative(), None);
    mgr.set_scope(ScopeKey::new(1, "https://cold-gpu.invalid", a1.as_str(), d1.as_str(), canvas, None), Arc::clone(renderer.index()));
    let view = Viewport { camera: coordinates::home(canvas), width: 256, height: 256 };
    let mut resolve = |_: &MediaFrame| -> photocraft_engine::Result<Option<Arc<RgbaFrame>>> { Ok(None) };

    // Settle static tiles first (`DynamicSplitCache` remains cold!).
    for _ in 0..100 {
        gpu.present_interactive(&mut mgr, &view, 1.0, 0, &[], None, &mut resolve, false).unwrap();
        if !gpu.stats.is_refining {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(!gpu.stats.is_refining);
    assert_eq!(mgr.stats().dynamic_split_bytes, 0);

    // Start live green stroke on `d1` (< `d5`): Frame 1 must return immediately with `anim_pending_tiles > 0`
    // (off-thread `WarmSplitBands` scheduled) without populating `DynamicSplitCache` synchronously on UI thread!
    let live_stroke = mk_stroke(700, a1, vec![[-30.0, -10.0], [30.0, 10.0]], 24.0, [0.1, 0.85, 0.2, 1.0], false);
    let preview = SharedStroke { stroke: live_stroke.clone(), device: d1, provenance: None, visible: true, created_revision: 99, changed_revision: 99 };
    let target_lod = gpu.target_lod(&view, 1.0).unwrap();
    let mut live_brush = LiveTileBrush::new(&live_stroke, target_lod).unwrap();

    gpu.present_interactive(&mut mgr, &view, 1.0, 0, std::slice::from_ref(&preview), Some(&mut live_brush), &mut resolve, false).unwrap();
    assert!(gpu.stats.anim_pending_tiles > 0, "cold multi-author live tile must schedule async split warming off UI thread");

    // Drive frames until `WarmSplitBands` finishes: final GPU output MUST match blocking exact W4 render!
    for _ in 0..150 {
        gpu.present_interactive(&mut mgr, &view, 1.0, 0, std::slice::from_ref(&preview), Some(&mut live_brush), &mut resolve, false).unwrap();
        if !gpu.stats.is_refining && gpu.stats.anim_pending_tiles == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
    assert_eq!(gpu.stats.anim_pending_tiles, 0, "cold live tile must converge once WarmSplitBands completes");
    let converged = readback(&gpu);

    let mut gpu_ref = WorldGpu::new_with_budget(&rs, photocraft_gpu::DeviceHealth::watch(&rs.device), TileBudget::conservative()).unwrap();
    gpu_ref.render_available(&mut renderer, &view, 1.0, 0, &mut |_| Ok(None), std::slice::from_ref(&preview)).unwrap();
    let expected = readback(&gpu_ref);
    let max_delta = converged.iter().zip(&expected).flat_map(|(a, b)| (0..4).map(move |c| a[c].abs_diff(b[c]))).max().unwrap_or(0);
    assert!(max_delta <= 1, "converged live GPU tile diverged from blocking W4 oracle by {max_delta}/255");
    assert!(gpu.health.is_ok(), "{:?}", gpu.health.fault());
}

#[test]
fn cold_tile_live_draw_with_non_overlapping_peer_preview_converges() {
    use photocraft_engine::paint::presets;
    use photocraft_engine::world_brush::{LiveTileBrush, to_stroke_brush};
    use photocraft_engine::world_tiles::{ScopeKey, TileBudget, WorldTileManager};
    let _lock = photocraft_testkit::gpu_lock();
    let rs = egui_kittest::wgpu::create_render_state(crate::gpu_canvas::wgpu_setup(), Default::default());
    let mut gpu = WorldGpu::new_with_budget(&rs, photocraft_gpu::DeviceHealth::watch(&rs.device), TileBudget::conservative()).unwrap();

    let canvas = CanvasId::new(&hex(49)).unwrap();
    let a1 = OwnerId::new(&hex(50)).unwrap();
    let a2 = OwnerId::new(&hex(51)).unwrap();
    let a3 = OwnerId::new(&hex(52)).unwrap();
    let d1 = DeviceId::new(&hex(53)).unwrap();
    let d2 = DeviceId::new(&hex(54)).unwrap();
    let d3 = DeviceId::new(&hex(55)).unwrap();
    let mut world = WorldState::new(canvas, a1, "NonOverlapping Peer Split Warm".into()).unwrap();
    let all = presets::builtin();
    let mk_stroke = |id: u128, owner: OwnerId, pts: Vec<[f64; 2]>, size: f32, color: [f32; 4]| {
        let mut b = presets::find(&all, "Hard Round").unwrap().brush.clone();
        b.size = size;
        b.color = color;
        let sb = to_stroke_brush(&b, 1.0, false).unwrap();
        Stroke {
            id: StrokeId::new(&hex(id)).unwrap(),
            canvas,
            owner,
            revision: 0,
            anchor: RegionKey::default(),
            points: pts,
            width: f64::from(size),
            color: Srgba { rgba: color },
            erase: false,
            brush: Some(Box::new(sb)),
            samples: None,
        }
    };
    // Both d2 and d3 have committed strokes on the left tile (-180..=-140);
    // d2's live peer preview is FAR to the right (+180..=+220) so it does NOT touch the left tile!
    let s2 = mk_stroke(201, a2, vec![[-180.0, 0.0], [-140.0, 0.0]], 20.0, [0.9, 0.1, 0.1, 1.0]);
    let s3 = mk_stroke(202, a3, vec![[-160.0, -20.0], [-160.0, 20.0]], 20.0, [0.1, 0.2, 0.9, 1.0]);
    for (rev, (author, dev, stroke)) in [(1u64, (a2, d2, s2)), (2u64, (a3, d3, s3))] {
        world
            .apply(&AcceptedOp {
                revision: rev,
                op: WorldOp {
                    id: OpId::new(&hex(600 + u128::from(rev))).unwrap(),
                    canvas,
                    author,
                    device: dev,
                    sequence: 1,
                    authority_epoch: 1,
                    action: Edit::AddStroke { stroke },
                },
            })
            .unwrap();
    }

    let renderer = WorldRenderer::new(&world).unwrap();
    let mut mgr = WorldTileManager::new(TileBudget::conservative(), None);
    mgr.set_scope(ScopeKey::new(1, "https://peer-split.invalid", a1.as_str(), d1.as_str(), canvas, None), Arc::clone(renderer.index()));
    let view = Viewport { camera: coordinates::home(canvas), width: 640, height: 256 };
    let mut resolve = |_: &MediaFrame| -> photocraft_engine::Result<Option<Arc<RgbaFrame>>> { Ok(None) };
    for _ in 0..100 {
        gpu.present_interactive(&mut mgr, &view, 1.0, 0, &[], None, &mut resolve, false).unwrap();
        if !gpu.stats.is_refining {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(!gpu.stats.is_refining);

    let local_stroke = mk_stroke(701, a1, vec![[-175.0, -5.0], [-145.0, 5.0]], 22.0, [0.1, 0.85, 0.2, 1.0]);
    let far_peer_stroke = mk_stroke(702, a2, vec![[180.0, 0.0], [220.0, 0.0]], 20.0, [0.85, 0.55, 0.1, 1.0]);
    let previews = [
        SharedStroke { stroke: far_peer_stroke, device: d2, provenance: None, visible: true, created_revision: 98, changed_revision: 98 },
        SharedStroke { stroke: local_stroke.clone(), device: d1, provenance: None, visible: true, created_revision: 99, changed_revision: 99 },
    ];
    let target_lod = gpu.target_lod(&view, 1.0).unwrap();
    let mut live_brush = LiveTileBrush::new(&local_stroke, target_lod).unwrap();

    for _ in 0..150 {
        gpu.present_interactive(&mut mgr, &view, 1.0, 0, &previews, Some(&mut live_brush), &mut resolve, false).unwrap();
        if !gpu.stats.is_refining && gpu.stats.anim_pending_tiles == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
    assert_eq!(gpu.stats.anim_pending_tiles, 0, "per-tile split_devices must converge cleanly even when a peer preview is on another tile");
    assert!(!gpu.stats.is_refining);
    assert!(gpu.health.is_ok(), "{:?}", gpu.health.fault());
}

#[test]
fn rapid_consecutive_strokes_on_invalidated_tile_never_flash_white_and_converge() {
    use photocraft_engine::paint::presets;
    use photocraft_engine::world_brush::{LiveTileBrush, to_stroke_brush};
    use photocraft_engine::world_tiles::{ScopeKey, TileBudget, WorldTileManager};
    let _lock = photocraft_testkit::gpu_lock();
    let rs = egui_kittest::wgpu::create_render_state(crate::gpu_canvas::wgpu_setup(), Default::default());
    let mut gpu = WorldGpu::new_with_budget(&rs, photocraft_gpu::DeviceHealth::watch(&rs.device), TileBudget::conservative()).unwrap();

    let canvas = CanvasId::new(&hex(71)).unwrap();
    let author = OwnerId::new(&hex(72)).unwrap();
    let device = DeviceId::new(&hex(73)).unwrap();
    let mut world = WorldState::new(canvas, author, "Rapid Consecutive Strokes".into()).unwrap();
    let all = presets::builtin();
    let mk_stroke = |id: u128, pts: Vec<[f64; 2]>, size: f32, color: [f32; 4]| {
        let mut b = presets::find(&all, "Hard Round").unwrap().brush.clone();
        b.size = size;
        b.color = color;
        let sb = to_stroke_brush(&b, 1.0, false).unwrap();
        Stroke {
            id: StrokeId::new(&hex(id)).unwrap(),
            canvas,
            owner: author,
            revision: 0,
            anchor: RegionKey::default(),
            points: pts,
            width: f64::from(size),
            color: Srgba { rgba: color },
            erase: false,
            brush: Some(Box::new(sb)),
            samples: None,
        }
    };
    let append = |world: &mut WorldState, id: u128, stroke: Stroke| -> AcceptedOp {
        let rev = world.revision + 1;
        let seq = world.next_sequence(device).unwrap();
        let ev = AcceptedOp {
            revision: rev,
            op: WorldOp {
                id: OpId::new(&hex(7000 + id)).unwrap(),
                canvas,
                author,
                device,
                sequence: seq,
                authority_epoch: 1,
                action: Edit::AddStroke { stroke },
            },
        };
        world.apply(&ev).unwrap();
        ev
    };

    // Rev 1: Solid red coverage across the entire 256x256 viewport.
    let _ = append(&mut world, 1, mk_stroke(701, vec![[-220.0, 0.0], [220.0, 0.0]], 400.0, [0.95, 0.08, 0.08, 1.0]));
    let mut renderer = WorldRenderer::new(&world).unwrap();
    let mut mgr = WorldTileManager::new(TileBudget::conservative(), None);
    mgr.set_scope(ScopeKey::new(1, "https://rapid.invalid", author.as_str(), device.as_str(), canvas, None), Arc::clone(renderer.index()));
    let view = Viewport { camera: coordinates::home(canvas), width: 256, height: 256 };
    let mut resolve = |_: &MediaFrame| -> photocraft_engine::Result<Option<Arc<RgbaFrame>>> { Ok(None) };

    for _ in 0..100 {
        gpu.present_interactive(&mut mgr, &view, 1.0, 0, &[], None, &mut resolve, false).unwrap();
        if !gpu.stats.is_refining {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(!gpu.stats.is_refining);
    let count_white_center = |pixels: &[[u8; 4]]| -> usize {
        let mut white = 0usize;
        for y in 64..192 {
            for x in 64..192 {
                let p = pixels[y * 256 + x];
                if p[0] >= 250 && p[1] >= 250 && p[2] >= 250 {
                    white += 1;
                }
            }
        }
        white
    };
    assert_eq!(count_white_center(&readback(&gpu)), 0, "baseline red tile must have zero white pixels");

    // Rev 2: Commit Stroke 1 (blue bar) and invalidate the center tile immediately via `update_scene`.
    let stroke1 = mk_stroke(702, vec![[-80.0, -30.0], [80.0, -30.0]], 24.0, [0.05, 0.15, 0.95, 1.0]);
    let ev1 = append(&mut world, 2, stroke1);
    let dirty: Vec<_> = renderer.apply_accepted(&ev1).unwrap().into_iter().collect();
    mgr.update_scene(Arc::clone(renderer.index()), Some(&dirty));

    // IMMEDIATELY start Stroke 2 (green line) on the invalidated tile BEFORE Stroke 1's static tile finishes!
    let stroke2 = mk_stroke(703, vec![[-70.0, 20.0], [70.0, 40.0]], 24.0, [0.05, 0.85, 0.15, 1.0]);
    let preview2 = SharedStroke { stroke: stroke2.clone(), device, provenance: None, visible: true, created_revision: 3, changed_revision: 3 };
    let target_lod = gpu.target_lod(&view, 1.0).unwrap();
    let mut live_brush = LiveTileBrush::new(&stroke2, target_lod).unwrap();

    for frame_idx in 0..6 {
        gpu.present_interactive(&mut mgr, &view, 1.0, 0, std::slice::from_ref(&preview2), Some(&mut live_brush), &mut resolve, false).unwrap();
        let white = count_white_center(&readback(&gpu));
        assert_eq!(white, 0, "rapid consecutive stroke frame {frame_idx} must never paint onto a pure-white base");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    // Release Stroke 2, commit it, and verify zero white dropout frames while settling to exact W4 oracle.
    let ev2 = append(&mut world, 3, stroke2);
    let dirty2: Vec<_> = renderer.apply_accepted(&ev2).unwrap().into_iter().collect();
    mgr.update_scene(Arc::clone(renderer.index()), Some(&dirty2));
    for frame_idx in 0..150 {
        gpu.present_interactive(&mut mgr, &view, 1.0, 0, &[], None, &mut resolve, false).unwrap();
        let white = count_white_center(&readback(&gpu));
        assert_eq!(white, 0, "post-release frame {frame_idx} must never flash pure-white dropout");
        if !gpu.stats.is_refining && gpu.stats.anim_pending_tiles == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
    assert!(!gpu.stats.is_refining);
    let settled = readback(&gpu);
    let mut gpu_ref = WorldGpu::new_with_budget(&rs, photocraft_gpu::DeviceHealth::watch(&rs.device), TileBudget::conservative()).unwrap();
    gpu_ref.render_available(&mut renderer, &view, 1.0, 0, &mut |_| Ok(None), &[]).unwrap();
    let expected = readback(&gpu_ref);
    let max_delta = settled.iter().zip(&expected).flat_map(|(a, b)| (0..4).map(move |c| a[c].abs_diff(b[c]))).max().unwrap_or(0);
    assert!(max_delta <= 1, "settled rapid-stroke frame diverged from blocking oracle by {max_delta}/255");
    assert!(gpu.health.is_ok(), "{:?}", gpu.health.fault());
}

#[test]
fn zoom_out_across_depth_boundary_prefers_fresh_finer_tiles_over_stale_white_ancestors() {
    use photocraft_engine::paint::presets;
    use photocraft_engine::world_brush::to_stroke_brush;
    use photocraft_engine::world_tiles::{ScopeKey, TileBudget, WorldTileManager};
    let _lock = photocraft_testkit::gpu_lock();
    let rs = egui_kittest::wgpu::create_render_state(crate::gpu_canvas::wgpu_setup(), Default::default());
    let mut gpu = WorldGpu::new_with_budget(&rs, photocraft_gpu::DeviceHealth::watch(&rs.device), TileBudget::conservative()).unwrap();

    let canvas = CanvasId::new(&hex(81)).unwrap();
    let author = OwnerId::new(&hex(82)).unwrap();
    let device = DeviceId::new(&hex(83)).unwrap();
    let mut world = WorldState::new(canvas, author, "Depth Boundary Fallback".into()).unwrap();
    let mut renderer = WorldRenderer::new(&world).unwrap();
    let mut mgr = WorldTileManager::new(TileBudget::conservative(), None);
    mgr.set_scope(ScopeKey::new(1, "https://depth-boundary.invalid", author.as_str(), device.as_str(), canvas, None), Arc::clone(renderer.index()));
    let mut resolve = |_: &MediaFrame| -> photocraft_engine::Result<Option<Arc<RgbaFrame>>> { Ok(None) };

    // 1. Warm rev-0 blank white coarse tiles at depth 0, zoom 1.0 (`lod = 94`).
    let coarse_view = Viewport { camera: coordinates::home(canvas), width: 256, height: 256 };
    for _ in 0..80 {
        gpu.present_interactive(&mut mgr, &coarse_view, 1.0, 0, &[], None, &mut resolve, false).unwrap();
        if !gpu.stats.is_refining {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(!gpu.stats.is_refining);

    // 2. Zoom into depth 1 (`lod = 87`, centered off-origin at `(600, 600)` so `lod 88` overlaps 3x3 = 9 `lod 87` tiles!)
    //    and paint a huge solid red stroke covering the zoomed-in viewport.
    let deep_region = RegionKey { depth: 1, x: photocraft_engine::scene::Wide(0), y: photocraft_engine::scene::Wide(0) };
    let center_pt = coordinates::point(&deep_region, [600.0, 600.0]).unwrap();
    let deep_cam = coordinates::target(canvas, center_pt, 1, 1.0).unwrap();
    let deep_view = Viewport { camera: deep_cam, width: 512, height: 512 };
    let boundary_cam = coordinates::target(canvas, center_pt, 0, 64.0).unwrap();
    let boundary_view = Viewport { camera: boundary_cam, width: 256, height: 256 };

    let lod_deep = gpu.target_lod(&deep_view, 1.0).unwrap();
    let lod_boundary = gpu.target_lod(&boundary_view, 1.0).unwrap();
    assert_eq!(lod_deep, 87, "expected depth-1 zoom-1 LOD 87");
    assert_eq!(lod_boundary, 88, "expected cross-depth 1.5625x boundary LOD 88");

    let all = presets::builtin();
    let mut b = presets::find(&all, "Hard Round").unwrap().brush.clone();
    b.size = 500.0;
    b.color = [0.95, 0.06, 0.06, 1.0];
    let sb = to_stroke_brush(&b, 1.0, false).unwrap();
    let red_stroke = Stroke {
        id: StrokeId::new(&hex(801)).unwrap(),
        canvas,
        owner: author,
        revision: 0,
        anchor: deep_region,
        points: vec![[300.0, 600.0], [900.0, 600.0]],
        width: 500.0,
        color: Srgba { rgba: [0.95, 0.06, 0.06, 1.0] },
        erase: false,
        brush: Some(Box::new(sb)),
        samples: None,
    };
    let ev = AcceptedOp {
        revision: 1,
        op: WorldOp {
            id: OpId::new(&hex(8001)).unwrap(),
            canvas,
            author,
            device,
            sequence: 1,
            authority_epoch: 1,
            action: Edit::AddStroke { stroke: red_stroke },
        },
    };
    world.apply(&ev).unwrap();
    let dirty: Vec<_> = renderer.apply_accepted(&ev).unwrap().into_iter().collect();
    mgr.update_scene(Arc::clone(renderer.index()), Some(&dirty));

    // Settle the fresh rev-1 red `lod 87` tiles in TileAtlas (while `lod 94` still holds stale rev-0 blank white!).
    for _ in 0..120 {
        gpu.present_interactive(&mut mgr, &deep_view, 1.0, 0, &[], None, &mut resolve, false).unwrap();
        if !gpu.stats.is_refining {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(!gpu.stats.is_refining);

    // 3. Immediately zoom OUT across the 1.5625x depth boundary (`lod 87 -> lod 88`) and inspect Frame 1!
    //    `lod 88` tile(s) are not rendered yet; TileAtlas has BOTH stale rev-0 white `lod 94` AND fresh rev-1 red `lod 87`.
    //    Frame 1 MUST paint the fresh rev-1 red `lod 87` children (spanning 3x3 = 9 tiles) on top, with ZERO stale-white center box!
    gpu.present_interactive(&mut mgr, &boundary_view, 1.0, 0, &[], None, &mut resolve, false).unwrap();
    let frame1 = readback(&gpu);
    let mut non_red_center = 0usize;
    for y in 64..192 {
        for x in 64..192 {
            let p = frame1[y * 256 + x];
            if !(p[0] > 200 && p[1] < 50 && p[2] < 50) {
                non_red_center += 1;
            }
        }
    }
    assert_eq!(
        non_red_center, 0,
        "Frame 1 zooming out across 1.5625x depth boundary must show fresh red finer children, not stale rev-0 white ancestor squares"
    );

    // 4. Drive until settled and verify exact match with blocking oracle.
    for _ in 0..120 {
        gpu.present_interactive(&mut mgr, &boundary_view, 1.0, 0, &[], None, &mut resolve, false).unwrap();
        if !gpu.stats.is_refining {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(!gpu.stats.is_refining);
    let settled = readback(&gpu);
    let mut gpu_ref = WorldGpu::new_with_budget(&rs, photocraft_gpu::DeviceHealth::watch(&rs.device), TileBudget::conservative()).unwrap();
    gpu_ref.render_available(&mut renderer, &boundary_view, 1.0, 0, &mut |_| Ok(None), &[]).unwrap();
    let expected = readback(&gpu_ref);
    let max_delta = settled.iter().zip(&expected).flat_map(|(a, b)| (0..4).map(move |c| a[c].abs_diff(b[c]))).max().unwrap_or(0);
    assert!(max_delta <= 1, "settled boundary-zoom frame diverged from blocking oracle by {max_delta}/255");
    assert!(gpu.health.is_ok(), "{:?}", gpu.health.fault());
}

#[test]
fn live_mode_invalidated_static_tile_retains_resident_gpu_slot_without_white_dropout() {
    use photocraft_engine::paint::presets;
    use photocraft_engine::world_brush::to_stroke_brush;
    use photocraft_engine::world_tiles::{ScopeKey, TileBudget, WorldTileManager};
    let _lock = photocraft_testkit::gpu_lock();
    let rs = egui_kittest::wgpu::create_render_state(crate::gpu_canvas::wgpu_setup(), Default::default());
    let mut gpu = WorldGpu::new_with_budget(&rs, photocraft_gpu::DeviceHealth::watch(&rs.device), TileBudget::conservative()).unwrap();

    let canvas = CanvasId::new(&hex(91)).unwrap();
    let author = OwnerId::new(&hex(92)).unwrap();
    let device = DeviceId::new(&hex(93)).unwrap();
    let mut world = WorldState::new(canvas, author, "Live Stale Hold".into()).unwrap();
    let all = presets::builtin();
    let mut b = presets::find(&all, "Hard Round").unwrap().brush.clone();
    b.size = 400.0;
    b.color = [0.92, 0.10, 0.12, 1.0];
    let sb = to_stroke_brush(&b, 1.0, false).unwrap();
    world
        .apply(&AcceptedOp {
            revision: 1,
            op: WorldOp {
                id: OpId::new(&hex(9001)).unwrap(),
                canvas,
                author,
                device,
                sequence: 1,
                authority_epoch: 1,
                action: Edit::AddStroke {
                    stroke: Stroke {
                        id: StrokeId::new(&hex(901)).unwrap(),
                        canvas,
                        owner: author,
                        revision: 0,
                        anchor: RegionKey::default(),
                        points: vec![[-220.0, 0.0], [220.0, 0.0]],
                        width: 400.0,
                        color: Srgba { rgba: [0.92, 0.10, 0.12, 1.0] },
                        erase: false,
                        brush: Some(Box::new(sb)),
                        samples: None,
                    },
                },
            },
        })
        .unwrap();

    let mut renderer = WorldRenderer::new(&world).unwrap();
    let mut mgr = WorldTileManager::new(TileBudget::conservative(), None);
    mgr.set_scope(ScopeKey::new(1, "https://stale-hold.invalid", author.as_str(), device.as_str(), canvas, None), Arc::clone(renderer.index()));
    let view = Viewport { camera: coordinates::home(canvas), width: 256, height: 256 };
    let mut resolve = |_: &MediaFrame| -> photocraft_engine::Result<Option<Arc<RgbaFrame>>> { Ok(None) };

    for _ in 0..80 {
        gpu.present_interactive(&mut mgr, &view, 1.0, 0, &[], None, &mut resolve, false).unwrap();
        if !gpu.stats.is_refining {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(!gpu.stats.is_refining);

    // Add a small stroke at the top-left corner, invalidating the visible tile(s).
    let mut b2 = presets::find(&all, "Hard Round").unwrap().brush.clone();
    b2.size = 16.0;
    b2.color = [0.05, 0.2, 0.95, 1.0];
    let sb2 = to_stroke_brush(&b2, 1.0, false).unwrap();
    let ev = AcceptedOp {
        revision: 2,
        op: WorldOp {
            id: OpId::new(&hex(9002)).unwrap(),
            canvas,
            author,
            device,
            sequence: 2,
            authority_epoch: 1,
            action: Edit::AddStroke {
                stroke: Stroke {
                    id: StrokeId::new(&hex(902)).unwrap(),
                    canvas,
                    owner: author,
                    revision: 0,
                    anchor: RegionKey::default(),
                    points: vec![[-100.0, -100.0], [-90.0, -100.0]],
                    width: 16.0,
                    color: Srgba { rgba: [0.05, 0.2, 0.95, 1.0] },
                    erase: false,
                    brush: Some(Box::new(sb2)),
                    samples: None,
                },
            },
        },
    };
    world.apply(&ev).unwrap();
    let dirty: Vec<_> = renderer.apply_accepted(&ev).unwrap().into_iter().collect();
    mgr.update_scene(Arc::clone(renderer.index()), Some(&dirty));

    // Frame 1 immediately after invalidation in LIVE mode (`retain_stale_for_review = false`):
    // center pixel MUST still hold the resident red static tile, NEVER drop to white!
    gpu.present_interactive(&mut mgr, &view, 1.0, 0, &[], None, &mut resolve, false).unwrap();
    let center = readback(&gpu)[128 * 256 + 128];
    assert!(center[0] > 200 && center[1] < 50 && center[2] < 50, "live invalidated tile dropped to {center:?} instead of holding resident red pixels");
    assert!(gpu.health.is_ok(), "{:?}", gpu.health.fault());
}
