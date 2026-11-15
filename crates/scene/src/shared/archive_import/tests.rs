use super::*;
use crate::shared::{AcceptedOp, Tour, TourStop, WorldOp};
use crate::world_protocol::{Action, CanvasSummary, ImportObject, Request, Response, VERSION};
use crate::{RegionKey, Srgba, Stroke, coordinates};

fn hex(n: u128) -> String {
    format!("{n:032x}")
}
fn owner(n: u128) -> OwnerId {
    OwnerId::new(&hex(n)).unwrap()
}
fn canvas(n: u128) -> CanvasId {
    CanvasId::new(&hex(n)).unwrap()
}
fn device(n: u128) -> DeviceId {
    DeviceId::new(&hex(n)).unwrap()
}
fn append(world: &mut WorldState, author: u128, writer: u128, action: Edit) {
    let revision = world.revision + 1;
    world
        .apply(&AcceptedOp {
            revision,
            op: WorldOp {
                id: OpId::new(&hex(10000 + u128::from(revision))).unwrap(),
                canvas: world.canvas,
                author: owner(author),
                device: device(writer),
                sequence: world.next_sequence(device(writer)).unwrap(),
                authority_epoch: 3,
                action,
            },
        })
        .unwrap();
}
fn populated(artists: u128) -> WorldState {
    let mut world = WorldState::new(canvas(1), owner(10), "Original".into()).unwrap();
    for n in 0..artists {
        let stroke = Stroke {
            id: StrokeId::new(&hex(1000 + n)).unwrap(),
            canvas: world.canvas,
            owner: owner(10 + n),
            revision: 0,
            anchor: RegionKey { depth: 6, ..Default::default() },
            points: vec![[0., 0.], [10., 10.]],
            width: 5.,
            color: Srgba { rgba: [0.2, 0.3, 0.4, 1.] },
            erase: n == 2,
        };
        // Device order intentionally differs from event/author order.
        append(&mut world, 10 + n, 1000 + artists - n, Edit::AddStroke { stroke });
    }
    world
}

#[test]
fn partial_imported_replay_is_viewable_but_not_portable() {
    let imported = populated(8).import_to(canvas(2), owner(900)).unwrap();
    let partial = imported.replay_to(3).unwrap();
    let before = serde_json::to_vec(&partial).unwrap();
    partial.validate().unwrap();
    assert!(partial.validate_complete_import().is_err());
    assert!(partial.import_to(canvas(3), owner(901)).is_err());
    let manifest = ArchiveManifest { schema: 1, world: partial.clone(), assets: vec![] };
    assert!(manifest.validate().is_err());
    assert!(ArchiveManifest::decode(&serde_json::to_vec(&manifest).unwrap()).is_err());
    assert_eq!(serde_json::to_vec(&partial).unwrap(), before);
    imported.validate_complete_import().unwrap();
    ArchiveManifest { schema: 1, world: imported.clone(), assets: vec![] }.validate().unwrap();
    imported.import_to(canvas(3), owner(901)).unwrap();
}

#[test]
fn imported_credit_copies_to_live_layer_without_profile_or_artist_impersonation() {
    let source = populated(2);
    let imported = source.import_to(canvas(2), owner(900)).unwrap();
    let historical = imported.strokes.values().next().unwrap();
    let credit = imported.imported_credit(historical.stroke.owner).unwrap();
    assert_eq!(credit.profile, imported.import_origin.as_ref().unwrap().original_author(historical.stroke.owner).unwrap());
    assert_eq!(credit.label, format!("Archive author {}", credit.profile.as_str()));
    assert!(imported.imported_credit(imported.owner).is_none());
    let target = super::super::StrokeCopyTarget {
        canvas: imported.canvas,
        id: StrokeId::new(&hex(777)).unwrap(),
        owner: imported.owner,
        anchor: historical.stroke.anchor,
        offset: [0., 0.],
        scale: 1.,
    };
    assert!(super::super::stroke_copy(historical, imported.canvas, credit.clone(), target.clone()).is_err());
    let Edit::CopyStroke { stroke, provenance } = imported.copy_stroke(historical.stroke.id, credit.clone(), target.clone()).unwrap() else { panic!() };
    assert_eq!(stroke.owner, imported.owner);
    assert_eq!(stroke.points, historical.stroke.points);
    assert_eq!(provenance.author, credit);
    let ordinary = source.strokes.values().next().unwrap();
    let wrong = super::super::media::Credit { profile: owner(999), label: "Wrong artist".into() };
    assert!(source.copy_stroke(ordinary.stroke.id, wrong, target).is_err());
}

