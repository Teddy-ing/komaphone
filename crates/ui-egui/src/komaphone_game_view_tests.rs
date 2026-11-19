//! Independent presentation-boundary checks. Real service/native flows are covered
//! by the K3 harness; these focus on state races and egui texture/navigation state.
use super::*;
use photocraft_engine::party::room::{Assignment, AssignmentView, MemberView};
use photocraft_engine::party::{AssetRef, AssignmentId, DeviceId, GameId, Participant, ParticipantId, ProjectId, RoomId};
use photocraft_engine::rooms::media::RoomImage;
use photocraft_engine::rooms::{NoteDraft, RevealPosition};
use std::sync::Arc;

fn participant(index: u8) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{:032x}", index + 1)).unwrap(),
        device: DeviceId::new(&format!("{:032x}", index + 11)).unwrap(),
        display_name: format!("Artist {}", index + 1),
    }
}
fn game() -> GameId {
    GameId::new(&"a".repeat(32)).unwrap()
}
fn assignment(chain: u8, round: u8) -> Assignment {
    Assignment {
        id: AssignmentId { game: game(), chain, round },
        player: participant((chain + round) % 2).id,
        project: ProjectId::new(&format!("{:032x}", 100 + u16::from(chain) * 12 + u16::from(round))).unwrap(),
    }
}
fn view(mode: Mode, phase: Phase) -> RoomView {
    let rounds = mode.rounds(2).unwrap();
    let terminal = matches!(phase, Phase::Reveal | Phase::Archived);
    RoomView {
        id: RoomId::new(&"b".repeat(32)).unwrap(),
        revision: 20,
        epoch: if terminal { u64::from(rounds) } else { 2 },
        phase,
        round: if terminal { rounds - 1 } else { 1 },
        mode,
        timer_ms: None,
        moderator: participant(0).id,
        members: (0..2).map(|index| MemberView { participant: participant(index), connected: true, ready: true }).collect(),
        waiting: vec![],
        paused: None,
        deadline_ms: None,
        assignment: (!terminal).then(|| AssignmentView { assignment: assignment(1, 1), submitted: false, reference: Some(Reference::Missing) }),
        reveal: if terminal {
            (0..2)
                .flat_map(|chain| {
                    (0..rounds).map(move |round| RevealedEntry::Missing { assignment: assignment(chain, round), reason: "Explicit fixture skip".into() })
                })
                .collect()
        } else {
            vec![]
        },
    }
}
fn app(room: RoomView) -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.ui.komaphone.enabled = true;
    app.session.rooms.status.profile = Some(participant(0));
    app.session.rooms.status.room = Some(room);
    app
}
fn asset(number: u8) -> AssetRef {
    AssetRef { hash: AssetHash::new(&format!("{number:064x}")).unwrap(), bytes: 100 }
}
fn paint_reveal(app: &mut PhotocraftApp, ctx: &egui::Context, now: f64) {
    ctx.run_ui(
        egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))), time: Some(now), ..Default::default() },
        |ui| {
            egui::CentralPanel::default().show(ui, |ui| reveal(app, ui));
        },
    )
    .textures_delta
    .clear();
}

#[test]
fn room_window_owns_assignment_controls_without_a_covered_banner_duplicate() {
    let mut app = app(view(Mode::Animation { rotations: 2 }, Phase::Drawing));
    let ctx = egui::Context::default();
    let name = "komaphone.room.openDraft";
    app.ui.rooms.open = true;
    ctx.run_ui(egui::RawInput::default(), |ui| reference_panel(&mut app, ui)).textures_delta.clear();
    assert!(crate::komaphone_workspace::control_rect(&ctx, name).is_none());
    app.ui.rooms.open = false;
    ctx.run_ui(egui::RawInput::default(), |ui| reference_panel(&mut app, ui)).textures_delta.clear();
    assert!(crate::komaphone_workspace::control_rect(&ctx, name).is_some());
}

