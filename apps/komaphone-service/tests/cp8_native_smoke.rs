//! CP8 focused real-process native verification against the exact frozen
//! `log/k7/qol-009-redesign-dev/owner-build-cp8/{photocraft.exe,photocraft-sync.exe}` binaries:
//! 1. Cold-tile vs warm-tile live drawing, animated-media playback & pan/zoom, and W4 author-isolated
//!    erasing on a 4,500-stroke + 2 overlapping animated PNG canvas where peer strokes live on a
//!    strictly higher `DeviceId` (`ffff...fffe`) so the real app's device MUST exercise cold W4 split bands.
//! 2. Deep-pocket (`d0 -> d6 -> d12`) navigation frame pacing vs sharpening refinement delay over
//!    30 dense depth-0 Watercolor/Airbrush/Chalk/Soft/Hard ancestors + depth-6/depth-12 strokes.
//! 3. Live checkpoint concurrency (`rev 10,240` trigger + concurrent other-canvas appends + party room polls)
//!    against those exact binaries.
//! 4. Strictly separates render-function timings (`worldRender.render_ms`) from actual native OS window
//!    frame intervals (`world.frameSamples[*].interval_ms` / `cpu_ms`) and `perf.slowFrames`.
//!
//! Run with:
//! `cargo test --release -p photocraft-sync --test cp8_native_smoke --offline -- --ignored --nocapture`
use std::collections::BTreeMap;
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
    DeviceId as PartyDevice, Mode, Participant, ParticipantId, RequestId,
    protocol::{Operation, RpcRequest, RpcResponse},
};
use photocraft_scene::{
    CanvasId, OwnerId, RegionKey, Srgba, Stroke, StrokeId,
    brush::{BrushBlendMode, BrushPattern, BrushPatternStyle, BrushTexture, BrushTip, ProceduralTipStyle, StrokeBrush, StrokeSample},
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

/// Collects render-function timings (`worldRender.render_ms`) STRICTLY SEPARATELY from actual
/// native OS window frame intervals (`world.frameSamples[*].interval_ms`) and full-frame CPU time.
struct PhaseTelemetry {
    start_frame: u64,
    start_slow: usize,
    render_fn_ms: Vec<f64>,
    frame_samples: BTreeMap<u64, (f64, Option<f64>)>,
}

impl PhaseTelemetry {
    fn begin(ins: &Value) -> Self {
        let start_frame = ins["frame"].as_u64().unwrap_or(0);
        let start_slow = ins["perf"]["slowFrames"].as_array().map(Vec::len).unwrap_or(0);
        Self { start_frame, start_slow, render_fn_ms: Vec::new(), frame_samples: BTreeMap::new() }
    }

    fn record(&mut self, ins: &Value) {
        if let Some(ms) = ins["worldRender"]["render_ms"].as_f64() {
            self.render_fn_ms.push(ms);
        }
        if let Some(arr) = ins["world"]["frameSamples"].as_array() {
            for entry in arr {
                let Some(f) = entry["frame"].as_u64() else { continue };
                if f <= self.start_frame {
                    continue;
                }
                let cpu = entry["cpu_ms"].as_f64().unwrap_or(0.0);
                let interval = entry["interval_ms"].as_f64();
                self.frame_samples.insert(f, (cpu, interval));
            }
        }
    }

    fn finish(mut self, end_ins: &Value) -> Value {
        self.record(end_ins);
        let end_slow = end_ins["perf"]["slowFrames"].as_array().map(Vec::len).unwrap_or(0);
        let cpu_ms: Vec<f64> = self.frame_samples.values().map(|(c, _)| *c).collect();
        let interval_ms: Vec<f64> = self.frame_samples.values().filter_map(|(_, iv)| *iv).collect();
        json!({
            "render_function_ms": stats_json(self.render_fn_ms),
            "actual_native_frame_cpu_ms": stats_json(cpu_ms),
            "actual_native_frame_interval_ms": stats_json(interval_ms),
            "slow_frames_over_50ms_delta": end_slow.saturating_sub(self.start_slow),
        })
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

/// Build a 4,500-stroke + 2 overlapping animated PNG world whose strokes belong to a strictly HIGHER
/// `DeviceId` (`ffff...fffe`) so the real app's random `DeviceId` ALWAYS sits below it in W4 order,
/// forcing the real multi-author cold split-band path on every new tile!
fn build_anim4500_high_peer_archive(peer_owner: OwnerId, high_peer_device: DeviceId) -> (Vec<u8>, Vec<(AssetHash, Vec<u8>)>) {
    let src_canvas = CanvasId::new(&hex(8101)).unwrap();
    let mut world = WorldState::new(src_canvas, peer_owner, "CP8 Native Anim 4500 HighPeer".into()).unwrap();
    let mut assets_meta = Vec::new();
    let mut asset_blobs = Vec::new();

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
        let seq = world.next_sequence(high_peer_device).unwrap();
        let ox = if m_idx == 0 { -60.0 } else { -20.0 };
        let oy = if m_idx == 0 { -40.0 } else { -10.0 };
        world
            .apply(&AcceptedOp {
                revision: rev,
                op: WorldOp {
                    id: OpId::new(&hex(81_000 + u128::from(rev))).unwrap(),
                    canvas: src_canvas,
                    author: peer_owner,
                    device: high_peer_device,
                    sequence: seq,
                    authority_epoch: 1,
                    action: Edit::PlaceMedia {
                        media: Box::new(MediaPlacement {
                            id: MediaId::new(&hex(82_000 + m_idx)).unwrap(),
                            canvas: src_canvas,
                            owner: peer_owner,
                            anchor: RegionKey::default(),
                            offset: [ox, oy],
                            width: 140.0,
                            height: 140.0,
                            frames,
                            looping: true,
                            provenance: Provenance {
                                source: MediaSource::Canvas {
                                    canvas: CanvasId::new(&hex(82_999)).unwrap(),
                                    media: MediaId::new(&hex(82_500 + m_idx)).unwrap(),
                                },
                                source_revision: 1,
                                artifact_hash: AssetHash::new(&format!("{:064x}", 8_900 + m_idx)).unwrap(),
                                authors: vec![Credit { profile: peer_owner, label: "CP8 Peer".into() }],
                            },
                        }),
                    },
                },
            })
            .unwrap();
    }

    let palette = [[0.85, 0.22, 0.20, 1.0], [0.18, 0.52, 0.85, 1.0], [0.16, 0.66, 0.36, 1.0], [0.85, 0.56, 0.12, 1.0]];
    for i in 0..4_500_u64 {
        let angle = (i as f64) * 0.13;
        let radius = 20.0 + ((i % 250) as f64) * 1.4;
        let x0 = angle.cos() * radius;
        let y0 = angle.sin() * radius * 0.7;
        let pts: Vec<[f64; 2]> = (0..10).map(|k| [x0 + (k as f64) * 4.5, y0 + ((k as f64) * 0.6).sin() * 6.0]).collect();
        let rev = world.revision + 1;
        let seq = world.next_sequence(high_peer_device).unwrap();
        let color = palette[(i as usize) % palette.len()];
        world
            .apply(&AcceptedOp {
                revision: rev,
                op: WorldOp {
                    id: OpId::new(&hex(85_000 + u128::from(rev))).unwrap(),
                    canvas: src_canvas,
                    author: peer_owner,
                    device: high_peer_device,
                    sequence: seq,
                    authority_epoch: 1,
                    action: Edit::AddStroke {
                        stroke: Stroke {
                            id: StrokeId::new(&hex(86_000 + u128::from(i))).unwrap(),
                            canvas: src_canvas,
                            owner: peer_owner,
                            revision: 0,
                            anchor: RegionKey::default(),
                            points: pts,
                            width: 4.5,
                            color: Srgba { rgba: color },
                            erase: false,
                            brush: None,
                            samples: None,
                        },
                    },
                },
            })
            .unwrap();
    }
    let manifest = ArchiveManifest { schema: 3, world, assets: assets_meta };
    (manifest.encode_v3().unwrap(), asset_blobs)
}

fn make_styled_brush(kind: usize, seed: u32) -> (StrokeBrush, f64) {
    let mut b = StrokeBrush {
        authored_size: 32.0,
        hardness: 1.0,
        spacing: 0.12,
        opacity: 0.75,
        flow: 0.75,
        mode: BrushBlendMode::Normal,
        seed: u64::from(seed),
        ..Default::default()
    };
    match kind % 5 {
        0 => {
            b.authored_size = 38.0;
            b.hardness = 0.3;
            b.wet_edges = true;
            b.opacity = 0.65;
            b.flow = 0.60;
            b.texture = Some(BrushTexture {
                pattern: BrushPattern::Procedural { style: BrushPatternStyle::Paper, size: 256, seed },
                scale: 1.0,
                brightness: 0.0,
                contrast: 0.0,
                invert: false,
                each_tip: false,
                mode: photocraft_scene::brush::BrushMaskMode::Multiply,
                depth: 0.55,
                depth_jitter: Default::default(),
            });
        }
        1 => {
            b.authored_size = 42.0;
            b.hardness = 0.0;
            b.flow = 0.10;
            b.opacity = 0.60;
            b.build_up = true;
            b.build_up_rate = 25.0;
        }
        2 => {
            b.authored_size = 28.0;
            b.opacity = 0.85;
            b.flow = 0.80;
            b.tip = BrushTip::Procedural { style: ProceduralTipStyle::Chalk, size: 64, seed, param: 0 };
        }
        3 => {
            b.authored_size = 34.0;
            b.hardness = 0.0;
            b.opacity = 0.55;
            b.flow = 0.30;
        }
        _ => {
            b.authored_size = 24.0;
            b.hardness = 1.0;
            b.opacity = 0.90;
            b.flow = 1.0;
        }
    }
    let w = f64::from(b.authored_size);
    (b, w)
}

/// Build a deep-pocket canvas with 30 dense depth-0 Watercolor/Airbrush/Chalk/Soft/Hard ancestors
/// directly covering (-120..-40, -80..-30) plus 15 depth-6 strokes and 15 depth-12 strokes!
fn build_deep_pocket_archive(owner: OwnerId, device: DeviceId) -> Vec<u8> {
    let src_canvas = CanvasId::new(&hex(8102)).unwrap();
    let mut world = WorldState::new(src_canvas, owner, "CP8 Deep Pocket d0-d6-d12".into()).unwrap();
    let palette = [[0.14, 0.36, 0.84, 1.0], [0.85, 0.24, 0.24, 1.0], [0.16, 0.68, 0.40, 1.0], [0.86, 0.52, 0.14, 1.0], [0.55, 0.24, 0.78, 1.0]];
    for i in 0..60_usize {
        let (depth, ox, oy) = if i < 30 {
            (0u8, -180.0 + ((i % 5) as f64) * 35.0, -120.0 + ((i / 5) as f64) * 38.0)
        } else if i < 45 {
            (6u8, -160.0 + (((i - 30) % 5) as f64) * 28.0, -100.0 + (((i - 30) / 5) as f64) * 24.0)
        } else {
            (12u8, -90.0 + (((i - 45) % 5) as f64) * 22.0, -70.0 + (((i - 45) / 5) as f64) * 20.0)
        };
        let (brush, width) = make_styled_brush(i, 100 + i as u32);
        let pts: Vec<[f64; 2]> = (0..60)
            .map(|k| {
                let t = k as f64 / 59.0;
                [ox + t * 140.0, oy + (t * std::f64::consts::TAU).sin() * 22.0]
            })
            .collect();
        let samples: Vec<StrokeSample> = (0..60).map(|k| StrokeSample { time_ms: (k as f64) * 16.0, ..Default::default() }).collect();
        let rev = world.revision + 1;
        let seq = world.next_sequence(device).unwrap();
        world
            .apply(&AcceptedOp {
                revision: rev,
                op: WorldOp {
                    id: OpId::new(&hex(88_000 + u128::from(rev))).unwrap(),
                    canvas: src_canvas,
                    author: owner,
                    device,
                    sequence: seq,
                    authority_epoch: 1,
                    action: Edit::AddStroke {
                        stroke: Stroke {
                            id: StrokeId::new(&hex(89_000 + i as u128)).unwrap(),
                            canvas: src_canvas,
                            owner,
                            revision: 0,
                            anchor: RegionKey { depth, ..Default::default() },
                            points: pts,
                            width,
                            color: Srgba { rgba: palette[i % palette.len()] },
                            erase: false,
                            brush: Some(Box::new(brush)),
                            samples: Some(samples),
                        },
                    },
                },
            })
            .unwrap();
    }
    ArchiveManifest { schema: 3, world, assets: Vec::new() }.encode_v3().unwrap()
}

fn brush_stroke_60pt(canvas: CanvasId, owner: OwnerId, idx: u64) -> Stroke {
    let bx = -180.0 + ((idx % 12) as f64) * 32.0;
    let by = -100.0 + (((idx / 12) % 8) as f64) * 28.0;
    let pts: Vec<[f64; 2]> = (0..60).map(|k| [bx + (k as f64) * 1.1, by + ((k as f64) * 0.2).sin() * 8.0]).collect();
    let samples: Vec<StrokeSample> = (0..60).map(|k| StrokeSample { time_ms: (k as f64) * 16.0, ..Default::default() }).collect();
    let brush = StrokeBrush {
        authored_size: 10.0,
        hardness: 0.9,
        spacing: 0.2,
        opacity: 0.85,
        flow: 0.9,
        mode: BrushBlendMode::Normal,
        seed: idx.wrapping_add(1),
        ..Default::default()
    };
    Stroke {
        id: StrokeId::new(&hex(92_000_000 + u128::from(idx))).unwrap(),
        canvas,
        owner,
        revision: 0,
        anchor: RegionKey::default(),
        points: pts,
        width: 10.0,
        color: Srgba { rgba: [0.16, 0.42, 0.84, 1.0] },
        erase: false,
        brush: Some(Box::new(brush)),
        samples: Some(samples),
    }
}

fn http_world(client: &reqwest::blocking::Client, url: &str, bearer: &str, action: Action) -> Response {
    let req = Request { version: VERSION, action };
    let resp = client.post(url).bearer_auth(bearer).json(&req).send().unwrap();
    let status = resp.status();
    let body = resp.text().unwrap();
    assert!(status.is_success(), "HTTP {status} for world RPC: {body}");
    serde_json::from_str(&body).unwrap_or_else(|e| panic!("bad world response `{body}`: {e}"))
}

fn import_archive_http(
    client: &reqwest::blocking::Client,
    world_url: &str,
    bearer: &str,
    import_id: OpId,
    new_canvas: CanvasId,
    manifest_bytes: &[u8],
    assets: &[(AssetHash, Vec<u8>)],
) {
    let manifest_sha = AssetHash::new(&format!("{:x}", Sha256::digest(manifest_bytes))).unwrap();
    let total_bytes = manifest_bytes.len() as u64 + assets.iter().map(|(_, b)| b.len() as u64).sum::<u64>();
    http_world(
        client,
        world_url,
        bearer,
        Action::ImportBegin { id: import_id, new_canvas, manifest_sha256: manifest_sha, manifest_bytes: manifest_bytes.len() as u64, total_bytes },
    );
    let mut off = 0usize;
    while off < manifest_bytes.len() {
        let end = (off + IMPORT_CHUNK_BYTES).min(manifest_bytes.len());
        http_world(
            client,
            world_url,
            bearer,
            Action::ImportChunk { id: import_id, object: ImportObject::Manifest {}, offset: off as u64, bytes: manifest_bytes[off..end].to_vec() },
        );
        off = end;
    }
    for (hash, blob) in assets {
        let mut aoff = 0usize;
        while aoff < blob.len() {
            let end = (aoff + IMPORT_CHUNK_BYTES).min(blob.len());
            http_world(
                client,
                world_url,
                bearer,
                Action::ImportChunk { id: import_id, object: ImportObject::Asset { hash: *hash }, offset: aoff as u64, bytes: blob[aoff..end].to_vec() },
            );
            aoff = end;
        }
    }
    http_world(client, world_url, bearer, Action::ImportCommit { id: import_id });
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

fn wait_canvas_crisp(ctrl: &mut ControlClient, canvas: CanvasId, min_rev: u64, timeout: Duration) -> (f64, Value) {
    let t0 = Instant::now();
    loop {
        let ins = ctrl.inspect_fast();
        let active_c = ins["worldScene"]["canvas"].as_str().unwrap_or("");
        let rev = ins["worldScene"]["revision"].as_u64().unwrap_or(0);
        let rendered_rev = ins["worldRender"]["rendered_revision"].as_u64().unwrap_or(0);
        let preparing = ins["worldRender"]["preparing"].as_bool().unwrap_or(true);
        let refining = ins["worldRender"]["stats"]["is_refining"].as_bool().unwrap_or(true);
        let anim_pending = ins["worldRender"]["stats"]["anim_pending_tiles"].as_u64().unwrap_or(1);
        let visible = ins["worldRender"]["stats"]["visible_tiles"].as_u64().unwrap_or(0);
        if active_c == canvas.as_str() && rev >= min_rev && rendered_rev >= min_rev && !preparing && !refining && anim_pending == 0 && visible > 0 {
            return (t0.elapsed().as_secs_f64() * 1000.0, ins);
        }
        assert!(
            t0.elapsed() < timeout,
            "timed out waiting for canvas {} rev>={min_rev} crisp: active={active_c} rev={rev} rrev={rendered_rev} prep={preparing} ref={refining} anim_p={anim_pending} vis={visible}",
            canvas.as_str()
        );
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn wait_pointer_stroke_ack(ctrl: &mut ControlClient, rev_before: u64, t_up: Instant) -> f64 {
    let dl = Instant::now() + Duration::from_secs(15);
    loop {
        let st = ctrl.exec("komaphone.world.status", json!({}));
        let can_rev = st["active"]["canonical_revision"].as_u64().unwrap_or(0);
        let pending = st["active"]["pending"].as_u64().unwrap_or(1);
        if can_rev > rev_before && pending == 0 {
            return t_up.elapsed().as_secs_f64() * 1000.0;
        }
        assert!(Instant::now() < dl, "timed out waiting for pointer stroke ACK: {st}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "CP8 focused real-binary native app + service smoke against frozen owner-build-cp8 executables"]
#[allow(clippy::cognitive_complexity)]
fn cp8_native_drawing_animation_deep_pocket_and_checkpoint_smoke() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bin_dir = std::env::var("KOMAPHONE_CP8_BIN_DIR").map(PathBuf::from).unwrap_or_else(|_| repo_root.join("log/k7/qol-009-redesign-dev/owner-build-cp8"));
    let app_bin = bin_dir.join("photocraft.exe");
    let sync_bin = bin_dir.join("photocraft-sync.exe");
    assert!(app_bin.exists(), "missing frozen CP8 app binary {}", app_bin.display());
    assert!(sync_bin.exists(), "missing frozen CP8 sync binary {}", sync_bin.display());

    let out_dir = repo_root.join("log/k7/qol-009-redesign-dev/cp8-cold-live-and-deep");
    let renders_dir = out_dir.join("renders");
    std::fs::create_dir_all(&renders_dir).unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let service_data = tmp.path().join("service-data");
    let app_data = tmp.path().join("app-data");
    let app_cfg = tmp.path().join("app-cfg");
    let rw_dir = tmp.path().join("rw");
    for d in [&service_data, &app_data, &app_cfg, &rw_dir] {
        std::fs::create_dir_all(d).unwrap();
    }
    let token_path = tmp.path().join("control.token");

    let sync_port = free_port();
    let ctrl_port = free_port();
    let endpoint = format!("http://127.0.0.1:{sync_port}");
    let world_url = format!("{endpoint}/v1/canvas");
    let rpc_url = format!("{endpoint}/v1/rpc");

    let now_ms = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap();
    // Strictly high DeviceId (`ffff...fffe`) guarantees the real native app's random DeviceId
    // sits strictly BELOW it in W4 order, forcing multi-author cold split-band handling!
    let high_peer_owner = OwnerId::new("fffffffffffffffffffffffffffffffd").unwrap();
    let high_peer_device = DeviceId::new("fffffffffffffffffffffffffffffffe").unwrap();
    let peer_token = token(888);
    let c3_ckpt = CanvasId::new(&hex(8301)).unwrap();

    // Pre-seed high-peer identity + Canvas 3 (10,239 revs + rev-9,728 checkpoint) cleanly before launch.
    {
        let db_path = service_data.join("service.sqlite3");
        let mut store = Store::open(&db_path, StoreOptions::default(), now_ms).unwrap();
        store
            .execute(
                None,
                &RpcRequest {
                    protocol_version: photocraft_party::protocol::PROTOCOL_VERSION,
                    request_id: RequestId::new(&hex(8001)).unwrap(),
                    operation: Operation::Pair {
                        participant: Participant {
                            id: ParticipantId::new(high_peer_owner.as_str()).unwrap(),
                            device: PartyDevice::new(high_peer_device.as_str()).unwrap(),
                            display_name: "CP8 HighPeer".into(),
                        },
                        credential: peer_token.clone(),
                    },
                },
                now_ms,
            )
            .unwrap();
        store
            .world_execute(&peer_token, &Request { version: VERSION, action: Action::Create { canvas: c3_ckpt, name: "CP8 Rev 10239".into() } }, now_ms)
            .unwrap();
        for rev in 1..=10_239_u64 {
            let stroke = brush_stroke_60pt(c3_ckpt, high_peer_owner, rev);
            store
                .world_execute(
                    &peer_token,
                    &Request {
                        version: VERSION,
                        action: Action::Append {
                            operation: WorldOp {
                                id: OpId::new(&hex(65_000_000 + u128::from(rev))).unwrap(),
                                canvas: c3_ckpt,
                                author: high_peer_owner,
                                device: high_peer_device,
                                sequence: rev,
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

    let _sync_proc = spawn_sync(&sync_bin, sync_port, &service_data);
    let _app_proc = spawn_photocraft(&app_bin, ctrl_port, &token_path, &app_data, &app_cfg, &rw_dir);
    let mut ctrl = ControlClient::connect(ctrl_port, &token_path);

    ctrl.ok("ui.resize", json!({"width": 1280.0, "height": 720.0}));
    ctrl.ok("ui.set", json!({"komaphone": true}));
    ctrl.exec("komaphone.room.connect", json!({"endpoint": endpoint, "display_name": "CP8 Native User"}));
    let pair_dl = Instant::now() + Duration::from_secs(15);
    let (native_profile, native_device) = loop {
        let ins = ctrl.inspect_fast();
        if ins["roomStatus"]["connected"] == true
            && let Some(pid) = ins["roomStatus"]["profile"]["id"].as_str()
            && let Some(did) = ins["roomStatus"]["profile"]["device"].as_str()
        {
            break (OwnerId::new(pid).unwrap(), DeviceId::new(did).unwrap());
        }
        assert!(Instant::now() < pair_dl, "photocraft.exe failed to connect/pair: {}", ins["roomStatus"]);
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(native_device < high_peer_device, "expected native device {} < high peer device {}", native_device.as_str(), high_peer_device.as_str());

    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build().unwrap();
    let room_resp: RpcResponse = http
        .post(&rpc_url)
        .bearer_auth(&peer_token)
        .json(&RpcRequest {
            protocol_version: photocraft_party::protocol::PROTOCOL_VERSION,
            request_id: RequestId::new(&hex(8002)).unwrap(),
            operation: Operation::Create { mode: Mode::ExquisiteCorpse, timer_ms: None },
        })
        .send()
        .unwrap()
        .json()
        .unwrap();
    let room_id = match room_resp {
        RpcResponse::Room { view, .. } => view.id,
        other => panic!("unexpected Create room response: {other:?}"),
    };

    // Import Canvas 1 (4,500 strokes + 2 animated PNGs on high_peer_device) & Canvas 2 (deep pocket d0/d6/d12)
    let c1_anim = CanvasId::new(&hex(8201)).unwrap();
    let (c1_bytes, c1_assets) = build_anim4500_high_peer_archive(high_peer_owner, high_peer_device);
    import_archive_http(&http, &world_url, &peer_token, OpId::new(&hex(8211)).unwrap(), c1_anim, &c1_bytes, &c1_assets);

    let c2_deep = CanvasId::new(&hex(8202)).unwrap();
    let c2_bytes = build_deep_pocket_archive(high_peer_owner, high_peer_device);
    import_archive_http(&http, &world_url, &peer_token, OpId::new(&hex(8212)).unwrap(), c2_deep, &c2_bytes, &[]);

    let c4_other = CanvasId::new(&hex(8204)).unwrap();
    http_world(&http, &world_url, &peer_token, Action::Create { canvas: c4_other, name: "CP8 Concurrent Other".into() });

    open_worlds_ui(&mut ctrl);
    let invite_exp = now_ms + 3_600_000;
    for (idx, c) in [(1u128, c1_anim), (2u128, c2_deep), (3u128, c3_ckpt)] {
        let sec = token(88_000 + idx);
        let inv_id = OpId::new(&hex(88_100 + idx)).unwrap();
        http_world(
            &http,
            &world_url,
            &peer_token,
            Action::Invite { canvas: c, id: inv_id, recipient: native_profile, secret: sec.clone(), expires_at_ms: invite_exp },
        );
        ctrl.exec("komaphone.world.accept", json!({"canvas": c, "id": inv_id, "secret": sec}));
        let dl = Instant::now() + Duration::from_secs(10);
        while Instant::now() < dl {
            let st = ctrl.exec("komaphone.world.status", json!({}));
            if st["items"].as_array().is_some_and(|arr| arr.iter().any(|it| it["canvas"] == c.as_str())) {
                break;
            }
            let _ = ctrl.exec("komaphone.world.list", json!({}));
            std::thread::sleep(Duration::from_millis(60));
        }
    }

    // =========================================================================
    // PILLAR 1: 4,500 Strokes + 2 Animated PNGs (High-DeviceId Peer) —
    //           Playback, Pan/Zoom, COLD Live Drag, WARM Live Drag, W4 Live Erase
    // =========================================================================
    let t_open = Instant::now();
    ctrl.exec("komaphone.world.open", json!({"canvas": c1_anim}));
    let (c1_open_crisp_ms, ins_open) = wait_canvas_crisp(&mut ctrl, c1_anim, 4_502, Duration::from_secs(30));
    let c1_open_total_ms = t_open.elapsed().as_secs_f64() * 1000.0;
    let vp_rect = ins_open["roomControls"]["world.drawing"].as_array().expect("world.drawing rect").clone();
    let cx = (vp_rect[0].as_f64().unwrap() + vp_rect[2].as_f64().unwrap()) * 0.5;
    let cy = (vp_rect[1].as_f64().unwrap() + vp_rect[3].as_f64().unwrap()) * 0.5;

    // 1A: 2.5s animated-media playback
    let mut pb_tel = PhaseTelemetry::begin(&ctrl.inspect_fast());
    let t_pb = Instant::now();
    while t_pb.elapsed() < Duration::from_millis(2500) {
        let ins = ctrl.inspect_fast();
        pb_tel.record(&ins);
        std::thread::sleep(Duration::from_millis(40));
    }
    let pb_stats = pb_tel.finish(&ctrl.inspect_fast());

    // 1B: 40 native pan/zoom steps during active animation
    let base_cam = coordinates::home(c1_anim);
    let mut pan_tel = PhaseTelemetry::begin(&ctrl.inspect_fast());
    for step in 0..40_usize {
        let phase = (step as f64) * 0.25;
        let cam = coordinates::pan(&base_cam, [phase.sin() * 60.0, phase.cos() * 40.0]).unwrap();
        let _ = ctrl.exec("komaphone.world.camera", json!({"camera": cam, "persist": false}));
        std::thread::sleep(Duration::from_millis(20));
        pan_tel.record(&ctrl.inspect_fast());
    }
    let pan_stats = pan_tel.finish(&ctrl.inspect_fast());
    let _ = ctrl.exec("komaphone.world.camera", json!({"camera": base_cam, "persist": false}));
    let _ = wait_canvas_crisp(&mut ctrl, c1_anim, 4_502, Duration::from_secs(10));

    // 1C: COLD-tile live pointer drag across fresh un-warmed tiles over 4,500 high-peer strokes + 2 anim PNGs!
    let rev_before_cold = ctrl.inspect_fast()["worldScene"]["revision"].as_u64().unwrap();
    let mut cold_tel = PhaseTelemetry::begin(&ctrl.inspect_fast());
    ctrl.ok("ui.worldInput", json!({"kind": "down", "x": cx - 140.0, "y": cy - 35.0}));
    cold_tel.record(&ctrl.inspect_fast());
    for step in 1..=25_usize {
        let x = (cx - 140.0) + (step as f64) * 11.2;
        let y = (cy - 35.0) + ((step as f64) * 0.35).sin() * 45.0;
        ctrl.ok("ui.worldInput", json!({"kind": "move", "x": x, "y": y}));
        cold_tel.record(&ctrl.inspect_fast());
    }
    // Measure remaining cold split-band convergence delay while stroke is still active before pointer up!
    let t_cold_conv = Instant::now();
    while t_cold_conv.elapsed() < Duration::from_secs(5) {
        let ins = ctrl.inspect_fast();
        if ins["worldRender"]["stats"]["anim_pending_tiles"].as_u64().unwrap_or(0) == 0
            && !ins["worldRender"]["stats"]["is_refining"].as_bool().unwrap_or(false)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let cold_split_refinement_ms = t_cold_conv.elapsed().as_secs_f64() * 1000.0;
    let t_cold_up = Instant::now();
    ctrl.ok("ui.worldInput", json!({"kind": "up", "x": cx + 140.0, "y": cy + 15.0}));
    let cold_drag_ack_ms = wait_pointer_stroke_ack(&mut ctrl, rev_before_cold, t_cold_up);
    let cold_drag_stats = cold_tel.finish(&ctrl.inspect_fast());

    // 1D: WARM-tile live pointer drag across the SAME tiles immediately afterward (DynamicSplitCache warm!)
    let rev_before_warm = ctrl.inspect_fast()["worldScene"]["revision"].as_u64().unwrap();
    let mut warm_tel = PhaseTelemetry::begin(&ctrl.inspect_fast());
    ctrl.ok("ui.worldInput", json!({"kind": "down", "x": cx - 140.0, "y": cy - 10.0}));
    warm_tel.record(&ctrl.inspect_fast());
    for step in 1..=25_usize {
        let x = (cx - 140.0) + (step as f64) * 11.2;
        let y = (cy - 10.0) + ((step as f64) * 0.35).cos() * 40.0;
        ctrl.ok("ui.worldInput", json!({"kind": "move", "x": x, "y": y}));
        warm_tel.record(&ctrl.inspect_fast());
    }
    let t_warm_up = Instant::now();
    ctrl.ok("ui.worldInput", json!({"kind": "up", "x": cx + 140.0, "y": cy + 30.0}));
    let warm_drag_ack_ms = wait_pointer_stroke_ack(&mut ctrl, rev_before_warm, t_warm_up);
    let warm_drag_stats = warm_tel.finish(&ctrl.inspect_fast());

    // 1E: Live W4 Eraser drag across own strokes + high-peer strokes (verify responsive drag + W4 isolation)
    ctrl.ok("ui.set", json!({"tool": "Eraser"}));
    let rev_before_erase = ctrl.inspect_fast()["worldScene"]["revision"].as_u64().unwrap();
    let mut erase_tel = PhaseTelemetry::begin(&ctrl.inspect_fast());
    ctrl.ok("ui.worldInput", json!({"kind": "down", "x": cx - 120.0, "y": cy - 20.0}));
    erase_tel.record(&ctrl.inspect_fast());
    for step in 1..=15_usize {
        let x = (cx - 120.0) + (step as f64) * 16.0;
        let y = cy - 20.0;
        ctrl.ok("ui.worldInput", json!({"kind": "move", "x": x, "y": y}));
        erase_tel.record(&ctrl.inspect_fast());
    }
    let t_erase_up = Instant::now();
    ctrl.ok("ui.worldInput", json!({"kind": "up", "x": cx + 120.0, "y": cy - 20.0}));
    let erase_ack_ms = wait_pointer_stroke_ack(&mut ctrl, rev_before_erase, t_erase_up);
    let erase_stats = erase_tel.finish(&ctrl.inspect_fast());
    ctrl.ok("ui.set", json!({"tool": "Brush"}));

    let shot1_rw = rw_dir.join("01_native_anim4500_w4_live.png");
    ctrl.ok("ui.screenshot", json!({"path": shot1_rw.to_string_lossy(), "focus": false, "settle": true}));
    std::fs::copy(&shot1_rw, renders_dir.join("01_native_anim4500_w4_live.png")).unwrap();

    // =========================================================================
    // PILLAR 2: Deep Pocket (30 depth-0 Watercolor/Airbrush/Chalk ancestors + d6/d12) —
    //           Separate Navigation Frame Pacing vs Sharpening Refinement Delay
    // =========================================================================
    ctrl.exec("komaphone.world.open", json!({"canvas": c2_deep}));
    let (c2_d0_crisp_ms, _) = wait_canvas_crisp(&mut ctrl, c2_deep, 60, Duration::from_secs(20));

    let p6 = coordinates::point(&RegionKey { depth: 6, ..Default::default() }, [-120.0, -80.0]).unwrap();
    let cam_d6 = coordinates::target(c2_deep, p6, 6, 1.37).unwrap();
    let t_d6 = Instant::now();
    let _ = ctrl.exec("komaphone.world.camera", json!({"camera": cam_d6, "persist": false}));
    let (d6_refine_ms, _) = wait_canvas_crisp(&mut ctrl, c2_deep, 60, Duration::from_secs(20));
    let d6_total_ms = t_d6.elapsed().as_secs_f64() * 1000.0;

    let p12 = coordinates::point(&RegionKey { depth: 12, ..Default::default() }, [-40.0, -30.0]).unwrap();
    let cam_d12 = coordinates::target(c2_deep, p12, 12, 2.23).unwrap();
    let t_d12 = Instant::now();
    let _ = ctrl.exec("komaphone.world.camera", json!({"camera": cam_d12, "persist": false}));
    let (d12_refine_ms, _) = wait_canvas_crisp(&mut ctrl, c2_deep, 60, Duration::from_secs(20));
    let d12_total_ms = t_d12.elapsed().as_secs_f64() * 1000.0;

    let mut d12_nav_tel = PhaseTelemetry::begin(&ctrl.inspect_fast());
    for step in 0..30_usize {
        let ph = (step as f64) * 0.28;
        let cam = coordinates::pan(&cam_d12, [ph.sin() * 45.0, ph.cos() * 30.0]).unwrap();
        let _ = ctrl.exec("komaphone.world.camera", json!({"camera": cam, "persist": false}));
        std::thread::sleep(Duration::from_millis(20));
        d12_nav_tel.record(&ctrl.inspect_fast());
    }
    let d12_nav_stats = d12_nav_tel.finish(&ctrl.inspect_fast());
    let _ = ctrl.exec("komaphone.world.camera", json!({"camera": cam_d12, "persist": false}));
    let _ = wait_canvas_crisp(&mut ctrl, c2_deep, 60, Duration::from_secs(10));
    let shot2_rw = rw_dir.join("02_native_deep_d12_settled.png");
    ctrl.ok("ui.screenshot", json!({"path": shot2_rw.to_string_lossy(), "focus": false, "settle": true}));
    std::fs::copy(&shot2_rw, renders_dir.join("02_native_deep_d12_settled.png")).unwrap();

    // =========================================================================
    // PILLAR 3: Live Checkpoint Concurrency Smoke on EXACT Frozen CP8 Binaries
    // =========================================================================
    ctrl.exec("komaphone.world.open", json!({"canvas": c3_ckpt}));
    let (c3_bootstrap_ms, _) = wait_canvas_crisp(&mut ctrl, c3_ckpt, 10_239, Duration::from_secs(45));

    let start_flag = Arc::new(AtomicBool::new(false));
    let sf_c4 = Arc::clone(&start_flag);
    let url_c4 = world_url.clone();
    let tok_c4 = peer_token.clone();
    let t_other = std::thread::spawn(move || {
        let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(15)).build().unwrap();
        while !sf_c4.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }
        let mut lats = Vec::with_capacity(30);
        for i in 1..=30_u64 {
            let stroke = brush_stroke_60pt(c4_other, high_peer_owner, 70_000 + i);
            let t0 = Instant::now();
            http_world(
                &client,
                &url_c4,
                &tok_c4,
                Action::Append {
                    operation: WorldOp {
                        id: OpId::new(&hex(71_000_000 + u128::from(i))).unwrap(),
                        canvas: c4_other,
                        author: high_peer_owner,
                        device: high_peer_device,
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
    let tok_room = peer_token.clone();
    let t_room = std::thread::spawn(move || {
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
                    request_id: RequestId::new(&hex(72_000_000 + u128::from(i))).unwrap(),
                    operation: Operation::Poll { room: room_id, after_revision: 0 },
                })
                .send()
                .unwrap();
            assert!(resp.status().is_success());
            let _: RpcResponse = resp.json().unwrap();
            lats.push(t0.elapsed().as_secs_f64() * 1000.0);
            std::thread::sleep(Duration::from_millis(10));
        }
        lats
    });

    start_flag.store(true, Ordering::Release);
    let mut c3_ack_ms = Vec::with_capacity(11);
    for step in 0..11_u64 {
        let target_rev = 10_240 + step;
        let x0 = -100.0 + (step as f64) * 14.0;
        let t0 = Instant::now();
        ctrl.exec(
            "komaphone.world.stroke",
            json!({
                "canvas": c3_ckpt,
                "anchor": RegionKey::default(),
                "points": [[x0, -15.0], [x0 + 25.0, 15.0]],
                "width": 6.0,
                "color": {"rgba": [0.92, 0.16, 0.24, 1.0]},
                "erase": false,
            }),
        );
        let dl = Instant::now() + Duration::from_secs(15);
        let ack = loop {
            let st = ctrl.exec("komaphone.world.status", json!({}));
            if st["active"]["canonical_revision"].as_u64().unwrap_or(0) >= target_rev && st["active"]["pending"].as_u64().unwrap_or(1) == 0 {
                break t0.elapsed().as_secs_f64() * 1000.0;
            }
            assert!(Instant::now() < dl, "timed out waiting for rev {target_rev}: {st}");
            std::thread::sleep(Duration::from_millis(5));
        };
        c3_ack_ms.push(ack);
    }
    let other_canvas_lats = t_other.join().unwrap();
    let room_poll_lats = t_room.join().unwrap();

    let ckpt_dl = Instant::now() + Duration::from_secs(15);
    let ckpt_rev = loop {
        match http_world(&http, &world_url, &peer_token, Action::CheckpointMeta { canvas: c3_ckpt }) {
            Response::CheckpointMeta { checkpoint: Some(meta) } if meta.revision >= 10_240 => break meta.revision,
            _ => {}
        }
        assert!(Instant::now() < ckpt_dl, "async server checkpoint at rev 10,240 did not publish");
        std::thread::sleep(Duration::from_millis(25));
    };

    let shot3_rw = rw_dir.join("03_native_rev10240_ckpt.png");
    ctrl.ok("ui.screenshot", json!({"path": shot3_rw.to_string_lossy(), "focus": false, "settle": true}));
    std::fs::copy(&shot3_rw, renders_dir.join("03_native_rev10240_ckpt.png")).unwrap();

    let report = json!({
        "profile": "release",
        "measurement_layer": "real_native_owner_build_cp8_executables (explicitly separates render_function_ms from actual_native_frame_interval_ms and actual_native_frame_cpu_ms)",
        "binaries": {
            "photocraft_exe": app_bin.to_string_lossy(),
            "photocraft_sync_exe": sync_bin.to_string_lossy(),
        },
        "w4_device_order_proof": {
            "native_app_device": native_device.as_str(),
            "peer_strokes_device": high_peer_device.as_str(),
            "native_strictly_below_peer": native_device < high_peer_device,
        },
        "pillar_1_anim4500_cold_vs_warm_live_draw": {
            "open_to_crisp_ms": c1_open_total_ms,
            "open_wait_crisp_ms": c1_open_crisp_ms,
            "playback_2_5s": pb_stats,
            "pan_zoom_40_steps": pan_stats,
            "cold_tile_live_drag_25_steps": {
                "timings": cold_drag_stats,
                "cold_split_refinement_delay_ms": cold_split_refinement_ms,
                "pointer_up_to_synced_ack_ms": cold_drag_ack_ms,
            },
            "warm_tile_live_drag_25_steps": {
                "timings": warm_drag_stats,
                "pointer_up_to_synced_ack_ms": warm_drag_ack_ms,
            },
            "w4_live_eraser_drag_15_steps": {
                "timings": erase_stats,
                "pointer_up_to_synced_ack_ms": erase_ack_ms,
            },
            "screenshot": "renders/01_native_anim4500_w4_live.png",
        },
        "pillar_2_deep_pocket_navigation_vs_refinement": {
            "d0_open_refinement_delay_ms": c2_d0_crisp_ms,
            "d6_jump_total_ms": d6_total_ms,
            "d6_refinement_delay_ms": d6_refine_ms,
            "d12_jump_total_ms": d12_total_ms,
            "d12_refinement_delay_ms": d12_refine_ms,
            "d12_navigation_pan_30_steps": d12_nav_stats,
            "screenshot": "renders/02_native_deep_d12_settled.png",
        },
        "pillar_3_checkpoint_concurrency_on_frozen_binaries": {
            "bootstrap_10239_to_crisp_ms": c3_bootstrap_ms,
            "trigger_rev_10240_native_input_to_ack_ms": c3_ack_ms[0],
            "followup_10_strokes_native_input_to_ack_ms": stats_json(c3_ack_ms[1..].to_vec()),
            "concurrent_other_canvas_30_http_appends_ms": stats_json(other_canvas_lats),
            "concurrent_party_room_30_http_polls_ms": stats_json(room_poll_lats),
            "async_server_checkpoint_verified_rev": ckpt_rev,
            "screenshot": "renders/03_native_rev10240_ckpt.png",
        },
    });
    let receipt_path = out_dir.join("cp8-native-smoke.json");
    let json_text = serde_json::to_string_pretty(&report).unwrap();
    std::fs::write(&receipt_path, &json_text).unwrap();
    eprintln!("=== CP8 NATIVE SMOKE WRITTEN TO {} ===\n{}", receipt_path.display(), json_text);
}
