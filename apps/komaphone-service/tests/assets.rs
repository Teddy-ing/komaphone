//! Real checkpoint/PNG assets, bounded resumable uploads, authorization and immutable acceptance.
use photocraft_doc::{Color, ColorMode, Document, SampleType, Size};
use photocraft_party::{
    AssetHash, AssetRef, CheckpointId, CheckpointRef, DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, ReplayId, ReplayRef, RequestId, RoomId,
    Submission, SubmissionId,
    protocol::{CHUNK_BYTES, Operation, RpcRequest, RpcResponse},
    room::{Action, Phase, Reference, RoomView},
};
use photocraft_sync::{Store, StoreOptions};
use sha2::{Digest, Sha256};

fn token(n: u8) -> String {
    format!("{:064x}", u16::from(n) + 100)
}
fn person(n: u8) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{n:032x}")).unwrap(),
        device: DeviceId::new(&format!("{:032x}", u16::from(n) + 100)).unwrap(),
        display_name: format!("Player {n}"),
    }
}
fn request(n: u64, operation: Operation) -> RpcRequest {
    RpcRequest { protocol_version: photocraft_party::protocol::PROTOCOL_VERSION, request_id: RequestId::new(&format!("{n:032x}")).unwrap(), operation }
}
fn room(response: RpcResponse) -> RoomView {
    match response {
        RpcResponse::Room { view, .. } => *view,
        _ => panic!("room response"),
    }
}
struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    id: RoomId,
    next: u64,
    now: u64,
}
impl Fixture {
    fn new(players: u8, mode: Mode) -> Self {
        Self::new_timed(players, mode, None)
    }
    fn new_timed(players: u8, mode: Mode, timer_ms: Option<u64>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("service.sqlite3"), StoreOptions::default(), 0).unwrap();
        for n in 1..=players + 1 {
            store.execute(None, &request(u64::from(n), Operation::Pair { participant: person(n), credential: token(n) }), 0).unwrap();
        }
        let id = room(store.execute(Some(&token(1)), &request(10, Operation::Create { mode, timer_ms }), 0).unwrap()).id;
        store.execute(Some(&token(1)), &request(11, Operation::Invite { room: id, secret: token(50), expires_in_ms: 60000, uses: players - 1 }), 0).unwrap();
        for n in 2..=players {
            store.execute(Some(&token(n)), &request(12, Operation::Join { room: id, invitation: token(50) }), 0).unwrap();
        }
        let mut result = Self { dir, store, id, next: 100, now: 0 };
        for n in 1..=players {
            result.action(n, Action::Ready { ready: true }).unwrap();
        }
        let projects = (0..u16::from(mode.rounds(usize::from(players)).unwrap()) * u16::from(players))
            .map(|n| ProjectId::new(&format!("{:032x}", n + 1000)).unwrap())
            .collect();
        result.action(1, Action::Start { game: GameId::new(&format!("{:032x}", 2000)).unwrap(), projects }).unwrap();
        result
    }
    fn call(&mut self, n: u8, operation: Operation) -> photocraft_sync::Result<RpcResponse> {
        self.next += 1;
        self.store.execute(Some(&token(n)), &request(self.next, operation), self.now)
    }
    fn view(&mut self, n: u8) -> RoomView {
        room(self.call(n, Operation::Poll { room: self.id, after_revision: 0 }).unwrap())
    }
    fn action(&mut self, n: u8, action: Action) -> photocraft_sync::Result<RpcResponse> {
        let revision = self.view(n).revision;
        self.call(n, Operation::Apply { room: self.id, expected_revision: revision, action: Box::new(action) })
    }
    fn upload(&mut self, n: u8, bytes: &[u8]) -> AssetRef {
        let hash = AssetHash::new(&format!("{:x}", Sha256::digest(bytes))).unwrap();
        let total = bytes.len() as u64;
        for (i, chunk) in bytes.chunks(CHUNK_BYTES).enumerate() {
            let offset = (i * CHUNK_BYTES) as u64;
            self.call(n, Operation::Upload { room: self.id, hash, total, offset, bytes: chunk.to_vec(), finish: offset + chunk.len() as u64 == total })
                .unwrap();
        }
        AssetRef { hash, bytes: total }
    }
    fn submission(&mut self, n: u8) -> Submission {
        let view = self.view(n);
        let assignment = view.assignment.unwrap().assignment;
        let (width, height) = if matches!(view.mode, Mode::ExquisiteCorpse) { (1024, 512) } else { (1536, 1024) };
        let tint = if n == 1 { Color::WHITE } else { Color::rgb(0.2 * f32::from(n), 0.3, 0.8) };
        let doc = Document::with_background(format!("part-{n}-{}", assignment.id.round), Size { width, height }, ColorMode::Rgb, SampleType::U8, tint);
        self.submission_from_document(n, &doc)
    }
    fn submission_from_document(&mut self, n: u8, doc: &Document) -> Submission {
        let view = self.view(n);
        let assignment = view.assignment.unwrap().assignment;
        let (width, height) = (doc.size.width, doc.size.height);
        let native = photocraft_format::save_to_bytes(doc, &Default::default()).unwrap();
        let mut rendered = photocraft_compose::flatten(doc);
        photocraft_party::canonical_color::to_srgb(doc, &mut rendered.px).unwrap();
        let rendered = rendered.to_rgba8();
        let image = photocraft_sync::encode_png(width, height, &rendered.pixels).unwrap();
        let checkpoint = CheckpointRef { id: CheckpointId::new(&format!("{:032x}", self.next + 10000)).unwrap(), asset: self.upload(n, &native) };
        let image = self.upload(n, &image);
        let seam = if matches!(view.mode, Mode::ExquisiteCorpse) {
            let pixels = rendered.pixels.get(((height - 24) * width * 4) as usize..).unwrap();
            let bytes = photocraft_sync::encode_png(width, 24, pixels).unwrap();
            Some(self.upload(n, &bytes))
        } else {
            None
        };
        let mut assets = vec![checkpoint.asset, image];
        assets.extend(seam);
        Submission {
            id: SubmissionId::new(&format!("{:032x}", self.next + 20000)).unwrap(),
            assignment: assignment.id,
            author: person(n).id,
            project: assignment.project,
            revision: 1,
            image,
            checkpoint,
            replay: ReplayRef {
                id: ReplayId::new(&format!("{:032x}", self.next + 30000)).unwrap(),
                manifest: None,
                baseline: checkpoint,
                end_event: None,
                event_count: 0,
                duration_ms: 0,
            },
            assets,
            note: format!("private note {n}"),
            seam,
            extent: Some(photocraft_party::ImageExtent { width, height }),
        }
    }
}

