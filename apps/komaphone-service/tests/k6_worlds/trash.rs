use super::*;

fn denied(store: &mut Store, actor: u32, action: Action) {
    assert!(store.world_execute(&token(actor), &request(action), 100).is_err());
}
fn empty_directory(store: &mut Store, actor: u32) {
    let Response::Canvases { items, next } = call(store, actor, Action::List { after: None, limit: 128 }) else { panic!() };
    assert!(items.is_empty() && next.is_none());
}

#[test]
fn deletion_revokes_all_surface_access_and_restores_exact_history_at_fresh_epochs() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("service.db");
    let mut store = Store::open(&path, StoreOptions::default(), 0).unwrap();
    for n in 1..=3 {
        pair(&mut store, n);
    }
    create(&mut store, 1, 100);
    invite(&mut store, 1, 2, 100, 700);
    call(&mut store, 1, Action::Append { operation: op(1, 100, 1, 301) });
    call(&mut store, 2, Action::Append { operation: op(2, 100, 1, 302) });
    let accepted = events(&mut store, 2, 100);
    call(
        &mut store,
        1,
        Action::Invite { canvas: canvas(100), id: OpId::new(&hex(701)).unwrap(), recipient: owner(3), secret: token(701), expires_at_ms: 10000 },
    );
    let presence = Action::Presence {
        canvas: canvas(100),
        epoch: 1,
        camera: coordinates::home(canvas(100)),
        preview: vec![],
        cursor: Some(Default::default()),
        stroke: None,
    };
    call(&mut store, 2, presence.clone());
    for actor in [2, 3] {
        denied(&mut store, actor, Action::Delete { canvas: canvas(100), expected_epoch: 1 });
    }
    call(&mut store, 1, Action::Delete { canvas: canvas(100), expected_epoch: 1 });
    call(&mut store, 1, Action::Delete { canvas: canvas(100), expected_epoch: 1 }); // Idempotent lost ACK.
    for actor in 1..=3 {
        empty_directory(&mut store, actor);
        for action in [
            Action::Info { canvas: canvas(100) },
            Action::Members { canvas: canvas(100), after: None, limit: 128 },
            Action::Events { canvas: canvas(100), after: 0, through: None, limit: 128 },
            Action::Peers { canvas: canvas(100) },
            presence.clone(),
            Action::Append { operation: op(actor, 100, 2, 400 + actor) },
        ] {
            denied(&mut store, actor, action);
        }
    }
    for actor in [2, 3] {
        let Response::Trash { items, next } = call(&mut store, actor, Action::Trash { after: None, limit: 128 }) else { panic!() };
        assert!(items.is_empty() && next.is_none());
        denied(&mut store, actor, Action::RestoreOriginal { canvas: canvas(100), expected_epoch: 2 });
    }
    let Response::Trash { items, next } = call(&mut store, 1, Action::Trash { after: None, limit: 128 }) else { panic!() };
    assert_eq!(items.len(), 1);
    assert!(next.is_none());
    assert_eq!((items[0].canvas, items[0].revision, items[0].owner_epoch, items[0].deleted_at_ms), (canvas(100), 2, 2, 100));
    let accept = Action::Accept { canvas: canvas(100), id: OpId::new(&hex(701)).unwrap(), secret: token(701) };
    denied(&mut store, 3, accept.clone());
    let backup = directory.path().join("deleted-backup.db");
    store.backup(&backup).unwrap();
    drop(store);
    let recovered = directory.path().join("fresh.db");
    Store::copy_verified(&backup, &recovered).unwrap();
    let mut store = Store::open(&recovered, StoreOptions::default(), 200).unwrap();
    denied(&mut store, 1, Action::RestoreOriginal { canvas: canvas(100), expected_epoch: 1 });
    call(&mut store, 1, Action::RestoreOriginal { canvas: canvas(100), expected_epoch: 2 });
    call(&mut store, 1, Action::RestoreOriginal { canvas: canvas(100), expected_epoch: 2 });
    denied(&mut store, 1, Action::Delete { canvas: canvas(100), expected_epoch: 1 }); // Delayed retry cannot delete restored art.
    denied(&mut store, 3, accept);
    assert_eq!(events(&mut store, 2, 100), accepted);
    let Response::Members { items, .. } = call(&mut store, 1, Action::Members { canvas: canvas(100), after: None, limit: 128 }) else { panic!() };
    assert_eq!(items.len(), 2);
    let Response::Peers { peers } = call(&mut store, 1, Action::Peers { canvas: canvas(100) }) else { panic!() };
    assert!(peers.is_empty());
    let stale = op(2, 100, 2, 402);
    denied(&mut store, 2, Action::Append { operation: stale.clone() });
    let mut fresh = stale;
    fresh.authority_epoch = 2;
    call(&mut store, 2, Action::Append { operation: fresh });
    call(&mut store, 1, Action::Delete { canvas: canvas(100), expected_epoch: 2 });
    denied(&mut store, 1, Action::RestoreOriginal { canvas: canvas(100), expected_epoch: 2 });
    call(&mut store, 1, Action::RestoreOriginal { canvas: canvas(100), expected_epoch: 3 });
    assert_eq!(events(&mut store, 2, 100).len(), 3);
}

