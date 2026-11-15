use std::collections::BTreeSet;

use photocraft_scene::shared::{
    review::ReviewState,
    tours::{ExportSettings, FrozenTour, TourPlayback},
};
use photocraft_scene::{Camera, CanvasId, OwnerId, RegionKey, Srgba, Stroke, StrokeId, coordinates, shared::*};

fn hex(n: u128) -> String {
    format!("{n:032x}")
}
fn canvas() -> CanvasId {
    CanvasId::new(&hex(1)).unwrap()
}
fn author(n: u128) -> OwnerId {
    OwnerId::new(&hex(n + 100)).unwrap()
}
fn device(n: u128) -> DeviceId {
    DeviceId::new(&hex(n + 200)).unwrap()
}
fn stroke_id(n: u128) -> StrokeId {
    StrokeId::new(&hex(n + 300)).unwrap()
}
fn world() -> WorldState {
    WorldState::new(canvas(), author(0), "Shared drawing".into()).unwrap()
}
fn stroke(n: u128, owner: OwnerId) -> Stroke {
    Stroke {
        id: stroke_id(n),
        canvas: canvas(),
        owner,
        revision: 0,
        anchor: RegionKey::default(),
        points: vec![[0.0, 0.0], [12.0, 12.0]],
        width: 5.0,
        color: Srgba { rgba: [0.2, 0.3, 0.4, 1.0] },
        erase: false,
    }
}
fn accepted(revision: u64, writer: u128, sequence: u64, action: Edit) -> AcceptedOp {
    AcceptedOp {
        revision,
        op: WorldOp {
            id: OpId::new(&hex(1000 + u128::from(revision))).unwrap(),
            canvas: canvas(),
            author: author(writer),
            device: device(writer),
            sequence,
            authority_epoch: 1,
            action,
        },
    }
}
fn add(revision: u64, writer: u128, sequence: u64) -> AcceptedOp {
    accepted(revision, writer, sequence, Edit::AddStroke { stroke: stroke(u128::from(revision), author(writer)) })
}
fn route() -> Tour {
    let home = coordinates::home(canvas());
    let near = coordinates::target(canvas(), coordinates::point(&RegionKey::default(), [400.0, 400.0]).unwrap(), 6, 1.0).unwrap();
    Tour {
        id: TourId::new(&hex(999)).unwrap(),
        canvas: canvas(),
        name: "Tiny house".into(),
        revision: 0,
        stops: vec![
            TourStop { camera: home, travel_ms: 0, pause_ms: 1000, target: None },
            TourStop { camera: near, travel_ms: 3000, pause_ms: 1000, target: None },
        ],
    }
}

#[test]
fn independent_stroke_copy_preserves_artist_and_old_hashes_and_replays_all_visibility_changes() {
    use photocraft_scene::shared::media::Credit;
    let mut original = world();
    original.apply(&add(1, 1, 1)).unwrap();
    let before = original.canonical_hash().unwrap();
    let source = original.strokes.get(&stroke_id(1)).unwrap();
    let legacy = serde_json::to_string(source).unwrap();
    assert!(!legacy.contains("provenance"));
    assert_eq!(serde_json::from_str::<SharedStroke>(&legacy).unwrap(), *source);
    let target = StrokeCopyTarget {
        canvas: canvas(),
        id: stroke_id(2),
        owner: author(2),
        anchor: RegionKey { depth: 6, ..Default::default() },
        offset: [10., 20.],
        scale: 2.,
    };
    let copied = stroke_copy(source, canvas(), Credit { profile: author(1), label: "Original artist".into() }, target.clone()).unwrap();
    let mut destination = world();
    destination.apply(&accepted(1, 2, 1, copied)).unwrap();
    let copied = destination.strokes.get(&stroke_id(2)).unwrap();
    assert_eq!(copied.stroke.points, [[10., 20.], [34., 44.]]);
    assert_eq!(copied.stroke.width, 10.);
    assert_eq!(copied.provenance.as_ref().unwrap().author.profile, author(1));
    let mut again = target.clone();
    again.id = stroke_id(3);
    again.owner = author(3);
    again.scale = 1.;
    again.offset = [0., 0.];
    let copied_again = stroke_copy(copied, canvas(), Credit { profile: author(2), label: "Remixer".into() }, again).unwrap();
    destination.apply(&accepted(2, 3, 1, copied_again)).unwrap();
    assert_eq!(destination.strokes.get(&stroke_id(3)).unwrap().provenance.as_ref().unwrap().author.label, "Original artist");
    destination.apply(&accepted(3, 2, 2, Edit::SetStrokeVisible { stroke: stroke_id(2), visible: false })).unwrap();
    destination.validate().unwrap();
    let archive = serde_json::to_vec(&destination).unwrap();
    let reopened: WorldState = serde_json::from_slice(&archive).unwrap();
    reopened.validate().unwrap();
    assert_eq!(reopened, destination);
    let copied_world = destination.copy_to(CanvasId::new(&hex(99)).unwrap()).unwrap();
    assert_eq!(copied_world.strokes.get(&stroke_id(3)).unwrap().provenance.as_ref().unwrap().canvas, canvas());
    assert_eq!(original.canonical_hash().unwrap(), before);
    let mut hidden = source.clone();
    hidden.visible = false;
    assert!(stroke_copy(&hidden, canvas(), Credit { profile: author(1), label: "Original artist".into() }, target.clone()).is_err());
    for scale in [f64::NAN, f64::INFINITY, -1., 0., 1_000_001.] {
        let mut bad = target.clone();
        bad.scale = scale;
        assert!(stroke_copy(source, canvas(), Credit { profile: author(1), label: "Original artist".into() }, bad).is_err());
    }
}

