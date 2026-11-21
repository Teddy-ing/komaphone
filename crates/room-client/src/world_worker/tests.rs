use super::*;
use photocraft_scene::shared::{
    AcceptedOp, MediaId,
    media::{AssetHash, Credit, MediaFrame, MediaPlacement, MediaSource, Provenance},
};

pub(super) fn fixture() -> (tempfile::TempDir, Coordinator, CanvasId) {
    let root = tempfile::tempdir().unwrap();
    let mut queue = WorldOutbox::open(root.path().join("queue.db")).unwrap();
    let scope =
        WorldScope::new("https://service.invalid", OwnerId::new(&format!("{:032x}", 1)).unwrap(), DeviceId::new(&format!("{:032x}", 2)).unwrap()).unwrap();
    let canvas = CanvasId::new(&format!("{:032x}", 3)).unwrap();
    let lease = queue.activate(scope.clone()).unwrap();
    queue.grant(&lease, canvas, 1, 1).unwrap();
    let world = WorldState::new(canvas, scope.profile, "Original".into()).unwrap();
    queue.save_snapshot(&lease, &world).unwrap();
    let info = wire::CanvasSummary {
        canvas,
        owner: scope.profile,
        name: world.name.clone(),
        import_origin: None,
        initial_name: world.name.clone(),
        revision: 0,
        epoch: 1,
    };
    let mut replicas = BTreeMap::new();
    replicas.insert(canvas, WorldReplica::open(info, Some(world)).unwrap());
    let now = Instant::now();
    (
        root,
        Coordinator {
            auth_fence: Arc::new(AuthFence::default()),
            auth_ticket: 0,
            queue,
            access: Some(WorldAccess { auth: None, scope, credential: "test-only-secret".into() }),
            lease: Some(lease),
            generation: 1,
            replicas,
            checkpoint_transfers: BTreeMap::new(),
            verifying_checkpoints: BTreeMap::new(),
            ckpt_jobs_out: VecDeque::new(),
            selected: Some(canvas),
            requests: VecDeque::new(),
            recovery_resume: BTreeMap::new(),
            outbound: VecDeque::new(),
            inflight: false,
            next_poll: now,
            network_after: now,
            cached_directory: None,
            directory_start: None,
            directory_scan: None,
            presence: presence::Lane::default(),
        },
        canvas,
    )
}
#[test]
fn opening_partial_import_cache_still_requests_authoritative_info() {
    use photocraft_scene::shared::{OpId, WorldOp};
    let (_root, mut state, canvas) = fixture();
    let lease = state.lease().unwrap().clone();
    let mut source = state.queue.load_snapshot(&lease, canvas).unwrap().unwrap();
    source
        .apply(&AcceptedOp {
            revision: 1,
            op: WorldOp {
                id: OpId::new(&format!("{:032x}", 90)).unwrap(),
                canvas,
                author: source.owner,
                device: lease.scope().device,
                sequence: 1,
                authority_epoch: 1,
                action: Edit::Rename { name: "Imported title".into(), expected_revision: 0 },
            },
        })
        .unwrap();
    let target = CanvasId::new(&format!("{:032x}", 91)).unwrap();
    let imported = source.import_to(target, lease.scope().profile).unwrap();
    let partial = imported.replay_to(0).unwrap();
    state.queue.grant(&lease, target, 1, 1).unwrap();
    state.queue.save_snapshot(&lease, &partial).unwrap();
    state.command(77, Command::Open(target)).unwrap();
    assert!(state.requests.iter().any(|(sequence, action)| *sequence == 77 && matches!(action, Action::Info {canvas} if *canvas == target)));
    assert!(!state.replicas.contains_key(&target));
    assert_eq!(state.queue.load_snapshot(&lease, target).unwrap().unwrap().canonical_hash().unwrap(), partial.canonical_hash().unwrap());
}

