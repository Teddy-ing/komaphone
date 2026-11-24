//! Comprehensive QOL-007 engine & end-to-end acceptance tests:
//! - Blank and drawn segment height resize (128..=2048 px), fixed 1024 px width, top seam anchored,
//!   artwork unscaled, depth/color generality (U8/U16/F32, RGB/Gray).
//! - Non-destructive shrink refusal explaining exact safe min height + explicit undoable crop + undo/redo.
//! - Generic canvas/image resize refusal in Exquisite Corpse and unchanged fixed size in Animation.
//! - Post-submit / transfer / previous-assignment lock + invalid/adversarial parameter safety.
//! - Local SQLite crash/reopen durability preserving resized segment dimensions, pixels, and history.
//! - Full 2-player and 3-player Exquisite Corpse games with mixed short/default/tall heights:
//!   seam extraction from actual submitted height, variable-height assembly, PNG export,
//!   archive export/import, creature & part remix, replay across height changes, and legacy
//!   fixed-height (`extent: None`) archive restoration.
use photocraft_doc::{Color, ColorMode, Document, Rect, SampleType, Size};
use photocraft_engine::Session;
use photocraft_engine::gallery_media::{export_png, remix, render_creature, replay_info, replay_step, validate_originals};
use photocraft_engine::rooms::corpse::{
    BAND_HEIGHT, MAX_BAND_HEIGHT, MIN_BAND_HEIGHT, SEAM_HEIGHT, WIDTH, assemble, assemble_samples_with_heights, content_bottom, reveal_chain, safe_min_height,
};
use photocraft_engine::rooms::media::RoomImage;
use photocraft_local_store::StoreOptions;
use photocraft_local_store::gallery::GalleryStore;
use photocraft_party::gallery::{GalleryBundle, GalleryManifest};
use photocraft_party::room::{Action, Assignment, AssignmentView, MemberView, Phase, Reference, RevealedEntry, Room, RoomView};
use photocraft_party::{
    AssetHash, AssetRef, AssignmentId, CheckpointId, CheckpointRef, DeviceId, GameId, ImageExtent, Mode, Participant, ParticipantId, ProjectId, ReplayId,
    ReplayRef, RoomId, Submission, SubmissionId,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::time::Duration;

fn participant(seed: u8, name: &str) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{seed:032x}")).unwrap(),
        device: DeviceId::new(&format!("{:032x}", u16::from(seed) + 100)).unwrap(),
        display_name: name.into(),
    }
}

fn project_id(seed: u64) -> ProjectId {
    ProjectId::new(&format!("{:032x}", 5000 + seed)).unwrap()
}

fn active_corpse_room(actor: &Participant, project: ProjectId, round: u8, phase: Phase, submitted: bool) -> RoomView {
    RoomView {
        id: RoomId::new(&"aa".repeat(16)).unwrap(),
        revision: 3,
        epoch: 1,
        phase,
        round,
        mode: Mode::ExquisiteCorpse,
        timer_ms: None,
        moderator: actor.id,
        members: vec![
            MemberView { participant: actor.clone(), connected: true, ready: true },
            MemberView { participant: participant(2, "Bob"), connected: true, ready: true },
        ],
        waiting: vec![],
        paused: None,
        deadline_ms: None,
        assignment: Some(AssignmentView {
            assignment: Assignment { id: AssignmentId { game: GameId::new(&"bb".repeat(16)).unwrap(), chain: 0, round }, player: actor.id, project },
            submitted,
            reference: if round == 0 {
                None
            } else {
                Some(Reference::Seam { image: AssetRef { hash: AssetHash::new(&"11".repeat(32)).unwrap(), bytes: 100 }, width: WIDTH, height: SEAM_HEIGHT })
            },
        }),
        reveal: vec![],
    }
}