#[test]
fn eight_artist_import_is_fresh_preserves_layer_order_geometry_and_original_credits() {
    let source = populated(8);
    let before = serde_json::to_vec(&source).unwrap();
    let imported = source.import_to(canvas(2), owner(900)).unwrap();
    assert_eq!(source.import_to(canvas(2), owner(900)).unwrap(), imported);
    assert_eq!(serde_json::to_vec(&source).unwrap(), before);
    assert_eq!(imported.owner, owner(900));
    assert_eq!(imported.revision, 8);
    let origin = imported.import_origin.as_ref().unwrap();
    assert_eq!(origin.actors.len(), 8);
    assert_eq!(origin.source_hash.as_str(), source.canonical_hash().unwrap());
    let old = source.events_after(0, 128).unwrap();
    let new = imported.events_after(0, 128).unwrap();
    let mut device_pairs = Vec::new();
    for (a, b) in old.iter().zip(new) {
        assert_ne!(a.op.id, b.op.id);
        assert_ne!(a.op.author, b.op.author);
        assert_ne!(b.op.author, imported.owner);
        assert_eq!(origin.original_author(b.op.author), Some(a.op.author));
        assert!(imported.imported_device(b.op.device));
        device_pairs.push((a.op.device, b.op.device));
        let (Edit::AddStroke { stroke: a }, Edit::AddStroke { stroke: b }) = (&a.op.action, &b.op.action) else { panic!() };
        assert_ne!(a.id, b.id);
        assert_eq!((a.anchor, &a.points, a.width, a.color, a.erase), (b.anchor, &b.points, b.width, b.color, b.erase));
    }
    device_pairs.sort_unstable();
    assert!(device_pairs.windows(2).all(|p| p[0].1 < p[1].1));
    imported.validate().unwrap();
}

#[test]
fn history_rename_visibility_tour_targets_and_partial_replay_survive() {
    let mut source = populated(2);
    let target = StrokeId::new(&hex(1000)).unwrap();
    append(&mut source, 10, 1002, Edit::Rename { name: "Renamed".into(), expected_revision: 0 });
    append(&mut source, 10, 1002, Edit::SetStrokeVisible { stroke: target, visible: false });
    let tour = Tour {
        id: TourId::new(&hex(50)).unwrap(),
        canvas: source.canvas,
        name: "Route".into(),
        revision: 0,
        stops: vec![TourStop { camera: coordinates::home(source.canvas), travel_ms: 0, pause_ms: 1000, target: Some(target) }],
    };
    append(&mut source, 10, 1002, Edit::PutTour { tour, expected_revision: 0 });
    let mut copy = source.import_to(canvas(2), owner(900)).unwrap();
    let origin = copy.import_origin.clone().unwrap();
    for n in 0..=source.revision {
        let prefix = copy.replay_to(n).unwrap();
        prefix.validate().unwrap();
        assert_eq!(prefix.revision, n);
        assert_eq!(prefix.import_origin, Some(origin.clone()));
        if n < source.revision {
            assert!(prefix.copy_to(canvas(3)).is_err());
        }
    }
    assert_eq!(copy.name, "Renamed");
    let route = copy.tours.values().next().unwrap();
    assert_eq!(route.stops[0].camera.canvas, canvas(2));
    assert!(!copy.strokes[&route.stops[0].target.unwrap()].visible);
    append(&mut copy, 900, 900, Edit::Rename { name: "Current owner".into(), expected_revision: 3 });
    copy.validate().unwrap();
    assert_eq!(copy.import_origin.unwrap(), origin);
}

