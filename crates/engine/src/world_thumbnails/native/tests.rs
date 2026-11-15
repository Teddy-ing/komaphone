#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use photocraft_scene::{
    OwnerId, Srgba, Stroke, StrokeId,
    shared::{AcceptedOp, DeviceId, Edit, OpId, WorldOp},
};

fn key(number: u64) -> Key {
    let canvas = CanvasId::new(&format!("{:032x}", 1)).unwrap();
    let profile = OwnerId::new(&format!("{number:032x}")).unwrap();
    Key {
        scope: WorldScope::new("https://example.invalid", profile, DeviceId::new(&format!("{:032x}", number + 100)).unwrap()).unwrap(),
        info: CanvasSummary { canvas, owner: profile, name: "Artwork".into(), import_origin: None, initial_name: "Artwork".into(), revision: 0, epoch: 1 },
    }
}
fn world(key: &Key) -> WorldState {
    WorldState::new(key.info.canvas, key.info.owner, key.info.name.clone()).unwrap()
}
fn draw(world: &mut WorldState, key: &Key, depth: u8, lateral: i128) {
    world
        .apply(&AcceptedOp {
            revision: 1,
            op: WorldOp {
                id: OpId::new(&format!("{:032x}", 10)).unwrap(),
                canvas: world.canvas,
                author: world.owner,
                device: key.scope.device,
                sequence: 1,
                authority_epoch: 1,
                action: Edit::AddStroke {
                    stroke: Stroke {
                        id: StrokeId::new(&format!("{:032x}", 20)).unwrap(),
                        canvas: world.canvas,
                        owner: world.owner,
                        revision: 0,
                        anchor: photocraft_scene::RegionKey { depth, x: photocraft_scene::Wide(lateral), ..Default::default() },
                        points: vec![[-40., 0.], [40., 0.]],
                        width: 40.,
                        color: Srgba { rgba: [1., 0., 0., 1.] },
                        erase: false,
                    },
                },
            },
        })
        .unwrap();
}
#[test]
fn accepted_art_preview_uses_cpu_renderer_and_finds_off_home_detail() {
    for (depth, lateral) in [(0, 0), (0, 100), (6, 0), (6, 100)] {
        let key = key(1);
        let mut scene = world(&key);
        draw(&mut scene, &key, depth, lateral);
        let mut renderer = world_media::WorldRenderer::new(&scene).unwrap();
        let mut camera = photocraft_scene::coordinates::home(scene.canvas);
        camera.zoom = photocraft_scene::coordinates::MIN_ROOT_ZOOM;
        let view = Viewport { camera, width: 640, height: 400 };
        if depth > 0 {
            assert!(!renderer.visible_content(&view).unwrap().is_empty(), "Deep art is indexed inside Home");
            assert!(!visible_at_thumbnail_scale(&mut renderer, &view).unwrap(), "Indexed deep art still needs native-depth thumbnail framing");
        }
        let before = scene.canonical_hash().unwrap();
        let image = render_scene(&scene, &mut |_| panic!("No media in stroke fixture"), &mut || true).unwrap();
        assert_eq!((image.width, image.height, image.rgba.len(), image.revision), (WIDTH, HEIGHT, (WIDTH * HEIGHT * 4) as usize, 1));
        assert!(
            image.rgba.chunks_exact(4).any(|pixel| pixel[0] > 200 && pixel[1] < 100 && pixel[2] < 100),
            "Missing red artwork at depth {depth}, lateral {lateral}"
        );
        assert_eq!(scene.canonical_hash().unwrap(), before);
        assert!(render_scene(&scene, &mut |_| panic!(), &mut || false).is_err());
    }
}