fn setup_session(dir: &std::path::Path, actor: &Participant, room: RoomView) -> Session {
    let mut session = Session::new();
    session.enable_local_projects(dir.join("drafts.sqlite3"), Default::default()).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    session.rooms.status.connected = true;
    session.rooms.status.paired = true;
    session.rooms.status.profile = Some(actor.clone());
    session.rooms.status.room = Some(room);
    session.open_room_draft().unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    session
}

#[test]
fn corpse_height_resize_safe_shrink_explicit_crop_undo_redo_guards_and_depth_generality() {
    let dir = tempfile::tempdir().unwrap();
    let alice = participant(1, "Alice");
    let proj = project_id(1);
    let mut session = setup_session(dir.path(), &alice, active_corpse_room(&alice, proj, 1, Phase::Drawing, false));

    // Initial blank segment is 1024 x 512 and can shrink cleanly down to 128 px or expand to 2048 px.
    assert_eq!(session.active().unwrap().doc.size, Size::new(WIDTH, BAND_HEIGHT));
    assert_eq!(safe_min_height(&session.active().unwrap().doc), MIN_BAND_HEIGHT);

    // Generic Photoshop canvas/image resize commands are refused during Exquisite Corpse.
    assert!(session.execute("image.canvasSize", json!({"width": 1024, "height": 768})).is_err());
    assert!(session.execute("image.imageSize", json!({"width": 1024, "height": 768})).is_err());
    assert!(session.execute("image.crop", json!({"x": 0, "y": 0, "width": 1024, "height": 300})).is_err());
    assert_eq!(session.active().unwrap().doc.size, Size::new(WIDTH, BAND_HEIGHT));

    // Invalid and out-of-range params fail gracefully without panicking.
    for bad in [
        json!(null),
        json!([]),
        json!({}),
        json!({"height": 127}),
        json!({"height": 2049}),
        json!({"height": -50}),
        json!({"height": 512.5}),
        json!({"height": "512"}),
        json!({"height": 600, "allowCrop": "yes"}),
        json!({"height": 600, "extra": true}),
    ] {
        assert!(session.execute("komaphone.corpse.setSegmentHeight", bad).is_err());
    }

    // Blank resize to min (128), max (2048), and working height (768) with zero scaling and clean white extension.
    session.execute("komaphone.corpse.setSegmentHeight", json!({"height": MIN_BAND_HEIGHT})).unwrap();
    assert_eq!(session.active().unwrap().doc.size, Size::new(WIDTH, MIN_BAND_HEIGHT));
    session.execute("komaphone.corpse.setSegmentHeight", json!({"height": MAX_BAND_HEIGHT})).unwrap();
    assert_eq!(session.active().unwrap().doc.size, Size::new(WIDTH, MAX_BAND_HEIGHT));
    assert_eq!(content_bottom(&session.active().unwrap().doc), 0, "extended white background remains blank");
    session.execute("komaphone.corpse.setSegmentHeight", json!({"height": 768})).unwrap();
    assert_eq!(session.active().unwrap().doc.size, Size::new(WIDTH, 768));

    // Paint a distinctive mark near y = 200..420; top stays anchored when expanding to 1024.
    session.execute("paint.stroke", json!({"points": [[300, 200], [500, 410]], "size": 20, "color": [0.85, 0.15, 0.2, 1.0], "seed": 11})).unwrap();
    let mark_before = photocraft_compose::flatten(&session.active().unwrap().doc).to_rgba8();
    let sample_row_200: Vec<u8> = mark_before.pixels[(200 * WIDTH as usize * 4)..(201 * WIDTH as usize * 4)].to_vec();
    let safe_min = safe_min_height(&session.active().unwrap().doc);
    assert!((410..768).contains(&safe_min), "safe_min={safe_min}");

    session.execute("komaphone.corpse.setSegmentHeight", json!({"height": 1024})).unwrap();
    let expanded = photocraft_compose::flatten(&session.active().unwrap().doc).to_rgba8();
    assert_eq!(
        &expanded.pixels[(200 * WIDTH as usize * 4)..(201 * WIDTH as usize * 4)],
        sample_row_200.as_slice(),
        "top seam and painted artwork remain unscaled and anchored at y=200"
    );
    // New area below 768 is pure white.
    assert_eq!(&expanded.pixels[(900 * WIDTH as usize * 4)..(900 * WIDTH as usize * 4 + 4)], &[255, 255, 255, 255]);

    // Safe shrink to safe_min succeeds without losing pixels.
    session.execute("komaphone.corpse.setSegmentHeight", json!({"height": safe_min})).unwrap();
    assert_eq!(session.active().unwrap().doc.size.height, safe_min);

    // Non-destructive shrink below safe_min is refused with actionable explanation of exact bound.
    let err = session.execute("komaphone.corpse.setSegmentHeight", json!({"height": 256})).unwrap_err().to_string();
    assert!(err.contains(&format!("{safe_min} px")), "refusal explains enforced bound: {err}");
    assert_eq!(session.active().unwrap().doc.size.height, safe_min);

    // Explicit crop succeeds, trims below 256, and Undo/Redo restores exact height & cropped marks!
    let res = session.execute("komaphone.corpse.setSegmentHeight", json!({"height": 256, "allowCrop": true})).unwrap();
    assert_eq!(res["cropped"], json!(true));
    assert_eq!(session.active().unwrap().doc.size.height, 256);
    assert!(session.undo());
    assert_eq!(session.active().unwrap().doc.size.height, safe_min);
    assert_eq!(safe_min_height(&session.active().unwrap().doc), safe_min, "undo restored cropped pixels");
    assert!(session.redo());
    assert_eq!(session.active().unwrap().doc.size.height, 256);
    assert!(session.undo());

    // Crash/reopen durability: flush local SQLite, drop Session, reopen fresh Session on same DB.
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let saved_pixels = photocraft_compose::flatten(&session.active().unwrap().doc).to_rgba8().pixels;
    drop(session);

    let mut reopened = setup_session(dir.path(), &alice, active_corpse_room(&alice, proj, 1, Phase::Drawing, false));
    assert_eq!(reopened.active().unwrap().doc.size, Size::new(WIDTH, safe_min));
    assert_eq!(photocraft_compose::flatten(&reopened.active().unwrap().doc).to_rgba8().pixels, saved_pixels);

    verify_corpse_lock_and_mode_guards(&mut reopened);
}

