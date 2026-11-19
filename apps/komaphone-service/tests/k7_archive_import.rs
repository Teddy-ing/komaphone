//! Portable import needs no source service: synthetic history is never authority.
use image::ImageEncoder;
use photocraft_party::{
    Participant, ParticipantId, RequestId,
    protocol::{Operation, RpcRequest},
};
use photocraft_scene::shared::archive_import::{ArchiveManifest, MAX_CHUNK_BYTES};
use photocraft_scene::shared::media::{AssetHash, Credit, MediaFrame, MediaPlacement, MediaSource, Provenance};
use photocraft_scene::shared::{AcceptedOp, DeviceId, Edit, MediaId, OpId, SourceId, Tour, TourId, TourStop, WorldOp, WorldState};
use photocraft_scene::world_protocol::{Action, ImportObject, Request, Response, VERSION};
use photocraft_scene::{CanvasId, OwnerId, RegionKey, Srgba, Stroke, StrokeId, coordinates};
use photocraft_sync::{SaveStage, Store, StoreOptions, worlds::Limits};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

fn hex(n: u64) -> String {
    format!("{n:032x}")
}
fn owner(n: u64) -> OwnerId {
    OwnerId::new(&hex(n)).unwrap()
}
fn canvas(n: u64) -> CanvasId {
    CanvasId::new(&hex(n)).unwrap()
}
fn id(n: u64) -> OpId {
    OpId::new(&hex(n)).unwrap()
}
fn device(n: u64) -> DeviceId {
    DeviceId::new(&hex(n + 1000)).unwrap()
}
fn token(n: u64) -> String {
    format!("{:064x}", n + 10000)
}
fn hash(bytes: &[u8]) -> AssetHash {
    AssetHash::new(&format!("{:x}", Sha256::digest(bytes))).unwrap()
}
fn request(action: Action) -> Request {
    Request { version: VERSION, action }
}
fn op(actor: u64, seq: u64, target: CanvasId, action: Edit) -> WorldOp {
    WorldOp { id: id(actor * 100 + seq), canvas: target, author: owner(actor), device: device(actor), sequence: seq, authority_epoch: 1, action }
}
fn stroke(actor: u64, target: CanvasId) -> Stroke {
    Stroke {
        id: StrokeId::new(&hex(actor)).unwrap(),
        canvas: target,
        owner: owner(actor),
        revision: 0,
        anchor: RegionKey::default(),
        points: vec![[1., 2.], [5., 8.]],
        width: 4.,
        color: Srgba { rgba: [0.3, 0.4, 0.6, 1.] },
        erase: false,
        brush: None,
        samples: None,
    }
}
fn fixture_archive() -> (ArchiveManifest, BTreeMap<AssetHash, Vec<u8>>) {
    let target = canvas(900);
    let mut state = WorldState::new(target, owner(99), "Foreign archive".into()).unwrap();
    for (revision, actor) in [(1, 99), (2, 100)] {
        state.apply(&AcceptedOp { revision, op: op(actor, 1, target, Edit::AddStroke { stroke: stroke(actor, target) }) }).unwrap();
    }
    let mut pixels = vec![0u8; 64 * 64 * 4];
    let mut seed = 17_u32;
    for byte in &mut pixels {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        *byte = (seed >> 24) as u8;
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png).write_image(&pixels, 64, 64, image::ExtendedColorType::Rgba8).unwrap();
    let frame = MediaFrame { hash: hash(&png), byte_len: png.len() as u64, width: 64, height: 64, duration_ms: 200 };
    let placement = MediaPlacement {
        id: MediaId::new(&hex(901)).unwrap(),
        canvas: target,
        owner: owner(99),
        anchor: RegionKey::default(),
        offset: [1., 1.],
        width: 64.,
        height: 64.,
        frames: vec![frame.clone(), MediaFrame { duration_ms: 400, ..frame.clone() }],
        looping: true,
        provenance: Provenance {
            source: MediaSource::Gallery { entry: SourceId::new(&hex(902)).unwrap(), room: None, game: None, chain: 0, frame: None },
            source_revision: 1,
            artifact_hash: hash(b"foreign art"),
            authors: vec![Credit { profile: owner(99), label: "Original artist".into() }],
        },
    };
    state.apply(&AcceptedOp { revision: 3, op: op(99, 2, target, Edit::PlaceMedia { media: Box::new(placement) }) }).unwrap();
    let tour = Tour {
        id: TourId::new(&hex(903)).unwrap(),
        canvas: target,
        name: "Original route".into(),
        revision: 0,
        stops: vec![TourStop { camera: coordinates::home(target), travel_ms: 0, pause_ms: 1000, target: None }],
    };
    state.apply(&AcceptedOp { revision: 4, op: op(99, 3, target, Edit::PutTour { tour, expected_revision: 0 }) }).unwrap();
    let manifest = ArchiveManifest { schema: 1, world: state, assets: vec![frame] };
    manifest.validate().unwrap();
    (manifest, BTreeMap::from([(hash(&png), png)]))
}
struct Fixture {
    root: tempfile::TempDir,
    store: Store,
    manifest: ArchiveManifest,
    bytes: Vec<u8>,
    assets: BTreeMap<AssetHash, Vec<u8>>,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path().join("service.sqlite3"), StoreOptions::default(), 0).unwrap();
        for n in [1, 2] {
            store
                .execute(
                    None,
                    &RpcRequest {
                        protocol_version: 1,
                        request_id: RequestId::new(&hex(n)).unwrap(),
                        operation: Operation::Pair {
                            participant: Participant {
                                id: ParticipantId::new(&hex(n)).unwrap(),
                                device: photocraft_party::DeviceId::new(device(n).as_str()).unwrap(),
                                display_name: format!("Importer {n}"),
                            },
                            credential: token(n),
                        },
                    },
                    0,
                )
                .unwrap();
        }
        let (manifest, assets) = fixture_archive();
        let bytes = serde_json::to_vec(&manifest).unwrap();
        Self { root, store, manifest, bytes, assets }
    }
    fn call(&mut self, actor: u64, action: Action) -> photocraft_sync::Result<Response> {
        self.store.world_execute(&token(actor), &request(action), 1)
    }
    fn begin(&self, n: u64) -> Action {
        Action::ImportBegin {
            id: id(n),
            new_canvas: canvas(n),
            manifest_sha256: hash(&self.bytes),
            manifest_bytes: self.bytes.len() as u64,
            total_bytes: self.bytes.len() as u64 + self.assets.values().map(|b| b.len() as u64).sum::<u64>(),
        }
    }
    fn stage(&mut self, n: u64) {
        let begin = self.begin(n);
        self.call(1, begin).unwrap();
        let bytes = self.bytes.clone();
        for (index, chunk) in bytes.chunks(MAX_CHUNK_BYTES).enumerate() {
            self.call(1, Action::ImportChunk { id: id(n), object: ImportObject::Manifest {}, offset: (index * MAX_CHUNK_BYTES) as u64, bytes: chunk.to_vec() })
                .unwrap();
        }
        for (hash, bytes) in self.assets.clone() {
            for (index, chunk) in bytes.chunks(MAX_CHUNK_BYTES).enumerate() {
                self.call(
                    1,
                    Action::ImportChunk { id: id(n), object: ImportObject::Asset { hash }, offset: (index * MAX_CHUNK_BYTES) as u64, bytes: chunk.to_vec() },
                )
                .unwrap();
            }
        }
    }
    fn read(&mut self, n: u64) -> WorldState {
        let Response::Canvas { info } = self.call(1, Action::Info { canvas: canvas(n) }).unwrap() else { panic!() };
        let mut world = WorldState::with_import_origin(info.canvas, info.owner, info.initial_name, info.import_origin).unwrap();
        let Response::Events { events, .. } = self.call(1, Action::Events { canvas: canvas(n), after: 0, through: None, limit: 128 }).unwrap() else {
            panic!()
        };
        for event in events {
            world.apply(&event).unwrap();
        }
        world.validate().unwrap();
        world
    }
}

