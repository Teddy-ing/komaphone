//! Mode-specific engine checks. Room projections here are synthetic; the native
//! multi-client harness separately proves service delivery and actual UI behavior.
#![cfg(not(target_arch = "wasm32"))]

use image::ImageEncoder;
use photocraft_doc::{Color, ColorMode, Document, LayerContent, SampleType, Size};
use photocraft_engine::Session;
use photocraft_engine::rooms::animation::{can_copy_previous, copy_previous_frame, previous_reference, reveal_chain};
use photocraft_party::room::{Assignment, AssignmentView, MemberView, Phase, Reference, RevealedEntry, RoomView};
use photocraft_party::{AssetHash, AssetRef, AssignmentId, DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, RoomId};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;

fn person(n: u8) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{:032x}", n + 1)).unwrap(),
        device: DeviceId::new(&format!("{:032x}", n + 11)).unwrap(),
        display_name: format!("Artist {n}"),
    }
}
fn png(width: u32, height: u32) -> Vec<u8> {
    png_color(width, height, [188, 188, 188, 255])
}
fn png_color(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
    let mut encoded = Vec::new();
    image::codecs::png::PngEncoder::new(&mut encoded)
        .write_image(&color.repeat(width as usize * height as usize), width, height, image::ExtendedColorType::Rgba8)
        .unwrap();
    encoded
}

#[test]
fn transparent_initial_copy_hides_only_pristine_background_and_keeps_edits() {
    let bytes = png_color(1536, 1024, [188, 188, 188, 64]);
    let root = tempfile::tempdir().unwrap();
    let mut session = session(root.path(), &bytes);
    let background = session.active().unwrap().doc.layers[0].id;
    copy_previous_frame(&mut session, &bytes).unwrap();
    assert!(!session.active().unwrap().doc.layer(background).unwrap().visible);
    let composite = photocraft_compose::flatten(&session.active().unwrap().doc);
    assert!((composite.px[0][3] - 64.0 / 255.0).abs() < 0.001);
    assert!(session.undo());
    assert!(session.active().unwrap().doc.layer(background).unwrap().visible);
    // A completed edit to the existing drawing prevents the fresh-baseline shortcut.
    session.execute("layer.renameLayer", serde_json::json!({"name":"My existing art"})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    copy_previous_frame(&mut session, &bytes).unwrap();
    assert!(session.active().unwrap().doc.layer(background).unwrap().visible);
    assert_eq!(session.active().unwrap().doc.layer(background).unwrap().name, "My existing art");
}
fn asset(bytes: &[u8]) -> AssetRef {
    AssetRef { hash: AssetHash::new(&format!("{:x}", Sha256::digest(bytes))).unwrap(), bytes: bytes.len() as u64 }
}
fn view(players: u8, rotations: u8) -> RoomView {
    RoomView {
        id: RoomId::new(&format!("{:032x}", 50)).unwrap(),
        revision: 20,
        epoch: 3,
        phase: Phase::Drawing,
        round: 1,
        mode: Mode::Animation { rotations },
        timer_ms: None,
        moderator: person(0).id,
        members: (0..players).map(|n| MemberView { participant: person(n), connected: true, ready: true }).collect(),
        waiting: Vec::new(),
        paused: None,
        deadline_ms: None,
        assignment: Some(AssignmentView {
            assignment: Assignment {
                id: AssignmentId { game: GameId::new(&format!("{:032x}", 100)).unwrap(), chain: 1, round: 1 },
                player: person(0).id,
                project: ProjectId::new(&format!("{:032x}", 200)).unwrap(),
            },
            submitted: false,
            reference: None,
        }),
        reveal: Vec::new(),
    }
}
fn session(root: &std::path::Path, bytes: &[u8]) -> Session {
    let mut session = Session::new();
    session.enable_local_projects(root.join("drafts.sqlite3"), Default::default()).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let mut room = view(2, 2);
    room.assignment.as_mut().unwrap().reference = Some(Reference::Animation { image: asset(bytes), note: "Keep the moon moving".into() });
    session.rooms.status.room = Some(room);
    session.rooms.status.profile = Some(person(0));
    session.open_room_draft().unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    session
}

#[test]
fn explicit_copy_is_independent_undoable_and_excludes_note_pixels() {
    let root = tempfile::tempdir().unwrap();
    let bytes = png(1536, 1024);
    let original = bytes.clone();
    let mut session = session(root.path(), &bytes);
    let blank = session.active().unwrap().doc.clone();
    let id = copy_previous_frame(&mut session, &bytes).unwrap();
    let copied = &session.active().unwrap().doc;
    assert_eq!(copied.layers.len(), 2);
    assert_ne!(id, blank.layers[0].id);
    assert_eq!(&bytes, &original);
    let layer = copied.layer(id).unwrap();
    assert!(layer.xmp.as_ref().unwrap().contains(asset(&bytes).hash.as_str()));
    assert!(!layer.xmp.as_ref().unwrap().contains("moon"));
    let LayerContent::Raster(surface) = &layer.content else { panic!("Copied pixels must be editable") };
    for (x, y) in [(0, 0), (768, 512), (1535, 1023)] {
        let samples = surface.pixel(x, y);
        assert!((samples[0] - 188.0 / 255.0).abs() < 0.001);
        assert_eq!(samples[3], 1.0);
    }
    assert!(session.undo());
    assert_eq!(*session.active().unwrap().doc, *blank);
    assert!(session.redo());
    assert!(session.active().unwrap().doc.layer(id).is_some());
}

#[test]
fn copying_preserves_destination_profile_and_depth_and_converts_srgb_samples() {
    let bytes = png(1536, 1024);
    for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
        let root = tempfile::tempdir().unwrap();
        let mut session = session(root.path(), &bytes);
        let original_id = session.active().unwrap().doc.id;
        let mut linear = Document::with_background("Linear assignment", Size { width: 1536, height: 1024 }, ColorMode::Rgb, depth, Color::WHITE);
        linear.id = original_id;
        let profile = photocraft_cms::Builtin::LinearSrgb.profile().to_bytes();
        linear.icc_profile = Some(profile.clone());
        let state = session.active_mut().unwrap();
        state.active_layer = Some(linear.layers[0].id);
        state.doc = Arc::new(linear);
        let id = copy_previous_frame(&mut session, &bytes).unwrap();
        let document = &session.active().unwrap().doc;
        assert_eq!(document.depth, depth);
        assert_eq!(document.icc_profile.as_ref(), Some(&profile));
        let LayerContent::Raster(surface) = &document.layer(id).unwrap().content else { panic!("raster") };
        assert_eq!(surface.format().sample, depth);
        assert!((surface.pixel(0, 0)[0] - 0.502_886_5).abs() < 0.004, "{depth:?}");
    }
}

