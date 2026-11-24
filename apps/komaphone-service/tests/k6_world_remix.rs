//! Real native game assets become independent, authorized world derivatives.
//! The oracle retains the canonical submission PNGs, not service-rendered answers.
use std::collections::BTreeMap;

#[path = "k6_world_remix/trash.rs"]
mod trash;

use photocraft_doc::{Color, ColorMode, Document, Rect, SampleType, Size};
use photocraft_party::{
    AssetHash as PartyHash, AssetRef, CheckpointId, CheckpointRef, DeviceId as PartyDevice, GameId, Mode, Participant, ParticipantId, ProjectId, ReplayId,
    ReplayRef, RequestId, RoomId, Submission, SubmissionId,
    protocol::{CHUNK_BYTES, Operation, RpcRequest, RpcResponse},
    room::{Action as RoomAction, Phase, RoomView},
};
use photocraft_scene::shared::media::{AssetHash, MediaPlacement, MediaSource, Provenance};
use photocraft_scene::shared::{AcceptedOp, DeviceId, Edit, MediaId, OpId, SourceId, WorldOp};
use photocraft_scene::world_protocol::{Action, GameMediaSource, PreparedMedia, Request, Response, VERSION};
use photocraft_scene::{CanvasId, OwnerId};
use photocraft_sync::{SaveStage, Store, StoreOptions, worlds::Limits};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

#[path = "k6_world_remix/restore.rs"]
mod restore;

fn hex(n: u64) -> String {
    format!("{n:032x}")
}
fn token(n: u8) -> String {
    format!("{:064x}", u64::from(n) + 500)
}
fn person(n: u8) -> Participant {
    Participant {
        id: ParticipantId::new(&hex(u64::from(n))).unwrap(),
        device: PartyDevice::new(&hex(u64::from(n) + 100)).unwrap(),
        display_name: if n == 2 { "Synthetic\n\tauthor 2".into() } else { format!("Synthetic author {n}") },
    }
}
fn canvas(n: u64) -> CanvasId {
    CanvasId::new(&hex(n)).unwrap()
}
fn owner(n: u8) -> OwnerId {
    OwnerId::new(person(n).id.as_str()).unwrap()
}
fn request(action: Action) -> Request {
    Request { version: VERSION, action }
}
fn room(response: RpcResponse) -> RoomView {
    match response {
        RpcResponse::Room { view, .. } | RpcResponse::Game { view } => *view,
        other => panic!("room: {other:?}"),
    }
}
fn world(store: &mut Store, actor: u8, action: Action) -> Response {
    store.world_execute(&token(actor), &request(action), 0).unwrap()
}
fn denied(store: &mut Store, actor: u8, action: Action) {
    assert!(store.world_execute(&token(actor), &request(action), 0).is_err());
}
fn events(store: &mut Store, actor: u8, id: u64) -> Vec<AcceptedOp> {
    let Response::Events { events, next: None, .. } = world(store, actor, Action::Events { canvas: canvas(id), after: 0, through: None, limit: 128 }) else {
        panic!("events")
    };
    events
}
fn create(store: &mut Store, actor: u8, id: u64) {
    world(store, actor, Action::Create { canvas: canvas(id), name: format!("World {id}") });
}
fn invite(store: &mut Store, by: u8, actor: u8, id: u64, invitation: u64) {
    let op = OpId::new(&hex(invitation)).unwrap();
    let secret = format!("{invitation:064x}");
    world(store, by, Action::Invite { canvas: canvas(id), id: op, recipient: owner(actor), secret: secret.clone(), expires_at_ms: 60_000 });
    world(store, actor, Action::Accept { canvas: canvas(id), id: op, secret });
}
fn operation(actor: u8, target: u64, sequence: u64, id: u64, prepared: &PreparedMedia) -> WorldOp {
    WorldOp {
        id: OpId::new(&hex(id)).unwrap(),
        canvas: canvas(target),
        author: owner(actor),
        device: DeviceId::new(person(actor).device.as_str()).unwrap(),
        sequence,
        authority_epoch: 1,
        action: Edit::PlaceMedia {
            media: Box::new(MediaPlacement {
                id: MediaId::new(&hex(id)).unwrap(),
                canvas: canvas(target),
                owner: owner(actor),
                anchor: Default::default(),
                offset: [30., 50.],
                width: 400.,
                height: 300.,
                frames: prepared.frames.clone(),
                looping: prepared.looping,
                provenance: prepared.provenance.clone(),
            }),
        },
    }
}
fn prepared(store: &mut Store, actor: u8, id: u64, source: GameMediaSource) -> PreparedMedia {
    let Response::MediaPrepared { media } = world(store, actor, Action::PrepareGameMedia { canvas: canvas(id), source }) else { panic!("prepared media") };
    *media
}
fn fetch(store: &mut Store, actor: u8, id: u64, hash: AssetHash) -> Vec<u8> {
    let mut result = Vec::new();
    loop {
        let Response::MediaChunk { bytes, total } = world(store, actor, Action::FetchMedia { canvas: canvas(id), hash, offset: result.len() as u64 }) else {
            panic!("media chunk")
        };
        assert!(!bytes.is_empty() && bytes.len() <= CHUNK_BYTES);
        result.extend(bytes);
        if result.len() as u64 == total {
            break;
        }
        assert!((result.len() as u64) < total);
    }
    assert_eq!(format!("{:x}", Sha256::digest(&result)), hash.as_str());
    result
}

