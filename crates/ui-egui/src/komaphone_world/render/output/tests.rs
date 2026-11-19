use super::{OutputCache, OutputKey};
use crate::komaphone_world::render::{Key, Presentation, Revision, Scope};
use crate::komaphone_world_gpu::{Stats, WorldGpu, tests::readback};
use photocraft_engine::{
    scene::{
        CanvasId, OwnerId, Srgba, Stroke, StrokeId, Viewport, coordinates,
        shared::{
            AcceptedOp, DeviceId, Edit, OpId, WorldOp, WorldState,
            media::{AssetHash, MediaFrame},
        },
    },
    world_media::WorldRenderer,
    worlds::Account,
};
use std::cell::Cell;

fn world() -> WorldState {
    let canvas = CanvasId::new(&format!("{:032x}", 1)).unwrap();
    let owner = OwnerId::new(&format!("{:032x}", 2)).unwrap();
    let mut world = WorldState::new(canvas, owner, "Cached GPU output".into()).unwrap();
    for revision in 1..=2 {
        let color = if revision == 1 { [1., 0., 0., 1.] } else { [0., 0., 1., 1.] };
        world
            .apply(&AcceptedOp {
                revision,
                op: WorldOp {
                    id: OpId::new(&format!("{:032x}", revision + 10)).unwrap(),
                    canvas,
                    author: owner,
                    device: DeviceId::new(&format!("{:032x}", 3)).unwrap(),
                    sequence: revision,
                    authority_epoch: 1,
                    action: Edit::AddStroke {
                        stroke: Stroke {
                            id: StrokeId::new(&format!("{:032x}", revision + 20)).unwrap(),
                            canvas,
                            owner,
                            revision: 0,
                            anchor: Default::default(),
                            points: if revision == 1 { vec![[-12., 0.], [12., 0.]] } else { vec![[0., 0.]] },
                            width: 8.,
                            color: Srgba { rgba: color },
                            erase: false,
                            brush: None,
                            samples: None,
                        },
                    },
                },
            })
            .unwrap();
    }
    world
}
fn key(world: &WorldState) -> OutputKey {
    OutputKey::new(
        Key {
            scope: Scope {
                activation: 1,
                canvas: world.canvas,
                account: Account { endpoint: "https://synthetic.invalid".into(), profile: world.owner, device: DeviceId::new(&format!("{:032x}", 3)).unwrap() },
            },
            revision: Revision::Live { canonical: world.revision, display: world.revision, pending: 0 },
        },
        Viewport { camera: coordinates::home(world.canvas), width: 64, height: 64 },
        1.,
        Vec::new(),
    )
}
fn fake_draw(calls: &Cell<u64>, pending_assets: usize) -> (egui::TextureId, Stats) {
    calls.set(calls.get() + 1);
    (egui::TextureId::User(calls.get()), Stats { pending_assets, ..Default::default() })
}

#[test]
fn exact_output_reuses_only_matching_scope_view_density_and_animation_frame() {
    let base = key(&world());
    let calls = Cell::new(0);
    let mut cache = OutputCache::default();
    let (first, _) = cache.render(base.clone(), true, || Ok(fake_draw(&calls, 0))).unwrap();
    let (reused, _) = cache.render(base.clone(), true, || panic!("Unchanged output must not draw")).unwrap();
    assert_eq!(first, reused);
    let mut changes = Vec::new();
    let mut add = |change: fn(&mut OutputKey)| {
        let mut changed = base.clone();
        change(&mut changed);
        changes.push(changed);
    };
    add(|key| key.view.camera.zoom = 2.);
    add(|key| key.view.width += 1);
    add(|key| key.density = 2f32.to_bits());
    add(|key| key.ready.scope.activation += 1);
    add(|key| key.ready.scope.account.endpoint = "https://another.invalid".into());
    add(|key| key.ready.scope.account.profile = OwnerId::new(&format!("{:032x}", 99)).unwrap());
    add(|key| key.ready.revision = Revision::Live { canonical: 2, display: 3, pending: 1 });
    add(|key| key.ready.revision = Revision::Review { end: 2, position: 1 });
    add(|key| {
        key.frames.push(MediaFrame { hash: AssetHash::new(&"a".repeat(64)).unwrap(), byte_len: 100, width: 1, height: 1, duration_ms: 100 });
    });
    for changed in changes {
        cache.render(base.clone(), true, || Ok(fake_draw(&calls, 0))).unwrap();
        let before = calls.get();
        cache.render(changed.clone(), true, || Ok(fake_draw(&calls, 0))).unwrap();
        assert_eq!(calls.get(), before + 1, "Changed source/view must draw");
        cache.render(changed, true, || panic!("Exact changed output should now be reusable")).unwrap();
    }
    let mut animated = base;
    animated.frames.push(MediaFrame { hash: AssetHash::new(&"b".repeat(64)).unwrap(), byte_len: 100, width: 1, height: 1, duration_ms: 100 });
    cache.render(animated.clone(), true, || Ok(fake_draw(&calls, 0))).unwrap();
    animated.frames[0].hash = AssetHash::new(&"c".repeat(64)).unwrap();
    let before = calls.get();
    cache.render(animated, true, || Ok(fake_draw(&calls, 0))).unwrap();
    assert_eq!(calls.get(), before + 1);
}

