//! Historical game grants must survive room reuse without admitting the next roster.
use photocraft_party::{
    AssetHash, AssetRef, CheckpointId, CheckpointRef, DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, ReplayId, ReplayRef, RequestId, RoomId,
    Submission, SubmissionId,
    protocol::{Operation, RpcRequest, RpcResponse},
    room::{Action, RoomView},
};
use photocraft_sync::{Store, StoreOptions};
use sha2::{Digest, Sha256};

fn person(n: u8) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{n:032x}")).unwrap(),
        device: DeviceId::new(&format!("{:032x}", n + 10)).unwrap(),
        display_name: format!("Player {n}"),
    }
}
fn token(n: u8) -> String {
    format!("{n:064x}")
}
fn game(n: u64) -> GameId {
    GameId::new(&format!("{n:032x}")).unwrap()
}
fn view(response: RpcResponse) -> RoomView {
    match response {
        RpcResponse::Room { view, .. } | RpcResponse::Game { view } => *view,
        other => panic!("room: {other:?}"),
    }
}
struct Fixture {
    directory: tempfile::TempDir,
    store: Store,
    room: RoomId,
    request: u64,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(directory.path().join("service.sqlite3"), StoreOptions::default(), 0).unwrap();
        for n in 1..=4 {
            store
                .execute(
                    None,
                    &RpcRequest {
                        protocol_version: photocraft_party::protocol::PROTOCOL_VERSION,
                        request_id: RequestId::new(&format!("{n:032x}")).unwrap(),
                        operation: Operation::Pair { participant: person(n), credential: token(n) },
                    },
                    0,
                )
                .unwrap();
        }
        let room = view(
            store
                .execute(
                    Some(&token(1)),
                    &RpcRequest {
                        protocol_version: photocraft_party::protocol::PROTOCOL_VERSION,
                        request_id: RequestId::new(&format!("{:032x}", 10)).unwrap(),
                        operation: Operation::Create { mode: Mode::Animation { rotations: 1 }, timer_ms: None },
                    },
                    0,
                )
                .unwrap(),
        )
        .id;
        let mut f = Self { directory, store, room, request: 100 };
        f.call(1, Operation::Invite { room, secret: token(50), expires_in_ms: 60000, uses: 2 }).unwrap();
        f.call(2, Operation::Join { room, invitation: token(50) }).unwrap();
        f.start(1000, &[1, 2]);
        f
    }
    fn call(&mut self, n: u8, operation: Operation) -> photocraft_sync::Result<RpcResponse> {
        self.request += 1;
        self.store.execute(
            Some(&token(n)),
            &RpcRequest {
                protocol_version: photocraft_party::protocol::PROTOCOL_VERSION,
                request_id: RequestId::new(&format!("{:032x}", self.request)).unwrap(),
                operation,
            },
            0,
        )
    }
    fn action(&mut self, n: u8, action: Action) -> RoomView {
        let revision = self.poll(n).revision;
        view(self.call(n, Operation::Apply { room: self.room, expected_revision: revision, action: Box::new(action) }).unwrap())
    }
    fn poll(&mut self, n: u8) -> RoomView {
        view(self.call(n, Operation::Poll { room: self.room, after_revision: 0 }).unwrap())
    }
    fn start(&mut self, id: u64, players: &[u8]) {
        for &n in players {
            self.action(n, Action::Ready { ready: true });
        }
        let projects = (0..players.len() * players.len()).map(|n| ProjectId::new(&format!("{:032x}", id * 100 + n as u64)).unwrap()).collect();
        self.action(1, Action::Start { game: game(id), projects });
    }
    fn upload(&mut self, n: u8, bytes: &[u8]) -> AssetRef {
        let asset = AssetRef { hash: AssetHash::new(&format!("{:x}", Sha256::digest(bytes))).unwrap(), bytes: bytes.len() as u64 };
        for (i, chunk) in bytes.chunks(photocraft_party::protocol::CHUNK_BYTES).enumerate() {
            let offset = i * photocraft_party::protocol::CHUNK_BYTES;
            self.call(
                n,
                Operation::Upload {
                    room: self.room,
                    hash: asset.hash,
                    total: asset.bytes,
                    offset: offset as u64,
                    bytes: chunk.to_vec(),
                    finish: offset + chunk.len() == bytes.len(),
                },
            )
            .unwrap();
        }
        asset
    }
    fn submit(&mut self) -> Submission {
        use photocraft_doc::{Color, ColorMode, Document, SampleType, Size};
        let current = self.poll(1);
        let assignment = current.assignment.unwrap().assignment;
        let doc = Document::with_background("archive fixture", Size::new(1536, 1024), ColorMode::Rgb, SampleType::U8, Color::WHITE);
        let bytes = photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap();
        let checkpoint = CheckpointRef { id: CheckpointId::new(&format!("{:032x}", 20)).unwrap(), asset: self.upload(1, &bytes) };
        let rendered = photocraft_compose::flatten(&doc).to_rgba8();
        let png = photocraft_sync::encode_png(1536, 1024, &rendered.pixels).unwrap();
        let image = self.upload(1, &png);
        let submitted = Submission {
            id: SubmissionId::new(&format!("{:032x}", 21)).unwrap(),
            assignment: assignment.id,
            author: person(1).id,
            project: assignment.project,
            revision: 1,
            image,
            checkpoint,
            replay: ReplayRef {
                id: ReplayId::new(&format!("{:032x}", 22)).unwrap(),
                manifest: None,
                baseline: checkpoint,
                end_event: None,
                event_count: 0,
                duration_ms: 0,
            },
            assets: vec![image, checkpoint.asset],
            note: "metadata outside artwork".into(),
            seam: None,
            extent: None,
        };
        self.action(1, Action::Submit { epoch: current.epoch, submission: submitted.clone() });
        submitted
    }
    fn history(&mut self, n: u8, after_revision: u64) -> (Vec<photocraft_party::protocol::ArchivedGameSummary>, Option<u64>) {
        match self.call(n, Operation::History { room: self.room, after_revision }).unwrap() {
            RpcResponse::History { games, next_revision } => (games, next_revision),
            other => panic!("history: {other:?}"),
        }
    }
}