struct Fixture {
    directory: tempfile::TempDir,
    store: Store,
    room: RoomId,
    game: GameId,
    mode: Mode,
    players: u8,
    next: u64,
    pngs: BTreeMap<(u8, u8), Vec<u8>>,
}
impl Fixture {
    fn new(mode: Mode, players: u8) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(directory.path().join("service.sqlite3"), StoreOptions::default(), 0).unwrap();
        for actor in 1..=4 {
            store
                .execute(
                    None,
                    &RpcRequest {
                        protocol_version: photocraft_party::protocol::PROTOCOL_VERSION,
                        request_id: RequestId::new(&hex(u64::from(actor))).unwrap(),
                        operation: Operation::Pair { participant: person(actor), credential: token(actor) },
                    },
                    0,
                )
                .unwrap();
        }
        let room = room(
            store
                .execute(
                    Some(&token(1)),
                    &RpcRequest {
                        protocol_version: photocraft_party::protocol::PROTOCOL_VERSION,
                        request_id: RequestId::new(&hex(10)).unwrap(),
                        operation: Operation::Create { mode, timer_ms: None },
                    },
                    0,
                )
                .unwrap(),
        )
        .id;
        let game = GameId::new(&hex(2000)).unwrap();
        let mut f = Self { directory, store, room, game, mode, players, next: 100, pngs: BTreeMap::new() };
        f.call(1, Operation::Invite { room, secret: token(50), expires_in_ms: 60000, uses: players - 1 });
        for actor in 2..=players {
            f.call(actor, Operation::Join { room, invitation: token(50) });
        }
        for actor in 1..=players {
            f.action(actor, RoomAction::Ready { ready: true });
        }
        let rounds = mode.rounds(usize::from(players)).unwrap();
        let projects = (0..u64::from(rounds) * u64::from(players)).map(|index| ProjectId::new(&hex(1000 + index)).unwrap()).collect();
        f.action(1, RoomAction::Start { game, projects });
        create(&mut f.store, 1, 100);
        f
    }
    fn call(&mut self, actor: u8, operation: Operation) -> RpcResponse {
        self.next += 1;
        self.store
            .execute(
                Some(&token(actor)),
                &RpcRequest { protocol_version: photocraft_party::protocol::PROTOCOL_VERSION, request_id: RequestId::new(&hex(self.next)).unwrap(), operation },
                0,
            )
            .unwrap()
    }
    fn view(&mut self, actor: u8) -> RoomView {
        room(self.call(actor, Operation::Poll { room: self.room, after_revision: 0 }))
    }
    fn action(&mut self, actor: u8, action: RoomAction) -> RoomView {
        let revision = self.view(actor).revision;
        room(self.call(actor, Operation::Apply { room: self.room, expected_revision: revision, action: Box::new(action) }))
    }
    fn upload(&mut self, actor: u8, bytes: &[u8]) -> AssetRef {
        let asset = AssetRef { hash: PartyHash::new(&format!("{:x}", Sha256::digest(bytes))).unwrap(), bytes: bytes.len() as u64 };
        for (index, part) in bytes.chunks(CHUNK_BYTES).enumerate() {
            let offset = index * CHUNK_BYTES;
            self.call(
                actor,
                Operation::Upload {
                    room: self.room,
                    hash: asset.hash,
                    total: asset.bytes,
                    offset: offset as u64,
                    bytes: part.to_vec(),
                    finish: offset + part.len() == bytes.len(),
                },
            );
        }
        asset
    }
    fn submit(&mut self, actor: u8) {
        let view = self.view(actor);
        let assignment = view.assignment.unwrap().assignment;
        let (width, height) = if self.mode == Mode::ExquisiteCorpse { (1024, 512) } else { (1536, 1024) };
        let mut document = Document::with_background(
            "Synthetic canonical remix",
            Size::new(width, height),
            ColorMode::Rgb,
            SampleType::U8,
            Color::rgb(f32::from(actor) / 5., f32::from(assignment.id.round + 1) / 5., 0.6),
        );
        // One deterministic noise patch forces the PNG across actual protocol chunk boundaries.
        if assignment.id.chain == 0 && assignment.id.round == 0 {
            let mut state = 0x12345678_u32;
            let mut patch = Vec::with_capacity(192 * 192 * 4);
            for _ in 0..192 * 192 {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                patch.extend_from_slice(&[state as u8, (state >> 8) as u8, (state >> 16) as u8, 255]);
            }
            document.layers[0].surface_mut().unwrap().write_interleaved(Rect::new(0, 0, 192, 192), &patch);
        }
        let native = photocraft_format::save_to_bytes(&document, &Default::default()).unwrap();
        let mut rendered = photocraft_compose::flatten(&document);
        photocraft_party::canonical_color::to_srgb(&document, &mut rendered.px).unwrap();
        let rendered = rendered.to_rgba8();
        let png = photocraft_sync::encode_png(width, height, &rendered.pixels).unwrap();
        self.pngs.insert((assignment.id.chain, assignment.id.round), png.clone());
        let checkpoint = CheckpointRef { id: CheckpointId::new(&hex(self.next + 10000)).unwrap(), asset: self.upload(actor, &native) };
        let image = self.upload(actor, &png);
        let seam = if self.mode == Mode::ExquisiteCorpse {
            let png = photocraft_sync::encode_png(width, 24, &rendered.pixels[((height - 24) * width * 4) as usize..]).unwrap();
            Some(self.upload(actor, &png))
        } else {
            None
        };
        let mut assets = vec![checkpoint.asset, image];
        assets.extend(seam);
        let submission = Submission {
            id: SubmissionId::new(&hex(self.next + 20000)).unwrap(),
            assignment: assignment.id,
            author: person(actor).id,
            project: assignment.project,
            revision: 1,
            image,
            checkpoint,
            replay: ReplayRef {
                id: ReplayId::new(&hex(self.next + 30000)).unwrap(),
                manifest: None,
                baseline: checkpoint,
                end_event: None,
                event_count: 0,
                duration_ms: 0,
            },
            assets,
            note: "private note never rendered or copied into provenance".into(),
            seam,
            extent: Some(photocraft_party::ImageExtent { width, height }),
        };
        self.action(actor, RoomAction::Submit { epoch: view.epoch, submission });
    }
    fn complete(&mut self) {
        for _ in 0..self.mode.rounds(usize::from(self.players)).unwrap() {
            for actor in 1..=self.players {
                self.submit(actor);
            }
        }
        assert_eq!(self.view(1).phase, Phase::Reveal);
    }
    fn source(&self, frame: Option<u16>) -> GameMediaSource {
        GameMediaSource {
            room: SourceId::new(self.room.as_str()).unwrap(),
            game: SourceId::new(self.game.as_str()).unwrap(),
            chain: 0,
            frame,
            fps: 6,
            looping: true,
        }
    }
    fn place(&mut self, descriptor: &PreparedMedia, sequence: u64, id: u64) -> WorldOp {
        let op = operation(1, 100, sequence, id, descriptor);
        world(&mut self.store, 1, Action::RemixGame { operation: op.clone(), room: SourceId::new(self.room.as_str()).unwrap() });
        op
    }
}

