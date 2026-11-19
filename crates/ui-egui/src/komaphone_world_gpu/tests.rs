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