#[test]
fn fresh_authority_import_preserves_history_assets_tours_but_grants_only_importer() {
    let mut f = Fixture::new();
    let source = f.manifest.world.clone();
    assert!(f.call(1, Action::Info { canvas: source.canvas }).is_err());
    f.stage(300);
    f.call(1, Action::ImportCommit { id: id(300) }).unwrap();
    let imported = f.read(300);
    assert_eq!(imported, source.import_to(canvas(300), owner(1)).unwrap());
    assert_eq!(imported.owner, owner(1));
    assert_eq!(imported.strokes.len(), 2);
    assert_eq!(imported.tours.len(), 1);
    assert_eq!(imported.media.values().next().unwrap().media.frames.iter().map(|f| f.duration_ms).collect::<Vec<_>>(), vec![200, 400]);
    let Response::Members { items, .. } = f.call(1, Action::Members { canvas: canvas(300), after: None, limit: 128 }).unwrap() else { panic!() };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].profile, owner(1));
    assert!(f.call(2, Action::Info { canvas: canvas(300) }).is_err());
    for (hash, bytes) in f.assets.clone() {
        let Response::MediaChunk { bytes: got, .. } = f.call(1, Action::FetchMedia { canvas: canvas(300), hash, offset: 0 }).unwrap() else { panic!() };
        assert_eq!(got, bytes);
        assert!(f.call(2, Action::FetchMedia { canvas: canvas(300), hash, offset: 0 }).is_err());
    }
    f.call(1, Action::Append { operation: op(1, 1, canvas(300), Edit::Rename { name: "New independent name".into(), expected_revision: 0 }) }).unwrap();
    let after = f.read(300);
    assert_eq!(after.name, "New independent name");
    let begin = f.begin(300);
    assert!(matches!(f.call(1, begin).unwrap(), Response::Canvas { .. }));
    f.call(1, Action::ImportCommit { id: id(300) }).unwrap();
    assert_eq!(f.read(300), after);
    assert_eq!(source, f.manifest.world);
    assert!(f.call(1, Action::Info { canvas: source.canvas }).is_err());
    let mut historical = imported.events_after(0, 1).unwrap()[0].op.clone();
    historical.author = owner(1); // Even an attempted author rewrite confers no authenticated device.
    assert!(f.call(1, Action::Append { operation: historical }).is_err());
    let snapshot = f.root.path().join("backup.sqlite3");
    f.store.backup(&snapshot).unwrap();
    let restored = f.root.path().join("restored.sqlite3");
    Store::copy_verified(&snapshot, &restored).unwrap();
    let reopened = Store::open(restored, StoreOptions::default(), 100).unwrap();
    drop(reopened);
}