fn media(state: &Coordinator, canvas: CanvasId) -> Edit {
    let owner = state.lease().unwrap().scope().profile;
    let hash = AssetHash::new(&"1".repeat(64)).unwrap();
    Edit::PlaceMedia {
        media: Box::new(MediaPlacement {
            id: MediaId::new(&format!("{:032x}", 10)).unwrap(),
            canvas,
            owner,
            anchor: Default::default(),
            offset: [0., 0.],
            width: 10.,
            height: 10.,
            frames: vec![MediaFrame { hash, byte_len: 10, width: 1, height: 1, duration_ms: 100 }],
            looping: false,
            provenance: Provenance {
                source: MediaSource::Canvas { canvas: CanvasId::new(&format!("{:032x}", 20)).unwrap(), media: MediaId::new(&format!("{:032x}", 21)).unwrap() },
                source_revision: 1,
                artifact_hash: hash,
                authors: vec![Credit { profile: owner, label: "Original author".into() }],
            },
        }),
    }
}
fn info_reply(state: &Coordinator, canvas: CanvasId, sequence: u64) -> Reply {
    Reply {
        generation: state.generation,
        sequence,
        action: Action::Info { canvas },
        result: Ok(Response::Canvas { info: state.replicas[&canvas].info().clone() }),
    }
}
#[test]
fn wrong_canvas_accept_response_cannot_install_a_replica_or_cached_grant() {
    let (_root, mut state, canvas) = fixture();
    let foreign = CanvasId::new(&format!("{:032x}", 999)).unwrap();
    let mut info = state.replicas[&canvas].info().clone();
    info.canvas = foreign;
    let response = Reply {
        generation: state.generation,
        sequence: 17,
        action: Action::Accept { canvas, id: OpId::new(&format!("{:032x}", 998)).unwrap(), secret: "synthetic-invitation".into() },
        result: Ok(Response::Canvas { info }),
    };
    assert!(state.reply(response).is_err());
    assert!(!state.replicas.contains_key(&foreign));
    assert!(state.queue.cached_grant(state.lease().unwrap(), foreign).unwrap().is_none());
    assert!(state.outbound.is_empty());
}

fn rejected_source(state: &mut Coordinator, canvas: CanvasId) -> WorldOp {
    state.command(1, Command::Edit { canvas, action: media(state, canvas) }).unwrap();
    let lease = state.lease().unwrap().clone();
    let original = state.queue.pending(&lease, 1).unwrap()[0].operation.clone();
    let action = upload_action(original.clone()).unwrap();
    assert!(matches!(action, Action::RemixCanvas { .. }));
    assert!(
        state
            .reply(Reply {
                generation: state.generation,
                sequence: 0,
                action,
                result: Err(Error::Rejected { code: "unauthorized".into(), message: "source access removed".into() })
            })
            .is_err()
    );
    original
}

