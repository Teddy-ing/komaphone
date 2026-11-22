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

#[cfg(not(target_arch = "wasm32"))]
fn synthetic_corpse_band(chain: u8, round: u8) -> RoomImage {
    use photocraft_engine::rooms::corpse::{BAND_HEIGHT, WIDTH};
    let bg = match (chain, round) {
        (0, 0) => [235, 110, 95, 255],
        (0, 1) => [242, 185, 78, 255],
        (0, 2) => [175, 102, 72, 255],
        (1, 0) => [92, 175, 240, 255],
        (1, 1) => [68, 192, 176, 255],
        (1, 2) => [66, 98, 178, 255],
        (_, 0) => [112, 206, 124, 255],
        (_, 1) => [168, 202, 82, 255],
        (_, _) => [156, 108, 204, 255],
    };
    let accent = [28, 32, 42, 255];
    let light = [248, 248, 244, 255];
    let w = WIDTH as usize;
    let h = BAND_HEIGHT as usize;
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let mut color = bg;
            let border = x < 18 || x >= w - 18 || y < 14 || y >= h - 14;
            if border {
                color = accent;
            } else {
                match round {
                    // Head: circle + two eyes + smile bar
                    0 => {
                        let dx = x as i32 - 512;
                        let dy = y as i32 - 260;
                        let r2 = dx * dx + dy * dy;
                        if r2 < 170 * 170 {
                            color = light;
                        }
                        let eye_l = (x as i32 - 445).pow(2) + (y as i32 - 220).pow(2) < 24 * 24;
                        let eye_r = (x as i32 - 579).pow(2) + (y as i32 - 220).pow(2) < 24 * 24;
                        let mouth = (430..=594).contains(&x) && (310..=338).contains(&y);
                        if eye_l || eye_r || mouth {
                            color = accent;
                        }
                    }
                    // Torso: central vest + arms
                    1 => {
                        let vest = (330..=694).contains(&x) && (40..=472).contains(&y);
                        let arms = (130..=894).contains(&x) && (120..=220).contains(&y);
                        if vest || arms {
                            color = light;
                        }
                        if (480..=544).contains(&x) && (120..=420).contains(&y) {
                            color = accent;
                        }
                    }
                    // Legs: two vertical legs + boots
                    _ => {
                        let leg_l = (330..=465).contains(&x) && (20..=450).contains(&y);
                        let leg_r = (559..=694).contains(&x) && (20..=450).contains(&y);
                        if leg_l || leg_r {
                            color = light;
                        }
                        let boot_l = (290..=465).contains(&x) && (385..=465).contains(&y);
                        let boot_r = (559..=734).contains(&x) && (385..=465).contains(&y);
                        if boot_l || boot_r {
                            color = accent;
                        }
                    }
                }
            }
            let idx = (y * w + x) * 4;
            rgba[idx..idx + 4].copy_from_slice(&color);
        }
    }
    RoomImage { width: WIDTH, height: BAND_HEIGHT, rgba }
}

