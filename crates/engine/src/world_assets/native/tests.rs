use super::*;
use image::ImageEncoder;
use photocraft_scene::{OwnerId, shared::DeviceId};
use std::sync::Mutex;
use std::time::{Duration, Instant};

fn scope(n: u128) -> Scope {
    Scope {
        account: crate::worlds::Account {
            endpoint: "http://127.0.0.1:1234".into(),
            profile: OwnerId::new(&format!("{:032x}", n + 1)).unwrap(),
            device: DeviceId::new(&format!("{:032x}", n + 101)).unwrap(),
        },
        canvas: CanvasId::new(&format!("{:032x}", 201)).unwrap(),
        generation: 0,
    }
}
fn png() -> (MediaFrame, Vec<u8>) {
    let mut random = 37_u32;
    let pixels: Vec<u8> = (0..256 * 128 * 4)
        .map(|_| {
            random = random.wrapping_mul(1664525).wrapping_add(1013904223);
            (random >> 24) as u8
        })
        .collect();
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes).write_image(&pixels, 256, 128, image::ExtendedColorType::Rgba8).unwrap();
    let frame = MediaFrame { hash: world_media::media_asset_hash(&bytes).unwrap(), byte_len: bytes.len() as u64, width: 256, height: 128, duration_ms: 100 };
    (frame, bytes)
}
fn ticket(scope: Scope, frame: &MediaFrame) -> Ticket {
    Ticket { scope, frame: frame.clone(), offset: 0 }
}
fn next(events: &Receiver<Event>) -> Event {
    events.recv_timeout(Duration::from_secs(5)).expect("background event")
}

#[test]
fn immutable_chunk_offsets_totals_and_hashes_are_checked_before_publishing() {
    let root = tempfile::tempdir().unwrap();
    let (frame, bytes) = png();
    let (tx, rx) = mpsc::sync_channel(32);
    let mut downloads = BTreeMap::new();
    let initial = ticket(scope(1), &frame);
    load(root.path(), initial.clone(), true, &mut downloads, &tx).unwrap();
    assert!(matches!(next(&rx), Event::Need(_)));
    let first = bytes[..photocraft_party::protocol::CHUNK_BYTES].to_vec();
    assert!(chunk(root.path(), initial.clone(), first.clone(), frame.byte_len + 1, &mut downloads, &tx).is_err());
    let mut wrong = initial.clone();
    wrong.offset = 1;
    assert!(chunk(root.path(), wrong, first, frame.byte_len, &mut downloads, &tx).is_err());
    let mut current = initial.clone();
    for part in bytes.chunks(photocraft_party::protocol::CHUNK_BYTES) {
        chunk(root.path(), current.clone(), part.to_vec(), frame.byte_len, &mut downloads, &tx).unwrap();
        match next(&rx) {
            Event::Need(next) => current = next,
            Event::Ready { bytes: accepted, .. } => assert_eq!(*accepted, bytes),
            _ => panic!("Unexpected event"),
        }
    }
    assert_eq!(read(&cache_path(root.path(), &initial.scope, &frame).unwrap(), &frame).unwrap(), Some(bytes.clone()));
    let other = ticket(scope(2), &frame);
    load(root.path(), other.clone(), true, &mut downloads, &tx).unwrap();
    next(&rx);
    let mut corrupt = bytes;
    corrupt[0] ^= 1;
    let mut current = other.clone();
    let mut failed = false;
    for part in corrupt.chunks(photocraft_party::protocol::CHUNK_BYTES) {
        if chunk(root.path(), current.clone(), part.to_vec(), frame.byte_len, &mut downloads, &tx).is_err() {
            failed = true;
            break;
        }
        if let Event::Need(next) = next(&rx) {
            current = next;
        }
    }
    assert!(failed);
    assert!(!cache_path(root.path(), &other.scope, &frame).unwrap().exists());
}