#[test]
fn previews_missing_assets_errors_and_gpu_recreation_never_reuse_dirty_output() {
    let key = key(&world());
    let calls = Cell::new(0);
    let mut cache = OutputCache::default();
    cache.render(key.clone(), true, || Ok(fake_draw(&calls, 0))).unwrap();
    cache.render(key.clone(), false, || Ok(fake_draw(&calls, 0))).unwrap();
    // Preview disappearance must redraw the retained scene, not reuse the
    // output texture overwritten by the preview's previous frame.
    cache.render(key.clone(), true, || Ok(fake_draw(&calls, 0))).unwrap();
    assert_eq!(calls.get(), 3);
    cache.clear(); // GPU/source recreation invalidates the texture identity.
    cache.render(key.clone(), true, || Ok(fake_draw(&calls, 1))).unwrap();
    cache.render(key.clone(), true, || Ok(fake_draw(&calls, 0))).unwrap();
    cache.render(key.clone(), true, || panic!("Available asset output should reuse")).unwrap();
    assert_eq!(calls.get(), 5);
    assert!(cache.render(key.clone(), false, || Err("Interrupted GPU render".into())).is_err());
    cache.render(key, true, || Ok(fake_draw(&calls, 0))).unwrap();
    assert_eq!(calls.get(), 6);
}

fn draw(p: &mut Presentation, key: OutputKey, calls: &Cell<u64>) -> egui::TextureId {
    let gpu = p.gpu.as_mut().unwrap();
    let renderer = p.renderer.as_mut().unwrap();
    let view = key.view.clone();
    let density = f32::from_bits(key.density);
    p.output
        .render(key, true, || {
            calls.set(calls.get() + 1);
            let texture = gpu.render(renderer, &view, density, 0, &mut |_| panic!("No media fixture")).unwrap();
            Ok((texture, gpu.stats.clone()))
        })
        .unwrap()
        .0
}

#[test]
fn exact_output_gpu_pixels_survive_hits_and_change_for_view_revision_and_review() {
    let _lock = photocraft_testkit::gpu_lock();
    let rs = egui_kittest::wgpu::create_render_state(crate::gpu_canvas::wgpu_setup(), Default::default());
    let mut p = Presentation { gpu: Some(Box::new(WorldGpu::new(&rs, photocraft_gpu::DeviceHealth::watch(&rs.device)).unwrap())), ..Default::default() };
    let complete = world();
    let earlier = complete.replay_to(1).unwrap();
    let original = key(&earlier);
    let calls = Cell::new(0);
    p.install(WorldRenderer::new(&earlier).unwrap(), original.ready.clone());
    let texture = draw(&mut p, original.clone(), &calls);
    let pixels = readback(p.gpu.as_ref().unwrap());
    assert_eq!(pixels[32 * 64 + 32], [255, 0, 0, 255]);
    assert_eq!(draw(&mut p, original.clone(), &calls), texture);
    assert_eq!(calls.get(), 1, "Cache hit must skip GPU drawing");
    assert_eq!(readback(p.gpu.as_ref().unwrap()), pixels);

    let mut zoomed = original.clone();
    zoomed.view.camera.zoom = 2.;
    draw(&mut p, zoomed, &calls);
    assert_eq!(calls.get(), 2);
    assert_ne!(readback(p.gpu.as_ref().unwrap()), pixels);

    let latest = key(&complete);
    p.install(WorldRenderer::new(&complete).unwrap(), latest.ready.clone());
    draw(&mut p, latest.clone(), &calls);
    assert_eq!(readback(p.gpu.as_ref().unwrap())[32 * 64 + 32], [0, 0, 255, 255]);

    let mut review = original.clone();
    review.ready.revision = Revision::Review { end: 2, position: 1 };
    p.install(WorldRenderer::new(&earlier).unwrap(), review.ready.clone());
    draw(&mut p, review.clone(), &calls);
    assert_eq!(readback(p.gpu.as_ref().unwrap()), pixels);
    draw(&mut p, review, &calls);
    assert_eq!(calls.get(), 4);

    // Even an equal key cannot authorize a texture from a replaced renderer.
    p.install(WorldRenderer::new(&complete).unwrap(), latest.ready.clone());
    draw(&mut p, latest.clone(), &calls);
    // Use a validated different source with the same revision: rename replaces
    // the second drawing entry, so the older image must not be inherited.
    let mut divergent = earlier.clone();
    divergent
        .apply(&AcceptedOp {
            revision: 2,
            op: WorldOp {
                id: OpId::new(&format!("{:032x}", 90)).unwrap(),
                canvas: divergent.canvas,
                author: divergent.owner,
                device: DeviceId::new(&format!("{:032x}", 3)).unwrap(),
                sequence: 2,
                authority_epoch: 1,
                action: Edit::Rename { name: "Different optimistic source".into(), expected_revision: 0 },
            },
        })
        .unwrap();
    assert_eq!(divergent.revision, complete.revision);
    p.install(WorldRenderer::new(&divergent).unwrap(), latest.ready.clone());
    draw(&mut p, latest, &calls);
    assert_eq!(calls.get(), 6);
    assert_eq!(readback(p.gpu.as_ref().unwrap()), pixels);
}