#[test]
fn linear_rgb_native_checkpoints_require_srgb_previews_and_seams_at_float_and_u16_depth() {
    for depth in [SampleType::F32, SampleType::U16] {
        let mut f = Fixture::new(2, Mode::ExquisiteCorpse);
        let mut document = Document::with_background("Linear RGB handoff", Size { width: 1024, height: 512 }, ColorMode::Rgb, depth, Color::rgb(0.5, 0.5, 0.5));
        document.icc_profile = Some(photocraft_cms::Builtin::LinearSrgb.profile().to_bytes());
        let native = photocraft_format::save_to_bytes(&document, &Default::default()).unwrap();
        let raw = photocraft_compose::flatten(&document);
        let raw_image = raw.to_rgba8();
        assert_eq!(raw_image.pixels[0], 128);
        let mut canonical = raw;
        photocraft_party::canonical_color::to_srgb(&document, &mut canonical.px).unwrap();
        let canonical = canonical.to_rgba8();
        assert!((187..=189).contains(&canonical.pixels[0]), "linear half intensity must become approximately sRGB188");
        let good = f.submission_from_document(1, &document);
        assert_eq!(good.checkpoint.asset.hash.as_str(), format!("{:x}", Sha256::digest(&native)));
        let raw_png = photocraft_sync::encode_png(1024, 512, &raw_image.pixels).unwrap();
        let raw_png = f.upload(1, &raw_png);
        let raw_seam = photocraft_sync::encode_png(1024, 24, &raw_image.pixels[(512 - 24) * 1024 * 4..]).unwrap();
        let raw_seam = f.upload(1, &raw_seam);
        let epoch = f.view(1).epoch;
        for replace_image in [true, false] {
            let mut wrong = good.clone();
            if replace_image {
                wrong.assets.retain(|asset| *asset != wrong.image);
                wrong.image = raw_png;
                wrong.assets.push(raw_png);
            } else {
                wrong.assets.retain(|asset| Some(*asset) != wrong.seam);
                wrong.seam = Some(raw_seam);
                wrong.assets.push(raw_seam);
            }
            let error = f.action(1, Action::Submit { epoch, submission: wrong }).unwrap_err().to_string();
            assert!(error.contains(if replace_image { "image must use canonical" } else { "seam must use canonical" }), "{error}");
            assert!(!f.view(1).assignment.unwrap().submitted);
        }
        f.action(1, Action::Submit { epoch, submission: good.clone() }).unwrap();
        assert!(f.view(1).assignment.unwrap().submitted);
        f.action(1, Action::End { reason: "Verify retained color-managed artifact".into() }).unwrap();
        for (asset, expected) in [
            (good.checkpoint.asset, native),
            (good.image, photocraft_sync::encode_png(1024, 512, &canonical.pixels).unwrap()),
            (good.seam.unwrap(), photocraft_sync::encode_png(1024, 24, &canonical.pixels[(512 - 24) * 1024 * 4..]).unwrap()),
        ] {
            let mut received = Vec::new();
            while received.len() < asset.bytes as usize {
                let response = f.call(1, Operation::Fetch { room: f.id, hash: asset.hash, offset: received.len() as u64 }).unwrap();
                let RpcResponse::AssetChunk { bytes, total } = response else { panic!("asset response") };
                assert_eq!(total, asset.bytes);
                assert!(!bytes.is_empty());
                received.extend(bytes);
            }
            assert_eq!(received, expected, "accepted original checkpoint and color-managed derivative must survive intact");
        }
    }
}