#[test]
fn source_only_remix_revocation_keeps_destination_and_requires_explicit_fresh_catchup_to_resume() {
    let (_root, mut state, canvas) = fixture();
    let original = rejected_source(&mut state, canvas);
    let lease = state.lease().unwrap().clone();
    assert!(state.replicas.contains_key(&canvas));
    assert!(!state.outbound.iter().any(|event| matches!(event, Event::Revoked { .. })));
    assert_eq!(state.queue.recovery(&lease, 10).unwrap()[0].operation, original);
    assert_eq!(state.queue.recovery(&lease, 10).unwrap()[0].reason, RecoveryReason::Conflict);
    state.reply(info_reply(&state, canvas, 0)).unwrap();
    assert_eq!(state.queue.cached_grant(&lease, canvas).unwrap(), None);
    state.command(88, Command::RecoveryPage { after: None, limit: 1 }).unwrap();
    assert!(state.outbound.iter().any(|event| matches!(event,Event::RecoveryPage{sequence:88,page} if page.items.len()==1)));
    state.command(99, Command::ResumeRecovery { canvas }).unwrap();
    state.reply(info_reply(&state, canvas, 0)).unwrap();
    assert!(!state.outbound.iter().any(|event| matches!(event, Event::RecoveryResumed { .. })));
    let mut accepted = original.clone();
    accepted.id = OpId::new(&format!("{:032x}", 99)).unwrap();
    accepted.action = Edit::Rename { name: "Accepted elsewhere".into(), expected_revision: 0 };
    let event = AcceptedOp { revision: 1, op: accepted };
    let mut fresh = state.replicas[&canvas].info().clone();
    fresh.revision = 1;
    fresh.name = "Accepted elsewhere".into();
    state.reply(Reply { generation: state.generation, sequence: 99, action: Action::Info { canvas }, result: Ok(Response::Canvas { info: fresh }) }).unwrap();
    assert!(!state.outbound.iter().any(|event| matches!(event, Event::RecoveryResumed { .. })));
    state
        .reply(Reply {
            generation: state.generation,
            sequence: 0,
            action: Action::Events { canvas, after: 0, through: Some(1), limit: 128 },
            result: Ok(Response::Events { events: vec![event], through: 1, next: None }),
        })
        .unwrap();
    assert!(state.outbound.iter().any(|event| matches!(event,Event::RecoveryResumed{sequence:99,canvas:got} if *got==canvas)));
    assert_eq!(state.queue.cached_grant(&lease, canvas).unwrap(), Some((1, 2)));
    assert_eq!(state.queue.recovery(&lease, 10).unwrap()[0].operation, original);
    let fresh = state.queue.enqueue(&lease, canvas, Edit::Rename { name: "Explicit new edit".into(), expected_revision: 1 }).unwrap();
    assert_eq!(fresh.sequence, 2);
    assert_ne!(fresh.id, original.id);
}

#[test]
fn independent_destination_denial_revokes_and_account_switch_discards_pending_resume() {
    let (_root, mut state, canvas) = fixture();
    rejected_source(&mut state, canvas);
    let lease = state.lease().unwrap().clone();
    assert!(
        state
            .reply(Reply {
                generation: state.generation,
                sequence: 0,
                action: Action::Info { canvas },
                result: Err(Error::Rejected { code: "unauthorized".into(), message: "destination removed".into() })
            })
            .is_err()
    );
    assert!(state.outbound.iter().any(|event| matches!(event,Event::Revoked{canvas:got} if *got==canvas)));
    assert!(state.queue.resume_after_recovery(&lease, canvas, 1, 1).is_err());
    let (_root, mut state, canvas) = fixture();
    rejected_source(&mut state, canvas);
    state.command(99, Command::ResumeRecovery { canvas }).unwrap();
    let stale = info_reply(&state, canvas, 99);
    state.access(1000, None).unwrap();
    state.reply(stale).unwrap();
    assert!(state.recovery_resume.is_empty());
    assert!(!state.outbound.iter().any(|event| matches!(event, Event::RecoveryResumed { .. })));
}

#[test]
fn duplicate_access_ack_preserves_saved_view_and_operation_receipts() {
    let (_root, mut state, canvas) = fixture();
    state.command(10, Command::Edit { canvas, action: Edit::Rename { name: "Saved locally".into(), expected_revision: 0 } }).unwrap();
    state
        .command(11, Command::SaveView { canvas, state: ViewerState { camera: photocraft_scene::coordinates::home(canvas), bookmarks: vec![], review: None } })
        .unwrap();
    let generation = state.generation;
    let access = state.access.as_ref().unwrap().copy_for_worker();
    state.command(12, Command::Access(Some(access))).unwrap();
    assert_eq!(state.generation, generation);
    assert!(state.outbound.iter().any(|event| matches!(event, Event::Saved { sequence: 10, .. })));
    assert!(state.outbound.iter().any(|event| matches!(event, Event::ViewSaved { sequence: 11, .. })));
    assert!(state.outbound.iter().any(|event| matches!(event, Event::Activated { sequence: 12, scope: Some(_) })));
    assert_eq!(state.queue.pending(state.lease().unwrap(), 10).unwrap().len(), 1);
}

