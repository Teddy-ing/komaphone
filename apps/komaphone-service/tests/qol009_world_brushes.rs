use photocraft_party::{
    DeviceId as PartyDevice, Error as DomainError, Participant, ParticipantId, RequestId,
    protocol::{Operation, RpcRequest},
};
use photocraft_scene::{
    CanvasId, OwnerId, RegionKey, Srgba, Stroke, StrokeId,
    brush::{BrushTip, ProceduralTipStyle, StrokeBrush, StrokeSample},
    coordinates,
    shared::{DeviceId, Edit, OpId, SHARED_SCHEMA, SHARED_SCHEMA_BRUSH, WorldOp, WorldState, archive_import::ArchiveManifest},
    world_protocol::{Action, PreviewStyle, Request, Response, VERSION},
};
use photocraft_sync::{Error, Store, StoreOptions};

fn hex(n: u128) -> String {
    format!("{n:032x}")
}
fn token(n: u128) -> String {
    format!("{n:064x}")
}
fn owner(n: u128) -> OwnerId {
    OwnerId::new(&hex(n)).unwrap()
}
fn device(n: u128) -> DeviceId {
    DeviceId::new(&hex(n + 1000)).unwrap()
}
fn canvas(n: u128) -> CanvasId {
    CanvasId::new(&hex(n + 2000)).unwrap()
}

fn pair(store: &mut Store, n: u128) {
    store
        .execute(
            None,
            &RpcRequest {
                protocol_version: photocraft_party::protocol::PROTOCOL_VERSION,
                request_id: RequestId::new(&hex(n + 3000)).unwrap(),
                operation: Operation::Pair {
                    participant: Participant {
                        id: ParticipantId::new(&hex(n)).unwrap(),
                        device: PartyDevice::new(&hex(n + 1000)).unwrap(),
                        display_name: format!("Artist {n}"),
                    },
                    credential: token(n),
                },
            },
            100,
        )
        .unwrap();
}

