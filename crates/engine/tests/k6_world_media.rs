#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;
use std::io::Cursor;

use image::AnimationDecoder;
use photocraft_engine::{Result, world_media::*};
use photocraft_scene::shared::media::{Credit, MediaFrame, MediaPlacement, MediaSource, Provenance};
use photocraft_scene::shared::tours::{ExportSettings, FrozenTour};
use photocraft_scene::shared::{AcceptedOp, DeviceId, Edit, MediaId, OpId, SourceId, Tour, TourId, TourStop, WorldState};
use photocraft_scene::{CanvasId, OwnerId, RegionKey, Srgba, Stroke, StrokeId, Viewport, coordinates};

fn hex(n: u128) -> String {
    format!("{n:032x}")
}
fn canvas() -> CanvasId {
    CanvasId::new(&hex(1)).unwrap()
}
fn owner(n: u128) -> OwnerId {
    OwnerId::new(&hex(n + 100)).unwrap()
}
fn device(n: u128) -> DeviceId {
    DeviceId::new(&hex(n + 200)).unwrap()
}
fn world() -> WorldState {
    WorldState::new(canvas(), owner(0), "Synthetic world".into()).unwrap()
}
fn view(depth: u8) -> Viewport {
    Viewport { camera: coordinates::target(canvas(), Default::default(), depth, 1.0).unwrap(), width: 64, height: 64 }
}
fn append(world: &mut WorldState, writer: u128, action: Edit) {
    let revision = world.revision + 1;
    world
        .apply(&AcceptedOp {
            revision,
            op: photocraft_scene::shared::WorldOp {
                id: OpId::new(&hex(1000 + u128::from(revision))).unwrap(),
                canvas: canvas(),
                author: owner(writer),
                device: device(writer),
                sequence: world.next_sequence(device(writer)).unwrap(),
                authority_epoch: 1,
                action,
            },
        })
        .unwrap();
}
fn stroke(id: u128, writer: u128, points: Vec<[f64; 2]>, color: [f32; 4], width: f64, erase: bool, depth: u8) -> Stroke {
    Stroke {
        id: StrokeId::new(&hex(id)).unwrap(),
        canvas: canvas(),
        owner: owner(writer),
        revision: 0,
        anchor: RegionKey { depth, ..Default::default() },
        points,
        color: Srgba { rgba: color },
        width,
        erase,
        brush: None,
        samples: None,
    }
}
fn pixel(image: &WorldImage, x: u32, y: u32) -> [f32; 4] {
    image.pixels[(y * image.width + x) as usize]
}
fn no_assets(_: &MediaFrame) -> Result<Vec<u8>> {
    panic!("Synthetic stroke fixture has no media")
}
fn draw(world: &WorldState, view: &Viewport) -> WorldImage {
    render_view(world, view, 0, &mut no_assets).unwrap()
}

#[test]
fn upper_author_eraser_reveals_other_author_instead_of_erasing_the_world() {
    let mut world = world();
    append(&mut world, 0, Edit::AddStroke { stroke: stroke(1, 0, vec![[-24., 0.], [24., 0.]], [1., 0., 0., 1.], 16., false, 0) });
    append(&mut world, 1, Edit::AddStroke { stroke: stroke(2, 1, vec![[0., -24.], [0., 24.]], [0., 0., 1., 1.], 16., false, 0) });
    append(&mut world, 1, Edit::AddStroke { stroke: stroke(3, 1, vec![[0., 0.]], [1.; 4], 8., true, 0) });
    let image = draw(&world, &view(0));
    assert_eq!(pixel(&image, 32, 32), [1., 0., 0., 1.]);
    assert_eq!(pixel(&image, 32, 20), [0., 0., 1., 1.]);
    assert_eq!(pixel(&image, 20, 32), [1., 0., 0., 1.]);
    append(&mut world, 0, Edit::SetStrokeVisible { stroke: StrokeId::new(&hex(1)).unwrap(), visible: false });
    let undone = draw(&world, &view(0));
    assert_eq!(pixel(&undone, 32, 32), [1.; 4]);
    assert_eq!(pixel(&undone, 32, 20), [0., 0., 1., 1.]);
}

#[test]
fn cpu_round_dot_matches_an_independent_distance_oracle_at_root_and_deep_scale() {
    for depth in [0, 6, 12] {
        let mut world = world();
        append(&mut world, 0, Edit::AddStroke { stroke: stroke(1, 0, vec![[0., 0.]], [0., 0., 0., 1.], 10., false, depth) });
        let image = draw(&world, &view(depth));
        for y in 0..64 {
            for x in 0..64 {
                let radius = ((f64::from(x) + 0.5 - 32.).powi(2) + (f64::from(y) + 0.5 - 32.).powi(2)).sqrt();
                if radius < 4.0 {
                    assert_eq!(pixel(&image, x, y), [0., 0., 0., 1.]);
                }
                if radius > 6.0 {
                    assert_eq!(pixel(&image, x, y), [1.; 4]);
                }
            }
        }
    }
}

