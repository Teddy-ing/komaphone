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
    let mut presentation =
        super::super::Presentation { renderer: Some(WorldRenderer::new(&earlier).unwrap()), ready: Some(historical.clone()), ..Default::default() };
    assert!(presentation.can_render(&historical));
    assert!(!presentation.can_render(&live), "Leaving review must await a live projection");
    presentation.renderer = Some(WorldRenderer::new(&latest).unwrap());
    presentation.ready = Some(live.clone());
    let reduced = key(&earlier, 1);
    assert!(!presentation.can_render(&reduced), "A quarantined future projection must disappear");
    presentation.renderer = Some(WorldRenderer::new(&earlier).unwrap());
    presentation.ready = Some(reduced);
    assert!(presentation.can_render(&live), "Older same-scope live progress remains explicitly labelled while preparing");
}
