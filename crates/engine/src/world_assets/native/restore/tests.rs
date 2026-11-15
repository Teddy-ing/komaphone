use super::*;
use photocraft_scene::CanvasId;
use photocraft_scene::{
    OwnerId, Srgba, Stroke, StrokeId,
    shared::{AcceptedOp, DeviceId, Edit, WorldOp},
};

fn id(n: u32) -> String {
    format!("{n:032x}")
}
fn fixture(root: &Path) -> (Job, WorldState, Vec<u8>) {
    let source = CanvasId::new(&id(1)).unwrap();
    let owner = OwnerId::new(&id(2)).unwrap();
    let target = CanvasId::new(&id(3)).unwrap();
    let importer = OwnerId::new(&id(4)).unwrap();
    let device = DeviceId::new(&id(5)).unwrap();
    let mut world = WorldState::new(source, owner, "Portable source".into()).unwrap();
    world
        .apply(&AcceptedOp {
            revision: 1,
            op: WorldOp {
                id: OpId::new(&id(6)).unwrap(),
                canvas: source,
                author: owner,
                device,
                sequence: 1,
                authority_epoch: 1,
                action: Edit::AddStroke {
                    stroke: Stroke {
                        id: StrokeId::new(&id(7)).unwrap(),
                        canvas: source,
                        owner,
                        revision: 0,
                        anchor: Default::default(),
                        points: (0..8192).map(|n| [n as f64 * 0.01, n as f64 * 0.002]).collect(),
                        width: 2.,
                        color: Srgba { rgba: [0., 0., 0., 1.] },
                        erase: false,
                    },
                },
            },
        })
        .unwrap();
    let bytes = serde_json::to_vec(&serde_json::json!({"schema":1,"world":world,"assets":[]})).unwrap();
    assert!(bytes.len() > MAX_CHUNK_BYTES);
    let path = root.join("portable");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("world.json"), &bytes).unwrap();
    let expected = world.import_to(target, importer).unwrap();
    (
        Job {
            scope: Scope {
                account: crate::worlds::Account { endpoint: "https://example.invalid".into(), profile: importer, device },
                canvas: target,
                generation: 0,
            },
            action: WorldExport::Restore { path, canvas: target },
            files: None,
            cancel: Arc::new(AtomicBool::new(false)),
            commit_started: AtomicBool::new(false),
        },
        expected,
        bytes,
    )
}
fn summary(world: &WorldState) -> wire::CanvasSummary {
    wire::CanvasSummary {
        canvas: world.canvas,
        owner: world.owner,
        name: world.name.clone(),
        initial_name: world.replay_to(0).unwrap().name,
        import_origin: world.import_origin.clone(),
        revision: world.revision,
        epoch: 1,
    }
}

#[test]
fn uploads_actual_archive_chunks_without_original_canvas_or_owner_and_verifies_commit() {
    let root = tempfile::tempdir().unwrap();
    let (job, expected, original) = fixture(root.path());
    let (send, receive) = mpsc::sync_channel(4);
    let worker = std::thread::spawn(move || run(job, &send, &AtomicU64::new(0)));
    let mut received = Vec::new();
    let mut committed = false;
    loop {
        match receive.recv_timeout(Duration::from_secs(5)).unwrap() {
            Event::Import { action, reply, .. } => {
                let response = match *action {
                    wire::Action::ImportBegin { id, manifest_bytes, total_bytes, new_canvas, .. } => {
                        assert_eq!(new_canvas, expected.canvas);
                        assert_eq!(total_bytes, manifest_bytes);
                        wire::Response::ImportProgress { id, object: wire::ImportObject::Manifest {}, received: 0, total: manifest_bytes }
                    }
                    wire::Action::ImportChunk { id, object, offset, bytes } => {
                        assert_eq!(offset, received.len() as u64);
                        assert!(bytes.len() <= MAX_CHUNK_BYTES);
                        received.extend(bytes);
                        wire::Response::ImportProgress { id, object, received: received.len() as u64, total: original.len() as u64 }
                    }
                    wire::Action::ImportCommit { .. } => {
                        assert_eq!(received, original);
                        committed = true;
                        wire::Response::Canvas { info: summary(&expected) }
                    }
                    _ => panic!("Unexpected source lookup or upload action"),
                };
                reply.send(Ok(response)).unwrap();
            }
            Event::Progress { status, .. } if !status.busy => {
                assert!(committed);
                break;
            }
            Event::Progress { .. } => {}
            _ => panic!("Unexpected export event"),
        }
    }
    worker.join().unwrap().unwrap();
    assert_eq!(std::fs::read(root.path().join("portable/world.json")).unwrap(), original);
}

