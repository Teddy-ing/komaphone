use super::*;
use photocraft_scene::world_protocol::{Peer, PreviewStyle};
use photocraft_sync::Error;

fn fixture() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path().join("service.db"), StoreOptions::default(), 0).unwrap();
    for n in 1..=3 {
        pair(&mut store, n);
    }
    create(&mut store, 1, 100);
    invite(&mut store, 1, 2, 100, 700);
    (root, store)
}
fn nonce(n: u32) -> OpId {
    OpId::new(&hex(n)).unwrap()
}
fn at(store: &mut Store, actor: u32, action: Action, now: u64) -> photocraft_sync::Result<Response> {
    store.world_execute(&token(actor), &request(action), now)
}
fn open(store: &mut Store, actor: u32, n: u32, epoch: u64, now: u64) -> OpId {
    let Response::PresenceReady { canvas: scope, epoch: actual, nonce: echo, stream } =
        at(store, actor, Action::PresenceOpen { canvas: canvas(100), epoch, nonce: nonce(n) }, now).unwrap()
    else {
        panic!("ready")
    };
    assert_eq!((scope, actual, echo), (canvas(100), epoch, nonce(n)));
    stream
}
fn sample(stream: OpId, serial: u64, id: Option<u32>) -> Action {
    Action::PresenceUpdate {
        canvas: canvas(100),
        epoch: 1,
        stream,
        serial,
        camera: coordinates::home(canvas(100)),
        cursor: Some(Default::default()),
        preview: if id.is_some() { vec![[1., 1.], [2., 2.]] } else { vec![] },
        stroke: id.map(|id| PreviewStyle {
            id: StrokeId::new(&hex(id)).unwrap(),
            anchor: Default::default(),
            width: 3.,
            color: Srgba { rgba: [0., 0., 0., 1.] },
            erase: false,
        }),
    }
}
fn legacy() -> Action {
    Action::Presence { canvas: canvas(100), epoch: 1, camera: coordinates::home(canvas(100)), preview: vec![], cursor: None, stroke: None }
}
fn snapshot(response: Response) -> (u64, Vec<Peer>) {
    let Response::PresenceSnapshot { canvas: scope, observation, peers, .. } = response else { panic!("snapshot") };
    assert_eq!(scope, canvas(100));
    (observation, peers)
}
fn read(store: &mut Store, actor: u32, stream: OpId, now: u64) -> photocraft_sync::Result<Response> {
    at(store, actor, Action::PresenceRead { canvas: canvas(100), stream }, now)
}
fn forbidden(result: photocraft_sync::Result<Response>) {
    assert!(matches!(result, Err(Error::Domain(photocraft_party::Error::Unauthorized))));
}
fn stream_error(result: photocraft_sync::Result<Response>) {
    assert!(matches!(result, Err(Error::PresenceStream)));
}

#[test]
fn publications_and_snapshots_have_independent_monotonic_orders_without_duplicate_renewal() {
    let (_root, mut store) = fixture();
    let stream = open(&mut store, 1, 800, 1, 100);
    let first = snapshot(at(&mut store, 1, sample(stream, 20, Some(301)), 200).unwrap());
    assert_eq!(first.0, 1);
    assert_eq!(first.1[0].expires_at_ms, 2200);
    let stale = snapshot(at(&mut store, 1, sample(stream, 19, Some(302)), 2100).unwrap());
    assert_eq!(stale.0, 2);
    assert_eq!(stale.1[0].stroke.as_ref().unwrap().id, StrokeId::new(&hex(301)).unwrap());
    assert_eq!(stale.1[0].expires_at_ms, 2200);
    assert_eq!(open(&mut store, 1, 800, 1, 2150), stream);
    let duplicate = snapshot(at(&mut store, 1, sample(stream, 20, None), 2200).unwrap());
    assert_eq!(duplicate.0, 3);
    assert!(duplicate.1.is_empty());
    assert_eq!(snapshot(read(&mut store, 1, stream, 30_199).unwrap()).0, 4);
    stream_error(read(&mut store, 1, stream, 30_200));
    stream_error(at(&mut store, 1, sample(stream, 21, Some(303)), 30_200));
    let fresh = open(&mut store, 1, 800, 1, 30_200);
    assert_ne!(fresh, stream);
    stream_error(read(&mut store, 1, stream, 30_201));
    assert_eq!(snapshot(read(&mut store, 1, fresh, 30_201).unwrap()).0, 1);
}

