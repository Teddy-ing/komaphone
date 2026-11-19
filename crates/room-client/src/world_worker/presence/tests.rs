use super::super::{Command, Edit, tests::fixture};
use super::*;
use std::sync::mpsc::{self, Receiver};
fn channels() -> (Vec<SyncSender<Job>>, Vec<Receiver<Job>>) {
    (0..SLOTS).map(|_| mpsc::sync_channel(1)).unzip()
}
fn response(job: Job, result: Result<Response>) -> Reply {
    Reply { generation: job.generation, sequence: job.sequence, action: job.action, result }
}
fn next(receivers: &[Receiver<Job>]) -> (usize, Job) {
    receivers.iter().enumerate().find_map(|(slot, rx)| rx.try_recv().ok().map(|job| (slot, job))).unwrap()
}
fn sample(canvas: CanvasId, x: f64) -> Action {
    Action::Presence {
        canvas,
        epoch: 1,
        camera: photocraft_scene::coordinates::home(canvas),
        cursor: None,
        preview: vec![[x, 20.0]],
        stroke: Some(wire::PreviewStyle {
            id: StrokeId::new(&"a".repeat(32)).unwrap(),
            anchor: Default::default(),
            width: 3.0,
            color: photocraft_scene::Srgba { rgba: [0., 0., 0., 1.] },
            erase: false,
            brush: None,
            samples: None,
        }),
    }
}
fn ready(state: &mut Coordinator, senders: &[SyncSender<Job>], receivers: &[Receiver<Job>], now: Instant) {
    if state.presence.pending.is_empty() {
        state.presence_enqueue(0, Action::Peers { canvas: state.selected.unwrap() });
    }
    state.schedule_presence(senders, now).unwrap();
    let (slot, job) = next(receivers);
    let Action::PresenceOpen { canvas, epoch, nonce } = job.action.clone() else { panic!("expected stream negotiation") };
    let stream = OpId::new(&"b".repeat(32)).unwrap();
    state.presence_reply(slot, response(job, Ok(Response::PresenceReady { canvas, epoch, nonce, stream })), now);
    state.presence.pending.retain(|(sequence, _)| *sequence != 0);
}
fn snapshot(job: Job, observation: u64) -> Reply {
    let (canvas, stream) = match job.action {
        Action::PresenceUpdate { canvas, stream, .. } | Action::PresenceRead { canvas, stream } => (canvas, stream),
        _ => panic!("expected ordered presence"),
    };
    response(job, Ok(Response::PresenceSnapshot { canvas, stream, observation, peers: vec![] }))
}
fn dispatch(state: &mut Coordinator, senders: &[SyncSender<Job>], receivers: &[Receiver<Job>], sequence: u64, action: Action, at: Instant) -> (usize, Job) {
    state.command(sequence, Command::Request(action)).unwrap();
    state.schedule_presence(senders, at).unwrap();
    next(receivers)
}
#[test]
fn three_physical_slots_latest_waiting_actions_and_every_receipt_are_bounded() {
    let (_root, mut state, canvas) = fixture();
    let now = Instant::now();
    let (senders, receivers) = channels();
    ready(&mut state, &senders, &receivers, now);
    let mut held = Vec::new();
    for n in 1..=3 {
        held.push(dispatch(&mut state, &senders, &receivers, n, sample(canvas, n as f64), now + CADENCE * n as u32));
    }
    for n in 4..=100 {
        state.command(n, Command::Request(sample(canvas, n as f64))).unwrap();
    }
    for n in 101..=120 {
        state.command(n, Command::Request(Action::Peers { canvas })).unwrap();
    }
    assert_eq!(state.presence.slots.iter().flatten().count(), 3);
    assert_eq!(state.presence.pending.len(), 2);
    state.schedule_presence(&senders, now + Duration::from_secs(1)).unwrap();
    assert!(receivers.iter().all(|rx| rx.try_recv().is_err()));
    let (slot, job) = held.remove(0);
    state.presence_reply(slot, snapshot(job, 1), now + Duration::from_secs(1));
    state.schedule_presence(&senders, now + Duration::from_secs(1)).unwrap();
    let latest = next(&receivers);
    assert_eq!(latest.1.sequence, 100);
    assert!(matches!(&latest.1.action,Action::PresenceUpdate {serial:4,preview,..} if preview==&vec![[100.,20.]]));
    held.push(latest);
    for (slot, job) in held {
        state.presence_reply(slot, snapshot(job, 2), now + Duration::from_secs(1));
    }
    state.schedule_presence(&senders, now + Duration::from_secs(2)).unwrap();
    let (slot, job) = next(&receivers);
    assert_eq!(job.sequence, 120);
    state.presence_reply(slot, snapshot(job, 3), now + Duration::from_secs(2));
    let mut receipts = state
        .outbound
        .iter()
        .filter_map(|event| match event {
            Event::Response { sequence, .. } | Event::PresenceDropped { sequence } => Some(*sequence),
            _ => None,
        })
        .collect::<Vec<_>>();
    receipts.sort_unstable();
    assert_eq!(receipts, (1..=120).collect::<Vec<_>>());
}
#[test]
fn snapshot_recency_uses_service_observation_not_dispatch_or_completion_order() {
    let (_root, mut state, canvas) = fixture();
    let now = Instant::now();
    let (senders, receivers) = channels();
    ready(&mut state, &senders, &receivers, now);
    let a = dispatch(&mut state, &senders, &receivers, 10, sample(canvas, 1.), now + CADENCE);
    let b = dispatch(&mut state, &senders, &receivers, 11, sample(canvas, 2.), now + CADENCE * 2);
    let c = dispatch(&mut state, &senders, &receivers, 12, Action::Peers { canvas }, now + CADENCE * 3);
    state.presence_reply(b.0, snapshot(b.1, 3), now);
    state.presence_reply(c.0, snapshot(c.1, 2), now);
    // Earlier-dispatched request arrived at the service last, so its snapshot is newest.
    state.presence_reply(a.0, snapshot(a.1, 4), now);
    assert_eq!(state.presence.stream.as_ref().unwrap().observation, 4);
    assert_eq!(
        state
            .outbound
            .iter()
            .filter_map(|e| match e {
                Event::Response { sequence, .. } => Some(*sequence),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![11, 10]
    );
    assert!(state.outbound.iter().any(|e| matches!(e, Event::PresenceDropped { sequence: 12 })));
}
#[test]
fn delayed_error_cannot_back_off_after_a_newer_success_or_cross_streams() {
    let (_root, mut state, canvas) = fixture();
    let now = Instant::now();
    let (senders, receivers) = channels();
    ready(&mut state, &senders, &receivers, now);
    let a = dispatch(&mut state, &senders, &receivers, 10, sample(canvas, 1.), now + CADENCE);
    let b = dispatch(&mut state, &senders, &receivers, 11, sample(canvas, 2.), now + CADENCE * 2);
    state.presence_reply(b.0, snapshot(b.1, 1), now);
    state.presence_reply(a.0, response(a.1, Err(Error::Unavailable)), now);
    assert!(state.presence.retry_after.is_none());
    let a = dispatch(&mut state, &senders, &receivers, 12, sample(canvas, 3.), now + CADENCE * 3);
    let b = dispatch(&mut state, &senders, &receivers, 13, sample(canvas, 4.), now + CADENCE * 4);
    state.presence_reply(a.0, response(a.1, Err(Error::Rejected { code: "presence_stream".into(), message: "expired".into() })), now);
    assert!(state.presence.stream.is_none());
    ready(&mut state, &senders, &receivers, now + CADENCE * 5);
    // The fake negotiation must issue a different token, just as the service does.
    state.presence.stream.as_mut().unwrap().id = OpId::new(&"c".repeat(32)).unwrap();
    state.presence_reply(b.0, response(b.1, Err(Error::Rejected { code: "session".into(), message: "old".into() })), now);
    assert!(!state.presence.paused);
    assert!(state.presence.stream.is_some());
}
#[test]
fn canvas_a_b_a_and_account_switch_retire_old_requests_without_crossing_scope() {
    let (_root, mut state, canvas) = fixture();
    let now = Instant::now();
    let (senders, receivers) = channels();
    ready(&mut state, &senders, &receivers, now);
    let old = dispatch(&mut state, &senders, &receivers, 10, sample(canvas, 1.), now + CADENCE);
    state.command(11, Command::Request(sample(canvas, 2.))).unwrap();
    let other = CanvasId::new(&"d".repeat(32)).unwrap();
    state.command(12, Command::Open(other)).unwrap();
    state.command(13, Command::Open(canvas)).unwrap();
    state.presence_reply(old.0, snapshot(old.1, 900), now);
    assert!(state.presence.stream.is_none());
    assert!(!state.outbound.iter().any(|e| matches!(e, Event::Response { .. })));
    ready(&mut state, &senders, &receivers, now + CADENCE * 2);
    let old = dispatch(&mut state, &senders, &receivers, 14, sample(canvas, 3.), now + CADENCE * 3);
    state.access(15, None).unwrap();
    state.presence_reply(old.0, response(old.1, Err(Error::Rejected { code: "session".into(), message: "old".into() })), now);
    assert!(!state.presence.paused);
    assert!(state.presence.slots.iter().all(Option::is_none));
}
#[test]
fn shared_cadence_stays_below_rate_bucket_and_does_not_catch_up_in_bursts() {
    let (_root, mut state, canvas) = fixture();
    let now = Instant::now();
    let (senders, receivers) = channels();
    ready(&mut state, &senders, &receivers, now);
    let mut times = vec![0];
    let mut observation = 0;
    for ms in 1..60_000u64 {
        state.presence_enqueue(ms, sample(canvas, 0.));
        let at = now + Duration::from_millis(ms);
        state.schedule_presence(&senders, at).unwrap();
        for (slot, rx) in receivers.iter().enumerate() {
            if let Ok(job) = rx.try_recv() {
                observation += 1;
                times.push(ms);
                state.presence_reply(slot, snapshot(job, observation), at);
            }
        }
        state.outbound.clear();
    }
    assert!(times.len() <= 1154);
    assert!(times.windows(2).all(|pair| pair[1] - pair[0] >= 52));
    state.presence_enqueue(60_001, sample(canvas, 0.));
    state.schedule_presence(&senders, now + Duration::from_secs(120)).unwrap();
    let (slot, job) = next(&receivers);
    state.presence_reply(slot, snapshot(job, observation + 1), now);
    state.presence_enqueue(60_002, sample(canvas, 0.));
    state.schedule_presence(&senders, now + Duration::from_secs(120)).unwrap();
    assert!(receivers.iter().all(|rx| rx.try_recv().is_err()));
}
#[test]
fn all_presence_slots_held_do_not_block_local_durability_and_commit_clears_queued_preview() {
    let (_root, mut state, canvas) = fixture();
    let now = Instant::now();
    let (senders, receivers) = channels();
    ready(&mut state, &senders, &receivers, now);
    for n in 1..=3 {
        let _ = dispatch(&mut state, &senders, &receivers, n, sample(canvas, 1.), now + CADENCE * n as u32);
    }
    state.presence_enqueue(4, sample(canvas, 2.));
    let Action::Presence { stroke: Some(style), preview, .. } = sample(canvas, 2.) else { panic!() };
    let stroke = photocraft_scene::Stroke {
        id: style.id,
        canvas,
        owner: state.access.as_ref().unwrap().scope.profile,
        revision: 0,
        anchor: style.anchor,
        points: preview,
        width: style.width,
        color: style.color,
        erase: false,
        brush: style.brush,
        samples: style.samples,
    };
    let start = Instant::now();
    state.command(5, Command::Edit { canvas, action: Edit::AddStroke { stroke } }).unwrap();
    assert!(start.elapsed() < Duration::from_secs(1));
    let operation = state
        .outbound
        .iter()
        .find_map(|e| match e {
            Event::Saved { sequence: 5, operation } => Some(operation.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(state.queue.pending(state.lease().unwrap(), 10).unwrap().len(), 1);
    state.presence_committed(&operation, 1);
    assert!(matches!(&state.presence.pending.front().unwrap().1,Action::Presence {stroke:None,preview,..} if preview.is_empty()));
    let mut later = sample(canvas, 3.);
    state.strip_committed_preview(&mut later);
    assert!(matches!(later,Action::Presence {stroke:None,preview,..} if preview.is_empty()));
}
#[test]
fn membership_denial_coalesces_verification_and_session_denial_pauses_without_revoking_edits() {
    let (_root, mut state, canvas) = fixture();
    let now = Instant::now();
    let (senders, receivers) = channels();
    ready(&mut state, &senders, &receivers, now);
    let a = dispatch(&mut state, &senders, &receivers, 10, sample(canvas, 1.), now + CADENCE);
    let b = dispatch(&mut state, &senders, &receivers, 11, sample(canvas, 2.), now + CADENCE * 2);
    for (slot, job) in [a, b] {
        state.presence_reply(slot, response(job, Err(Error::Rejected { code: "unauthorized".into(), message: "denied".into() })), now);
    }
    assert!(state.presence.paused);
    assert_eq!(state.requests.iter().filter(|(_, a)| matches!(a, Action::Info { .. })).count(), 1);
    assert!(state.queue.cached_grant(state.lease().unwrap(), canvas).unwrap().is_some());
    state.presence_verified(canvas);
    assert!(!state.presence.paused);
    ready(&mut state, &senders, &receivers, now + CADENCE * 3);
    let a = dispatch(&mut state, &senders, &receivers, 12, sample(canvas, 3.), now + CADENCE * 4);
    state.presence_reply(a.0, response(a.1, Err(Error::Rejected { code: "session".into(), message: "expired".into() })), now);
    assert!(state.presence.paused);
    assert!(state.queue.cached_grant(state.lease().unwrap(), canvas).unwrap().is_some());
}
#[test]
fn malformed_snapshot_does_not_advance_observation_or_emit_unvalidated_peers() {
    let (_root, mut state, canvas) = fixture();
    let now = Instant::now();
    let (senders, receivers) = channels();
    ready(&mut state, &senders, &receivers, now);
    let a = dispatch(&mut state, &senders, &receivers, 10, sample(canvas, 1.), now + CADENCE);
    let mut reply = snapshot(a.1, 999);
    if let Ok(Response::PresenceSnapshot { canvas, .. }) = &mut reply.result {
        *canvas = CanvasId::new(&"d".repeat(32)).unwrap();
    }
    state.presence_reply(a.0, reply, now);
    assert_eq!(state.presence.stream.as_ref().unwrap().observation, 0);
    assert!(!state.outbound.iter().any(|e| matches!(e, Event::Response { .. })));
}