#[test]
fn retry_of_committed_destination_accepts_same_origin_without_reuploading() {
    let root = tempfile::tempdir().unwrap();
    let (job, expected, _) = fixture(root.path());
    let (send, receive) = mpsc::sync_channel(4);
    let worker = std::thread::spawn(move || run(job, &send, &AtomicU64::new(0)));
    let Event::Import { action, reply, .. } = receive.recv_timeout(Duration::from_secs(5)).unwrap() else { panic!("begin") };
    assert!(matches!(*action, wire::Action::ImportBegin { .. }));
    let mut info = summary(&expected);
    info.revision += 1; // A completed retry must not replace later accepted edits.
    reply.send(Ok(wire::Response::Canvas { info })).unwrap();
    assert!(matches!(receive.recv_timeout(Duration::from_secs(5)).unwrap(), Event::Progress { status, .. } if !status.busy));
    worker.join().unwrap().unwrap();
}

#[test]
fn account_change_cancels_waiting_upload_without_sending_cleanup_as_new_account() {
    let root = tempfile::tempdir().unwrap();
    let (job, _, _) = fixture(root.path());
    let generation = Arc::new(AtomicU64::new(0));
    let changed = generation.clone();
    let (send, receive) = mpsc::sync_channel(4);
    let worker = std::thread::spawn(move || run(job, &send, &generation));
    let Event::Import { reply, .. } = receive.recv_timeout(Duration::from_secs(5)).unwrap() else { panic!("begin") };
    changed.store(1, Ordering::Release);
    assert!(worker.join().unwrap().is_err());
    assert!(reply.send(Err("late previous account response".into())).is_err());
    assert!(receive.try_recv().is_err());
}

#[test]
fn cancellation_before_commit_requests_staging_cleanup_and_keeps_archive() {
    let root = tempfile::tempdir().unwrap();
    let (job, _, original) = fixture(root.path());
    let cancel = Arc::clone(&job.cancel);
    let (send, receive) = mpsc::sync_channel(4);
    let worker = std::thread::spawn(move || run(job, &send, &AtomicU64::new(0)));
    let Event::Import { action, reply, .. } = receive.recv_timeout(Duration::from_secs(5)).unwrap() else { panic!("begin") };
    assert!(matches!(*action, wire::Action::ImportBegin { .. }));
    cancel.store(true, Ordering::Release);
    let Event::Import { action, reply: cleanup, .. } = receive.recv_timeout(Duration::from_secs(5)).unwrap() else { panic!("cancel") };
    assert!(matches!(*action, wire::Action::ImportCancel { .. }));
    cleanup.send(Ok(wire::Response::Done)).unwrap();
    assert!(worker.join().unwrap().is_err());
    assert!(reply.send(Err("late response".into())).is_err());
    assert!(receive.try_recv().is_err());
    assert_eq!(std::fs::read(root.path().join("portable/world.json")).unwrap(), original);
}

