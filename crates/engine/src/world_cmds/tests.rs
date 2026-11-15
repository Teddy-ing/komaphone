use super::*;
use photocraft_scene::shared::{AcceptedOp, WorldOp};
fn hex(n: u32) -> String {
    format!("{n:032x}")
}
fn profile(n: u32) -> OwnerId {
    OwnerId::new(&hex(n)).unwrap()
}
fn device(n: u32) -> DeviceId {
    DeviceId::new(&hex(n)).unwrap()
}
fn fixture() -> WorldState {
    WorldState::new(CanvasId::new(&hex(1)).unwrap(), profile(10), "Synthetic history".into()).unwrap()
}
fn add(world: &mut WorldState, writer: u32, author: u32, id: u32) {
    let stroke = Stroke {
        id: StrokeId::new(&hex(id)).unwrap(),
        canvas: world.canvas,
        owner: profile(author),
        revision: 0,
        anchor: Default::default(),
        points: vec![[0., 0.]],
        width: 4.,
        color: Srgba { rgba: [0., 0., 0., 1.] },
        erase: false,
    };
    apply(world, writer, author, Edit::AddStroke { stroke });
}
fn apply(world: &mut WorldState, writer: u32, author: u32, action: Edit) {
    world
        .apply(&AcceptedOp {
            revision: world.revision + 1,
            op: WorldOp {
                id: OpId::new(&hex(1000 + world.revision as u32)).unwrap(),
                canvas: world.canvas,
                author: profile(author),
                device: device(writer),
                sequence: world.next_sequence(device(writer)).unwrap(),
                authority_epoch: 1,
                action,
            },
        })
        .unwrap();
}
fn target(world: &WorldState, redo: bool) -> Option<StrokeId> {
    match history_target(world, profile(10), device(20), redo) {
        Some(Target::Stroke(id)) => Some(id),
        _ => None,
    }
}

#[test]
fn undo_redo_selection_is_device_local_and_new_drawing_invalidates_old_redo() {
    let mut world = fixture();
    add(&mut world, 20, 10, 100);
    add(&mut world, 20, 10, 101);
    add(&mut world, 21, 10, 102);
    add(&mut world, 22, 11, 103);
    let first = StrokeId::new(&hex(100)).unwrap();
    let second = StrokeId::new(&hex(101)).unwrap();
    assert_eq!(target(&world, false), Some(second));
    apply(&mut world, 20, 10, Edit::SetStrokeVisible { stroke: second, visible: false });
    assert_eq!(target(&world, true), Some(second));
    assert_eq!(target(&world, false), Some(first));
    apply(&mut world, 20, 10, Edit::SetStrokeVisible { stroke: first, visible: false });
    assert_eq!(target(&world, true), Some(first));
    apply(&mut world, 20, 10, Edit::SetStrokeVisible { stroke: first, visible: true });
    assert_eq!(target(&world, true), Some(second));
    assert_eq!(target(&world, false), Some(first));
    add(&mut world, 20, 10, 104);
    assert!(target(&world, true).is_none());
    assert!(world.strokes.get(&StrokeId::new(&hex(102)).unwrap()).unwrap().visible);
    assert!(world.strokes.get(&StrokeId::new(&hex(103)).unwrap()).unwrap().visible);
}
#[test]
fn parameter_preflight_rejects_large_arrays_deep_nesting_and_unknown_fields() {
    assert!(parse::<Name>(&json!({"name":"ok","author":"spoof"})).is_err());
    assert!(parse::<Draw>(&json!({"points":vec![[0,0];photocraft_scene::MAX_POINTS+1]})).is_err());
    let mut deep = json!({});
    for _ in 0..26 {
        deep = json!([deep]);
    }
    assert!(parse::<Name>(&deep).is_err());
    assert!(parse::<Name>(&json!({"name":"x".repeat(8193)})).is_err());
}