#[test]
fn wrong_asset_or_dimensions_and_forbidden_assignment_never_change_document() {
    let root = tempfile::tempdir().unwrap();
    let bytes = png(1536, 1024);
    let mut session = session(root.path(), &bytes);
    let before = session.active().unwrap().doc.clone();
    assert!(copy_previous_frame(&mut session, b"wrong image").is_err());
    let wrong_size = png(2, 2);
    session.rooms.status.room.as_mut().unwrap().assignment.as_mut().unwrap().reference =
        Some(Reference::Animation { image: asset(&wrong_size), note: String::new() });
    assert!(copy_previous_frame(&mut session, &wrong_size).is_err());
    assert_eq!(*session.active().unwrap().doc, *before);
    session.rooms.status.room.as_mut().unwrap().phase = Phase::Transfer;
    assert!(can_copy_previous(&session).is_err());
    session.rooms.status.room.as_mut().unwrap().phase = Phase::Drawing;
    session.rooms.status.room.as_mut().unwrap().assignment.as_mut().unwrap().submitted = true;
    assert!(can_copy_previous(&session).is_err());
    session.rooms.status.room.as_mut().unwrap().assignment.as_mut().unwrap().submitted = false;
    session.rooms.status.profile = Some(person(1));
    assert!(copy_previous_frame(&mut session, &wrong_size).is_err());
    assert_eq!(*session.active().unwrap().doc, *before);
}

#[test]
fn references_require_current_authorized_animation_assignment() {
    let mut room = view(2, 2);
    let actor = person(0).id;
    assert!(previous_reference(&room, actor).unwrap().is_none());
    room.assignment.as_mut().unwrap().reference = Some(Reference::Missing);
    assert!(previous_reference(&room, actor).unwrap().is_none());
    room.assignment.as_mut().unwrap().reference = Some(Reference::Animation { image: asset(b"fixture"), note: "Note".into() });
    assert_eq!(previous_reference(&room, actor).unwrap().unwrap().1, "Note");
    assert!(previous_reference(&room, person(1).id).is_err());
    room.assignment.as_mut().unwrap().reference =
        Some(Reference::Animation { image: asset(b"fixture"), note: "x".repeat(photocraft_party::MAX_NOTE_BYTES + 1) });
    assert!(previous_reference(&room, actor).is_err());
    room.assignment.as_mut().unwrap().reference = Some(Reference::Animation { image: asset(b"fixture"), note: "Note".into() });
    room.assignment.as_mut().unwrap().assignment.id.round = 0;
    assert!(previous_reference(&room, actor).is_err());
    room.assignment.as_mut().unwrap().assignment.id.round = 1;
    room.phase = Phase::Reveal;
    assert!(previous_reference(&room, actor).is_err());
    room.phase = Phase::Drawing;
    room.mode = Mode::ExquisiteCorpse;
    assert!(previous_reference(&room, actor).is_err());
}

#[test]
fn complete_chains_sort_cyclic_authors_and_keep_explicit_missing_slots() {
    for players in [2, 3] {
        for rotations in 1..=4 {
            let mut room = view(players, rotations);
            room.phase = Phase::Archived;
            let game = room.assignment.as_ref().unwrap().assignment.id.game;
            let rounds = room.mode.rounds(usize::from(players)).unwrap();
            for round in (0..rounds).rev() {
                for chain in (0..players).rev() {
                    room.reveal.push(RevealedEntry::Missing {
                        assignment: Assignment {
                            id: AssignmentId { game, chain, round },
                            player: person((chain + round) % players).id,
                            project: ProjectId::new(&format!("{:032x}", 300 + u16::from(round) * 3 + u16::from(chain))).unwrap(),
                        },
                        reason: "Participant skipped".into(),
                    });
                }
            }
            for chain in 0..players {
                assert_eq!(reveal_chain(&room, chain).unwrap().len(), usize::from(rounds));
            }
            assert!(reveal_chain(&room, players).is_err());
            room.phase = Phase::Drawing;
            assert!(reveal_chain(&room, 0).is_err(), "Even preloaded caches must not reveal early");
            room.phase = Phase::Reveal;
            let duplicate = room.reveal.last().unwrap().clone();
            room.reveal.push(duplicate);
            assert!(reveal_chain(&room, 0).is_err());
        }
    }
}
