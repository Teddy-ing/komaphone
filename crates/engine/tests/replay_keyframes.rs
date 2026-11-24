//! QOL-014 regression and acceptance tests: bounded keyframed party replay packaging,
//! unchanged K7 service compatibility, deadline freeze determinism, and persistent
//! submission-preparation error reporting across 250ms room polls.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use image::ImageEncoder;
use photocraft_doc::{BlendMode, Color, ColorMode, Document, SampleType, Size};
use photocraft_engine::gallery_media::{
    encode_png, remix, render_creature, render_document, render_frame, replay_image, replay_info, replay_step, validate_originals,
};
use photocraft_engine::{Session, rooms::FreezePin};
use photocraft_local_store::{ReplayKind, ReplayRecord, SaveRequest, Store, StoreOptions};
use photocraft_party::gallery::{GalleryBundle, GalleryManifest};
use photocraft_party::protocol::{Operation, RpcRequest, RpcResponse};
use photocraft_party::replay_artifact::{ReplayArtifact, ReplayArtifactStep, pack_checkpoints, unpack_checkpoints};
use photocraft_party::room::{Action, Phase, Room};
use photocraft_party::{
    AssetHash, AssetRef, CheckpointId, CheckpointRef, DeviceId, EventId, GameId, MAX_ASSET_BYTES, MAX_PROJECT_BYTES, Mode, Participant, ParticipantId,
    ProjectId, ReplayId, ReplayRef, RequestId, RoomId, Submission, SubmissionId,
};
use serde_json::json;
use sha2::{Digest, Sha256};

type AssetMap = BTreeMap<AssetHash, Vec<u8>>;

struct TimedQueued {
    submission: Submission,
    objects: AssetMap,
    op_count: usize,
    outbox_json_bytes: u64,
    prepare_ms: u128,
}

struct FrozenAttempt {
    submission: Submission,
    objects: AssetMap,
    pin: FreezePin,
}

fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn person(n: u8) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{n:032x}")).unwrap(),
        device: DeviceId::new(&format!("{:032x}", u16::from(n) + 200)).unwrap(),
        display_name: format!("Friend {n}"),
    }
}

fn canonical_png(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out).write_image(pixels, width, height, image::ExtendedColorType::Rgba8).unwrap();
    out
}

/// Exact replica of `apps/komaphone-service/src/assets.rs` (`validate_submission` + `validate_replay`)
/// so every test verifies byte-for-byte compatibility with the unchanged live K7 service (`f4d77aee…`).
fn assert_matches_live_k7_service_validation(room: &Room, submission: &Submission, uploaded_objects: &BTreeMap<AssetHash, Vec<u8>>) {
    submission.validate().expect("Submission::validate");
    let mut used = 0u64;
    for asset in &submission.assets {
        let bytes = uploaded_objects.get(&asset.hash).expect("referenced asset must be uploaded");
        assert_eq!(bytes.len() as u64, asset.bytes, "asset byte length must match");
        assert_eq!(hex_digest(bytes), asset.hash.as_str(), "asset SHA256 must match");
        used = used.saturating_add(asset.bytes);
    }
    assert!(used <= MAX_PROJECT_BYTES, "room object bytes within MAX_PROJECT_BYTES");
    assert!(uploaded_objects.len() < 9216, "room object count within 9216");

    let decode_checkpoint = |bytes: &[u8]| -> Document {
        let options = photocraft_format::LoadOptions {
            max_manifest_bytes: 8 * 1024 * 1024,
            max_blob_bytes: MAX_ASSET_BYTES as usize,
            max_total_bytes: MAX_PROJECT_BYTES,
            max_canvas_side: photocraft_party::MAX_IMAGE_SIDE,
            max_canvas_pixels: photocraft_party::MAX_IMAGE_PIXELS,
            preserve_ids: true,
        };
        photocraft_format::load_from_bytes_with(bytes, &options).expect("service decode_checkpoint")
    };

    let mode = room.view(submission.author).expect("author room view").mode;
    let (width, height) = match mode {
        Mode::Animation { .. } => (1536, 1024),
        Mode::ExquisiteCorpse => (1024, 512),
    };
    let checkpoint_bytes = uploaded_objects.get(&submission.checkpoint.asset.hash).unwrap();
    let doc = decode_checkpoint(checkpoint_bytes);
    assert_eq!((doc.size.width, doc.size.height), (width, height), "final checkpoint dimensions");

    if submission.replay.baseline.asset != submission.checkpoint.asset {
        let baseline_bytes = uploaded_objects.get(&submission.replay.baseline.asset.hash).unwrap();
        let baseline_doc = decode_checkpoint(baseline_bytes);
        assert_eq!((baseline_doc.size.width, baseline_doc.size.height), (width, height), "baseline checkpoint dimensions");
    }

    if let Some(manifest) = submission.replay.manifest {
        let manifest_bytes = uploaded_objects.get(&manifest.hash).unwrap();
        let replay = ReplayArtifact::decode(manifest_bytes, submission).expect("ReplayArtifact::decode");
        let mut checked = BTreeSet::new();
        let mut wanted = BTreeMap::new();
        for cp in replay.checkpoints() {
            if let Some(prev) = wanted.insert(cp.hash, cp.bytes) {
                assert_eq!(prev, cp.bytes, "no checkpoint length conflict");
            }
        }
        let mut packs = BTreeSet::new();
        for pack in &replay.packs {
            assert!(packs.insert(pack.hash), "no duplicate replay pack");
            let pack_bytes = uploaded_objects.get(&pack.hash).unwrap();
            for (cp, bytes) in unpack_checkpoints(pack_bytes).expect("unpack_checkpoints") {
                assert_eq!(wanted.get(&cp.hash), Some(&cp.bytes), "packed checkpoint must be wanted");
                assert!(checked.insert(cp.hash), "packed checkpoint must not be duplicated");
                let cp_doc = decode_checkpoint(bytes);
                assert_eq!((cp_doc.size.width, cp_doc.size.height), (width, height), "packed checkpoint dimensions");
            }
        }
        for cp in replay.checkpoints() {
            if !checked.insert(cp.hash) {
                continue;
            }
            assert!(submission.assets.contains(&cp), "unpacked checkpoint must be in submission.assets");
            let cp_bytes = uploaded_objects.get(&cp.hash).unwrap();
            let cp_doc = decode_checkpoint(cp_bytes);
            assert_eq!((cp_doc.size.width, cp_doc.size.height), (width, height), "direct checkpoint dimensions");
        }
    } else {
        assert_eq!(submission.replay.event_count, 0);
        assert_eq!(submission.revision, 1);
        assert_eq!(submission.replay.baseline, submission.checkpoint);
    }

    let mut rendered = photocraft_compose::flatten(&doc);
    photocraft_party::canonical_color::to_srgb(&doc, &mut rendered.px).expect("canonical_color::to_srgb");
    let rendered = rendered.to_rgba8();
    let uploaded_image = uploaded_objects.get(&submission.image.hash).unwrap();
    assert_eq!(&canonical_png(width, height, &rendered.pixels), uploaded_image, "canonical PNG must match byte-for-byte");

    if let Some(seam) = submission.seam {
        let uploaded_seam = uploaded_objects.get(&seam.hash).unwrap();
        let start = ((height - 24) as usize) * (width as usize) * 4;
        assert_eq!(&canonical_png(width, 24, &rendered.pixels[start..]), uploaded_seam, "canonical 24px seam PNG must match byte-for-byte");
    }
}

fn spawn_offline_endpoint() -> (String, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let stop_bg = Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !stop_bg.load(Ordering::Acquire) && Instant::now() < deadline {
            match listener.accept() {
                Ok((stream, _)) => drop(stream),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(5)),
                Err(_) => break,
            }
        }
    });
    (endpoint, stop, handle)
}

fn pair_isolated_outbox(path: &Path, endpoint: &str, display_name: &str) -> Participant {
    let mut outbox = photocraft_room_client::Outbox::open(path, Box::new(photocraft_room_client::WindowsCredentials)).unwrap();
    outbox.configure(endpoint, display_name).unwrap();
    let pair = outbox.next().unwrap().unwrap();
    let Operation::Pair { participant, .. } = pair.operation.clone() else {
        panic!("expected Operation::Pair");
    };
    outbox.acknowledge(&pair, &RpcResponse::Paired { participant: participant.clone() }).unwrap();
    participant
}

fn update_isolated_outbox_poll(path: &Path, view: photocraft_party::room::RoomView) {
    let mut outbox = photocraft_room_client::Outbox::open(path, Box::new(photocraft_room_client::WindowsCredentials)).unwrap();
    outbox.update_poll(view).unwrap();
}

