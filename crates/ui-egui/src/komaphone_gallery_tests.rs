//! Headless view checks complement the real native gallery/release acceptance run.
//! No storage, network or archive claim follows from these layout fixtures.
use super::*;
use photocraft_engine::gallery::GalleryItem;
use photocraft_engine::gallery_media::{ReplayInfo, ReplayMoment};
use photocraft_engine::party::gallery::{GALLERY_SCHEMA, GalleryManifest};
use photocraft_engine::party::room::{Assignment, MemberView, Phase, RevealedEntry, RoomView};
use photocraft_engine::party::{
    AssetHash, AssetRef, AssignmentId, CheckpointId, CheckpointRef, DeviceId, GameId, Mode, Participant, ParticipantId, ReplayId, ReplayRef, RoomId,
};
use photocraft_engine::rooms::media::RoomImage;
use std::sync::Arc;

fn id(value: u64) -> ProjectId {
    ProjectId::new(&format!("{value:032x}")).unwrap()
}
fn asset(value: u64) -> AssetRef {
    AssetRef { hash: AssetHash::new(&format!("{value:064x}")).unwrap(), bytes: 100 }
}
fn author(value: u64) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{value:032x}")).unwrap(),
        device: DeviceId::new(&format!("{value:032x}")).unwrap(),
        display_name: format!("Artist {value}"),
    }
}
fn manifest() -> GalleryManifest {
    GalleryManifest {
        schema_version: GALLERY_SCHEMA,
        id: id(100),
        title: "Saved animation".into(),
        created_at: "2026-11-06T01:00:00Z".into(),
        updated_at: "2026-11-06T01:00:00Z".into(),
        authors: vec![author(1)],
        content: GalleryContent::Project { checkpoint: asset(1), direct_assets: Vec::new(), replay: None },
        provenance: None,
        notes: Vec::new(),
        fps: 6,
        looping: true,
    }
}
fn item(number: u64, title: &str, author: &str, favorite: bool, trashed: bool) -> GalleryItem {
    GalleryItem {
        id: id(number),
        title: title.into(),
        kind: "animation".into(),
        authors: vec![author.into()],
        updated_at: "2026-11-06".into(),
        favorite,
        trashed,
        complete: true,
        draft: false,
        revision: 1,
        state: "Completed game".into(),
        source_projects: vec![id(number)],
        saved_matches_head: false,
        has_saved_copy: false,
        has_unsaved_edits: false,
        untouched_blank: false,
        covered_by_game: false,
    }
}
fn app(active: bool) -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.ui.komaphone.enabled = true;
    app.ui.gallery.open = true;
    if active {
        app.session.gallery.active = Some(Arc::new(manifest()));
        app.session.gallery.frame_count = 8;
        app.session.gallery.items.push(item(100, "Saved animation", "Artist 1", false, false));
        app.session.gallery.image = Some(Arc::new(RoomImage { width: 1536, height: 1024, rgba: [255, 80, 40, 255].repeat(1536 * 1024) }));
        app.session.gallery.replay = Some(ReplayInfo {
            duration_ms: 400,
            baseline_only: false,
            missing: false,
            moments: (0..5)
                .map(|index| ReplayMoment {
                    revision: index + 1,
                    elapsed_ms: index * 100,
                    command: if index == 0 { "file.new" } else { "paint.stroke" }.into(),
                    kind: if index == 0 { "baseline" } else { "resolved" }.into(),
                })
                .collect(),
        });
    }
    app
}
fn paint(app: &mut PhotocraftApp, ctx: &egui::Context, now: f64, events: Vec<egui::Event>) -> Vec<String> {
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            time: Some(now),
            events,
            ..Default::default()
        },
        |ui| {
            egui::CentralPanel::default().show(ui, |ui| show(app, ui));
        },
    );
    let mut texts = Vec::new();
    fn collect(shape: &egui::Shape, texts: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(text) => texts.push(text.galley.text().into()),
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    collect(shape, texts);
                }
            }
            _ => {}
        }
    }
    for shape in &output.shapes {
        collect(&shape.shape, &mut texts);
    }
    output.textures_delta.clear();
    texts
}
fn tick_at(app: &mut PhotocraftApp, ctx: &egui::Context, now: f64) {
    ctx.run_ui(egui::RawInput { time: Some(now), ..Default::default() }, |ui| tick(app, ui)).textures_delta.clear();
}
fn click(app: &mut PhotocraftApp, ctx: &egui::Context, name: &str, now: f64) {
    let rect = crate::komaphone_workspace::control_rect(ctx, name).unwrap_or_else(|| panic!("missing gallery control {name}"));
    let pos = rect.center();
    paint(
        app,
        ctx,
        now,
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() },
        ],
    );
    paint(app, ctx, now + 0.01, vec![egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() }]);
}