#[test]
fn native_manifest_budget_matches_local_drafts_and_rejects_oversized_metadata() {
    let mut f = Fixture::new(2, Mode::ExquisiteCorpse);
    let original = f.submission(1);
    let epoch = f.view(1).epoch;
    for (metadata_bytes, accepted) in [(9 * 1024 * 1024, false), (2 * 1024 * 1024, true)] {
        let mut document = Document::with_background("Manifest bound", Size { width: 1024, height: 512 }, ColorMode::Rgb, SampleType::U8, Color::WHITE);
        document.metadata.xmp = Some(std::sync::Arc::from("m".repeat(metadata_bytes)));
        let native = photocraft_format::save_to_bytes(&document, &Default::default()).unwrap();
        let mut limits = photocraft_format::LoadOptions { max_manifest_bytes: 1024 * 1024, ..Default::default() };
        assert!(photocraft_format::load_from_bytes_with(&native, &limits).is_err(), "fixture must exceed the former 1 MiB manifest bound");
        limits.max_manifest_bytes = 8 * 1024 * 1024;
        assert_eq!(photocraft_format::load_from_bytes_with(&native, &limits).is_ok(), accepted);
        let checkpoint = CheckpointRef { id: CheckpointId::new(&format!("{:032x}", f.next + 10000)).unwrap(), asset: f.upload(1, &native) };
        let mut submission = original.clone();
        submission.assets.retain(|asset| *asset != submission.checkpoint.asset);
        submission.assets.push(checkpoint.asset);
        submission.checkpoint = checkpoint;
        submission.replay.baseline = checkpoint;
        let result = f.action(1, Action::Submit { epoch, submission });
        if accepted {
            result.unwrap();
        } else {
            assert!(result.unwrap_err().to_string().contains("native checkpoint"));
        }
        assert_eq!(f.view(1).assignment.unwrap().submitted, accepted);
    }
}

#[test]
fn deterministic_service_deadline_freezes_manifest_and_grace_labels_missing_work() {
    let mut f = Fixture::new_timed(2, Mode::Animation { rotations: 1 }, Some(1000));
    let submission = f.submission(1);
    f.now = 1000;
    f.store.tick(f.now).unwrap();
    let transfer = f.view(1);
    assert_eq!(transfer.phase, Phase::Transfer);
    let epoch = transfer.epoch;
    f.action(1, Action::FreezeReport { epoch, submission: submission.clone() }).unwrap();
    let mut different = submission.clone();
    different.note.push_str(" changed after freeze");
    assert!(f.action(1, Action::Submit { epoch, submission: different }).is_err());
    f.action(1, Action::Submit { epoch, submission: submission.clone() }).unwrap();
    f.now = 30999;
    f.view(1);
    f.view(2);
    f.now = 31000;
    f.store.tick(f.now).unwrap();
    let next = f.view(1);
    assert_eq!(next.phase, Phase::Drawing);
    assert_eq!(next.round, 1);
    assert!(matches!(next.assignment.unwrap().reference, Some(Reference::Missing)));
    let mut late = submission;
    late.id = SubmissionId::new(&format!("{:032x}", 888888)).unwrap();
    assert!(f.action(1, Action::Submit { epoch, submission: late }).is_err());
}

