//! CP7 real-process end-to-end verification of `photocraft.exe` + `photocraft-sync.exe`:
//! 1. Native OS window frame pacing (`worldRender.render_ms`, `perf.slowFrames`) on a real
//!    4,500-stroke + 2 overlapping animated PNG `SharedMedia` canvas during playback, pan/zoom,
//!    and live pointer drawing (`ui.worldInput`).
//! 2. Real native app + live `photocraft-sync.exe` rev-10,240 checkpoint boundary & rev-10,500
//!    end-to-end durable input-to-ACK latency while concurrent HTTP requests hammer another
//!    canvas and a party room on the same service.
//! 3. Real service-restart mid-sync and hard kill-after-ACK recovery at rev 10,501.
//!
//! Run with:
//! `cargo test --release -p photocraft-sync --test cp7_native_app_e2e --offline -- --ignored --nocapture`
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use photocraft_party::{
    DeviceId as PartyDevice, Mode, Participant, ParticipantId, RequestId, RoomId,
    protocol::{Operation, RpcRequest, RpcResponse},
};
use photocraft_scene::{
    CanvasId, OwnerId, RegionKey, Srgba, Stroke, StrokeId,
    brush::{BrushBlendMode, StrokeBrush, StrokeSample},
    coordinates,
    shared::{
        AcceptedOp, DeviceId, Edit, MediaId, OpId, WorldOp, WorldState,
        archive_import::ArchiveManifest,
        media::{AssetHash, Credit, MediaFrame, MediaPlacement, MediaSource, Provenance},
    },
    world_protocol::{Action, ImportObject, Request, Response, VERSION},
};
use photocraft_sync::{Store, StoreOptions};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const IMPORT_CHUNK_BYTES: usize = 64 * 1024;

fn hex(n: u128) -> String {
    format!("{n:032x}")
}
fn token(n: u128) -> String {
    format!("{n:064x}")
}
fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}
fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}
fn stats_json(mut samples: Vec<f64>) -> Value {
    samples.sort_by(|a, b| a.total_cmp(b));
    json!({
        "count": samples.len(),
        "p50_ms": pct(&samples, 0.50),
        "p95_ms": pct(&samples, 0.95),
        "max_ms": samples.last().copied().unwrap_or(0.0),
    })
}

struct ChildGuard(Option<Child>);
impl ChildGuard {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }
    fn kill_and_wait(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill_and_wait();
    }
}

struct ControlClient {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    next_id: u64,
}
impl ControlClient {
    fn connect(port: u16, token_path: &Path) -> Self {
        let deadline = Instant::now() + Duration::from_secs(30);
        let tok = loop {
            if let Ok(s) = std::fs::read_to_string(token_path) {
                let t = s.trim().to_owned();
                if t.len() == 64 {
                    break t;
                }
            }
            assert!(Instant::now() < deadline, "timed out waiting for control token file at {}", token_path.display());
            std::thread::sleep(Duration::from_millis(50));
        };
        let stream = loop {
            if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
                break s;
            }
            assert!(Instant::now() < deadline, "timed out connecting to photocraft --control port {port}");
            std::thread::sleep(Duration::from_millis(50));
        };
        stream.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
        stream.set_write_timeout(Some(Duration::from_secs(15))).unwrap();
        let writer = stream.try_clone().unwrap();
        let mut client = Self { reader: BufReader::new(stream), writer, next_id: 1 };
        let auth = client.call("auth", json!({"token": tok}));
        assert_eq!(auth["ok"], true, "control auth failed: {auth}");
        client
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let line = serde_json::to_string(&json!({"id": id, "method": method, "params": params})).unwrap();
        writeln!(self.writer, "{line}").unwrap();
        self.writer.flush().unwrap();
        let mut resp_line = String::new();
        self.reader.read_line(&mut resp_line).unwrap();
        serde_json::from_str(&resp_line).unwrap_or_else(|e| panic!("bad control reply `{resp_line}`: {e}"))
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let r = self.call(method, params);
        assert_eq!(r["ok"], true, "control `{method}` failed: {r}");
        r["result"].clone()
    }

    fn inspect_fast(&mut self) -> Value {
        self.ok("ui.inspect", json!({"worldHash": false}))
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
}

fn spawn_sync(bin: &Path, port: u16, data_dir: &Path) -> ChildGuard {
    let guard = ChildGuard::new(
        Command::new(bin)
            .arg("--bind")
            .arg(format!("127.0.0.1:{port}"))
            .arg("--data-dir")
            .arg(data_dir)
            .arg("--legacy-private-network")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("failed spawning {}: {e}", bin.display())),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return guard;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("photocraft-sync failed to listen on 127.0.0.1:{port}");
}

fn spawn_photocraft(bin: &Path, ctrl_port: u16, token_path: &Path, data_dir: &Path, cfg_dir: &Path, rw_dir: &Path) -> ChildGuard {
    let child = Command::new(bin)
        .arg("--new-instance")
        .arg("--control")
        .arg(ctrl_port.to_string())
        .arg("--control-token-file")
        .arg(token_path)
        .arg("--automation-read-root")
        .arg(rw_dir)
        .arg("--automation-write-root")
        .arg(rw_dir)
        .env("KOMAPHONE_DATA_DIR", data_dir)
        .env("PHOTOCRAFT_CONFIG_DIR", cfg_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("failed spawning {}: {e}", bin.display()));
    ChildGuard::new(child)
}