#[test]
fn scoped_verified_cache_reopens_offline_and_cannot_cross_accounts_devices_or_canvases() {
    let root = tempfile::tempdir().unwrap();
    let (frame, bytes) = png();
    let origin = scope(1);
    save(root.path(), &origin, &frame, &bytes).unwrap();
    let worker = Worker::start(root.path().into()).unwrap();
    worker.send(Command::Load(ticket(origin.clone(), &frame))).unwrap();
    match next(&worker.events) {
        Event::Ready { bytes: got, .. } => assert_eq!(*got, bytes),
        _ => panic!("Expected offline cache hit"),
    }
    let mut variants = vec![scope(2), origin.clone(), origin.clone(), origin.clone()];
    variants[1].account.device = DeviceId::new(&format!("{:032x}", 999)).unwrap();
    variants[2].canvas = CanvasId::new(&format!("{:032x}", 998)).unwrap();
    variants[3].account.endpoint = "http://127.0.0.1:4321".into();
    for foreign in variants {
        assert_eq!(read(&cache_path(root.path(), &foreign, &frame).unwrap(), &frame).unwrap(), None);
    }
    std::fs::write(cache_path(root.path(), &origin, &frame).unwrap(), b"corrupt retained cache").unwrap();
    assert!(read(&cache_path(root.path(), &origin, &frame).unwrap(), &frame).is_err());
}

#[test]
fn long_scoped_media_cache_publishes_reopens_and_preserves_existing_bytes() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("long-media-cache-directory-".repeat(6));
    let (frame, bytes) = png();
    let origin = scope(1);
    let path = cache_path(&root, &origin, &frame).unwrap();
    assert!(path.as_os_str().len() > 260, "Exercise the Windows legacy path boundary");
    save(&root, &origin, &frame, &bytes).unwrap();
    assert_eq!(read(&path, &frame).unwrap(), Some(bytes.clone()));
    save(&root, &origin, &frame, &bytes).unwrap();
    assert_eq!(cache_size(&root).unwrap(), (1, bytes.len() as u64), "No leaked temporary or replacement file");

    let worker = Worker::start(root.clone()).unwrap();
    worker.send(Command::Load(ticket(origin, &frame))).unwrap();
    match next(&worker.events) {
        Event::Ready { bytes: loaded, .. } => assert_eq!(*loaded, bytes),
        _ => panic!("Expected verified offline media on the long path"),
    }
    drop(worker);
    let corrupt = b"Retained existing cache bytes";
    std::fs::write(&path, corrupt).unwrap();
    assert!(save(&root, &scope(1), &frame, &bytes).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), corrupt, "A failed validation must not overwrite existing content");
}

struct GateFiles {
    entered: mpsc::Sender<()>,
    release: Mutex<Receiver<()>>,
    path: PathBuf,
    finished: Arc<AtomicBool>,
}
struct Prepared {
    action: WorldExport,
    finished: Arc<AtomicBool>,
}
impl WorldFileAccess for GateFiles {
    fn prepare(&self, action: &WorldExport) -> Result<Box<dyn PreparedWorldFiles>> {
        self.entered.send(()).map_err(error)?;
        self.release.lock().map_err(error)?.recv_timeout(Duration::from_secs(5)).map_err(error)?;
        Ok(Box::new(Prepared { action: action.clone().with_path(self.path.clone()), finished: Arc::clone(&self.finished) }))
    }
}
impl PreparedWorldFiles for Prepared {
    fn action(&self) -> WorldExport {
        self.action.clone()
    }
    fn finish(&mut self) -> Result<()> {
        self.finished.store(true, Ordering::Release);
        Ok(())
    }
}
fn make_job(scope: Scope, path: PathBuf) -> Job {
    let world = Arc::new(WorldState::new(scope.canvas, scope.account.profile, "Pinned original".into()).unwrap());
    Job {
        camera: photocraft_scene::coordinates::home(scope.canvas),
        view_revision: None,
        scope,
        world,
        action: WorldExport::View { path, width: 64, height: 64, elapsed_ms: 0 },
        files: None,
        cancel: Arc::new(AtomicBool::new(false)),
    }
}