#[test]
fn clipped_room_controls_remove_their_previous_click_target() {
    let ctx = egui::Context::default();
    let name = "komaphone.room.resume";
    ctx.run_ui(egui::RawInput::default(), |ui| {
        let response = ui.button("Resume");
        crate::komaphone_workspace::remember(ui, name, &response);
        assert!(crate::komaphone_workspace::control_rect(&ctx, name).is_some());
        ui.set_clip_rect(egui::Rect::NOTHING);
        crate::komaphone_workspace::remember(ui, name, &response);
        assert!(crate::komaphone_workspace::control_rect(&ctx, name).is_none());
    })
    .textures_delta
    .clear();
}

#[test]
fn omitted_room_controls_expire_after_the_next_render() {
    let ctx = egui::Context::default();
    let name = "komaphone.room.resume";
    ctx.run_ui(egui::RawInput::default(), |ui| {
        let response = ui.button("Resume");
        crate::komaphone_workspace::remember(ui, name, &response);
    })
    .textures_delta
    .clear();
    assert!(crate::komaphone_workspace::control_rect(&ctx, name).is_some());
    ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
    assert!(crate::komaphone_workspace::control_rect(&ctx, name).is_none());
}

#[test]
fn authorization_change_purges_engine_and_egui_texture_caches_before_next_paint() {
    let mut room = view(Mode::ExquisiteCorpse, Phase::Drawing);
    room.assignment.as_mut().unwrap().reference = Some(Reference::Seam { image: asset(1), width: 1024, height: 24 });
    let mut app = app(room);
    let ctx = egui::Context::default();
    app.session.rooms.media.reconcile(&app.session.rooms.status);
    let image = Arc::new(RoomImage { width: 1024, height: 24, rgba: vec![255; 1024 * 24 * 4] });
    app.session.rooms.media.images.insert(asset(1).hash, image.clone());
    app.session.rooms.media.images.insert(asset(2).hash, image.clone());
    sync(&mut app, &ctx);
    assert_eq!(cached_hashes(&ctx), vec![asset(1).hash]);
    assert_eq!(app.session.rooms.media.images.keys().copied().collect::<Vec<_>>(), vec![asset(1).hash]);
    app.session.rooms.status.room.as_mut().unwrap().assignment.as_mut().unwrap().reference = Some(Reference::Seam { image: asset(2), width: 1024, height: 24 });
    app.session.rooms.media.images.insert(asset(2).hash, image);
    sync(&mut app, &ctx);
    assert_eq!(cached_hashes(&ctx), vec![asset(2).hash]);
    assert!(texture(&ctx, asset(1).hash).is_none());
    app.session.rooms.status.profile = Some(participant(2));
    sync(&mut app, &ctx);
    assert!(cached_hashes(&ctx).is_empty(), "A waiting/unknown viewer cannot retain rendered room textures");
    assert!(app.session.rooms.media.images.is_empty());
}

#[test]
fn reveal_announcement_is_once_per_game_and_does_not_trap_advanced_room_navigation() {
    for curated in [false, true] {
        let mut app = app(view(Mode::ExquisiteCorpse, Phase::Reveal));
        app.ui.komaphone.enabled = curated;
        app.ui.rooms.open = true;
        app.ui.rooms.view_reveal = false;
        let ctx = egui::Context::default();
        sync(&mut app, &ctx);
        assert!(app.ui.rooms.view_reveal);
        assert_eq!(app.ui.rooms.open, !curated);
        assert_eq!(app.ui.rooms.announced_reveal, Some(game()));
        assert_eq!(app.ui.rooms.restored_reveal, None, "Announcing a result is independent from rendering it");
        app.ui.rooms.open = true;
        app.ui.rooms.view_reveal = false;
        sync(&mut app, &ctx);
        assert!(app.ui.rooms.open);
        assert!(!app.ui.rooms.view_reveal, "An explicit return to local drawings must stick");
    }
}