#[test]
fn real_native_assets_complete_two_three_players_both_modes_with_same_base_revision_and_duplicates() {
    for players in [2, 3] {
        for mode in [Mode::Animation { rotations: 1 }, Mode::ExquisiteCorpse] {
            let mut f = Fixture::new(players, mode);
            for round in 0..mode.rounds(usize::from(players)).unwrap() {
                let view = f.view(1);
                let base = view.revision;
                let epoch = view.epoch;
                let submissions: Vec<_> = (1..=players).map(|n| f.submission(n)).collect();
                let mut last = None;
                for n in (1..=players).rev() {
                    let s = submissions.get(usize::from(n - 1)).unwrap().clone();
                    f.next += 1;
                    let request = request(
                        f.next,
                        Operation::Apply { room: f.id, expected_revision: base, action: Box::new(Action::Submit { epoch, submission: s.clone() }) },
                    );
                    let response = f.store.execute(Some(&token(n)), &request, 0).unwrap();
                    let revision = room(response).revision;
                    f.store.execute(Some(&token(n)), &request, 0).unwrap();
                    assert_eq!(f.view(n).revision, revision);
                    let mut changed = request.clone();
                    if let Operation::Apply { action, .. } = &mut changed.operation
                        && let Action::Submit { submission, .. } = action.as_mut()
                    {
                        submission.note.push('!');
                    }
                    assert!(f.store.execute(Some(&token(n)), &changed, 0).is_err());
                    last = Some((request, n));
                }
                let state = f.view(1);
                assert_eq!(state.round, if round + 1 == mode.rounds(usize::from(players)).unwrap() { round } else { round + 1 });
                let (request, n) = last.unwrap();
                let unchanged = state.revision;
                f.store.execute(Some(&token(n)), &request, 0).unwrap();
                assert_eq!(f.view(1).revision, unchanged);
            }
            let revealed = f.view(1);
            assert_eq!(revealed.phase, Phase::Reveal);
            assert_eq!(revealed.reveal.len(), usize::from(players) * usize::from(mode.rounds(usize::from(players)).unwrap()));
            assert!(!serde_json::to_string(&revealed).unwrap().contains("private note"));
        }
    }
}

#[test]
fn hidden_assets_and_hash_attachment_are_denied_even_when_the_digest_is_known() {
    let mut f = Fixture::new(2, Mode::ExquisiteCorpse);
    let s = f.submission(1);
    assert!(f.call(2, Operation::Fetch { room: f.id, hash: s.image.hash, offset: 0 }).is_err());
    assert!(f.call(3, Operation::Fetch { room: f.id, hash: s.image.hash, offset: 0 }).is_err());
    let mut other = f.submission(2);
    other.assets.push(s.checkpoint.asset);
    let epoch = f.view(2).epoch;
    assert!(f.action(2, Action::Submit { epoch, submission: other }).is_err());
    f.action(1, Action::Submit { epoch, submission: s.clone() }).unwrap();
    let own = f.submission(2);
    f.action(2, Action::Submit { epoch, submission: own }).unwrap();
    let next = f.view(2);
    assert!(matches!(next.assignment.as_ref().unwrap().reference, Some(Reference::Seam { .. })));
    assert!(f.call(2, Operation::Fetch { room: f.id, hash: s.image.hash, offset: 0 }).is_err());
    assert!(f.call(2, Operation::Fetch { room: f.id, hash: s.checkpoint.asset.hash, offset: 0 }).is_err());
    assert!(f.call(2, Operation::Fetch { room: f.id, hash: s.seam.unwrap().hash, offset: 0 }).is_ok());
}

