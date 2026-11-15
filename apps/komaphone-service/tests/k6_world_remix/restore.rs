use super::*;
use photocraft_scene::shared::{Tour, TourId, TourStop, WorldState};

fn accepted(f: &mut Fixture, id: u64) -> WorldState {
    let Response::Canvas { info } = world(&mut f.store, 1, Action::Info { canvas: canvas(id) }) else { panic!() };
    let mut state = WorldState::new(info.canvas, info.owner, info.initial_name).unwrap();
    for event in events(&mut f.store, 1, id) {
        state.apply(&event).unwrap();
    }
    state
}

#[test]
fn archive_restore_proves_current_owner_and_prefix_then_copies_history_assets_without_grants() {
    let mut f = Fixture::new(Mode::Animation { rotations: 1 }, 2);
    f.complete();
    invite(&mut f.store, 1, 2, 100, 8000);
    let source = f.source(None);
    let prepared = prepared(&mut f.store, 1, 100, source);
    let op = operation(2, 100, 1, 8100, &prepared);
    world(&mut f.store, 2, Action::RemixGame { operation: op.clone(), room: SourceId::new(f.room.as_str()).unwrap() });
    let mut tour = operation(1, 100, 1, 8101, &prepared);
    tour.action = Edit::PutTour {
        tour: Tour {
            id: TourId::new(&hex(8101)).unwrap(),
            canvas: canvas(100),
            name: "Restored route".into(),
            revision: 0,
            stops: vec![TourStop { camera: photocraft_scene::coordinates::home(canvas(100)), travel_ms: 0, pause_ms: 1000, target: None }],
        },
        expected_revision: 0,
    };
    world(&mut f.store, 1, Action::Append { operation: tour });
    let archived = accepted(&mut f, 100);
    let request = Action::RestoreCopy {
        source_canvas: canvas(100),
        source_revision: archived.revision,
        source_hash: AssetHash::new(&archived.canonical_hash().unwrap()).unwrap(),
        new_canvas: canvas(200),
    };
    denied(&mut f.store, 2, request.clone()); // Source editor cannot restore ownership.
    denied(&mut f.store, 3, request.clone()); // Neither IDs nor hashes grant access.
    let mut forged = request.clone();
    if let Action::RestoreCopy { source_hash, .. } = &mut forged {
        *source_hash = AssetHash::new(&"a".repeat(64)).unwrap();
    }
    denied(&mut f.store, 1, forged);
    let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        f.store.world_execute_observed(&token(1), &super::request(request.clone()), 0, Limits::default(), |stage| {
            if stage == SaveStage::BeforeCommit {
                panic!("synthetic loss after all media copied");
            }
        })
    }));
    assert!(interrupted.is_err());
    denied(&mut f.store, 1, Action::Info { canvas: canvas(200) });
    let check = Connection::open(f.directory.path().join("service.sqlite3")).unwrap();
    let count: u32 = check.query_row("SELECT count(*) FROM world_blobs WHERE canvas=?1", [canvas(200).as_str()], |r| r.get(0)).unwrap();
    assert_eq!(count, 0);
    drop(check);
    // Later art on the source must not enter an earlier archive's restored prefix.
    let mut hide = operation(2, 100, 2, 8102, &prepared);
    hide.action = Edit::SetMediaVisible { media: MediaId::new(&hex(8100)).unwrap(), visible: false };
    world(&mut f.store, 2, Action::Append { operation: hide });
    let source_after = accepted(&mut f, 100);
    let Response::Canvas { info } = world(&mut f.store, 1, request.clone()) else { panic!() };
    assert_eq!(info.revision, 2);
    assert_eq!(info.owner, owner(1));
    let restored = accepted(&mut f, 200);
    assert_eq!(restored, archived.copy_to(canvas(200)).unwrap());
    assert_eq!(restored.media[&MediaId::new(&hex(8100)).unwrap()].media.owner, owner(2));
    assert!(restored.media[&MediaId::new(&hex(8100)).unwrap()].visible);
    assert_eq!(restored.tours[&TourId::new(&hex(8101)).unwrap()].stops[0].camera.canvas, canvas(200));
    let Response::Members { items, .. } = world(&mut f.store, 1, Action::Members { canvas: canvas(200), after: None, limit: 128 }) else { panic!() };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].profile, owner(1));
    denied(&mut f.store, 2, Action::Info { canvas: canvas(200) });
    let mut unauthorized = op;
    unauthorized.canvas = canvas(200);
    if let Edit::PlaceMedia { media } = &mut unauthorized.action {
        media.canvas = canvas(200);
    }
    denied(&mut f.store, 2, Action::RemixGame { operation: unauthorized, room: SourceId::new(f.room.as_str()).unwrap() });
    for frame in &prepared.frames {
        assert_eq!(fetch(&mut f.store, 1, 200, frame.hash), fetch(&mut f.store, 1, 100, frame.hash));
        denied(&mut f.store, 2, Action::FetchMedia { canvas: canvas(200), hash: frame.hash, offset: 0 });
    }
    assert_eq!(accepted(&mut f, 100), source_after);
    // Retry may never overwrite either newer art or newly chosen memberships.
    let mut rename = operation(1, 200, 2, 8103, &prepared);
    rename.action = Edit::Rename { name: "Independent copy".into(), expected_revision: 0 };
    world(&mut f.store, 1, Action::Append { operation: rename });
    let newer = accepted(&mut f, 200);
    world(&mut f.store, 1, request.clone());
    assert_eq!(accepted(&mut f, 200), newer);
    let path = f.directory.path().join("service.sqlite3");
    drop(f.store);
    f.store = Store::open(&path, StoreOptions::default(), 100).unwrap();
    world(&mut f.store, 1, request);
    assert_eq!(accepted(&mut f, 200), newer);
    assert_eq!(accepted(&mut f, 100), source_after);
}