#[test]
fn restart_restores_both_mode_positions_from_durable_state_and_keeps_notes_closed() {
    for mode in [Mode::Animation { rotations: 2 }, Mode::ExquisiteCorpse] {
        let mut app = app(view(mode, Phase::Reveal));
        let position =
            RevealPosition { game: game(), chain: 1, frame: if mode == Mode::ExquisiteCorpse { 0 } else { 2 }, fps: 13, looping: false, visible_parts: 2 };
        app.session.rooms.status.reveal_position = Some(position.clone());
        app.ui.rooms.animation.chain = 0;
        app.ui.rooms.animation.frame = 0;
        app.ui.rooms.animation.show_notes = true;
        app.ui.rooms.animation.playing = true;
        app.ui.rooms.corpse_view.visible_parts = 3;
        app.ui.rooms.restored_reveal = Some(game());
        app.ui.rooms.announced_reveal = Some(game());
        app.ui.rooms = serde_json::from_value(serde_json::to_value(&app.ui.rooms).unwrap()).unwrap();
        assert!(app.ui.rooms.restored_reveal.is_none());
        assert!(app.ui.rooms.announced_reveal.is_none());
        let initial_status = app.ui.status.clone();
        let ctx = egui::Context::default();
        sync(&mut app, &ctx);
        paint_reveal(&mut app, &ctx, 10.0);
        assert_eq!(app.ui.rooms.animation.chain, 1);
        assert_eq!(app.ui.rooms.animation.frame, usize::from(position.frame));
        assert_eq!(app.ui.rooms.animation.fps, 13);
        assert!(!app.ui.rooms.animation.looping);
        assert!(!app.ui.rooms.animation.playing);
        assert!(!app.ui.rooms.animation.show_notes);
        assert_eq!(app.ui.rooms.corpse_view.chain, 1);
        assert_eq!(app.ui.rooms.corpse_view.visible_parts, 2);
        assert_eq!(app.ui.status, initial_status, "Restored preferences must not write over the durable viewer position");
        assert!(app.ui.rooms.requested_notes.is_none());
    }
}

#[test]
fn delayed_note_ack_preserves_new_typing_and_assignment_change_discards_only_old_view_text() {
    let room = view(Mode::Animation { rotations: 2 }, Phase::Drawing);
    let id = room.assignment.as_ref().unwrap().assignment.id;
    let mut app = app(room.clone());
    app.ui.rooms.note_assignment = Some(id);
    app.ui.rooms.note_scope = Some((app.session.rooms.status.endpoint.clone(), room.id));
    app.ui.rooms.note_ack = "Previously saved".into();
    app.ui.rooms.note = "Newest still-being-typed note".into();
    app.session.rooms.status.note_draft = Some(NoteDraft { assignment: id, note: "Intermediate acknowledged text".into() });
    app.session.rooms.status.pending = 1;
    let ctx = egui::Context::default();
    ctx.run_ui(Default::default(), |ui| note_editor(&mut app, ui, &room)).textures_delta.clear();
    assert_eq!(app.ui.rooms.note, "Newest still-being-typed note");
    assert_eq!(app.ui.rooms.note_ack, "Intermediate acknowledged text");
    let mut next = room;
    next.round = 2;
    next.assignment = Some(AssignmentView { assignment: assignment(0, 2), submitted: false, reference: Some(Reference::Missing) });
    ctx.run_ui(Default::default(), |ui| note_editor(&mut app, ui, &next)).textures_delta.clear();
    assert_eq!(app.ui.rooms.note_assignment, Some(assignment(0, 2).id));
    assert!(app.ui.rooms.note.is_empty());
    assert_eq!(
        app.session.rooms.status.note_draft.as_ref().unwrap().note,
        "Intermediate acknowledged text",
        "Old acknowledged text remains in durable state until explicitly replaced"
    );
}

#[test]
fn submission_note_is_scoped_to_animation_and_the_current_assignment() {
    let room = view(Mode::Animation { rotations: 2 }, Phase::Drawing);
    let id = room.assignment.as_ref().unwrap().assignment.id;
    let mut app = app(room.clone());
    app.ui.rooms.note_assignment = Some(id);
    app.ui.rooms.note_scope = Some((app.session.rooms.status.endpoint.clone(), room.id));
    app.ui.rooms.note = "Newest unsaved Animation note".into();
    app.session.rooms.status.note_draft = Some(NoteDraft { assignment: id, note: "Durable Animation note".into() });
    assert_eq!(submission_note(&app, &room), "Newest unsaved Animation note");

    let corpse = view(Mode::ExquisiteCorpse, Phase::Drawing);
    assert!(submission_note(&app, &corpse).is_empty(), "Mode changes must not silently attach an old Animation note");

    app.ui.rooms.note_assignment = Some(assignment(0, 0).id);
    assert_eq!(submission_note(&app, &room), "Durable Animation note", "Stale view text must yield to the current durable draft");
    app.session.rooms.status.note_draft.as_mut().unwrap().assignment = assignment(0, 0).id;
    assert!(submission_note(&app, &room).is_empty(), "A previous assignment's view and durable text are both out of scope");
    app.ui.rooms.note_assignment = Some(id);
    app.ui.rooms.note.clear();
    assert!(submission_note(&app, &room).is_empty(), "An intentionally cleared current note remains empty");
    let mut lobby = room;
    lobby.assignment = None;
    assert!(submission_note(&app, &lobby).is_empty());
}