#[test]
fn independent_canvas_copy_keeps_history_ids_authors_and_tour_targets_without_mutating_source() {
    let mut source = world();
    source.apply(&add(1, 0, 1)).unwrap();
    source.apply(&add(2, 1, 1)).unwrap();
    source.apply(&accepted(3, 0, 2, Edit::PutTour { tour: route(), expected_revision: 0 })).unwrap();
    source.apply(&accepted(4, 1, 2, Edit::SetStrokeVisible { stroke: stroke_id(2), visible: false })).unwrap();
    let before = source.clone();
    let destination = CanvasId::new(&hex(987)).unwrap();
    let copy = source.copy_to(destination).unwrap();
    copy.validate().unwrap();
    assert_eq!(source, before);
    assert_eq!(copy.owner, source.owner);
    assert_eq!(copy.revision, 4);
    assert_eq!(copy.next_sequence(device(0)), source.next_sequence(device(0)));
    assert_eq!(copy.next_sequence(device(1)), source.next_sequence(device(1)));
    assert_eq!(copy.strokes.keys().collect::<Vec<_>>(), source.strokes.keys().collect::<Vec<_>>());
    assert_eq!(copy.strokes[&stroke_id(2)].stroke.owner, author(1));
    assert!(!copy.strokes[&stroke_id(2)].visible);
    assert!(copy.tours.values().all(|tour| tour.canvas == destination && tour.stops.iter().all(|stop| stop.camera.canvas == destination)));
    assert!(source.copy_to(source.canvas).is_err());
}

#[test]
fn eight_writers_converge_after_gaps_duplicates_and_reordered_delivery() {
    let mut authority = world();
    let mut events = Vec::new();
    for sequence in 1..=3 {
        for writer in 0..8 {
            let event = add(authority.revision + 1, writer, sequence);
            authority.apply(&event).unwrap();
            events.push(event);
        }
    }
    let expected = authority.canonical_hash().unwrap();
    for rotation in 0..8 {
        let mut replica = world();
        for event in events.iter().cycle().skip(rotation).take(events.len()) {
            let before = replica.canonical_hash().unwrap();
            if replica.apply(event).is_err() {
                assert_eq!(replica.canonical_hash().unwrap(), before);
            }
        }
        // The transport refetches the missing canonical interval, including retries.
        for event in &events {
            replica.apply(event).unwrap();
            replica.apply(event).unwrap();
        }
        replica.validate().unwrap();
        assert_eq!(replica.canonical_hash().unwrap(), expected);
        assert_eq!(replica.strokes.len(), 24);
    }
}

#[test]
fn writer_gap_and_duplicate_id_content_reuse_never_change_state() {
    let mut state = world();
    let event = add(1, 0, 1);
    state.apply(&event).unwrap();
    let hash = state.canonical_hash().unwrap();
    let gap = add(2, 0, 3);
    assert_eq!(state.apply(&gap), Err(Error::Sequence { expected: 2 }));
    let mut reused = event.clone();
    reused.op.authority_epoch = 2;
    assert_eq!(state.apply(&reused), Err(Error::DuplicateMismatch));
    assert_eq!(state.canonical_hash().unwrap(), hash);
    assert_eq!(state.apply(&event).unwrap(), Applied::Duplicate);
}