#[test]
fn activation_ack_identifies_each_requested_switch_even_when_returning_to_previous_account() {
    let (_root, mut state, _) = fixture();
    let first = state.access.as_ref().unwrap().copy_for_worker();
    let scope =
        WorldScope::new(&first.scope.endpoint, OwnerId::new(&format!("{:032x}", 91)).unwrap(), DeviceId::new(&format!("{:032x}", 92)).unwrap()).unwrap();
    let second = WorldAccess { auth: None, scope: scope.clone(), credential: "another-synthetic-session".into() };
    state.command(20, Command::Access(Some(first.copy_for_worker()))).unwrap();
    let buffered = state.outbound.pop_back().unwrap();
    state.command(21, Command::Access(Some(second))).unwrap();
    let middle = state.outbound.pop_front().unwrap();
    state.command(22, Command::Access(Some(first.copy_for_worker()))).unwrap();
    assert!(matches!(buffered, Event::Activated { sequence: 20, scope: Some(value) } if value == first.scope));
    assert!(matches!(middle, Event::Activated { sequence: 21, scope: Some(value) } if value == scope));
    assert!(matches!(state.outbound.pop_front().unwrap(), Event::Activated { sequence: 22, scope: Some(value) } if value == first.scope));
}

fn directory_item(state: &Coordinator, number: u64) -> wire::CanvasSummary {
    wire::CanvasSummary {
        canvas: CanvasId::new(&format!("{number:032x}")).unwrap(),
        owner: state.lease().unwrap().scope().profile,
        name: format!("Canvas {number}"),
        import_origin: None,
        initial_name: format!("Canvas {number}"),
        epoch: 1,
        revision: 0,
    }
}
fn directory_reply(state: &Coordinator, sequence: u64, after: Option<CanvasId>, items: Vec<wire::CanvasSummary>, next: Option<CanvasId>) -> Reply {
    Reply { generation: state.generation, sequence, action: Action::List { after, limit: 128 }, result: Ok(Response::Canvases { items, next }) }
}
fn directory_first(state: &mut Coordinator, sequence: u64) -> CanvasId {
    let item = directory_item(state, 6);
    let cursor = item.canvas;
    state.command(sequence, Command::Request(Action::List { after: None, limit: 128 })).unwrap();
    state.reply(directory_reply(state, sequence, None, vec![item], Some(cursor))).unwrap();
    cursor
}

#[test]
fn complete_directory_removal_survives_offline_restart_without_deleting_original_art_or_operations() {
    let (root, mut state, canvas) = fixture();
    let lease = state.lease().unwrap().clone();
    let scope = lease.scope().clone();
    let before = state.queue.load_snapshot(&lease, canvas).unwrap().unwrap().canonical_hash().unwrap();
    let original = state.queue.enqueue(&lease, canvas, Edit::Rename { name: "Unsent original".into(), expected_revision: 0 }).unwrap();
    let cursor = directory_first(&mut state, 100);
    assert!(state.queue.cached_grant(&lease, canvas).unwrap().is_some(), "A partial list cannot revoke a grant");
    assert_eq!(state.queue.pending(&lease, 10).unwrap()[0].operation, original);
    state.command(101, Command::Request(Action::List { after: Some(cursor), limit: 128 })).unwrap();
    let last = directory_item(&state, 7);
    state.reply(directory_reply(&state, 101, Some(cursor), vec![last], None)).unwrap();
    assert!(state.selected.is_none());
    assert!(!state.replicas.contains_key(&canvas));
    assert!(state.outbound.iter().any(|event| matches!(event, Event::Revoked {canvas: id} if *id == canvas)));
    drop(state);
    let mut queue = WorldOutbox::open(root.path().join("queue.db")).unwrap();
    let lease = queue.activate(scope.clone()).unwrap();
    assert!(queue.cached_summaries(&lease, None, 128).unwrap().items.is_empty());
    assert!(queue.cached_grant(&lease, canvas).unwrap().is_none());
    assert_eq!(queue.load_snapshot(&lease, canvas).unwrap().unwrap().canonical_hash().unwrap(), before);
    assert_eq!(queue.recovery(&lease, 10).unwrap()[0].operation, original);
    assert_eq!(queue.recovery(&lease, 10).unwrap()[0].reason, RecoveryReason::AccessRemoved);
    assert!(queue.grant(&lease, canvas, 1, 1).is_err());
    queue.grant(&lease, canvas, 2, 1).unwrap();
    assert_eq!(queue.cached_summaries(&lease, None, 128).unwrap().items[0].epoch, 2);
    let new = queue.enqueue(&lease, canvas, Edit::Rename { name: "New invitation".into(), expected_revision: 0 }).unwrap();
    assert_eq!(new.authority_epoch, 2);
    assert_ne!(new.id, original.id);
    assert_eq!(queue.recovery(&lease, 10).unwrap()[0].operation, original);
    let foreign = WorldScope::new(&scope.endpoint, OwnerId::new(&format!("{:032x}", 900)).unwrap(), scope.device).unwrap();
    let other = queue.activate(foreign).unwrap();
    assert!(queue.cached_summaries(&other, None, 128).unwrap().items.is_empty());
}