fn encode_checker_png(m_idx: u128, f_idx: u8) -> (Vec<u8>, AssetHash) {
    let mut img = image::RgbaImage::new(64, 64);
    for y in 0..64 {
        for x in 0..64 {
            let checker = ((x / 8 + y / 8 + u32::from(f_idx)) % 2) == 0;
            let r = if m_idx == 0 { if checker { 235 } else { 40 } } else { 45 };
            let g = if m_idx == 1 { if checker { 220 } else { 55 } } else { 145 };
            let b = if checker { 245 } else { 115 };
            img.put_pixel(x, y, image::Rgba([r, g, b, 225]));
        }
    }
    let mut bytes = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut bytes);
    image::DynamicImage::ImageRgba8(img).write_to(&mut cursor, image::ImageFormat::Png).unwrap();
    let hash = AssetHash::new(&format!("{:x}", Sha256::digest(&bytes))).unwrap();
    (bytes, hash)
}

fn build_anim4500_archive(helper_owner: OwnerId, helper_device: DeviceId) -> (Vec<u8>, Vec<(AssetHash, Vec<u8>)>) {
    let src_canvas = CanvasId::new(&hex(7001)).unwrap();
    let mut world = WorldState::new(src_canvas, helper_owner, "Native Anim 4500".into()).unwrap();
    let mut assets_meta = Vec::new();
    let mut asset_blobs = Vec::new();

    // 2 overlapping animated PNG SharedMedia placements (2 frames each, 200ms & 250ms loops).
    for m_idx in 0..2_u128 {
        let mut frames = Vec::new();
        for f_idx in 0..2_u8 {
            let (png, hash) = encode_checker_png(m_idx, f_idx);
            let frame = MediaFrame { hash, byte_len: png.len() as u64, width: 64, height: 64, duration_ms: if m_idx == 0 { 200 } else { 250 } };
            assets_meta.push(frame.clone());
            asset_blobs.push((hash, png));
            frames.push(frame);
        }
        let rev = world.revision + 1;
        let seq = world.next_sequence(helper_device).unwrap();
        let placement = MediaPlacement {
            id: MediaId::new(&hex(8800 + m_idx)).unwrap(),
            canvas: src_canvas,
            owner: helper_owner,
            anchor: RegionKey::default(),
            offset: if m_idx == 0 { [-100.0, -70.0] } else { [10.0, -35.0] },
            width: 140.0,
            height: 140.0,
            frames,
            looping: true,
            provenance: Provenance {
                source: MediaSource::Canvas { canvas: CanvasId::new(&hex(7999)).unwrap(), media: MediaId::new(&hex(8900 + m_idx)).unwrap() },
                source_revision: 1,
                artifact_hash: AssetHash::new(&format!("{:064x}", 900 + m_idx)).unwrap(),
                authors: vec![Credit { profile: helper_owner, label: "E2E Helper".into() }],
            },
        };
        world
            .apply(&AcceptedOp {
                revision: rev,
                op: WorldOp {
                    id: OpId::new(&hex(9900 + m_idx)).unwrap(),
                    canvas: src_canvas,
                    author: helper_owner,
                    device: helper_device,
                    sequence: seq,
                    authority_epoch: 1,
                    action: Edit::PlaceMedia { media: Box::new(placement) },
                },
            })
            .unwrap();
    }

    let palette = [[0.85, 0.22, 0.20, 0.85], [0.18, 0.52, 0.85, 0.85], [0.16, 0.66, 0.36, 0.85], [0.85, 0.56, 0.12, 0.85]];
    for i in 0..4_500_u64 {
        let angle = (i as f64) * 0.13;
        let radius = 20.0 + ((i % 250) as f64) * 1.4;
        let cx = angle.cos() * radius;
        let cy = angle.sin() * radius * 0.65;
        let pts: Vec<[f64; 2]> = (0..12).map(|k| [cx + (k as f64 * 2.5) * angle.cos(), cy + (k as f64 * 2.5) * angle.sin()]).collect();
        let brush = StrokeBrush {
            authored_size: 10.0 + (i % 10) as f32,
            hardness: if i.is_multiple_of(3) { 0.25 } else { 0.85 },
            spacing: 0.2,
            opacity: 0.75,
            flow: 0.6,
            mode: if i.is_multiple_of(29) { BrushBlendMode::Multiply } else { BrushBlendMode::Normal },
            seed: i + 1,
            ..Default::default()
        };
        let rev = world.revision + 1;
        let seq = world.next_sequence(helper_device).unwrap();
        let s = Stroke {
            id: StrokeId::new(&hex(500_000 + u128::from(i))).unwrap(),
            canvas: src_canvas,
            owner: helper_owner,
            revision: 0,
            anchor: RegionKey { depth: if i.is_multiple_of(20) { 6 } else { 0 }, ..Default::default() },
            points: pts,
            width: f64::from(brush.authored_size),
            color: Srgba { rgba: palette[(i % 4) as usize] },
            erase: i.is_multiple_of(37),
            brush: Some(Box::new(brush)),
            samples: None,
        };
        world
            .apply(&AcceptedOp {
                revision: rev,
                op: WorldOp {
                    id: OpId::new(&hex(15_000_000 + u128::from(rev))).unwrap(),
                    canvas: src_canvas,
                    author: helper_owner,
                    device: helper_device,
                    sequence: seq,
                    authority_epoch: 1,
                    action: Edit::AddStroke { stroke: s },
                },
            })
            .unwrap();
    }
    let manifest_bytes = ArchiveManifest { schema: 3, world, assets: assets_meta }.encode_v3().unwrap();
    (manifest_bytes, asset_blobs)
}