#[test]
fn undo_and_eraser_stay_on_the_owning_device_layer() {
    let mut state = world();
    state.apply(&add(1, 0, 1)).unwrap();
    state.apply(&add(2, 1, 1)).unwrap();
    let b = state.strokes.get(&stroke_id(2)).unwrap().clone();
    let mut erase = stroke(3, author(0));
    erase.erase = true;
    state.apply(&accepted(3, 0, 2, Edit::AddStroke { stroke: erase })).unwrap();
    state.apply(&accepted(4, 0, 3, Edit::SetStrokeVisible { stroke: stroke_id(1), visible: false })).unwrap();
    assert_eq!(state.strokes.get(&stroke_id(2)).unwrap(), &b);
    let before = state.canonical_hash().unwrap();
    assert_eq!(state.apply(&accepted(5, 0, 4, Edit::SetStrokeVisible { stroke: stroke_id(2), visible: false })), Err(Error::Permission));
    assert_eq!(state.canonical_hash().unwrap(), before);
    // A second device of the same profile cannot destructively change its sibling.
    let mut other_device = accepted(5, 0, 1, Edit::SetStrokeVisible { stroke: stroke_id(1), visible: true });
    other_device.op.device = device(9);
    assert_eq!(state.apply(&other_device), Err(Error::Permission));
    state.apply(&accepted(5, 0, 4, Edit::SetStrokeVisible { stroke: stroke_id(1), visible: true })).unwrap();
    assert!(state.strokes.get(&stroke_id(1)).unwrap().visible);
}

#[test]
fn device_identity_cannot_switch_authors() {
    let mut state = world();
    state.apply(&add(1, 0, 1)).unwrap();
    let mut impostor = add(2, 1, 2);
    impostor.op.device = device(0);
    assert_eq!(state.apply(&impostor), Err(Error::Permission));
    assert_eq!(state.revision, 1);
}

#[test]
fn owner_metadata_compare_and_set_preserves_conflicting_proposal() {
    let mut state = world();
    state.apply(&accepted(1, 0, 1, Edit::Rename { name: "First".into(), expected_revision: 0 })).unwrap();
    let proposed = accepted(2, 0, 2, Edit::Rename { name: "Offline alternative".into(), expected_revision: 0 });
    assert_eq!(state.apply(&proposed), Err(Error::Conflict { current: 1 }));
    assert_eq!(state.name, "First");
    assert_eq!(proposed.op.action, Edit::Rename { name: "Offline alternative".into(), expected_revision: 0 });
    assert_eq!(state.next_sequence(device(0)).unwrap(), 2);
    assert_eq!(state.apply(&accepted(2, 1, 1, Edit::Rename { name: "Editor rename".into(), expected_revision: 1 })), Err(Error::Permission));
}

#[test]
fn deserialized_checkpoint_must_match_all_accepted_history() {
    let mut state = world();
    state.apply(&add(1, 0, 1)).unwrap();
    state.apply(&add(2, 1, 1)).unwrap();
    let json = serde_json::to_string(&state).unwrap();
    let restored: WorldState = serde_json::from_str(&json).unwrap();
    restored.validate().unwrap();
    assert_eq!(restored.canonical_hash().unwrap(), state.canonical_hash().unwrap());
    let mut corrupted = restored;
    corrupted.strokes.get_mut(&stroke_id(2)).unwrap().visible = false;
    assert!(corrupted.validate().is_err());
    corrupted.schema = 99;
    assert_eq!(corrupted.validate(), Err(Error::Schema));
}

#[test]
fn bounds_and_hostile_numbers_fail_without_mutating_acknowledged_art() {
    let limits = Limits { max_operations: 4, max_strokes: 2, max_points: 4, max_tours: 1, ..Limits::default() };
    let mut state = WorldState::with_limits(canvas(), author(0), "Bounded".into(), limits).unwrap();
    state.apply(&add(1, 0, 1)).unwrap();
    state.apply(&add(2, 1, 1)).unwrap();
    let before = state.canonical_hash().unwrap();
    assert!(matches!(state.apply(&add(3, 2, 1)), Err(Error::Limit(_))));
    for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX] {
        let mut body = stroke(5, author(0));
        body.points[0][0] = number;
        assert!(state.apply(&accepted(3, 0, 2, Edit::AddStroke { stroke: body })).is_err());
    }
    assert_eq!(state.canonical_hash().unwrap(), before);
    assert!(state.events_after(u64::MAX, 1).is_err());
    assert!(state.events_after(0, usize::MAX).is_err());
    assert!(state.replay_to(u64::MAX).is_err());
}

