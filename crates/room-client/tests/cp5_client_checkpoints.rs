#![cfg(not(target_arch = "wasm32"))]
//! CP5 client tests for WorldOutbox Schema v4, WorldReplica, and WorldWorker:
//! - Pinned checkpoint download + tail events catchup + concurrent appends
//! - Checkpoint rotation retry (409 Conflict -> clear stage -> re-fetch meta)
//! - Mid-transfer revocation (401 Unauthorized -> Event::Revoked + recovery preservation)
//! - Zip-bomb, compressed_sha256 mismatch, state_hash mismatch, invalid structure & stale rev rejection
//!   without altering pending/recovery operations or personal viewer state
//! - Interrupted checkpoint transfer resume across WorldOutbox reopen
use photocraft_party::protocol::RpcResponse;
use photocraft_room_client::{
    CredentialStore, Outbox, Result,
    world_queue::{ViewerState, WorldOutbox, WorldScope},
    world_worker::{Command, Event, WorldAccess, WorldWorker},
};
use photocraft_scene::{
    Bookmark, CanvasId, OwnerId, RegionKey, Srgba, Stroke, StrokeId,
    brush::{StrokeBrush, StrokeSample},
    coordinates,
    shared::{AcceptedOp, DeviceId, Edit, OpId, ReviewState, WorldOp, WorldState, chain_hash_init, chain_hash_step, media::AssetHash},
    world_protocol::{self as wire, Action, CanvasSummary, CheckpointRef, Response},
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Default, Clone)]
struct Credentials(Arc<Mutex<BTreeMap<(String, String), String>>>);
impl CredentialStore for Credentials {
    fn get(&self, n: &str, k: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(&(n.into(), k.into())).cloned())
    }
    fn set(&self, n: &str, k: &str, s: &str) -> Result<()> {
        self.0.lock().unwrap().insert((n.into(), k.into()), s.into());
        Ok(())
    }
    fn remove(&self, n: &str, k: &str) -> Result<()> {
        self.0.lock().unwrap().remove(&(n.into(), k.into()));
        Ok(())
    }
}

fn hex(n: u128) -> String {
    format!("{n:032x}")
}

fn make_stroke(canvas: CanvasId, owner: OwnerId, idx: u64) -> Stroke {
    let mut points = Vec::with_capacity(40);
    let mut samples = Vec::with_capacity(40);
    for i in 0..40 {
        let t = i as f64;
        points.push([((idx % 200) as f64 - 100.0) * 2.0 + t, ((i % 8) as f64 - 4.0) * 0.5]);
        samples.push(StrokeSample { time_ms: t * 5.0, ..Default::default() });
    }
    Stroke {
        id: StrokeId::new(&hex(50_000 + u128::from(idx))).unwrap(),
        canvas,
        owner,
        revision: 0,
        anchor: RegionKey::default(),
        points,
        width: 10.0,
        color: Srgba { rgba: [0.2, 0.5, 0.8, 0.9] },
        erase: false,
        brush: Some(Box::new(StrokeBrush { authored_size: 10.0, hardness: 0.5, opacity: 0.9, seed: idx, ..Default::default() })),
        samples: Some(samples),
    }
}

fn build_world_with_checkpoint(canvas: CanvasId, owner: OwnerId, dev: DeviceId, ckpt_rev: u64, head_rev: u64) -> (WorldState, CheckpointRef, Vec<u8>) {
    let initial_name = "Checkpoint Canvas".to_owned();
    let mut world = WorldState::new(canvas, owner, initial_name.clone()).unwrap();
    let mut chain = chain_hash_init(canvas, owner, &initial_name);
    let mut ckpt_data = None;
    for rev in 1..=head_rev {
        let op = WorldOp {
            id: OpId::new(&hex(100_000 + u128::from(rev))).unwrap(),
            canvas,
            author: owner,
            device: dev,
            sequence: rev,
            authority_epoch: 1,
            action: Edit::AddStroke { stroke: make_stroke(canvas, owner, rev) },
        };
        let ev = AcceptedOp { revision: rev, op };
        let ev_json = serde_json::to_vec(&ev).unwrap();
        let ev_hash = format!("{:x}", Sha256::digest(&ev_json));
        chain = chain_hash_step(&chain, rev, &ev_hash);
        world.apply(&ev).unwrap();
        if rev == ckpt_rev {
            let ckpt = world.to_checkpoint();
            let (compressed, comp_sha, state_hash, uncompressed) = ckpt.encode_zlib().unwrap();
            let ckpt_ref = CheckpointRef {
                canvas,
                revision: rev,
                schema_ver: ckpt.schema,
                chain_hash: AssetHash::new(&chain).unwrap(),
                state_hash: AssetHash::new(&state_hash).unwrap(),
                compressed_sha256: AssetHash::new(&comp_sha).unwrap(),
                compressed_bytes: compressed.len() as u64,
                uncompressed_bytes: uncompressed,
            };
            ckpt_data = Some((ckpt_ref, compressed));
        }
    }
    let (ckpt_ref, compressed) = ckpt_data.unwrap();
    (world, ckpt_ref, compressed)
}