#[test]
fn restore_conflicts_and_interrupted_transaction_leave_existing_canvases_and_blobs_intact() {
    let mut f = Fixture::new(Mode::Animation { rotations: 1 }, 2);
    let seed = accepted(&mut f, 100);
    create(&mut f.store, 1, 200);
    let existing = accepted(&mut f, 200);
    let action = Action::RestoreCopy {
        source_canvas: canvas(100),
        source_revision: 0,
        source_hash: AssetHash::new(&seed.canonical_hash().unwrap()).unwrap(),
        new_canvas: canvas(200),
    };
    denied(&mut f.store, 1, action.clone());
    assert_eq!(accepted(&mut f, 200), existing);
    let mut action = action;
    if let Action::RestoreCopy { new_canvas, .. } = &mut action {
        *new_canvas = canvas(300);
    }
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        f.store.world_execute_observed(&token(1), &request(action.clone()), 0, Limits::default(), |stage| {
            if stage == SaveStage::BeforeCommit {
                panic!("synthetic power loss");
            }
        })
    }));
    assert!(failed.is_err());
    denied(&mut f.store, 1, Action::Info { canvas: canvas(300) });
    world(&mut f.store, 1, action);
    assert_eq!(accepted(&mut f, 300), seed.copy_to(canvas(300)).unwrap());
    assert_eq!(accepted(&mut f, 100), seed);
    assert_eq!(accepted(&mut f, 200), existing);
}

#[test]
fn restore_rejects_hash_valid_checkpoint_that_disagrees_with_authorized_canvas_owner() {
    let mut f = Fixture::new(Mode::Animation { rotations: 1 }, 2);
    let original = accepted(&mut f, 100);
    let forged = WorldState::new(canvas(100), owner(2), original.name.clone()).unwrap();
    let bytes = serde_json::to_vec(&forged).unwrap();
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let db = Connection::open(f.directory.path().join("service.sqlite3")).unwrap();
    db.execute("UPDATE worlds SET state=?1,hash=?2 WHERE id=?3", rusqlite::params![bytes, hash, canvas(100).as_str()]).unwrap();
    let action = Action::RestoreCopy {
        source_canvas: canvas(100),
        source_revision: 0,
        source_hash: AssetHash::new(&forged.canonical_hash().unwrap()).unwrap(),
        new_canvas: canvas(400),
    };
    denied(&mut f.store, 1, action);
    denied(&mut f.store, 1, Action::Info { canvas: canvas(400) });
    let count: u32 = db.query_row("SELECT count(*) FROM world_members WHERE canvas=?1", [canvas(400).as_str()], |r| r.get(0)).unwrap();
    assert_eq!(count, 0);
}
