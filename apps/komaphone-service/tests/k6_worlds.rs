//! Real durable authority coverage; all identities and artwork are synthetic.
use photocraft_party::{
    DeviceId as PartyDevice, Participant, ParticipantId, RequestId,
    protocol::{Operation, RpcRequest},
};
use photocraft_scene::shared::{DeviceId, Edit, OpId, WorldOp, WorldState};
use photocraft_scene::world_protocol::{Action, Request, Response, VERSION};
use photocraft_scene::{CanvasId, OwnerId, RegionKey, Srgba, Stroke, StrokeId, coordinates};
use photocraft_sync::{SaveStage, Store, StoreOptions, worlds::Limits};

#[path = "k6_worlds/membership_matrix.rs"]
mod membership_matrix;
#[path = "k6_worlds/presence_ordering.rs"]
mod presence_ordering;
#[path = "k6_worlds/stroke_copy.rs"]
mod stroke_copy;
#[path = "k6_worlds/trash.rs"]
mod trash;

fn hex(n: u32) -> String {
    format!("{n:032x}")
}
fn canvas(n: u32) -> CanvasId {
    CanvasId::new(&hex(n)).unwrap()
}
fn owner(n: u32) -> OwnerId {
    OwnerId::new(&hex(n)).unwrap()
}
fn token(n: u32) -> String {
    format!("{:064x}", n + 1000)
}
fn person(n: u32) -> Participant {
    Participant { id: ParticipantId::new(&hex(n)).unwrap(), device: PartyDevice::new(&hex(n + 10000)).unwrap(), display_name: format!("Synthetic {n}") }
}
fn pair(store: &mut Store, n: u32) {
    store
        .execute(
            None,
            &RpcRequest {
                protocol_version: 1,
                request_id: RequestId::new(&hex(n)).unwrap(),
                operation: Operation::Pair { participant: person(n), credential: token(n) },
            },
            100,
        )
        .unwrap();
}
fn request(action: Action) -> Request {
    Request { version: VERSION, action }
}
fn call(store: &mut Store, n: u32, action: Action) -> Response {
    store.world_execute(&token(n), &request(action), 100).unwrap()
}
fn create(store: &mut Store, n: u32, c: u32) {
    call(store, n, Action::Create { canvas: canvas(c), name: format!("Canvas {c}") });
}
fn invite(store: &mut Store, by: u32, to: u32, c: u32, id: u32) {
    call(store, by, Action::Invite { canvas: canvas(c), id: OpId::new(&hex(id)).unwrap(), recipient: owner(to), secret: token(id), expires_at_ms: 100000 });
    call(store, to, Action::Accept { canvas: canvas(c), id: OpId::new(&hex(id)).unwrap(), secret: token(id) });
}
fn op(n: u32, c: u32, sequence: u64, id: u32) -> WorldOp {
    WorldOp {
        id: OpId::new(&hex(id)).unwrap(),
        canvas: canvas(c),
        author: owner(n),
        device: DeviceId::new(&hex(n + 10000)).unwrap(),
        sequence,
        authority_epoch: 1,
        action: Edit::AddStroke {
            stroke: Stroke {
                id: StrokeId::new(&hex(id)).unwrap(),
                canvas: canvas(c),
                owner: owner(n),
                revision: 0,
                anchor: RegionKey::default(),
                points: vec![[1., 1.], [2., 2.]],
                width: 3.,
                color: Srgba { rgba: [0., 0., 0., 1.] },
                erase: false,
                brush: None,
                samples: None,
            },
        },
    }
}
fn events(store: &mut Store, n: u32, c: u32) -> Vec<photocraft_scene::shared::AcceptedOp> {
    match call(store, n, Action::Events { canvas: canvas(c), after: 0, through: None, limit: 128 }) {
        Response::Events { events, .. } => events,
        _ => panic!("events"),
    }
}