fn setup_corpse_room_with_host(host: Participant, timer_ms: Option<u64>) -> (Room, Participant, Participant) {
    let guest = person(2);
    let mut room = Room::new(RoomId::new(&format!("{:032x}", 1001)).unwrap(), host.clone(), Mode::ExquisiteCorpse, timer_ms).unwrap();
    room.apply(guest.id, room.revision(), 0, Action::Join { participant: guest.clone() }).unwrap();
    for p in [&host, &guest] {
        room.apply(p.id, room.revision(), 0, Action::Ready { ready: true }).unwrap();
    }
    let projects = (0..6).map(|n| ProjectId::new(&format!("{:032x}", 3000 + n)).unwrap()).collect();
    room.apply(host.id, room.revision(), 0, Action::Start { game: GameId::new(&format!("{:032x}", 2001)).unwrap(), projects }).unwrap();
    (room, host, guest)
}

fn setup_corpse_room() -> (Room, Participant, Participant) {
    setup_corpse_room_with_host(person(1), None)
}

fn decode_queued_submission(outbox_path: &Path) -> (Submission, BTreeMap<AssetHash, Vec<u8>>, usize, u64) {
    let db = rusqlite::Connection::open(outbox_path).unwrap();
    let mut stmt = db.prepare("SELECT body FROM outbox WHERE retained=0 ORDER BY seq").unwrap();
    let rows: Vec<Vec<u8>> = stmt.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
    let op_count = rows.len();
    let outbox_json_bytes: u64 = rows.iter().map(|b| b.len() as u64).sum();
    let mut buffers: BTreeMap<AssetHash, Vec<u8>> = BTreeMap::new();
    let mut submitted: Option<Submission> = None;
    for body in rows {
        let req = RpcRequest::decode(&body).unwrap();
        match req.operation {
            Operation::Upload { hash, total, offset, bytes, finish, .. } => {
                let buf = buffers.entry(hash).or_default();
                assert_eq!(buf.len() as u64, offset);
                buf.extend_from_slice(&bytes);
                if finish {
                    assert_eq!(buf.len() as u64, total);
                    assert_eq!(hex_digest(buf), hash.as_str());
                }
            }
            Operation::Apply { action, .. } => {
                if let Action::Submit { submission, .. } = *action {
                    submitted = Some(submission);
                }
            }
            _ => {}
        }
    }
    (submitted.expect("queued Action::Submit"), buffers, op_count, outbox_json_bytes)
}

fn minimal_submission_for_document(
    assignment: photocraft_party::room::Assignment,
    mode: Mode,
    doc: &Document,
    objects: &mut BTreeMap<AssetHash, Vec<u8>>,
) -> Submission {
    let (width, height) = match mode {
        Mode::Animation { .. } => (1536, 1024),
        Mode::ExquisiteCorpse => (1024, 512),
    };
    let cp_bytes = photocraft_format::save_to_bytes(doc, &Default::default()).unwrap();
    let cp_hash = AssetHash::new(&hex_digest(&cp_bytes)).unwrap();
    let cp_asset = AssetRef { hash: cp_hash, bytes: cp_bytes.len() as u64 };
    let cp_ref = CheckpointRef { id: CheckpointId::new(&cp_hash.as_str()[..32]).unwrap(), asset: cp_asset };
    objects.insert(cp_hash, cp_bytes);

    let mut flat = photocraft_compose::flatten(doc);
    photocraft_party::canonical_color::to_srgb(doc, &mut flat.px).unwrap();
    let rgba = flat.to_rgba8();
    let img_bytes = canonical_png(width, height, &rgba.pixels);
    let img_hash = AssetHash::new(&hex_digest(&img_bytes)).unwrap();
    let img_asset = AssetRef { hash: img_hash, bytes: img_bytes.len() as u64 };
    objects.insert(img_hash, img_bytes);

    let mut assets = vec![cp_asset, img_asset];
    let seam = if mode == Mode::ExquisiteCorpse {
        let start = ((height - 24) as usize) * (width as usize) * 4;
        let seam_bytes = canonical_png(width, 24, &rgba.pixels[start..]);
        let seam_hash = AssetHash::new(&hex_digest(&seam_bytes)).unwrap();
        let seam_asset = AssetRef { hash: seam_hash, bytes: seam_bytes.len() as u64 };
        objects.insert(seam_hash, seam_bytes);
        assets.push(seam_asset);
        Some(seam_asset)
    } else {
        None
    };

    Submission {
        id: SubmissionId::new(&hex_digest(format!("sub:{}:{}:{}", assignment.project.as_str(), assignment.id.chain, assignment.id.round).as_bytes())[..32])
            .unwrap(),
        assignment: assignment.id,
        author: assignment.player,
        project: assignment.project,
        revision: 1,
        image: img_asset,
        checkpoint: cp_ref,
        replay: ReplayRef {
            id: ReplayId::new(&hex_digest(assignment.project.as_str().as_bytes())[..32]).unwrap(),
            manifest: None,
            baseline: cp_ref,
            end_event: None,
            event_count: 0,
            duration_ms: 0,
        },
        assets,
        note: String::new(),
        seam,
        extent: None,
    }
}