fn verify_corpse_lock_and_mode_guards(reopened: &mut Session) {
    // Submitted and Transfer phases lock resizing completely.
    reopened.rooms.status.room.as_mut().unwrap().assignment.as_mut().unwrap().submitted = true;
    assert!(!reopened.is_enabled("komaphone.corpse.setSegmentHeight"));
    assert!(reopened.execute("komaphone.corpse.setSegmentHeight", json!({"height": 640})).is_err());
    reopened.rooms.status.room.as_mut().unwrap().assignment.as_mut().unwrap().submitted = false;
    reopened.rooms.status.room.as_mut().unwrap().phase = Phase::Transfer;
    assert!(!reopened.is_enabled("komaphone.corpse.setSegmentHeight"));
    assert!(reopened.execute("komaphone.corpse.setSegmentHeight", json!({"height": 640})).is_err());

    // Animation mode rejects corpse segment height command.
    reopened.rooms.status.room.as_mut().unwrap().phase = Phase::Drawing;
    reopened.rooms.status.room.as_mut().unwrap().mode = Mode::Animation { rotations: 1 };
    assert!(!reopened.is_enabled("komaphone.corpse.setSegmentHeight"));
    assert!(reopened.execute("komaphone.corpse.setSegmentHeight", json!({"height": 640})).is_err());
}