#[test]
fn imported_writers_cannot_append_and_corrupt_prefix_is_rejected() {
    let source = populated(2);
    let mut copy = source.import_to(canvas(2), owner(900)).unwrap();
    let mut event = copy.events_after(0, 1).unwrap()[0].clone();
    event.revision = 3;
    event.op.id = OpId::new(&hex(9999)).unwrap();
    event.op.sequence += 1;
    event.op.action = Edit::PutTour {
        tour: Tour {
            id: TourId::new(&hex(5)).unwrap(),
            canvas: copy.canvas,
            name: "No".into(),
            revision: 0,
            stops: vec![TourStop { camera: coordinates::home(copy.canvas), travel_ms: 0, pause_ms: 10, target: None }],
        },
        expected_revision: 0,
    };
    let before = copy.clone();
    assert_eq!(copy.apply(&event), Err(Error::Permission));
    assert_eq!(copy, before);
    copy.import_origin.as_mut().unwrap().prefix_hash = AssetHash::new(&"a".repeat(64)).unwrap();
    assert!(copy.validate().is_err());
    assert!(source.import_to(source.canvas, owner(900)).is_err());
    before.import_to(canvas(3), owner(900)).unwrap().validate().unwrap();
    assert_eq!(before.canonical_hash().unwrap(), source.import_to(canvas(2), owner(900)).unwrap().canonical_hash().unwrap());
}

#[test]
fn old_serialized_worlds_omit_origin_and_imported_summaries_rebuild_exactly() {
    let source = populated(3);
    let bytes = serde_json::to_vec(&source).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("import_origin"));
    let old: WorldState = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(serde_json::to_vec(&old).unwrap(), bytes);
    let copy = source.import_to(canvas(2), owner(900)).unwrap();
    let mut rebuilt = WorldState::with_import_origin(copy.canvas, copy.owner, "Original".into(), copy.import_origin.clone()).unwrap();
    for event in copy.events_after(0, 128).unwrap() {
        rebuilt.apply(event).unwrap();
    }
    assert_eq!(rebuilt, copy);
    let second = copy.copy_to(canvas(3)).unwrap();
    second.validate().unwrap();
    assert_eq!(second.import_origin.as_ref().unwrap().source_hash, copy.import_origin.as_ref().unwrap().source_hash);
}

#[test]
fn actor_bound_rejects_129_without_mutation_and_worst_page_fits_response_budget() {
    let source = populated(129);
    let before = source.canonical_hash().unwrap();
    assert!(source.import_to(canvas(2), owner(900)).is_err());
    assert_eq!(source.canonical_hash().unwrap(), before);
    let mut accepted = populated(128).import_to(canvas(2), owner(900)).unwrap();
    let origin = accepted.import_origin.as_mut().unwrap();
    origin.limits =
        Limits { max_operations: 1_048_576, max_strokes: 262_144, max_points: 4_194_304, max_tours: 1024, max_media: 4096, max_media_frames: 65_536 };
    origin.source_revision = origin.limits.max_operations as u64;
    origin.device_count = origin.limits.max_operations as u32;
    origin.device_prefix = u64::MAX;
    origin.validate(accepted.canvas, accepted.owner).unwrap();
    let summary = CanvasSummary {
        canvas: accepted.canvas,
        owner: accepted.owner,
        name: "\\".repeat(96),
        initial_name: "\\".repeat(96),
        revision: i64::MAX as u64,
        epoch: i64::MAX as u64,
        import_origin: accepted.import_origin,
    };
    let bytes = serde_json::to_vec(&Response::Canvases { items: vec![summary; 128], next: Some(canvas(99)) }).unwrap();
    assert!(bytes.len() < crate::world_protocol::MAX_RESPONSE_BYTES, "{} bytes", bytes.len());
    eprintln!("128 maximal-credit summaries: {} bytes", bytes.len());
}