#[test]
fn archives_retain_frozen_grants_after_next_game_leave_restart_and_backup() {
    let mut f = Fixture::new();
    let submission = f.submit();
    let active_revision = f.poll(1).revision;
    assert!(f.history(1, 0).0.is_empty());
    for n in [1, 2, 3, 4] {
        assert!(f.call(n, Operation::Game { room: f.room, game: game(1000) }).is_err());
        assert!(
            f.call(n, Operation::GameFetch { room: f.room, game: game(1000), revision: active_revision, hash: submission.checkpoint.asset.hash, offset: 0 })
                .is_err()
        );
        assert!(f.call(n, Operation::GameNotes { room: f.room, game: game(1000) }).is_err());
    }
    // Third arrival waits and must not inherit this game's reveal when promoted later.
    f.call(3, Operation::Join { room: f.room, invitation: token(50) }).unwrap();
    f.action(1, Action::End { reason: "intentional incomplete fixture".into() });
    let (summary, next) = f.history(1, 0);
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].authors, vec![person(1), person(2)]);
    assert!(!summary[0].complete);
    assert_eq!(next, None);
    assert!(f.history(3, 0).0.is_empty());
    assert!(f.call(4, Operation::History { room: f.room, after_revision: 0 }).is_err());
    let original = view(f.call(2, Operation::Game { room: f.room, game: game(1000) }).unwrap());
    f.call(3, Operation::Disconnect { room: f.room }).unwrap();
    f.action(1, Action::NextGame);
    let revision = f.poll(2).revision;
    assert_eq!(f.call(2, Operation::Apply { room: f.room, expected_revision: revision, action: Box::new(Action::Leave) }).unwrap(), RpcResponse::Disconnected);
    assert!(f.call(2, Operation::Poll { room: f.room, after_revision: 0 }).is_err());
    assert_eq!(f.history(2, 0).0[0].game, game(1000), "leaving future lobbies cannot revoke already revealed work");
    let backup = f.directory.path().join("backup.sqlite3");
    f.store.backup(&backup).unwrap();
    let restored = f.directory.path().join("restored.sqlite3");
    Store::copy_verified(&backup, &restored).unwrap();
    f.store = Store::open(restored, StoreOptions::default(), 0).unwrap();
    let archived = view(f.call(2, Operation::Game { room: f.room, game: game(1000) }).unwrap());
    assert_eq!(archived.reveal, original.reveal);
    assert!(archived.revision > original.revision);
    for n in [1, 2] {
        assert!(matches!(
            f.call(n, Operation::GameFetch { room: f.room, game: game(1000), revision: original.revision, hash: submission.checkpoint.asset.hash, offset: 0 })
                .unwrap(),
            RpcResponse::AssetChunk { .. }
        ));
        assert!(
            matches!(f.call(n, Operation::GameNotes { room: f.room, game: game(1000) }).unwrap(), RpcResponse::Notes { notes } if notes[0].1 == "metadata outside artwork")
        );
        assert!(
            f.call(n, Operation::GameFetch { room: f.room, game: game(9999), revision: original.revision, hash: submission.image.hash, offset: 0 }).is_err()
        );
        for revision in [0, archived.revision + 1, u64::MAX] {
            assert!(f.call(n, Operation::GameFetch { room: f.room, game: game(1000), revision, hash: submission.image.hash, offset: 0 }).is_err());
        }
    }
    for n in [3, 4] {
        assert!(f.call(n, Operation::Game { room: f.room, game: game(1000) }).is_err());
        assert!(
            f.call(n, Operation::GameFetch { room: f.room, game: game(1000), revision: original.revision, hash: submission.image.hash, offset: 0 }).is_err()
        );
        assert!(f.call(n, Operation::GameNotes { room: f.room, game: game(1000) }).is_err());
    }
}