#[test]
fn outbox_checkpoint_staging_rejects_zipbomb_bad_hashes_invalid_state_and_resumes_cleanly() {
    let root = tempfile::tempdir().unwrap();
    let db_path = root.path().join("client.sqlite3");
    let profile = OwnerId::new(&hex(1)).unwrap();
    let device = DeviceId::new(&hex(10)).unwrap();
    let peer_dev = DeviceId::new(&hex(20)).unwrap();
    let canvas = CanvasId::new(&hex(100)).unwrap();
    let scope = WorldScope::new("https://example.invalid", profile, device).unwrap();

    let (world, ckpt_ref, compressed) = build_world_with_checkpoint(canvas, profile, peer_dev, 512, 520);
    let info = CanvasSummary {
        canvas,
        owner: profile,
        name: world.name.clone(),
        initial_name: world.initial_name().to_owned(),
        revision: world.revision,
        epoch: 1,
        import_origin: None,
    };

    let mut queue = WorldOutbox::open(&db_path).unwrap();
    let lease = queue.activate(scope.clone()).unwrap();
    queue.grant(&lease, canvas, 1, 1).unwrap();
    // Seed initial rev-0 head, a personal viewer camera/bookmark, and a pending offline edit!
    queue.save_snapshot(&lease, &WorldState::new(canvas, profile, info.initial_name.clone()).unwrap()).unwrap();
    let viewer = ViewerState {
        camera: coordinates::home(canvas),
        bookmarks: vec![Bookmark { name: "My Spot".into(), camera: coordinates::home(canvas) }],
        review: Some(ReviewState::first_visit(canvas, profile, 0).unwrap()),
    };
    queue.save_view(&lease, canvas, &viewer).unwrap();
    let offline_op = queue.enqueue(&lease, canvas, Edit::AddStroke { stroke: make_stroke(canvas, profile, 9999) }).unwrap();

    // 1. Zip-bomb (> 128 MiB uncompressed zeros compressed to ~130 KiB) MUST fail bounded decompression!
    {
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        let zeros = vec![0u8; 1024 * 1024];
        for _ in 0..130 {
            enc.write_all(&zeros).unwrap();
        }
        let bomb = enc.finish().unwrap();
        let bomb_sha = AssetHash::new(&format!("{:x}", Sha256::digest(&bomb))).unwrap();
        let bomb_ref = CheckpointRef {
            canvas,
            revision: 512,
            schema_ver: ckpt_ref.schema_ver,
            chain_hash: ckpt_ref.chain_hash,
            state_hash: ckpt_ref.state_hash,
            compressed_sha256: bomb_sha,
            compressed_bytes: bomb.len() as u64,
            uncompressed_bytes: wire::MAX_CHECKPOINT_UNCOMPRESSED_BYTES,
        };
        assert_eq!(queue.begin_checkpoint_stage(&lease, &bomb_ref).unwrap(), 0);
        let mut off = 0usize;
        while off < bomb.len() {
            let end = (off + wire::CHECKPOINT_CHUNK_BYTES as usize).min(bomb.len());
            queue.stage_checkpoint_chunk(&lease, canvas, 512, bomb_sha, off as u64, &bomb[off..end], bomb.len() as u64).unwrap();
            off = end;
        }
        assert!(queue.publish_checkpoint(&lease, &info, &bomb_ref).is_err(), "zip-bomb must be rejected");
        // Verify pending op, viewer state, and rev-0 head are 100% intact!
        assert_eq!(queue.pending(&lease, 10).unwrap()[0].operation, offline_op);
        assert_eq!(queue.load_view(&lease, canvas).unwrap(), Some(viewer.clone()));
        assert_eq!(queue.load_snapshot(&lease, canvas).unwrap().unwrap().revision, 0);
    }

    // 2. Compressed SHA-256 mismatch MUST fail before publish!
    {
        let mut corrupted = compressed.clone();
        corrupted[0] ^= 0xff;
        assert_eq!(queue.begin_checkpoint_stage(&lease, &ckpt_ref).unwrap(), 0);
        queue.stage_checkpoint_chunk(&lease, canvas, ckpt_ref.revision, ckpt_ref.compressed_sha256, 0, &corrupted, ckpt_ref.compressed_bytes).unwrap();
        assert!(queue.publish_checkpoint(&lease, &info, &ckpt_ref).is_err());
        assert_eq!(queue.load_snapshot(&lease, canvas).unwrap().unwrap().revision, 0);
    }

    // 3. State hash mismatch (valid compressed_sha256, wrong state_hash) MUST fail!
    {
        let mut bad_state_ref = ckpt_ref.clone();
        bad_state_ref.state_hash = AssetHash::new(&"f".repeat(64)).unwrap();
        assert_eq!(queue.begin_checkpoint_stage(&lease, &bad_state_ref).unwrap(), 0);
        queue
            .stage_checkpoint_chunk(&lease, canvas, bad_state_ref.revision, bad_state_ref.compressed_sha256, 0, &compressed, bad_state_ref.compressed_bytes)
            .unwrap();
        assert!(queue.publish_checkpoint(&lease, &info, &bad_state_ref).is_err());
    }

    // 4. Interrupted multi-chunk download resumes across WorldOutbox close/reopen, publishes atomically,
    //    preserves offline_op & viewer, and fast-loads via checkpoint + tail events!
    queue.clear_checkpoint_stage(&lease, canvas).unwrap();
    let mid = compressed.len() / 2;
    assert_eq!(queue.begin_checkpoint_stage(&lease, &ckpt_ref).unwrap(), 0);
    let r1 =
        queue.stage_checkpoint_chunk(&lease, canvas, ckpt_ref.revision, ckpt_ref.compressed_sha256, 0, &compressed[..mid], ckpt_ref.compressed_bytes).unwrap();
    assert_eq!(r1, mid as u64);
    // Idempotent retry of first chunk succeeds at same offset.
    assert_eq!(
        queue.stage_checkpoint_chunk(&lease, canvas, ckpt_ref.revision, ckpt_ref.compressed_sha256, 0, &compressed[..mid], ckpt_ref.compressed_bytes).unwrap(),
        mid as u64
    );
    drop(queue);

    // Reopen after crash/restart -> resumes from `mid`!
    let mut queue = WorldOutbox::open(&db_path).unwrap();
    let lease = queue.activate(scope.clone()).unwrap();
    assert_eq!(queue.begin_checkpoint_stage(&lease, &ckpt_ref).unwrap(), mid as u64);
    let r2 = queue
        .stage_checkpoint_chunk(&lease, canvas, ckpt_ref.revision, ckpt_ref.compressed_sha256, mid as u64, &compressed[mid..], ckpt_ref.compressed_bytes)
        .unwrap();
    assert_eq!(r2, ckpt_ref.compressed_bytes);

    let published = queue.publish_checkpoint(&lease, &info, &ckpt_ref).unwrap();
    assert_eq!(published.revision, 512);
    assert_eq!(published.base_revision(), 512);
    assert_eq!(published.strokes.len(), 512);
    // Append remaining 8 tail events incrementally!
    let tail = world.events_after(512, 128).unwrap();
    queue.append_accepted_events(&lease, &info, tail, None).unwrap();

    // Pending offline edit & viewer bookmarks MUST be 100% preserved!
    assert_eq!(queue.pending(&lease, 10).unwrap()[0].operation, offline_op);
    assert_eq!(queue.load_view(&lease, canvas).unwrap(), Some(viewer));

    // Reopen again and verify fast load_snapshot reconstructs rev 520!
    drop(queue);
    let mut queue = WorldOutbox::open(&db_path).unwrap();
    let lease = queue.activate(scope).unwrap();
    let reopened = queue.load_snapshot(&lease, canvas).unwrap().unwrap();
    assert_eq!(reopened.revision, 520);
    assert_eq!(reopened.strokes.len(), 520);

    // Stale/older checkpoint cannot overwrite newer durable rev 520!
    assert_eq!(queue.begin_checkpoint_stage(&lease, &ckpt_ref).unwrap(), 0);
    queue.stage_checkpoint_chunk(&lease, canvas, ckpt_ref.revision, ckpt_ref.compressed_sha256, 0, &compressed, ckpt_ref.compressed_bytes).unwrap();
    assert!(queue.publish_checkpoint(&lease, &info, &ckpt_ref).is_err());
    assert_eq!(queue.load_snapshot(&lease, canvas).unwrap().unwrap().revision, 520);
}