#[test]
fn wrong_canvas_and_owner_cannot_be_written_in_a_stroke() {
    let mut state = world();
    let mut bad = add(1, 0, 1);
    bad.op.canvas = CanvasId::new(&hex(2)).unwrap();
    assert!(state.apply(&bad).is_err());
    let mut bad = stroke(2, author(9));
    bad.canvas = CanvasId::new(&hex(2)).unwrap();
    assert!(state.apply(&accepted(1, 0, 1, Edit::AddStroke { stroke: bad })).is_err());
    assert_eq!(state.revision, 0);
}

#[test]
fn accepted_stroke_cannot_leave_the_bounded_world_coordinate_extent() {
    let mut state = world();
    let mut outside = stroke(9, author(0));
    outside.anchor.x = photocraft_scene::Wide(photocraft_scene::ROOT_TILE_LIMIT - 1);
    outside.points = vec![[4096.0, 0.0]];
    assert!(state.apply(&accepted(1, 0, 1, Edit::AddStroke { stroke: outside })).is_err());
    assert_eq!(state.revision, 0);
}

#[test]
fn tour_targets_reach_six_level_depth_with_identical_preview_export_path() {
    let tour = route();
    assert_eq!(tour.frame_at(0).unwrap().camera, tour.stops[0].camera);
    assert!(tour.frame_at(2500).unwrap().travelling);
    assert_eq!(tour.frame_at(5000).unwrap().camera, tour.stops[1].camera);
    let mut playback = TourPlayback::new(&tour).unwrap();
    playback.advance(&tour, 1500).unwrap();
    playback.playing = false;
    let paused = playback.advance(&tour, 1000).unwrap();
    assert_eq!(paused, tour.frame_at(1500).unwrap());
    let restored: TourPlayback = serde_json::from_str(&serde_json::to_string(&playback).unwrap()).unwrap();
    assert_eq!(restored, playback);
    assert_eq!(playback.scrub(&tour, 2500).unwrap(), tour.frame_at(2500).unwrap());
    playback.cancel();
    assert_eq!(playback.elapsed_ms, 0);
    assert!(!playback.playing);
}

#[test]
fn tour_writes_are_cas_and_deleted_ids_cannot_be_recreated_by_stale_retries() {
    let mut state = world();
    let tour = route();
    state.apply(&accepted(1, 0, 1, Edit::PutTour { tour: tour.clone(), expected_revision: 0 })).unwrap();
    let failed = accepted(2, 1, 1, Edit::PutTour { tour: tour.clone(), expected_revision: 0 });
    assert_eq!(state.apply(&failed), Err(Error::Conflict { current: 1 }));
    state.apply(&accepted(2, 1, 1, Edit::RemoveTour { tour: tour.id, expected_revision: 1 })).unwrap();
    assert_eq!(state.apply(&accepted(3, 0, 2, Edit::PutTour { tour, expected_revision: 0 })), Err(Error::Conflict { current: 2 }));
    assert!(state.tours.is_empty());
    state.validate().unwrap();
}

#[test]
fn frozen_tour_is_independent_of_concurrent_art_and_route_edits() {
    let mut state = world();
    let tour = route();
    state.apply(&accepted(1, 0, 1, Edit::PutTour { tour: tour.clone(), expected_revision: 0 })).unwrap();
    let frozen = FrozenTour::new(&state, tour.id, ExportSettings { width: 640, height: 480, fps: 6 }).unwrap();
    assert_eq!(frozen.frame_count(), 30);
    state.apply(&add(2, 1, 1)).unwrap();
    let mut changed = tour.clone();
    changed.stops.reverse();
    changed.stops[0].travel_ms = 0;
    changed.stops[1].travel_ms = 3000;
    state.apply(&accepted(3, 0, 2, Edit::PutTour { tour: changed, expected_revision: 1 })).unwrap();
    assert_eq!(frozen.world().revision, 1);
    assert_eq!(frozen.scene_hash(), frozen.world().canonical_hash().unwrap());
    assert_eq!(frozen.frame(29).unwrap().camera, tour.stops[1].camera);
    for i in 0..29 {
        assert_eq!(frozen.frame(i).unwrap(), tour.frame_at(u64::from(i) * 1000 / 6).unwrap());
    }
    assert!(frozen.frame(30).is_err());
    assert!(FrozenTour::new(&state, tour.id, ExportSettings { width: 640, height: 480, fps: 0 }).is_err());
}