#[test]
fn terminal_animation_and_corpse_derivatives_match_canonical_pixels_and_metadata() {
    for (mode, players) in [(Mode::Animation { rotations: 1 }, 2), (Mode::ExquisiteCorpse, 3)] {
        let mut f = Fixture::new(mode, players);
        let source = f.source(None);
        denied(&mut f.store, 1, Action::PrepareGameMedia { canvas: canvas(100), source: source.clone() });
        f.complete();
        let expected_reveal = f.view(1).reveal;
        let media = prepared(&mut f.store, 1, 100, source.clone());
        assert_eq!(prepared(&mut f.store, 1, 100, source.clone()), media);
        assert_eq!(media.provenance.authors.iter().map(|credit| credit.profile).collect::<Vec<_>>(), (1..=players).map(owner).collect::<Vec<_>>());
        assert_eq!(media.provenance.authors[1].label, person(2).display_name);
        assert!(!serde_json::to_string(&media).unwrap().contains("private note"));
        assert_eq!(
            media.provenance.source,
            MediaSource::Gallery { entry: source.room, room: Some(source.room), game: Some(source.game), chain: 0, frame: None }
        );
        let expected = if mode == Mode::ExquisiteCorpse {
            assert!(!media.looping);
            assert_eq!(media.frames.len(), 1);
            let pixels: Vec<u8> = (0..3).flat_map(|round| image::load_from_memory(&f.pngs[&(0, round)]).unwrap().into_rgba8().into_raw()).collect();
            vec![photocraft_sync::encode_png(1024, 1536, &pixels).unwrap()]
        } else {
            assert!(media.looping);
            assert_eq!(media.frames.iter().map(|frame| frame.duration_ms).collect::<Vec<_>>(), vec![166, 167]);
            vec![f.pngs[&(0, 0)].clone(), f.pngs[&(0, 1)].clone()]
        };
        let proposed = operation(1, 100, 1, 5000, &media);
        denied(&mut f.store, 1, Action::Append { operation: proposed.clone() });
        let accepted = world(&mut f.store, 1, Action::RemixGame { operation: proposed.clone(), room: source.room });
        let retry = world(&mut f.store, 1, Action::RemixGame { operation: proposed.clone(), room: source.room });
        assert_eq!(serde_json::to_value(accepted).unwrap(), serde_json::to_value(retry).unwrap());
        assert_eq!(events(&mut f.store, 1, 100).len(), 1);
        for (frame, bytes) in media.frames.iter().zip(&expected) {
            assert_eq!(frame.byte_len, bytes.len() as u64);
            assert_eq!(frame.hash.as_str(), format!("{:x}", Sha256::digest(bytes)));
            assert_eq!(&fetch(&mut f.store, 1, 100, frame.hash), bytes);
            let image = image::load_from_memory(bytes).unwrap();
            assert_eq!((frame.width, frame.height), (image.width(), image.height()));
        }
        assert!(expected[0].len() > CHUNK_BYTES);
        let source_still = f.source(Some(1));
        let still = prepared(&mut f.store, 1, 100, source_still);
        assert_eq!(still.frames.len(), 1);
        assert!(!still.looping);
        assert_eq!(still.frames[0].duration_ms, 1000);
        f.place(&still, 2, 5001);
        assert_eq!(fetch(&mut f.store, 1, 100, still.frames[0].hash), f.pngs[&(0, 1)]);
        f.action(1, RoomAction::NextGame);
        assert_eq!(prepared(&mut f.store, 1, 100, source), media);
        assert_eq!(room(f.call(1, Operation::Game { room: f.room, game: f.game })).reveal, expected_reveal);
    }
}