#[test]
fn thumbnail_includes_verified_remix_pixels_without_altering_source_history() {
    use photocraft_scene::shared::{
        MediaId, SourceId,
        media::{Credit, MediaFrame, MediaPlacement, MediaSource, Provenance},
    };
    let key = key(1);
    let mut scene = world(&key);
    let png = world_media::encode_png(&world_media::WorldImage { width: 2, height: 1, pixels: vec![[0., 0., 1., 1.]; 2] }).unwrap();
    let hash = world_media::media_asset_hash(&png).unwrap();
    let media = MediaPlacement {
        id: MediaId::new(&format!("{:032x}", 31)).unwrap(),
        canvas: scene.canvas,
        owner: scene.owner,
        anchor: Default::default(),
        offset: [-200., -100.],
        width: 400.,
        height: 200.,
        frames: vec![MediaFrame { hash, byte_len: png.len() as u64, width: 2, height: 1, duration_ms: 100 }],
        looping: true,
        provenance: Provenance {
            source: MediaSource::Gallery { entry: SourceId::new(&format!("{:032x}", 32)).unwrap(), room: None, game: None, chain: 0, frame: None },
            source_revision: 1,
            artifact_hash: hash,
            authors: vec![Credit { profile: scene.owner, label: "Fixture credit".into() }],
        },
    };
    scene
        .apply(&AcceptedOp {
            revision: 1,
            op: WorldOp {
                id: OpId::new(&format!("{:032x}", 33)).unwrap(),
                canvas: scene.canvas,
                author: scene.owner,
                device: key.scope.device,
                sequence: 1,
                authority_epoch: 1,
                action: Edit::PlaceMedia { media: Box::new(media) },
            },
        })
        .unwrap();
    let before = scene.canonical_hash().unwrap();
    let mut resolved = 0;
    let image = render_scene(
        &scene,
        &mut |frame| {
            assert_eq!(frame.hash, hash);
            resolved += 1;
            Ok(png.clone())
        },
        &mut || true,
    )
    .unwrap();
    assert_eq!(resolved, 1);
    assert!(image.rgba.chunks_exact(4).any(|pixel| pixel[0] < 10 && pixel[1] < 10 && pixel[2] > 240));
    assert_eq!(scene.canonical_hash().unwrap(), before);
    assert!(render_scene(&scene, &mut |_| Ok(vec![0; png.len()]), &mut || true).is_err());
}
#[test]
fn persisted_preview_is_scoped_checksummed_and_reopens_without_a_network_reader() {
    let root = tempfile::tempdir().unwrap();
    let key = key(1);
    let image = Thumbnail { width: WIDTH, height: HEIGHT, rgba: vec![255; (WIDTH * HEIGHT * 4) as usize], revision: 0 };
    let path = root.path().join(format!("{}.thumb", key.id().unwrap()));
    write_cache(root.path(), &path, &key, &image).unwrap();
    assert_eq!(read_cache(&path, &key).unwrap().unwrap().rgba, image.rgba);
    assert!(read_cache(&path, &super::tests::key(2)).is_err());
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] = b'!';
    std::fs::write(&path, bytes).unwrap();
    assert!(read_cache(&path, &key).is_err());
}
#[test]
fn removed_canvas_and_changed_generation_reject_late_results() {
    let root = tempfile::tempdir().unwrap();
    let first = key(1);
    let worker = Worker::start(root.path().into(), None).unwrap();
    let mut state = State { worker: Some(worker), ..Default::default() };
    state.catalog.insert(first.info.canvas, first.clone());
    state.reconcile();
    let shared = Arc::clone(&state.worker.as_ref().unwrap().shared);
    assert!(current(&shared, &first, 0));
    state.remove(first.info.canvas);
    assert!(!current(&shared, &first, 0));
    assert!(!state.contains(first.info.canvas));
    state.catalog.insert(first.info.canvas, first.clone());
    state.reconcile();
    lock(&shared).completed.push_back(Completed {
        key: first.clone(),
        generation: 0,
        result: Ok(Thumbnail { width: WIDTH, height: HEIGHT, rgba: vec![0; (WIDTH * HEIGHT * 4) as usize], revision: 0 }),
    });
    state.reset();
    state.catalog.insert(first.info.canvas, first.clone());
    state.reconcile();
    assert!(!current(&shared, &first, 0));
    state.poll();
    assert!(state.memory.is_empty());
    let foreign = key(2);
    assert!(!current(&shared, &foreign, 1));
}
#[test]
fn decoded_preview_memory_evicts_oldest_and_disk_inventory_is_bounded() {
    let root = tempfile::tempdir().unwrap();
    let worker = Worker::start(root.path().into(), None).unwrap();
    let mut state = State { worker: Some(worker), ..Default::default() };
    for n in 1..=40 {
        let mut key = key(1);
        key.info.canvas = CanvasId::new(&format!("{n:032x}")).unwrap();
        state.catalog.insert(key.info.canvas, key.clone());
        state.reconcile();
        lock(&state.worker.as_ref().unwrap().shared).completed.push_back(Completed {
            key,
            generation: 0,
            result: Ok(Thumbnail { width: WIDTH, height: HEIGHT, rgba: vec![0; (WIDTH * HEIGHT * 4) as usize], revision: 0 }),
        });
        state.poll();
    }
    assert_eq!(state.memory.len(), MEMORY_ITEMS);
    assert!(!state.memory.contains_key(&CanvasId::new(&format!("{:032x}", 1)).unwrap()));
    for n in 0..=DISK_ITEMS {
        std::fs::write(root.path().join(format!("{n:064x}.thumb")), [0u8]).unwrap();
    }
    make_room(root.path(), 1024).unwrap();
    assert!(std::fs::read_dir(root.path()).unwrap().count() < DISK_ITEMS);
}

