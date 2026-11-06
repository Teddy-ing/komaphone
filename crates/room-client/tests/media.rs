//! Independent projection tests: presentation never speculates on hidden originals.
use photocraft_party::room::{Assignment, AssignmentView, MemberView, Phase, Reference, RevealedEntry, RoomView};
use photocraft_party::{
    AssetHash, AssetRef, AssignmentId, CheckpointId, CheckpointRef, DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, ReplayId, ReplayRef, RoomId,
};
use photocraft_room_client::media;

fn actor(number: u8) -> ParticipantId {
    ParticipantId::new(&format!("{number:032x}")).unwrap()
}
fn asset(number: u8) -> AssetRef {
    AssetRef { hash: AssetHash::new(&format!("{number:064x}")).unwrap(), bytes: 128 }
}
fn assigned(round: u8) -> Assignment {
    Assignment {
        id: AssignmentId { game: GameId::new(&format!("{:032x}", 1)).unwrap(), chain: 0, round },
        player: actor(round % 2 + 1),
        project: ProjectId::new(&format!("{:032x}", 1)).unwrap(),
    }
}
fn view(mode: Mode) -> RoomView {
    RoomView {
        id: RoomId::new(&format!("{:032x}", 1)).unwrap(),
        revision: 1,
        epoch: 2,
        phase: Phase::Drawing,
        round: 1,
        mode,
        timer_ms: None,
        moderator: actor(1),
        members: (1..=2)
            .map(|n| MemberView {
                participant: Participant { id: actor(n), device: DeviceId::new(&format!("{n:032x}")).unwrap(), display_name: format!("Player {n}") },
                connected: true,
                ready: true,
            })
            .collect(),
        waiting: vec![],
        paused: None,
        deadline_ms: None,
        assignment: Some(AssignmentView {
            assignment: Assignment { id: AssignmentId { chain: 1, ..assigned(1).id }, player: actor(1), ..assigned(1) },
            submitted: false,
            reference: None,
        }),
        reveal: vec![],
    }
}
fn reveal(round: u8, image: AssetRef) -> RevealedEntry {
    let checkpoint = CheckpointRef { id: CheckpointId::new(&format!("{:032x}", 5)).unwrap(), asset: asset(50) };
    RevealedEntry::Artwork {
        assignment: assigned(round),
        revision: 4,
        image,
        checkpoint,
        replay: Box::new(ReplayRef {
            id: ReplayId::new(&format!("{:032x}", 6)).unwrap(),
            manifest: Some(asset(60)),
            baseline: checkpoint,
            end_event: None,
            event_count: 0,
            duration_ms: 0,
        }),
        assets: vec![asset(50), asset(60), asset(70)],
    }
}

#[test]
fn active_corpse_requests_only_its_locked_derivative_never_reveal_or_replay_assets() {
    let mut view = view(Mode::ExquisiteCorpse);
    view.assignment.as_mut().unwrap().reference = Some(Reference::Seam { image: asset(1), width: 1024, height: 24 });
    view.reveal = vec![reveal(0, asset(10)), reveal(1, asset(11))];
    assert_eq!(media::images(&view, actor(1)), vec![asset(1)]);
    view.phase = Phase::Transfer;
    assert_eq!(media::images(&view, actor(1)), vec![asset(1)]);
    assert!(media::images(&view, actor(2)).is_empty(), "Another member cannot fetch this assigned strip");
    assert!(media::images(&view, actor(3)).is_empty(), "A waiting/unknown participant has no assignment");
}

#[test]
fn crossed_mode_full_segment_wrong_strip_and_stale_assignment_are_never_requested() {
    let mut corpse = view(Mode::ExquisiteCorpse);
    for reference in [
        Reference::Animation { image: asset(1), note: "full hidden segment".into() },
        Reference::Seam { image: asset(1), width: 1024, height: 512 },
        Reference::Seam { image: asset(1), width: 1023, height: 24 },
    ] {
        corpse.assignment.as_mut().unwrap().reference = Some(reference);
        assert!(media::images(&corpse, actor(1)).is_empty());
    }
    corpse.assignment.as_mut().unwrap().reference = Some(Reference::Seam { image: asset(1), width: 1024, height: 24 });
    corpse.assignment.as_mut().unwrap().assignment.id.round = 0;
    assert!(media::images(&corpse, actor(1)).is_empty());
    corpse.round = 0;
    assert!(media::images(&corpse, actor(1)).is_empty(), "A first round has no predecessor");
    let mut animation = view(Mode::Animation { rotations: 1 });
    animation.assignment.as_mut().unwrap().reference = Some(Reference::Seam { image: asset(1), width: 1024, height: 24 });
    assert!(media::images(&animation, actor(1)).is_empty());
}

#[test]
fn animation_receives_only_the_current_previous_frame_and_no_note_as_image() {
    let mut view = view(Mode::Animation { rotations: 2 });
    view.assignment.as_mut().unwrap().reference = Some(Reference::Animation { image: asset(2), note: "Move the dot left".into() });
    assert_eq!(media::images(&view, actor(1)), vec![asset(2)]);
    view.assignment.as_mut().unwrap().reference = Some(Reference::Missing);
    assert!(media::images(&view, actor(1)).is_empty());
    view.phase = Phase::Lobby;
    view.assignment.as_mut().unwrap().reference = Some(Reference::Animation { image: asset(2), note: String::new() });
    assert!(media::images(&view, actor(1)).is_empty());
}

#[test]
fn reveal_unlocks_only_final_images_and_deduplicates_without_fetching_native_assets() {
    let mut view = view(Mode::Animation { rotations: 2 });
    view.phase = Phase::Reveal;
    view.assignment.as_mut().unwrap().reference = Some(Reference::Animation { image: asset(2), note: "Old".into() });
    view.reveal = vec![reveal(0, asset(10)), reveal(1, asset(11)), reveal(2, asset(10))];
    assert_eq!(media::images(&view, actor(1)), vec![asset(10), asset(11)]);
    view.phase = Phase::Archived;
    assert_eq!(media::images(&view, actor(2)), vec![asset(10), asset(11)]);
    assert!(media::images(&view, actor(3)).is_empty());
}

#[test]
fn zero_and_oversized_images_are_rejected_before_any_download_allocation() {
    let mut view = view(Mode::Animation { rotations: 2 });
    for bytes in [0, media::MAX_IMAGE_BYTES + 1, u64::MAX] {
        view.assignment.as_mut().unwrap().reference = Some(Reference::Animation { image: AssetRef { bytes, ..asset(2) }, note: String::new() });
        assert!(media::images(&view, actor(1)).is_empty());
    }
}

#[test]
fn malformed_reveal_never_promotes_foreign_games_authors_or_assignment_coordinates() {
    let mut original = view(Mode::Animation { rotations: 2 });
    original.phase = Phase::Reveal;
    original.reveal = vec![reveal(0, asset(10)), reveal(1, asset(11))];
    for variant in 0..5 {
        let mut view = original.clone();
        if let RevealedEntry::Artwork { assignment, .. } = &mut view.reveal[1] {
            match variant {
                0 => assignment.id.game = GameId::new(&format!("{:032x}", 9)).unwrap(),
                1 => assignment.player = actor(3),
                2 => assignment.id.chain = 9,
                3 => assignment.id.round = 12,
                _ => assignment.id = assigned(0).id,
            }
        }
        assert!(media::images(&view, actor(1)).is_empty(), "Malformed reveal variant {variant}");
    }
}