#[test]
fn source_and_destination_grants_are_required_but_accepted_copy_is_independent() {
    let mut f = Fixture::new(Mode::Animation { rotations: 1 }, 2);
    f.complete();
    invite(&mut f.store, 1, 2, 100, 7000);
    invite(&mut f.store, 1, 3, 100, 7001);
    let source = f.source(None);
    denied(&mut f.store, 3, Action::PrepareGameMedia { canvas: canvas(100), source: source.clone() });
    create(&mut f.store, 2, 200);
    denied(&mut f.store, 1, Action::PrepareGameMedia { canvas: canvas(200), source: source.clone() });
    let media = prepared(&mut f.store, 1, 100, source.clone());
    let forged = operation(3, 100, 1, 5002, &media);
    denied(&mut f.store, 3, Action::RemixGame { operation: forged, room: source.room });
    let accepted = f.place(&media, 1, 5000);
    world(&mut f.store, 1, Action::Remove { canvas: canvas(100), member: owner(3) });
    invite(&mut f.store, 2, 3, 200, 7002);
    let copy = PreparedMedia {
        frames: media.frames.clone(),
        looping: media.looping,
        provenance: Provenance {
            source: MediaSource::Canvas { canvas: canvas(100), media: MediaId::new(&hex(5000)).unwrap() },
            source_revision: 1,
            artifact_hash: media.provenance.artifact_hash,
            authors: media.provenance.authors.clone(),
        },
    };
    let copied = operation(2, 200, 1, 6000, &copy);
    let copy_action = Action::RemixCanvas { operation: copied.clone(), source_canvas: canvas(100), source_media: MediaId::new(&hex(5000)).unwrap() };
    denied(
        &mut f.store,
        3,
        Action::RemixCanvas { operation: operation(3, 200, 1, 6001, &copy), source_canvas: canvas(100), source_media: MediaId::new(&hex(5000)).unwrap() },
    );
    denied(&mut f.store, 3, Action::FetchMedia { canvas: canvas(100), hash: media.frames[0].hash, offset: 0 });
    world(&mut f.store, 2, copy_action.clone());
    let before = events(&mut f.store, 2, 200);
    assert_eq!(before.len(), 1);
    let mut hide = accepted;
    hide.id = OpId::new(&hex(5003)).unwrap();
    hide.sequence = 2;
    hide.action = Edit::SetMediaVisible { media: MediaId::new(&hex(5000)).unwrap(), visible: false };
    world(&mut f.store, 1, Action::Append { operation: hide });
    world(&mut f.store, 1, Action::Remove { canvas: canvas(100), member: owner(2) });
    denied(&mut f.store, 2, copy_action);
    denied(&mut f.store, 2, Action::FetchMedia { canvas: canvas(100), hash: media.frames[0].hash, offset: 0 });
    assert_eq!(fetch(&mut f.store, 3, 200, media.frames[0].hash), f.pngs[&(0, 0)]);
    let path = f.directory.path().join("service.sqlite3");
    drop(f.store);
    let mut store = Store::open(&path, StoreOptions::default(), 100).unwrap();
    assert_eq!(events(&mut store, 2, 200), before);
    assert_eq!(fetch(&mut store, 3, 200, media.frames[0].hash), f.pngs[&(0, 0)]);
    world(&mut store, 2, Action::Remove { canvas: canvas(200), member: owner(3) });
    denied(&mut store, 3, Action::FetchMedia { canvas: canvas(200), hash: media.frames[0].hash, offset: 0 });
    let backup = f.directory.path().join("backup.sqlite3");
    store.backup(&backup).unwrap();
    let restored = f.directory.path().join("fresh.sqlite3");
    Store::copy_verified(&backup, &restored).unwrap();
    let mut restored = Store::open(restored, StoreOptions::default(), 100).unwrap();
    assert_eq!(fetch(&mut restored, 2, 200, media.frames[0].hash), f.pngs[&(0, 0)]);
    denied(&mut restored, 3, Action::FetchMedia { canvas: canvas(200), hash: media.frames[0].hash, offset: 0 });
}