#[test]
fn selected_png_pins_review_cursor_offthread_and_preserves_latest_scene_for_archives() {
    use photocraft_scene::shared::{AcceptedOp, Edit, OpId, WorldOp};
    use photocraft_scene::{Srgba, Stroke, StrokeId};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("review.png");
    let job = make_job(scope(1), path.clone());
    let mut latest = (*job.world).clone();
    let id = StrokeId::new(&format!("{:032x}", 700)).unwrap();
    let stroke = Stroke {
        id,
        canvas: latest.canvas,
        owner: latest.owner,
        revision: 0,
        anchor: Default::default(),
        points: vec![[0., 0.]],
        width: 16.,
        color: Srgba { rgba: [0., 0., 0., 1.] },
        erase: false,
    };
    for (index, action) in [Edit::AddStroke { stroke }, Edit::SetStrokeVisible { stroke: id, visible: false }].into_iter().enumerate() {
        let revision = index as u64 + 1;
        latest
            .apply(&AcceptedOp {
                revision,
                op: WorldOp {
                    id: OpId::new(&format!("{:032x}", 800 + index)).unwrap(),
                    canvas: latest.canvas,
                    author: latest.owner,
                    device: job.scope.account.device,
                    sequence: revision,
                    authority_epoch: 1,
                    action,
                },
            })
            .unwrap();
    }
    assert!(!latest.strokes[&id].visible);
    let mut review = photocraft_scene::shared::ReviewState::first_visit(latest.canvas, latest.owner, 0).unwrap();
    review.begin(latest.revision).unwrap();
    review.scrub(1).unwrap();
    let mut session = Session::new();
    session.worlds.account = Some(job.scope.account.clone());
    session.worlds.active = Some(crate::worlds::Canvas {
        info: wire::CanvasSummary {
            canvas: latest.canvas,
            owner: latest.owner,
            name: latest.name.clone(),
            import_origin: None,
            initial_name: latest.name.clone(),
            revision: latest.revision,
            epoch: 1,
        },
        canonical: Arc::new(latest.clone()),
        display: Arc::new(latest.clone()),
        pending: 0,
        notice: None,
        viewer: crate::worlds::Viewer { camera: job.camera, bookmarks: vec![], review: Some(review) },
    });
    session.enable_world_assets(root.path().join("cache")).unwrap();
    session.world_export(job.action).unwrap();
    loop {
        if let Event::Progress { status, .. } = next(&session.worlds.assets.inner.worker.as_ref().unwrap().events)
            && !status.busy
        {
            assert_eq!(status.message, "Exported accepted revision 1");
            break;
        }
    }
    let output = image::open(&path).unwrap().into_rgba8();
    assert_eq!(output.get_pixel(32, 32).0, [0, 0, 0, 255]);
    assert_eq!(output.get_pixel(0, 0).0, [255, 255, 255, 255]);
    assert!(!latest.strokes[&id].visible);
    let mut archive = make_job(scope(1), root.path().join("archive"));
    archive.world = Arc::new(latest);
    archive.view_revision = Some(1);
    archive.action = WorldExport::Archive { path: root.path().join("archive") };
    let archive = prepare_job(archive).unwrap();
    assert_eq!(archive.job.world.revision, 2);
    assert!(!archive.job.world.strokes[&id].visible);
}
#[test]
fn export_preparation_is_offthread_and_scope_switch_cancels_before_publication() {
    let root = tempfile::tempdir().unwrap();
    let worker = Worker::start(root.path().join("cache")).unwrap();
    let (entered, seen) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let actual = root.path().join("capability-output.png");
    let claimed = root.path().join("raw-claim.png");
    let finished = Arc::new(AtomicBool::new(false));
    let mut job = make_job(scope(1), claimed.clone());
    job.files = Some(Arc::new(GateFiles { entered, release: Mutex::new(wait), path: actual.clone(), finished: Arc::clone(&finished) }));
    let started = Instant::now();
    worker.send(Command::Export(job)).unwrap();
    assert!(started.elapsed() < Duration::from_millis(100));
    seen.recv_timeout(Duration::from_secs(5)).unwrap();
    worker.generation.store(1, Ordering::Release);
    release.send(()).unwrap();
    match next(&worker.events) {
        Event::Progress { status, .. } => {
            assert!(!status.busy);
            assert!(status.message.contains("cancelled"));
        }
        _ => panic!("Expected cancelled export"),
    }
    assert!(!actual.exists());
    assert!(!claimed.exists());
    assert!(!finished.load(Ordering::Acquire));
}