#[test]
fn gallery_sections_search_and_trash_show_only_the_selected_items() {
    let mut app = app(false);
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, app.ui.theme);
    app.session.gallery.items = vec![
        GalleryItem { kind: "project".into(), ..item(1, "Saved portrait", "Ada", false, false) },
        item(2, "Warm sunset", "Ada", false, false),
        GalleryItem { kind: "creature".into(), ..item(3, "Blue monster", "Bob", false, false) },
        GalleryItem { kind: "draft".into(), draft: true, ..item(4, "Work in progress", "You", false, false) },
        GalleryItem { kind: "draft".into(), draft: true, untouched_blank: true, ..item(5, "Blank canvas", "You", false, false) },
        GalleryItem { kind: "draft".into(), draft: true, covered_by_game: true, ..item(6, "Room round", "You", false, false) },
        GalleryItem { kind: "draft".into(), draft: true, saved_matches_head: true, ..item(7, "Synced draft", "You", false, false) },
        item(8, "Old sketch", "Ada", false, true),
    ];
    for entry in &app.session.gallery.items {
        app.session.gallery.thumbnails.insert((entry.id, entry.draft), Arc::new(RoomImage { width: 4, height: 4, rgba: vec![255; 64] }));
    }
    // Default Saved section shows only explicitly saved solo drawings.
    paint(&mut app, &ctx, 0.0, Vec::new());
    let texts = paint(&mut app, &ctx, 0.1, Vec::new());
    assert!(texts.iter().any(|t| t == "Saved portrait"));
    assert!(!texts.iter().any(|t| t == "Warm sunset"));
    assert!(!texts.iter().any(|t| t == "Work in progress"));
    assert!(!texts.iter().any(|t| t == "Old sketch"));
    assert!(crate::komaphone_workspace::control_rect(&ctx, "gallery.draw").is_none(), "redundant Draw button removed");

    // Completed section shows Animation and Exquisite Corpse games with sub-filtering and search.
    click(&mut app, &ctx, "gallery.section.completed", 0.2);
    let texts = paint(&mut app, &ctx, 0.25, Vec::new());
    assert!(texts.iter().any(|t| t == "Warm sunset"));
    assert!(texts.iter().any(|t| t == "Blue monster"));
    assert!(!texts.iter().any(|t| t == "Saved portrait"));
    assert!(!texts.iter().any(|t| t == "Old sketch"));

    app.ui.gallery.filter = "ada".into();
    let texts = paint(&mut app, &ctx, 0.3, Vec::new());
    assert!(texts.iter().any(|t| t == "Warm sunset"));
    assert!(!texts.iter().any(|t| t == "Blue monster"));
    app.ui.gallery.filter.clear();

    click(&mut app, &ctx, "gallery.completed.creature", 0.35);
    let texts = paint(&mut app, &ctx, 0.4, Vec::new());
    assert!(texts.iter().any(|t| t == "Blue monster"));
    assert!(!texts.iter().any(|t| t == "Warm sunset"));

    // Temp section shows only real unsaved drafts (hiding blanks, covered game rounds, and synced copies).
    click(&mut app, &ctx, "gallery.section.temp", 0.45);
    let texts = paint(&mut app, &ctx, 0.5, Vec::new());
    assert!(texts.iter().any(|t| t == "Work in progress"));
    assert!(!texts.iter().any(|t| t == "Blank canvas"));
    assert!(!texts.iter().any(|t| t == "Room round"));
    assert!(!texts.iter().any(|t| t == "Synced draft"));

    // Trash section shows trashed items with their original section label.
    click(&mut app, &ctx, "gallery.section.trash", 0.55);
    let texts = paint(&mut app, &ctx, 0.6, Vec::new());
    assert!(texts.iter().any(|t| t == "Old sketch"));
    assert!(texts.iter().any(|t| t.contains("From Completed")));
    assert!(!texts.iter().any(|t| t == "Warm sunset"));
}