#[test]
fn missing_tour_stop_requires_explicit_skip_without_changing_saved_route() {
    let mut state = world();
    state.apply(&add(1, 0, 1)).unwrap();
    let mut tour = route();
    tour.stops[1].target = Some(stroke_id(1));
    state.apply(&accepted(2, 0, 2, Edit::PutTour { tour: tour.clone(), expected_revision: 0 })).unwrap();
    state.apply(&accepted(3, 0, 3, Edit::SetStrokeVisible { stroke: stroke_id(1), visible: false })).unwrap();
    assert_eq!(tour.missing_stops(&state).unwrap(), vec![1]);
    assert!(FrozenTour::new(&state, tour.id, ExportSettings { width: 640, height: 480, fps: 6 }).is_err());
    let repaired = tour.skipping_missing(&state).unwrap();
    assert_eq!(repaired.stops.len(), 1);
    assert_eq!(state.tours.get(&tour.id).unwrap().stops.len(), 2);
}

#[test]
fn tour_rejects_foreign_world_and_excessive_export_resources() {
    let mut tour = route();
    let mut camera: Camera = tour.stops[1].camera.clone();
    camera.canvas = CanvasId::new(&hex(2)).unwrap();
    tour.stops[1].camera = camera;
    assert!(tour.validate().is_err());
    let tour = route();
    for settings in
        [ExportSettings { width: u32::MAX, height: 1, fps: 1 }, ExportSettings { width: 1, height: 1, fps: 0 }, ExportSettings { width: 1, height: 0, fps: 6 }]
    {
        assert!(settings.frame_count(&tour).is_err());
    }
    let mut long = tour;
    long.stops[1].pause_ms = u32::MAX;
    assert!(long.validate().is_err());
}

#[test]
fn catchup_fixed_end_preserves_new_arrivals_and_restart_playhead() {
    let mut state = world();
    state.apply(&add(1, 0, 1)).unwrap();
    let mut review = ReviewState::first_visit(canvas(), author(0), 1).unwrap();
    state.apply(&add(2, 1, 1)).unwrap();
    review.begin(state.revision).unwrap();
    review.scrub(2).unwrap();
    let mut restored: ReviewState = serde_json::from_str(&serde_json::to_string(&review).unwrap()).unwrap();
    state.apply(&add(3, 1, 2)).unwrap();
    assert_eq!(restored.unseen_revisions(state.revision).unwrap(), 2);
    assert_eq!(restored.interval.as_ref().unwrap().end_inclusive, 2);
    assert_eq!(restored.scene_at_position(&state).unwrap().revision, 2);
    restored.mark_seen(state.revision).unwrap();
    assert_eq!(restored.reviewed_revision, 2);
    assert_eq!(restored.unseen_revisions(state.revision).unwrap(), 1);
}

#[test]
fn catchup_filters_highlights_but_replays_every_causal_event() {
    let mut state = world();
    state.apply(&add(1, 0, 1)).unwrap();
    state.apply(&add(2, 1, 1)).unwrap();
    state.apply(&accepted(3, 0, 2, Edit::SetStrokeVisible { stroke: stroke_id(1), visible: false })).unwrap();
    let mut review = ReviewState::first_visit(canvas(), author(1), 0).unwrap();
    review.begin(3).unwrap();
    let page = review.changes(&state, 0, 256, Some(&BTreeSet::from([author(0)])), false).unwrap();
    assert_eq!(page.changes.iter().map(|c| c.revision).collect::<Vec<_>>(), vec![1, 3]);
    assert!(page.changes[1].region.is_some());
    assert_eq!(page.scanned_through, 3);
    review.scrub(3).unwrap();
    let replay = review.scene_at_position(&state).unwrap();
    assert_eq!(replay.canonical_hash().unwrap(), state.canonical_hash().unwrap());
    assert!(!replay.strokes.get(&stroke_id(1)).unwrap().visible);
    assert!(replay.strokes.get(&stroke_id(2)).unwrap().visible);
}