#[test]
fn reused_assignment_in_another_room_or_service_cannot_republish_old_view_text() {
    for change_service in [false, true] {
        let mut room = view(Mode::Animation { rotations: 2 }, Phase::Drawing);
        let id = room.assignment.as_ref().unwrap().assignment.id;
        let mut app = app(room.clone());
        app.session.rooms.status.endpoint = Some("http://127.0.0.1:43127".into());
        app.ui.rooms.note_assignment = Some(id);
        app.ui.rooms.note_scope = Some((app.session.rooms.status.endpoint.clone(), room.id));
        app.ui.rooms.note = "Private text from the previous room".into();
        app.ui.rooms.note_ack = app.ui.rooms.note.clone();
        if change_service {
            app.session.rooms.status.endpoint = Some("http://127.0.0.1:43128".into());
        } else {
            room.id = RoomId::new(&"c".repeat(32)).unwrap();
        }
        app.session.rooms.status.room = Some(room.clone());
        assert!(submission_note(&app, &room).is_empty());
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |ui| note_editor(&mut app, ui, &room)).textures_delta.clear();
        assert!(app.ui.rooms.note.is_empty());
        assert!(app.ui.rooms.note_ack.is_empty());
        assert!(submission_note(&app, &room).is_empty());
    }
}

#[test]
fn playback_checkpoints_are_throttled_but_pause_saves_the_chosen_frame_immediately() {
    let mut app = app(view(Mode::Animation { rotations: 2 }, Phase::Reveal));
    app.ui.rooms.restored_reveal = Some(game());
    app.ui.rooms.animation.game = Some(game());
    app.ui.rooms.animation.frame = 1;
    app.ui.rooms.animation.playing = true;
    app.ui.rooms.last_reveal_save_time = 10.0;
    app.session.rooms.status.reveal_position = Some(RevealPosition { game: game(), chain: 0, frame: 0, fps: 6, looping: true, visible_parts: 1 });
    let ctx = egui::Context::default();
    let initial_status = app.ui.status.clone();
    paint_reveal(&mut app, &ctx, 10.2);
    assert_eq!(app.ui.rooms.last_reveal_save_time, 10.0);
    assert_eq!(app.ui.status, initial_status, "No room worker is configured: no save should have been attempted yet");
    app.ui.rooms.animation.pause();
    paint_reveal(&mut app, &ctx, 10.3);
    assert_eq!(app.ui.rooms.last_reveal_save_time, 10.3, "Pausing makes the pending position eligible immediately");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn submission_error_renders_in_reference_panel_and_room_window_until_dismissed() {
    use egui_kittest::kittest::Queryable;

    let directory = tempfile::tempdir().unwrap();
    let room = view(Mode::ExquisiteCorpse, Phase::Drawing);
    let assigned_project = room.assignment.as_ref().unwrap().assignment.project;
    let mut app = app(room.clone());
    app.session.enable_local_projects(directory.path().join("drafts.sqlite3"), Default::default()).unwrap();
    app.session.flush_local_projects(std::time::Duration::from_secs(20)).unwrap();
    app.session.rooms.status.connected = true;
    app.session.rooms.status.paired = true;
    app.session.open_room_draft().unwrap();
    app.session.flush_local_projects(std::time::Duration::from_secs(20)).unwrap();
    let doc_id = app.session.active().unwrap().doc.id;
    assert_eq!(app.session.local_projects.status(doc_id).project, Some(assigned_project));

    let error_text =
        "This drawing exceeds the 64 MiB single-object upload limit; merge or simplify heavy imported layers in your saved local draft and try again.";
    app.session.rooms.set_submission_error(error_text);
    assert_eq!(app.session.room_sync_label(doc_id), "Submission failed · local draft retained");

    // 1. With `Private room` closed (normal drawing view), `reference_panel` + `chrome` render the error and Dismiss button.
    app.ui.rooms.open = false;
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1280.0, 800.0)).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            crate::komaphone_workspace::chrome(app, ui);
            reference_panel(app, ui);
        },
        app,
    );
    h.run_steps(3);
    assert!(h.query_by_label(error_text).is_some(), "reference_panel must display submission error while Private room is closed");
    assert!(h.query_by_label("Submission failed · local draft retained").is_some(), "footer must show honest failed sync label");
    assert!(crate::komaphone_workspace::control_rect(&h.ctx, "komaphone.room.dismissError").is_some());

    // 2. Opening `Private room` moves the control ownership into the modal without duplicating covered controls.
    h.state_mut().ui.rooms.open = true;
    h.run_steps(3);
    assert!(h.query_by_label(error_text).is_some(), "Private room window also displays persistent submission error");
    assert!(crate::komaphone_workspace::control_rect(&h.ctx, "komaphone.room.dismissError").is_some());

    // 3. Clicking `Dismiss` clears the error in both views and restores `Local draft · not submitted`.
    h.state_mut().ui.rooms.open = false;
    h.run_steps(2);
    h.get_by_label("Dismiss").click();
    h.run_steps(2);
    assert!(h.state().session.rooms.submission_error().is_none());
    assert!(h.query_by_label(error_text).is_none());
    assert!(h.query_by_label("Local draft · not submitted").is_some());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn capture_submission_error_ui_fixtures() {
    let Some(out_dir) = std::env::var_os("KOMAPHONE_SUBMISSION_ERROR_SCREENSHOTS").map(std::path::PathBuf::from) else {
        return;
    };
    let _gpu = photocraft_testkit::gpu_lock();
    std::fs::create_dir_all(&out_dir).unwrap();

    let directory = tempfile::tempdir().unwrap();
    let room = view(Mode::ExquisiteCorpse, Phase::Drawing);
    let mut app = app(room);
    app.session.enable_local_projects(directory.path().join("drafts.sqlite3"), Default::default()).unwrap();
    app.session.flush_local_projects(std::time::Duration::from_secs(20)).unwrap();
    app.session.rooms.status.connected = true;
    app.session.rooms.status.paired = true;
    app.session.rooms.status.polling = true;
    app.session.open_room_draft().unwrap();
    app.session.flush_local_projects(std::time::Duration::from_secs(20)).unwrap();
    app.session
        .execute("paint.stroke", serde_json::json!({"points": [[180, 120], [520, 280], [840, 420]], "size": 22, "color": [0.2, 0.55, 0.85, 1.0], "seed": 7}))
        .unwrap();
    app.session.flush_local_projects(std::time::Duration::from_secs(20)).unwrap();

    let error_text =
        "This drawing exceeds the 64 MiB single-object upload limit; merge or simplify heavy imported layers in your saved local draft and try again.";
    app.session.rooms.set_submission_error(error_text);
    app.ui.rooms.open = false;

    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1280.0, 800.0)).with_pixels_per_point(1.0).wgpu().build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            crate::komaphone_workspace::chrome(app, ui);
            reference_panel(app, ui);
            egui::CentralPanel::default().show(ui, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.label("Assigned Exquisite Corpse canvas (1024 × 512 · Saved locally)");
                });
            });
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::Studio);
    h.run_steps(5);
    h.render().unwrap().save(out_dir.join("01-workspace-submission-error.png")).unwrap();

    h.state_mut().ui.rooms.open = true;
    h.run_steps(5);
    h.render().unwrap().save(out_dir.join("02-room-window-submission-error.png")).unwrap();

    h.state_mut().ui.rooms.open = false;
    h.run_steps(3);
    egui_kittest::kittest::Queryable::get_by_label(&h, "Dismiss").click();
    h.run_steps(5);
    h.render().unwrap().save(out_dir.join("03-workspace-after-dismiss.png")).unwrap();
}