#[test]
fn animation_and_drawing_playback_controls_are_separate_and_mouse_accessible() {
    let mut app = app(true);
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, app.ui.theme);
    paint(&mut app, &ctx, 0.0, Vec::new());
    paint(&mut app, &ctx, 0.1, Vec::new());
    let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0));
    for name in [
        "gallery.filter",
        "gallery.finishedMode",
        "gallery.replayMode",
        "gallery.animationPlay",
        "gallery.frame",
        "gallery.fps",
        "gallery.loop",
        "komaphone.gallery.remix",
        "gallery.remixStill",
    ] {
        let rect = crate::komaphone_workspace::control_rect(&ctx, name).unwrap_or_else(|| panic!("missing visible {name}"));
        assert!(viewport.contains_rect(rect), "{name}: {rect:?}");
    }
    assert!(crate::komaphone_workspace::control_rect(&ctx, "gallery.replayPlay").is_none());
    app.ui.gallery.playing_animation = true;
    click(&mut app, &ctx, "gallery.replayMode", 0.2);
    assert!(app.ui.gallery.replay_mode);
    assert!(!app.ui.gallery.playing_animation);
    assert!(!app.ui.gallery.playing_replay);
    paint(&mut app, &ctx, 0.3, Vec::new());
    paint(&mut app, &ctx, 0.4, Vec::new());
    assert!(crate::komaphone_workspace::control_rect(&ctx, "gallery.replayPlay").is_some());
    assert!(crate::komaphone_workspace::control_rect(&ctx, "gallery.animationPlay").is_none());
    app.ui.gallery.playing_replay = true;
    click(&mut app, &ctx, "gallery.finishedMode", 0.5);
    assert!(!app.ui.gallery.replay_mode);
    assert!(!app.ui.gallery.playing_replay);
}

#[test]
fn configured_playback_uses_elapsed_time_and_stops_at_the_real_project_end() {
    let mut app = app(true);
    let ctx = egui::Context::default();
    app.ui.gallery.fps = 24;
    app.ui.gallery.playing_animation = true;
    app.ui.gallery.last_time = 1.0;
    tick_at(&mut app, &ctx, 1.25);
    assert_eq!(app.ui.gallery.frame, 6, "six 24fps frames elapsed in 250ms");
    assert_eq!(app.ui.gallery.replay_step, 0);
    tick_at(&mut app, &ctx, 1.5);
    assert_eq!(app.ui.gallery.frame, 4, "eight real project frames wrap, not a hard-coded 12");
    app.ui.gallery.looping = false;
    app.ui.gallery.frame = 6;
    tick_at(&mut app, &ctx, 1.75);
    assert_eq!(app.ui.gallery.frame, 7);
    assert!(!app.ui.gallery.playing_animation);
    app.ui.gallery.replay_mode = true;
    app.ui.gallery.playing_replay = true;
    app.ui.gallery.replay_speed = 2.0;
    app.ui.gallery.accumulator = 0.0;
    tick_at(&mut app, &ctx, 2.0);
    assert_eq!(app.ui.gallery.replay_step, 4);
    assert!(!app.ui.gallery.playing_replay);
    assert_eq!(app.ui.gallery.frame, 7);
}