#[cfg(not(target_arch = "wasm32"))]
fn corpse_room_with_artwork(game_byte: u8, players: u8) -> (RoomView, Vec<(AssetHash, Arc<RoomImage>)>) {
    let game_id = GameId::new(&format!("{game_byte:032x}")).unwrap();
    let names = ["Alice", "Bob", "Cara"];
    let members: Vec<MemberView> = (0..players)
        .map(|index| MemberView {
            participant: Participant {
                id: ParticipantId::new(&format!("{:032x}", index + 1)).unwrap(),
                device: DeviceId::new(&format!("{:032x}", index + 11)).unwrap(),
                display_name: names[usize::from(index)].into(),
            },
            connected: true,
            ready: true,
        })
        .collect();
    let mut reveal = Vec::new();
    let mut images = Vec::new();
    for chain in 0..players {
        for round in 0..3u8 {
            let asset_num = game_byte.wrapping_mul(16).wrapping_add(chain * 3 + round + 1);
            let image_ref = asset(asset_num);
            let ckpt_ref = asset(asset_num.wrapping_add(64));
            let project_id = ProjectId::new(&format!("{:032x}", u16::from(game_byte) * 100 + u16::from(chain) * 12 + u16::from(round) + 1)).unwrap();
            let assignment = Assignment {
                id: AssignmentId { game: game_id, chain, round },
                player: members[usize::from((chain + round) % players)].participant.id,
                project: project_id,
            };
            let ckpt =
                photocraft_engine::party::CheckpointRef { id: photocraft_engine::party::CheckpointId::new(project_id.as_str()).unwrap(), asset: ckpt_ref };
            reveal.push(RevealedEntry::Artwork {
                assignment,
                revision: 1,
                image: image_ref,
                checkpoint: ckpt,
                replay: Box::new(photocraft_engine::party::ReplayRef {
                    id: photocraft_engine::party::ReplayId::new(project_id.as_str()).unwrap(),
                    manifest: None,
                    baseline: ckpt,
                    end_event: None,
                    event_count: 0,
                    duration_ms: 0,
                }),
                assets: vec![image_ref, ckpt_ref],
            });
            images.push((image_ref.hash, Arc::new(synthetic_corpse_band(chain, round))));
        }
    }
    let room = RoomView {
        id: RoomId::new(&"b".repeat(32)).unwrap(),
        revision: 20,
        epoch: 3,
        phase: Phase::Reveal,
        round: 2,
        mode: Mode::ExquisiteCorpse,
        timer_ms: None,
        moderator: members[0].participant.id,
        members,
        waiting: vec![],
        paused: None,
        deadline_ms: None,
        assignment: None,
        reveal,
    };
    (room, images)
}

#[cfg(not(target_arch = "wasm32"))]
fn assemble_corpse_rgba(room: &RoomView, chain_idx: u8, imgs: &std::collections::BTreeMap<AssetHash, Arc<RoomImage>>) -> Vec<u8> {
    let chain = photocraft_engine::rooms::corpse::reveal_chain(room, chain_idx).unwrap();
    let parts = chain.parts.each_ref().map(|part| part.image.and_then(|img| imgs.get(&img.hash).map(|arc| arc.as_ref())));
    photocraft_engine::rooms::corpse::assemble(parts).unwrap().rgba
}

#[cfg(not(target_arch = "wasm32"))]
fn verify_corpse_sequential_switch(h: &mut egui_kittest::Harness<'_, PhotocraftApp>, players: u8) {
    use egui_kittest::kittest::Queryable;

    let c1_legs = if players == 2 { "Legs and feet · Alice" } else { "Legs and feet · Cara" };
    let c2_torso = if players == 2 { "Torso and arms · Alice" } else { "Torso and arms · Cara" };
    let c2_legs = if players == 2 { "Legs and feet · Bob" } else { "Legs and feet · Alice" };

    // Creature 1 starts at Head only with notes absent.
    assert_eq!((h.state().ui.rooms.corpse_view.chain, h.state().ui.rooms.corpse_view.visible_parts), (0, 1));
    assert!(h.query_by_label("Head · Alice").is_some());
    assert!(h.query_by_label("Torso and arms · Bob").is_none());
    assert!(h.query_by_label("Open handoff notes").is_none());
    assert!(h.state().ui.rooms.requested_notes.is_none());

    // Progress Creature 1 sequentially: Head -> Add torso -> Full creature.
    h.get_by_label("Add torso").click();
    h.run_steps(2);
    assert_eq!((h.state().ui.rooms.corpse_view.chain, h.state().ui.rooms.corpse_view.visible_parts), (0, 2));
    assert!(h.query_by_label("Torso and arms · Bob").is_some());
    assert!(h.query_by_label(c1_legs).is_none());

    h.get_by_label("Full creature").click();
    h.run_steps(2);
    assert_eq!((h.state().ui.rooms.corpse_view.chain, h.state().ui.rooms.corpse_view.visible_parts), (0, 3));
    assert!(h.query_by_label(c1_legs).is_some());

    // Clicking the already-selected creature preserves its progress.
    h.get_by_label("Creature 1").click();
    h.run_steps(2);
    assert_eq!((h.state().ui.rooms.corpse_view.chain, h.state().ui.rooms.corpse_view.visible_parts), (0, 3));

    // Selecting Creature 2 immediately restarts at Head (visible_parts == 1).
    h.get_by_label("Creature 2").click();
    h.run_steps(2);
    assert_eq!((h.state().ui.rooms.corpse_view.chain, h.state().ui.rooms.corpse_view.visible_parts), (1, 1));
    assert!(h.query_by_label("Head · Bob").is_some());
    assert!(h.query_by_label(c2_torso).is_none());
    assert!(h.query_by_label(c2_legs).is_none());

    // Creature 2 progresses Head -> Add torso cleanly.
    h.get_by_label("Add torso").click();
    h.run_steps(2);
    assert_eq!((h.state().ui.rooms.corpse_view.chain, h.state().ui.rooms.corpse_view.visible_parts), (1, 2));
    assert!(h.query_by_label(c2_torso).is_some());
    assert!(h.query_by_label(c2_legs).is_none());
}