fn brush_stroke_60pt(c: CanvasId, author: OwnerId, seq: u64) -> Stroke {
    let mut points = Vec::with_capacity(60);
    let mut samples = Vec::with_capacity(60);
    let bx = ((seq % 64) as f64 - 32.0) * 12.0;
    let by = (((seq / 64) % 64) as f64 - 32.0) * 12.0;
    for i in 0..60 {
        let t = i as f64;
        points.push([bx + t * 1.5, by + (t * 0.2).sin() * 6.0]);
        samples.push(StrokeSample { time_ms: t * 8.0, ..Default::default() });
    }
    let depth = match seq % 10 {
        0 => 12,
        1 | 2 => 6,
        _ => 0,
    };
    Stroke {
        id: StrokeId::new(&hex(2_000_000 + u128::from(seq))).unwrap(),
        canvas: c,
        owner: author,
        revision: 0,
        anchor: RegionKey { depth, ..Default::default() },
        points,
        width: 14.0,
        color: Srgba { rgba: [0.20, 0.45, 0.80, 0.75] },
        erase: seq.is_multiple_of(23),
        brush: Some(Box::new(StrokeBrush { authored_size: 14.0, hardness: 0.45, opacity: 0.75, flow: 0.55, seed: seq.max(1), ..Default::default() })),
        samples: Some(samples),
    }
}

fn http_world(http: &reqwest::blocking::Client, url: &str, bearer: &str, action: Action) -> Response {
    let resp = http.post(url).bearer_auth(bearer).json(&Request { version: VERSION, action }).send().unwrap();
    let status = resp.status();
    let text = resp.text().unwrap();
    assert!(status.is_success(), "HTTP world call failed ({status}): {text}");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("bad world response `{text}`: {e}"))
}

fn open_worlds_ui(ctrl: &mut ControlClient) {
    ctrl.ok("ui.set", json!({"komaphone": true}));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let ins = ctrl.inspect_fast();
        if ins["world"]["open"] == true {
            return;
        }
        if let Some(rect) = ins["roomControls"].get("home.worlds").or_else(|| ins["roomControls"].get("workspace.worlds")).and_then(Value::as_array)
            && rect.len() == 4
        {
            let x = (rect[0].as_f64().unwrap() + rect[2].as_f64().unwrap()) * 0.5;
            let y = (rect[1].as_f64().unwrap() + rect[3].as_f64().unwrap()) * 0.5;
            let _ = ctrl.call("ui.click", json!({"x": x, "y": y}));
        }
        assert!(Instant::now() < deadline, "timed out opening Shared Canvases UI");
        std::thread::sleep(Duration::from_millis(80));
    }
}