#[test]
fn play_after_the_end_restarts_each_timeline_from_its_own_beginning() {
    let mut app = app(true);
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, app.ui.theme);
    paint(&mut app, &ctx, 0.0, Vec::new());
    paint(&mut app, &ctx, 0.1, Vec::new());
    app.ui.gallery.frame = 7;
    app.ui.gallery.looping = false;
    click(&mut app, &ctx, "gallery.animationPlay", 0.2);
    assert_eq!(app.ui.gallery.frame, 0);
    assert!(app.ui.gallery.playing_animation);
    click(&mut app, &ctx, "gallery.replayMode", 0.3);
    paint(&mut app, &ctx, 0.4, Vec::new());
    app.ui.gallery.replay_step = 4;
    click(&mut app, &ctx, "gallery.replayPlay", 0.5);
    assert_eq!(app.ui.gallery.replay_step, 0);
    assert!(app.ui.gallery.playing_replay);
    assert!(!app.ui.gallery.playing_animation);
}

#[test]
fn restart_does_not_autoplay_or_advance_a_saved_view_without_user_input() {
    let state =
        GalleryUi { frame: 3, replay_step: 2, playing_animation: true, playing_replay: true, accumulator: 500.0, last_time: 1000.0, ..Default::default() };
    let restored: GalleryUi = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!((restored.frame, restored.replay_step), (3, 2));
    assert!(!restored.playing_animation);
    assert!(!restored.playing_replay);
    assert_eq!(restored.accumulator, 0.0);
}

fn noted_game() -> GalleryManifest {
    let game = GameId::new(&format!("{:032x}", 8)).unwrap();
    let mut reveal = Vec::new();
    for chain in 0..2 {
        for round in 0..2 {
            let assignment = Assignment {
                id: AssignmentId { game, chain, round },
                player: author(u64::from((chain + round) % 2) + 1).id,
                project: id(10 + u64::from(chain) * 2 + u64::from(round)),
            };
            let checkpoint = CheckpointRef { id: CheckpointId::new(assignment.project.as_str()).unwrap(), asset: asset(2) };
            reveal.push(RevealedEntry::Artwork {
                assignment,
                revision: 1,
                image: asset(1),
                checkpoint,
                replay: Box::new(ReplayRef {
                    id: ReplayId::new(id(20).as_str()).unwrap(),
                    manifest: None,
                    baseline: checkpoint,
                    end_event: None,
                    event_count: 0,
                    duration_ms: 0,
                }),
                assets: vec![asset(1), asset(2)],
            });
        }
    }
    let room = RoomView {
        id: RoomId::new(id(21).as_str()).unwrap(),
        revision: 10,
        epoch: 1,
        phase: Phase::Reveal,
        round: 1,
        mode: Mode::Animation { rotations: 1 },
        timer_ms: None,
        moderator: author(1).id,
        members: vec![MemberView { participant: author(1), connected: true, ready: true }, MemberView { participant: author(2), connected: true, ready: true }],
        waiting: Vec::new(),
        paused: None,
        deadline_ms: None,
        assignment: None,
        reveal,
    };
    let mut result = GalleryManifest::from_reveal(id(100), &room, author(1).id, "2026-11-06T01:00:00Z".into()).unwrap();
    result.notes.push((AssignmentId { game, chain: 0, round: 0 }, "This note stays hidden until opened".into()));
    result
}

#[test]
fn handoff_notes_are_hidden_until_explicitly_opened_and_reset_for_another_item() {
    let mut app = app(true);
    app.session.gallery.active = Some(Arc::new(noted_game()));
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, app.ui.theme);
    paint(&mut app, &ctx, 0.0, Vec::new());
    let hidden = paint(&mut app, &ctx, 0.1, Vec::new());
    assert!(!hidden.iter().any(|t| t.contains("This note stays hidden")));
    // This explicit view-state action is equivalent to opening the notes checkbox;
    // the real native harness exercises its registered mouse target.
    app.ui.gallery.show_notes = true;
    let shown = paint(&mut app, &ctx, 0.2, Vec::new());
    assert!(shown.iter().any(|t| t.contains("This note stays hidden")));
    Arc::make_mut(app.session.gallery.active.as_mut().unwrap()).id = id(101);
    paint(&mut app, &ctx, 0.3, Vec::new());
    assert!(!app.ui.gallery.show_notes);
}