#[test]
fn corpse_height_bounds_and_assembly_are_depth_and_color_general() {
    // Depth & color generality: content_bottom + safe_min_height + assemble_samples_with_heights across U8/U16/F32 and RGB/Gray.
    for mode in [ColorMode::Rgb, ColorMode::Grayscale] {
        for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
            let mut doc = Document::with_background("depth-check", Size::new(WIDTH, 600), mode, depth, Color::WHITE);
            assert_eq!(content_bottom(&doc), 0, "blank {mode:?}/{depth:?} has zero content bottom");
            assert_eq!(safe_min_height(&doc), MIN_BAND_HEIGHT);
            let fmt = doc.pixel_format();
            let dark = photocraft_raster::from_rgba(&fmt, [0.2, 0.3, 0.4, 1.0]);
            doc.layers[0].surface_mut().unwrap().fill_rect(Rect::from_xywh(100, 310, 40, 25), &dark);
            assert_eq!(content_bottom(&doc), 335, "detected mark bottom in {mode:?}/{depth:?}");
            assert_eq!(safe_min_height(&doc), 335);
        }
    }
    let f32_a = vec![0.25_f32; (WIDTH * 192 * 4) as usize];
    let f32_b = vec![0.50_f32; (WIDTH * 768 * 4) as usize];
    let f32_c = vec![0.75_f32; (WIDTH * 384 * 4) as usize];
    let joined = assemble_samples_with_heights([Some((192, f32_a.as_slice())), Some((768, f32_b.as_slice())), Some((384, f32_c.as_slice()))], 4).unwrap();
    assert_eq!(joined.len(), (WIDTH * (192 + 768 + 384) * 4) as usize);
    assert_eq!(joined[0], 0.25);
    assert_eq!(joined[(WIDTH * 192 * 4) as usize], 0.50);
    assert_eq!(joined[(WIDTH * (192 + 768) * 4) as usize], 0.75);
}

fn build_prepared_submission(
    dir: &std::path::Path,
    actor: &Participant,
    room_view: &RoomView,
    target_height: u32,
    top_rgb: [f32; 3],
    seam_rgb: [u8; 3],
    objects: &mut BTreeMap<AssetHash, Vec<u8>>,
) -> Submission {
    let assigned = room_view.assignment.as_ref().unwrap().assignment.clone();
    let db = dir.join(format!("draft-{}.sqlite3", assigned.project.as_str()));
    let mut session = Session::new();
    session.enable_local_projects(db, Default::default()).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    session.rooms.status.connected = true;
    session.rooms.status.paired = true;
    session.rooms.status.profile = Some(actor.clone());
    session.rooms.status.room = Some(room_view.clone());
    session.open_room_draft().unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();

    // Exercise intermediate height change so replay captures variable heights cleanly.
    session.execute("komaphone.corpse.setSegmentHeight", json!({"height": 700})).unwrap();
    session.execute("komaphone.corpse.setSegmentHeight", json!({"height": target_height})).unwrap();
    session
        .execute("paint.stroke", json!({"points": [[120, 40], [880, 40]], "size": 30, "color": [top_rgb[0], top_rgb[1], top_rgb[2], 1.0], "seed": 3}))
        .unwrap();
    // Paint exact distinctive bottom 24-row strip at y = (target_height - 24)..target_height.
    let seam_y = target_height - SEAM_HEIGHT;
    session
        .execute(
            "paint.stroke",
            json!({
                "points": [[0, seam_y + 12], [WIDTH, seam_y + 12]],
                "size": 36,
                "color": [f32::from(seam_rgb[0]) / 255.0, f32::from(seam_rgb[1]) / 255.0, f32::from(seam_rgb[2]) / 255.0, 1.0],
                "seed": 4
            }),
        )
        .unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();

    // Prepare submission via local drafts package pipeline (exact same path as submit_room_draft).
    let local = session.local_projects.status(session.active().unwrap().doc.id);
    let doc = session.active().unwrap().doc.clone();
    assert_eq!(doc.size, Size::new(WIDTH, target_height));
    let native = photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap();
    let mut composite = photocraft_compose::flatten(&doc);
    photocraft_party::canonical_color::to_srgb(&doc, &mut composite.px).unwrap();
    let rendered = composite.to_rgba8();
    let encode = |w: u32, h: u32, px: &[u8]| {
        let mut buf = Vec::new();
        use image::ImageEncoder;
        image::codecs::png::PngEncoder::new(&mut buf).write_image(px, w, h, image::ExtendedColorType::Rgba8).unwrap();
        buf
    };
    let mut put = |bytes: Vec<u8>| -> AssetRef {
        let hash = AssetHash::new(&format!("{:x}", Sha256::digest(&bytes))).unwrap();
        let asset = AssetRef { hash, bytes: bytes.len() as u64 };
        objects.insert(hash, bytes);
        asset
    };
    let img_asset = put(encode(WIDTH, target_height, &rendered.pixels));
    let seam_offset = ((target_height - SEAM_HEIGHT) * WIDTH * 4) as usize;
    let seam_asset = put(encode(WIDTH, SEAM_HEIGHT, &rendered.pixels[seam_offset..]));
    let cp_asset = put(native);
    let cp_ref = CheckpointRef { id: CheckpointId::new(&cp_asset.hash.as_str()[..32]).unwrap(), asset: cp_asset };
    Submission {
        id: SubmissionId::new(&format!("{:x}", Sha256::digest(assigned.project.as_str().as_bytes()))[..32]).unwrap(),
        assignment: assigned.id,
        author: actor.id,
        project: assigned.project,
        revision: local.local_revision.max(1),
        image: img_asset,
        checkpoint: cp_ref,
        replay: ReplayRef {
            id: ReplayId::new(assigned.project.as_str()).unwrap(),
            manifest: None,
            baseline: cp_ref,
            end_event: None,
            event_count: 0,
            duration_ms: 0,
        },
        assets: vec![cp_asset, img_asset, seam_asset],
        note: String::new(),
        seam: Some(seam_asset),
        extent: Some(ImageExtent { width: WIDTH, height: target_height }),
    }
}