fn wait_canvas_crisp(ctrl: &mut ControlClient, expected_canvas: CanvasId, min_rev: u64, timeout: Duration) -> (f64, Value) {
    let t0 = Instant::now();
    loop {
        let ins = ctrl.inspect_fast();
        let active_canvas = ins["worldScene"]["canvas"].as_str();
        let rev = ins["worldScene"]["revision"].as_u64().unwrap_or(0);
        let preparing = ins["worldRender"]["preparing"].as_bool().unwrap_or(true);
        let rendered_rev = ins["worldRender"]["rendered_revision"].as_u64().unwrap_or(0);
        let refining = ins["worldRender"]["stats"]["is_refining"].as_bool().unwrap_or(true);
        let anim_pending = ins["worldRender"]["stats"]["anim_pending_tiles"].as_u64().unwrap_or(1);
        let visible_tiles = ins["worldRender"]["stats"]["visible_tiles"].as_u64().unwrap_or(0);
        if active_canvas == Some(expected_canvas.as_str())
            && rev >= min_rev
            && !preparing
            && rendered_rev >= min_rev
            && !refining
            && anim_pending == 0
            && visible_tiles > 0
        {
            return (t0.elapsed().as_secs_f64() * 1000.0, ins);
        }
        assert!(
            t0.elapsed() < timeout,
            "timed out waiting for canvas {} rev>={min_rev} crisp settle; last inspect worldScene={} worldRender={}",
            expected_canvas.as_str(),
            ins["worldScene"],
            ins["worldRender"]
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
#[ignore = "CP7 real native photocraft.exe + photocraft-sync.exe E2E verification"]
#[allow(clippy::cognitive_complexity)]
fn run_cp7_native_app_and_service_end_to_end_verification() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out_dir = repo_root.join("log/k7/qol-009-redesign-dev/cp7-async-and-e2e");
    let renders_dir = out_dir.join("renders");
    std::fs::create_dir_all(&renders_dir).unwrap();
    let app_bin = out_dir.join("bin/photocraft.exe");
    let sync_bin = out_dir.join("bin/photocraft-sync.exe");
    assert!(app_bin.is_file(), "missing {}", app_bin.display());
    assert!(sync_bin.is_file(), "missing {}", sync_bin.display());

    let tmp = tempfile::tempdir().unwrap();
    let service_data = tmp.path().join("service-data");
    let app_data = tmp.path().join("app-data");
    let app_cfg = tmp.path().join("app-cfg");
    let rw_dir = tmp.path().join("rw");
    std::fs::create_dir_all(&service_data).unwrap();
    std::fs::create_dir_all(&app_data).unwrap();
    std::fs::create_dir_all(&app_cfg).unwrap();
    std::fs::create_dir_all(&rw_dir).unwrap();
    let token_path = tmp.path().join("control.token");

    let sync_port = free_port();
    let ctrl_port = free_port();
    let endpoint = format!("http://127.0.0.1:{sync_port}");
    let world_url = format!("{endpoint}/v1/canvas");
    let rpc_url = format!("{endpoint}/v1/rpc");

    let now_ms = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap();
    let helper_owner = OwnerId::new(&hex(77)).unwrap();
    let helper_device = DeviceId::new(&hex(1077)).unwrap();
    let helper_token = token(77);
    let c2 = CanvasId::new(&hex(9201)).unwrap();
    let mut helper_c2_seq = 0_u64;

    // 1. Pre-seed helper identity + Canvas 2 (10,239 revisions of 60-pt strokes + rev-9,728 checkpoint)
    //    into fresh service.sqlite3 BEFORE launching photocraft.exe so --control stays active throughout!
    {
        let db_path = service_data.join("service.sqlite3");
        let mut store = Store::open(&db_path, StoreOptions::default(), now_ms).unwrap();
        store
            .execute(
                None,
                &RpcRequest {
                    protocol_version: photocraft_party::protocol::PROTOCOL_VERSION,
                    request_id: RequestId::new(&hex(7001)).unwrap(),
                    operation: Operation::Pair {
                        participant: Participant {
                            id: ParticipantId::new(helper_owner.as_str()).unwrap(),
                            device: PartyDevice::new(helper_device.as_str()).unwrap(),
                            display_name: "CP7 Helper".into(),
                        },
                        credential: helper_token.clone(),
                    },
                },
                now_ms,
            )
            .unwrap();
        store
            .world_execute(&helper_token, &Request { version: VERSION, action: Action::Create { canvas: c2, name: "Native Rev 10239".into() } }, now_ms)
            .unwrap();
        for rev in 1..=10_239_u64 {
            helper_c2_seq += 1;
            let stroke = brush_stroke_60pt(c2, helper_owner, helper_c2_seq);
            store
                .world_execute(
                    &helper_token,
                    &Request {
                        version: VERSION,
                        action: Action::Append {
                            operation: WorldOp {
                                id: OpId::new(&hex(25_000_000 + u128::from(rev))).unwrap(),
                                canvas: c2,
                                author: helper_owner,
                                device: helper_device,
                                sequence: helper_c2_seq,
                                authority_epoch: 1,
                                action: Edit::AddStroke { stroke },
                            },
                        },
                    },
                    now_ms + rev,
                )
                .unwrap();
            if rev.is_multiple_of(512) {
                store.flush_pending_checkpoints().unwrap();
            }
        }
    }
    eprintln!("[CP7-E2E] Pre-seeded Canvas 2 (10,239 revs + rev-9,728 checkpoint) cleanly.");

    let mut sync_proc = spawn_sync(&sync_bin, sync_port, &service_data);
    let mut app_proc = spawn_photocraft(&app_bin, ctrl_port, &token_path, &app_data, &app_cfg, &rw_dir);
    let mut ctrl = ControlClient::connect(ctrl_port, &token_path);

    // 2. Connect native photocraft.exe to live photocraft-sync.exe & capture its paired profile ID.
    ctrl.ok("ui.resize", json!({"width": 1280.0, "height": 720.0}));
    ctrl.ok("ui.set", json!({"komaphone": true}));
    ctrl.exec("komaphone.room.connect", json!({"endpoint": endpoint, "display_name": "CP7 Native User"}));
    let pair_deadline = Instant::now() + Duration::from_secs(15);
    let native_profile = loop {
        let ins = ctrl.inspect_fast();
        if ins["roomStatus"]["connected"] == true
            && let Some(pid) = ins["roomStatus"]["profile"]["id"].as_str()
        {
            break OwnerId::new(pid).unwrap();
        }
        assert!(Instant::now() < pair_deadline, "photocraft.exe failed to connect/pair: {}", ins["roomStatus"]);
        std::thread::sleep(Duration::from_millis(50));
    };
    eprintln!("[CP7-E2E] Native photocraft.exe connected as profile {}", native_profile.as_str());

    // 3. Over REAL HTTP: create Party Room, import Canvas 1 (4,500 strokes + 2 overlapping animated PNG media),
    //    create Canvas 3 (other canvas), and invite native_profile to Canvas 1 & Canvas 2!
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build().unwrap();
    let room_resp: RpcResponse = http
        .post(&rpc_url)
        .bearer_auth(&helper_token)
        .json(&RpcRequest {
            protocol_version: photocraft_party::protocol::PROTOCOL_VERSION,
            request_id: RequestId::new(&hex(7002)).unwrap(),
            operation: Operation::Create { mode: Mode::ExquisiteCorpse, timer_ms: None },
        })
        .send()
        .unwrap()
        .json()
        .unwrap();
    let room_id: RoomId = match room_resp {
        RpcResponse::Room { view, .. } => view.id,
        other => panic!("expected room view, got {other:?}"),
    };

    // Import Canvas 1 (4,500 strokes + 2 overlapping animated PNG SharedMedia) over REAL HTTP!
    let c1 = CanvasId::new(&hex(9101)).unwrap();
    let (manifest_bytes, asset_blobs) = build_anim4500_archive(helper_owner, helper_device);
    let manifest_sha = AssetHash::new(&format!("{:x}", Sha256::digest(&manifest_bytes))).unwrap();
    let total_bytes = manifest_bytes.len() as u64 + asset_blobs.iter().map(|(_, b)| b.len() as u64).sum::<u64>();
    let import_id = OpId::new(&hex(9102)).unwrap();
    http_world(
        &http,
        &world_url,
        &helper_token,
        Action::ImportBegin { id: import_id, new_canvas: c1, manifest_sha256: manifest_sha, manifest_bytes: manifest_bytes.len() as u64, total_bytes },
    );
    let mut off = 0usize;
    while off < manifest_bytes.len() {
        let end = (off + IMPORT_CHUNK_BYTES).min(manifest_bytes.len());
        http_world(
            &http,
            &world_url,
            &helper_token,
            Action::ImportChunk { id: import_id, object: ImportObject::Manifest {}, offset: off as u64, bytes: manifest_bytes[off..end].to_vec() },
        );
        off = end;
    }
    for (hash, png) in &asset_blobs {
        let mut aoff = 0usize;
        while aoff < png.len() {
            let end = (aoff + IMPORT_CHUNK_BYTES).min(png.len());
            http_world(
                &http,
                &world_url,
                &helper_token,
                Action::ImportChunk { id: import_id, object: ImportObject::Asset { hash: *hash }, offset: aoff as u64, bytes: png[aoff..end].to_vec() },
            );
            aoff = end;
        }
    }
    http_world(&http, &world_url, &helper_token, Action::ImportCommit { id: import_id });
    let invite_exp_ms = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap() + 3_600_000;
    let inv1_id = OpId::new(&hex(9103)).unwrap();
    let inv1_sec = token(9103);
    http_world(
        &http,
        &world_url,
        &helper_token,
        Action::Invite { canvas: c1, id: inv1_id, recipient: native_profile, secret: inv1_sec.clone(), expires_at_ms: invite_exp_ms },
    );

    let inv2_id = OpId::new(&hex(9202)).unwrap();
    let inv2_sec = token(9202);
    http_world(
        &http,
        &world_url,
        &helper_token,
        Action::Invite { canvas: c2, id: inv2_id, recipient: native_profile, secret: inv2_sec.clone(), expires_at_ms: invite_exp_ms },
    );

    // Create Canvas 3 ("Other Canvas") for concurrent HTTP appends during rev-10,240 checkpoint!
    let c3 = CanvasId::new(&hex(9301)).unwrap();
    http_world(&http, &world_url, &helper_token, Action::Create { canvas: c3, name: "Other Canvas".into() });
    eprintln!("[CP7-E2E] Ready: Canvas 1 (4500 strokes + 2 anim media), Canvas 2 (10,239 revs + ckpt 9728), Canvas 3, and Party Room!");

    // Accept both invitations inside native photocraft.exe!
    open_worlds_ui(&mut ctrl);
    ctrl.exec("komaphone.world.accept", json!({"canvas": c1, "id": inv1_id, "secret": inv1_sec}));
    ctrl.exec("komaphone.world.accept", json!({"canvas": c2, "id": inv2_id, "secret": inv2_sec}));
    let list_dl = Instant::now() + Duration::from_secs(15);
    loop {
        let st = ctrl.exec("komaphone.world.status", json!({}));
        let items = st["items"].as_array().map(Vec::len).unwrap_or(0);
        if items >= 2 {
            break;
        }
        let _ = ctrl.exec("komaphone.world.list", json!({}));
        assert!(Instant::now() < list_dl, "timed out waiting for accepted canvases: {st}");
        std::thread::sleep(Duration::from_millis(100));
    }

    // =========================================================================
    // PILLAR 1: Native Window Frame Pacing on 4,500 Strokes + 2 Overlapping Animated Media
    // =========================================================================
    ctrl.exec("komaphone.world.open", json!({"canvas": c1}));
    let (c1_open_crisp_ms, ins_c1) = wait_canvas_crisp(&mut ctrl, c1, 4_502, Duration::from_secs(30));
    let draw_rect = ins_c1["roomControls"]["world.drawing"].as_array().expect("world.drawing rect").clone();
    let cx = (draw_rect[0].as_f64().unwrap() + draw_rect[2].as_f64().unwrap()) * 0.5;
    let cy = (draw_rect[1].as_f64().unwrap() + draw_rect[3].as_f64().unwrap()) * 0.5;
    let shot1_tmp = rw_dir.join("08_native_app_anim4500.png");
    let _ = ctrl.ok("ui.screenshot", json!({"path": shot1_tmp.to_string_lossy(), "focus": false, "settle": true}));
    let _ = std::fs::copy(&shot1_tmp, renders_dir.join("08_native_app_anim4500.png"));

    let slow_before_playback = ctrl.inspect_fast()["perf"]["slowFrames"].as_array().map(Vec::len).unwrap_or(0);
    // 1A: 2.5s continuous animated-media playback in real native OS window!
    let mut playback_render_ms = Vec::new();
    let pb_end = Instant::now() + Duration::from_millis(2500);
    while Instant::now() < pb_end {
        let ins = ctrl.inspect_fast();
        if let Some(ms) = ins["worldRender"]["render_ms"].as_f64() {
            playback_render_ms.push(ms);
        }
        std::thread::sleep(Duration::from_millis(45));
    }
    let slow_after_playback = ctrl.inspect_fast()["perf"]["slowFrames"].as_array().map(Vec::len).unwrap_or(0);

    // 1B: 40 native camera pan/zoom steps during active animated-media playback!
    let mut pan_render_ms = Vec::new();
    let base_cam = coordinates::home(c1);
    for step in 0..40_usize {
        let phase = (step as f64) * 0.25;
        let cam = coordinates::pan(&base_cam, [phase.sin() * 60.0, phase.cos() * 40.0]).unwrap();
        let _ = ctrl.exec("komaphone.world.camera", json!({"camera": cam, "persist": false}));
        std::thread::sleep(Duration::from_millis(20));
        let ins = ctrl.inspect_fast();
        if let Some(ms) = ins["worldRender"]["render_ms"].as_f64() {
            pan_render_ms.push(ms);
        }
    }
    let slow_after_pan = ctrl.inspect_fast()["perf"]["slowFrames"].as_array().map(Vec::len).unwrap_or(0);

    // 1C: Live native pointer brush stroke dragged directly across the overlapping animated media!
    let rev_before_drag = ctrl.inspect_fast()["worldScene"]["revision"].as_u64().unwrap();
    let mut live_drag_render_ms = Vec::new();
    ctrl.ok("ui.worldInput", json!({"kind": "down", "x": cx - 80.0, "y": cy - 30.0}));
    for step in 1..=25_usize {
        let x = (cx - 80.0) + (step as f64) * 6.5;
        let y = (cy - 30.0) + ((step as f64) * 0.35).sin() * 25.0;
        ctrl.ok("ui.worldInput", json!({"kind": "move", "x": x, "y": y}));
        let ins = ctrl.inspect_fast();
        if let Some(ms) = ins["worldRender"]["render_ms"].as_f64() {
            live_drag_render_ms.push(ms);
        }
    }
    let t_up = Instant::now();
    ctrl.ok("ui.worldInput", json!({"kind": "up", "x": cx + 85.0, "y": cy + 10.0}));
    let drag_ack_dl = Instant::now() + Duration::from_secs(15);
    let drag_input_to_ack_ms = loop {
        let st = ctrl.exec("komaphone.world.status", json!({}));
        let can_rev = st["active"]["canonical_revision"].as_u64().unwrap_or(0);
        let pending = st["active"]["pending"].as_u64().unwrap_or(1);
        if can_rev > rev_before_drag && pending == 0 {
            break t_up.elapsed().as_secs_f64() * 1000.0;
        }
        assert!(Instant::now() < drag_ack_dl, "timed out waiting for pointer stroke ACK: {st}");
        std::thread::sleep(Duration::from_millis(5));
    };
    let slow_after_drag = ctrl.inspect_fast()["perf"]["slowFrames"].as_array().map(Vec::len).unwrap_or(0);
    eprintln!(
        "[CP7-E2E] Pillar 1 (Native Anim 4500): open_crisp={c1_open_crisp_ms:.1}ms | playback_render={:?} | pan_render={:?} | drag_render={:?} | drag_ack={drag_input_to_ack_ms:.1}ms | slowFrames(>{}ms): pb=+{} pan=+{} drag=+{}",
        stats_json(playback_render_ms.clone()),
        stats_json(pan_render_ms.clone()),
        stats_json(live_drag_render_ms.clone()),
        50,
        slow_after_playback.saturating_sub(slow_before_playback),
        slow_after_pan.saturating_sub(slow_after_playback),
        slow_after_drag.saturating_sub(slow_after_pan),
    );

    // =========================================================================
    // PILLAR 2: Real Native App + Live Service Rev-10,240 Checkpoint Boundary + Concurrent Canvas 3 & Room RPCs
    // =========================================================================
    let t_c2_open = Instant::now();
    ctrl.exec("komaphone.world.open", json!({"canvas": c2}));
    let (c2_bootstrap_crisp_ms, _) = wait_canvas_crisp(&mut ctrl, c2, 10_239, Duration::from_secs(45));
    let c2_open_total_ms = t_c2_open.elapsed().as_secs_f64() * 1000.0;
    eprintln!("[CP7-E2E] Pillar 2: Native Canvas 2 (10,239 revs, pinned ckpt 9728 + 511 tail) bootstrap+crisp = {c2_open_total_ms:.1}ms");

    // Start 2 background threads hammering Canvas 3 (other canvas) & Party Room Poll on live photocraft-sync.exe!
    let start_flag = Arc::new(AtomicBool::new(false));
    let sf_c3 = Arc::clone(&start_flag);
    let url_c3 = world_url.clone();
    let tok_c3 = helper_token.clone();
    let t_other_canvas = std::thread::spawn(move || {
        let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(15)).build().unwrap();
        while !sf_c3.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }
        let mut lats = Vec::with_capacity(30);
        for i in 1..=30_u64 {
            let stroke = brush_stroke_60pt(c3, helper_owner, 50_000 + i);
            let t0 = Instant::now();
            http_world(
                &client,
                &url_c3,
                &tok_c3,
                Action::Append {
                    operation: WorldOp {
                        id: OpId::new(&hex(40_000_000 + u128::from(i))).unwrap(),
                        canvas: c3,
                        author: helper_owner,
                        device: helper_device,
                        sequence: i,
                        authority_epoch: 1,
                        action: Edit::AddStroke { stroke },
                    },
                },
            );
            lats.push(t0.elapsed().as_secs_f64() * 1000.0);
            std::thread::sleep(Duration::from_millis(10));
        }
        lats
    });

    let sf_room = Arc::clone(&start_flag);
    let url_room = rpc_url.clone();
    let tok_room = helper_token.clone();
    let t_room_rpc = std::thread::spawn(move || {
        let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(15)).build().unwrap();
        while !sf_room.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }
        let mut lats = Vec::with_capacity(30);
        for i in 1..=30_u64 {
            let t0 = Instant::now();
            let resp = client
                .post(&url_room)
                .bearer_auth(&tok_room)
                .json(&RpcRequest {
                    protocol_version: photocraft_party::protocol::PROTOCOL_VERSION,
                    request_id: RequestId::new(&hex(45_000_000 + u128::from(i))).unwrap(),
                    operation: Operation::Poll { room: room_id, after_revision: 0 },
                })
                .send()
                .unwrap();
            assert!(resp.status().is_success());
            lats.push(t0.elapsed().as_secs_f64() * 1000.0);
            std::thread::sleep(Duration::from_millis(10));
        }
        lats
    });

    // Release concurrent traffic AND fire native stroke crossing rev 10,240 + 15 sequential follow-up strokes!
    start_flag.store(true, Ordering::Release);
    let mut trigger_10240_e2e_ack_ms = 0.0_f64;
    let mut followup_e2e_ack_ms = Vec::with_capacity(15);
    for step in 0..16_u64 {
        let target_rev = 10_240 + step;
        let pts: Vec<[f64; 2]> = (0..20).map(|k| [k as f64 * 4.0, (step as f64) * 5.0]).collect();
        let t0 = Instant::now();
        ctrl.exec(
            "komaphone.world.stroke",
            json!({
                "canvas": c2,
                "anchor": RegionKey::default(),
                "points": pts,
                "width": 12.0,
                "color": {"rgba": [0.85, 0.25, 0.20, 1.0]},
                "erase": false,
            }),
        );
        let ack_dl = Instant::now() + Duration::from_secs(20);
        let ack_ms = loop {
            let st = ctrl.exec("komaphone.world.status", json!({}));
            let can_rev = st["active"]["canonical_revision"].as_u64().unwrap_or(0);
            let pending = st["active"]["pending"].as_u64().unwrap_or(1);
            if can_rev >= target_rev && pending == 0 {
                break t0.elapsed().as_secs_f64() * 1000.0;
            }
            assert!(Instant::now() < ack_dl, "timed out waiting for native rev {target_rev} ACK: {st}");
            std::thread::sleep(Duration::from_millis(5));
        };
        if step == 0 {
            trigger_10240_e2e_ack_ms = ack_ms;
        } else {
            followup_e2e_ack_ms.push(ack_ms);
        }
    }
    let other_canvas_lats = t_other_canvas.join().unwrap();
    let room_rpc_lats = t_room_rpc.join().unwrap();

    // Confirm server published rev-10,240 async checkpoint cleanly!
    let ckpt_dl = Instant::now() + Duration::from_secs(15);
    let server_ckpt_10240_verified = loop {
        if let Response::CheckpointMeta { checkpoint: Some(c_ref) } = http_world(&http, &world_url, &helper_token, Action::CheckpointMeta { canvas: c2 })
            && c_ref.revision >= 10_240
        {
            break c_ref.revision;
        }
        assert!(Instant::now() < ckpt_dl, "timed out waiting for async server rev-10240 checkpoint");
        std::thread::sleep(Duration::from_millis(50));
    };
    eprintln!(
        "[CP7-E2E] Pillar 2 done: trigger_10240_ack={trigger_10240_e2e_ack_ms:.1}ms | followups={:?} | other_canvas={:?} | room_rpc={:?} | srv_ckpt={server_ckpt_10240_verified}",
        stats_json(followup_e2e_ack_ms.clone()),
        stats_json(other_canvas_lats.clone()),
        stats_json(room_rpc_lats.clone()),
    );

    // =========================================================================
    // PILLAR 3: Advance to Rev 10,500 -> Service Restart Mid-Sync + Hard Kill-After-ACK Recovery!
    // =========================================================================
    for rev in 10_256..=10_500_u64 {
        helper_c2_seq += 1;
        let stroke = brush_stroke_60pt(c2, helper_owner, helper_c2_seq);
        http_world(
            &http,
            &world_url,
            &helper_token,
            Action::Append {
                operation: WorldOp {
                    id: OpId::new(&hex(55_000_000 + u128::from(rev))).unwrap(),
                    canvas: c2,
                    author: helper_owner,
                    device: helper_device,
                    sequence: helper_c2_seq,
                    authority_epoch: 1,
                    action: Edit::AddStroke { stroke },
                },
            },
        );
    }
    // Wait for native photocraft.exe to catch up to rev 10,500!
    let catchup_dl = Instant::now() + Duration::from_secs(30);
    loop {
        let st = ctrl.exec("komaphone.world.status", json!({}));
        if st["active"]["canonical_revision"].as_u64().unwrap_or(0) >= 10_500 {
            break;
        }
        assert!(Instant::now() < catchup_dl, "timed out catching up to 10,500: {st}");
        std::thread::sleep(Duration::from_millis(100));
    }

    // 3A: Stop photocraft-sync.exe mid-session, submit stroke in photocraft.exe (must durably queue offline as pending==1),
    //     restart photocraft-sync.exe, and confirm automatic sync to rev 10,501!
    sync_proc.kill_and_wait();
    ctrl.exec(
        "komaphone.world.stroke",
        json!({
            "canvas": c2,
            "anchor": RegionKey::default(),
            "points": [[0.0, 0.0], [50.0, 20.0], [100.0, 0.0]],
            "width": 16.0,
            "color": {"rgba": [0.15, 0.75, 0.35, 1.0]},
            "erase": false,
        }),
    );
    let q_dl = Instant::now() + Duration::from_secs(10);
    loop {
        let st = ctrl.exec("komaphone.world.status", json!({}));
        if st["active"]["pending"].as_u64().unwrap_or(0) >= 1 {
            break;
        }
        assert!(Instant::now() < q_dl, "offline stroke did not queue locally: {st}");
        std::thread::sleep(Duration::from_millis(25));
    }
    // Restart real photocraft-sync.exe on same data_dir & port -> must recover & sync automatically!
    sync_proc = spawn_sync(&sync_bin, sync_port, &service_data);
    let sync_rec_dl = Instant::now() + Duration::from_secs(25);
    loop {
        let _ = ctrl.exec("komaphone.room.retry", json!({}));
        let st = ctrl.exec("komaphone.world.status", json!({}));
        if st["active"]["canonical_revision"].as_u64().unwrap_or(0) == 10_501 && st["active"]["pending"].as_u64().unwrap_or(1) == 0 {
            break;
        }
        assert!(Instant::now() < sync_rec_dl, "failed automatic sync after service restart: {st}");
        std::thread::sleep(Duration::from_millis(150));
    }

    // 3B: Hard kill photocraft.exe immediately after ACK (rev 10,501), relaunch against same data/config dir & live service,
    //     reopen Canvas 2, and confirm 100% recovery!
    drop(ctrl);
    app_proc.kill_and_wait();
    let ctrl_port_2 = free_port();
    let token_path_2 = tmp.path().join("control2.token");
    app_proc = spawn_photocraft(&app_bin, ctrl_port_2, &token_path_2, &app_data, &app_cfg, &rw_dir);
    let mut ctrl2 = ControlClient::connect(ctrl_port_2, &token_path_2);
    ctrl2.ok("ui.resize", json!({"width": 1280.0, "height": 720.0}));
    open_worlds_ui(&mut ctrl2);
    let conn2_dl = Instant::now() + Duration::from_secs(15);
    loop {
        let st = ctrl2.exec("komaphone.world.status", json!({}));
        if st["connected"] == true {
            break;
        }
        assert!(Instant::now() < conn2_dl, "relaunched photocraft.exe failed to reconnect: {st}");
        std::thread::sleep(Duration::from_millis(50));
    }
    let t_reopen2 = Instant::now();
    ctrl2.exec("komaphone.world.open", json!({"canvas": c2}));
    let (reopen_10501_crisp_ms, ins_rec) = wait_canvas_crisp(&mut ctrl2, c2, 10_501, Duration::from_secs(30));
    let reopen_10501_total_ms = t_reopen2.elapsed().as_secs_f64() * 1000.0;
    let st_final = ctrl2.exec("komaphone.world.status", json!({}));
    assert_eq!(st_final["active"]["canonical_revision"].as_u64(), Some(10_501));
    assert_eq!(st_final["active"]["pending"].as_u64(), Some(0));
    let shot2_tmp = rw_dir.join("09_native_app_rev10501_recovered.png");
    let _ = ctrl2.ok("ui.screenshot", json!({"path": shot2_tmp.to_string_lossy(), "focus": false, "settle": true}));
    let _ = std::fs::copy(&shot2_tmp, renders_dir.join("09_native_app_rev10501_recovered.png"));

    let service_sqlite_bytes = std::fs::metadata(service_data.join("service.sqlite3")).unwrap().len();
    let report = json!({
        "profile": "release",
        "measurement_layer": "real_native_photocraft_exe_and_photocraft_sync_exe (NOT headless component harness)",
        "binaries": {
            "photocraft_exe": app_bin.display().to_string(),
            "photocraft_sync_exe": sync_bin.display().to_string(),
        },
        "pillar_1_native_anim_4500_frame_pacing": {
            "canvas": c1.as_str(),
            "open_to_crisp_ms": c1_open_crisp_ms,
            "playback_2_5s_world_render_ms": stats_json(playback_render_ms),
            "pan_zoom_40_steps_world_render_ms": stats_json(pan_render_ms),
            "live_pointer_drag_world_render_ms": stats_json(live_drag_render_ms),
            "live_pointer_drag_input_to_ack_ms": drag_input_to_ack_ms,
            "slow_frames_over_50ms_deltas": {
                "playback": slow_after_playback.saturating_sub(slow_before_playback),
                "pan_zoom": slow_after_pan.saturating_sub(slow_after_playback),
                "live_drag": slow_after_drag.saturating_sub(slow_after_pan),
            },
            "screenshot": "renders/08_native_app_anim4500.png",
        },
        "pillar_2_native_rev10240_checkpoint_and_concurrent_requests": {
            "canvas": c2.as_str(),
            "bootstrap_10239_pinned_ckpt_9728_plus_511_tail_to_crisp_ms": c2_open_total_ms,
            "bootstrap_wait_crisp_ms": c2_bootstrap_crisp_ms,
            "trigger_rev_10240_native_input_to_ack_ms": trigger_10240_e2e_ack_ms,
            "followup_15_strokes_native_input_to_ack_ms": stats_json(followup_e2e_ack_ms),
            "concurrent_other_canvas_30_http_appends_ms": stats_json(other_canvas_lats),
            "concurrent_party_room_30_http_polls_ms": stats_json(room_rpc_lats),
            "async_server_checkpoint_verified_rev": server_ckpt_10240_verified,
        },
        "pillar_3_rev10500_service_restart_and_hard_kill_recovery": {
            "offline_stroke_queued_during_service_stop": true,
            "automatic_sync_after_service_restart_rev": 10_501,
            "hard_kill_after_ack_recovered_rev": ins_rec["worldScene"]["revision"],
            "hard_kill_after_ack_pending_count": st_final["active"]["pending"],
            "reopen_10501_after_hard_kill_to_crisp_ms": reopen_10501_total_ms,
            "reopen_10501_wait_crisp_ms": reopen_10501_crisp_ms,
            "service_sqlite_mib": service_sqlite_bytes as f64 / (1024.0 * 1024.0),
            "screenshot": "renders/09_native_app_rev10501_recovered.png",
        },
    });
    let receipt_path = out_dir.join("native-e2e-verification.json");
    let text = serde_json::to_string_pretty(&report).unwrap();
    std::fs::write(&receipt_path, &text).unwrap();
    eprintln!("=== CP7 NATIVE APP + SERVICE E2E RECEIPT WRITTEN TO {} ===\n{}", receipt_path.display(), text);

    drop(ctrl2);
    app_proc.kill_and_wait();
    sync_proc.kill_and_wait();
}