#[test]
fn renderer_queries_visible_regions_without_scanning_offscreen_objects_each_frame() {
    let mut world = world();
    append(&mut world, 0, Edit::AddStroke { stroke: stroke(1, 0, vec![[0., 0.]], [0., 0., 0., 1.], 10., false, 0) });
    for n in 2..200 {
        let mut source = stroke(n, 0, vec![[0., 0.]], [0., 0., 0., 1.], 10., false, 0);
        source.anchor.x = photocraft_scene::Wide(n as i128 * 10);
        append(&mut world, 0, Edit::AddStroke { stroke: source });
    }
    let mut renderer = WorldRenderer::new(&world).unwrap();
    renderer.render(&view(0), 0, &mut no_assets).unwrap();
    assert_eq!(renderer.stats.candidates, 1);
    assert_eq!(renderer.stats.rendered_objects, 1);
    assert!(renderer.stats.indexed_references > 199);
    let index_count = renderer.stats.indexed_references;
    renderer.render(&view(6), 0, &mut no_assets).unwrap();
    assert_eq!(renderer.stats.candidates, 1);
    assert_eq!(renderer.stats.indexed_references, index_count);
}

fn media_fixture() -> (MediaPlacement, BTreeMap<photocraft_scene::shared::media::AssetHash, Vec<u8>>) {
    let red = encode_png(&WorldImage { width: 2, height: 1, pixels: vec![[1., 0., 0., 1.], [0., 0., 1., 1.]] }).unwrap();
    let green = encode_png(&WorldImage { width: 2, height: 1, pixels: vec![[0., 1., 0., 1.], [0., 1., 0., 1.]] }).unwrap();
    let hashes = [media_asset_hash(&red).unwrap(), media_asset_hash(&green).unwrap()];
    let placement = MediaPlacement {
        id: MediaId::new(&hex(99)).unwrap(),
        canvas: canvas(),
        owner: owner(0),
        anchor: RegionKey { depth: 6, ..Default::default() },
        offset: [-10., -5.],
        width: 20.,
        height: 10.,
        frames: vec![
            MediaFrame { hash: hashes[0], byte_len: red.len() as u64, width: 2, height: 1, duration_ms: 100 },
            MediaFrame { hash: hashes[1], byte_len: green.len() as u64, width: 2, height: 1, duration_ms: 200 },
        ],
        looping: true,
        provenance: Provenance {
            source: MediaSource::Gallery { entry: SourceId::new(&hex(9)).unwrap(), room: None, game: None, chain: 0, frame: None },
            source_revision: 2,
            artifact_hash: hashes[0],
            authors: vec![Credit { profile: owner(1), label: "Synthetic artist".into() }],
        },
    };
    (placement, BTreeMap::from([(hashes[0], red), (hashes[1], green)]))
}

#[test]
fn verified_media_preserves_animation_timing_and_source_resolution_at_depth() {
    let mut world = world();
    let (media, assets) = media_fixture();
    append(&mut world, 0, Edit::PlaceMedia { media: Box::new(media.clone()) });
    let mut resolver = |frame: &MediaFrame| Ok(assets.get(&frame.hash).unwrap().clone());
    let mut renderer = WorldRenderer::new(&world).unwrap();
    let first = renderer.render(&view(6), 0, &mut resolver).unwrap();
    assert_eq!(pixel(&first, 24, 32), [1., 0., 0., 1.]);
    assert_eq!(pixel(&first, 40, 32), [0., 0., 1., 1.]);
    let second = renderer.render(&view(6), 100, &mut resolver).unwrap();
    assert_eq!(pixel(&second, 24, 32), [0., 1., 0., 1.]);
    assert_eq!(renderer.render(&view(6), 300, &mut resolver).unwrap(), first);
    let mut corrupt = assets.get(&media.frames[0].hash).unwrap().clone();
    corrupt[0] ^= 1;
    assert!(decode_frame(&media.frames[0], &corrupt).is_err());
    let mut wrong = media.frames[0].clone();
    wrong.width = 3;
    assert!(decode_frame(&wrong, assets.get(&wrong.hash).unwrap()).is_err());
}