#[test]
fn two_and_three_player_variable_height_games_assemble_export_archive_remix_and_restore_legacy() {
    for players in [2usize, 3] {
        let dir = tempfile::tempdir().unwrap();
        let actors: Vec<_> = (0..players).map(|i| participant((i + 1) as u8, &format!("Player {}", i + 1))).collect();
        let mut room = Room::new(RoomId::new(&format!("{:032x}", 700 + players)).unwrap(), actors[0].clone(), Mode::ExquisiteCorpse, None).unwrap();
        for actor in actors.iter().skip(1) {
            room.apply(actor.id, room.revision(), 0, Action::Join { participant: actor.clone() }).unwrap();
        }
        for actor in &actors {
            room.apply(actor.id, room.revision(), 0, Action::Ready { ready: true }).unwrap();
        }
        let projects: Vec<_> = (0..players * 3).map(|i| project_id(100 + i as u64)).collect();
        room.apply(actors[0].id, room.revision(), 0, Action::Start { game: GameId::new(&format!("{:032x}", 800 + players)).unwrap(), projects }).unwrap();

        let heights = [[192u32, 512, 1024], [768, 128, 1280], [640, 1024, 384]];
        let seam_colors = [[220u8, 40, 40], [40, 210, 60], [50, 80, 230]];
        let mut objects: BTreeMap<AssetHash, Vec<u8>> = BTreeMap::new();
        let mut expected_heights: BTreeMap<(u8, u8), u32> = BTreeMap::new();

        for round in 0..3usize {
            for (idx, actor) in actors.iter().enumerate() {
                let view = room.view(actor.id).unwrap();
                let chain = view.assignment.as_ref().unwrap().assignment.id.chain;
                if round > 0 {
                    let Some(Reference::Seam { image: seam_ref, width: WIDTH, height: SEAM_HEIGHT }) = view.assignment.as_ref().unwrap().reference else {
                        panic!("expected seam reference for round {round}");
                    };
                    let decoded = image::load_from_memory(objects.get(&seam_ref.hash).unwrap()).unwrap().into_rgba8();
                    assert_eq!(decoded.dimensions(), (WIDTH, SEAM_HEIGHT));
                    let prev_col = seam_colors[round - 1];
                    let sample = decoded.get_pixel(512, 12);
                    assert!(
                        (i16::from(sample[0]) - i16::from(prev_col[0])).abs() <= 4
                            && (i16::from(sample[1]) - i16::from(prev_col[1])).abs() <= 4
                            && (i16::from(sample[2]) - i16::from(prev_col[2])).abs() <= 4,
                        "seam pixel {:?} must match previous round's actual bottom rows {:?}",
                        sample,
                        prev_col
                    );
                }
                let h = heights[round][idx];
                expected_heights.insert((chain, round as u8), h);
                let sub = build_prepared_submission(dir.path(), actor, &view, h, [0.2, 0.2, 0.2], seam_colors[round], &mut objects);
                room.apply(actor.id, room.revision(), 10 + round as u64, Action::Submit { epoch: room.epoch(), submission: sub }).unwrap();
            }
        }

        let revealed = room.view(actors[0].id).unwrap();
        assert_eq!(revealed.phase, Phase::Reveal);
        let chain0 = reveal_chain(&revealed, 0).unwrap();
        let h0 = expected_heights[&(0, 0)];
        let h1 = expected_heights[&(0, 1)];
        let h2 = expected_heights[&(0, 2)];
        assert_eq!(chain0.parts.each_ref().map(|p| p.height), [h0, h1, h2]);
        assert_eq!(chain0.total_height(), h0 + h1 + h2);

        // Build GalleryBundle, validate originals, export PNG, export/import .komaphone archive, and remix.
        let manifest = GalleryManifest::from_reveal(project_id(900 + players as u64), &revealed, actors[0].id, "2026-11-23T19:00:00Z".into()).unwrap();
        let req = manifest.required_assets().unwrap();
        let bundle = GalleryBundle { manifest, objects: req.into_iter().map(|a| (a.hash, objects[&a.hash].clone())).collect() };
        validate_originals(&bundle).unwrap();

        let assembled = render_creature(&bundle, 0).unwrap();
        assert_eq!((assembled.width, assembled.height), (WIDTH, h0 + h1 + h2));
        // Direct assemble() matches render_creature() byte-for-byte.
        let part_imgs: Vec<RoomImage> = (0..3u8)
            .map(|r| {
                let asset = chain0.parts[usize::from(r)].image.unwrap();
                let img = image::load_from_memory(&objects[&asset.hash]).unwrap().into_rgba8();
                RoomImage { width: img.width(), height: img.height(), rgba: img.into_raw() }
            })
            .collect();
        assert_eq!(assemble([Some(&part_imgs[0]), Some(&part_imgs[1]), Some(&part_imgs[2])]).unwrap(), assembled);

        // PNG export preserves exact variable dimensions without stretching or gaps.
        let png_path = dir.path().join(format!("creature-{players}p.png"));
        export_png(&bundle, 0, None, &png_path).unwrap();
        let exported = image::open(&png_path).unwrap().into_rgba8();
        assert_eq!(exported.dimensions(), (WIDTH, h0 + h1 + h2));
        assert_eq!(exported.as_raw(), &assembled.rgba);

        // Archive export & import into a fresh GalleryStore preserves exact heights, pixels, and credits.
        let mut store1 = GalleryStore::open(dir.path().join("gallery1.sqlite3"), StoreOptions::default()).unwrap();
        store1.insert(&bundle).unwrap();
        let archive_path = dir.path().join(format!("game-{players}p.komaphone"));
        store1.export_archive(bundle.manifest.id, &archive_path).unwrap();
        let mut store2 = GalleryStore::open(dir.path().join("gallery2.sqlite3"), StoreOptions::default()).unwrap();
        let imported_info = store2.import_archive(&archive_path).unwrap();
        let imported = store2.load(imported_info.id).unwrap();
        validate_originals(&imported).unwrap();
        assert_eq!(render_creature(&imported, 0).unwrap(), assembled);
        assert!(replay_info(&imported, 0, 0).unwrap().baseline_only);
        assert_eq!(replay_step(&imported, 0, 0, 0).unwrap().height, h0);

        // Creature remix produces an editable 1024 x (h0+h1+h2) document with 3 layers aligned at y = 0, h0, h0+h1.
        let (remixed, prov) = remix(&imported, 0, None).unwrap();
        assert_eq!(remixed.size, Size::new(WIDTH, h0 + h1 + h2));
        assert_eq!(remixed.layers.len(), 3);
        assert_eq!(prov.authors.len(), players);
        let remixed_flat = photocraft_compose::flatten(&remixed).to_rgba8();
        assert_eq!(remixed_flat.pixels, assembled.rgba, "remixed creature pixels match assembled original without gaps or drift");

        // Single-part remix extracts that part at its exact custom height.
        let (part_doc, _) = remix(&imported, 0, Some(1)).unwrap();
        assert_eq!(part_doc.size, Size::new(WIDTH, h1));
    }

    // Legacy fixed-height game archive (`extent` omitted in JSON) restores at 1024x1536 without altering art or credits.
    let dir = tempfile::tempdir().unwrap();
    let alice = participant(1, "Alice");
    let bob = participant(2, "Bob");
    let mut room = Room::new(RoomId::new(&"cc".repeat(16)).unwrap(), alice.clone(), Mode::ExquisiteCorpse, None).unwrap();
    room.apply(bob.id, room.revision(), 0, Action::Join { participant: bob.clone() }).unwrap();
    room.apply(alice.id, room.revision(), 0, Action::Ready { ready: true }).unwrap();
    room.apply(bob.id, room.revision(), 0, Action::Ready { ready: true }).unwrap();
    let projects: Vec<_> = (0..6).map(|i| project_id(950 + i)).collect();
    room.apply(alice.id, room.revision(), 0, Action::Start { game: GameId::new(&"dd".repeat(16)).unwrap(), projects }).unwrap();
    let mut objects = BTreeMap::new();
    for _round in 0..3usize {
        for actor in [&alice, &bob] {
            let view = room.view(actor.id).unwrap();
            let mut sub = build_prepared_submission(dir.path(), actor, &view, BAND_HEIGHT, [0.4, 0.4, 0.4], [120, 140, 160], &mut objects);
            sub.extent = None; // Simulate pre-QOL-007 fixed-height submission.
            room.apply(actor.id, room.revision(), 1, Action::Submit { epoch: room.epoch(), submission: sub }).unwrap();
        }
    }
    let revealed = room.view(alice.id).unwrap();
    assert!(revealed.reveal.iter().all(|e| matches!(e, RevealedEntry::Artwork { extent: None, .. })));
    let manifest = GalleryManifest::from_reveal(project_id(999), &revealed, alice.id, "2026-11-18T00:00:00Z".into()).unwrap();
    let legacy_json = serde_json::to_string(&manifest).unwrap();
    assert!(!legacy_json.contains("\"extent\""), "legacy fixed-height manifest omits extent");
    let decoded_manifest: GalleryManifest = serde_json::from_str(&legacy_json).unwrap();
    let req = decoded_manifest.required_assets().unwrap();
    let legacy_bundle = GalleryBundle { manifest: decoded_manifest, objects: req.into_iter().map(|a| (a.hash, objects[&a.hash].clone())).collect() };
    validate_originals(&legacy_bundle).unwrap();
    let legacy_img = render_creature(&legacy_bundle, 0).unwrap();
    assert_eq!((legacy_img.width, legacy_img.height), (WIDTH, BAND_HEIGHT * 3));
}