#[test]
fn high_revision_multi_layer_party_drawing_packages_compactly_validates_replays_and_remixes() {
    let dir = tempfile::tempdir().unwrap();
    let (offline_ep, offline_stop, offline_srv) = spawn_offline_endpoint();
    let rooms_db = dir.path().join("rooms.sqlite3");
    let host = pair_isolated_outbox(&rooms_db, &offline_ep, "Friend 1");
    let (mut room, host, guest) = setup_corpse_room_with_host(host, None);
    let host_view = room.view(host.id).unwrap();
    let assigned = host_view.assignment.as_ref().unwrap().assignment.clone();
    update_isolated_outbox_poll(&rooms_db, host_view.clone());

    // Build a realistic 1,020-revision multi-layer Exquisite Corpse drawing using real engine commands
    // for layers, imported smart object, brush strokes, erases, undo/redo, and dense stroke history.
    let mut session = Session::new();
    session.enable_local_projects(dir.path().join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    session.rooms.status.profile = Some(host.clone());
    session.rooms.status.room = Some(host_view.clone());
    session.rooms.status.endpoint = Some("http://127.0.0.1:43127".into());
    session.rooms.status.paired = true;
    session.open_room_draft().unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();

    // Place an embedded reference/stamp PNG smart object + multiple paint layers with blend mode & opacity.
    let stamp_path = dir.path().join("stamp.png");
    let stamp_img = photocraft_engine::rooms::media::RoomImage { width: 32, height: 32, rgba: [40, 140, 220, 255].repeat(32 * 32) };
    std::fs::write(&stamp_path, encode_png(&stamp_img).unwrap()).unwrap();
    session.execute("file.placeEmbedded", json!({"path": stamp_path.to_str().unwrap()})).unwrap();
    session.execute("layer.new.layer", json!({"name": "Shading"})).unwrap();
    session.execute("layer.setProps", json!({"opacity": 0.65})).unwrap();
    session.execute("paint.stroke", json!({"points": [[80, 60], [520, 260], [920, 440]], "size": 26, "color": [0.85, 0.25, 0.15, 1.0], "seed": 11})).unwrap();
    session.execute("layer.new.layer", json!({"name": "Ink"})).unwrap();
    session.execute("paint.stroke", json!({"points": [[120, 495], [512, 506], [900, 511]], "size": 14, "color": [0.1, 0.2, 0.8, 1.0], "seed": 12})).unwrap();
    session.execute("paint.stroke", json!({"points": [[300, 200], [340, 220]], "size": 12, "erase": true})).unwrap();
    session.execute("edit.undo", json!({})).unwrap();
    session.execute("edit.redo", json!({})).unwrap();
    session.flush_local_projects(Duration::from_secs(30)).unwrap();

    let expected_final_doc = session.active().unwrap().doc.as_ref().clone();
    let doc_id = session.active().unwrap().doc.id;
    let current_rev = session.local_projects.status(doc_id).local_revision;
    assert!(current_rev >= 9);
    drop(session);

    // Append painted strokes directly into `drafts.sqlite3` up to revision 1,020 with realistic ~4 KB
    // resolved stroke `params` payloads (mirroring real brush dabs + preset metadata) so the test
    // completes in a few seconds while exercising 1,020 real `.pcraft` revisions in SQLite!
    let mut store = Store::open(dir.path().join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    let (mut working_doc, info) = store.load_revision(assigned.project, current_rev).unwrap();
    assert_eq!(info.revision, current_rev);
    let ink_layer_idx = working_doc.layers.len() - 1;
    let target_revisions = 1_020u64;
    let bloated_points: Vec<_> = (0..64).map(|i| json!({"x": i * 12, "y": (i * 7) % 512, "pressure": 1.0, "time": i * 4})).collect();
    for rev in (current_rev + 1)..=target_revisions {
        let x = (((rev * 37) % 990) + 8) as i32;
        let y = (((rev * 29) % 496) + 4) as i32;
        let r = ((rev % 200) as f32) / 255.0;
        let g = (((rev * 3) % 200) as f32) / 255.0;
        let b = (((rev * 7) % 200) as f32) / 255.0;
        if let Some(surface) = working_doc.layers[ink_layer_idx].surface_mut() {
            surface
                .try_write_region(photocraft_geom::Rect::new(x, y, x + 12, (y + 12).min(512)), &[r, g, b, 1.0].repeat(12 * ((y + 12).min(512) - y) as usize))
                .unwrap();
        }
        // Paint deterministic colored seam pixels on the final revision so seam verification is nontrivial.
        if rev == target_revisions
            && let Some(surface) = working_doc.layers[ink_layer_idx].surface_mut()
        {
            surface.try_write_region(photocraft_geom::Rect::new(500, 500, 524, 512), &[0.1, 0.8, 0.3, 1.0].repeat(24 * 12)).unwrap();
        }
        store
            .save(&SaveRequest {
                project: assigned.project,
                expected_revision: rev - 1,
                request_id: photocraft_local_store::fresh_event_id().unwrap(),
                title: "Detailed Corpse Head".into(),
                document: working_doc.clone(),
                event: ReplayRecord {
                    schema_version: 1,
                    command: "paint.stroke".into(),
                    params: json!({
                        "input": {
                            "brush": {"size": 14.0, "hardness": 0.8, "opacity": 1.0, "spacing": 0.1},
                            "points": bloated_points,
                            "seed": rev
                        },
                        "context": {"activeLayer": 3, "foreground": [r, g, b, 1.0]}
                    }),
                    elapsed_ms: rev * 125,
                    kind: ReplayKind::Resolved,
                },
            })
            .unwrap();
    }
    let draft_usage = store.usage().unwrap();
    let expected_final_image = render_document(&working_doc).unwrap();
    let expected_baseline_doc = store.load_revision(assigned.project, 1).unwrap().0;
    let expected_baseline_image = render_document(&expected_baseline_doc).unwrap();
    drop(store);

    // Reopen in a real Session paired with the isolated Outbox and submit via `komaphone.room.submit`.
    let prepare_result = (|| -> Result<TimedQueued, String> {
        let mut session = Session::new();
        session.enable_local_projects(dir.path().join("drafts.sqlite3"), StoreOptions::default()).map_err(|e| e.to_string())?;
        session.flush_local_projects(Duration::from_secs(20)).map_err(|e| e.to_string())?;
        session.enable_rooms(rooms_db.clone(), Default::default()).map_err(|e| e.to_string())?;
        let wait_start = Instant::now();
        while session.rooms.status.room.is_none() && wait_start.elapsed() < Duration::from_secs(5) {
            session.poll_rooms();
            std::thread::sleep(Duration::from_millis(10));
        }
        session.open_room_draft().map_err(|e| e.to_string())?;
        let open_start = Instant::now();
        while session.active().is_none() && open_start.elapsed() < Duration::from_secs(10) {
            session.poll_local_projects();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(session.local_projects.status(session.active().unwrap().doc.id).local_revision, target_revisions);

        let t0 = Instant::now();
        session.execute("komaphone.room.submit", json!({})).map_err(|e| e.to_string())?;
        while (session.local_projects.has_pending() || session.rooms.status.pending == 0) && t0.elapsed() < Duration::from_secs(30) {
            session.poll_local_projects();
            session.poll_rooms();
            if let Some(err) = session.rooms.submission_error() {
                return Err(format!("unexpected submission preparation error: {err}"));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let elapsed_ms = t0.elapsed().as_millis();
        assert_eq!(session.rooms.submission_project(), Some(assigned.project), "submission must queue into outbox");
        assert!(session.rooms.status.pending > 0, "worker must commit queued submission to SQLite");
        drop(session);
        let (submission, objects, op_count, outbox_json_bytes) = decode_queued_submission(&rooms_db);
        Ok(TimedQueued { submission, objects, op_count, outbox_json_bytes, prepare_ms: elapsed_ms })
    })();

    offline_stop.store(true, Ordering::Release);
    let _ = offline_srv.join();

    // Clean up the isolated credential entry from Windows Credential Manager regardless of test outcome.
    if let Ok(cleanup) = photocraft_room_client::Outbox::open(&rooms_db, Box::new(photocraft_room_client::WindowsCredentials)) {
        let _ = cleanup.remove_credentials_for_discarded_root();
    }

    let TimedQueued { submission, objects: uploaded_objects, op_count, outbox_json_bytes, prepare_ms } = prepare_result.unwrap();
    let total_raw_bytes: u64 = uploaded_objects.values().map(|b| b.len() as u64).sum();
    let manifest_bytes = uploaded_objects.get(&submission.replay.manifest.unwrap().hash).unwrap();
    let artifact = ReplayArtifact::decode(manifest_bytes, &submission).unwrap();

    eprintln!(
        "QOL-014 measured 1,020-rev detailed Corpse drawing: local_draft_db={} B, total_upload_raw={} B ({:.2} MiB), manifest={} B, steps={}, packs={}, upload_ops={}, outbox_json={} B ({:.2} MiB), prepare_ms={} ms",
        draft_usage.stored_bytes,
        total_raw_bytes,
        total_raw_bytes as f64 / 1_048_576.0,
        manifest_bytes.len(),
        artifact.steps.len(),
        artifact.packs.len(),
        op_count,
        outbox_json_bytes,
        outbox_json_bytes as f64 / 1_048_576.0,
        prepare_ms
    );

    // 1. Keyframed replay bounds & local-draft preservation:
    assert_eq!(submission.revision, 36, "1,020 local revisions sampled to 36 steps");
    assert_eq!(submission.replay.event_count, 35);
    assert_eq!(submission.replay.duration_ms, target_revisions * 125);
    assert_eq!(artifact.steps.len(), 36);
    assert!(artifact.steps.iter().all(|s| s.params == json!({})), "bloated raw params must be stripped");
    assert!(manifest_bytes.len() < 32 * 1024, "manifest JSON shrunk from MiBs to <32 KiB: {} B", manifest_bytes.len());
    assert!(total_raw_bytes < 8 * 1024 * 1024, "compact total upload (<8 MiB): {total_raw_bytes} B");
    assert!(op_count < 300, "far below 8,190 op limit: {op_count}");
    assert!(outbox_json_bytes < 32 * 1024 * 1024, "far below 384 MiB pending limit: {outbox_json_bytes}");

    let reopened_store = Store::open(dir.path().join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    assert_eq!(reopened_store.load(assigned.project).unwrap().info.revision, target_revisions, "local undo/history remains 100% intact");
    assert_eq!(reopened_store.replay_page(assigned.project, target_revisions - 1, 1).unwrap()[0].params["input"]["seed"], target_revisions);
    drop(reopened_store);

    // 2. Validate against exact live K7 service (`f4d77aee…`) `validate_submission` + `validate_replay` rules:
    assert_matches_live_k7_service_validation(&room, &submission, &uploaded_objects);

    // 3. Complete the 2-player Exquisite Corpse game on the party authority through Reveal + Gallery + Remix:
    complete_corpse_reveal_and_verify_gallery_remix(
        &mut room,
        &host,
        &guest,
        &submission,
        &uploaded_objects,
        target_revisions * 125,
        &expected_baseline_image,
        &expected_final_image,
        expected_final_doc.layers.len(),
    );
}

#[allow(clippy::too_many_arguments)]
fn complete_corpse_reveal_and_verify_gallery_remix(
    room: &mut Room,
    host: &Participant,
    guest: &Participant,
    submission: &Submission,
    uploaded_objects: &AssetMap,
    expected_duration_ms: u64,
    expected_baseline_image: &photocraft_engine::rooms::media::RoomImage,
    expected_final_image: &photocraft_engine::rooms::media::RoomImage,
    expected_layer_count: usize,
) {
    let mut all_game_objects = uploaded_objects.clone();
    room.apply(host.id, room.revision(), 10, Action::Submit { epoch: room.epoch(), submission: submission.clone() }).unwrap();
    let dup = room.apply(host.id, room.revision(), 11, Action::Submit { epoch: room.epoch(), submission: submission.clone() }).unwrap();
    assert!(dup.duplicate);

    while room.phase() != Phase::Reveal {
        for actor in [host.id, guest.id] {
            let v = room.view(actor).unwrap();
            if let Some(a) = v.assignment.filter(|a| !a.submitted) {
                let mut part_doc =
                    Document::with_background("Friend part", Size { width: 1024, height: 512 }, ColorMode::Rgb, SampleType::U8, Color::rgb(0.95, 0.95, 0.98));
                part_doc.layers[0]
                    .surface_mut()
                    .unwrap()
                    .try_write_region(photocraft_geom::Rect::new(500, 0, 524, 24), &[0.1, 0.8, 0.3, 1.0].repeat(24 * 24))
                    .unwrap();
                let sub = minimal_submission_for_document(a.assignment, Mode::ExquisiteCorpse, &part_doc, &mut all_game_objects);
                assert_matches_live_k7_service_validation(room, &sub, &all_game_objects);
                room.apply(actor, room.revision(), 20, Action::Submit { epoch: room.epoch(), submission: sub }).unwrap();
            }
        }
    }
    assert_eq!(room.phase(), Phase::Reveal);

    let reveal_view = room.view(host.id).unwrap();
    let manifest = GalleryManifest::from_reveal(
        ProjectId::new(reveal_view.reveal.first().map(|_| "99999999999999999999999999999999").unwrap()).unwrap(),
        &reveal_view,
        host.id,
        "2026-11-18T21:00:00Z".into(),
    )
    .unwrap();
    let required = manifest.required_assets().unwrap();
    let bundle_objects: AssetMap = required.into_iter().map(|a| (a.hash, all_game_objects.get(&a.hash).unwrap().clone())).collect();
    let bundle = GalleryBundle { manifest, objects: bundle_objects };
    validate_originals(&bundle).expect("gallery_media::validate_originals");

    let info = replay_info(&bundle, 0, 0).unwrap();
    assert_eq!(info.moments.len(), 36);
    assert!(!info.baseline_only);
    assert_eq!(info.duration_ms, expected_duration_ms);
    assert_eq!(&replay_step(&bundle, 0, 0, 0).unwrap(), expected_baseline_image, "step 0 renders exact baseline");
    let mid_image = replay_step(&bundle, 0, 0, 17).unwrap();
    assert_ne!(&mid_image, expected_baseline_image, "middle keyframe shows progressive strokes");
    assert_ne!(&mid_image, expected_final_image, "middle keyframe precedes later strokes");
    assert_eq!(&replay_step(&bundle, 0, 0, 35).unwrap(), expected_final_image, "final step renders exact final pixels");
    assert_eq!(&replay_image(&bundle, 0, 0, info.duration_ms).unwrap(), expected_final_image);
    assert_eq!(&render_frame(&bundle, 0, 0).unwrap(), expected_final_image);
    let creature = render_creature(&bundle, 0).unwrap();
    assert_eq!((creature.width, creature.height), (1024, 1536));

    let (remixed_part, provenance) = remix(&bundle, 0, Some(0)).unwrap();
    assert_eq!(provenance.authors.len(), 1);
    assert_eq!(provenance.authors[0].id, host.id);
    assert_eq!(remixed_part.layers.len(), expected_layer_count, "final checkpoint must remain unflattened multi-layer");
    let layer_names: Vec<_> = remixed_part.layers.iter().map(|l| l.name.as_str()).collect();
    assert!(layer_names.contains(&"Shading") && layer_names.contains(&"Ink"), "editable layer names preserved: {layer_names:?}");
    let shading = remixed_part.layers.iter().find(|l| l.name == "Shading").unwrap();
    assert!((shading.opacity - 0.65).abs() < 1e-5, "layer opacity preserved");
    assert!(remixed_part.layers.iter().any(|l| matches!(l.content, photocraft_doc::LayerContent::Smart(_))), "embedded smart object preserved");
    assert_eq!(&render_document(&remixed_part).unwrap(), expected_final_image, "remixed editable part renders exact final pixels");
    let _ = BlendMode::Normal;

    let (remixed_creature, _) = remix(&bundle, 0, None).unwrap();
    assert_eq!((remixed_creature.size.width, remixed_creature.size.height), (1024, 1536));
    assert_eq!(remixed_creature.layers.len(), 3);
}

#[test]
fn small_drawings_and_historical_full_history_bundles_remain_compatible() {
    let dir = tempfile::tempdir().unwrap();
    let (mut room, host, guest) = setup_corpse_room();
    let mut objects = BTreeMap::new();

    // 1. Historical 300-step full-history submission with non-empty `params` (pre-QOL-014 archive format)
    // continues to validate on service + GalleryBundle and play back step-for-step!
    let host_assignment = room.view(host.id).unwrap().assignment.unwrap().assignment;
    let base_doc = Document::with_background("Hist base", Size { width: 1024, height: 512 }, ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let base_bytes = photocraft_format::save_to_bytes(&base_doc, &Default::default()).unwrap();
    let base_hash = AssetHash::new(&hex_digest(&base_bytes)).unwrap();
    let base_asset = AssetRef { hash: base_hash, bytes: base_bytes.len() as u64 };
    let base_cp = CheckpointRef { id: CheckpointId::new(&base_hash.as_str()[..32]).unwrap(), asset: base_asset };
    objects.insert(base_hash, base_bytes);

    let mut packed_entries = Vec::new();
    let mut steps = vec![ReplayArtifactStep {
        event: EventId::new(&format!("{:032x}", 1)).unwrap(),
        revision: 1,
        elapsed_ms: 0,
        command: "file.new".into(),
        params: json!({"width": 1024, "height": 512}),
        kind: "baseline".into(),
        checkpoint: base_cp,
    }];
    let mut final_doc = base_doc.clone();
    for rev in 2..=300u64 {
        let mut d = base_doc.clone();
        d.layers[0].name = format!("Historical step {rev}");
        if rev == 300 {
            d.layers[0].surface_mut().unwrap().try_write_region(photocraft_geom::Rect::new(10, 10, 30, 30), &[0.9, 0.1, 0.2, 1.0].repeat(400)).unwrap();
            final_doc = d.clone();
        }
        let bytes = photocraft_format::save_to_bytes(&d, &Default::default()).unwrap();
        let h = AssetHash::new(&hex_digest(&bytes)).unwrap();
        let asset = AssetRef { hash: h, bytes: bytes.len() as u64 };
        let cp = CheckpointRef { id: CheckpointId::new(&h.as_str()[..32]).unwrap(), asset };
        if rev < 300 {
            packed_entries.push((asset, bytes));
        } else {
            objects.insert(h, bytes);
        }
        steps.push(ReplayArtifactStep {
            event: EventId::new(&format!("{rev:032x}")).unwrap(),
            revision: rev,
            elapsed_ms: rev * 50,
            command: "paint.stroke".into(),
            params: json!({"historical_raw_params": [1, 2, 3, rev]}),
            kind: "resolved".into(),
            checkpoint: cp,
        });
    }
    let pack_bytes = pack_checkpoints(&packed_entries).unwrap();
    let pack_hash = AssetHash::new(&hex_digest(&pack_bytes)).unwrap();
    let pack_asset = AssetRef { hash: pack_hash, bytes: pack_bytes.len() as u64 };
    objects.insert(pack_hash, pack_bytes);

    let replay_id = ReplayId::new(&format!("{:032x}", 777)).unwrap();
    let final_cp = steps.last().unwrap().checkpoint;
    let end_event = Some(steps.last().unwrap().event);
    let artifact = ReplayArtifact { schema_version: 1, replay: replay_id, project: host_assignment.project, packs: vec![pack_asset], steps };
    let manifest_bytes = serde_json::to_vec(&artifact).unwrap();
    let manifest_hash = AssetHash::new(&hex_digest(&manifest_bytes)).unwrap();
    let manifest_asset = AssetRef { hash: manifest_hash, bytes: manifest_bytes.len() as u64 };
    objects.insert(manifest_hash, manifest_bytes);

    let mut comp = photocraft_compose::flatten(&final_doc);
    photocraft_party::canonical_color::to_srgb(&final_doc, &mut comp.px).unwrap();
    let rgba = comp.to_rgba8();
    let img_bytes = canonical_png(1024, 512, &rgba.pixels);
    let img_asset = AssetRef { hash: AssetHash::new(&hex_digest(&img_bytes)).unwrap(), bytes: img_bytes.len() as u64 };
    objects.insert(img_asset.hash, img_bytes);
    let seam_bytes = canonical_png(1024, 24, &rgba.pixels[(512 - 24) * 1024 * 4..]);
    let seam_asset = AssetRef { hash: AssetHash::new(&hex_digest(&seam_bytes)).unwrap(), bytes: seam_bytes.len() as u64 };
    objects.insert(seam_asset.hash, seam_bytes);

    let hist_sub = Submission {
        id: SubmissionId::new(&format!("{:032x}", 888)).unwrap(),
        assignment: host_assignment.id,
        author: host.id,
        project: host_assignment.project,
        revision: 300,
        image: img_asset,
        checkpoint: final_cp,
        replay: ReplayRef { id: replay_id, manifest: Some(manifest_asset), baseline: base_cp, end_event, event_count: 299, duration_ms: 300 * 50 },
        assets: vec![base_asset, final_cp.asset, pack_asset, manifest_asset, img_asset, seam_asset],
        note: String::new(),
        seam: Some(seam_asset),
        extent: None,
    };
    assert_matches_live_k7_service_validation(&room, &hist_sub, &objects);
    room.apply(host.id, room.revision(), 1, Action::Submit { epoch: room.epoch(), submission: hist_sub }).unwrap();
    room.apply(host.id, room.revision(), 2, Action::End { reason: "Complete historical compatibility fixture".into() }).unwrap();

    let view = room.view(guest.id).unwrap();
    let g_manifest = GalleryManifest::from_reveal(ProjectId::new(&"e".repeat(32)).unwrap(), &view, guest.id, "2026-11-18T21:05:00Z".into()).unwrap();
    let g_req = g_manifest.required_assets().unwrap();
    let g_bundle = GalleryBundle { manifest: g_manifest, objects: g_req.into_iter().map(|a| (a.hash, objects.get(&a.hash).unwrap().clone())).collect() };
    validate_originals(&g_bundle).unwrap();
    assert_eq!(replay_info(&g_bundle, 0, 0).unwrap().moments.len(), 300);
    assert_eq!(replay_step(&g_bundle, 0, 0, 150).unwrap(), render_document(&base_doc).unwrap());
    assert_eq!(replay_step(&g_bundle, 0, 0, 299).unwrap(), render_document(&final_doc).unwrap());

    // 2. Small new drawings (1 revision and 4 revisions) retain every revision without loss:
    let (offline_ep, offline_stop, offline_srv) = spawn_offline_endpoint();
    let rooms_db = dir.path().join("small_rooms.sqlite3");
    let anim_host = pair_isolated_outbox(&rooms_db, &offline_ep, "Friend 1");
    let mut anim_room = Room::new(RoomId::new(&format!("{:032x}", 1002)).unwrap(), anim_host.clone(), Mode::Animation { rotations: 1 }, None).unwrap();
    anim_room.apply(guest.id, anim_room.revision(), 0, Action::Join { participant: guest.clone() }).unwrap();
    for p in [&anim_host, &guest] {
        anim_room.apply(p.id, anim_room.revision(), 0, Action::Ready { ready: true }).unwrap();
    }
    let anim_projects: Vec<_> = (0..4).map(|n| ProjectId::new(&format!("{:032x}", 4000 + n)).unwrap()).collect();
    anim_room
        .apply(anim_host.id, anim_room.revision(), 0, Action::Start { game: GameId::new(&format!("{:032x}", 2002)).unwrap(), projects: anim_projects })
        .unwrap();
    let anim_view = anim_room.view(anim_host.id).unwrap();
    let anim_project = anim_view.assignment.as_ref().unwrap().assignment.project;
    update_isolated_outbox_poll(&rooms_db, anim_view);
    let small_res = (|| -> Result<(Submission, BTreeMap<AssetHash, Vec<u8>>), String> {
        let mut s = Session::new();
        s.enable_local_projects(dir.path().join("small_drafts.sqlite3"), StoreOptions::default()).map_err(|e| e.to_string())?;
        s.flush_local_projects(Duration::from_secs(20)).map_err(|e| e.to_string())?;
        s.enable_rooms(rooms_db.clone(), Default::default()).map_err(|e| e.to_string())?;
        let t = Instant::now();
        while s.rooms.status.room.is_none() && t.elapsed() < Duration::from_secs(5) {
            s.poll_rooms();
            std::thread::sleep(Duration::from_millis(5));
        }
        s.open_room_draft().map_err(|e| e.to_string())?;
        while s.active().is_none() && t.elapsed() < Duration::from_secs(10) {
            s.poll_local_projects();
            std::thread::sleep(Duration::from_millis(5));
        }
        for step in 0..3 {
            s.execute(
                "paint.stroke",
                json!({"points": [[20 + step * 50, 30], [80 + step * 50, 120]], "size": 10, "color": [0.2, 0.5, 0.9, 1.0], "seed": step + 1}),
            )
            .map_err(|e| e.to_string())?;
        }
        s.flush_local_projects(Duration::from_secs(20)).map_err(|e| e.to_string())?;
        assert_eq!(s.local_projects.status(s.active().unwrap().doc.id).local_revision, 4);
        s.execute("komaphone.room.submit", json!({"note": "Four exact steps"})).map_err(|e| e.to_string())?;
        let t2 = Instant::now();
        while (s.local_projects.has_pending() || s.rooms.status.pending == 0) && t2.elapsed() < Duration::from_secs(15) {
            s.poll_local_projects();
            s.poll_rooms();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(s.rooms.submission_project(), Some(anim_project));
        assert!(s.rooms.status.pending > 0);
        drop(s);
        let (sub, objs, _, _) = decode_queued_submission(&rooms_db);
        Ok((sub, objs))
    })();
    offline_stop.store(true, Ordering::Release);
    let _ = offline_srv.join();
    if let Ok(cleanup) = photocraft_room_client::Outbox::open(&rooms_db, Box::new(photocraft_room_client::WindowsCredentials)) {
        let _ = cleanup.remove_credentials_for_discarded_root();
    }
    let (small_sub, small_objs) = small_res.unwrap();
    assert_eq!(small_sub.revision, 4, "4-revision drawing retains all 4 steps");
    assert_eq!(small_sub.replay.event_count, 3);
    assert_eq!(small_sub.note, "Four exact steps");
    assert_matches_live_k7_service_validation(&anim_room, &small_sub, &small_objs);
}

#[test]
fn transfer_deadline_freeze_pin_retries_produce_identical_keyframed_submission() {
    let dir = tempfile::tempdir().unwrap();
    let (offline_ep, offline_stop, offline_srv) = spawn_offline_endpoint();
    let rooms_base = dir.path().join("rooms_base.sqlite3");
    let host = pair_isolated_outbox(&rooms_base, &offline_ep, "Friend 1");
    let (mut room, host, _guest) = setup_corpse_room_with_host(host, Some(1000));
    let drawing_view = room.view(host.id).unwrap();
    let assigned = drawing_view.assignment.as_ref().unwrap().assignment.clone();

    // Populate 90 revisions in `drafts.sqlite3`:
    let mut store = Store::open(dir.path().join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    let mut doc = Document::with_background("Timed Corpse", Size { width: 1024, height: 512 }, ColorMode::Rgb, SampleType::U8, Color::WHITE);
    store
        .save(&SaveRequest {
            project: assigned.project,
            expected_revision: 0,
            request_id: photocraft_local_store::fresh_event_id().unwrap(),
            title: "Timed Corpse".into(),
            document: doc.clone(),
            event: ReplayRecord { schema_version: 1, command: "file.new".into(), params: json!({}), elapsed_ms: 0, kind: ReplayKind::Baseline },
        })
        .unwrap();
    for rev in 2..=90u64 {
        let x = ((rev * 9) % 1000) as i32;
        doc.layers[0].surface_mut().unwrap().try_write_region(photocraft_geom::Rect::new(x, 490, x + 8, 512), &[0.3, 0.2, 0.7, 1.0].repeat(8 * 22)).unwrap();
        store
            .save(&SaveRequest {
                project: assigned.project,
                expected_revision: rev - 1,
                request_id: photocraft_local_store::fresh_event_id().unwrap(),
                title: "Timed Corpse".into(),
                document: doc.clone(),
                event: ReplayRecord {
                    schema_version: 1,
                    command: "paint.stroke".into(),
                    params: json!({"step": rev}),
                    elapsed_ms: rev * 10,
                    kind: ReplayKind::Resolved,
                },
            })
            .unwrap();
    }
    drop(store);

    // Advance authority to `Phase::Transfer`:
    assert!(room.tick(1000).unwrap());
    assert_eq!(room.phase(), Phase::Transfer);
    let transfer_view = room.view(host.id).unwrap();
    update_isolated_outbox_poll(&rooms_base, transfer_view);

    // Run deadline freeze twice on two isolated outbox copies pinned to local revision 90 and prove
    // deterministic byte-for-byte identity of `Submission`, `SubmissionId`, and `assets`!
    let mut results = Vec::new();
    for attempt in 0..2 {
        let db_path = dir.path().join(format!("rooms_attempt_{attempt}.sqlite3"));
        std::fs::copy(&rooms_base, &db_path).unwrap();
        let attempt_res = (|| -> Result<FrozenAttempt, String> {
            let mut session = Session::new();
            session.enable_local_projects(dir.path().join("drafts.sqlite3"), StoreOptions::default()).map_err(|e| e.to_string())?;
            session.flush_local_projects(Duration::from_secs(20)).map_err(|e| e.to_string())?;
            session.enable_rooms(db_path.clone(), Default::default()).map_err(|e| e.to_string())?;
            let t0 = Instant::now();
            while (session.rooms.status.room.is_none() || session.rooms.status.pending == 0) && t0.elapsed() < Duration::from_secs(15) {
                session.poll_local_projects();
                session.poll_rooms();
                if let Some(err) = session.rooms.submission_error() {
                    return Err(format!("deadline freeze failed: {err}"));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let pin = session.rooms.status.freeze_pin.clone().expect("deadline pin committed");
            assert_eq!(pin.revision, 90, "FreezePin records acknowledged local revision 90");
            assert_eq!(session.rooms.submission_project(), Some(assigned.project));
            assert!(session.rooms.status.pending > 0);
            drop(session);
            let (submission, objects, _, _) = decode_queued_submission(&db_path);
            Ok(FrozenAttempt { submission, objects, pin })
        })();
        results.push(attempt_res);
    }
    offline_stop.store(true, Ordering::Release);
    let _ = offline_srv.join();
    if let Ok(cleanup) = photocraft_room_client::Outbox::open(&rooms_base, Box::new(photocraft_room_client::WindowsCredentials)) {
        let _ = cleanup.remove_credentials_for_discarded_root();
    }
    let results: Vec<_> = results.into_iter().map(Result::unwrap).collect();

    let FrozenAttempt { submission: sub_a, objects: objs_a, pin: pin_a } = &results[0];
    let FrozenAttempt { submission: sub_b, objects: objs_b, pin: pin_b } = &results[1];
    assert_eq!(pin_a, pin_b);
    assert_eq!(sub_a, sub_b, "Transfer deadline retry produces identical Submission");
    assert_eq!(objs_a, objs_b, "Transfer deadline retry produces byte-for-byte identical asset objects");
    assert_eq!(sub_a.revision, 36, "keyframed step count is 36 while FreezePin is 90");
    assert_matches_live_k7_service_validation(&room, sub_a, objs_a);

    // Authority accepts FreezeReport + Submit in Phase::Transfer:
    room.apply(host.id, room.revision(), 1050, Action::FreezeReport { epoch: room.epoch(), submission: sub_a.clone() }).unwrap();
    room.apply(host.id, room.revision(), 1100, Action::Submit { epoch: room.epoch(), submission: sub_a.clone() }).unwrap();
}

#[test]
fn submission_preparation_error_persists_across_250ms_room_polls_and_clears_on_dismiss_retry_or_leave() {
    let dir = tempfile::tempdir().unwrap();
    // Start a real loopback HTTP room server that answers `Poll` every 250ms and `Leave` on departure!
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());

    let rooms_db = dir.path().join("poll_rooms.sqlite3");
    let host = pair_isolated_outbox(&rooms_db, &endpoint, "Friend 1");
    let (room, host, _guest) = setup_corpse_room_with_host(host, None);
    let view = room.view(host.id).unwrap();
    let assigned_project = view.assignment.as_ref().unwrap().assignment.project;
    update_isolated_outbox_poll(&rooms_db, view.clone());

    let stop = Arc::new(AtomicBool::new(false));
    let stop_bg = Arc::clone(&stop);
    let poll_count = Arc::new(AtomicUsize::new(0));
    let poll_count_bg = Arc::clone(&poll_count);
    let server_submitted = Arc::new(AtomicBool::new(false));
    let server_submitted_bg = Arc::clone(&server_submitted);
    let server_view = view.clone();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !stop_bg.load(Ordering::Acquire) && Instant::now() < deadline {
            let mut stream = match listener.accept() {
                Ok((s, _)) => s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(_) => break,
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut req_buf = Vec::new();
            let mut chunk = [0u8; 2048];
            let rpc: RpcRequest = loop {
                let n = match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => {
                        break RpcRequest {
                            protocol_version: photocraft_party::protocol::PROTOCOL_VERSION,
                            request_id: RequestId::new(&"1".repeat(32)).unwrap(),
                            operation: Operation::Disconnect { room: server_view.id },
                        };
                    }
                    Ok(n) => n,
                };
                req_buf.extend_from_slice(&chunk[..n]);
                if let Some(pos) = req_buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let hdr = String::from_utf8_lossy(&req_buf[..pos]);
                    let len = hdr
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok()))
                        .unwrap_or(0);
                    if req_buf.len() >= pos + 4 + len {
                        break serde_json::from_slice(&req_buf[pos + 4..pos + 4 + len]).unwrap();
                    }
                }
            };
            let current_view = || {
                let mut v = server_view.clone();
                if server_submitted_bg.load(Ordering::Acquire) {
                    v.revision += 1;
                    if let Some(a) = v.assignment.as_mut() {
                        a.submitted = true;
                    }
                }
                v
            };
            let resp = match rpc.operation {
                Operation::Poll { .. } => {
                    poll_count_bg.fetch_add(1, Ordering::AcqRel);
                    RpcResponse::Room { view: Box::new(current_view()), outcome: None }
                }
                Operation::Upload { total, offset, bytes, finish, hash, .. } => {
                    let next = offset + bytes.len() as u64;
                    RpcResponse::Uploaded { asset: (finish && next == total).then_some(AssetRef { hash, bytes: total }), next_offset: next }
                }
                Operation::Apply { action, .. } => match *action {
                    Action::Leave => RpcResponse::Disconnected,
                    Action::Submit { .. } => {
                        server_submitted_bg.store(true, Ordering::Release);
                        let submitted_view = current_view();
                        let rev = submitted_view.revision;
                        RpcResponse::Room {
                            view: Box::new(submitted_view),
                            outcome: Some(photocraft_party::room::ApplyOutcome { revision: rev, duplicate: false }),
                        }
                    }
                    _ => RpcResponse::Room { view: Box::new(current_view()), outcome: None },
                },
                _ => RpcResponse::Disconnected,
            };
            let body = serde_json::to_vec(&resp).unwrap();
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = stream.write_all(&body);
        }
    });

    let test_result = (|| -> Result<(), String> {
        // Pre-populate `drafts.sqlite3` with revision 1 (1024x512) and revision 2 (800x512) so opening
        // the draft and submitting asynchronously fails in `Response::Prepared` (`drafts::prepare`)!
        {
            let mut store = Store::open(dir.path().join("drafts.sqlite3"), StoreOptions::default()).map_err(|e| e.to_string())?;
            let doc_ok = Document::with_background("Corpse draft", Size { width: 1024, height: 512 }, ColorMode::Rgb, SampleType::U8, Color::WHITE);
            store
                .save(&SaveRequest {
                    project: assigned_project,
                    expected_revision: 0,
                    request_id: photocraft_local_store::fresh_event_id().map_err(|e| e.to_string())?,
                    title: "Corpse draft".into(),
                    document: doc_ok.clone(),
                    event: ReplayRecord { schema_version: 1, command: "file.new".into(), params: json!({}), elapsed_ms: 0, kind: ReplayKind::Baseline },
                })
                .map_err(|e| e.to_string())?;
            let mut doc_bad = doc_ok;
            doc_bad.size.width = 800;
            store
                .save(&SaveRequest {
                    project: assigned_project,
                    expected_revision: 1,
                    request_id: photocraft_local_store::fresh_event_id().map_err(|e| e.to_string())?,
                    title: "Corpse draft".into(),
                    document: doc_bad,
                    event: ReplayRecord {
                        schema_version: 1,
                        command: "image.canvasSize".into(),
                        params: json!({}),
                        elapsed_ms: 50,
                        kind: ReplayKind::Resolved,
                    },
                })
                .map_err(|e| e.to_string())?;
        }

        let mut session = Session::new();
        session.enable_local_projects(dir.path().join("drafts.sqlite3"), StoreOptions::default()).map_err(|e| e.to_string())?;
        session.flush_local_projects(Duration::from_secs(20)).map_err(|e| e.to_string())?;
        session.enable_rooms(rooms_db.clone(), Default::default()).map_err(|e| e.to_string())?;

        // Wait for initial room status + open assigned draft:
        let t0 = Instant::now();
        while session.rooms.status.room.is_none() && t0.elapsed() < Duration::from_secs(5) {
            session.poll_rooms();
            std::thread::sleep(Duration::from_millis(10));
        }
        session.open_room_draft().map_err(|e| e.to_string())?;
        while session.active().is_none() && t0.elapsed() < Duration::from_secs(10) {
            session.poll_local_projects();
            std::thread::sleep(Duration::from_millis(10));
        }
        let doc_id = session.active().unwrap().doc.id;
        assert_eq!(session.local_projects.status(doc_id).project, Some(assigned_project));
        assert_eq!(session.active().unwrap().doc.size.width, 800);

        session.execute("komaphone.room.submit", json!({})).map_err(|e| e.to_string())?;
        let wait_err = Instant::now();
        while session.rooms.submission_error().is_none() && wait_err.elapsed() < Duration::from_secs(5) {
            session.poll_local_projects();
            session.poll_rooms();
            std::thread::sleep(Duration::from_millis(10));
        }
        let err_msg = session.rooms.submission_error().ok_or("expected submission preparation error")?.to_owned();
        assert!(err_msg.contains("dimensions changed"), "unexpected error text: {err_msg}");
        assert_eq!(session.rooms.status.error.as_deref(), Some(err_msg.as_str()));
        assert_eq!(session.room_sync_label(doc_id), "Submission failed · local draft retained");

        // Let >= 3 live 250ms HTTP `Poll` cycles complete and prove the error is NOT wiped!
        let polls_before = poll_count.load(Ordering::Acquire);
        let poll_wait = Instant::now();
        while poll_count.load(Ordering::Acquire) < polls_before + 3 && poll_wait.elapsed() < Duration::from_secs(5) {
            session.poll_local_projects();
            session.poll_rooms();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(poll_count.load(Ordering::Acquire) >= polls_before + 3, "expected at least 3 live 250ms polls");
        session.poll_rooms();
        assert_eq!(session.rooms.submission_error(), Some(err_msg.as_str()), "250ms Poll must NOT wipe submission_error");
        assert_eq!(session.rooms.status.error.as_deref(), Some(err_msg.as_str()), "250ms Poll must NOT wipe status.error");
        assert_eq!(session.room_sync_label(doc_id), "Submission failed · local draft retained");
        let status_cmd = session.execute("komaphone.room.status", json!({})).map_err(|e| e.to_string())?;
        assert_eq!(status_cmd["submissionError"], err_msg);
        assert_eq!(status_cmd["status"]["error"], err_msg);

        // Explicit `komaphone.room.dismissError` clears it cleanly:
        session.execute("komaphone.room.dismissError", json!({})).map_err(|e| e.to_string())?;
        assert!(session.rooms.submission_error().is_none());
        assert!(session.rooms.status.error.is_none());
        assert_eq!(session.room_sync_label(doc_id), "Local draft · not submitted");

        // Trigger the preparation error a second time, then restore valid 1024x512 dimensions and
        // retry `komaphone.room.submit`: error clears and submission succeeds!
        session.execute("komaphone.room.submit", json!({})).map_err(|e| e.to_string())?;
        let wait_err2 = Instant::now();
        while session.rooms.submission_error().is_none() && wait_err2.elapsed() < Duration::from_secs(5) {
            session.poll_local_projects();
            session.poll_rooms();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(session.rooms.submission_error().is_some());

        session
            .edit("Restore 1024x512", |doc, _| {
                doc.size.width = 1024;
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        session.capture_local_fallback().map_err(|e| e.to_string())?;
        session.flush_local_projects(Duration::from_secs(20)).map_err(|e| e.to_string())?;

        session.execute("komaphone.room.submit", json!({})).map_err(|e| e.to_string())?;
        assert!(session.rooms.submission_error().is_none(), "starting a fresh submit immediately clears prior submission error");
        let wait_accept = Instant::now();
        while !session.rooms.status.room.as_ref().and_then(|r| r.assignment.as_ref()).is_some_and(|a| a.submitted)
            && wait_accept.elapsed() < Duration::from_secs(10)
        {
            session.poll_local_projects();
            session.poll_rooms();
            std::thread::sleep(Duration::from_millis(15));
        }
        assert!(session.rooms.status.room.as_ref().unwrap().assignment.as_ref().unwrap().submitted, "valid retry completes submission");
        assert!(session.rooms.submission_error().is_none());
        assert_eq!(session.room_sync_label(doc_id), "Synced · submission accepted");

        // Also verify room departure clears any set submission error:
        session.rooms.set_submission_error("Synthetic error before leave");
        assert!(session.rooms.submission_error().is_some());
        session.execute("komaphone.room.leave", json!({})).map_err(|e| e.to_string())?;
        assert!(session.rooms.submission_error().is_none(), "Leave room clears submission error");
        Ok(())
    })();

    stop.store(true, Ordering::Release);
    let _ = server.join();
    if let Ok(cleanup) = photocraft_room_client::Outbox::open(&rooms_db, Box::new(photocraft_room_client::WindowsCredentials)) {
        let _ = cleanup.remove_credentials_for_discarded_root();
    }
    test_result.unwrap();
}

#[test]
fn adversarial_and_oversized_inputs_fail_gracefully_without_panicking_or_losing_drafts() {
    let mut session = Session::new();
    // Dismiss when no error is set is disabled / rejected cleanly without panicking:
    assert!(!session.is_enabled("komaphone.room.dismissError"));
    for bad in [json!(123), json!("bad"), json!([1]), json!({"extra": 1})] {
        assert!(session.execute("komaphone.room.dismissError", bad.clone()).is_err());
        assert!(session.execute("komaphone.room.submit", bad).is_err());
    }
    assert!(session.execute("komaphone.room.submit", json!({"note": "x".repeat(2000)})).is_err());
    assert!(session.execute("komaphone.room.submit", json!({"note": "\u{0000}"})).is_err());

    session.rooms.set_submission_error("Test notice");
    assert!(session.is_enabled("komaphone.room.dismissError"));
    session.execute("komaphone.room.dismissError", json!({})).unwrap();
    assert!(!session.is_enabled("komaphone.room.dismissError"));

    // Verify oversized mandatory submission (> 384 MiB pending transfer queue budget) fails cleanly
    // in `prepare` while preserving the saved local draft 100% intact!
    let dir = tempfile::tempdir().unwrap();
    let (offline_ep, offline_stop, offline_srv) = spawn_offline_endpoint();
    let rooms_db = dir.path().join("oversized_rooms.sqlite3");
    let host = pair_isolated_outbox(&rooms_db, &offline_ep, "Friend 1");
    let (room, host, _) = setup_corpse_room_with_host(host, None);
    let view = room.view(host.id).unwrap();
    let project = view.assignment.as_ref().unwrap().assignment.project;
    update_isolated_outbox_poll(&rooms_db, view);

    let fill_noisy_f32_layers = |seed_start: u64| -> Document {
        let mut d = Document::with_background("Oversized", Size { width: 1024, height: 512 }, ColorMode::Rgb, SampleType::F32, Color::WHITE);
        let mut state = seed_start;
        let fmt = d.pixel_format();
        for layer_idx in 0..7usize {
            if layer_idx > 0 {
                d.layers.push(photocraft_doc::Layer::raster(format!("HeavyF32_{layer_idx}"), fmt));
            }
            let mut buf = vec![0.0f32; 1024 * 512 * 4];
            for sample in &mut buf {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let bits = 0x3000_0000u32 | ((state >> 32) as u32 & 0x0fff_ffff);
                *sample = f32::from_bits(bits);
            }
            d.layers[layer_idx].surface_mut().unwrap().try_write_region(photocraft_geom::Rect::new(0, 0, 1024, 512), &buf).unwrap();
        }
        d
    };

    let mut store = Store::open(dir.path().join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    let doc_a = fill_noisy_f32_layers(0x1234_5678_9abc_def0);
    store
        .save(&SaveRequest {
            project,
            expected_revision: 0,
            request_id: photocraft_local_store::fresh_event_id().unwrap(),
            title: "Oversized".into(),
            document: doc_a,
            event: ReplayRecord { schema_version: 1, command: "file.new".into(), params: json!({}), elapsed_ms: 0, kind: ReplayKind::Baseline },
        })
        .unwrap();

    let doc_b = fill_noisy_f32_layers(0xfedc_ba98_7654_3210);
    store
        .save(&SaveRequest {
            project,
            expected_revision: 1,
            request_id: photocraft_local_store::fresh_event_id().unwrap(),
            title: "Oversized".into(),
            document: doc_b,
            event: ReplayRecord { schema_version: 1, command: "paint.stroke".into(), params: json!({}), elapsed_ms: 100, kind: ReplayKind::Resolved },
        })
        .unwrap();
    drop(store);

    let oversized_err = (|| -> Result<String, String> {
        let mut s = Session::new();
        s.enable_local_projects(dir.path().join("drafts.sqlite3"), StoreOptions::default()).map_err(|e| e.to_string())?;
        s.flush_local_projects(Duration::from_secs(30)).map_err(|e| e.to_string())?;
        s.enable_rooms(rooms_db.clone(), Default::default()).map_err(|e| e.to_string())?;
        let t = Instant::now();
        while s.rooms.status.room.is_none() && t.elapsed() < Duration::from_secs(5) {
            s.poll_rooms();
            std::thread::sleep(Duration::from_millis(10));
        }
        s.open_room_draft().map_err(|e| e.to_string())?;
        while s.active().is_none() && t.elapsed() < Duration::from_secs(20) {
            s.poll_local_projects();
            std::thread::sleep(Duration::from_millis(10));
        }
        s.execute("komaphone.room.submit", json!({})).map_err(|e| e.to_string())?;
        let t2 = Instant::now();
        while s.rooms.submission_error().is_none() && t2.elapsed() < Duration::from_secs(20) {
            s.poll_local_projects();
            s.poll_rooms();
            std::thread::sleep(Duration::from_millis(10));
        }
        s.rooms.submission_error().map(str::to_owned).ok_or_else(|| "expected persistent actionable limit error".into())
    })();
    offline_stop.store(true, Ordering::Release);
    let _ = offline_srv.join();
    if let Ok(cleanup) = photocraft_room_client::Outbox::open(&rooms_db, Box::new(photocraft_room_client::WindowsCredentials)) {
        let _ = cleanup.remove_credentials_for_discarded_root();
    }
    let err = oversized_err.unwrap();
    assert!(err.contains("384 MiB pending transfer queue budget"), "expected transfer queue limit error, got: {err}");
    let preserved = Store::open(dir.path().join("drafts.sqlite3"), StoreOptions::default()).unwrap().load(project).unwrap();
    assert_eq!(preserved.info.revision, 2, "oversized submission failure must preserve the local draft untouched");
}

#[test]
fn real_unchanged_komaphone_service_accepts_keyframed_two_player_corpse_game_and_gallery_download() {
    let Some(sync_bin) = std::env::var_os("KOMAPHONE_SYNC_BIN").map(std::path::PathBuf::from) else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let probe = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let bind = format!("127.0.0.1:{port}");
    let endpoint = format!("http://{bind}");
    let service_dir = root.path().join("service");
    std::fs::create_dir_all(&service_dir).unwrap();

    let mut child = std::process::Command::new(&sync_bin)
        .args(["--bind", &bind, "--data-dir"])
        .arg(&service_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn unchanged photocraft-sync");

    let client_dbs = [root.path().join("c0/rooms.sqlite3"), root.path().join("c1/rooms.sqlite3")];
    let result = (|| -> Result<(), String> {
        let mut sessions = [Session::new(), Session::new()];
        for (i, s) in sessions.iter_mut().enumerate() {
            let sub = root.path().join(format!("c{i}"));
            std::fs::create_dir_all(&sub).unwrap();
            s.enable_local_projects(sub.join("drafts.sqlite3"), StoreOptions::default()).map_err(|e| e.to_string())?;
            s.flush_local_projects(Duration::from_secs(20)).map_err(|e| e.to_string())?;
            s.enable_rooms(sub.join("rooms.sqlite3"), Default::default()).map_err(|e| e.to_string())?;
            s.execute("komaphone.room.connect", json!({"endpoint": endpoint, "display_name": format!("LiveCheck {i}")})).map_err(|e| e.to_string())?;
        }

        let wait_both = |sessions: &mut [Session; 2], pred: &dyn Fn(&Session) -> bool, label: &str| -> Result<(), String> {
            let t = Instant::now();
            while t.elapsed() < Duration::from_secs(30) {
                for s in sessions.iter_mut() {
                    s.poll_local_projects();
                    s.poll_rooms();
                    s.poll_gallery();
                }
                if sessions.iter().all(pred) {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(format!("timed out waiting for {label}: err0={:?}, err1={:?}", sessions[0].rooms.status.error, sessions[1].rooms.status.error))
        };

        wait_both(&mut sessions, &|s| s.rooms.status.paired && !s.rooms.busy(), "paired")?;
        sessions[0].execute("komaphone.room.create", json!({"mode": {"kind": "exquisite_corpse"}, "timer_ms": null})).map_err(|e| e.to_string())?;
        let t = Instant::now();
        while (sessions[0].rooms.status.room.is_none() || sessions[0].rooms.busy()) && t.elapsed() < Duration::from_secs(10) {
            sessions[0].poll_rooms();
            std::thread::sleep(Duration::from_millis(15));
        }
        let room_id = sessions[0].rooms.status.room.as_ref().ok_or("missing created room")?.id;
        sessions[0].execute("komaphone.room.invite", json!({"expires_in_ms": 3_600_000, "uses": 1})).map_err(|e| e.to_string())?;
        while (sessions[0].rooms.invitation().is_none() || sessions[0].rooms.busy()) && t.elapsed() < Duration::from_secs(10) {
            sessions[0].poll_rooms();
            std::thread::sleep(Duration::from_millis(15));
        }
        let invite_secret = sessions[0].execute("komaphone.room.invitation", json!({})).map_err(|e| e.to_string())?["invitation"]
            .as_str()
            .ok_or("missing invite secret")?
            .to_owned();

        sessions[1].execute("komaphone.room.join", json!({"room": room_id.as_str(), "invitation": invite_secret})).map_err(|e| e.to_string())?;
        wait_both(&mut sessions, &|s| s.rooms.status.room.as_ref().is_some_and(|r| r.members.len() == 2) && !s.rooms.busy(), "both joined")?;

        sessions[0].execute("komaphone.room.ready", json!({"ready": true})).map_err(|e| e.to_string())?;
        wait_both(
            &mut sessions,
            &|s| s.rooms.status.room.as_ref().is_some_and(|r| r.members.first().is_some_and(|m| m.ready)) && !s.rooms.busy(),
            "host ready",
        )?;
        sessions[1].execute("komaphone.room.ready", json!({"ready": true})).map_err(|e| e.to_string())?;
        wait_both(&mut sessions, &|s| s.rooms.status.room.as_ref().is_some_and(|r| r.members.iter().all(|m| m.ready)) && !s.rooms.busy(), "both ready")?;
        sessions[0].execute("komaphone.room.start", json!({})).map_err(|e| e.to_string())?;

        for round in 0..3u8 {
            wait_both(
                &mut sessions,
                &|s| s.rooms.status.room.as_ref().is_some_and(|r| r.phase == Phase::Drawing && r.round == round) && !s.rooms.busy(),
                &format!("round {round} drawing"),
            )?;
            for (idx, s) in sessions.iter_mut().enumerate() {
                s.execute("komaphone.room.openDraft", json!({})).map_err(|e| e.to_string())?;
                let t_open = Instant::now();
                let target_proj = s.rooms.status.room.as_ref().unwrap().assignment.as_ref().unwrap().assignment.project;
                while s.active().is_none_or(|d| s.local_projects.status(d.doc.id).project != Some(target_proj)) && t_open.elapsed() < Duration::from_secs(10) {
                    s.poll_local_projects();
                    std::thread::sleep(Duration::from_millis(10));
                }
                // On player 0 round 0, paint 80 multi-layer strokes (exercising real keyframing > 36 steps over HTTP to unchanged photocraft-sync)!
                let stroke_count = if idx == 0 && round == 0 { 80 } else { 3 };
                if idx == 0 && round == 0 {
                    s.execute("layer.new.layer", json!({"name": "LiveServiceLayer"})).map_err(|e| e.to_string())?;
                }
                for k in 0..stroke_count {
                    let x0 = 40 + (k * 11) % 900;
                    let y0 = if k == stroke_count - 1 { 496 } else { 30 + (k * 17) % 450 };
                    s.execute(
                        "paint.stroke",
                        json!({"points": [[x0, y0], [x0 + 25, (y0 + 14).min(511)]], "size": 12, "color": [0.2, 0.6, 0.3, 1.0], "seed": k + 1}),
                    )
                    .map_err(|e| e.to_string())?;
                    if (k + 1) % 20 == 0 {
                        s.flush_local_projects(Duration::from_secs(20)).map_err(|e| e.to_string())?;
                    }
                }
                s.flush_local_projects(Duration::from_secs(30)).map_err(|e| e.to_string())?;
                s.execute("komaphone.room.submit", json!({})).map_err(|e| e.to_string())?;
            }
            if round < 2 {
                wait_both(
                    &mut sessions,
                    &|s| s.rooms.status.room.as_ref().is_some_and(|r| r.round > round) && !s.rooms.busy(),
                    &format!("advance past round {round}"),
                )?;
            }
        }
        wait_both(&mut sessions, &|s| s.rooms.status.room.as_ref().is_some_and(|r| r.phase == Phase::Reveal && r.reveal.len() == 6), "reveal")?;

        // Guest (`sessions[1]`) downloads the complete game from the unchanged service into Gallery!
        let game_id = match &sessions[1].rooms.status.room.as_ref().unwrap().reveal[0] {
            photocraft_party::room::RevealedEntry::Artwork { assignment, .. } | photocraft_party::room::RevealedEntry::Missing { assignment, .. } => {
                assignment.id.game
            }
        };
        sessions[1].execute("komaphone.gallery.download", json!({"game": game_id.as_str()})).map_err(|e| e.to_string())?;
        let t_dl = Instant::now();
        while sessions[1].gallery.items.iter().all(|item| item.id.as_str() != game_id.as_str()) && t_dl.elapsed() < Duration::from_secs(30) {
            sessions[1].poll_rooms();
            sessions[1].poll_gallery();
            if let Some(err) = &sessions[1].gallery.error {
                return Err(format!("gallery download failed: {err}"));
            }
            std::thread::sleep(Duration::from_millis(15));
        }

        let g_store =
            photocraft_local_store::gallery::GalleryStore::open(root.path().join("c1/gallery.sqlite3"), StoreOptions::default()).map_err(|e| e.to_string())?;
        let downloaded_bundle = g_store.load(ProjectId::new(game_id.as_str()).unwrap()).map_err(|e| e.to_string())?;
        validate_originals(&downloaded_bundle).map_err(|e| e.to_string())?;
        let r_info = replay_info(&downloaded_bundle, 0, 0).map_err(|e| e.to_string())?;
        assert_eq!(r_info.moments.len(), 36, "downloaded game from unchanged service has 36 sampled keyframes");
        let (remixed, _) = remix(&downloaded_bundle, 0, Some(0)).map_err(|e| e.to_string())?;
        assert!(remixed.layers.iter().any(|l| l.name == "LiveServiceLayer"), "unflattened final layers survived live-service round-trip");
        Ok(())
    })();

    let _ = child.kill();
    let _ = child.wait();
    for db in &client_dbs {
        if db.exists()
            && let Ok(cleanup) = photocraft_room_client::Outbox::open(db, Box::new(photocraft_room_client::WindowsCredentials))
        {
            let _ = cleanup.remove_credentials_for_discarded_root();
        }
    }
    result.unwrap();
}
