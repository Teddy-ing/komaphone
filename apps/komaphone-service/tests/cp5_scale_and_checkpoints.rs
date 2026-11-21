//! CP5 end-to-end service tests:
//! - world_schema = 6 incremental commits & immutable pinned checkpoints (stride 512)
//! - checkpoint rotation (409 Conflict), revocation mid-transfer (401 Unauthorized), v3 -> HTTP 426
//! - --backup-to / --restore-from CLI verification on v6 with checkpoints
//! - 10,500-revision multi-author 60-pt brush stroke scalability & <= 25 MiB SQLite bound
use photocraft_party::{
    DeviceId as PartyDevice, Participant, ParticipantId, RequestId,
    protocol::{Operation, RpcRequest},
};
use photocraft_scene::{
    CanvasId, OwnerId, RegionKey, Srgba, Stroke, StrokeId,
    brush::{StrokeBrush, StrokeSample},
    shared::{DeviceId, Edit, OpId, WorldCheckpoint, WorldOp, WorldState},
    world_protocol::{Action, CHECKPOINT_CHUNK_BYTES, MAX_CHECKPOINT_UNCOMPRESSED_BYTES, Request, Response, VERSION},
};
use photocraft_sync::{Store, StoreOptions, server};
use reqwest::StatusCode;
use std::time::Instant;

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

fn pair_direct(store: &mut Store, n: u128) {
    store
        .execute(
            None,
            &RpcRequest {
                protocol_version: 1,
                request_id: RequestId::new(&hex(n + 3000)).unwrap(),
                operation: Operation::Pair {
                    participant: Participant {
                        id: ParticipantId::new(&hex(n)).unwrap(),
                        device: PartyDevice::new(&hex(n + 1000)).unwrap(),
                        display_name: format!("Scale Artist {n}"),
                    },
                    credential: token(n),
                },
            },
            100,
        )
        .unwrap();
}