#[test]
fn arbitrary_groups_pagination_nonmembers_and_independent_owners() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path().join("service.db"), StoreOptions::default(), 0).unwrap();
    for n in 1..=170 {
        pair(&mut store, n);
    }
    for (c, by, group) in [(1001, 1, vec![1, 2]), (1002, 2, vec![2, 3, 4, 5]), (1003, 3, vec![3, 6, 7, 8, 9, 10, 11, 12])] {
        create(&mut store, by, c);
        for &n in &group {
            if n != by {
                invite(&mut store, by, n, c, c + n);
            }
        }
        for n in 1..=12 {
            let result = store.world_execute(&token(n), &request(Action::Info { canvas: canvas(c) }), 100);
            assert_eq!(result.is_ok(), group.contains(&n));
        }
        for &n in &group {
            call(&mut store, n, Action::Append { operation: op(n, c, 1, c * 100 + n) });
        }
        let accepted = events(&mut store, by, c);
        assert_eq!(accepted.len(), group.len());
        let mut world = WorldState::new(canvas(c), owner(by), format!("Canvas {c}")).unwrap();
        for event in &accepted {
            world.apply(event).unwrap();
        }
        assert_eq!(world.strokes.len(), group.len());
    }
    // 160 invited members is independent of live connection capacity and party roster limits.
    create(&mut store, 1, 2000);
    for n in 2..=160 {
        invite(&mut store, 1, n, 2000, 2000 + n);
    }
    let Response::Members { items, next } = call(&mut store, 1, Action::Members { canvas: canvas(2000), after: None, limit: 128 }) else { panic!() };
    assert_eq!(items.len(), 128);
    assert!(next.is_some());
    let Response::Members { items: tail, next } = call(&mut store, 1, Action::Members { canvas: canvas(2000), after: next, limit: 128 }) else { panic!() };
    assert_eq!(tail.len(), 32);
    assert!(next.is_none());
    assert!(store.world_execute(&token(170), &request(Action::Members { canvas: canvas(2000), after: None, limit: 128 }), 100).is_err());
    let Response::Canvases { items, .. } = call(&mut store, 1, Action::List { after: None, limit: 128 }) else { panic!() };
    assert_eq!(items.iter().map(|i| i.canvas).collect::<Vec<_>>(), vec![canvas(1001), canvas(2000)]);
}

#[test]
fn duplicate_reordered_operations_restart_and_personal_undo_converge() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("service.db");
    let mut store = Store::open(&path, StoreOptions::default(), 0).unwrap();
    pair(&mut store, 1);
    pair(&mut store, 2);
    create(&mut store, 1, 100);
    invite(&mut store, 1, 2, 100, 700);
    let a = op(1, 100, 1, 301);
    let b = op(2, 100, 1, 302);
    let second = op(1, 100, 2, 303);
    assert!(store.world_execute(&token(1), &request(Action::Append { operation: second.clone() }), 100).is_err());
    call(&mut store, 1, Action::Append { operation: a.clone() });
    call(&mut store, 2, Action::Append { operation: b });
    call(&mut store, 1, Action::Append { operation: a.clone() });
    call(&mut store, 1, Action::Append { operation: second });
    let mut bad = a.clone();
    bad.author = owner(2);
    assert!(store.world_execute(&token(1), &request(Action::Append { operation: bad }), 100).is_err());
    let mut undo = op(1, 100, 3, 304);
    undo.action = Edit::SetStrokeVisible { stroke: StrokeId::new(&hex(302)).unwrap(), visible: false };
    assert!(store.world_execute(&token(1), &request(Action::Append { operation: undo.clone() }), 100).is_err());
    undo.action = Edit::SetStrokeVisible { stroke: StrokeId::new(&hex(301)).unwrap(), visible: false };
    call(&mut store, 1, Action::Append { operation: undo });
    let before = events(&mut store, 2, 100);
    assert_eq!(before.len(), 4);
    drop(store);
    let mut store = Store::open(&path, StoreOptions::default(), 500).unwrap();
    let after = events(&mut store, 2, 100);
    assert_eq!(before, after);
    for fetched in [before, after] {
        let mut world = WorldState::new(canvas(100), owner(1), "Canvas 100".into()).unwrap();
        for event in &fetched {
            world.apply(event).unwrap();
        }
        assert!(!world.strokes.get(&StrokeId::new(&hex(301)).unwrap()).unwrap().visible);
        assert!(world.strokes.get(&StrokeId::new(&hex(302)).unwrap()).unwrap().visible);
    }
}