#[test]
fn canonical_strip_rejects_pixel_forgery_and_hidden_trailing_payload() {
    for hidden in [false, true] {
        let mut f = Fixture::new(2, Mode::ExquisiteCorpse);
        let mut s = f.submission(1);
        let pixels = vec![if hidden { 255 } else { 0 }; 1024 * 24 * 4];
        let mut bytes = photocraft_sync::encode_png(1024, 24, &pixels).unwrap();
        if hidden {
            bytes.extend_from_slice(b"forbidden original hidden after PNG");
        }
        let forged = f.upload(1, &bytes);
        s.assets.retain(|a| Some(*a) != s.seam);
        s.seam = Some(forged);
        s.assets.push(forged);
        let epoch = f.view(1).epoch;
        assert!(f.action(1, Action::Submit { epoch, submission: s }).is_err());
        assert!(!f.view(1).assignment.unwrap().submitted);
    }
}

#[test]
fn partial_upload_survives_restart_and_mismatched_overlap_or_hash_never_completes() {
    let mut f = Fixture::new(2, Mode::Animation { rotations: 1 });
    let bytes = vec![42u8; CHUNK_BYTES + 15];
    let hash = AssetHash::new(&format!("{:x}", Sha256::digest(&bytes))).unwrap();
    let total = bytes.len() as u64;
    let first = Operation::Upload { room: f.id, hash, total, offset: 0, bytes: bytes[..CHUNK_BYTES].to_vec(), finish: false };
    f.call(1, first.clone()).unwrap();
    let path = f.dir.path().join("service.sqlite3");
    drop(f.store);
    f.now = 7 * 24 * 60 * 60 * 1000;
    f.store = Store::open(path, StoreOptions::default(), f.now).unwrap();
    f.view(1);
    f.call(1, first).unwrap();
    assert!(f.call(1, Operation::Upload { room: f.id, hash, total, offset: 0, bytes: vec![1; 10], finish: false }).is_err());
    assert!(f.call(1, Operation::Upload { room: f.id, hash, total, offset: CHUNK_BYTES as u64, bytes: vec![1; 15], finish: true }).is_err());
    let response = f.call(1, Operation::Upload { room: f.id, hash, total, offset: CHUNK_BYTES as u64, bytes: vec![42; 15], finish: true }).unwrap();
    assert!(matches!(response, RpcResponse::Uploaded { asset: Some(_), .. }));
}

#[test]
fn online_backup_restores_room_and_complete_assets_without_overwriting_a_destination() {
    let mut f = Fixture::new(2, Mode::ExquisiteCorpse);
    let s = f.submission(1);
    let epoch = f.view(1).epoch;
    f.action(1, Action::Submit { epoch, submission: s }).unwrap();
    let backup = f.dir.path().join("backup.sqlite3");
    f.store.backup(&backup).unwrap();
    assert!(f.store.backup(&backup).is_err());
    let mut restored = Store::open(backup, StoreOptions::default(), 0).unwrap();
    let saved = room(restored.execute(Some(&token(1)), &request(99999, Operation::Poll { room: f.id, after_revision: 0 }), 0).unwrap());
    assert!(saved.assignment.unwrap().submitted);
    assert!(saved.paused.is_some());
}

#[test]
fn nonzero_replay_requires_complete_valid_manifest_and_never_executes_params() {
    use photocraft_party::{
        EventId,
        replay_artifact::{ReplayArtifact, ReplayArtifactStep},
    };
    for corrupt in [false, true] {
        let mut f = Fixture::new(2, Mode::ExquisiteCorpse);
        let mut s = f.submission(1);
        s.revision = 2;
        s.replay.event_count = 1;
        s.replay.duration_ms = 1200;
        let first = EventId::new(&format!("{:032x}", 50001)).unwrap();
        let last = EventId::new(&format!("{:032x}", 50002)).unwrap();
        s.replay.end_event = Some(last);
        let epoch = f.view(1).epoch;
        assert!(f.action(1, Action::Submit { epoch, submission: s.clone() }).is_err());
        let artifact = ReplayArtifact {
            schema_version: 1,
            replay: s.replay.id,
            project: s.project,
            packs: vec![],
            steps: vec![
                ReplayArtifactStep {
                    event: first,
                    revision: 1,
                    elapsed_ms: 0,
                    command: "file.new".into(),
                    params: serde_json::json!({}),
                    kind: "baseline".into(),
                    checkpoint: s.checkpoint,
                },
                ReplayArtifactStep {
                    event: last,
                    revision: if corrupt { 9 } else { 2 },
                    elapsed_ms: 1200,
                    command: "file.saveAs".into(),
                    params: serde_json::json!({"path":"must-never-be-executed"}),
                    kind: "checkpoint".into(),
                    checkpoint: s.checkpoint,
                },
            ],
        };
        let bytes = serde_json::to_vec(&artifact).unwrap();
        let asset = f.upload(1, &bytes);
        s.replay.manifest = Some(asset);
        s.assets.push(asset);
        assert_eq!(f.action(1, Action::Submit { epoch, submission: s }).is_ok(), !corrupt);
        assert_eq!(f.view(1).assignment.unwrap().submitted, !corrupt);
        assert!(!std::path::Path::new("must-never-be-executed").exists());
    }
}