#[test]
fn private_or_closed_gallery_releases_preview_and_thumbnail_texture_handles() {
    let mut app = app(true);
    app.ui.gallery.section = GallerySection::Completed;
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, app.ui.theme);
    let thumbnail = Arc::new(RoomImage { width: 4, height: 4, rgba: vec![255; 64] });
    app.session.gallery.thumbnails.insert((id(100), false), thumbnail.clone());
    let preview_key = egui::Id::unique("gallery-preview");
    let thumbnail_key = egui::Id::unique(("gallery-thumb", id(100).as_str(), false));
    let cache_key = egui::Id::unique("gallery-texture-keys");
    let assert_cached = |expected: bool| {
        ctx.data(|data| {
            assert_eq!(data.get_temp::<Texture>(preview_key).is_some(), expected);
            assert_eq!(data.get_temp::<Texture>(thumbnail_key).is_some(), expected);
            let keys = data.get_temp::<TextureKeys>(cache_key).unwrap_or_default();
            assert_eq!(keys.0.len(), if expected { 2 } else { 0 });
        });
    };
    paint(&mut app, &ctx, 0.0, Vec::new());
    paint(&mut app, &ctx, 0.1, Vec::new());
    assert_cached(true);

    // Engine privacy pruning may occur while a different view is being shown.
    let active = app.session.gallery.active.take();
    let image = app.session.gallery.image.take();
    app.session.gallery.thumbnails.clear();
    prune_textures(&app, &ctx);
    assert_cached(false);

    app.session.gallery.active = active;
    app.session.gallery.image = image;
    app.session.gallery.thumbnails.insert((id(100), false), thumbnail);
    paint(&mut app, &ctx, 0.2, Vec::new());
    assert_cached(true);
    app.ui.gallery.open = false;
    prune_textures(&app, &ctx);
    assert_cached(false);
}

fn corpse_game_manifest(entry_num: u64, players: u8) -> GalleryManifest {
    let game = GameId::new(&format!("{entry_num:032x}")).unwrap();
    let mut reveal = Vec::new();
    for chain in 0..players {
        for round in 0..3u8 {
            let project = id(entry_num * 100 + u64::from(chain) * 3 + u64::from(round) + 1);
            let assignment = Assignment { id: AssignmentId { game, chain, round }, player: author(u64::from((chain + round) % players) + 1).id, project };
            let checkpoint = CheckpointRef { id: CheckpointId::new(project.as_str()).unwrap(), asset: asset(2) };
            reveal.push(RevealedEntry::Artwork {
                assignment,
                revision: 1,
                image: asset(1),
                checkpoint,
                replay: Box::new(ReplayRef {
                    id: ReplayId::new(project.as_str()).unwrap(),
                    manifest: None,
                    baseline: checkpoint,
                    end_event: None,
                    event_count: 0,
                    duration_ms: 0,
                }),
                assets: vec![asset(1), asset(2)],
            });
        }
    }
    let room = RoomView {
        id: RoomId::new(id(entry_num + 50).as_str()).unwrap(),
        revision: 12,
        epoch: 3,
        phase: Phase::Reveal,
        round: 2,
        mode: Mode::ExquisiteCorpse,
        timer_ms: None,
        moderator: author(1).id,
        members: (0..players).map(|i| MemberView { participant: author(u64::from(i) + 1), connected: true, ready: true }).collect(),
        waiting: Vec::new(),
        paused: None,
        deadline_ms: None,
        assignment: None,
        reveal,
    };
    GalleryManifest::from_reveal(id(entry_num), &room, author(1).id, "2026-11-21T01:00:00Z".into()).unwrap()
}