#[test]
fn current_auth_epoch_and_validated_payload_precede_presence_mutation() {
    let (_root, mut store) = fixture();
    let stream = open(&mut store, 1, 800, 1, 100);
    at(&mut store, 1, sample(stream, 1, Some(301)), 200).unwrap();
    for action in [
        Action::PresenceOpen { canvas: canvas(100), epoch: 1, nonce: nonce(802) },
        sample(stream, 999, Some(302)),
        Action::PresenceRead { canvas: canvas(100), stream: nonce(999) },
    ] {
        forbidden(at(&mut store, 3, action, 201));
    }
    stream_error(at(&mut store, 2, sample(stream, 2, Some(302)), 201));
    let mut wrong_epoch = sample(nonce(999), 2, Some(302));
    if let Action::PresenceUpdate { epoch, .. } = &mut wrong_epoch {
        *epoch = 2;
    }
    forbidden(at(&mut store, 1, wrong_epoch, 201));
    let mut invalid = sample(stream, 999, Some(302));
    if let Action::PresenceUpdate { camera, .. } = &mut invalid {
        camera.canvas = canvas(200);
    }
    assert!(matches!(at(&mut store, 1, invalid, 201), Err(Error::Invalid(_))));
    assert!(matches!(at(&mut store, 1, sample(stream, 0, None), 201), Err(Error::Invalid(_))));
    let latest = snapshot(at(&mut store, 1, sample(stream, 2, Some(303)), 300).unwrap());
    assert_eq!(latest.0, 2);
    assert_eq!(latest.1[0].stroke.as_ref().unwrap().id, StrokeId::new(&hex(303)).unwrap());
    assert!(matches!(store.world_execute(&token(999), &request(sample(stream, 3, None)), 301), Err(Error::Session)));
    forbidden(at(&mut store, 3, Action::PresenceRead { canvas: canvas(100), stream }, 99_000));
}

#[test]
fn legacy_writes_cannot_replace_active_streams_and_connection_cap_counts_the_union() {
    let (_root, mut store) = fixture();
    let limits = Limits { presence_connections: 1, ..Limits::default() };
    store.world_execute_observed(&token(1), &request(legacy()), 100, limits, |_| {}).unwrap();
    let Response::PresenceReady { stream, .. } = store
        .world_execute_observed(&token(1), &request(Action::PresenceOpen { canvas: canvas(100), epoch: 1, nonce: nonce(800) }), 200, limits, |_| {})
        .unwrap()
    else {
        panic!()
    };
    assert!(snapshot(read(&mut store, 1, stream, 201).unwrap()).1.is_empty());
    for action in [legacy(), Action::PresenceOpen { canvas: canvas(100), epoch: 1, nonce: nonce(801) }] {
        assert!(matches!(
            store.world_execute_observed(&token(2), &request(action), 2500, limits, |_| {}),
            Err(Error::Domain(photocraft_party::Error::Limit(_)))
        ));
    }
    stream_error(at(&mut store, 1, legacy(), 2500));
    let newer = open(&mut store, 1, 802, 1, 2600);
    assert_ne!(newer, stream);
    stream_error(read(&mut store, 1, stream, 2601));
    store.world_execute_observed(&token(2), &request(legacy()), 32_600, limits, |_| {}).unwrap();
    stream_error(read(&mut store, 1, newer, 32_600));
}

#[test]
fn commits_close_inflight_preview_ids_and_tombstone_exhaustion_rotates_without_losing_art() {
    let (_root, mut store) = fixture();
    let stream = open(&mut store, 1, 800, 1, 100);
    at(&mut store, 1, sample(stream, 1, Some(301)), 200).unwrap();
    call(&mut store, 1, Action::Append { operation: op(1, 100, 1, 301) });
    let delayed = snapshot(at(&mut store, 1, sample(stream, 3, Some(301)), 300).unwrap());
    assert!(delayed.1[0].stroke.is_none());
    assert!(delayed.1[0].preview.is_empty());
    call(&mut store, 1, Action::Append { operation: op(1, 100, 2, 302) });
    assert!(snapshot(at(&mut store, 1, sample(stream, 4, Some(302)), 301).unwrap()).1[0].stroke.is_none());
    call(&mut store, 1, Action::Append { operation: op(1, 100, 2, 302) });
    for sequence in 3..=128 {
        call(&mut store, 1, Action::Append { operation: op(1, 100, sequence, 300 + sequence as u32) });
    }
    read(&mut store, 1, stream, 302).unwrap();
    call(&mut store, 1, Action::Append { operation: op(1, 100, 129, 429) });
    stream_error(at(&mut store, 1, sample(stream, u64::MAX, Some(301)), 303));
    let Response::Canvas { info } = call(&mut store, 1, Action::Info { canvas: canvas(100) }) else { panic!() };
    assert_eq!(info.revision, 129);
    let fresh = open(&mut store, 1, 801, 1, 304);
    assert_ne!(fresh, stream);
    assert!(snapshot(read(&mut store, 1, fresh, 305).unwrap()).1.is_empty());
}