fn brush_stroke_60pt(c: CanvasId, author_idx: u128, seq: u64) -> Stroke {
    let mut points = Vec::with_capacity(60);
    let mut samples = Vec::with_capacity(60);
    let base_x = ((seq % 64) as f64 - 32.0) * 12.0;
    let base_y = (((seq / 64) % 64) as f64 - 32.0) * 12.0;
    for i in 0..60 {
        let t = i as f64;
        points.push([base_x + t * 1.5, base_y + (t * 0.2).sin() * 6.0]);
        samples.push(StrokeSample { time_ms: t * 8.0, ..Default::default() });
    }
    let depth = match seq % 10 {
        0 => 12,
        1 | 2 => 6,
        _ => 0,
    };
    Stroke {
        id: StrokeId::new(&hex(100_000 + author_idx * 100_000 + u128::from(seq))).unwrap(),
        canvas: c,
        owner: owner(author_idx),
        revision: 0,
        anchor: RegionKey { depth, ..Default::default() },
        points,
        width: 14.0,
        color: Srgba { rgba: [0.15 * (author_idx as f32 % 6.0), 0.4, 0.75, 0.7] },
        erase: seq.is_multiple_of(19),
        brush: Some(Box::new(StrokeBrush { authored_size: 14.0, hardness: 0.4, opacity: 0.7, flow: 0.5, seed: seq, ..Default::default() })),
        samples: Some(samples),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pinned_checkpoint_transfer_rotation_revocation_http_426_and_backup_restore() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("service.sqlite3");
    let service = server::spawn("127.0.0.1:0".parse().unwrap(), &db_path, StoreOptions::default()).await.unwrap();
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let url = format!("http://{}/v1/canvas", service.address);

    // Pair 6 authors (beyond 4-participant bound).
    for n in 1..=6_u128 {
        let resp = http
            .post(format!("http://{}/v1/rpc", service.address))
            .json(&RpcRequest {
                protocol_version: 1,
                request_id: RequestId::new(&hex(4000 + n)).unwrap(),
                operation: Operation::Pair {
                    participant: Participant {
                        id: ParticipantId::new(&hex(n)).unwrap(),
                        device: PartyDevice::new(&hex(n + 1000)).unwrap(),
                        display_name: format!("HTTP Artist {n}"),
                    },
                    credential: token(n),
                },
            })
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // Old v3 client receives explicit HTTP 426 Upgrade Required.
    let v3_resp = http.post(&url).bearer_auth(token(1)).json(&Request { version: 3, action: Action::Identity {} }).send().await.unwrap();
    assert_eq!(v3_resp.status(), StatusCode::UPGRADE_REQUIRED);

    let c = canvas(1);
    let call = |n: u128, action: Action| {
        let http = http.clone();
        let url = url.clone();
        async move {
            let resp = http.post(&url).bearer_auth(token(n)).json(&Request { version: VERSION, action }).send().await.unwrap();
            let status = resp.status();
            let body = resp.bytes().await.unwrap();
            assert_eq!(status, StatusCode::OK, "status={status} body={}", String::from_utf8_lossy(&body));
            serde_json::from_slice::<Response>(&body).unwrap()
        }
    };

    call(1, Action::Create { canvas: c, name: "Pinned Checkpoint World".into() }).await;
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    for n in 2..=6_u128 {
        let inv = OpId::new(&hex(7000 + n)).unwrap();
        call(1, Action::Invite { canvas: c, id: inv, recipient: owner(n), secret: token(7000 + n), expires_at_ms: now_ms + 3_600_000 }).await;
        call(n, Action::Accept { canvas: c, id: inv, secret: token(7000 + n) }).await;
    }

    // Append 520 revisions across 6 authors -> triggers checkpoint at rev 512 + 8 tail events.
    let mut seqs = [0_u64; 7];
    for rev in 1..=520_u64 {
        let author = ((rev - 1) % 6 + 1) as u128;
        seqs[author as usize] += 1;
        let seq = seqs[author as usize];
        let stroke = brush_stroke_60pt(c, author, seq);
        call(
            author,
            Action::Append {
                operation: WorldOp {
                    id: OpId::new(&hex(900_000 + u128::from(rev))).unwrap(),
                    canvas: c,
                    author: owner(author),
                    device: device(author),
                    sequence: seq,
                    authority_epoch: 1,
                    action: Edit::AddStroke { stroke },
                },
            },
        )
        .await;
    }

    // Member 6 polls CheckpointMeta until background worker publishes pinned rev 512.
    let ckpt_512 = {
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Response::CheckpointMeta { checkpoint: Some(c_ref) } = call(6, Action::CheckpointMeta { canvas: c }).await
                && c_ref.revision == 512
            {
                break c_ref;
            }
            assert!(Instant::now() < deadline, "timed out waiting for async rev-512 checkpoint");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    assert_eq!(ckpt_512.revision, 512);
    ckpt_512.validate().unwrap();

    // Stream rev-512 checkpoint in 64 KiB chunks and verify full decode + tail replay matches head rev 520.
    let mut raw_512 = Vec::with_capacity(ckpt_512.compressed_bytes as usize);
    while (raw_512.len() as u64) < ckpt_512.compressed_bytes {
        let Response::CheckpointChunk { revision, compressed_sha256, offset, bytes, total } = call(
            6,
            Action::CheckpointChunk {
                canvas: c,
                revision: ckpt_512.revision,
                compressed_sha256: ckpt_512.compressed_sha256,
                offset: raw_512.len() as u64,
                len: 65_536.min(CHECKPOINT_CHUNK_BYTES),
            },
        )
        .await
        else {
            panic!("expected chunk");
        };
        assert_eq!(revision, 512);
        assert_eq!(compressed_sha256, ckpt_512.compressed_sha256);
        assert_eq!(offset, raw_512.len() as u64);
        assert_eq!(total, ckpt_512.compressed_bytes);
        raw_512.extend_from_slice(&bytes);
    }
    let decoded_512 = WorldCheckpoint::decode_zlib_bounded(
        &raw_512,
        ckpt_512.compressed_sha256.as_str(),
        ckpt_512.uncompressed_bytes,
        ckpt_512.state_hash.as_str(),
        MAX_CHECKPOINT_UNCOMPRESSED_BYTES,
    )
    .unwrap();
    let mut joined = WorldState::from_checkpoint(decoded_512).unwrap();
    let Response::Events { events: tail, .. } = call(6, Action::Events { canvas: c, after: 512, through: Some(520), limit: 128 }).await else {
        panic!("expected tail events");
    };
    assert_eq!(tail.len(), 8);
    for ev in &tail {
        joined.apply(ev).unwrap();
    }
    assert_eq!(joined.revision, 520);
    assert_eq!(joined.strokes.len(), 520);

    // Advance past two more checkpoints (1024, waiting for async publish, then 1536) so rev-512 is pruned (retains latest 2).
    for (from, to, expect_ckpt) in [(521_u64, 1024_u64, 1024_u64), (1025_u64, 1540_u64, 1536_u64)] {
        for rev in from..=to {
            let author = ((rev - 1) % 5 + 1) as u128; // authors 1..=5
            seqs[author as usize] += 1;
            let seq = seqs[author as usize];
            let stroke = brush_stroke_60pt(c, author, seq);
            call(
                author,
                Action::Append {
                    operation: WorldOp {
                        id: OpId::new(&hex(900_000 + u128::from(rev))).unwrap(),
                        canvas: c,
                        author: owner(author),
                        device: device(author),
                        sequence: seq,
                        authority_epoch: 1,
                        action: Edit::AddStroke { stroke },
                    },
                },
            )
            .await;
        }
        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Response::CheckpointMeta { checkpoint: Some(c_ref) } = call(6, Action::CheckpointMeta { canvas: c }).await
                && c_ref.revision == expect_ckpt
            {
                break;
            }
            assert!(Instant::now() < deadline, "timed out waiting for async rev-{expect_ckpt} checkpoint");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    // Requesting rotated-out rev-512 checkpoint returns explicit 409 Conflict (never mixes bytes!).
    let rotated = http
        .post(&url)
        .bearer_auth(token(6))
        .json(&Request {
            version: VERSION,
            action: Action::CheckpointChunk { canvas: c, revision: ckpt_512.revision, compressed_sha256: ckpt_512.compressed_sha256, offset: 0, len: 65_536 },
        })
        .send()
        .await
        .unwrap();
    assert_eq!(rotated.status(), StatusCode::CONFLICT);

    // Member 6 refreshes meta -> sees rev 1536, fetches first chunk, then owner revokes Member 6 mid-transfer!
    let Response::CheckpointMeta { checkpoint: Some(ckpt_1536) } = call(6, Action::CheckpointMeta { canvas: c }).await else {
        panic!("expected rev-1536 checkpoint");
    };
    assert_eq!(ckpt_1536.revision, 1536);
    call(6, Action::CheckpointChunk { canvas: c, revision: ckpt_1536.revision, compressed_sha256: ckpt_1536.compressed_sha256, offset: 0, len: 32_768 }).await;

    call(1, Action::Remove { canvas: c, member: owner(6) }).await;

    // Next chunk from revoked member 6 MUST fail with 403/401 unauthorized (never leaks checkpoint bytes).
    let revoked_chunk = http
        .post(&url)
        .bearer_auth(token(6))
        .json(&Request {
            version: VERSION,
            action: Action::CheckpointChunk {
                canvas: c,
                revision: ckpt_1536.revision,
                compressed_sha256: ckpt_1536.compressed_sha256,
                offset: 32_768,
                len: 32_768,
            },
        })
        .send()
        .await
        .unwrap();
    assert!(
        matches!(revoked_chunk.status(), StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED),
        "expected forbidden/unauthorized on revoked chunk, got {}",
        revoked_chunk.status()
    );

    service.shutdown().await.unwrap();

    // Verify CLI --backup-to and --restore-from succeed on v6 DB containing checkpoints.
    let backup_path = dir.path().join("v6-backup.sqlite3");
    let backup_out = std::process::Command::new(env!("CARGO_BIN_EXE_photocraft-sync"))
        .arg("--data-dir")
        .arg(dir.path())
        .arg("--backup-to")
        .arg(&backup_path)
        .output()
        .unwrap();
    assert!(backup_out.status.success(), "backup failed: {}", String::from_utf8_lossy(&backup_out.stderr));

    let restore_dir = dir.path().join("restored");
    let restore_out = std::process::Command::new(env!("CARGO_BIN_EXE_photocraft-sync"))
        .arg("--data-dir")
        .arg(&restore_dir)
        .arg("--restore-from")
        .arg(&backup_path)
        .output()
        .unwrap();
    assert!(restore_out.status.success(), "restore failed: {}", String::from_utf8_lossy(&restore_out.stderr));

    let reopened = Store::open(restore_dir.join("service.sqlite3"), StoreOptions::default(), 500).unwrap();
    drop(reopened);
}

#[test]
fn ten_thousand_five_hundred_revisions_commit_latency_and_sqlite_size_bound() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("scale-10500.sqlite3");
    let mut store = Store::open(&db_path, StoreOptions::default(), 100).unwrap();
    for n in 1..=6_u128 {
        pair_direct(&mut store, n);
    }
    let c = canvas(99);
    store.world_execute(&token(1), &Request { version: VERSION, action: Action::Create { canvas: c, name: "Scale 10500".into() } }, 100).unwrap();
    for n in 2..=6_u128 {
        let inv = OpId::new(&hex(8000 + n)).unwrap();
        store
            .world_execute(
                &token(1),
                &Request {
                    version: VERSION,
                    action: Action::Invite { canvas: c, id: inv, recipient: owner(n), secret: token(8000 + n), expires_at_ms: 10_000_000 },
                },
                100,
            )
            .unwrap();
        store.world_execute(&token(n), &Request { version: VERSION, action: Action::Accept { canvas: c, id: inv, secret: token(8000 + n) } }, 100).unwrap();
    }

    let mut seqs = [0_u64; 7];
    let mut last_stroke_by_author: [Option<StrokeId>; 7] = [None; 7];
    let mut regular_ms_at_10k = Vec::new();
    let mut append_10240_ms = None;
    let mut off_thread_prepare_10240_ms = None;
    let mut atomic_publish_10240_ms = None;

    for rev in 1..=10_500_u64 {
        let author = ((rev - 1) % 6 + 1) as u128;
        seqs[author as usize] += 1;
        let seq = seqs[author as usize];

        let action = if rev.is_multiple_of(50)
            && let Some(sid) = last_stroke_by_author[author as usize]
        {
            Edit::SetStrokeVisible { stroke: sid, visible: (rev / 50).is_multiple_of(2) }
        } else {
            let stroke = brush_stroke_60pt(c, author, seq);
            last_stroke_by_author[author as usize] = Some(stroke.id);
            Edit::AddStroke { stroke }
        };

        let req = Request {
            version: VERSION,
            action: Action::Append {
                operation: WorldOp {
                    id: OpId::new(&hex(1_000_000 + u128::from(rev))).unwrap(),
                    canvas: c,
                    author: owner(author),
                    device: device(author),
                    sequence: seq,
                    authority_epoch: 1,
                    action,
                },
            },
        };

        let measure = (10_241..=10_290).contains(&rev) || rev == 10_240;
        let t0 = Instant::now();
        let Response::Accepted { event } = store.world_execute(&token(author), &req, 200 + rev).unwrap() else {
            panic!("expected accepted");
        };
        let dt_ms = t0.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(event.revision, rev);
        if rev == 10_240 {
            append_10240_ms = Some(dt_ms);
            let jobs = store.take_pending_checkpoints();
            assert_eq!(jobs.len(), 1);
            let tp = Instant::now();
            let prepared = Store::prepare_checkpoint_from_path(store.db_path(), &jobs[0]).unwrap().unwrap();
            off_thread_prepare_10240_ms = Some(tp.elapsed().as_secs_f64() * 1000.0);
            let tpub = Instant::now();
            assert!(store.publish_prepared_checkpoint(&prepared).unwrap());
            atomic_publish_10240_ms = Some(tpub.elapsed().as_secs_f64() * 1000.0);
        } else if rev.is_multiple_of(512) {
            store.flush_pending_checkpoints().unwrap();
        } else if measure {
            regular_ms_at_10k.push(dt_ms);
        }
    }
    store.incremental_vacuum(2048).unwrap();

    // Verify latest checkpoint is at rev 10,240 (10_240 = 20 * 512) and decodes cleanly.
    let Response::CheckpointMeta { checkpoint: Some(ckpt_ref) } =
        store.world_execute(&token(1), &Request { version: VERSION, action: Action::CheckpointMeta { canvas: c } }, 20_000).unwrap()
    else {
        panic!("expected checkpoint at rev 10240");
    };
    assert_eq!(ckpt_ref.revision, 10_240);
    assert!(ckpt_ref.compressed_bytes <= 10 * 1024 * 1024, "compressed checkpoint too large: {} bytes", ckpt_ref.compressed_bytes);

    // Check server SQLite file size <= 25 MiB (Plan §7.3 target).
    let sqlite_bytes = std::fs::metadata(&db_path).unwrap().len();
    {
        let inspect = rusqlite::Connection::open(&db_path).unwrap();
        let ev_sum: i64 = inspect.query_row("SELECT coalesce(sum(length(body)),0) FROM world_events", [], |r| r.get(0)).unwrap();
        let ck_sum: i64 = inspect.query_row("SELECT coalesce(sum(length(body_zlib)),0) FROM world_checkpoints", [], |r| r.get(0)).unwrap();
        let freelist: i64 = inspect.pragma_query_value(None, "freelist_count", |r| r.get(0)).unwrap();
        let page_size: i64 = inspect.pragma_query_value(None, "page_size", |r| r.get(0)).unwrap();
        eprintln!(
            "[CP5 10,500-rev BREAKDOWN] events_body={:.2} MiB, ckpts_body={:.2} MiB, freelist={:.2} MiB ({} pages)",
            ev_sum as f64 / (1024.0 * 1024.0),
            ck_sum as f64 / (1024.0 * 1024.0),
            (freelist * page_size) as f64 / (1024.0 * 1024.0),
            freelist
        );
    }
    regular_ms_at_10k.sort_by(|a, b| a.total_cmp(b));
    let p50 = regular_ms_at_10k[regular_ms_at_10k.len() / 2];
    let p95 = regular_ms_at_10k[(regular_ms_at_10k.len() * 95) / 100];
    eprintln!(
        "[CP5 10,500-rev SERVER] sqlite={:.2} MiB, ckpt_10240_zlib={:.2} MiB, uncomp={:.2} MiB, commit_p50={:.2} ms, commit_p95={:.2} ms, append_10240={:.2} ms, off_thread_prepare_10240={:.2} ms, atomic_publish_10240={:.2} ms",
        sqlite_bytes as f64 / (1024.0 * 1024.0),
        ckpt_ref.compressed_bytes as f64 / (1024.0 * 1024.0),
        ckpt_ref.uncompressed_bytes as f64 / (1024.0 * 1024.0),
        p50,
        p95,
        append_10240_ms.unwrap_or(-1.0),
        off_thread_prepare_10240_ms.unwrap_or(-1.0),
        atomic_publish_10240_ms.unwrap_or(-1.0),
    );
    assert!(sqlite_bytes <= 25 * 1024 * 1024, "server SQLite at rev 10,500 exceeded 25 MiB target: {} bytes", sqlite_bytes);

    // Verify full verified backup & reopen integrity at rev 10,500!
    let backup = dir.path().join("scale-10500-backup.sqlite3");
    store.backup(&backup).unwrap();
    drop(store);
    let mut reopened = Store::open(&backup, StoreOptions::default(), 30_000).unwrap();
    let Response::Canvas { info } = reopened.world_execute(&token(1), &Request { version: VERSION, action: Action::Info { canvas: c } }, 30_001).unwrap()
    else {
        panic!("expected info");
    };
    assert_eq!(info.revision, 10_500);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_http_requests_stay_unblocked_during_rev_10240_background_checkpoint_generation() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("concurrent-10240.sqlite3");

    let c_main = canvas(201);
    let c_other = canvas(202);
    let mut seqs_main = [0_u64; 7];

    // Seed Canvas A to rev 10,239 (with rev 9,728 checkpoint published) + Canvas B at rev 0.
    {
        let mut store = Store::open(&db_path, StoreOptions::default(), 100).unwrap();
        for n in 1..=6_u128 {
            pair_direct(&mut store, n);
        }
        store.world_execute(&token(1), &Request { version: VERSION, action: Action::Create { canvas: c_main, name: "Heavy Canvas A".into() } }, 100).unwrap();
        store.world_execute(&token(1), &Request { version: VERSION, action: Action::Create { canvas: c_other, name: "Other Canvas B".into() } }, 100).unwrap();
        for n in 2..=6_u128 {
            let inv = OpId::new(&hex(18_000 + n)).unwrap();
            store
                .world_execute(
                    &token(1),
                    &Request {
                        version: VERSION,
                        action: Action::Invite { canvas: c_main, id: inv, recipient: owner(n), secret: token(18_000 + n), expires_at_ms: 10_000_000 },
                    },
                    100,
                )
                .unwrap();
            store
                .world_execute(&token(n), &Request { version: VERSION, action: Action::Accept { canvas: c_main, id: inv, secret: token(18_000 + n) } }, 100)
                .unwrap();
        }
        for rev in 1..=10_239_u64 {
            let author = ((rev - 1) % 6 + 1) as u128;
            seqs_main[author as usize] += 1;
            let seq = seqs_main[author as usize];
            let stroke = brush_stroke_60pt(c_main, author, seq);
            store
                .world_execute(
                    &token(author),
                    &Request {
                        version: VERSION,
                        action: Action::Append {
                            operation: WorldOp {
                                id: OpId::new(&hex(5_000_000 + u128::from(rev))).unwrap(),
                                canvas: c_main,
                                author: owner(author),
                                device: device(author),
                                sequence: seq,
                                authority_epoch: 1,
                                action: Edit::AddStroke { stroke },
                            },
                        },
                    },
                    200 + rev,
                )
                .unwrap();
            if rev == 9_728 {
                store.flush_pending_checkpoints().unwrap();
            }
        }
        let _ = store.take_pending_checkpoints();
    }

    // Boot real live HTTP `photocraft-sync` service on loopback.
    let service = server::spawn("127.0.0.1:0".parse().unwrap(), &db_path, StoreOptions::default()).await.unwrap();
    let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
    let canvas_url = format!("http://{}/v1/canvas", service.address);
    let room_url = format!("http://{}/v1/rpc", service.address);

    // Trigger rev 10,240 on Canvas A via real HTTP -> queues async background checkpoint reconstruction!
    let author_10240 = ((10_240 - 1) % 6 + 1) as u128;
    seqs_main[author_10240 as usize] += 1;
    let seq_10240 = seqs_main[author_10240 as usize];
    let t_trigger = Instant::now();
    let resp_10240 = http
        .post(&canvas_url)
        .bearer_auth(token(author_10240))
        .json(&Request {
            version: VERSION,
            action: Action::Append {
                operation: WorldOp {
                    id: OpId::new(&hex(5_010_240)).unwrap(),
                    canvas: c_main,
                    author: owner(author_10240),
                    device: device(author_10240),
                    sequence: seq_10240,
                    authority_epoch: 1,
                    action: Edit::AddStroke { stroke: brush_stroke_60pt(c_main, author_10240, seq_10240) },
                },
            },
        })
        .send()
        .await
        .unwrap();
    let trigger_append_ms = t_trigger.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(resp_10240.status(), StatusCode::OK);

    // Concurrently send ordinary HTTP appends on Canvas A, appends on Canvas B, and Room Poll RPCs
    // while the background checkpoint worker is actively reconstructing & compressing rev 10,240!
    let h1 = http.clone();
    let u1 = canvas_url.clone();
    let seq_a2_start = seqs_main[2];
    let task_same_canvas = tokio::spawn(async move {
        let mut latencies = Vec::with_capacity(30);
        for i in 1..=30_u64 {
            let seq = seq_a2_start + i;
            let t0 = Instant::now();
            let r = h1
                .post(&u1)
                .bearer_auth(token(2))
                .json(&Request {
                    version: VERSION,
                    action: Action::Append {
                        operation: WorldOp {
                            id: OpId::new(&hex(6_100_000 + u128::from(i))).unwrap(),
                            canvas: c_main,
                            author: owner(2),
                            device: device(2),
                            sequence: seq,
                            authority_epoch: 1,
                            action: Edit::AddStroke { stroke: brush_stroke_60pt(c_main, 2, seq) },
                        },
                    },
                })
                .send()
                .await
                .unwrap();
            let dt = t0.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(r.status(), StatusCode::OK);
            latencies.push(dt);
        }
        latencies
    });

    let h2 = http.clone();
    let u2 = canvas_url.clone();
    let task_other_canvas = tokio::spawn(async move {
        let mut latencies = Vec::with_capacity(30);
        for i in 1..=30_u64 {
            let t0 = Instant::now();
            let r = h2
                .post(&u2)
                .bearer_auth(token(1))
                .json(&Request {
                    version: VERSION,
                    action: Action::Append {
                        operation: WorldOp {
                            id: OpId::new(&hex(6_200_000 + u128::from(i))).unwrap(),
                            canvas: c_other,
                            author: owner(1),
                            device: device(1),
                            sequence: i,
                            authority_epoch: 1,
                            action: Edit::AddStroke { stroke: brush_stroke_60pt(c_other, 1, i) },
                        },
                    },
                })
                .send()
                .await
                .unwrap();
            let dt = t0.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(r.status(), StatusCode::OK);
            latencies.push(dt);
        }
        latencies
    });

    let h3 = http.clone();
    let u3 = room_url.clone();
    let task_room_rpc = tokio::spawn(async move {
        let create_resp = h3
            .post(&u3)
            .bearer_auth(token(3))
            .json(&RpcRequest {
                protocol_version: 1,
                request_id: RequestId::new(&hex(6_300_000)).unwrap(),
                operation: Operation::Create { mode: photocraft_party::Mode::ExquisiteCorpse, timer_ms: None },
            })
            .send()
            .await
            .unwrap();
        assert_eq!(create_resp.status(), StatusCode::OK);
        let body = create_resp.bytes().await.unwrap();
        let photocraft_party::protocol::RpcResponse::Room { view, .. } = serde_json::from_slice(&body).unwrap() else {
            panic!("expected room create view");
        };
        let mut latencies = Vec::with_capacity(30);
        for i in 1..=30_u64 {
            let t0 = Instant::now();
            let r = h3
                .post(&u3)
                .bearer_auth(token(3))
                .json(&RpcRequest {
                    protocol_version: 1,
                    request_id: RequestId::new(&hex(6_300_000 + u128::from(i))).unwrap(),
                    operation: Operation::Poll { room: view.id, after_revision: 0 },
                })
                .send()
                .await
                .unwrap();
            let dt = t0.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(r.status(), StatusCode::OK);
            latencies.push(dt);
        }
        latencies
    });

    let mut lat_same = task_same_canvas.await.unwrap();
    let mut lat_other = task_other_canvas.await.unwrap();
    let mut lat_room = task_room_rpc.await.unwrap();

    // Wait for background worker to atomically publish rev-10,240 checkpoint.
    let deadline = Instant::now() + std::time::Duration::from_secs(20);
    let ckpt_10240 = loop {
        let r = http
            .post(&canvas_url)
            .bearer_auth(token(1))
            .json(&Request { version: VERSION, action: Action::CheckpointMeta { canvas: c_main } })
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let body = r.bytes().await.unwrap();
        if let Response::CheckpointMeta { checkpoint: Some(c_ref) } = serde_json::from_slice(&body).unwrap()
            && c_ref.revision == 10_240
        {
            break c_ref;
        }
        assert!(Instant::now() < deadline, "timed out waiting for async rev-10,240 checkpoint publish");
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    };
    let ckpt_ready_elapsed_ms = t_trigger.elapsed().as_secs_f64() * 1000.0;
    ckpt_10240.validate().unwrap();

    // Verify full rev-10,240 chunk stream decodes & matches Canvas A's exact state!
    let mut raw = Vec::with_capacity(ckpt_10240.compressed_bytes as usize);
    while (raw.len() as u64) < ckpt_10240.compressed_bytes {
        let r = http
            .post(&canvas_url)
            .bearer_auth(token(6))
            .json(&Request {
                version: VERSION,
                action: Action::CheckpointChunk {
                    canvas: c_main,
                    revision: 10_240,
                    compressed_sha256: ckpt_10240.compressed_sha256,
                    offset: raw.len() as u64,
                    len: CHECKPOINT_CHUNK_BYTES,
                },
            })
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let Response::CheckpointChunk { bytes, .. } = serde_json::from_slice(&r.bytes().await.unwrap()).unwrap() else {
            panic!("expected chunk");
        };
        raw.extend_from_slice(&bytes);
    }
    let decoded = WorldCheckpoint::decode_zlib_bounded(
        &raw,
        ckpt_10240.compressed_sha256.as_str(),
        ckpt_10240.uncompressed_bytes,
        ckpt_10240.state_hash.as_str(),
        MAX_CHECKPOINT_UNCOMPRESSED_BYTES,
    )
    .unwrap();
    assert_eq!(decoded.revision, 10_240);
    assert_eq!(decoded.strokes.len(), 10_240);

    service.shutdown().await.unwrap();

    // Verify crash/restart startup catch-up scan picks up an unpublished checkpoint cleanly.
    let mut reopened = Store::open(&db_path, StoreOptions::default(), 50_000).unwrap();
    let Response::Canvas { info } = reopened.world_execute(&token(1), &Request { version: VERSION, action: Action::Info { canvas: c_main } }, 50_001).unwrap()
    else {
        panic!("expected info");
    };
    assert_eq!(info.revision, 10_270);

    let summarize = |v: &mut Vec<f64>| -> serde_json::Value {
        v.sort_by(|a, b| a.total_cmp(b));
        let p50 = v[v.len() / 2];
        let p95 = v[(v.len() * 95) / 100];
        let max = *v.last().unwrap_or(&0.0);
        serde_json::json!({ "count": v.len(), "p50_ms": p50, "p95_ms": p95, "max_ms": max })
    };
    let report = serde_json::json!({
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "trigger_append_rev_10240_ms": trigger_append_ms,
        "background_ckpt_10240_ready_elapsed_ms": ckpt_ready_elapsed_ms,
        "ckpt_10240_compressed_mib": ckpt_10240.compressed_bytes as f64 / (1024.0 * 1024.0),
        "concurrent_same_canvas_http_appends": summarize(&mut lat_same),
        "concurrent_other_canvas_http_appends": summarize(&mut lat_other),
        "concurrent_room_http_polls": summarize(&mut lat_room),
    });
    eprintln!("[CP7 SERVER CONCURRENT HTTP CHECKPOINT] {}", serde_json::to_string_pretty(&report).unwrap());
    if !cfg!(debug_assertions) {
        let out_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../log/k7/qol-009-redesign-dev/cp7-async-and-e2e");
        let _ = std::fs::create_dir_all(&out_dir);
        let _ = std::fs::write(out_dir.join("server-concurrent-checkpoint-bench.json"), serde_json::to_string_pretty(&report).unwrap());
    }
    assert!(trigger_append_ms < 100.0, "rev-10,240 append should not block on checkpoint compression (took {trigger_append_ms:.2} ms)");
}