#[test]
fn removal_invalidates_invitations_reads_presence_and_offline_queue_after_rejoin() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path().join("service.db"), StoreOptions::default(), 0).unwrap();
    for n in 1..=3 {
        pair(&mut store, n);
    }
    create(&mut store, 1, 100);
    invite(&mut store, 1, 2, 100, 700);
    let pending = op(2, 100, 1, 301);
    call(
        &mut store,
        2,
        Action::Presence {
            canvas: canvas(100),
            epoch: 1,
            camera: coordinates::home(canvas(100)),
            preview: vec![],
            cursor: Some(Default::default()),
            stroke: None,
        },
    );
    call(&mut store, 1, Action::Remove { canvas: canvas(100), member: owner(2) });
    for action in [
        Action::Info { canvas: canvas(100) },
        Action::Append { operation: pending.clone() },
        Action::Peers { canvas: canvas(100) },
        Action::Accept { canvas: canvas(100), id: OpId::new(&hex(700)).unwrap(), secret: token(700) },
    ] {
        assert!(store.world_execute(&token(2), &request(action), 100).is_err());
    }
    let Response::Peers { peers } = call(&mut store, 1, Action::Peers { canvas: canvas(100) }) else { panic!() };
    assert!(peers.is_empty());
    invite(&mut store, 1, 2, 100, 701);
    assert!(store.world_execute(&token(2), &request(Action::Append { operation: pending.clone() }), 100).is_err());
    let mut new = pending;
    new.authority_epoch = 2;
    call(&mut store, 2, Action::Append { operation: new });
    assert!(store.world_execute(&token(3), &request(Action::Events { canvas: canvas(100), after: 0, through: None, limit: 128 }), 100).is_err());
}

#[test]
fn commit_observer_failure_rolls_back_and_limits_preserve_saved_art() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("service.db");
    let mut store = Store::open(&path, StoreOptions::default(), 0).unwrap();
    pair(&mut store, 1);
    create(&mut store, 1, 100);
    let pending = op(1, 100, 1, 301);
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store.world_execute_observed(&token(1), &request(Action::Append { operation: pending.clone() }), 100, Limits::default(), |stage| {
            if stage == SaveStage::BeforeCommit {
                panic!("synthetic interruption")
            }
        })
    }));
    assert!(failed.is_err());
    drop(store);
    let mut store = Store::open(&path, StoreOptions::default(), 100).unwrap();
    assert!(events(&mut store, 1, 100).is_empty());
    call(&mut store, 1, Action::Append { operation: pending });
    assert!(
        store
            .world_execute_observed(
                &token(1),
                &request(Action::Create { canvas: canvas(101), name: "No room".into() }),
                100,
                Limits { canvases: 1, ..Limits::default() },
                |_| {}
            )
            .is_err()
    );
    assert_eq!(events(&mut store, 1, 100).len(), 1);
}

#[tokio::test]
async fn actual_http_world_route_rejects_no_auth_and_persists_after_service_restart() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("service.db");
    let mut store = Store::open(&path, StoreOptions::default(), 0).unwrap();
    pair(&mut store, 1);
    drop(store);
    let service = photocraft_sync::server::spawn("127.0.0.1:0".parse().unwrap(), &path, StoreOptions::default()).await.unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let url = format!("http://{}/v1/canvas", service.address);
    let create = request(Action::Create { canvas: canvas(100), name: "Across restarts".into() });
    assert_eq!(client.post(&url).json(&create).send().await.unwrap().status(), reqwest::StatusCode::FORBIDDEN);
    assert!(client.post(&url).bearer_auth(token(1)).json(&create).send().await.unwrap().status().is_success());
    assert!(
        client.post(&url).bearer_auth(token(1)).json(&request(Action::Append { operation: op(1, 100, 1, 301) })).send().await.unwrap().status().is_success()
    );
    service.shutdown().await.unwrap();
    let service = photocraft_sync::server::spawn("127.0.0.1:0".parse().unwrap(), &path, StoreOptions::default()).await.unwrap();
    let response = client
        .post(format!("http://{}/v1/canvas", service.address))
        .bearer_auth(token(1))
        .json(&request(Action::Events { canvas: canvas(100), after: 0, through: None, limit: 128 }))
        .send()
        .await
        .unwrap();
    let Response::Events { events, .. } = response.json::<Response>().await.unwrap() else { panic!() };
    assert_eq!(events.len(), 1);
    service.shutdown().await.unwrap();
}

