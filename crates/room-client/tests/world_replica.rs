use photocraft_room_client::world_replica::WorldReplica;
use photocraft_scene::{
    CanvasId, OwnerId,
    shared::{AcceptedOp, DeviceId, Edit, OpId, WorldOp, WorldState},
    world_protocol::{CanvasSummary, Response},
};
fn id(n: u32) -> String {
    format!("{n:032x}")
}
fn info() -> CanvasSummary {
    CanvasSummary {
        canvas: CanvasId::new(&id(1)).unwrap(),
        owner: OwnerId::new(&id(2)).unwrap(),
        name: "Original".into(),
        import_origin: None,
        initial_name: "Original".into(),
        revision: 0,
        epoch: 1,
    }
}

#[test]
fn imported_history_replays_and_reopens_with_provenance_intact() {
    let source = info();
    let mut world = WorldState::new(source.canvas, source.owner, source.name).unwrap();
    world.apply(&rename()).unwrap();
    let imported = world.import_to(CanvasId::new(&id(80)).unwrap(), OwnerId::new(&id(81)).unwrap()).unwrap();
    let summary = CanvasSummary {
        canvas: imported.canvas,
        owner: imported.owner,
        name: imported.name.clone(),
        initial_name: imported.replay_to(0).unwrap().name,
        import_origin: imported.import_origin.clone(),
        revision: imported.revision,
        epoch: 1,
    };
    let mut replica = WorldReplica::open(summary.clone(), None).unwrap();
    let mut truncated = summary.clone();
    truncated.revision = 0;
    truncated.name = truncated.initial_name.clone();
    assert!(WorldReplica::open(truncated, None).is_err());
    let mut corrupt = summary.clone();
    corrupt.import_origin.as_mut().unwrap().prefix_hash = photocraft_scene::shared::media::AssetHash::new(&"a".repeat(64)).unwrap();
    let mut rejected = WorldReplica::open(corrupt, None).unwrap();
    let before = rejected.accepted().canonical_hash().unwrap();
    assert!(
        rejected.apply_page(&Response::Events { events: imported.events_after(0, 128).unwrap().to_vec(), through: imported.revision, next: None }).is_err()
    );
    assert_eq!(before, rejected.accepted().canonical_hash().unwrap());
    replica.apply_page(&Response::Events { events: imported.events_after(0, 128).unwrap().to_vec(), through: imported.revision, next: None }).unwrap();
    assert!(replica.complete());
    assert_eq!(replica.accepted().canonical_hash().unwrap(), imported.canonical_hash().unwrap());
    let cached: WorldState = serde_json::from_slice(&serde_json::to_vec(replica.accepted()).unwrap()).unwrap();
    let reopened = WorldReplica::open(summary.clone(), Some(cached.clone())).unwrap();
    assert_eq!(reopened.accepted().canonical_hash().unwrap(), imported.canonical_hash().unwrap());
    let mut missing_origin = summary;
    missing_origin.import_origin = None;
    assert!(WorldReplica::open(missing_origin, Some(cached)).is_err());
}
fn rename() -> AcceptedOp {
    let i = info();
    AcceptedOp {
        revision: 1,
        op: WorldOp {
            id: OpId::new(&id(4)).unwrap(),
            canvas: i.canvas,
            author: i.owner,
            device: DeviceId::new(&id(3)).unwrap(),
            sequence: 1,
            authority_epoch: 1,
            action: Edit::Rename { name: "Changed".into(), expected_revision: 0 },
        },
    }
}
#[test]
fn renamed_first_sync_replays_identically_and_rejects_partial_bad_page() {
    let mut i = info();
    i.name = "Changed".into();
    i.revision = 1;
    let mut replica = WorldReplica::open(i, None).unwrap();
    let original = replica.accepted().canonical_hash().unwrap();
    let mut bad = rename();
    bad.revision = 2;
    assert!(replica.apply_page(&Response::Events { events: vec![rename(), bad], through: 1, next: None }).is_err());
    assert_eq!(original, replica.accepted().canonical_hash().unwrap());
    replica.apply_page(&Response::Events { events: vec![rename()], through: 1, next: None }).unwrap();
    assert!(replica.complete());
    let i = info();
    let mut direct = WorldState::new(i.canvas, i.owner, i.name).unwrap();
    direct.apply(&rename()).unwrap();
    assert_eq!(direct.canonical_hash().unwrap(), replica.accepted().canonical_hash().unwrap());
}
#[test]
fn optimistic_art_is_separate_and_cannot_cross_account_canvas_or_revocation() {
    let i = info();
    let replica = WorldReplica::open(i.clone(), None).unwrap();
    let pending = rename().op;
    let view = replica.with_pending(i.owner, std::slice::from_ref(&pending)).unwrap();
    assert_eq!(view.name, "Changed");
    assert_eq!(replica.accepted().name, "Original");
    assert_eq!(replica.accepted().revision, 0);
    assert!(replica.with_pending(OwnerId::new(&id(9)).unwrap(), std::slice::from_ref(&pending)).is_err());
    let mut bad = pending.clone();
    bad.authority_epoch = 2;
    assert!(replica.with_pending(i.owner, &[bad]).is_err());
    let mut bad = pending;
    bad.canvas = CanvasId::new(&id(9)).unwrap();
    assert!(replica.with_pending(i.owner, &[bad]).is_err());
}
#[test]
fn stale_restore_foreign_seed_and_false_pagination_never_replace_cache() {
    let i = info();
    let mut cached = WorldState::new(i.canvas, i.owner, i.name.clone()).unwrap();
    cached.apply(&rename()).unwrap();
    assert!(WorldReplica::open(i.clone(), Some(cached)).is_err());
    let wrong_seed = WorldState::new(i.canvas, i.owner, "Other".into()).unwrap();
    assert!(WorldReplica::open(i.clone(), Some(wrong_seed)).is_err());
    let mut current = i;
    current.revision = 1;
    current.name = "Changed".into();
    let mut replica = WorldReplica::open(current, None).unwrap();
    assert!(replica.apply_page(&Response::Events { events: vec![], through: 1, next: Some(0) }).is_err());
    assert!(replica.apply_page(&Response::Events { events: vec![rename()], through: 1, next: Some(1) }).is_err());
    assert_eq!(replica.accepted().revision, 0);
    assert!(replica.append_contiguous(&rename()).unwrap());
    assert!(!replica.append_contiguous(&rename()).unwrap());
}

#[test]
fn refreshed_grants_reject_invalid_or_regressing_advertised_state() {
    let mut future = info();
    future.revision = 5;
    let mut replica = WorldReplica::open(future.clone(), None).unwrap();
    let mut bad = future.clone();
    bad.revision = 4;
    assert!(replica.update_info(bad).is_err());
    let mut bad = future.clone();
    bad.epoch = u64::MAX;
    assert!(replica.update_info(bad).is_err());
    let mut bad = future.clone();
    bad.name = "x".repeat(97);
    assert!(replica.update_info(bad).is_err());
    let mut bad = future.clone();
    bad.revision = u64::MAX;
    assert!(replica.update_info(bad).is_err());
    assert_eq!(replica.info(), &future);
}