#[test]
fn manifest_is_independent_and_rejects_forged_inventory_and_invalid_history() {
    let source = populated(8);
    let mut value = ArchiveManifest { schema: 1, world: source.clone(), assets: vec![] };
    let bytes = serde_json::to_vec(&value).unwrap();
    let decoded = ArchiveManifest::decode(&bytes).unwrap();
    assert_eq!(decoded.world, source);
    value.assets.push(MediaFrame { hash: AssetHash::new(&"a".repeat(64)).unwrap(), byte_len: 10, width: 1, height: 1, duration_ms: 100 });
    assert!(value.validate().is_err());
    value.assets.clear();
    value.world.name = "Forgery".into();
    assert!(value.validate().is_err());
    assert!(ArchiveManifest::decode(b"{}").is_err());
}

#[test]
fn import_wire_rejects_old_version_oversized_chunks_and_overflow_before_staging() {
    let id = OpId::new(&hex(50)).unwrap();
    let valid = Action::ImportChunk { id, object: ImportObject::Manifest {}, offset: 0, bytes: vec![255; MAX_CHUNK_BYTES] };
    let encode = |version, action| serde_json::to_vec(&Request { version, action }).unwrap();
    let bytes = encode(VERSION, valid.clone());
    assert!(bytes.len() < crate::world_protocol::MAX_REQUEST_BYTES);
    Request::decode(&bytes).unwrap();
    assert!(Request::decode(&encode(1, valid)).is_err());
    for (offset, bytes) in [(0, vec![]), (0, vec![0; MAX_CHUNK_BYTES + 1]), (u64::MAX, vec![1])] {
        assert!(Request::decode(&encode(VERSION, Action::ImportChunk { id, object: ImportObject::Manifest {}, offset, bytes })).is_err());
    }
    for (manifest_bytes, total_bytes) in [(0, 0), (MAX_MANIFEST_BYTES + 1, MAX_TOTAL_BYTES), (32, 31), (1, MAX_TOTAL_BYTES + 1)] {
        let action = Action::ImportBegin { id, new_canvas: canvas(2), manifest_sha256: AssetHash::new(&"a".repeat(64)).unwrap(), manifest_bytes, total_bytes };
        assert!(Request::decode(&encode(VERSION, action)).is_err());
    }
}

#[test]
fn empty_archive_import_is_valid_but_invalid_aliases_are_not() {
    let source = WorldState::new(canvas(1), owner(10), "Empty".into()).unwrap();
    let mut imported = source.import_to(canvas(2), owner(900)).unwrap();
    imported.validate().unwrap();
    assert_eq!(imported.import_origin.as_ref().unwrap().device_count, 0);
    imported.import_origin.as_mut().unwrap().actors[0].imported = owner(900);
    assert!(imported.validate().is_err());
}