#[cfg(not(target_arch = "wasm32"))]
fn verify_corpse_rapid_and_game_switch(h: &mut egui_kittest::Harness<'_, PhotocraftApp>, players: u8) {
    use egui_kittest::kittest::Queryable;

    // Switching mid-reveal (from torso = 2) resets the destination creature to Head, and explicit Full creature works directly.
    let (next_label, next_chain, next_head, next_torso, next_legs) = if players == 3 {
        ("Creature 3", 2u8, "Head · Cara", "Torso and arms · Alice", "Legs and feet · Bob")
    } else {
        ("Creature 1", 0u8, "Head · Alice", "Torso and arms · Bob", "Legs and feet · Alice")
    };
    h.get_by_label(next_label).click();
    h.run_steps(2);
    assert_eq!((h.state().ui.rooms.corpse_view.chain, h.state().ui.rooms.corpse_view.visible_parts), (next_chain, 1));
    assert!(h.query_by_label(next_head).is_some());
    assert!(h.query_by_label(next_torso).is_none());
    h.get_by_label("Full creature").click();
    h.run_steps(2);
    assert_eq!((h.state().ui.rooms.corpse_view.chain, h.state().ui.rooms.corpse_view.visible_parts), (next_chain, 3));
    assert!(h.query_by_label(next_legs).is_some());

    // Rapid next/previous and backward switches always land at Head (visible_parts == 1).
    let sequence: &[(&str, u8)] = if players == 3 {
        &[("Creature 2", 1), ("Creature 1", 0), ("Creature 3", 2), ("Creature 2", 1), ("Creature 1", 0)]
    } else {
        &[("Creature 2", 1), ("Creature 1", 0), ("Creature 2", 1), ("Creature 1", 0)]
    };
    for &(label, expected_chain) in sequence {
        h.get_by_label("Add torso").click();
        h.run_steps(1);
        assert_eq!(h.state().ui.rooms.corpse_view.visible_parts, 2);
        h.get_by_label(label).click();
        h.run_steps(1);
        assert_eq!((h.state().ui.rooms.corpse_view.chain, h.state().ui.rooms.corpse_view.visible_parts), (expected_chain, 1));
    }

    // Out-of-range stale chain index resets both chain and visible_parts to Head.
    h.state_mut().ui.rooms.corpse_view.chain = players + 4;
    h.state_mut().ui.rooms.corpse_view.visible_parts = 3;
    h.run_steps(1);
    assert_eq!((h.state().ui.rooms.corpse_view.chain, h.state().ui.rooms.corpse_view.visible_parts), (0, 1));

    // Switching to a different revealed game resets chain=0 and visible_parts=1.
    h.get_by_label("Creature 2").click();
    h.run_steps(1);
    h.get_by_label("Full creature").click();
    h.run_steps(1);
    assert_eq!((h.state().ui.rooms.corpse_view.chain, h.state().ui.rooms.corpse_view.visible_parts), (1, 3));
    let (other_room, other_images) = corpse_room_with_artwork(players + 10, players);
    {
        let rooms = &mut h.state_mut().session.rooms;
        rooms.status.room = Some(other_room);
        rooms.media.reconcile(&rooms.status);
        for (hash, img) in &other_images {
            rooms.media.images.insert(*hash, img.clone());
        }
    }
    h.run_steps(2);
    assert_eq!((h.state().ui.rooms.corpse_view.chain, h.state().ui.rooms.corpse_view.visible_parts), (0, 1));
    assert!(h.state().ui.rooms.requested_notes.is_none());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn corpse_reveal_restarts_at_head_when_switching_creatures_in_two_and_three_player_games() {
    for players in [2u8, 3u8] {
        let (room, images) = corpse_room_with_artwork(players, players);
        let original_by_hash: std::collections::BTreeMap<AssetHash, Vec<u8>> = images.iter().map(|(h, img)| (*h, img.rgba.clone())).collect();
        let mut app = app(room.clone());
        app.session.rooms.media.reconcile(&app.session.rooms.status);
        for (hash, img) in &images {
            app.session.rooms.media.images.insert(*hash, img.clone());
        }
        let baseline_assembled: Vec<Vec<u8>> = (0..players).map(|c| assemble_corpse_rgba(&room, c, &app.session.rooms.media.images)).collect();
        app.ui.rooms.view_reveal = true;
        app.ui.rooms.open = false;

        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1280.0, 860.0)).build_ui_state(
            |ui, app: &mut PhotocraftApp| {
                sync(app, ui.ctx());
                crate::komaphone_workspace::chrome(app, ui);
                egui::CentralPanel::default().show(ui, |ui| reveal(app, ui));
            },
            app,
        );
        h.run_steps(3);

        verify_corpse_sequential_switch(&mut h, players);
        verify_corpse_rapid_and_game_switch(&mut h, players);

        // Original artwork bytes and assembled full composition are completely unchanged.
        for (hash, img) in &images {
            assert_eq!(img.rgba.as_slice(), original_by_hash[hash].as_slice());
        }
        let reloaded_map: std::collections::BTreeMap<_, _> = images.into_iter().collect();
        for c in 0..players {
            assert_eq!(assemble_corpse_rgba(&room, c, &reloaded_map), baseline_assembled[usize::from(c)]);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn capture_qol002_corpse_reveal_ui_fixtures() {
    use egui_kittest::kittest::Queryable;

    let Some(out_dir) = std::env::var_os("KOMAPHONE_QOL002_AFTER_SCREENSHOTS").map(std::path::PathBuf::from) else {
        return;
    };
    let _gpu = photocraft_testkit::gpu_lock();
    std::fs::create_dir_all(&out_dir).unwrap();

    // 1. Two-player game: Creature 1 full -> Creature 2 resets to Head -> Add torso -> Full creature.
    let (room_2p, images_2p) = corpse_room_with_artwork(1, 2);
    let mut app_2p = app(room_2p);
    app_2p.session.rooms.media.reconcile(&app_2p.session.rooms.status);
    for (hash, img) in images_2p {
        app_2p.session.rooms.media.images.insert(hash, img);
    }
    app_2p.ui.rooms.view_reveal = true;
    app_2p.ui.rooms.open = false;

    let mut h2 = egui_kittest::Harness::builder().with_size(egui::vec2(1280.0, 860.0)).with_pixels_per_point(1.0).wgpu().build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            sync(app, ui.ctx());
            crate::komaphone_workspace::chrome(app, ui);
            egui::CentralPanel::default().show(ui, |ui| reveal(app, ui));
        },
        app_2p,
    );
    PhotocraftApp::setup_context(&h2.ctx, crate::theme::ThemeKind::Studio);
    h2.run_steps(4);
    h2.get_by_label("Add torso").click();
    h2.run_steps(3);
    h2.get_by_label("Full creature").click();
    h2.run_steps(4);
    assert_eq!((h2.state().ui.rooms.corpse_view.chain, h2.state().ui.rooms.corpse_view.visible_parts), (0, 3));
    h2.render().unwrap().save(out_dir.join("after-01-2p-creature1-full.png")).unwrap();

    h2.get_by_label("Creature 2").click();
    h2.run_steps(4);
    assert_eq!((h2.state().ui.rooms.corpse_view.chain, h2.state().ui.rooms.corpse_view.visible_parts), (1, 1));
    h2.render().unwrap().save(out_dir.join("after-02-2p-creature2-reset-head.png")).unwrap();

    h2.get_by_label("Add torso").click();
    h2.run_steps(4);
    assert_eq!((h2.state().ui.rooms.corpse_view.chain, h2.state().ui.rooms.corpse_view.visible_parts), (1, 2));
    h2.render().unwrap().save(out_dir.join("after-03-2p-creature2-torso.png")).unwrap();

    h2.get_by_label("Full creature").click();
    h2.run_steps(4);
    assert_eq!((h2.state().ui.rooms.corpse_view.chain, h2.state().ui.rooms.corpse_view.visible_parts), (1, 3));
    h2.render().unwrap().save(out_dir.join("after-04-2p-creature2-full.png")).unwrap();

    // 2. Three-player game: Creature 1 full -> Creature 2 mid-reveal (torso) -> Creature 3 resets to Head -> switch back to Creature 1 resets to Head.
    let (room_3p, images_3p) = corpse_room_with_artwork(3, 3);
    let mut app_3p = app(room_3p);
    app_3p.session.rooms.media.reconcile(&app_3p.session.rooms.status);
    for (hash, img) in images_3p {
        app_3p.session.rooms.media.images.insert(hash, img);
    }
    app_3p.ui.rooms.view_reveal = true;
    app_3p.ui.rooms.open = false;

    let mut h3 = egui_kittest::Harness::builder().with_size(egui::vec2(1280.0, 860.0)).with_pixels_per_point(1.0).wgpu().build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            sync(app, ui.ctx());
            crate::komaphone_workspace::chrome(app, ui);
            egui::CentralPanel::default().show(ui, |ui| reveal(app, ui));
        },
        app_3p,
    );
    PhotocraftApp::setup_context(&h3.ctx, crate::theme::ThemeKind::Studio);
    h3.run_steps(4);
    h3.get_by_label("Full creature").click();
    h3.run_steps(3);
    h3.get_by_label("Creature 2").click();
    h3.run_steps(3);
    h3.get_by_label("Add torso").click();
    h3.run_steps(3);
    assert_eq!((h3.state().ui.rooms.corpse_view.chain, h3.state().ui.rooms.corpse_view.visible_parts), (1, 2));
    h3.get_by_label("Creature 3").click();
    h3.run_steps(4);
    assert_eq!((h3.state().ui.rooms.corpse_view.chain, h3.state().ui.rooms.corpse_view.visible_parts), (2, 1));
    h3.render().unwrap().save(out_dir.join("after-05-3p-creature3-reset-head.png")).unwrap();

    h3.get_by_label("Full creature").click();
    h3.run_steps(3);
    h3.get_by_label("Creature 1").click();
    h3.run_steps(4);
    assert_eq!((h3.state().ui.rooms.corpse_view.chain, h3.state().ui.rooms.corpse_view.visible_parts), (0, 1));
    h3.render().unwrap().save(out_dir.join("after-06-3p-switch-back-creature1-head.png")).unwrap();
}