#[test]
fn asynchronous_capability_export_finishes_only_after_publication_and_never_overwrites() {
    let root = tempfile::tempdir().unwrap();
    let worker = Worker::start(root.path().join("cache")).unwrap();
    let (entered, seen) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let actual = root.path().join("granted.png");
    let claimed = root.path().join("not-granted.png");
    let finished = Arc::new(AtomicBool::new(false));
    let mut job = make_job(scope(1), claimed.clone());
    job.files = Some(Arc::new(GateFiles { entered, release: Mutex::new(wait), path: actual.clone(), finished: Arc::clone(&finished) }));
    worker.send(Command::Export(job)).unwrap();
    seen.recv_timeout(Duration::from_secs(5)).unwrap();
    release.send(()).unwrap();
    loop {
        if let Event::Progress { status, .. } = next(&worker.events)
            && !status.busy
        {
            assert!(status.message.starts_with("Exported"));
            break;
        }
    }
    assert!(finished.load(Ordering::Acquire));
    assert!(!claimed.exists());
    let original = std::fs::read(&actual).unwrap();
    assert_eq!(image::load_from_memory(&original).unwrap().width(), 64);
    worker.send(Command::Export(make_job(scope(1), actual.clone()))).unwrap();
    loop {
        if let Event::Progress { status, .. } = next(&worker.events)
            && !status.busy
        {
            assert!(!status.message.starts_with("Exported"));
            break;
        }
    }
    assert_eq!(std::fs::read(actual).unwrap(), original);
}

#[test]
fn invalid_export_reports_terminal_failure_and_cache_reset_drops_all_account_data() {
    let root = tempfile::tempdir().unwrap();
    let worker = Worker::start(root.path().into()).unwrap();
    let mut invalid = make_job(scope(1), root.path().join("invalid.png"));
    invalid.action = WorldExport::View { path: root.path().join("invalid.png"), width: 5000, height: 2, elapsed_ms: 0 };
    worker.send(Command::Export(invalid)).unwrap();
    assert!(matches!(next(&worker.events), Event::Failed { hash: None, .. }));
    let (frame, bytes) = png();
    let mut assets = Assets::default();
    assets.cache(frame.clone(), Arc::new(bytes)).unwrap();
    assets.scope = Some(scope(1));
    assets.reset();
    assert!(assets.bytes(&frame).unwrap().is_none());
    assert!(assets.scope.is_none());
    assert!(assets.requests.is_empty());
}

fn media_job(origin: Scope, path: PathBuf) -> Job {
    use photocraft_scene::shared::media::{Credit, MediaSource, Provenance};
    use photocraft_scene::shared::{AcceptedOp, Edit, MediaId, OpId, SourceId, WorldOp};
    let mut job = make_job(origin, path);
    let (frame, _) = png();
    let mut later = frame.clone();
    later.hash = AssetHash::new(&"b".repeat(64)).unwrap();
    let mut world = (*job.world).clone();
    for index in 0..2 {
        let media = MediaPlacement {
            id: MediaId::new(&format!("{:032x}", 300 + index)).unwrap(),
            canvas: world.canvas,
            owner: world.owner,
            anchor: Default::default(),
            offset: if index == 0 { [0., 0.] } else { [400., 400.] },
            width: 10.,
            height: 10.,
            frames: if index == 0 { vec![frame.clone(), later.clone()] } else { vec![later.clone()] },
            looping: true,
            provenance: Provenance {
                source: MediaSource::Gallery { entry: SourceId::new(&format!("{:032x}", 1)).unwrap(), room: None, game: None, chain: 0, frame: None },
                source_revision: 1,
                artifact_hash: frame.hash,
                authors: vec![Credit { profile: world.owner, label: "Synthetic".into() }],
            },
        };
        world
            .apply(&AcceptedOp {
                revision: index + 1,
                op: WorldOp {
                    id: OpId::new(&format!("{:032x}", 400 + index)).unwrap(),
                    canvas: world.canvas,
                    author: world.owner,
                    device: job.scope.account.device,
                    sequence: index + 1,
                    authority_epoch: 1,
                    action: Edit::PlaceMedia { media: Box::new(media) },
                },
            })
            .unwrap();
    }
    job.world = Arc::new(world);
    job
}
#[test]
fn viewport_export_only_requires_visible_current_frame() {
    let root = tempfile::tempdir().unwrap();
    let job = media_job(scope(1), root.path().join("view.png"));
    let prepared = prepare_job(job).unwrap();
    assert_eq!(prepared.frames.len(), 1);
    assert_eq!(prepared.frames[0].hash, png().0.hash);
    let view = Viewport { camera: prepared.job.camera.clone(), width: 64, height: 64 };
    let mut renderer = world_media::WorldRenderer::new(&prepared.job.world).unwrap();
    assert_eq!(renderer.visible_content(&view).unwrap().len(), 1);
    let (frames, animated) = renderer.media_frames(&view, 0).unwrap();
    assert!(animated);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].hash, png().0.hash);
}