#[test]
fn old_receipt_after_next_game_keeps_acceptance_without_redelivering_old_reveal() {
    let mut f = Fixture::new(2, Mode::ExquisiteCorpse);
    let s = f.submission(1);
    let state = f.view(1);
    f.action(1, Action::Submit { epoch: state.epoch, submission: s }).unwrap();
    let revision = f.view(1).revision;
    f.next += 1;
    let end = request(
        f.next,
        Operation::Apply { room: f.id, expected_revision: revision, action: Box::new(Action::End { reason: "test incomplete archive".into() }) },
    );
    let before = f.store.execute(Some(&token(1)), &end, 0).unwrap();
    let RpcResponse::Room { view: old, outcome: original } = before else { panic!("room") };
    assert!(!old.reveal.is_empty());
    f.action(1, Action::NextGame).unwrap();
    let replay = f.store.execute(Some(&token(1)), &end, 0).unwrap();
    let RpcResponse::Room { view: current, outcome } = replay else { panic!("room") };
    assert_eq!(outcome, original);
    assert!(current.reveal.is_empty());
    assert_eq!(current.phase, Phase::Lobby);
    let old_game = GameId::new(&format!("{:032x}", 2000)).unwrap();
    f.action(1, Action::Ready { ready: true }).unwrap();
    f.action(2, Action::Ready { ready: true }).unwrap();
    let projects = (0..6).map(|n| ProjectId::new(&format!("{:032x}", n + 99000)).unwrap()).collect();
    assert!(f.action(1, Action::Start { game: old_game, projects }).is_err());
}

#[test]
fn packed_replay_accepts_three_hundred_distinct_edits_and_rejects_missing_members() {
    use photocraft_party::{
        EventId,
        replay_artifact::{ReplayArtifact, ReplayArtifactStep, pack_checkpoints},
    };
    for missing in [false, true] {
        let mut f = Fixture::new(2, Mode::ExquisiteCorpse);
        let mut s = f.submission(1);
        let mut entries = Vec::new();
        let mut steps = Vec::new();
        let step = |revision: u64, checkpoint: CheckpointRef| ReplayArtifactStep {
            event: EventId::new(&format!("{:032x}", 70000 + revision)).unwrap(),
            revision,
            elapsed_ms: (revision - 1) * 10,
            command: if revision == 1 { "file.new".into() } else { "layer.rename".into() },
            params: serde_json::json!({}),
            kind: if revision == 1 { "baseline".into() } else { "checkpoint".into() },
            checkpoint,
        };
        steps.push(step(1, s.checkpoint));
        for index in 0..300 {
            let doc =
                Document::with_background(format!("intermediate-{index}"), Size { width: 1024, height: 512 }, ColorMode::Rgb, SampleType::U8, Color::WHITE);
            let bytes = photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap();
            let digest = format!("{:x}", Sha256::digest(&bytes));
            let asset = AssetRef { hash: AssetHash::new(&digest).unwrap(), bytes: bytes.len() as u64 };
            let checkpoint = CheckpointRef { id: CheckpointId::new(digest.get(..32).unwrap()).unwrap(), asset };
            steps.push(step(index + 2, checkpoint));
            if !missing || index != 299 {
                entries.push((asset, bytes));
            }
        }
        steps.push(step(302, s.checkpoint));
        s.revision = 302;
        s.replay.event_count = 301;
        s.replay.duration_ms = 3010;
        s.replay.end_event = Some(steps.last().unwrap().event);
        let pack = f.upload(1, &pack_checkpoints(&entries).unwrap());
        s.assets.push(pack);
        let artifact = ReplayArtifact { schema_version: 1, replay: s.replay.id, project: s.project, packs: vec![pack], steps };
        let manifest = f.upload(1, &serde_json::to_vec(&artifact).unwrap());
        s.assets.push(manifest);
        s.replay.manifest = Some(manifest);
        assert!(s.assets.len() < 10);
        let epoch = f.view(1).epoch;
        assert_eq!(f.action(1, Action::Submit { epoch, submission: s }).is_ok(), !missing);
    }
}