#[test]
fn switching_gallery_creature_or_game_during_playback_resets_part_step_and_timer() {
    let mut app = app(true);
    app.ui.gallery.section = GallerySection::Completed;
    let game_a = Arc::new(corpse_game_manifest(200, 3));
    let game_b = Arc::new(corpse_game_manifest(201, 2));
    app.session.gallery.active = Some(game_a);
    app.session.gallery.items = vec![
        GalleryItem { kind: "creature".into(), ..item(200, "Corpse game A", "Artist 1", false, false) },
        GalleryItem { kind: "creature".into(), ..item(201, "Corpse game B", "Artist 1", false, false) },
    ];
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, app.ui.theme);
    paint(&mut app, &ctx, 1.0, Vec::new());
    paint(&mut app, &ctx, 1.1, Vec::new());

    // Enter Drawing replay on Creature 1, Part 2 (legs), mid-playback with leftover fractional accumulator.
    click(&mut app, &ctx, "gallery.replayMode", 1.2);
    app.ui.gallery.frame = 2;
    app.ui.gallery.replay_step = 3;
    app.ui.gallery.playing_replay = true;
    app.ui.gallery.accumulator = 75.0;
    app.ui.gallery.last_time = 1.2;

    // Switching to Creature 2 stops replay and restarts at Part 0 (Head), step 0, accumulator 0.
    click(&mut app, &ctx, "gallery.chain.1", 1.3);
    assert_eq!(app.ui.gallery.chain, 1);
    assert_eq!(app.ui.gallery.frame, 0);
    assert_eq!(app.ui.gallery.replay_step, 0);
    assert!(!app.ui.gallery.playing_replay);
    assert_eq!(app.ui.gallery.accumulator, 0.0);

    // Subsequent clock ticks must not advance from stale playback state.
    paint(&mut app, &ctx, 2.5, Vec::new());
    assert_eq!((app.ui.gallery.chain, app.ui.gallery.frame, app.ui.gallery.replay_step), (1, 0, 0));
    assert!(!app.ui.gallery.playing_replay);

    // Switching backward to Creature 1 during playback also resets part, step and timer.
    app.ui.gallery.frame = 1;
    app.ui.gallery.replay_step = 2;
    app.ui.gallery.playing_replay = true;
    app.ui.gallery.accumulator = 60.0;
    click(&mut app, &ctx, "gallery.chain.0", 2.6);
    assert_eq!((app.ui.gallery.chain, app.ui.gallery.frame, app.ui.gallery.replay_step), (0, 0, 0));
    assert!(!app.ui.gallery.playing_replay);
    assert_eq!(app.ui.gallery.accumulator, 0.0);

    // Clicking another gallery game in the list stops active playback immediately before async open finishes.
    app.ui.gallery.playing_replay = true;
    app.ui.gallery.accumulator = 45.0;
    click(&mut app, &ctx, &format!("gallery.item.{}.original", id(201).as_str()), 2.8);
    assert!(!app.ui.gallery.playing_replay);
    assert_eq!(app.ui.gallery.accumulator, 0.0);

    // Switching to another gallery game mid-playback resets chain, part, replay mode, playback and timer.
    app.ui.gallery.chain = 2;
    app.ui.gallery.frame = 2;
    app.ui.gallery.replay_step = 4;
    app.ui.gallery.playing_replay = true;
    app.ui.gallery.accumulator = 90.0;
    app.session.gallery.active = Some(game_b);
    paint(&mut app, &ctx, 3.0, Vec::new());
    assert_eq!((app.ui.gallery.chain, app.ui.gallery.frame, app.ui.gallery.replay_step), (0, 0, 0));
    assert!(!app.ui.gallery.replay_mode);
    assert!(!app.ui.gallery.playing_replay);
    assert!(!app.ui.gallery.playing_animation);
    assert_eq!(app.ui.gallery.accumulator, 0.0);
    tick_at(&mut app, &ctx, 5.0);
    assert_eq!((app.ui.gallery.chain, app.ui.gallery.frame, app.ui.gallery.replay_step), (0, 0, 0));
}