#[test]
fn removal_leave_trash_device_revocation_and_restart_fence_old_streams() {
    let (root, mut store) = fixture();
    let removed = open(&mut store, 2, 800, 1, 100);
    at(&mut store, 2, sample(removed, 1, Some(301)), 100).unwrap();
    call(&mut store, 1, Action::Remove { canvas: canvas(100), member: owner(2) });
    forbidden(read(&mut store, 2, removed, 101));
    invite(&mut store, 1, 2, 100, 701);
    stream_error(read(&mut store, 2, removed, 101));
    let left = open(&mut store, 2, 801, 2, 101);
    call(&mut store, 2, Action::Leave { canvas: canvas(100) });
    forbidden(read(&mut store, 2, left, 102));
    invite(&mut store, 1, 2, 100, 702);
    stream_error(read(&mut store, 2, left, 103));
    let trashed = open(&mut store, 1, 802, 1, 104);
    call(&mut store, 1, Action::Delete { canvas: canvas(100), expected_epoch: 1 });
    forbidden(read(&mut store, 1, trashed, 105));
    call(&mut store, 1, Action::RestoreOriginal { canvas: canvas(100), expected_epoch: 2 });
    stream_error(read(&mut store, 1, trashed, 106));
    let revoked = open(&mut store, 2, 803, 4, 107);
    store.identity_revoke_device(&token(2), person(2).device, 108).unwrap();
    assert!(matches!(read(&mut store, 2, revoked, 109), Err(Error::Session)));
    let restart = open(&mut store, 1, 804, 2, 110);
    drop(store);
    let mut store = Store::open(root.path().join("service.db"), StoreOptions::default(), 111).unwrap();
    stream_error(read(&mut store, 1, restart, 112));
    let fresh = open(&mut store, 1, 804, 2, 113);
    assert_ne!(fresh, restart);
    assert!(snapshot(read(&mut store, 1, fresh, 114).unwrap()).1.is_empty());
}

#[test]
fn ordered_presence_wire_roundtrips_at_current_canvas_version() {
    for action in [
        legacy(),
        Action::Peers { canvas: canvas(100) },
        Action::PresenceOpen { canvas: canvas(100), epoch: 1, nonce: nonce(800) },
        sample(nonce(801), u64::MAX, Some(301)),
        Action::PresenceRead { canvas: canvas(100), stream: nonce(801) },
    ] {
        let bytes = serde_json::to_vec(&request(action)).unwrap();
        let decoded = Request::decode(&bytes).unwrap();
        assert_eq!(decoded.version, VERSION);
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), bytes);
    }
}

#[tokio::test]
async fn http_stream_fault_is_distinct_from_session_and_membership_denial() {
    let (root, store) = fixture();
    drop(store);
    let service = photocraft_sync::server::spawn("127.0.0.1:0".parse().unwrap(), root.path().join("service.db"), StoreOptions::default()).await.unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let url = format!("http://{}/v1/canvas", service.address);
    for (actor, expected, code) in [(1, 409, "presence_stream"), (3, 403, "unauthorized"), (999, 401, "session")] {
        let response =
            client.post(&url).bearer_auth(token(actor)).json(&request(Action::PresenceRead { canvas: canvas(100), stream: nonce(800) })).send().await.unwrap();
        assert_eq!(response.status().as_u16(), expected);
        let fault: photocraft_party::protocol::Fault = response.json().await.unwrap();
        assert_eq!(fault.code, code);
    }
    let Response::PresenceReady { stream, .. } = client
        .post(&url)
        .bearer_auth(token(1))
        .json(&request(Action::PresenceOpen { canvas: canvas(100), epoch: 1, nonce: nonce(800) }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
    else {
        panic!()
    };
    for serial in [3, 1, 2] {
        let response: Response = client
            .post(&url)
            .bearer_auth(token(1))
            .json(&request(sample(stream, serial, Some(300 + serial as u32))))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let (_, peers) = snapshot(response);
        assert_eq!(peers[0].stroke.as_ref().unwrap().id, StrokeId::new(&hex(303)).unwrap());
    }
    service.shutdown().await.unwrap();
}

#[test]
fn copied_stroke_commit_also_closes_late_ordered_preview() {
    use photocraft_scene::shared::media::Credit;
    use photocraft_scene::shared::{StrokeCopyTarget, stroke_copy};
    let (_root, mut store) = fixture();
    call(&mut store, 1, Action::Append { operation: op(1, 100, 1, 301) });
    let mut source = WorldState::new(canvas(100), owner(1), "Canvas 100".into()).unwrap();
    for event in events(&mut store, 1, 100) {
        source.apply(&event).unwrap();
    }
    let mut copied = op(1, 100, 2, 302);
    copied.action = stroke_copy(
        &source.strokes[&StrokeId::new(&hex(301)).unwrap()],
        canvas(100),
        Credit { profile: owner(1), label: person(1).display_name },
        StrokeCopyTarget {
            canvas: canvas(100),
            id: StrokeId::new(&hex(302)).unwrap(),
            owner: owner(1),
            anchor: Default::default(),
            offset: [0., 0.],
            scale: 1.,
        },
    )
    .unwrap();
    let stream = open(&mut store, 1, 800, 1, 100);
    at(&mut store, 1, sample(stream, 1, Some(302)), 101).unwrap();
    call(&mut store, 1, Action::RemixStroke { operation: copied });
    assert!(snapshot(at(&mut store, 1, sample(stream, 2, Some(302)), 102).unwrap()).1[0].stroke.is_none());
}