#[test]
fn restore_start_does_not_require_an_active_source_and_switching_cancels_it() {
    let root = tempfile::tempdir().unwrap();
    let (job, _, _) = fixture(root.path());
    let mut session = Session::new();
    session.enable_world_assets(root.path().join("cache")).unwrap();
    session.worlds.account = Some(job.scope.account.clone());
    assert!(session.worlds.active.is_none());
    session.world_export(job.action).unwrap();
    assert!(session.worlds.assets.export.as_ref().unwrap().busy);
    let cancel = session.worlds.assets.inner.export_cancel.as_ref().unwrap().clone();
    session.worlds.assets.reset();
    assert!(cancel.load(Ordering::Acquire));
    assert!(session.worlds.assets.inner.import_reply.is_none());
}

#[test]
fn malformed_ack_and_changed_origin_cannot_claim_success() {
    let root = tempfile::tempdir().unwrap();
    let (_, expected, _) = fixture(root.path());
    let id = OpId::new(&id(3)).unwrap();
    let object = wire::ImportObject::Manifest {};
    assert!(uploaded(wire::Response::ImportProgress { id, object: object.clone(), received: 2, total: 1 }, id, &object, 0, 1).is_err());
    let mut info = summary(&expected);
    info.import_origin = None;
    assert!(completed(&info, &expected).is_err());
}

#[test]
fn asset_upload_is_chunked_and_resumes_from_acknowledged_prefix() {
    let root = tempfile::tempdir().unwrap();
    let (job, _, _) = fixture(root.path());
    let original: Vec<u8> = (0..MAX_CHUNK_BYTES * 3 + 11).map(|n| (n % 251) as u8).collect();
    let path = root.path().join("asset.png");
    std::fs::write(&path, &original).unwrap();
    let hash = world_media::media_asset_hash(&original).unwrap();
    let total = original.len() as u64;
    let (send, receive) = mpsc::sync_channel(4);
    let worker = std::thread::spawn(move || {
        upload_file(&job, &path, wire::ImportObject::Asset { hash }, OpId::new(&id(3)).unwrap(), total, 0, &send, &AtomicU64::new(0))
    });
    let mut first = true;
    let mut expected_offset = 0;
    while expected_offset < total {
        let Event::Import { action, reply, .. } = receive.recv_timeout(Duration::from_secs(5)).unwrap() else { panic!("chunk") };
        let wire::Action::ImportChunk { id, object, offset, bytes } = *action else { panic!("asset chunk") };
        assert_eq!(object, wire::ImportObject::Asset { hash });
        assert_eq!(offset, expected_offset);
        assert!(bytes.len() <= MAX_CHUNK_BYTES);
        assert_eq!(bytes, original[offset as usize..offset as usize + bytes.len()]);
        expected_offset = offset + bytes.len() as u64;
        if first {
            expected_offset += MAX_CHUNK_BYTES as u64;
            first = false;
        }
        reply.send(Ok(wire::Response::ImportProgress { id, object, received: expected_offset, total })).unwrap();
    }
    worker.join().unwrap().unwrap();
}

#[test]
fn archive_inspection_is_cancellable_and_refuses_extra_objects() {
    let root = tempfile::tempdir().unwrap();
    let (job, _, _) = fixture(root.path());
    let path = job.action.path();
    assert!(world_media::inspect_archive_for_upload(path, &mut || false).is_err());
    std::fs::write(path.join("unrelated.txt"), b"not portable art").unwrap();
    assert!(world_media::inspect_archive_for_upload(path, &mut || true).is_err());
}

#[test]
fn incomplete_imported_replay_cannot_be_published_as_a_portable_archive() {
    let root = tempfile::tempdir().unwrap();
    let (_, expected, _) = fixture(root.path());
    let partial = expected.replay_to(0).unwrap();
    partial.validate().unwrap();
    let destination = root.path().join("incomplete.komworld");
    let failure = world_media::export_archive(&partial, &destination, &mut |_| panic!("No asset read before completeness check"), |_, _| true).unwrap_err();
    assert!(failure.to_string().contains("incomplete"));
    assert!(!destination.exists());
    assert_eq!(partial.revision, 0);
}