#[test]
fn restart_retry_account_isolation_and_interrupted_commit_keep_originals() {
    let mut f = Fixture::new();
    let begin = f.begin(301);
    f.call(1, begin.clone()).unwrap();
    let first = f.bytes.get(..100).unwrap().to_vec();
    let chunk = Action::ImportChunk { id: id(301), object: ImportObject::Manifest {}, offset: 0, bytes: first.clone() };
    f.call(1, chunk.clone()).unwrap();
    f.call(1, chunk.clone()).unwrap();
    assert!(f.call(2, chunk).is_err());
    assert!(f.call(2, Action::ImportCancel { id: id(301) }).is_err());
    let mut bad = first;
    bad[0] ^= 1;
    assert!(f.call(1, Action::ImportChunk { id: id(301), object: ImportObject::Manifest {}, offset: 0, bytes: bad }).is_err());
    let root = f.root.path().to_path_buf();
    drop(f.store);
    f.store = Store::open(root.join("service.sqlite3"), StoreOptions::default(), 100).unwrap();
    let Response::ImportProgress { received, .. } = f.call(1, begin).unwrap() else { panic!() };
    assert_eq!(received, 100);
    let rest = f.bytes.get(100..).unwrap().to_vec();
    f.call(1, Action::ImportChunk { id: id(301), object: ImportObject::Manifest {}, offset: 100, bytes: rest }).unwrap();
    assert!(f.call(1, Action::ImportCommit { id: id(301) }).is_err()); // Image still absent.
    for (hash, bytes) in f.assets.clone() {
        f.call(1, Action::ImportChunk { id: id(301), object: ImportObject::Asset { hash }, offset: 0, bytes }).unwrap();
    }
    let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        f.store.world_execute_observed(&token(1), &request(Action::ImportCommit { id: id(301) }), 1, Limits::default(), |stage| {
            if stage == SaveStage::BeforeCommit {
                panic!("synthetic interruption");
            }
        })
    }));
    assert!(interrupted.is_err());
    assert!(f.call(1, Action::Info { canvas: canvas(301) }).is_err());
    f.call(1, Action::ImportCommit { id: id(301) }).unwrap();
    assert_eq!(f.read(301).revision, 4);
    let db = rusqlite::Connection::open(root.join("service.sqlite3")).unwrap();
    assert_eq!(db.query_row("SELECT count(*) FROM world_imports", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(db.query_row("SELECT count(*) FROM world_import_assets", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(db.query_row("SELECT count(*) FROM world_import_receipts", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
}

#[test]
fn corrupt_chunks_undeclared_assets_expiry_cancel_and_bounds_never_publish() {
    let mut f = Fixture::new();
    let begin = f.begin(302);
    f.call(1, begin.clone()).unwrap();
    assert!(f.call(1, Action::ImportChunk { id: id(302), object: ImportObject::Asset { hash: hash(b"unknown") }, offset: 0, bytes: vec![1] }).is_err());
    assert!(f.call(1, Action::ImportChunk { id: id(302), object: ImportObject::Manifest {}, offset: u64::MAX, bytes: vec![1] }).is_err());
    assert!(f.call(1, Action::ImportChunk { id: id(302), object: ImportObject::Manifest {}, offset: 0, bytes: vec![1; MAX_CHUNK_BYTES + 1] }).is_err());
    let mut wrong = f.bytes.clone();
    wrong[0] ^= 1;
    assert!(f.call(1, Action::ImportChunk { id: id(302), object: ImportObject::Manifest {}, offset: 0, bytes: wrong }).is_err());
    let Response::ImportProgress { received, .. } = f.call(1, begin.clone()).unwrap() else { panic!() };
    assert_eq!(received, 0);
    f.call(1, Action::ImportCancel { id: id(302) }).unwrap();
    f.call(1, Action::ImportCancel { id: id(302) }).unwrap();
    assert!(f.call(1, Action::ImportCommit { id: id(302) }).is_err());
    f.call(1, begin.clone()).unwrap();
    assert!(f.store.world_execute(&token(1), &request(Action::ImportCommit { id: id(302) }), 24 * 60 * 60 * 1000 + 10).is_err());
    f.store.world_execute(&token(1), &request(begin), 24 * 60 * 60 * 1000 + 10).unwrap();
    assert!(f.call(1, Action::Info { canvas: canvas(302) }).is_err());
}

#[test]
fn actual_quota_failure_keeps_staged_content_and_commit_can_retry_with_headroom() {
    let mut f = Fixture::new();
    f.stage(310);
    let expected = f.manifest.world.import_to(canvas(310), owner(1)).unwrap();
    let path = f.root.path().join("service.sqlite3");
    drop(f.store);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("VACUUM").unwrap(); // Remove free pages before measuring failure headroom.
    let pages: i64 = db.pragma_query_value(None, "page_count", |r| r.get(0)).unwrap();
    let page_size: i64 = db.pragma_query_value(None, "page_size", |r| r.get(0)).unwrap();
    drop(db);
    f.store = Store::open(&path, StoreOptions { quota_bytes: ((pages + 1) * page_size) as u64, ..Default::default() }, 100).unwrap();
    let failure = f.call(1, Action::ImportCommit { id: id(310) });
    assert!(matches!(failure, Err(photocraft_sync::Error::Full)), "{failure:?}");
    assert!(f.call(1, Action::Info { canvas: canvas(310) }).is_err());
    drop(f.store);
    f.store = Store::open(&path, StoreOptions::default(), 200).unwrap();
    f.call(1, Action::ImportCommit { id: id(310) }).unwrap();
    assert_eq!(f.read(310), expected);
}

#[test]
fn corrupt_image_refusal_and_staging_capacity_preserve_verified_manifest() {
    let mut f = Fixture::new();
    let begin = f.begin(311);
    f.call(1, begin.clone()).unwrap();
    f.call(1, Action::ImportChunk { id: id(311), object: ImportObject::Manifest {}, offset: 0, bytes: f.bytes.clone() }).unwrap();
    let (&hash, bytes) = f.assets.iter().next().unwrap();
    let mut wrong = bytes.clone();
    wrong[0] ^= 1;
    assert!(f.call(1, Action::ImportChunk { id: id(311), object: ImportObject::Asset { hash }, offset: 0, bytes: wrong }).is_err());
    let other = f.begin(312);
    f.call(1, other).unwrap();
    let excess = f.begin(313);
    assert!(f.call(1, excess).is_err());
    let Response::ImportProgress { received, total, .. } = f.call(1, begin).unwrap() else { panic!() };
    assert_eq!(received, total);
    let asset = f.assets.get(&hash).unwrap().clone();
    f.call(1, Action::ImportChunk { id: id(311), object: ImportObject::Asset { hash }, offset: 0, bytes: asset }).unwrap();
    f.call(1, Action::ImportCommit { id: id(311) }).unwrap();
}

#[test]
fn schema_three_migration_retains_verified_backup_and_existing_world() {
    let mut f = Fixture::new();
    f.call(1, Action::Create { canvas: canvas(320), name: "Prior world".into() }).unwrap();
    f.call(1, Action::Append { operation: op(1, 1, canvas(320), Edit::AddStroke { stroke: stroke(1, canvas(320)) }) }).unwrap();
    let expected = f.read(320);
    let path = f.root.path().join("service.sqlite3");
    drop(f.store);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE world_import_assets; DROP TABLE world_imports; DROP TABLE world_import_receipts; DROP TABLE world_import_origins; DROP TABLE world_schema; CREATE TABLE world_schema(version INTEGER NOT NULL CHECK(version=3)); INSERT INTO world_schema VALUES(3); PRAGMA user_version=3;").unwrap();
    drop(db);
    f.store = Store::open(&path, StoreOptions::default(), 200).unwrap();
    assert_eq!(f.read(320), expected);
    let backup = std::fs::read_dir(f.root.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.file_name().unwrap().to_string_lossy().contains("before-world-v4"))
        .unwrap();
    // SQLite's backup API may rewrite file-header bookkeeping. Verify schema
    // and content, rather than requiring identical physical page bytes.
    let old = rusqlite::Connection::open(&backup).unwrap();
    assert_eq!(old.query_row("SELECT version FROM world_schema", [], |r| r.get::<_, i64>(0)).unwrap(), 3);
    let body: Vec<u8> = old.query_row("SELECT state FROM worlds WHERE id=?1", [canvas(320).as_str()], |r| r.get(0)).unwrap();
    assert_eq!(serde_json::from_slice::<WorldState>(&body).unwrap(), expected);
    assert_eq!(old.query_row("SELECT count(*) FROM sqlite_master WHERE name='world_imports'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    drop(old);
    let before = hash(&std::fs::read(&backup).unwrap());
    let restored = f.root.path().join("old-copy.sqlite3");
    Store::copy_verified(&backup, &restored).unwrap();
    let recovered = Store::open(&restored, StoreOptions::default(), 300).unwrap();
    drop(recovered);
    assert_eq!(hash(&std::fs::read(&backup).unwrap()), before);
    let db = rusqlite::Connection::open(&path).unwrap();
    let version: i64 = db.query_row("SELECT version FROM world_schema", [], |r| r.get(0)).unwrap();
    assert_eq!(version, 5);
    let user_version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
    assert_eq!(user_version, 5);
    assert!(std::fs::read_dir(f.root.path()).unwrap().map(|e| e.unwrap().path()).any(|p| p.file_name().unwrap().to_string_lossy().contains("before-world-v5")));
}

#[test]
fn real_restore_quarantines_incomplete_import_but_preserves_accepted_copy_and_receipt() {
    let mut f = Fixture::new();
    f.stage(330);
    f.call(1, Action::ImportCommit { id: id(330) }).unwrap();
    let expected = f.read(330);
    let pending = f.begin(331);
    f.call(1, pending).unwrap();
    f.call(1, Action::ImportChunk { id: id(331), object: ImportObject::Manifest {}, offset: 0, bytes: f.bytes.get(..100).unwrap().to_vec() }).unwrap();
    let backup = f.root.path().join("snapshot.sqlite3");
    f.store.backup(&backup).unwrap();
    let before = std::fs::read(&backup).unwrap();
    let restored = f.root.path().join("restored-cli");
    let output =
        std::process::Command::new(env!("CARGO_BIN_EXE_photocraft-sync")).arg("--data-dir").arg(&restored).arg("--restore-from").arg(&backup).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(std::fs::read(&backup).unwrap(), before);
    let db = rusqlite::Connection::open(restored.join("service.sqlite3")).unwrap();
    for table in ["world_imports", "world_import_assets"] {
        assert_eq!(db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }
    assert_eq!(db.query_row("SELECT count(*) FROM world_import_receipts", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    assert_eq!(db.query_row("SELECT count(*) FROM hosted_profiles WHERE revoked=0", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    let bytes: Vec<u8> = db.query_row("SELECT state FROM worlds WHERE id=?1", [canvas(330).as_str()], |r| r.get(0)).unwrap();
    assert_eq!(serde_json::from_slice::<WorldState>(&bytes).unwrap(), expected);
}

#[test]
fn imported_stroke_copy_uses_original_credit_without_a_live_source_profile() {
    let mut f = Fixture::new();
    f.stage(340);
    f.call(1, Action::ImportCommit { id: id(340) }).unwrap();
    let imported = f.read(340);
    let source = imported.strokes.values().next().unwrap();
    let credit = imported.imported_credit(source.stroke.owner).unwrap();
    assert!([owner(99), owner(100)].contains(&credit.profile));
    let copy_id = StrokeId::new(&hex(9999)).unwrap();
    let action = imported
        .copy_stroke(
            source.stroke.id,
            credit.clone(),
            photocraft_scene::shared::StrokeCopyTarget {
                canvas: canvas(340),
                id: copy_id,
                owner: owner(1),
                anchor: RegionKey::default(),
                offset: [30., 40.],
                scale: 1.,
            },
        )
        .unwrap();
    f.call(1, Action::RemixStroke { operation: op(1, 1, canvas(340), action) }).unwrap();
    let current = f.read(340);
    let copied = current.strokes.get(&copy_id).unwrap();
    assert_eq!(copied.stroke.owner, owner(1));
    assert_eq!(copied.provenance.as_ref().unwrap().author, credit);
}

#[test]
fn another_authenticated_device_of_same_profile_cannot_resume_import() {
    let mut f = Fixture::new();
    let begin = f.begin(350);
    f.call(1, begin.clone()).unwrap();
    let db = rusqlite::Connection::open(f.root.path().join("service.sqlite3")).unwrap();
    let verifier = format!("{:x}", Sha256::digest(token(4).as_bytes()));
    db.execute("INSERT INTO identity_devices(device,profile,verifier) VALUES(?1,?2,?3)", rusqlite::params![device(4).as_str(), owner(1).as_str(), verifier])
        .unwrap();
    db.execute("INSERT INTO identity_sessions(verifier,device,expires) VALUES(?1,?2,10000)", rusqlite::params![verifier, device(4).as_str()]).unwrap();
    drop(db);
    assert!(f.call(4, begin).is_err());
    assert!(f.call(4, Action::ImportCancel { id: id(350) }).is_err());
    f.call(1, Action::ImportCancel { id: id(350) }).unwrap();
}

#[test]
fn corrupt_staging_origins_and_receipts_are_refused_without_replacing_data() {
    for damage in ["manifest", "asset", "origin", "receipt-owner"] {
        let mut f = Fixture::new();
        f.stage(360);
        if matches!(damage, "origin" | "receipt-owner") {
            f.call(1, Action::ImportCommit { id: id(360) }).unwrap();
        }
        let path = f.root.path().join("service.sqlite3");
        drop(f.store);
        let db = rusqlite::Connection::open(&path).unwrap();
        match damage {
            "manifest" => {
                db.execute("UPDATE world_imports SET data=zeroblob(manifest_bytes)", []).unwrap();
            }
            "asset" => {
                db.execute("UPDATE world_import_assets SET received=total+1", []).unwrap();
            }
            "origin" => {
                db.execute("UPDATE world_import_origins SET hash=?1", ["f".repeat(64)]).unwrap();
            }
            _ => {
                db.execute("UPDATE world_import_receipts SET profile=?1,device=?2", rusqlite::params![owner(2).as_str(), device(2).as_str()]).unwrap();
            }
        }
        drop(db);
        let before = hash(&std::fs::read(&path).unwrap());
        assert!(Store::open(&path, StoreOptions::default(), 200).is_err(), "{damage}");
        assert_eq!(hash(&std::fs::read(&path).unwrap()), before, "{damage}");
        assert!(Store::copy_verified(&path, f.root.path().join("refused-backup.sqlite3")).is_err());
        assert!(!f.root.path().join("refused-backup.sqlite3").exists());
    }
}

#[test]
fn self_consistent_partial_import_prefix_is_never_published_or_accepted_on_disk() {
    let mut f = Fixture::new();
    f.stage(370);
    f.call(1, Action::ImportCommit { id: id(370) }).unwrap();
    let complete = f.read(370);
    let partial = complete.replay_to(complete.revision - 1).unwrap();
    // Partial worlds are legitimate during client replay, but never canonical storage.
    partial.validate().unwrap();
    assert!(partial.revision < partial.import_origin.as_ref().unwrap().source_revision);
    let path = f.root.path().join("service.sqlite3");
    let db = rusqlite::Connection::open(&path).unwrap();
    let body = serde_json::to_vec(&partial).unwrap();
    db.execute(
        "UPDATE worlds SET state=?1,hash=?2,revision=?3 WHERE id=?4",
        rusqlite::params![body, hash(&body).as_str(), i64::try_from(partial.revision).unwrap(), partial.canvas.as_str()],
    )
    .unwrap();
    db.execute(
        "DELETE FROM world_events WHERE canvas=?1 AND revision>?2",
        rusqlite::params![partial.canvas.as_str(), i64::try_from(partial.revision).unwrap()],
    )
    .unwrap();
    drop(db);
    assert!(f.call(1, Action::Info { canvas: canvas(370) }).is_err());
    drop(f.store);
    let before = hash(&std::fs::read(&path).unwrap());
    assert!(Store::open(&path, StoreOptions::default(), 200).is_err());
    assert_eq!(hash(&std::fs::read(&path).unwrap()), before);
    assert!(Store::copy_verified(&path, f.root.path().join("partial-backup.sqlite3")).is_err());
    assert!(!f.root.path().join("partial-backup.sqlite3").exists());
}