#[test]
fn rejected_or_interrupted_remix_never_publishes_blobs_or_consumes_sequence() {
    let mut f = Fixture::new(Mode::Animation { rotations: 1 }, 2);
    f.complete();
    let source = f.source(None);
    let media = prepared(&mut f.store, 1, 100, source.clone());
    let mut bad = operation(1, 100, 1, 5000, &media);
    if let Edit::PlaceMedia { media } = &mut bad.action {
        media.provenance.authors[0].label = "Forged credit".into();
    }
    denied(&mut f.store, 1, Action::RemixGame { operation: bad, room: source.room });
    denied(&mut f.store, 1, Action::RemixGame { operation: operation(1, 100, 2, 5000, &media), room: source.room });
    let path = f.directory.path().join("service.sqlite3");
    let counts = || {
        let db = Connection::open(&path).unwrap();
        db.query_row("SELECT (SELECT count(*) FROM world_blobs),(SELECT count(*) FROM world_events)", [], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .unwrap()
    };
    assert_eq!(counts(), (0, 0));
    let operation = operation(1, 100, 1, 5000, &media);
    let action = request(Action::RemixGame { operation: operation.clone(), room: source.room });
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        f.store.world_execute_observed(&token(1), &action, 0, Limits::default(), |stage| {
            if stage == SaveStage::BeforeCommit {
                panic!("synthetic interruption after verified blobs")
            }
        })
    }));
    assert!(failed.is_err());
    assert_eq!(counts(), (0, 0));
    denied(&mut f.store, 1, Action::FetchMedia { canvas: canvas(100), hash: media.frames[0].hash, offset: 0 });
    world(&mut f.store, 1, action.action);
    assert_eq!(events(&mut f.store, 1, 100).len(), 1);
    assert!(counts().0 > 0);
    assert_eq!(fetch(&mut f.store, 1, 100, media.frames[0].hash), f.pngs[&(0, 0)]);
    for offset in [media.frames[0].byte_len, u64::MAX] {
        denied(&mut f.store, 1, Action::FetchMedia { canvas: canvas(100), hash: media.frames[0].hash, offset });
    }
}