#[test]
fn tour_cancel_survives_concurrent_deletion_of_the_playing_route() {
    use photocraft_scene::shared::{Tour, TourId, TourStop, tours::TourPlayback};
    let world = fixture();
    let tour = Tour {
        id: TourId::new(&hex(500)).unwrap(),
        canvas: world.canvas,
        name: "Deleted route".into(),
        revision: 1,
        stops: vec![TourStop { camera: photocraft_scene::coordinates::home(world.canvas), travel_ms: 0, pause_ms: 1000, target: None }],
    };
    let mut session = Session::new();
    let world = std::sync::Arc::new(world);
    session.worlds.active = Some(crate::worlds::Canvas {
        info: wire::CanvasSummary {
            canvas: world.canvas,
            owner: world.owner,
            name: world.name.clone(),
            import_origin: None,
            initial_name: world.name.clone(),
            revision: 0,
            epoch: 1,
        },
        canonical: world.clone(),
        display: world.clone(),
        pending: 0,
        notice: None,
        viewer: crate::worlds::Viewer { camera: photocraft_scene::coordinates::home(world.canvas), bookmarks: vec![], review: None },
    });
    session.worlds.tour_playback = Some(TourPlayback::new(&tour).unwrap());
    // The route has already disappeared from the active scene. Cancellation is
    // a local playback action and must not require its old source to exist.
    assert!(world.tours.is_empty());
    let command = playback::specs().into_iter().find(|spec| spec.id == "komaphone.world.tour.control").unwrap();
    let result = (command.run)(&mut session, &json!({"state":"cancel"})).unwrap();
    assert_eq!(result, json!({"cancelled":true}));
    assert!(session.worlds.tour_playback.is_none());
}

#[test]
fn imported_stroke_copy_keeps_archive_credit_without_live_people_lookup() {
    let mut source = fixture();
    add(&mut source, 20, 10, 100);
    let mut imported = source.import_to(CanvasId::new(&hex(501)).unwrap(), profile(11)).unwrap();
    let stroke_id = *imported.strokes.keys().next().unwrap();
    let mut session = Session::new();
    let world = std::sync::Arc::new(imported.clone());
    session.worlds.active = Some(crate::worlds::Canvas {
        info: wire::CanvasSummary {
            canvas: world.canvas,
            owner: world.owner,
            name: world.name.clone(),
            initial_name: world.replay_to(0).unwrap().name,
            import_origin: world.import_origin.clone(),
            revision: world.revision,
            epoch: 1,
        },
        canonical: world.clone(),
        display: world.clone(),
        pending: 0,
        notice: None,
        viewer: crate::worlds::Viewer { camera: photocraft_scene::coordinates::home(world.canvas), bookmarks: vec![], review: None },
    });
    assert!(session.worlds.people.is_empty());
    let copy = stroke_copy::specs().into_iter().find(|spec| spec.id == "komaphone.world.stroke.copy").unwrap();
    (copy.run)(&mut session, &json!({"stroke":stroke_id})).unwrap();
    let (frozen, id, credit) = session.worlds.stroke_copy_source.as_ref().unwrap();
    assert_eq!(credit.profile, source.owner);
    let action = frozen
        .copy_stroke(
            *id,
            credit.clone(),
            photocraft_scene::shared::StrokeCopyTarget {
                canvas: imported.canvas,
                id: StrokeId::new(&hex(502)).unwrap(),
                owner: imported.owner,
                anchor: Default::default(),
                offset: [10., 10.],
                scale: 1.,
            },
        )
        .unwrap();
    apply(&mut imported, 30, 11, action);
    let pasted = imported.strokes.get(&StrokeId::new(&hex(502)).unwrap()).unwrap();
    assert_eq!(pasted.stroke.owner, profile(11));
    assert_eq!(pasted.provenance.as_ref().unwrap().author.profile, source.owner);
    assert_eq!(world.revision, 1);
}
