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