#[test]
fn failed_delete_or_restore_rolls_back_grants_invitations_and_tombstone() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path().join("service.db"), StoreOptions::default(), 0).unwrap();
    pair(&mut store, 1);
    pair(&mut store, 2);
    create(&mut store, 1, 100);
    invite(&mut store, 1, 2, 100, 700);
    for (action, deleted) in
        [(Action::Delete { canvas: canvas(100), expected_epoch: 1 }, false), (Action::RestoreOriginal { canvas: canvas(100), expected_epoch: 2 }, true)]
    {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store.world_execute_observed(&token(1), &request(action.clone()), 100, Limits::default(), |stage| {
                if stage == SaveStage::BeforeCommit {
                    panic!("synthetic interruption");
                }
            })
        }));
        assert!(result.is_err());
        let Response::Trash { items, .. } = call(&mut store, 1, Action::Trash { after: None, limit: 128 }) else { panic!() };
        assert_eq!(!items.is_empty(), deleted);
        if !deleted {
            let Response::Canvas { info } = call(&mut store, 2, Action::Info { canvas: canvas(100) }) else { panic!() };
            assert_eq!(info.epoch, 1);
        } else {
            denied(&mut store, 2, Action::Info { canvas: canvas(100) });
        }
        call(&mut store, 1, action);
    }
}

#[test]
fn trash_directory_pages_only_owned_canvases_and_v2_migration_retains_verified_backup() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("service.db");
    let mut store = Store::open(&path, StoreOptions::default(), 0).unwrap();
    pair(&mut store, 1);
    pair(&mut store, 2);
    for id in [100, 101, 102] {
        create(&mut store, 1, id);
    }
    create(&mut store, 2, 200);
    call(&mut store, 1, Action::Append { operation: op(1, 100, 1, 301) });
    let before = events(&mut store, 1, 100);
    drop(store);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE world_import_assets; DROP TABLE world_imports; DROP TABLE world_import_receipts; DROP TABLE world_import_origins; DROP TABLE world_trash; DROP TABLE world_schema; CREATE TABLE world_schema(version INTEGER NOT NULL CHECK(version=2)); INSERT INTO world_schema VALUES(2);").unwrap();
    drop(db);
    let mut store = Store::open(&path, StoreOptions::default(), 100).unwrap();
    assert_eq!(events(&mut store, 1, 100), before);
    let backups = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|entry| entry.file_name().unwrap().to_string_lossy().contains("before-world-v3-"))
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    let db = rusqlite::Connection::open(&backups[0]).unwrap();
    assert_eq!(db.query_row::<u32, _, _>("SELECT version FROM world_schema", [], |row| row.get(0)).unwrap(), 2);
    assert_eq!(db.query_row::<u32, _, _>("SELECT count(*) FROM world_events", [], |row| row.get(0)).unwrap(), 1);
    for id in [100, 101, 102] {
        call(&mut store, 1, Action::Delete { canvas: canvas(id), expected_epoch: 1 });
    }
    call(&mut store, 2, Action::Delete { canvas: canvas(200), expected_epoch: 1 });
    let mut cursor = None;
    let mut canvases = Vec::new();
    loop {
        let Response::Trash { items, next } = call(&mut store, 1, Action::Trash { after: cursor, limit: 2 }) else { panic!() };
        assert!(items.len() <= 2);
        canvases.extend(items.iter().map(|item| item.canvas));
        if next.is_none() {
            break;
        }
        cursor = next;
    }
    assert_eq!(canvases, [canvas(100), canvas(101), canvas(102)]);
    denied(&mut store, 1, Action::Trash { after: None, limit: 0 });
    store.backup(directory.path().join("verified-trash.db")).unwrap();
}