#[test]
fn incomplete_terminal_game_has_transparent_gaps_but_missing_selected_still_is_refused() {
    let mut f = Fixture::new(Mode::Animation { rotations: 1 }, 2);
    f.submit(1);
    f.action(1, RoomAction::End { reason: "synthetic incomplete game".into() });
    let source = f.source(None);
    let media = prepared(&mut f.store, 1, 100, source);
    f.place(&media, 1, 5000);
    assert_eq!(media.frames.len(), 2);
    let blank = image::load_from_memory(&fetch(&mut f.store, 1, 100, media.frames[1].hash)).unwrap().into_rgba8();
    assert_eq!(blank.dimensions(), (1536, 1024));
    assert!(blank.as_raw().iter().all(|byte| *byte == 0));
    let missing = f.source(Some(1));
    denied(&mut f.store, 1, Action::PrepareGameMedia { canvas: canvas(100), source: missing });
}

#[test]
fn world_v1_migration_keeps_a_verified_backup_and_all_existing_grants_events() {
    let mut f = Fixture::new(Mode::Animation { rotations: 1 }, 2);
    invite(&mut f.store, 1, 2, 100, 7000);
    let op = WorldOp {
        id: OpId::new(&hex(5000)).unwrap(),
        canvas: canvas(100),
        author: owner(1),
        device: DeviceId::new(person(1).device.as_str()).unwrap(),
        sequence: 1,
        authority_epoch: 1,
        action: Edit::Rename { name: "Retained label".into(), expected_revision: 0 },
    };
    world(&mut f.store, 1, Action::Append { operation: op });
    let before = events(&mut f.store, 2, 100);
    let Response::Canvas { info: pre_info } = world(&mut f.store, 1, Action::Info { canvas: canvas(100) }) else { panic!("info") };
    let mut legacy_world = photocraft_scene::shared::WorldState::new(canvas(100), owner(1), pre_info.initial_name).unwrap();
    for ev in &before {
        legacy_world.apply(ev).unwrap();
    }
    let legacy_bytes = serde_json::to_vec(&legacy_world).unwrap();
    let legacy_hash = format!("{:x}", Sha256::digest(&legacy_bytes));
    let path = f.directory.path().join("service.sqlite3");
    drop(f.store);
    let db = Connection::open(&path).unwrap();
    db.execute("UPDATE worlds SET state=?1,hash=?2 WHERE id=?3", rusqlite::params![&legacy_bytes, &legacy_hash, canvas(100).as_str()]).unwrap();
    db.execute_batch("DROP TABLE world_checkpoints; DROP TABLE world_tours_idx; DROP TABLE world_media_idx; DROP TABLE world_strokes_idx; DROP TABLE world_devices; DROP TABLE world_meta; DROP TABLE world_import_assets; DROP TABLE world_imports; DROP TABLE world_import_receipts; DROP TABLE world_import_origins; DROP TABLE world_trash; DROP TABLE world_blobs; DROP TABLE world_media_sources; DROP TABLE world_schema; CREATE TABLE world_schema(version INTEGER NOT NULL CHECK(version=1)); INSERT INTO world_schema VALUES(1); PRAGMA user_version=3;").unwrap();
    drop(db);
    let mut migrated = Store::open(&path, StoreOptions::default(), 100).unwrap();
    assert_eq!(events(&mut migrated, 2, 100), before);
    let Response::Canvas { info } = world(&mut migrated, 2, Action::Info { canvas: canvas(100) }) else { panic!("canvas") };
    assert_eq!(info.name, "Retained label");
    assert_eq!(info.epoch, 1);
    let snapshots: Vec<_> = std::fs::read_dir(f.directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|entry| entry.file_name().unwrap().to_string_lossy().contains("before-world-v2-"))
        .collect();
    assert_eq!(snapshots.len(), 1);
    let backup = Connection::open(&snapshots[0]).unwrap();
    assert_eq!(backup.query_row::<u32, _, _>("SELECT version FROM world_schema", [], |row| row.get(0)).unwrap(), 1);
    assert_eq!(backup.query_row::<i64, _, _>("SELECT count(*) FROM world_events", [], |row| row.get(0)).unwrap(), 1);
    assert_eq!(backup.query_row::<i64, _, _>("SELECT count(*) FROM world_members WHERE active=1", [], |row| row.get(0)).unwrap(), 2);
    drop(backup);
    let saved = std::fs::read(&snapshots[0]).unwrap();
    let recovered = f.directory.path().join("fresh.sqlite3");
    Store::copy_verified(&snapshots[0], &recovered).unwrap();
    let mut restored = Store::open(&recovered, StoreOptions::default(), 200).unwrap();
    assert_eq!(events(&mut restored, 2, 100), before);
    assert_eq!(std::fs::read(&snapshots[0]).unwrap(), saved);
    drop(migrated);
    let reopened = Store::open(&path, StoreOptions::default(), 200).unwrap();
    assert_eq!(reopened.identity_authenticate(&token(2), 200).unwrap().profile, person(2).id);
    assert_eq!(
        std::fs::read_dir(f.directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("service.sqlite3.before-world-v2-"))
            .count(),
        1
    );
}