#[test]
fn failed_malformed_and_interleaved_directory_pages_never_revoke_saved_access() {
    for mode in 0..4 {
        let (_root, mut state, canvas) = fixture();
        let lease = state.lease().unwrap().clone();
        let cursor = directory_first(&mut state, 100);
        state.command(101, Command::Request(Action::List { after: Some(cursor), limit: 128 })).unwrap();
        match mode {
            0 => {
                let mut reply = directory_reply(&state, 101, Some(cursor), vec![], None);
                reply.result = Err(Error::Unavailable);
                assert!(state.reply(reply).is_err());
            }
            1 => {
                let repeated = directory_item(&state, 6);
                assert!(state.reply(directory_reply(&state, 101, Some(cursor), vec![repeated], None)).is_err());
            }
            2 => {
                state.command(102, Command::Request(Action::List { after: None, limit: 128 })).unwrap();
                state.reply(directory_reply(&state, 101, Some(cursor), vec![], None)).unwrap();
            }
            _ => {
                let mut reply = directory_reply(&state, 101, Some(cursor), vec![], None);
                reply.result = Ok(Response::Done);
                assert!(state.reply(reply).is_err());
            }
        }
        assert!(state.queue.cached_grant(&lease, canvas).unwrap().is_some());
        assert_eq!(state.queue.cached_summaries(&lease, None, 128).unwrap().items.len(), 1);
        assert!(!state.outbound.iter().any(|event| matches!(event, Event::Revoked { .. })));
    }
}

#[test]
fn directory_scan_cannot_revoke_new_or_independently_refreshed_grants_or_cross_accounts() {
    let (_root, mut state, canvas) = fixture();
    let lease = state.lease().unwrap().clone();
    let cursor = directory_first(&mut state, 100);
    state.queue.grant(&lease, canvas, 2, 1).unwrap();
    let newer = directory_item(&state, 8);
    state.queue.grant(&lease, newer.canvas, 1, 1).unwrap();
    state.command(101, Command::Request(Action::List { after: Some(cursor), limit: 128 })).unwrap();
    state.reply(directory_reply(&state, 101, Some(cursor), vec![], None)).unwrap();
    assert_eq!(state.queue.cached_grant(&lease, canvas).unwrap(), Some((2, 1)));
    assert_eq!(state.queue.cached_grant(&lease, newer.canvas).unwrap(), Some((1, 1)));

    let cursor = directory_first(&mut state, 102);
    // A fresh same-epoch authenticated Info is newer evidence than an older
    // paginated directory prefix, so it removes this candidate from the scan.
    state.directory_refreshed(canvas);
    state.command(103, Command::Request(Action::List { after: Some(cursor), limit: 128 })).unwrap();
    state.reply(directory_reply(&state, 103, Some(cursor), vec![], None)).unwrap();
    assert_eq!(state.queue.cached_grant(&lease, canvas).unwrap(), Some((2, 1)));

    let cursor = directory_first(&mut state, 104);
    state.command(105, Command::Request(Action::List { after: Some(cursor), limit: 128 })).unwrap();
    let old = directory_reply(&state, 105, Some(cursor), vec![], None);
    let access = state.access.as_ref().unwrap().copy_for_worker();
    state.access(106, None).unwrap();
    state.reply(old).unwrap();
    state.access(107, Some(access)).unwrap();
    assert_eq!(state.queue.cached_grant(state.lease().unwrap(), canvas).unwrap(), Some((2, 1)));
}

