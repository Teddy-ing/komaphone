use super::*;
use photocraft_engine::scene::{
    OwnerId, Srgba, Stroke, StrokeId,
    shared::{AcceptedOp, DeviceId, Edit, OpId, WorldOp},
};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

fn world(n: u128) -> Arc<WorldState> {
    let canvas = CanvasId::new(&format!("{n:032x}")).unwrap();
    let owner = OwnerId::new(&format!("{:032x}", n + 100)).unwrap();
    let mut world = WorldState::new(canvas, owner, "Synthetic projection".into()).unwrap();
    for revision in 1..=3 {
        let id = format!("{:032x}", revision + 1000);
        world
            .apply(&AcceptedOp {
                revision,
                op: WorldOp {
                    id: OpId::new(&id).unwrap(),
                    canvas,
                    author: owner,
                    device: DeviceId::new(&format!("{:032x}", n + 200)).unwrap(),
                    sequence: revision,
                    authority_epoch: 1,
                    action: Edit::AddStroke {
                        stroke: Stroke {
                            id: StrokeId::new(&id).unwrap(),
                            canvas,
                            owner,
                            revision: 0,
                            anchor: Default::default(),
                            points: vec![[revision as f64, 0.]],
                            width: 2.,
                            color: Srgba { rgba: [0., 0., 0., 1.] },
                            erase: false,
                            brush: None,
                            samples: None,
                        },
                    },
                },
            })
            .unwrap();
    }
    Arc::new(world)
}
fn key(world: &WorldState, activation: u64) -> Key {
    Key {
        scope: Scope {
            activation,
            canvas: world.canvas,
            account: Account { endpoint: "https://synthetic.invalid".into(), profile: world.owner, device: DeviceId::new(&format!("{:032x}", 900)).unwrap() },
        },
        revision: Revision::Live { canonical: world.revision, display: world.revision, pending: 0 },
    }
}
fn wait(worker: &Worker) -> Completed {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(result) = worker.take() {
            return result;
        }
        assert!(Instant::now() < until, "CPU projection did not complete");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn fixed_review_projection_ignores_later_arrivals_and_keeps_exact_history_cursor() {
    let source = world(1);
    let mut request_key = key(&source, 1);
    request_key.revision = Revision::Review { end: 2, position: 1 };
    let worker = Worker::start().unwrap();
    worker.request(request_key.clone(), source.clone()).unwrap();
    let projected = wait(&worker);
    assert_eq!(projected.key, request_key);
    let expected = source.replay_to(1).unwrap();
    assert_eq!(projected.result.unwrap().world(), &expected);
    let mut later = (*source).clone();
    later
        .apply(&AcceptedOp {
            revision: 4,
            op: WorldOp {
                id: OpId::new(&format!("{:032x}", 1004)).unwrap(),
                canvas: later.canvas,
                author: later.owner,
                device: DeviceId::new(&format!("{:032x}", 201)).unwrap(),
                sequence: 4,
                authority_epoch: 1,
                action: Edit::SetStrokeVisible { stroke: StrokeId::new(&format!("{:032x}", 1001)).unwrap(), visible: false },
            },
        })
        .unwrap();
    // New current-world content changes no part of the frozen review cache key.
    worker.request(request_key.clone(), Arc::new(later)).unwrap();
    let again = wait(&worker);
    assert_eq!(again.key, request_key);
    assert_eq!(again.result.unwrap().world(), &expected);
    assert_eq!(source.revision, 3);
}

#[test]
fn account_switch_during_cpu_work_discards_stale_result_and_coalesces_pending_requests() {
    let first = world(1);
    let final_source = world(2);
    let (entered, observed) = mpsc::sync_channel(1);
    let (release, wait_release) = mpsc::sync_channel(1);
    let mut wait_release = Some(wait_release);
    let worker = Worker::start_with(move |_| {
        if let Some(wait_release) = wait_release.take() {
            entered.send(()).unwrap();
            wait_release.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    })
    .unwrap();
    worker.request(key(&first, 1), first).unwrap();
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    worker.invalidate();
    for activation in 2..=100 {
        worker.request(key(&final_source, activation), final_source.clone()).unwrap();
    }
    assert!(worker.take().is_none());
    let expected = key(&final_source, 100);
    release.send(()).unwrap();
    let result = wait(&worker);
    assert_eq!(result.key, expected);
    assert_eq!(result.result.unwrap().world(), &*final_source);
    assert!(worker.take().is_none());
}

#[test]
fn hostile_scope_and_future_review_requests_fail_without_publishing_foreign_art() {
    let source = world(1);
    let foreign = world(2);
    let worker = Worker::start().unwrap();
    worker.request(key(&foreign, 1), source.clone()).unwrap();
    assert!(wait(&worker).result.is_err());
    let mut future = key(&source, 1);
    future.revision = Revision::Review { end: 4, position: 4 };
    worker.request(future, source).unwrap();
    assert!(wait(&worker).result.is_err());
}

#[test]
fn busy_live_updates_publish_same_scope_progress_then_the_latest_projection() {
    let latest = world(1);
    let earlier = Arc::new(latest.replay_to(1).unwrap());
    let (entered, observed) = mpsc::sync_channel(1);
    let (release, wait_release) = mpsc::sync_channel(1);
    let worker = Worker::start_with(move |_| {
        entered.send(()).unwrap();
        wait_release.recv_timeout(Duration::from_secs(5)).unwrap();
    })
    .unwrap();
    let earlier_key = key(&earlier, 1);
    let latest_key = key(&latest, 1);
    worker.request(earlier_key.clone(), earlier).unwrap();
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    worker.request(latest_key.clone(), latest.clone()).unwrap();
    release.send(()).unwrap();
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    let progress = wait(&worker);
    assert_eq!(progress.key, earlier_key);
    assert!(progress.key.may_show_before(&latest_key));
    assert_eq!(progress.result.unwrap().world().revision, 1);
    release.send(()).unwrap();
    let finished = wait(&worker);
    assert_eq!(finished.key, latest_key);
    assert_eq!(finished.result.unwrap().world(), &*latest);
}

#[test]
fn installed_review_and_quarantined_future_projection_cannot_masquerade_as_live_scene() {
    let latest = world(1);
    let earlier = latest.replay_to(1).unwrap();
    let live = key(&latest, 1);
    let mut historical = live.clone();
    historical.revision = Revision::Review { end: 2, position: 1 };
    let mut historical_next = live.clone();
    historical_next.revision = Revision::Review { end: 2, position: 2 };
    let mut historical_prev = live.clone();
    historical_prev.revision = Revision::Review { end: 2, position: 0 };
    let mut different_interval = live.clone();
    different_interval.revision = Revision::Review { end: 3, position: 2 };
    let mut presentation =
        super::super::Presentation { renderer: Some(WorldRenderer::new(&earlier).unwrap()), ready: Some(historical.clone()), ..Default::default() };
    assert!(presentation.can_render(&historical));
    assert!(presentation.can_render(&historical_next), "Forward same-interval review step must retain previous frame without white flash");
    presentation.ready = Some(historical_next.clone());
    assert!(presentation.can_render(&historical_prev), "Backward same-interval scrub must retain previous frame without white flash");
    assert!(!presentation.can_render(&different_interval), "Cross-interval review frames cannot masquerade across different end snapshots");
    assert!(!presentation.can_render(&live), "Leaving review must await a live projection");
    presentation.renderer = Some(WorldRenderer::new(&latest).unwrap());
    presentation.ready = Some(live.clone());
    let reduced = key(&earlier, 1);
    assert!(!presentation.can_render(&reduced), "A quarantined future projection must disappear");
    presentation.renderer = Some(WorldRenderer::new(&earlier).unwrap());
    presentation.ready = Some(reduced);
    assert!(presentation.can_render(&live), "Older same-scope live progress remains explicitly labelled while preparing");
}

#[test]
fn ack_with_identical_display_state_skips_rebuild_and_emits_zero_dirty_rects() {
    let source = world(1);
    let worker = Worker::start().unwrap();
    let mut pending_key = key(&source, 1);
    pending_key.revision = Revision::Live { canonical: 2, display: 3, pending: 1 };
    worker.request(pending_key.clone(), source.clone()).unwrap();
    let initial = wait(&worker);
    assert_eq!(initial.key, pending_key);
    assert!(initial.result.is_ok());
    assert!(initial.dirty_rects.is_none());

    // Server ACK transitions canonical 2->3 and pending 1->0 with identical display history.
    let mut acked_key = pending_key.clone();
    acked_key.revision = Revision::Live { canonical: 3, display: 3, pending: 0 };
    worker.request(acked_key.clone(), source.clone()).unwrap();
    let acked = wait(&worker);
    assert_eq!(acked.key, acked_key);
    assert_eq!(acked.dirty_rects, Some(Vec::new()), "identical ACK must not invalidate any tiles");
    assert_eq!(acked.result.unwrap().world(), &*source);
}

#[test]
fn ack_plus_peer_stroke_takes_incremental_path_while_conflicting_ack_rebuilds() {
    let base = world(1);
    let worker = Worker::start().unwrap();
    let mut pending_key = key(&base, 1);
    pending_key.revision = Revision::Live { canonical: 2, display: 3, pending: 1 };
    worker.request(pending_key.clone(), base.clone()).unwrap();
    let _ = wait(&worker);

    // 1. ACK + concurrent peer stroke on rev 4: prefix 1..=3 matches, so apply incrementally.
    let mut with_peer = (*base).clone();
    let peer_device = DeviceId::new(&format!("{:032x}", 301)).unwrap();
    with_peer
        .apply(&AcceptedOp {
            revision: 4,
            op: WorldOp {
                id: OpId::new(&format!("{:032x}", 1004)).unwrap(),
                canvas: with_peer.canvas,
                author: with_peer.owner,
                device: peer_device,
                sequence: 1,
                authority_epoch: 1,
                action: Edit::AddStroke {
                    stroke: Stroke {
                        id: StrokeId::new(&format!("{:032x}", 1004)).unwrap(),
                        canvas: with_peer.canvas,
                        owner: with_peer.owner,
                        revision: 0,
                        anchor: Default::default(),
                        points: vec![[40., 40.], [60., 60.]],
                        width: 4.,
                        color: Srgba { rgba: [0., 0., 1., 1.] },
                        erase: false,
                        brush: None,
                        samples: None,
                    },
                },
            },
        })
        .unwrap();
    let with_peer = Arc::new(with_peer);
    let mut peer_key = pending_key.clone();
    peer_key.revision = Revision::Live { canonical: 4, display: 4, pending: 0 };
    worker.request(peer_key.clone(), with_peer.clone()).unwrap();
    let inc = wait(&worker);
    assert_eq!(inc.key, peer_key);
    assert!(inc.dirty_rects.as_ref().is_some_and(|rects| !rects.is_empty()), "matching prefix ACK + peer stroke must apply incrementally");
    assert_eq!(inc.result.unwrap().world(), &*with_peer);

    // 2. Conflicting/reordered history at the same display rev (op ID differs): MUST fall back to full rebuild.
    let mut diverged = with_peer.replay_to(2).unwrap();
    for (rev, op_num) in [(3u64, 9003u128), (4u64, 9004u128)] {
        let seq = diverged.next_sequence(peer_device).unwrap();
        diverged
            .apply(&AcceptedOp {
                revision: rev,
                op: WorldOp {
                    id: OpId::new(&format!("{op_num:032x}")).unwrap(),
                    canvas: diverged.canvas,
                    author: diverged.owner,
                    device: peer_device,
                    sequence: seq,
                    authority_epoch: 1,
                    action: Edit::AddStroke {
                        stroke: Stroke {
                            id: StrokeId::new(&format!("{op_num:032x}")).unwrap(),
                            canvas: diverged.canvas,
                            owner: diverged.owner,
                            revision: 0,
                            anchor: Default::default(),
                            points: vec![[-20., -20.]],
                            width: 4.,
                            color: Srgba { rgba: [1., 0., 0., 1.] },
                            erase: false,
                            brush: None,
                            samples: None,
                        },
                    },
                },
            })
            .unwrap();
    }
    let diverged = Arc::new(diverged);
    let mut conflict_key = pending_key;
    conflict_key.revision = Revision::Live { canonical: 4, display: 4, pending: 0 };
    // First re-install pending rev 4 from `with_peer` as pending=2 so next transition tests conflict check.
    let mut pending2_key = conflict_key.clone();
    pending2_key.revision = Revision::Live { canonical: 2, display: 4, pending: 2 };
    worker.request(pending2_key, with_peer).unwrap();
    let _ = wait(&worker);
    worker.request(conflict_key.clone(), diverged.clone()).unwrap();
    let rebuilt = wait(&worker);
    assert_eq!(rebuilt.key, conflict_key);
    assert!(rebuilt.dirty_rects.is_none(), "diverged op IDs on ACK must trigger full rebuild");
    assert_eq!(rebuilt.result.unwrap().world(), &*diverged);
}