fn access(root: &Path) -> WorldAccess {
    #[derive(Default)]
    struct Credentials(Mutex<BTreeMap<(String, String), String>>);
    impl photocraft_room_client::CredentialStore for Credentials {
        fn get(&self, namespace: &str, key: &str) -> photocraft_room_client::Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(&(namespace.into(), key.into())).cloned())
        }
        fn set(&self, namespace: &str, key: &str, secret: &str) -> photocraft_room_client::Result<()> {
            self.0.lock().unwrap().insert((namespace.into(), key.into()), secret.into());
            Ok(())
        }
        fn remove(&self, namespace: &str, key: &str) -> photocraft_room_client::Result<()> {
            self.0.lock().unwrap().remove(&(namespace.into(), key.into()));
            Ok(())
        }
    }
    let mut room = photocraft_room_client::Outbox::open(root.join("room.sqlite"), Box::<Credentials>::default()).unwrap();
    room.configure("https://example.invalid", "Thumbnail test").unwrap();
    let pair = room.next().unwrap().unwrap();
    room.acknowledge(&pair, &photocraft_party::protocol::RpcResponse::Paired { participant: room.status().profile.unwrap() }).unwrap();
    WorldAccess::from_room(&room).unwrap().unwrap()
}

#[test]
fn older_blank_derived_cache_regenerates_visible_deep_art_without_changing_original_history() {
    let root = tempfile::tempdir().unwrap();
    let access = access(root.path());
    let mut key = key(1);
    key.scope = access.scope().clone();
    key.info.owner = key.scope.profile;
    let mut scene = world(&key);
    draw(&mut scene, &key, 6, 100);
    key.info.revision = scene.revision;
    let before = scene.canonical_hash().unwrap();
    let path = root.path().join(format!("{}.thumb", key.id().unwrap()));
    write_cache(root.path(), &path, &key, &Thumbnail { width: WIDTH, height: HEIGHT, rgba: vec![255; (WIDTH * HEIGHT * 4) as usize], revision: 1 }).unwrap();
    let mut old: Cache = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    old.schema = 1;
    std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
    assert!(read_cache(&path, &key).is_err());
    let shared = Shared::default();
    lock(&shared).allowed.insert(key.info.canvas, key.clone());
    let original = Arc::new(scene);
    let job = Job { key: key.clone(), generation: 0, access, source: Some(Arc::clone(&original)) };
    let regenerated = prepare(root.path(), &shared, &job, None).unwrap();
    assert!(regenerated.rgba.chunks_exact(4).any(|pixel| pixel[0] > 200 && pixel[1] < 100 && pixel[2] < 100));
    assert_eq!(read_cache(&path, &key).unwrap().unwrap().rgba, regenerated.rgba);
    assert_eq!(original.canonical_hash().unwrap(), before);
    let saved: Cache = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(saved.schema, CACHE_SCHEMA);
}

#[test]
fn completed_mailbox_backpressure_preserves_the_next_preview_and_reset_keeps_only_access() {
    let root = tempfile::tempdir().unwrap();
    let access = access(root.path());
    let shared = Arc::new(Shared::default());
    let mut state = State { worker: Some(Worker { shared: Arc::clone(&shared) }), ..Default::default() };
    state.access(Some(&access));
    let generation = lock(&shared).generation;
    let image = || Thumbnail { width: WIDTH, height: HEIGHT, rgba: vec![255; (WIDTH * HEIGHT * 4) as usize], revision: 0 };
    let mut last = key(1);
    last.scope = access.scope().clone();
    last.info.owner = last.scope.profile;
    for number in 1..=JOBS + 1 {
        last.info.canvas = CanvasId::new(&format!("{number:032x}")).unwrap();
        state.catalog.insert(last.info.canvas, last.clone());
        state.pending.insert(last.info.canvas, last.clone());
        if number <= JOBS {
            lock(&shared).completed.push_back(Completed { key: last.clone(), generation, result: Ok(image()) });
        }
    }
    state.reconcile();
    write_cache(root.path(), &root.path().join(format!("{}.thumb", last.id().unwrap())), &last, &image()).unwrap();
    lock(&shared).requests.push_back(Job { key: last.clone(), generation, access: access.copy_for_worker(), source: None });
    let background = Arc::clone(&shared);
    let directory = root.path().to_path_buf();
    let join = std::thread::spawn(move || work(background, directory, None));
    // A full result mailbox must pause the next job rather than consume and lose it.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(lock(&shared).requests.len(), 1);
    state.poll();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !state.pending.is_empty() && Instant::now() < deadline {
        state.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(state.pending.is_empty());
    assert_eq!(state.memory.len(), JOBS + 1);
    state.reset();
    assert_eq!(state.access.as_ref().unwrap().scope(), access.scope());
    assert!(state.catalog.is_empty() && state.memory.is_empty());
    state.access(None);
    assert!(state.access.is_none());
    drop(state);
    join.join().unwrap();
}