#[test]
fn v4_service_db_migrates_to_v5_with_backup_and_rejects_v2_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("service.sqlite3");
    let mut store = Store::open(&path, StoreOptions::default(), 100).unwrap();
    pair(&mut store, 1);
    pair(&mut store, 2);

    let c = canvas(1);
    store.world_execute(&token(1), &Request { version: VERSION, action: Action::Create { canvas: c, name: "Pre-v5 canvas".into() } }, 100).unwrap();
    let legacy = Stroke {
        id: StrokeId::new(&hex(501)).unwrap(),
        canvas: c,
        owner: owner(1),
        revision: 0,
        anchor: RegionKey::default(),
        points: vec![[10.0, 10.0], [40.0, 20.0]],
        width: 8.0,
        color: Srgba { rgba: [0.1, 0.2, 0.3, 1.0] },
        erase: false,
        brush: None,
        samples: None,
    };
    store
        .world_execute(
            &token(1),
            &Request {
                version: VERSION,
                action: Action::Append {
                    operation: WorldOp {
                        id: OpId::new(&hex(601)).unwrap(),
                        canvas: c,
                        author: owner(1),
                        device: device(1),
                        sequence: 1,
                        authority_epoch: 1,
                        action: Edit::AddStroke { stroke: legacy.clone() },
                    },
                },
            },
            100,
        )
        .unwrap();
    let Response::Events { events: pre_events, .. } =
        store.world_execute(&token(1), &Request { version: VERSION, action: Action::Events { canvas: c, after: 0, through: None, limit: 16 } }, 100).unwrap()
    else {
        panic!("expected events");
    };
    drop(store);

    // Simulate an existing production v4 database before upgrading (legacy worlds.state/hash, no v6 tables).
    let mut legacy_world = WorldState::new(c, owner(1), "Pre-v5 canvas".into()).unwrap();
    for ev in &pre_events {
        legacy_world.apply(ev).unwrap();
    }
    let legacy_bytes = serde_json::to_vec(&legacy_world).unwrap();
    let legacy_hash = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(&legacy_bytes));
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE worlds SET state=?1,hash=?2 WHERE id=?3", rusqlite::params![&legacy_bytes, &legacy_hash, c.as_str()]).unwrap();
    db.execute_batch(
        "DROP TABLE world_checkpoints; DROP TABLE world_tours_idx; DROP TABLE world_media_idx; DROP TABLE world_strokes_idx; DROP TABLE world_devices; DROP TABLE world_meta; DROP TABLE world_schema; CREATE TABLE world_schema(version INTEGER NOT NULL CHECK(version=4)); INSERT INTO world_schema VALUES(4); PRAGMA user_version=4;",
    )
    .unwrap();
    drop(db);

    let mut store = Store::open(&path, StoreOptions::default(), 200).unwrap();
    let backups: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().contains("before-world-v5-"))
        .collect();
    assert_eq!(backups.len(), 1);
    let old = rusqlite::Connection::open(&backups[0]).unwrap();
    assert_eq!(old.query_row::<u32, _, _>("SELECT version FROM world_schema", [], |r| r.get(0)).unwrap(), 4);
    assert_eq!(old.pragma_query_value::<u32, _>(None, "user_version", |r| r.get(0)).unwrap(), 4);
    drop(old);
    let backups_v6: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().contains("before-world-v6-"))
        .collect();
    assert_eq!(backups_v6.len(), 1);

    // Old v2 and v3 protocol requests are explicitly rejected with DomainError::Version.
    let old_err = store.world_execute(&token(1), &Request { version: 2, action: Action::Identity {} }, 200).unwrap_err();
    assert!(matches!(old_err, Error::Domain(DomainError::Version(2))));
    let v3_err = store.world_execute(&token(1), &Request { version: 3, action: Action::Identity {} }, 200).unwrap_err();
    assert!(matches!(v3_err, Error::Domain(DomainError::Version(3))));

    // Append a v2 brush stroke and verify schema promotion + presence preview round-trip.
    let brush = StrokeBrush {
        authored_size: 40.0,
        hardness: 0.0,
        opacity: 0.5,
        flow: 0.1,
        build_up: true,
        build_up_rate: 25.0,
        tip: BrushTip::Procedural { style: ProceduralTipStyle::Chalk, size: 64, seed: 7, param: 0 },
        seed: 42,
        ..Default::default()
    };
    let samples = vec![StrokeSample { time_ms: 0.0, ..Default::default() }, StrokeSample { time_ms: 80.0, ..Default::default() }];
    let preview_style = PreviewStyle {
        id: StrokeId::new(&hex(502)).unwrap(),
        anchor: RegionKey::default(),
        width: 40.0,
        color: Srgba { rgba: [0.8, 0.1, 0.2, 1.0] },
        erase: false,
        brush: Some(Box::new(brush.clone())),
        samples: Some(samples.clone()),
    };
    let Response::Peers { peers } = store
        .world_execute(
            &token(1),
            &Request {
                version: VERSION,
                action: Action::Presence {
                    canvas: c,
                    epoch: 1,
                    camera: coordinates::home(c),
                    preview: vec![[20.0, 20.0], [80.0, 40.0]],
                    cursor: None,
                    stroke: Some(preview_style.clone()),
                },
            },
            210,
        )
        .unwrap()
    else {
        panic!("expected peers")
    };
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].stroke.as_ref(), Some(&preview_style));

    let brush_stroke = Stroke {
        id: StrokeId::new(&hex(502)).unwrap(),
        canvas: c,
        owner: owner(1),
        revision: 0,
        anchor: RegionKey::default(),
        points: vec![[20.0, 20.0], [80.0, 40.0]],
        width: 40.0,
        color: Srgba { rgba: [0.8, 0.1, 0.2, 1.0] },
        erase: false,
        brush: Some(Box::new(brush.clone())),
        samples: Some(samples.clone()),
    };
    store
        .world_execute(
            &token(1),
            &Request {
                version: VERSION,
                action: Action::Append {
                    operation: WorldOp {
                        id: OpId::new(&hex(602)).unwrap(),
                        canvas: c,
                        author: owner(1),
                        device: device(1),
                        sequence: 2,
                        authority_epoch: 1,
                        action: Edit::AddStroke { stroke: brush_stroke.clone() },
                    },
                },
            },
            220,
        )
        .unwrap();

    let Response::Events { events, .. } =
        store.world_execute(&token(1), &Request { version: VERSION, action: Action::Events { canvas: c, after: 0, through: None, limit: 16 } }, 230).unwrap()
    else {
        panic!("expected events")
    };
    assert_eq!(events.len(), 2);
    let mut rebuilt = WorldState::new(c, owner(1), "Pre-v5 canvas".into()).unwrap();
    rebuilt.apply(&events[0]).unwrap();
    assert_eq!(rebuilt.schema, SHARED_SCHEMA);
    rebuilt.apply(&events[1]).unwrap();
    assert_eq!(rebuilt.schema, SHARED_SCHEMA_BRUSH);
    assert_eq!(rebuilt.strokes[&brush_stroke.id].stroke.brush.as_deref(), Some(&brush));
    assert_eq!(rebuilt.strokes[&brush_stroke.id].stroke.samples.as_ref(), Some(&samples));
    let manifest = ArchiveManifest { schema: 2, world: rebuilt, assets: vec![] };
    manifest.validate().unwrap();

    // Backup & restore of v5 DB containing brush-v2 world succeeds cleanly.
    let snap = dir.path().join("v5-backup.sqlite3");
    store.backup(&snap).unwrap();
    let restored = dir.path().join("v5-restored.sqlite3");
    Store::copy_verified(&snap, &restored).unwrap();
    let reopened = Store::open(&restored, StoreOptions::default(), 300).unwrap();
    drop(reopened);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_clients_http_brush_sync_presence_undo_offline_restart_and_v2_rejection() {
    use photocraft_sync::server;
    use reqwest::StatusCode;

    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("service.sqlite3");
    let service = server::spawn("127.0.0.1:0".parse().unwrap(), &db_path, StoreOptions::default()).await.unwrap();
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(15)).build().unwrap();

    let pair_http = |addr: std::net::SocketAddr, n: u128| {
        let http = http.clone();
        async move {
            let rpc = format!("http://{addr}/v1/rpc");
            let req = RpcRequest {
                protocol_version: photocraft_party::protocol::PROTOCOL_VERSION,
                request_id: RequestId::new(&hex(4000 + n)).unwrap(),
                operation: Operation::Pair {
                    participant: Participant {
                        id: ParticipantId::new(&hex(n)).unwrap(),
                        device: PartyDevice::new(&hex(n + 1000)).unwrap(),
                        display_name: format!("HTTP Artist {n}"),
                    },
                    credential: token(n),
                },
            };
            let resp = http.post(&rpc).json(&req).send().await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }
    };
    let world_call = |addr: std::net::SocketAddr, n: u128, action: Action| {
        let http = http.clone();
        async move {
            let url = format!("http://{addr}/v1/canvas");
            let resp = http.post(&url).bearer_auth(token(n)).json(&Request { version: VERSION, action }).send().await.unwrap();
            let status = resp.status();
            let bytes = resp.bytes().await.unwrap();
            assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
            serde_json::from_slice::<Response>(&bytes).unwrap()
        }
    };

    pair_http(service.address, 1).await;
    pair_http(service.address, 2).await;

    // 1. Old v2 client hitting /v1/canvas gets explicit 426 Upgrade Required (no silent downgrade).
    let old_resp = http
        .post(format!("http://{}/v1/canvas", service.address))
        .bearer_auth(token(1))
        .json(&Request { version: 2, action: Action::Identity {} })
        .send()
        .await
        .unwrap();
    assert_eq!(old_resp.status(), StatusCode::UPGRADE_REQUIRED);

    // 2. Client A creates a shared canvas, invites Client B, Client B accepts.
    let c = canvas(10);
    world_call(service.address, 1, Action::Create { canvas: c, name: "Two-client brush world".into() }).await;
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    world_call(
        service.address,
        1,
        Action::Invite { canvas: c, id: OpId::new(&hex(7001)).unwrap(), recipient: owner(2), secret: token(7001), expires_at_ms: now_ms + 3_600_000 },
    )
    .await;
    world_call(service.address, 2, Action::Accept { canvas: c, id: OpId::new(&hex(7001)).unwrap(), secret: token(7001) }).await;

    // 3. Client A publishes live brush Presence preview; Client B receives exact brush + timed samples.
    let soft_brush =
        StrokeBrush { authored_size: 36.0, hardness: 0.15, opacity: 0.45, flow: 0.25, build_up: true, build_up_rate: 25.0, seed: 77, ..Default::default() };
    let timed_samples = vec![StrokeSample { time_ms: 0.0, ..Default::default() }, StrokeSample { time_ms: 120.0, ..Default::default() }];
    let preview_style = PreviewStyle {
        id: StrokeId::new(&hex(8001)).unwrap(),
        anchor: RegionKey::default(),
        width: 36.0,
        color: Srgba { rgba: [0.1, 0.4, 0.9, 0.45] },
        erase: false,
        brush: Some(Box::new(soft_brush.clone())),
        samples: Some(timed_samples.clone()),
    };
    world_call(
        service.address,
        1,
        Action::Presence {
            canvas: c,
            epoch: 1,
            camera: coordinates::home(c),
            preview: vec![[0.0, 0.0], [50.0, 20.0]],
            cursor: Some(coordinates::point(&RegionKey::default(), [50.0, 20.0]).unwrap()),
            stroke: Some(preview_style.clone()),
        },
    )
    .await;
    let Response::Peers { peers } =
        world_call(service.address, 2, Action::Presence { canvas: c, epoch: 1, camera: coordinates::home(c), preview: vec![], cursor: None, stroke: None })
            .await
    else {
        panic!("expected peers response");
    };
    let peer_a = peers.iter().find(|p| p.profile == owner(1)).expect("client B must see client A peer");
    assert_eq!(peer_a.stroke.as_ref(), Some(&preview_style));

    // 4. Client A appends translucent brush + soft eraser; Client B appends Chalk brush.
    let stroke_a1 = Stroke {
        id: StrokeId::new(&hex(8001)).unwrap(),
        canvas: c,
        owner: owner(1),
        revision: 0,
        anchor: RegionKey::default(),
        points: vec![[0.0, 0.0], [50.0, 20.0]],
        width: 36.0,
        color: Srgba { rgba: [0.1, 0.4, 0.9, 0.45] },
        erase: false,
        brush: Some(Box::new(soft_brush.clone())),
        samples: Some(timed_samples.clone()),
    };
    let eraser_a2 = Stroke {
        id: StrokeId::new(&hex(8002)).unwrap(),
        canvas: c,
        owner: owner(1),
        revision: 0,
        anchor: RegionKey::default(),
        points: vec![[20.0, 10.0], [35.0, 15.0]],
        width: 20.0,
        color: Srgba { rgba: [0.0, 0.0, 0.0, 0.6] },
        erase: true,
        brush: Some(Box::new(StrokeBrush { authored_size: 20.0, hardness: 0.2, opacity: 0.6, seed: 78, ..Default::default() })),
        samples: None,
    };
    let chalk_b1 = Stroke {
        id: StrokeId::new(&hex(8003)).unwrap(),
        canvas: c,
        owner: owner(2),
        revision: 0,
        anchor: RegionKey::default(),
        points: vec![[-20.0, 30.0], [40.0, -10.0]],
        width: 24.0,
        color: Srgba { rgba: [0.85, 0.25, 0.1, 0.8] },
        erase: false,
        brush: Some(Box::new(StrokeBrush {
            authored_size: 24.0,
            opacity: 0.8,
            tip: BrushTip::Procedural { style: ProceduralTipStyle::Chalk, size: 64, seed: 3, param: 0 },
            seed: 79,
            ..Default::default()
        })),
        samples: None,
    };
    world_call(
        service.address,
        1,
        Action::Append {
            operation: WorldOp {
                id: OpId::new(&hex(9001)).unwrap(),
                canvas: c,
                author: owner(1),
                device: device(1),
                sequence: 1,
                authority_epoch: 1,
                action: Edit::AddStroke { stroke: stroke_a1.clone() },
            },
        },
    )
    .await;
    world_call(
        service.address,
        1,
        Action::Append {
            operation: WorldOp {
                id: OpId::new(&hex(9002)).unwrap(),
                canvas: c,
                author: owner(1),
                device: device(1),
                sequence: 2,
                authority_epoch: 1,
                action: Edit::AddStroke { stroke: eraser_a2.clone() },
            },
        },
    )
    .await;
    world_call(
        service.address,
        2,
        Action::Append {
            operation: WorldOp {
                id: OpId::new(&hex(9003)).unwrap(),
                canvas: c,
                author: owner(2),
                device: device(2),
                sequence: 1,
                authority_epoch: 1,
                action: Edit::AddStroke { stroke: chalk_b1.clone() },
            },
        },
    )
    .await;

    // 5. Author-local undo & redo for Client A; cross-author undo of Client B's stroke is rejected.
    world_call(
        service.address,
        1,
        Action::Append {
            operation: WorldOp {
                id: OpId::new(&hex(9004)).unwrap(),
                canvas: c,
                author: owner(1),
                device: device(1),
                sequence: 3,
                authority_epoch: 1,
                action: Edit::SetStrokeVisible { stroke: eraser_a2.id, visible: false },
            },
        },
    )
    .await;
    let cross_undo = http
        .post(format!("http://{}/v1/canvas", service.address))
        .bearer_auth(token(1))
        .json(&Request {
            version: VERSION,
            action: Action::Append {
                operation: WorldOp {
                    id: OpId::new(&hex(9005)).unwrap(),
                    canvas: c,
                    author: owner(1),
                    device: device(1),
                    sequence: 4,
                    authority_epoch: 1,
                    action: Edit::SetStrokeVisible { stroke: chalk_b1.id, visible: false },
                },
            },
        })
        .send()
        .await
        .unwrap();
    assert!(cross_undo.status().is_client_error(), "cross-author undo must be rejected");
    world_call(
        service.address,
        1,
        Action::Append {
            operation: WorldOp {
                id: OpId::new(&hex(9006)).unwrap(),
                canvas: c,
                author: owner(1),
                device: device(1),
                sequence: 4,
                authority_epoch: 1,
                action: Edit::SetStrokeVisible { stroke: eraser_a2.id, visible: true },
            },
        },
    )
    .await;

    // 6. Server shutdown (offline period) + Client A queues an offline brush op + server restart/reopen + sync!
    service.shutdown().await.unwrap();
    let offline_op = WorldOp {
        id: OpId::new(&hex(9007)).unwrap(),
        canvas: c,
        author: owner(1),
        device: device(1),
        sequence: 5,
        authority_epoch: 1,
        action: Edit::AddStroke {
            stroke: Stroke {
                id: StrokeId::new(&hex(8004)).unwrap(),
                canvas: c,
                owner: owner(1),
                revision: 0,
                anchor: RegionKey { depth: 6, ..Default::default() },
                points: vec![[0.0, 0.0], [16.0, 12.0]],
                width: 8.0,
                color: Srgba { rgba: [0.2, 0.7, 0.3, 0.5] },
                erase: false,
                brush: Some(Box::new(StrokeBrush { authored_size: 16.0, hardness: 0.5, opacity: 0.5, seed: 80, ..Default::default() })),
                samples: None,
            },
        },
    };
    let restarted = server::spawn("127.0.0.1:0".parse().unwrap(), &db_path, StoreOptions::default()).await.unwrap();
    world_call(restarted.address, 1, Action::Append { operation: offline_op }).await;

    // 7. Both clients pull full event history after reopen and verify identical canonical_hash & brush fidelity.
    let pull = |n: u128| async move {
        let Response::Events { events, .. } = world_call(restarted.address, n, Action::Events { canvas: c, after: 0, through: None, limit: 64 }).await else {
            panic!("expected events");
        };
        let mut world = WorldState::new(c, owner(1), "Two-client brush world".into()).unwrap();
        for ev in &events {
            world.apply(ev).unwrap();
        }
        world
    };
    let world_a = pull(1).await;
    let world_b = pull(2).await;
    assert_eq!(world_a.schema, SHARED_SCHEMA_BRUSH);
    assert_eq!(world_a.canonical_hash().unwrap(), world_b.canonical_hash().unwrap());
    assert_eq!(world_a.strokes.len(), 4);
    assert!(world_a.strokes[&eraser_a2.id].visible);
    assert_eq!(world_b.strokes[&stroke_a1.id].stroke.brush.as_deref(), Some(&soft_brush));
    assert_eq!(world_b.strokes[&stroke_a1.id].stroke.samples.as_ref(), Some(&timed_samples));
    assert_eq!(world_b.strokes[&chalk_b1.id].stroke.brush.as_deref(), chalk_b1.brush.as_deref());
    restarted.shutdown().await.unwrap();
}