#[test]
fn import_then_new_edits_then_reimport_retains_ultimate_credits_and_all_owner_renames() {
    let mut source = populated(8);
    append(&mut source, 10, 1008, Edit::Rename { name: "Before import".into(), expected_revision: 0 });
    // Same original human now imports: keep their old/new history as separate
    // inert aliases on the next import, while both retain original credit10.
    let mut first = source.import_to(canvas(2), owner(10)).unwrap();
    append(&mut first, 10, 7000, Edit::Rename { name: "After import".into(), expected_revision: 9 });
    let stroke = Stroke {
        id: StrokeId::new(&hex(9000)).unwrap(),
        canvas: first.canvas,
        owner: owner(10),
        revision: 0,
        anchor: RegionKey { depth: 6, ..Default::default() },
        points: vec![[20., 20.], [30., 30.]],
        width: 3.,
        color: Srgba { rgba: [1., 0., 0., 1.] },
        erase: false,
    };
    append(&mut first, 10, 7000, Edit::AddStroke { stroke });
    let archived = ArchiveManifest { schema: 1, world: first.clone(), assets: vec![] };
    let reopened = ArchiveManifest::decode(&serde_json::to_vec(&archived).unwrap()).unwrap();
    let second = reopened.world.import_to(canvas(3), owner(900)).unwrap();
    second.validate().unwrap();
    assert_eq!(second.name, "After import");
    assert_eq!(second.strokes.len(), 9);
    assert_eq!(second.revision, first.revision);
    let origin = second.import_origin.as_ref().unwrap();
    assert_eq!(origin.source_hash.as_str(), first.canonical_hash().unwrap());
    assert_eq!(origin.actors.iter().filter(|actor| actor.original == owner(10)).count(), 2);
    for (old, new) in first.events_after(0, 128).unwrap().iter().zip(second.events_after(0, 128).unwrap()) {
        let credit = first.import_origin.as_ref().unwrap().original_author(old.op.author).unwrap_or(old.op.author);
        assert_eq!(origin.original_author(new.op.author), Some(credit));
    }
    for revision in 0..=second.revision {
        second.replay_to(revision).unwrap().validate().unwrap();
    }
    let third = second.import_to(canvas(4), owner(901)).unwrap();
    third.validate().unwrap();
    assert_eq!(third.revision, second.revision);
}

#[test]
fn animated_hidden_media_keeps_frames_timing_and_credits_through_independent_import() {
    use crate::shared::media::{Credit, MediaPlacement, MediaSource, Provenance};
    let mut source = populated(2);
    let frames = vec![
        MediaFrame { hash: AssetHash::new(&"a".repeat(64)).unwrap(), byte_len: 100, width: 12, height: 8, duration_ms: 100 },
        MediaFrame { hash: AssetHash::new(&"b".repeat(64)).unwrap(), byte_len: 120, width: 12, height: 8, duration_ms: 250 },
    ];
    let media = MediaPlacement {
        id: MediaId::new(&hex(50)).unwrap(),
        canvas: source.canvas,
        owner: owner(10),
        anchor: RegionKey { depth: 6, ..Default::default() },
        offset: [20., 30.],
        width: 12.,
        height: 8.,
        frames: frames.clone(),
        looping: true,
        provenance: Provenance {
            source: MediaSource::Canvas { canvas: canvas(80), media: MediaId::new(&hex(80)).unwrap() },
            source_revision: 1,
            artifact_hash: AssetHash::new(&"c".repeat(64)).unwrap(),
            authors: vec![Credit { profile: owner(80), label: "Original artist".into() }],
        },
    };
    append(&mut source, 10, 1002, Edit::PlaceMedia { media: Box::new(media.clone()) });
    append(&mut source, 10, 1002, Edit::SetMediaVisible { media: media.id, visible: false });
    let manifest = ArchiveManifest { schema: 1, world: source.clone(), assets: frames.clone() };
    let decoded = ArchiveManifest::decode(&serde_json::to_vec(&manifest).unwrap()).unwrap();
    let imported = decoded.world.import_to(canvas(2), owner(900)).unwrap();
    let copied = imported.media.values().next().unwrap();
    assert_ne!(copied.media.id, media.id);
    assert!(!copied.visible);
    assert_eq!(copied.media.frames, frames);
    assert_eq!(copied.media.provenance, media.provenance);
    assert_eq!((copied.media.anchor, copied.media.offset, copied.media.width, copied.media.height), (media.anchor, media.offset, media.width, media.height));
    assert!(imported.replay_to(3).unwrap().media.values().next().unwrap().visible);
    assert_eq!(copied.media.frame_at(100).unwrap().0, 1);
    assert_eq!(source.media[&media.id].media, media);
    let mut duplicate = manifest.clone();
    duplicate.assets[1] = duplicate.assets[0].clone();
    assert!(duplicate.validate().is_err());
    let mut mismatch = manifest;
    mismatch.assets[0].width += 1;
    assert!(mismatch.validate().is_err());
}