#[test]
fn verified_backup_preserves_world_ids_membership_events_and_detects_corruption() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("service.db");
    let backup = root.path().join("backup.db");
    let restored = root.path().join("restored.db");
    let mut store = Store::open(&path, StoreOptions::default(), 0).unwrap();
    pair(&mut store, 1);
    pair(&mut store, 2);
    create(&mut store, 1, 100);
    invite(&mut store, 1, 2, 100, 700);
    call(&mut store, 2, Action::Append { operation: op(2, 100, 1, 301) });
    let expected = events(&mut store, 1, 100);
    store.backup(&backup).unwrap();
    Store::copy_verified(&backup, &restored).unwrap();
    let mut restored_store = Store::open(&restored, StoreOptions::default(), 0).unwrap();
    assert_eq!(expected, events(&mut restored_store, 2, 100));
    drop(restored_store);
    let db = rusqlite::Connection::open(&restored).unwrap();
    db.execute("UPDATE world_events SET body=x'00'", []).unwrap();
    drop(db);
    assert!(Store::open(&restored, StoreOptions::default(), 0).is_err());
    assert!(Store::copy_verified(&restored, root.path().join("must-not-exist.db")).is_err());
    assert!(!root.path().join("must-not-exist.db").exists());
    assert_eq!(expected, events(&mut store, 2, 100));
}

#[test]
fn fresh_replay_after_rename_retains_original_canonical_seed() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path().join("service.db"), StoreOptions::default(), 0).unwrap();
    pair(&mut store, 1);
    create(&mut store, 1, 100);
    let mut rename = op(1, 100, 1, 301);
    rename.action = Edit::Rename { name: "Renamed".into(), expected_revision: 0 };
    call(&mut store, 1, Action::Append { operation: rename });
    let Response::Canvas { info } = call(&mut store, 1, Action::Info { canvas: canvas(100) }) else { panic!() };
    assert_eq!(info.initial_name, "Canvas 100");
    assert_eq!(info.name, "Renamed");
    let mut seed = WorldState::new(info.canvas, info.owner, info.initial_name).unwrap();
    for event in events(&mut store, 1, 100) {
        seed.apply(&event).unwrap();
    }
    assert_eq!(seed.name, info.name);
    assert_eq!(seed.revision, info.revision);
    seed.validate().unwrap();
}

#[test]
fn unknown_world_schema_rejected_and_presence_capacity_separate_from_membership() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("service.db");
    let mut store = Store::open(&path, StoreOptions::default(), 0).unwrap();
    pair(&mut store, 1);
    pair(&mut store, 2);
    create(&mut store, 1, 100);
    invite(&mut store, 1, 2, 100, 700);
    let limits = Limits { presence_connections: 1, ..Limits::default() };
    let presence = request(Action::Presence {
        canvas: canvas(100),
        epoch: 1,
        camera: coordinates::home(canvas(100)),
        preview: vec![],
        cursor: Some(Default::default()),
        stroke: None,
    });
    store.world_execute_observed(&token(1), &presence, 100, limits, |_| {}).unwrap();
    assert!(store.world_execute_observed(&token(2), &presence, 100, limits, |_| {}).is_err());
    call(&mut store, 2, Action::Append { operation: op(2, 100, 1, 301) });
    store.world_execute_observed(&token(2), &presence, 3000, limits, |_| {}).unwrap();
    drop(store);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE world_schema SET version=999;").unwrap();
    drop(db);
    assert!(Store::open(path, StoreOptions::default(), 0).is_err());
}