fn read_request(socket: &mut TcpStream) -> wire::Request {
    socket.set_nonblocking(false).unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let n = socket.read(&mut buffer).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buffer[..n]);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&bytes[..end]);
            let length = header
                .lines()
                .find_map(|line| {
                    line.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("content-length")).map(|(_, v)| v.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return wire::Request::decode(&bytes[end + 4..end + 4 + length]).unwrap();
            }
        }
    }
}

fn write_json(socket: &mut TcpStream, status: &str, body: &[u8]) {
    let head = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    let _ = socket.write_all(head.as_bytes());
    let _ = socket.write_all(body);
}

#[test]
fn worker_downloads_checkpoint_retries_on_rotation_conflict_and_handles_mid_transfer_revocation() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());

    let mut room = Outbox::open(root.path().join("room.db"), Box::new(Credentials::default())).unwrap();
    room.configure(&endpoint, "Checkpoint Worker").unwrap();
    let pair = room.next().unwrap().unwrap();
    let profile = room.status().profile.unwrap();
    room.acknowledge(&pair, &RpcResponse::Paired { participant: profile.clone() }).unwrap();
    let owner = OwnerId::new(profile.id.as_str()).unwrap();
    let peer_dev = DeviceId::new(&hex(777)).unwrap();

    let c_rotate = CanvasId::new(&hex(301)).unwrap();
    let c_revoke = CanvasId::new(&hex(302)).unwrap();

    // Canvas 1: rev 1030 with checkpoints at 512 (old) and 1024 (rotated!).
    let (world_rot, ckpt_512, bytes_512) = build_world_with_checkpoint(c_rotate, owner, peer_dev, 512, 1030);
    let ckpt_1024_obj = world_rot.replay_to(1024).unwrap().to_checkpoint();
    let (bytes_1024, sha_1024, state_1024, uncomp_1024) = ckpt_1024_obj.encode_zlib().unwrap();
    let mut chain_1024 = chain_hash_init(c_rotate, owner, world_rot.initial_name());
    for rev in 1..=1024_u64 {
        let ev = &world_rot.events_after(rev - 1, 1).unwrap()[0];
        let h = format!("{:x}", Sha256::digest(serde_json::to_vec(ev).unwrap()));
        chain_1024 = chain_hash_step(&chain_1024, rev, &h);
    }
    let ckpt_1024 = CheckpointRef {
        canvas: c_rotate,
        revision: 1024,
        schema_ver: ckpt_1024_obj.schema,
        chain_hash: AssetHash::new(&chain_1024).unwrap(),
        state_hash: AssetHash::new(&state_1024).unwrap(),
        compressed_sha256: AssetHash::new(&sha_1024).unwrap(),
        compressed_bytes: bytes_1024.len() as u64,
        uncompressed_bytes: uncomp_1024,
    };

    // Canvas 2: rev 520 with checkpoint at 512 (revoked on chunk request!).
    let (world_rev, ckpt_rev, _bytes_rev) = build_world_with_checkpoint(c_revoke, owner, peer_dev, 512, 520);

    let stop = Arc::new(AtomicBool::new(false));
    let meta_calls = Arc::new(AtomicU32::new(0));
    let conflict_injected = Arc::new(AtomicBool::new(false));

    let stop_s = stop.clone();
    let meta_s = meta_calls.clone();
    let conflict_s = conflict_injected.clone();
    let server = std::thread::spawn(move || {
        while !stop_s.load(Ordering::SeqCst) {
            let (mut socket, _) = match listener.accept() {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(e) => panic!("{e}"),
            };
            let req = read_request(&mut socket);
            match req.action {
                Action::List { .. } => {
                    let body = serde_json::to_vec(&Response::Canvases { items: vec![], next: None }).unwrap();
                    write_json(&mut socket, "200 OK", &body);
                }
                Action::Info { canvas } if canvas == c_rotate => {
                    let body = serde_json::to_vec(&Response::Canvas {
                        info: CanvasSummary {
                            canvas: c_rotate,
                            owner,
                            name: world_rot.name.clone(),
                            initial_name: world_rot.initial_name().to_owned(),
                            revision: world_rot.revision,
                            epoch: 1,
                            import_origin: None,
                        },
                    })
                    .unwrap();
                    write_json(&mut socket, "200 OK", &body);
                }
                Action::Info { canvas } if canvas == c_revoke => {
                    let body = serde_json::to_vec(&Response::Canvas {
                        info: CanvasSummary {
                            canvas: c_revoke,
                            owner,
                            name: world_rev.name.clone(),
                            initial_name: world_rev.initial_name().to_owned(),
                            revision: world_rev.revision,
                            epoch: 1,
                            import_origin: None,
                        },
                    })
                    .unwrap();
                    write_json(&mut socket, "200 OK", &body);
                }
                Action::CheckpointMeta { canvas } if canvas == c_rotate => {
                    let n = meta_s.fetch_add(1, Ordering::SeqCst);
                    let chosen = if n == 0 { ckpt_512.clone() } else { ckpt_1024.clone() };
                    let body = serde_json::to_vec(&Response::CheckpointMeta { checkpoint: Some(chosen) }).unwrap();
                    write_json(&mut socket, "200 OK", &body);
                }
                Action::CheckpointMeta { canvas } if canvas == c_revoke => {
                    let body = serde_json::to_vec(&Response::CheckpointMeta { checkpoint: Some(ckpt_rev.clone()) }).unwrap();
                    write_json(&mut socket, "200 OK", &body);
                }
                Action::CheckpointChunk { canvas, revision, compressed_sha256, offset, len } if canvas == c_rotate => {
                    if revision == 512 && !conflict_s.swap(true, Ordering::SeqCst) {
                        // Simulate server rotating out rev-512 mid-transfer -> 409 Conflict!
                        let fault =
                            serde_json::to_vec(&photocraft_party::protocol::Fault { code: "conflict".into(), message: "checkpoint rotated".into() }).unwrap();
                        write_json(&mut socket, "409 Conflict", &fault);
                    } else {
                        let buf = if revision == 512 { &bytes_512 } else { &bytes_1024 };
                        let start = offset as usize;
                        let end = (start + (len as usize).min(32_768)).min(buf.len());
                        let body = serde_json::to_vec(&Response::CheckpointChunk {
                            revision,
                            compressed_sha256,
                            offset,
                            bytes: buf[start..end].to_vec(),
                            total: buf.len() as u64,
                        })
                        .unwrap();
                        write_json(&mut socket, "200 OK", &body);
                    }
                }
                Action::CheckpointChunk { canvas, .. } if canvas == c_revoke => {
                    let fault =
                        serde_json::to_vec(&photocraft_party::protocol::Fault { code: "unauthorized".into(), message: "revoked mid-transfer".into() }).unwrap();
                    write_json(&mut socket, "403 Forbidden", &fault);
                }
                Action::Events { canvas, after, through, limit } if canvas == c_rotate => {
                    let through = through.unwrap_or(world_rot.revision);
                    let slice = world_rot.events_after(after, (limit as usize).min(128)).unwrap();
                    let events: Vec<_> = slice.iter().filter(|e| e.revision <= through).cloned().collect();
                    let last = events.last().map_or(after, |e| e.revision);
                    let body = serde_json::to_vec(&Response::Events { events, through, next: (last < through).then_some(last) }).unwrap();
                    write_json(&mut socket, "200 OK", &body);
                }
                other => panic!("unexpected action: {other:?}"),
            }
        }
    });

    let worker = WorldWorker::start(root.path().join("world.db"), None).unwrap();
    worker.send(Command::Access(WorldAccess::from_room(&room).unwrap())).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut activated = false;
    while Instant::now() < deadline {
        if let Ok(Event::Activated { scope: Some(_), .. }) = worker.poll() {
            activated = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(activated);

    // 1. Open c_rotate: sees rev-512 meta -> gets 409 Conflict -> clears stage -> re-fetches rev-1024 meta -> downloads chunks + 6 tail events!
    worker.send(Command::Open(c_rotate)).unwrap();
    let mut synced_rot = false;
    while Instant::now() < deadline {
        if let Ok(Event::State { info, canonical, .. }) = worker.poll()
            && info.canvas == c_rotate
            && canonical.revision == 1030
        {
            assert_eq!(canonical.base_revision(), 1024);
            assert_eq!(canonical.strokes.len(), 1030);
            synced_rot = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(synced_rot, "worker failed to converge after checkpoint rotation conflict");
    assert!(conflict_injected.load(Ordering::SeqCst));
    assert!(meta_calls.load(Ordering::SeqCst) >= 2);

    // 2. Open c_revoke: gets 403 Unauthorized on CheckpointChunk -> emits Event::Revoked!
    worker.send(Command::Open(c_revoke)).unwrap();
    let mut revoked = false;
    while Instant::now() < deadline {
        if let Ok(Event::Revoked { canvas }) = worker.poll()
            && canvas == c_revoke
        {
            revoked = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(revoked, "worker did not emit Event::Revoked on unauthorized checkpoint chunk");

    stop.store(true, Ordering::SeqCst);
    drop(worker);
    server.join().unwrap();
}

#[test]
#[ignore = "CP6 --release client rev-10500 SQLite size, incremental append & cold-reopen benchmark"]
fn client_outbox_rev_10500_sqlite_size_append_and_cold_reopen_release() {
    let root = tempfile::tempdir().unwrap();
    let db_path = root.path().join("client-10500.sqlite3");
    let profile = OwnerId::new(&hex(1)).unwrap();
    let device = DeviceId::new(&hex(10)).unwrap();
    let peer_dev = DeviceId::new(&hex(20)).unwrap();
    let canvas = CanvasId::new(&hex(555)).unwrap();
    let scope = WorldScope::new("https://example.invalid", profile, device).unwrap();

    // Build 10,500-rev world (60-pt strokes, checkpoint at 10,240 + 260 tail events).
    let initial_name = "Client 10500 Endurance".to_owned();
    let mut world = WorldState::new(canvas, profile, initial_name.clone()).unwrap();
    let mut chain = chain_hash_init(canvas, profile, &initial_name);
    let mut ckpt_data = None;
    for rev in 1..=10_500_u64 {
        let mut points = Vec::with_capacity(60);
        let mut samples = Vec::with_capacity(60);
        let bx = ((rev % 120) as f64 - 60.0) * 6.0;
        let by = (((rev / 120) % 120) as f64 - 60.0) * 5.0;
        for i in 0..60 {
            let t = i as f64;
            points.push([bx + t * 0.5, by + ((i % 8) as f64 - 4.0) * 0.5]);
            samples.push(StrokeSample { time_ms: t * 5.0, ..Default::default() });
        }
        let stroke = Stroke {
            id: StrokeId::new(&hex(900_000 + u128::from(rev))).unwrap(),
            canvas,
            owner: profile,
            revision: 0,
            anchor: RegionKey::default(),
            points,
            width: 12.0,
            color: Srgba { rgba: [0.2, 0.5, 0.8, 0.9] },
            erase: false,
            brush: Some(Box::new(StrokeBrush { authored_size: 12.0, hardness: 0.45, opacity: 0.85, seed: rev, ..Default::default() })),
            samples: Some(samples),
        };
        let ev = AcceptedOp {
            revision: rev,
            op: WorldOp {
                id: OpId::new(&hex(9_000_000 + u128::from(rev))).unwrap(),
                canvas,
                author: profile,
                device: peer_dev,
                sequence: rev,
                authority_epoch: 1,
                action: Edit::AddStroke { stroke },
            },
        };
        let ev_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&ev).unwrap()));
        chain = chain_hash_step(&chain, rev, &ev_hash);
        world.apply(&ev).unwrap();
        if rev == 10_240 {
            let ckpt = world.to_checkpoint();
            let (compressed, comp_sha, state_hash, uncompressed) = ckpt.encode_zlib().unwrap();
            let ckpt_ref = CheckpointRef {
                canvas,
                revision: rev,
                schema_ver: ckpt.schema,
                chain_hash: AssetHash::new(&chain).unwrap(),
                state_hash: AssetHash::new(&state_hash).unwrap(),
                compressed_sha256: AssetHash::new(&comp_sha).unwrap(),
                compressed_bytes: compressed.len() as u64,
                uncompressed_bytes: uncompressed,
            };
            ckpt_data = Some((ckpt_ref, compressed));
        }
    }
    let (ckpt_ref, compressed) = ckpt_data.unwrap();
    let info_10240 =
        CanvasSummary { canvas, owner: profile, name: world.name.clone(), initial_name: initial_name.clone(), revision: 10_240, epoch: 1, import_origin: None };

    let mut queue = WorldOutbox::open(&db_path).unwrap();
    let lease = queue.activate(scope.clone()).unwrap();
    queue.grant(&lease, canvas, 1, 1).unwrap();

    // Stage & publish checkpoint 10,240 in 256 KiB chunks.
    let t_pub0 = Instant::now();
    assert_eq!(queue.begin_checkpoint_stage(&lease, &ckpt_ref).unwrap(), 0);
    let mut off = 0usize;
    while off < compressed.len() {
        let end = (off + wire::CHECKPOINT_CHUNK_BYTES as usize).min(compressed.len());
        queue
            .stage_checkpoint_chunk(&lease, canvas, ckpt_ref.revision, ckpt_ref.compressed_sha256, off as u64, &compressed[off..end], ckpt_ref.compressed_bytes)
            .unwrap();
        off = end;
    }
    let _ = queue.publish_checkpoint(&lease, &info_10240, &ckpt_ref).unwrap();
    let publish_ckpt_ms = t_pub0.elapsed().as_secs_f64() * 1000.0;

    // Append 260 tail events incrementally one-by-one (measuring per-edit incremental SQLite latency!).
    let mut tail = Vec::with_capacity(260);
    let mut cursor = 10_240_u64;
    while cursor < 10_500 {
        let page = world.events_after(cursor, 128).unwrap();
        assert!(!page.is_empty());
        cursor = page.last().unwrap().revision;
        tail.extend_from_slice(page);
    }
    let mut append_ms = Vec::with_capacity(tail.len());
    for ev in &tail {
        let info = CanvasSummary {
            canvas,
            owner: profile,
            name: world.name.clone(),
            initial_name: initial_name.clone(),
            revision: ev.revision,
            epoch: 1,
            import_origin: None,
        };
        let t0 = Instant::now();
        queue.append_accepted_events(&lease, &info, std::slice::from_ref(ev), None).unwrap();
        append_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    drop(queue);

    // Measure 10 cold DB reopens (`WorldOutbox::open` + `activate` + `load_snapshot`).
    let mut reopen_ms = Vec::with_capacity(10);
    for _ in 0..10 {
        let t0 = Instant::now();
        let mut q = WorldOutbox::open(&db_path).unwrap();
        let l = q.activate(scope.clone()).unwrap();
        let snap = q.load_snapshot(&l, canvas).unwrap().unwrap();
        reopen_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(snap.revision, 10_500);
        assert_eq!(snap.strokes.len(), 10_500);
        assert_eq!(snap.base_revision(), 10_240);
    }

    let sqlite_bytes = std::fs::metadata(&db_path).unwrap().len();
    let sqlite_mib = sqlite_bytes as f64 / (1024.0 * 1024.0);
    assert!(sqlite_mib <= 25.0, "client sqlite {sqlite_mib:.2} MiB exceeded 25 MiB bound");

    append_ms.sort_by(|a, b| a.total_cmp(b));
    reopen_ms.sort_by(|a, b| a.total_cmp(b));
    let pct = |s: &[f64], p: f64| s[(((s.len() - 1) as f64) * p).round() as usize];

    let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out_dir = repo_root.join("log/k7/qol-009-redesign-dev/cp6");
    std::fs::create_dir_all(&out_dir).unwrap();
    let receipt = serde_json::json!({
        "profile": "release",
        "revisions": 10_500,
        "checkpoint_revision": 10_240,
        "tail_events": 260,
        "checkpoint_compressed_mib": compressed.len() as f64 / (1024.0 * 1024.0),
        "checkpoint_uncompressed_mib": ckpt_ref.uncompressed_bytes as f64 / (1024.0 * 1024.0),
        "publish_checkpoint_ms": publish_ckpt_ms,
        "client_sqlite_bytes": sqlite_bytes,
        "client_sqlite_mib": sqlite_mib,
        "incremental_append_p50_ms": pct(&append_ms, 0.50),
        "incremental_append_p95_ms": pct(&append_ms, 0.95),
        "incremental_append_max_ms": *append_ms.last().unwrap(),
        "cold_reopen_load_snapshot_p50_ms": pct(&reopen_ms, 0.50),
        "cold_reopen_load_snapshot_p95_ms": pct(&reopen_ms, 0.95),
        "cold_reopen_load_snapshot_max_ms": *reopen_ms.last().unwrap(),
    });
    let out_path = out_dir.join("client-10500-release-bench.json");
    let json = serde_json::to_string_pretty(&receipt).unwrap();
    std::fs::write(&out_path, &json).unwrap();
    eprintln!("=== CP6 CLIENT REV-10500 --release RECEIPT WRITTEN TO {} ===\n{}", out_path.display(), json);
}

#[test]
fn worker_keeps_durable_local_ack_responsive_during_large_checkpoint_verification() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());

    let mut room = Outbox::open(root.path().join("rooms.sqlite3"), Box::new(Credentials::default())).unwrap();
    room.configure(&endpoint, "Owner").unwrap();
    let pair = room.next().unwrap().unwrap();
    let party_profile = room.status().profile.unwrap();
    room.acknowledge(&pair, &RpcResponse::Paired { participant: party_profile.clone() }).unwrap();
    let profile = OwnerId::new(party_profile.id.as_str()).unwrap();
    let dev_local = DeviceId::new(party_profile.device.as_str()).unwrap();
    let dev_peer = DeviceId::new(&hex(72)).unwrap();
    let canvas_heavy = CanvasId::new(&hex(700)).unwrap();
    let canvas_other = CanvasId::new(&hex(701)).unwrap();

    // Build a 10,240-rev checkpoint in --release (1,536-rev in debug) while canvas_other receives
    // sequential 1-at-a-time durable local edits (measuring true per-edit Event::Saved latency).
    let ckpt_rev = if cfg!(debug_assertions) { 1_536_u64 } else { 10_240_u64 };
    let (heavy_world, ckpt_ref, compressed) = build_world_with_checkpoint(canvas_heavy, profile, dev_peer, ckpt_rev, ckpt_rev);
    let heavy_name = heavy_world.name.clone();
    let heavy_initial = heavy_world.initial_name().to_owned();

    let stop = Arc::new(AtomicBool::new(false));
    let stop_s = Arc::clone(&stop);
    let ckpt_ref_srv = ckpt_ref.clone();
    let compressed_srv = compressed.clone();
    let server_handle = std::thread::spawn(move || {
        let mut accepted_other = 0_u64;
        while !stop_s.load(Ordering::SeqCst) {
            let (mut stream, _) = match listener.accept() {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(_) => break,
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
            let req = read_request(&mut stream);
            match req.action {
                Action::List { .. } => {
                    let body = serde_json::to_vec(&Response::Canvases {
                        items: vec![
                            CanvasSummary {
                                canvas: canvas_heavy,
                                owner: profile,
                                name: heavy_name.clone(),
                                initial_name: heavy_initial.clone(),
                                revision: ckpt_rev,
                                epoch: 1,
                                import_origin: None,
                            },
                            CanvasSummary {
                                canvas: canvas_other,
                                owner: profile,
                                name: "Other Canvas".into(),
                                initial_name: "Other Canvas".into(),
                                revision: accepted_other,
                                epoch: 1,
                                import_origin: None,
                            },
                        ],
                        next: None,
                    })
                    .unwrap();
                    write_json(&mut stream, "200 OK", &body);
                }
                Action::Info { canvas } if canvas == canvas_heavy => {
                    let body = serde_json::to_vec(&Response::Canvas {
                        info: CanvasSummary {
                            canvas: canvas_heavy,
                            owner: profile,
                            name: heavy_name.clone(),
                            initial_name: heavy_initial.clone(),
                            revision: ckpt_rev,
                            epoch: 1,
                            import_origin: None,
                        },
                    })
                    .unwrap();
                    write_json(&mut stream, "200 OK", &body);
                }
                Action::Info { canvas } if canvas == canvas_other => {
                    let body = serde_json::to_vec(&Response::Canvas {
                        info: CanvasSummary {
                            canvas: canvas_other,
                            owner: profile,
                            name: "Other Canvas".into(),
                            initial_name: "Other Canvas".into(),
                            revision: accepted_other,
                            epoch: 1,
                            import_origin: None,
                        },
                    })
                    .unwrap();
                    write_json(&mut stream, "200 OK", &body);
                }
                Action::CheckpointMeta { canvas } if canvas == canvas_heavy => {
                    let body = serde_json::to_vec(&Response::CheckpointMeta { checkpoint: Some(ckpt_ref_srv.clone()) }).unwrap();
                    write_json(&mut stream, "200 OK", &body);
                }
                Action::CheckpointChunk { canvas, revision, compressed_sha256, offset, len } if canvas == canvas_heavy => {
                    assert_eq!(revision, ckpt_ref_srv.revision);
                    assert_eq!(compressed_sha256, ckpt_ref_srv.compressed_sha256);
                    let start = offset as usize;
                    let end = (start + len as usize).min(compressed_srv.len());
                    let body = serde_json::to_vec(&Response::CheckpointChunk {
                        revision,
                        compressed_sha256,
                        offset,
                        bytes: compressed_srv[start..end].to_vec(),
                        total: compressed_srv.len() as u64,
                    })
                    .unwrap();
                    write_json(&mut stream, "200 OK", &body);
                }
                Action::Append { operation } if operation.canvas == canvas_other => {
                    accepted_other += 1;
                    let ev = AcceptedOp { revision: accepted_other, op: operation };
                    let body = serde_json::to_vec(&Response::Accepted { event: Box::new(ev) }).unwrap();
                    write_json(&mut stream, "200 OK", &body);
                }
                Action::Events { canvas, after, .. } if canvas == canvas_other => {
                    let body = serde_json::to_vec(&Response::Events { events: Vec::new(), through: after, next: None }).unwrap();
                    write_json(&mut stream, "200 OK", &body);
                }
                _ => {
                    let body = serde_json::to_vec(&Response::Done).unwrap();
                    write_json(&mut stream, "200 OK", &body);
                }
            }
        }
    });

    let db_path = root.path().join("worker-nonblocking.sqlite3");
    {
        let mut q = WorldOutbox::open(&db_path).unwrap();
        let scope = WorldScope::new(&endpoint, profile, dev_local).unwrap();
        let lease = q.activate(scope).unwrap();
        q.grant(&lease, canvas_other, 1, 1).unwrap();
        q.grant(&lease, canvas_heavy, 1, 1).unwrap();
        q.save_snapshot(&lease, &WorldState::new(canvas_other, profile, "Other Canvas".into()).unwrap()).unwrap();
    }

    let access = WorldAccess::from_room(&room).unwrap().unwrap();
    let worker = WorldWorker::start(db_path.clone(), None).unwrap();
    worker.send(Command::Access(Some(access))).unwrap();
    let act_deadline = Instant::now() + Duration::from_secs(10);
    let mut activated = false;
    while Instant::now() < act_deadline {
        if let Ok(Event::Activated { scope: Some(_), .. }) = worker.poll() {
            activated = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(activated, "worker must activate before concurrent checkpoint test");
    // Open canvas_other first so its replica is loaded and ready for immediate local edits.
    worker.send(Command::Open(canvas_other)).unwrap();
    let mut other_ready = false;
    while Instant::now() < act_deadline {
        if let Ok(Event::State { info, .. }) = worker.poll()
            && info.canvas == canvas_other
        {
            other_ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(other_ready, "canvas_other must load before concurrent checkpoint test");
    // Open heavy canvas -> begins pinned checkpoint download + off-thread verification, then switch
    // active selection back to canvas_other so concurrent local edits can commit immediately while
    // canvas_heavy finishes off-thread verification and atomic publication!
    worker.send(Command::Open(canvas_heavy)).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    worker.send(Command::Open(canvas_other)).unwrap();
    let sw_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < sw_deadline {
        if let Ok(Event::State { info, .. }) = worker.poll()
            && info.canvas == canvas_other
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    // Send sequential 1-at-a-time local edits on canvas_other (waiting for each Event::Saved before
    // sending the next) while canvas_heavy downloads & verifies its pinned checkpoint off-thread!
    let mut saved_latencies_ms = Vec::new();
    let mut heavy_ready = false;
    let mut edits_sent = 0_u64;
    let mut inflight_edit: Option<(u64, Instant)> = None;
    let deadline = Instant::now() + Duration::from_secs(30);

    while Instant::now() < deadline && (!heavy_ready || saved_latencies_ms.len() < 20) {
        if inflight_edit.is_none() && edits_sent < 20 {
            edits_sent += 1;
            let t0 = Instant::now();
            let seq = worker
                .send(Command::Edit { canvas: canvas_other, action: Edit::AddStroke { stroke: make_stroke(canvas_other, profile, 900_000 + edits_sent) } })
                .unwrap();
            inflight_edit = Some((seq, t0));
        }
        while let Ok(ev) = worker.poll() {
            match ev {
                Event::Saved { sequence, .. } => {
                    if let Some((exp_seq, t0)) = inflight_edit
                        && exp_seq == sequence
                    {
                        saved_latencies_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
                        inflight_edit = None;
                    }
                }
                Event::State { info, canonical, .. } if info.canvas == canvas_heavy && canonical.revision == ckpt_rev => {
                    heavy_ready = true;
                }
                Event::Error { message, .. } => panic!("unexpected worker error: {message}"),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    // Re-open canvas_heavy if its Ready event wasn't emitted while unselected; verify its off-thread checkpoint committed!
    if !heavy_ready {
        worker.send(Command::Open(canvas_heavy)).unwrap();
        let d2 = Instant::now() + Duration::from_secs(15);
        while Instant::now() < d2 && !heavy_ready {
            while let Ok(ev) = worker.poll() {
                if let Event::State { info, canonical, .. } = ev
                    && info.canvas == canvas_heavy
                    && canonical.revision == ckpt_rev
                {
                    heavy_ready = true;
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    assert!(heavy_ready, "heavy checkpoint canvas must become ready via off-thread verification");
    assert_eq!(saved_latencies_ms.len(), 20, "all 20 durable local Event::Saved ACKs must complete");

    saved_latencies_ms.sort_by(|a, b| a.total_cmp(b));
    let p50 = saved_latencies_ms[saved_latencies_ms.len() / 2];
    let p95 = saved_latencies_ms[(saved_latencies_ms.len() * 95) / 100];
    let max = *saved_latencies_ms.last().unwrap();
    eprintln!("[CP7 CLIENT NONBLOCKING CHECKPOINT] durable Event::Saved during {ckpt_rev}-rev checkpoint verify: p50={p50:.2}ms p95={p95:.2}ms max={max:.2}ms");
    if !cfg!(debug_assertions) {
        let out_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../log/k7/qol-009-redesign-dev/cp7-async-and-e2e");
        let _ = std::fs::create_dir_all(&out_dir);
        let rep = serde_json::json!({
            "profile": "release",
            "checkpoint_rev": ckpt_rev,
            "checkpoint_compressed_bytes": compressed.len(),
            "checkpoint_uncompressed_bytes": ckpt_ref.uncompressed_bytes,
            "concurrent_local_durable_ack_count": saved_latencies_ms.len(),
            "concurrent_local_durable_ack_p50_ms": p50,
            "concurrent_local_durable_ack_p95_ms": p95,
            "concurrent_local_durable_ack_max_ms": max,
        });
        let _ = std::fs::write(out_dir.join("client-concurrent-checkpoint-bench.json"), serde_json::to_string_pretty(&rep).unwrap());
    }

    stop.store(true, Ordering::SeqCst);
    drop(worker);
    let _ = server_handle.join();

    // Reopen DB cleanly and confirm heavy checkpoint survived restart.
    let mut reopened = WorldOutbox::open(&db_path).unwrap();
    let scope = WorldScope::new(&endpoint, profile, dev_local).unwrap();
    let lease = reopened.activate(scope).unwrap();
    let snap = reopened.load_snapshot(&lease, canvas_heavy).unwrap().unwrap();
    assert_eq!(snap.revision, ckpt_rev);
    assert_eq!(snap.strokes.len(), ckpt_rev as usize);
}

#[test]
fn worker_keeps_durable_local_ack_responsive_during_periodic_local_checkpoint_compression() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());

    let mut room = Outbox::open(root.path().join("room-local-ckpt.db"), Box::new(Credentials::default())).unwrap();
    room.configure(&endpoint, "Periodic Ckpt Worker").unwrap();
    let pair = room.next().unwrap().unwrap();
    let profile = room.status().profile.unwrap();
    room.acknowledge(&pair, &RpcResponse::Paired { participant: profile.clone() }).unwrap();
    let owner = OwnerId::new(profile.id.as_str()).unwrap();
    let dev_local = DeviceId::new(profile.device.as_str()).unwrap();
    let peer_dev = DeviceId::new(&hex(888)).unwrap();

    let canvas = CanvasId::new(&hex(409)).unwrap();
    // Seed at rev 10,239 in release (or 1,023 in dev) so the very next accepted edit hits the
    // 512-rev periodic local checkpoint boundary (10,240 / 1,024) and triggers CkptJob::CompressLocal!
    let trigger_rev: u64 = if cfg!(debug_assertions) { 1_024 } else { 10_240 };
    let base_ckpt_rev = trigger_rev - 512;
    let seed_rev = trigger_rev - 1;
    let (seed_world, _, _) = build_world_with_checkpoint(canvas, owner, peer_dev, base_ckpt_rev, seed_rev);
    let initial_name = seed_world.initial_name().to_owned();
    let world_name = seed_world.name.clone();

    let db_path = root.path().join("worker-local-ckpt.sqlite3");
    {
        let mut q = WorldOutbox::open(&db_path).unwrap();
        let scope = WorldScope::new(&endpoint, owner, dev_local).unwrap();
        let lease = q.activate(scope).unwrap();
        q.grant(&lease, canvas, 1, 1).unwrap();
        let prefix = seed_world.replay_to(base_ckpt_rev).unwrap();
        q.save_snapshot(&lease, &prefix).unwrap();
        let info =
            CanvasSummary { canvas, owner, name: world_name.clone(), initial_name: initial_name.clone(), revision: seed_rev, epoch: 1, import_origin: None };
        let mut after = base_ckpt_rev;
        while after < seed_rev {
            let page = seed_world.events_after(after, 256).unwrap();
            assert!(!page.is_empty());
            after = page.last().unwrap().revision;
            q.append_accepted_events(&lease, &info, page, None).unwrap();
        }
    }

    let stop = Arc::new(AtomicBool::new(false));
    let stop_s = Arc::clone(&stop);
    let srv_name = world_name.clone();
    let srv_initial = initial_name.clone();
    let server_handle = std::thread::spawn(move || {
        let mut head_rev = seed_rev;
        while !stop_s.load(Ordering::SeqCst) {
            let (mut stream, _) = match listener.accept() {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                Err(e) => panic!("{e}"),
            };
            let req = read_request(&mut stream);
            match req.action {
                Action::List { .. } => {
                    let body = serde_json::to_vec(&Response::Canvases {
                        items: vec![CanvasSummary {
                            canvas,
                            owner,
                            name: srv_name.clone(),
                            initial_name: srv_initial.clone(),
                            revision: head_rev,
                            epoch: 1,
                            import_origin: None,
                        }],
                        next: None,
                    })
                    .unwrap();
                    write_json(&mut stream, "200 OK", &body);
                }
                Action::Info { canvas: c } if c == canvas => {
                    let body = serde_json::to_vec(&Response::Canvas {
                        info: CanvasSummary {
                            canvas,
                            owner,
                            name: srv_name.clone(),
                            initial_name: srv_initial.clone(),
                            revision: head_rev,
                            epoch: 1,
                            import_origin: None,
                        },
                    })
                    .unwrap();
                    write_json(&mut stream, "200 OK", &body);
                }
                Action::Append { operation } if operation.canvas == canvas => {
                    head_rev += 1;
                    let ev = AcceptedOp { revision: head_rev, op: operation };
                    let body = serde_json::to_vec(&Response::Accepted { event: Box::new(ev) }).unwrap();
                    write_json(&mut stream, "200 OK", &body);
                }
                Action::Events { after, .. } => {
                    let body = serde_json::to_vec(&Response::Events { events: Vec::new(), through: after, next: None }).unwrap();
                    write_json(&mut stream, "200 OK", &body);
                }
                _ => {
                    let body = serde_json::to_vec(&Response::Done).unwrap();
                    write_json(&mut stream, "200 OK", &body);
                }
            }
        }
    });

    let access = WorldAccess::from_room(&room).unwrap().unwrap();
    let worker = WorldWorker::start(db_path.clone(), None).unwrap();
    worker.send(Command::Access(Some(access))).unwrap();
    let act_deadline = Instant::now() + Duration::from_secs(15);
    let mut activated = false;
    while Instant::now() < act_deadline {
        if let Ok(Event::Activated { scope: Some(_), .. }) = worker.poll() {
            activated = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(activated);
    worker.send(Command::Open(canvas)).unwrap();
    let mut ready = false;
    while Instant::now() < act_deadline {
        if let Ok(Event::State { info, canonical, .. }) = worker.poll()
            && info.canvas == canvas
            && canonical.revision == seed_rev
        {
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(ready, "seeded {seed_rev}-rev canvas must open cleanly");

    // Edit 0 crosses `trigger_rev` (10,240), firing off-thread CkptJob::CompressLocal on komaphone-world-ckpt.
    // Then 20 follow-up edits run immediately to prove neither local Event::Saved nor end-to-end sync ACK stalls!
    let mut trigger_saved_ms = 0.0_f64;
    let mut trigger_sync_ms = 0.0_f64;
    let mut followup_saved_ms = Vec::with_capacity(20);
    let mut followup_sync_ms = Vec::with_capacity(20);

    for i in 0..21_u64 {
        let target_rev = trigger_rev + i;
        let t0 = Instant::now();
        let seq = worker.send(Command::Edit { canvas, action: Edit::AddStroke { stroke: make_stroke(canvas, owner, 950_000 + i) } }).unwrap();
        let mut got_saved: Option<f64> = None;
        let mut got_synced: Option<f64> = None;
        let dl = Instant::now() + Duration::from_secs(15);
        while Instant::now() < dl && (got_saved.is_none() || got_synced.is_none()) {
            while let Ok(ev) = worker.poll() {
                match ev {
                    Event::Saved { sequence, .. } if sequence == seq => {
                        got_saved = Some(t0.elapsed().as_secs_f64() * 1000.0);
                    }
                    Event::State { info, canonical, pending, .. } if info.canvas == canvas && canonical.revision >= target_rev && pending == 0 => {
                        got_synced = Some(t0.elapsed().as_secs_f64() * 1000.0);
                    }
                    Event::Error { message, .. } => panic!("unexpected worker error: {message}"),
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_micros(250));
        }
        let s_ms = got_saved.expect("Event::Saved received");
        let y_ms = got_synced.expect("sync ACK received");
        if i == 0 {
            trigger_saved_ms = s_ms;
            trigger_sync_ms = y_ms;
        } else {
            followup_saved_ms.push(s_ms);
            followup_sync_ms.push(y_ms);
        }
    }

    // Give off-thread local checkpoint compression time to publish into SQLite cleanly.
    std::thread::sleep(Duration::from_millis(900));
    stop.store(true, Ordering::SeqCst);
    drop(worker);
    let _ = server_handle.join();

    followup_saved_ms.sort_by(|a, b| a.total_cmp(b));
    followup_sync_ms.sort_by(|a, b| a.total_cmp(b));
    let s_p50 = followup_saved_ms[followup_saved_ms.len() / 2];
    let s_p95 = followup_saved_ms[(followup_saved_ms.len() * 95) / 100];
    let s_max = *followup_saved_ms.last().unwrap();
    let y_p50 = followup_sync_ms[followup_sync_ms.len() / 2];
    let y_p95 = followup_sync_ms[(followup_sync_ms.len() * 95) / 100];
    let y_max = *followup_sync_ms.last().unwrap();
    eprintln!(
        "[CP7 CLIENT PERIODIC LOCAL CKPT] trigger rev={trigger_rev}: saved={trigger_saved_ms:.2}ms sync={trigger_sync_ms:.2}ms | 20 followups: saved p50={s_p50:.2}ms p95={s_p95:.2}ms max={s_max:.2}ms | sync p50={y_p50:.2}ms p95={y_p95:.2}ms max={y_max:.2}ms"
    );

    // Reopen DB cleanly and confirm rev trigger_rev+20 survived AND rev trigger_rev checkpoint landed!
    let mut reopened = WorldOutbox::open(&db_path).unwrap();
    let scope = WorldScope::new(&endpoint, owner, dev_local).unwrap();
    let lease = reopened.activate(scope).unwrap();
    let snap = reopened.load_snapshot(&lease, canvas).unwrap().unwrap();
    assert_eq!(snap.revision, trigger_rev + 20);
    assert!(snap.base_revision() >= trigger_rev, "async periodic local checkpoint at >= {trigger_rev} must have published (got {})", snap.base_revision());

    if !cfg!(debug_assertions) {
        let out_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../log/k7/qol-009-redesign-dev/cp7-async-and-e2e");
        let _ = std::fs::create_dir_all(&out_dir);
        let rep = serde_json::json!({
            "profile": "release",
            "periodic_local_checkpoint_rev": trigger_rev,
            "trigger_edit_durable_saved_ms": trigger_saved_ms,
            "trigger_edit_loopback_sync_ack_ms": trigger_sync_ms,
            "followup_count": followup_saved_ms.len(),
            "followup_durable_saved_p50_ms": s_p50,
            "followup_durable_saved_p95_ms": s_p95,
            "followup_durable_saved_max_ms": s_max,
            "followup_loopback_sync_ack_p50_ms": y_p50,
            "followup_loopback_sync_ack_p95_ms": y_p95,
            "followup_loopback_sync_ack_max_ms": y_max,
            "async_local_checkpoint_base_revision_verified": snap.base_revision(),
        });
        let _ = std::fs::write(out_dir.join("client-periodic-local-checkpoint-bench.json"), serde_json::to_string_pretty(&rep).unwrap());
    }
}