#[test]
fn account_pause_bypasses_full_queue_and_preserves_durable_receipts() {
    let (tx, commands) = mpsc::sync_channel(1);
    let (events, rx) = mpsc::sync_channel(4);
    let worker = WorldWorker {
        alive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        tx,
        rx,
        sequence: AtomicU64::new(1),
        _join: std::thread::spawn(|| {}),
        auth_fence: Arc::new(AuthFence::default()),
        priority: Arc::new(Priority::default()),
    };
    worker.send(Command::Retry).unwrap();
    let old = worker.auth_fence.ticket();
    assert!(worker.send(Command::Retry).is_err());
    let sequence = worker.send(Command::Access(None)).unwrap();
    assert!(worker.auth_fence.paused());
    assert!(!worker.auth_fence.allowed(old));
    assert_eq!(worker.priority.take().unwrap().sequence, sequence);
    assert_eq!(commands.try_recv().unwrap().ticket, old);
    let canvas = CanvasId::new(&"1".repeat(32)).unwrap();
    assert!(events.try_send(Stamped { ticket: old, value: Event::CachedCanvases { items: vec![] } }).is_ok());
    assert!(events.try_send(Stamped { ticket: old, value: Event::ViewSaved { sequence: 1, canvas } }).is_ok());
    assert!(matches!(worker.poll().unwrap(), Event::ViewSaved { sequence: 1, .. }));
    assert!(matches!(worker.poll(), Err(TryRecvError::Empty)));
    worker.send(Command::Retry).unwrap();
    assert!(worker.auth_fence.paused(), "Retry cannot restore account authorization");
}

#[test]
fn paused_access_cannot_activate_or_schedule_queued_http() {
    let (_root, mut state, canvas) = fixture();
    let fence = Arc::new(AuthFence::default());
    let access = state.access.as_ref().unwrap().copy_for_worker().with_auth_fence(Arc::clone(&fence), 0, false);
    let copy = access.copy_for_worker();
    fence.pause();
    assert!(state.access(2, Some(copy)).is_err());
    assert!(access.verify_identity(None).is_err());
    assert!(access.thumbnail_reader(None).unwrap().scene(state.replicas[&canvas].info(), &mut || true).is_err());
    state.auth_fence.pause();
    let (sender, jobs) = mpsc::sync_channel(1);
    state.schedule(&sender).unwrap();
    state.schedule_presence(&[sender], Instant::now()).unwrap();
    assert!(jobs.try_recv().is_err());
}

#[test]
fn switching_accounts_retains_saved_local_operation_acknowledgments() {
    let (_root, mut state, canvas) = fixture();
    state.command(10, Command::Edit { canvas, action: Edit::Rename { name: "Kept locally".into(), expected_revision: 0 } }).unwrap();
    state.access(11, None).unwrap();
    assert!(state.outbound.iter().any(|event| matches!(event, Event::Saved { sequence: 10, .. })));
    assert!(!state.outbound.iter().any(|event| matches!(event, Event::State { .. })));
}