#[test]
fn review_playback_cancel_and_background_sync_never_advance_seen_watermark() {
    let mut review = ReviewState::first_visit(canvas(), author(0), 0).unwrap();
    review.begin(10).unwrap();
    review.interval.as_mut().unwrap().playing = true;
    review.interval.as_mut().unwrap().speed = 2.0;
    review.advance(500, 12).unwrap();
    assert_eq!(review.interval.as_ref().unwrap().position, 4);
    assert_eq!(review.reviewed_revision, 0);
    review.cancel();
    assert_eq!(review.reviewed_revision, 0);
    review.begin(12).unwrap();
    review.interval.as_mut().unwrap().speed = f64::NAN;
    assert!(review.advance(u64::MAX, 12).is_err());
    assert!(review.mark_seen(12).is_err());
    assert_eq!(review.reviewed_revision, 0);
}

fn media() -> MediaPlacement {
    use photocraft_scene::shared::media::*;
    let frame = |n, duration_ms| MediaFrame { hash: AssetHash::new(&format!("{n:064x}")).unwrap(), byte_len: 1024, width: 128, height: 96, duration_ms };
    MediaPlacement {
        id: MediaId::new(&hex(5000)).unwrap(),
        canvas: canvas(),
        owner: author(0),
        anchor: RegionKey { depth: 6, ..RegionKey::default() },
        offset: [12.0, 40.0],
        width: 128.0,
        height: 96.0,
        frames: vec![frame(1, 100), frame(2, 250), frame(3, 150)],
        looping: true,
        provenance: Provenance {
            source: MediaSource::Gallery {
                entry: SourceId::new(&hex(2)).unwrap(),
                room: None,
                game: Some(SourceId::new(&hex(3)).unwrap()),
                chain: 0,
                frame: None,
            },
            source_revision: 42,
            artifact_hash: AssetHash::new(&format!("{:064x}", 9)).unwrap(),
            authors: vec![Credit { profile: author(1), label: "First artist".into() }, Credit { profile: author(2), label: "Second artist".into() }],
        },
    }
}

#[test]
fn animated_remix_keeps_frames_timing_provenance_and_deep_placement() {
    let original = media();
    assert_eq!(original.duration_ms().unwrap(), 500);
    for (time, expected) in [(0, 0), (99, 0), (100, 1), (349, 1), (350, 2), (499, 2), (500, 0), (u64::MAX, 1)] {
        assert_eq!(original.frame_at(time).unwrap().0, expected);
    }
    let mut state = world();
    let op = accepted(1, 0, 1, Edit::PlaceMedia { media: Box::new(original.clone()) });
    state.apply(&op).unwrap();
    assert_eq!(state.apply(&op).unwrap(), Applied::Duplicate);
    assert_eq!(state.media.len(), 1);
    let restored: WorldState = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
    restored.validate().unwrap();
    assert_eq!(restored.media.get(&original.id).unwrap().media, original);
    assert_eq!(state.change(&op).unwrap().region.unwrap().depth, 6);
    assert_eq!(state.canonical_hash().unwrap(), restored.canonical_hash().unwrap());
}

#[test]
fn media_delete_and_undo_are_owned_and_preserve_original_history_and_baseline() {
    let mut state = world();
    let original = media();
    state.apply(&accepted(1, 0, 1, Edit::PlaceMedia { media: Box::new(original.clone()) })).unwrap();
    state.apply(&add(2, 1, 1)).unwrap();
    let before = state.canonical_hash().unwrap();
    let denied = accepted(3, 1, 2, Edit::SetMediaVisible { media: original.id, visible: false });
    assert_eq!(state.apply(&denied), Err(Error::Permission));
    assert_eq!(state.canonical_hash().unwrap(), before);
    let erase = accepted(3, 0, 2, Edit::SetMediaVisible { media: original.id, visible: false });
    state.apply(&erase).unwrap();
    assert!(!state.media.get(&original.id).unwrap().visible);
    assert_eq!(state.media.get(&original.id).unwrap().media, original);
    assert_eq!(state.change(&erase).unwrap().region, Some(original.anchor));
    assert!(state.replay_to(1).unwrap().media.get(&original.id).unwrap().visible);
    state.apply(&accepted(4, 0, 3, Edit::SetMediaVisible { media: original.id, visible: true })).unwrap();
    assert!(state.media.get(&original.id).unwrap().visible);
    assert!(state.strokes.get(&stroke_id(2)).unwrap().visible);
}