#[test]
fn variable_height_corpse_submissions_derive_exact_bottom_seam_and_reject_mismatches() {
    use photocraft_doc::Rect;
    use photocraft_party::{
        EventId, ImageExtent,
        replay_artifact::{ReplayArtifact, ReplayArtifactStep, pack_checkpoints},
        room::RevealedEntry,
    };
    let mut f = Fixture::new(3, Mode::ExquisiteCorpse);
    let heights_by_round: [[u32; 3]; 3] = [
        [192, 512, 1024], // round 0: players 1, 2, 3
        [768, 2048, 128], // round 1
        [1536, 384, 640], // round 2
    ];
    let paint_band = |height: u32, top_rgb: [u8; 3], seam_rgb: [u8; 3]| -> Document {
        let mut doc = Document::with_background(
            format!("corpse-{height}"),
            Size { width: 1024, height },
            ColorMode::Rgb,
            SampleType::U8,
            Color::rgb(f32::from(top_rgb[0]) / 255.0, f32::from(top_rgb[1]) / 255.0, f32::from(top_rgb[2]) / 255.0),
        );
        let seam_px = [seam_rgb[0], seam_rgb[1], seam_rgb[2], 255].repeat((1024 * 24) as usize);
        doc.layers[0].surface_mut().unwrap().write_interleaved(Rect::from_xywh(0, (height - 24) as i32, 1024, 24), &seam_px);
        doc
    };
    // Reject mismatched declared extent vs checkpoint/PNG before accepting valid submission.
    let bad_doc = paint_band(768, [20, 40, 60], [200, 100, 50]);
    let mut bad_sub = f.submission_from_document(1, &bad_doc);
    bad_sub.extent = Some(ImageExtent { width: 1024, height: 512 });
    let epoch = f.view(1).epoch;
    assert!(f.action(1, Action::Submit { epoch, submission: bad_sub }).is_err());
    // Reject out-of-range segment heights (127 and 2049).
    for invalid_h in [127u32, 2049] {
        let invalid_doc = Document::with_background("bad", Size { width: 1024, height: invalid_h }, ColorMode::Rgb, SampleType::U8, Color::WHITE);
        let invalid_sub = f.submission_from_document(1, &invalid_doc);
        assert!(f.action(1, Action::Submit { epoch, submission: invalid_sub }).is_err());
    }
    // Complete all 3 rounds with mixed heights + multi-height replay history on round 0 player 1.
    let mut expected_seam_rgb: std::collections::BTreeMap<(u8, u8), [u8; 3]> = std::collections::BTreeMap::new();
    let mut expected_heights: std::collections::BTreeMap<(u8, u8), u32> = std::collections::BTreeMap::new();
    for (round, round_heights) in heights_by_round.iter().enumerate() {
        for player in 1..=3u8 {
            let view = f.view(player);
            let assigned = view.assignment.as_ref().unwrap();
            let chain = assigned.assignment.id.chain;
            if round > 0 {
                // Verify seam matches previous round's actual bottom 24 rows, not row 512 - 24.
                let Some(Reference::Seam { image: seam_ref, width: 1024, height: 24 }) = assigned.reference else {
                    panic!("expected seam reference for round {round}");
                };
                let RpcResponse::AssetChunk { bytes, total } = f.call(player, Operation::Fetch { room: f.id, hash: seam_ref.hash, offset: 0 }).unwrap() else {
                    panic!("asset chunk");
                };
                assert_eq!(bytes.len() as u64, total);
                let decoded = image::load_from_memory(&bytes).unwrap().into_rgba8();
                assert_eq!(decoded.dimensions(), (1024, 24));
                let want = expected_seam_rgb[&(chain, (round - 1) as u8)];
                assert_eq!(&decoded.as_raw()[..4], &[want[0], want[1], want[2], 255]);
                assert_eq!(&decoded.as_raw()[decoded.as_raw().len() - 4..], &[want[0], want[1], want[2], 255]);
            }
            let h = round_heights[(player - 1) as usize];
            let top_rgb = [30 + player * 20, 40 + (round as u8) * 30, 80];
            let seam_rgb = [100 + chain * 40, 120 + (round as u8) * 35, 180 + player * 15];
            expected_seam_rgb.insert((chain, round as u8), seam_rgb);
            expected_heights.insert((chain, round as u8), h);
            let doc = paint_band(h, top_rgb, seam_rgb);
            let mut sub = f.submission_from_document(player, &doc);
            if round == 0 && player == 2 {
                // Legacy omission (`extent: None`) still accepted for 1024x512.
                assert_eq!(h, 512);
                sub.extent = None;
            }
            if round == 0 && player == 1 {
                // Include a baseline (512 px) -> intermediate (800 px) -> final (192 px) replay.
                let base_doc = Document::with_background("base-512", Size { width: 1024, height: 512 }, ColorMode::Rgb, SampleType::U8, Color::WHITE);
                let base_bytes = photocraft_format::save_to_bytes(&base_doc, &Default::default()).unwrap();
                let base_asset = f.upload(player, &base_bytes);
                let base_cp = CheckpointRef { id: CheckpointId::new(&base_asset.hash.as_str()[..32]).unwrap(), asset: base_asset };
                let mid_doc = Document::with_background("mid-800", Size { width: 1024, height: 800 }, ColorMode::Rgb, SampleType::U8, Color::WHITE);
                let mid_bytes = photocraft_format::save_to_bytes(&mid_doc, &Default::default()).unwrap();
                let mid_digest = format!("{:x}", Sha256::digest(&mid_bytes));
                let mid_asset = AssetRef { hash: AssetHash::new(&mid_digest).unwrap(), bytes: mid_bytes.len() as u64 };
                let mid_cp = CheckpointRef { id: CheckpointId::new(&mid_digest[..32]).unwrap(), asset: mid_asset };
                let pack = f.upload(player, &pack_checkpoints(&[(mid_asset, mid_bytes)]).unwrap());
                let steps = vec![
                    ReplayArtifactStep {
                        event: EventId::new(&format!("{:032x}", 80001)).unwrap(),
                        revision: 1,
                        elapsed_ms: 0,
                        command: "file.new".into(),
                        params: serde_json::json!({}),
                        kind: "baseline".into(),
                        checkpoint: base_cp,
                    },
                    ReplayArtifactStep {
                        event: EventId::new(&format!("{:032x}", 80002)).unwrap(),
                        revision: 2,
                        elapsed_ms: 50,
                        command: "komaphone.corpse.setSegmentHeight".into(),
                        params: serde_json::json!({}),
                        kind: "checkpoint".into(),
                        checkpoint: mid_cp,
                    },
                    ReplayArtifactStep {
                        event: EventId::new(&format!("{:032x}", 80003)).unwrap(),
                        revision: 3,
                        elapsed_ms: 100,
                        command: "komaphone.corpse.setSegmentHeight".into(),
                        params: serde_json::json!({}),
                        kind: "checkpoint".into(),
                        checkpoint: sub.checkpoint,
                    },
                ];
                sub.revision = 3;
                sub.replay.baseline = base_cp;
                sub.replay.event_count = 2;
                sub.replay.duration_ms = 100;
                sub.replay.end_event = Some(steps[2].event);
                sub.assets.push(base_asset);
                sub.assets.push(pack);
                let artifact = ReplayArtifact { schema_version: 1, replay: sub.replay.id, project: sub.project, packs: vec![pack], steps };
                let manifest = f.upload(player, &serde_json::to_vec(&artifact).unwrap());
                sub.assets.push(manifest);
                sub.replay.manifest = Some(manifest);
            }
            let epoch = view.epoch;
            f.action(player, Action::Submit { epoch, submission: sub }).unwrap();
        }
    }
    let revealed = f.view(1);
    assert_eq!(revealed.phase, Phase::Reveal);
    assert_eq!(revealed.reveal.len(), 9);
    for entry in &revealed.reveal {
        let RevealedEntry::Artwork { assignment, .. } = entry else { panic!("expected artwork") };
        let extent = entry.resolved_extent(Mode::ExquisiteCorpse).unwrap();
        assert_eq!(extent, ImageExtent { width: 1024, height: expected_heights[&(assignment.id.chain, assignment.id.round)] });
    }
}