#[test]
fn history_pages_are_bounded_stable_and_do_not_consume_receipts() {
    let mut f = Fixture::new();
    for id in 1000..1018 {
        if id != 1000 {
            f.start(id, &[1, 2]);
        }
        f.action(1, Action::End { reason: "pagination fixture".into() });
        if id != 1017 {
            f.action(1, Action::NextGame);
        }
    }
    let db = rusqlite::Connection::open(f.directory.path().join("service.sqlite3")).unwrap();
    let count = || db.query_row("SELECT count(*) FROM receipts", [], |r| r.get::<_, i64>(0)).unwrap();
    let before = count();
    let (first, cursor) = f.history(1, 0);
    assert_eq!(first.len(), 16);
    assert_eq!(cursor, Some(first[15].revision));
    let (last, cursor) = f.history(1, cursor.unwrap());
    assert_eq!(last.len(), 2);
    assert_eq!(last[1].game, game(1017));
    assert_eq!(cursor, None);
    assert!(f.history(1, last[1].revision).0.is_empty());
    assert_eq!(count(), before);
    assert!(f.call(1, Operation::History { room: f.room, after_revision: u64::MAX }).is_err());
}

#[test]
fn corrupted_archived_snapshot_blocks_backup_without_publishing_a_destination() {
    let mut f = Fixture::new();
    f.action(1, Action::End { reason: "corruption fixture".into() });
    f.action(1, Action::NextGame);
    let db = rusqlite::Connection::open(f.directory.path().join("service.sqlite3")).unwrap();
    db.execute("UPDATE archives SET snapshot=x'00'", []).unwrap();
    let destination = f.directory.path().join("bad-backup.sqlite3");
    assert!(f.store.backup(&destination).is_err());
    assert!(!destination.exists());
    assert!(f.call(1, Operation::History { room: f.room, after_revision: 0 }).is_err());
}

#[test]
fn missing_accepted_asset_blocks_backup_and_reopen_even_after_next_game() {
    let mut f = Fixture::new();
    let submitted = f.submit();
    f.action(1, Action::End { reason: "dependency corruption fixture".into() });
    f.action(1, Action::NextGame);
    let database = f.directory.path().join("service.sqlite3");
    let db = rusqlite::Connection::open(&database).unwrap();
    db.execute("DELETE FROM objects WHERE hash=?1", [submitted.checkpoint.asset.hash.as_str()]).unwrap();
    let destination = f.directory.path().join("missing-backup.sqlite3");
    assert!(f.store.backup(&destination).is_err());
    assert!(!destination.exists());
    assert!(Store::open(&database, StoreOptions::default(), 0).is_err());
    assert!(database.exists());
}