#[test]
fn media_copy_has_independent_identity_while_preserving_source_credits() {
    let mut state = world();
    let original = media();
    state.apply(&accepted(1, 0, 1, Edit::PlaceMedia { media: Box::new(original.clone()) })).unwrap();
    let mut copy = original.clone();
    copy.id = MediaId::new(&hex(5001)).unwrap();
    copy.owner = author(1);
    copy.provenance.source = photocraft_scene::shared::media::MediaSource::Canvas { canvas: canvas(), media: original.id };
    state.apply(&accepted(2, 1, 1, Edit::PlaceMedia { media: Box::new(copy.clone()) })).unwrap();
    state.apply(&accepted(3, 0, 2, Edit::SetMediaVisible { media: original.id, visible: false })).unwrap();
    assert!(state.media.get(&copy.id).unwrap().visible);
    assert_eq!(state.media.get(&copy.id).unwrap().media.frames, original.frames);
    assert_eq!(state.media.get(&copy.id).unwrap().media.provenance.authors, original.provenance.authors);
}

#[test]
fn media_bounds_and_explicit_still_semantics_reject_malformed_inputs() {
    use photocraft_scene::shared::media::*;
    let original = media();
    let mut bad = original.clone();
    bad.width = f64::NAN;
    assert!(bad.validate().is_err());
    let mut bad = original.clone();
    bad.frames[0].duration_ms = 0;
    assert!(bad.frame_at(u64::MAX).is_err());
    let mut bad = original.clone();
    bad.frames[0].byte_len = u64::MAX;
    assert!(bad.validate().is_err());
    let mut bad = original.clone();
    bad.frames[1].width += 1;
    assert!(bad.validate().is_err());
    let mut bad = original.clone();
    bad.provenance.authors.push(bad.provenance.authors[0].clone());
    assert!(bad.validate().is_err());
    let mut still = original.clone();
    if let MediaSource::Gallery { frame, .. } = &mut still.provenance.source {
        *frame = Some(1);
    }
    assert!(still.validate().is_err());
    still.frames = vec![original.frames[1].clone()];
    still.looping = false;
    still.validate().unwrap();
    assert_eq!(still.frame_at(u64::MAX).unwrap().0, 0);
    assert!(AssetHash::new("a").is_err());
    assert!(serde_json::from_str::<AssetHash>(&format!("\"{}\"", "G".repeat(64))).is_err());
}

#[test]
fn media_preserves_legacy_party_credit_labels_without_renaming() {
    let mut original = media();
    original.provenance.authors[0].label = "Synthetic\nArtist\tOne".into();
    original.validate().unwrap();
    let encoded = serde_json::to_vec(&original).unwrap();
    let decoded: photocraft_scene::shared::MediaPlacement = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded.provenance.authors, original.provenance.authors);
    for invalid in ["\0", "\r", "\n\t"] {
        original.provenance.authors[0].label = invalid.into();
        assert!(original.validate().is_err());
    }
}

#[test]
fn eight_writer_replay_workload_reports_bounded_checkpoint_cost() {
    let mut state = world();
    let start = std::time::Instant::now();
    for sequence in 1..=256 {
        for writer in 0..8 {
            state.apply(&add(state.revision + 1, writer, sequence)).unwrap();
        }
    }
    let apply_us = start.elapsed().as_micros();
    let start = std::time::Instant::now();
    let bytes = serde_json::to_vec(&state).unwrap();
    let encode_us = start.elapsed().as_micros();
    let start = std::time::Instant::now();
    let restored: WorldState = serde_json::from_slice(&bytes).unwrap();
    restored.validate().unwrap();
    let restore_validate_us = start.elapsed().as_micros();
    assert_eq!(state.canonical_hash().unwrap(), restored.canonical_hash().unwrap());
    assert_eq!(state.operation_count(), 2048);
    assert_eq!(state.point_count(), 4096);
    println!(
        "K6_REDUCER_MEASUREMENT operations=2048 writers=8 points=4096 bytes={} apply_us={} encode_us={} restore_validate_us={}",
        bytes.len(),
        apply_us,
        encode_us,
        restore_validate_us
    );
}