#[test]
fn submitted_restore_cannot_claim_cancellation_and_error_waits_for_terminal_service_result() {
    let mut s = Session::new();
    s.worlds.assets.inner.restore_request = Some(77);
    s.worlds.assets.export = Some(ExportStatus { busy: true, completed: 0, total: 1, message: "Restoring".into() });
    assert!(s.world_cancel_export().is_err());
    assert!(s.worlds.assets.export.as_ref().unwrap().busy);
    failure(&mut s, 76, "unrelated request");
    assert!(s.worlds.assets.export.as_ref().unwrap().busy);
    failure(&mut s, 77, "current owner denied");
    assert!(!s.worlds.assets.export.as_ref().unwrap().busy);
    assert_eq!(s.worlds.assets.error.as_deref(), Some("current owner denied"));
}
#[test]
fn cancellation_wakes_export_waiting_for_network_and_keeps_ui_busy_until_ack() {
    let root = tempfile::tempdir().unwrap();
    let worker = Worker::start(root.path().join("cache")).unwrap();
    let job = media_job(scope(1), root.path().join("cancel.png"));
    let cancel = job.cancel.clone();
    worker.send(Command::Export(job)).unwrap();
    assert!(matches!(next(&worker.events), Event::Need(_)));
    assert!(matches!(next(&worker.events),Event::Progress{status,..} if status.busy));
    cancel.store(true, Ordering::Release);
    worker.send(Command::Cancel).unwrap();
    assert!(matches!(next(&worker.events),Event::Progress{status,..} if !status.busy && status.message.contains("cancelled")));
    assert!(!root.path().join("cancel.png").exists());
    let mut s = Session::new();
    s.worlds.assets.export = Some(ExportStatus { busy: true, completed: 0, total: 1, message: "Exporting".into() });
    s.world_cancel_export().unwrap();
    assert!(s.worlds.assets.export.as_ref().unwrap().busy);
}
#[test]
fn stale_failure_does_not_remove_a_new_scope_download_of_the_same_hash() {
    let root = tempfile::tempdir().unwrap();
    let worker = Worker::start(root.path().into()).unwrap();
    let (frame, bytes) = png();
    let old = ticket(scope(1), &frame);
    let mut current_scope = scope(2);
    current_scope.generation = 1;
    worker.generation.store(1, Ordering::Release);
    let mut current = ticket(current_scope, &frame);
    worker.send(Command::Load(current.clone())).unwrap();
    assert!(matches!(next(&worker.events), Event::Need(_)));
    worker.send(Command::Failure { ticket: old, message: "old generation failure".into() }).unwrap();
    for part in bytes.chunks(photocraft_party::protocol::CHUNK_BYTES) {
        worker.send(Command::Chunk { ticket: current.clone(), bytes: part.to_vec(), total: frame.byte_len }).unwrap();
        match next(&worker.events) {
            Event::Need(ticket) => current = ticket,
            Event::Ready { bytes: got, .. } => assert_eq!(*got, bytes),
            _ => panic!("stale failure disturbed new scope"),
        }
    }
}

#[test]
fn unrelated_failed_display_download_does_not_cancel_a_view_export() {
    let root = tempfile::tempdir().unwrap();
    let worker = Worker::start(root.path().join("cache")).unwrap();
    let job = media_job(scope(1), root.path().join("view.png"));
    let cancel = job.cancel.clone();
    worker.send(Command::Export(job)).unwrap();
    assert!(matches!(next(&worker.events), Event::Need(_)));
    assert!(matches!(next(&worker.events),Event::Progress{status,..} if status.busy));
    let (mut unrelated, _) = png();
    unrelated.hash = AssetHash::new(&"c".repeat(64)).unwrap();
    worker.send(Command::Failure { ticket: ticket(scope(1), &unrelated), message: "unrelated display image failed".into() }).unwrap();
    assert!(matches!(next(&worker.events),Event::Failed{hash:Some(hash),..} if hash==unrelated.hash));
    assert!(matches!(next(&worker.events),Event::Progress{status,..} if status.busy));
    cancel.store(true, Ordering::Release);
    worker.send(Command::Cancel).unwrap();
    assert!(matches!(next(&worker.events),Event::Progress{status,..} if !status.busy));
}