#[test]
fn variable_height_corpse_remix_assembles_exact_summed_dimensions_and_single_part() {
    let mut f = Fixture::new(Mode::ExquisiteCorpse, 2);
    let heights = [256u32, 768, 1280];
    let colors = [Color::rgb(0.8, 0.2, 0.2), Color::rgb(0.2, 0.8, 0.2), Color::rgb(0.2, 0.2, 0.8)];
    for round in 0..3usize {
        for actor in 1..=2u8 {
            let view = f.view(actor);
            let assignment = view.assignment.unwrap().assignment;
            let height = if assignment.id.chain == 0 { heights[round] } else { 512 };
            let document = Document::with_background("Variable remix", Size::new(1024, height), ColorMode::Rgb, SampleType::U8, colors[round]);
            let native = photocraft_format::save_to_bytes(&document, &Default::default()).unwrap();
            let mut rendered = photocraft_compose::flatten(&document);
            photocraft_party::canonical_color::to_srgb(&document, &mut rendered.px).unwrap();
            let rendered = rendered.to_rgba8();
            let png = photocraft_sync::encode_png(1024, height, &rendered.pixels).unwrap();
            let checkpoint = CheckpointRef { id: CheckpointId::new(&hex(f.next + 10000)).unwrap(), asset: f.upload(actor, &native) };
            let image = f.upload(actor, &png);
            let seam_png = photocraft_sync::encode_png(1024, 24, &rendered.pixels[((height - 24) * 1024 * 4) as usize..]).unwrap();
            let seam = f.upload(actor, &seam_png);
            let assets = vec![checkpoint.asset, image, seam];
            let submission = Submission {
                id: SubmissionId::new(&hex(f.next + 20000)).unwrap(),
                assignment: assignment.id,
                author: person(actor).id,
                project: assignment.project,
                revision: 1,
                image,
                checkpoint,
                replay: ReplayRef {
                    id: ReplayId::new(&hex(f.next + 30000)).unwrap(),
                    manifest: None,
                    baseline: checkpoint,
                    end_event: None,
                    event_count: 0,
                    duration_ms: 0,
                },
                assets,
                note: String::new(),
                seam: Some(seam),
                extent: Some(photocraft_party::ImageExtent { width: 1024, height }),
            };
            f.action(actor, RoomAction::Submit { epoch: view.epoch, submission });
        }
    }
    let whole_source = f.source(None);
    let whole = prepared(&mut f.store, 1, 100, whole_source);
    f.place(&whole, 1, 5000);
    assert_eq!(whole.frames.len(), 1);
    let assembled = image::load_from_memory(&fetch(&mut f.store, 1, 100, whole.frames[0].hash)).unwrap().into_rgba8();
    assert_eq!(assembled.dimensions(), (1024, 256 + 768 + 1280));
    assert!(assembled.get_pixel(10, 0)[0] > 180 && assembled.get_pixel(10, 255)[0] > 180);
    assert!(assembled.get_pixel(10, 256)[1] > 180 && assembled.get_pixel(10, 256 + 767)[1] > 180);
    assert!(assembled.get_pixel(10, 256 + 768)[2] > 180 && assembled.get_pixel(10, 256 + 768 + 1279)[2] > 180);
    let part_source = f.source(Some(1));
    let part = prepared(&mut f.store, 1, 100, part_source);
    f.place(&part, 2, 5001);
    assert_eq!(part.frames.len(), 1);
    let middle = image::load_from_memory(&fetch(&mut f.store, 1, 100, part.frames[0].hash)).unwrap().into_rgba8();
    assert_eq!(middle.dimensions(), (1024, 768));
}