fn frozen_tour() -> (WorldState, FrozenTour) {
    let mut world = world();
    append(&mut world, 0, Edit::AddStroke { stroke: stroke(1, 0, vec![[0., 0.]], [1., 0., 0., 1.], 8., false, 0) });
    let from = coordinates::home(canvas());
    let to = coordinates::pan(&from, [10., 0.]).unwrap();
    let tour = Tour {
        id: TourId::new(&hex(10)).unwrap(),
        canvas: canvas(),
        name: "Small trip".into(),
        revision: 0,
        stops: vec![
            TourStop { camera: from, travel_ms: 0, pause_ms: 500, target: None },
            TourStop { camera: to, travel_ms: 1000, pause_ms: 500, target: None },
        ],
    };
    append(&mut world, 0, Edit::PutTour { tour: tour.clone(), expected_revision: 0 });
    let frozen = FrozenTour::new(&world, tour.id, ExportSettings { width: 64, height: 64, fps: 2 }).unwrap();
    (world, frozen)
}

#[test]
fn frozen_tour_gif_has_frame_order_timing_and_excludes_later_art() {
    let (mut world, frozen) = frozen_tour();
    append(&mut world, 0, Edit::AddStroke { stroke: stroke(2, 0, vec![[0., 0.]], [0., 1., 0., 1.], 60., false, 0) });
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("tour.gif");
    export_tour_gif(&frozen, &path, true, &mut no_assets, |_, _| true).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let frames = image::codecs::gif::GifDecoder::new(Cursor::new(&bytes)).unwrap().into_frames().collect_frames().unwrap();
    assert_eq!(frames.len(), 4);
    for frame in &frames {
        assert_eq!(frame.delay().numer_denom_ms(), (500, 1));
    }
    assert_eq!(frames[0].buffer().get_pixel(32, 32).0, [255, 0, 0, 255]);
    assert_eq!(frames[3].buffer().get_pixel(42, 32).0, [255, 0, 0, 255]);
    assert_eq!(frames[3].buffer().get_pixel(32, 32).0, [255; 4]);
    assert!(bytes.windows(11).any(|w| w == b"NETSCAPE2.0"));
    assert!(export_tour_gif(&frozen, &path, true, &mut no_assets, |_, _| true).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn failed_or_cancelled_gif_has_no_published_partial_and_preserves_scene() {
    let (world, frozen) = frozen_tour();
    let hash = world.canonical_hash().unwrap();
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("cancelled.gif");
    assert!(export_tour_gif(&frozen, &output, false, &mut no_assets, |index, _| index < 2).is_err());
    assert!(!output.exists());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    assert_eq!(world.canonical_hash().unwrap(), hash);
}

#[cfg(windows)]
#[test]
fn tour_sequence_is_atomically_published_and_records_pinned_scene() {
    let (_, frozen) = frozen_tour();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("journey");
    let frames = export_tour_sequence(&frozen, &path, &mut no_assets, |_, _| true).unwrap();
    assert_eq!(frames.len(), 4);
    let last = image::open(&frames[3]).unwrap().into_rgba8();
    assert_eq!(last.get_pixel(42, 32).0, [255, 0, 0, 255]);
    let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(path.join("tour.json")).unwrap()).unwrap();
    assert_eq!(manifest["world_hash"], frozen.scene_hash());
    let cancelled = root.path().join("cancelled");
    assert!(export_tour_sequence(&frozen, &cancelled, &mut no_assets, |index, _| index == 0).is_err());
    assert!(!cancelled.exists());
    let raced = root.path().join("raced");
    assert!(
        export_tour_sequence(&frozen, &raced, &mut no_assets, |index, total| {
            if index == total {
                std::fs::create_dir(&raced).unwrap();
            }
            true
        })
        .is_err()
    );
    assert!(raced.is_dir());
    assert_eq!(std::fs::read_dir(&raced).unwrap().count(), 0);
}

#[cfg(windows)]
#[test]
fn portable_archive_roundtrips_originals_tours_history_and_independent_media() {
    let (mut world, _) = frozen_tour();
    let (media, assets) = media_fixture();
    append(&mut world, 0, Edit::PlaceMedia { media: Box::new(media.clone()) });
    append(&mut world, 0, Edit::SetMediaVisible { media: media.id, visible: false });
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("archive");
    let mut resolve = |frame: &MediaFrame| Ok(assets.get(&frame.hash).unwrap().clone());
    export_archive(&world, &path, &mut resolve, |_, _| true).unwrap();
    let (restored, restored_assets) = read_archive(&path).unwrap();
    assert_eq!(restored.canonical_hash().unwrap(), world.canonical_hash().unwrap());
    assert_eq!(restored_assets, assets);
    assert_eq!(restored.media.get(&media.id).unwrap().media.provenance, media.provenance);
    assert!(!restored.media.get(&media.id).unwrap().visible);
    let victim = path.join(format!("{}.png", media.frames[0].hash.as_str()));
    std::fs::write(&victim, b"truncated").unwrap();
    assert!(read_archive(&path).is_err());
    assert_eq!(world.media.get(&media.id).unwrap().media.frames.len(), 2);
}
