//! Thin party view integration. Textures are disposable and follow current grants.
use crate::{PhotocraftApp, komaphone_rooms::button};
use egui::{TextureHandle, TextureId, Vec2};
use photocraft_engine::party::room::{Phase, Reference, RevealedEntry, RoomView};
use photocraft_engine::party::{AssetHash, Mode};
use serde_json::json;
use std::collections::BTreeMap;

#[derive(Clone, Default)]
struct Textures(BTreeMap<AssetHash, TextureHandle>);

pub fn sync(app: &mut PhotocraftApp, ctx: &egui::Context) {
    app.session.rooms.media.reconcile(&app.session.rooms.status);
    let reveal_game = app.session.rooms.status.room.as_ref().and_then(|room| room.reveal.first()).map(|entry| match entry {
        RevealedEntry::Artwork { assignment, .. } | RevealedEntry::Missing { assignment, .. } => assignment.id.game,
    });
    if reveal_game.is_some() && reveal_game != app.ui.rooms.announced_reveal {
        app.ui.rooms.announced_reveal = reveal_game;
        app.ui.rooms.view_reveal = true;
        if app.ui.komaphone.enabled {
            app.ui.rooms.open = false;
        }
    }
    let key = egui::Id::unique("komaphone-party-textures");
    let mut textures = ctx.data_mut(|data| data.remove_temp::<Textures>(key)).unwrap_or_default();
    textures.0.retain(|hash, _| app.session.rooms.media.images.contains_key(hash));
    for (hash, image) in &app.session.rooms.media.images {
        textures.0.entry(*hash).or_insert_with(|| {
            ctx.load_texture(
                format!("party-{}", hash.as_str()),
                egui::ColorImage::from_rgba_unmultiplied([image.width as usize, image.height as usize], &image.rgba),
                egui::TextureOptions::LINEAR,
            )
        });
    }
    ctx.data_mut(|data| data.insert_temp(key, textures));
}
fn texture(ctx: &egui::Context, hash: AssetHash) -> Option<(TextureId, Vec2)> {
    ctx.data(|data| data.get_temp::<Textures>(egui::Id::unique("komaphone-party-textures")))
        .and_then(|textures| textures.0.get(&hash).map(|texture| (texture.id(), texture.size_vec2())))
}
pub fn cached_hashes(ctx: &egui::Context) -> Vec<AssetHash> {
    ctx.data(|data| data.get_temp::<Textures>(egui::Id::unique("komaphone-party-textures")))
        .map(|textures| textures.0.keys().copied().collect())
        .unwrap_or_default()
}
fn run(app: &mut PhotocraftApp, id: &str, params: serde_json::Value) {
    if let Err(error) = app.run(id, params) {
        app.ui.status = error;
    }
}
fn is_assigned(app: &PhotocraftApp) -> bool {
    app.session
        .rooms
        .status
        .room
        .as_ref()
        .and_then(|room| room.assignment.as_ref())
        .zip(app.session.active())
        .is_some_and(|(assignment, document)| app.session.local_projects.status(document.doc.id).project == Some(assignment.assignment.project))
}
pub fn submission_note(app: &PhotocraftApp, room: &RoomView) -> String {
    if !matches!(room.mode, Mode::Animation { .. }) {
        return String::new();
    }
    let Some(assignment) = &room.assignment else { return String::new() };
    if app.ui.rooms.note_assignment == Some(assignment.assignment.id)
        && app.ui.rooms.note_scope.as_ref().is_some_and(|(endpoint, id)| endpoint == &app.session.rooms.status.endpoint && *id == room.id)
    {
        return app.ui.rooms.note.clone();
    }
    app.session
        .rooms
        .status
        .note_draft
        .as_ref()
        .filter(|draft| draft.assignment == assignment.assignment.id)
        .map(|draft| draft.note.clone())
        .unwrap_or_default()
}

pub fn reference_panel(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    // The room window owns the assignment/note controls while it is open. Avoid
    // duplicate controls behind it (and an automation rectangle for a covered button).
    if app.ui.rooms.open {
        return;
    }
    if !is_assigned(app) {
        if let Some(room) = app
            .session
            .rooms
            .status
            .room
            .as_ref()
            .filter(|room| room.phase == Phase::Drawing && room.assignment.as_ref().is_some_and(|assignment| !assignment.submitted))
        {
            let round = room.round;
            egui::Panel::top("komaphone-next-assignment").show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.strong(format!("Round {} · your assigned drawing is ready", u16::from(round) + 1));
                    button(app, ui, "Open assigned drawing", "komaphone.room.openDraft", json!({}));
                });
            });
        }
        return;
    }
    let Some(room) = app.session.rooms.status.room.clone() else { return };
    if !matches!(room.phase, Phase::Drawing | Phase::Transfer) {
        return;
    }
    egui::Panel::left("komaphone-reference-panel").resizable(true).default_size(240.0).size_range(210.0..=360.0).show(ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.strong(format!("Round {}", u16::from(room.round) + 1));
            if let Some(actor) = app.session.rooms.status.profile.as_ref().map(|p| p.id) {
                match room.mode {
                    Mode::Animation { .. } => {
                        let previous = match room.assignment.as_ref().and_then(|a| a.reference.as_ref()) {
                            Some(Reference::Animation { image, .. }) => texture(ui.ctx(), image.hash),
                            _ => None,
                        };
                        let enabled = app.session.is_enabled("komaphone.animation.copyPrevious");
                        if crate::komaphone_animation::reference(ui, &mut app.ui.rooms.animation, &room, actor, previous, enabled) {
                            run(app, "komaphone.animation.copyPrevious", json!({}));
                        }
                    }
                    Mode::ExquisiteCorpse => crate::komaphone_corpse::seam_caption(
                        ui,
                        room.round,
                        matches!(room.assignment.as_ref().and_then(|a| a.reference.as_ref()), Some(Reference::Missing)),
                    ),
                }
            }
            note_editor(app, ui, &room);
            if let Some(error) = &app.session.rooms.media.error {
                ui.label(error);
            }
            ui.separator();
            if room.assignment.as_ref().is_some_and(|a| a.submitted) {
                ui.strong("Submission accepted · waiting for friends");
            } else {
                button(app, ui, "Submit saved drawing", "komaphone.room.submit", json!({"note":submission_note(app,&room)}));
            }
            if !app.session.rooms.status.connected {
                ui.label("Service unavailable · your drawing stays saved locally");
            }
        });
    });
}

pub fn note_editor(app: &mut PhotocraftApp, ui: &mut egui::Ui, room: &RoomView) {
    if !matches!(room.mode, Mode::Animation { .. }) {
        return;
    }
    let Some(assignment) = room.assignment.as_ref() else { return };
    let scope = (app.session.rooms.status.endpoint.clone(), room.id);
    if app.ui.rooms.note_assignment != Some(assignment.assignment.id) || app.ui.rooms.note_scope.as_ref() != Some(&scope) {
        app.ui.rooms.note_assignment = Some(assignment.assignment.id);
        app.ui.rooms.note_scope = Some(scope);
        app.ui.rooms.note = app
            .session
            .rooms
            .status
            .note_draft
            .as_ref()
            .filter(|note| note.assignment == assignment.assignment.id)
            .map(|note| note.note.clone())
            .unwrap_or_default();
        app.ui.rooms.note_ack = app.ui.rooms.note.clone();
    }
    if let Some(saved) = app.session.rooms.status.note_draft.as_ref().filter(|draft| draft.assignment == assignment.assignment.id)
        && saved.note != app.ui.rooms.note_ack
    {
        if app.ui.rooms.note == app.ui.rooms.note_ack {
            app.ui.rooms.note = saved.note.clone();
        }
        app.ui.rooms.note_ack = saved.note.clone();
    }
    ui.separator();
    ui.label("Note for the next artist (optional)");
    let enabled = room.phase == Phase::Drawing && !assignment.submitted;
    let response = ui.add_enabled(enabled, egui::TextEdit::multiline(&mut app.ui.rooms.note).desired_rows(2).char_limit(256));
    crate::komaphone_workspace::remember(ui, "komaphone.room.note", &response);
    let saved =
        app.session.rooms.status.note_draft.as_ref().is_some_and(|draft| draft.assignment == assignment.assignment.id && draft.note == app.ui.rooms.note);
    if enabled && !saved && !app.session.rooms.busy() {
        run(app, "komaphone.room.setNote", json!({"note":app.ui.rooms.note}));
    }
    ui.weak(if saved { "Note saved locally · excluded from artwork" } else { "Saving note locally…" });
}

pub fn overlay(app: &PhotocraftApp, painter: &egui::Painter, xf: &crate::canvas::ViewXform) {
    if !is_assigned(app) {
        return;
    }
    let Some(room) = app.session.rooms.status.room.as_ref() else { return };
    if !matches!(room.phase, Phase::Drawing | Phase::Transfer) {
        return;
    }
    match room.assignment.as_ref().and_then(|a| a.reference.as_ref()) {
        Some(Reference::Animation { image, .. }) => {
            if let Some((texture, _)) = texture(painter.ctx(), image.hash) {
                crate::komaphone_animation::onion_overlay(painter, &app.ui.rooms.animation, xf, texture)
            }
        }
        Some(Reference::Seam { image, .. }) => {
            if let Some((texture, _)) = texture(painter.ctx(), image.hash) {
                crate::komaphone_corpse::seam_overlay(painter, xf, texture)
            }
        }
        _ => {}
    }
}

pub fn has_reveal(app: &PhotocraftApp) -> bool {
    app.ui.rooms.view_reveal
        && app.session.rooms.status.room.as_ref().is_some_and(|room| !room.reveal.is_empty() && matches!(room.phase, Phase::Reveal | Phase::Archived))
}
pub fn reveal(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let Some(room) = app.session.rooms.status.room.clone() else { return };
    let Some(game) = room.reveal.first().map(|entry| match entry {
        RevealedEntry::Artwork { assignment, .. } | RevealedEntry::Missing { assignment, .. } => assignment.id.game,
    }) else {
        return;
    };
    if app.ui.rooms.restored_reveal != Some(game) {
        app.ui.rooms.animation.game = Some(game);
        app.ui.rooms.animation.pause();
        app.ui.rooms.animation.show_notes = false;
        app.ui.rooms.corpse_view.game = Some(game);
        if let Some(position) = app.session.rooms.status.reveal_position.as_ref().filter(|p| p.game == game) {
            app.ui.rooms.animation.chain = position.chain;
            app.ui.rooms.animation.frame = usize::from(position.frame);
            app.ui.rooms.animation.fps = position.fps;
            app.ui.rooms.animation.looping = position.looping;
            app.ui.rooms.corpse_view.chain = position.chain;
            app.ui.rooms.corpse_view.visible_parts = position.visible_parts;
        } else {
            app.ui.rooms.animation.chain = 0;
            app.ui.rooms.animation.frame = 0;
            app.ui.rooms.corpse_view.chain = 0;
            app.ui.rooms.corpse_view.visible_parts = 1;
        }
        app.ui.rooms.restored_reveal = Some(game);
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.heading("Your finished game");
        ui.horizontal_wrapped(|ui| {
            button(app, ui, "Archive game", "komaphone.room.archive", json!({}));
            button(app, ui, "Next game", "komaphone.room.next", json!({}));
            if ui.button("Return to my local drawings").clicked() {
                app.ui.rooms.view_reveal = false;
            }
        });
        let ctx = ui.ctx().clone();
        let show_notes = match room.mode {
            Mode::Animation { .. } => crate::komaphone_animation::reveal(ui, &mut app.ui.rooms.animation, &room, |hash| texture(&ctx, hash)),
            Mode::ExquisiteCorpse => {
                crate::komaphone_corpse::reveal(ui, &room, &mut app.ui.rooms.corpse_view, &mut |hash| texture(&ctx, hash));
                false
            }
        };
        if show_notes {
            if app.ui.rooms.requested_notes != Some((room.id, game, room.epoch)) && !app.session.rooms.busy() {
                run(app, "komaphone.reveal.notes", json!({}));
                app.ui.rooms.requested_notes = Some((room.id, game, room.epoch));
            }
            for (assignment, note) in &app.session.rooms.media.notes {
                if assignment.chain == app.ui.rooms.animation.chain && !note.is_empty() {
                    ui.label(format!("Frame {}: {}", u16::from(assignment.round) + 1, note));
                }
            }
            if !app.session.rooms.media.notes_loaded(room.id, room.epoch) {
                ui.label("Handoff notes have not loaded yet.");
                if ui.add_enabled(!app.session.rooms.busy(), egui::Button::new("Retry notes")).clicked() {
                    app.ui.rooms.requested_notes = None;
                }
            }
        } else {
            app.ui.rooms.requested_notes = None;
        }
        if let Some(error) = &app.session.rooms.media.error {
            ui.label(error);
        }
    });
    let (chain, frame) = match room.mode {
        Mode::Animation { .. } => (app.ui.rooms.animation.chain, app.ui.rooms.animation.frame as u8),
        Mode::ExquisiteCorpse => (app.ui.rooms.corpse_view.chain, 0),
    };
    let position = json!({"game":game,"chain":chain,"frame":frame,"fps":app.ui.rooms.animation.fps,"looping":app.ui.rooms.animation.looping,"visible_parts":app.ui.rooms.corpse_view.visible_parts});
    let saved = app.session.rooms.status.reveal_position.as_ref().and_then(|p| serde_json::to_value(p).ok());
    let now = ui.input(|input| input.time);
    let playback_due = !app.ui.rooms.animation.playing || now - app.ui.rooms.last_reveal_save_time >= 1.0;
    if saved.as_ref() != Some(&position) && !app.session.rooms.busy() && playback_due {
        run(app, "komaphone.reveal.position", position);
        app.ui.rooms.last_reveal_save_time = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_engine::party::room::{Assignment, AssignmentView, MemberView};
    use photocraft_engine::party::{AssignmentId, DeviceId, GameId, Participant, ParticipantId, ProjectId, RoomId};
    #[test]
    fn reopened_note_uses_acknowledged_storage_instead_of_stale_ui_preferences() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let initial_status = app.ui.status.clone();
        let actor =
            Participant { id: ParticipantId::new(&"1".repeat(32)).unwrap(), device: DeviceId::new(&"2".repeat(32)).unwrap(), display_name: "Artist".into() };
        let assignment = AssignmentId { game: GameId::new(&"3".repeat(32)).unwrap(), chain: 0, round: 0 };
        let room = RoomView {
            id: RoomId::new(&"4".repeat(32)).unwrap(),
            revision: 1,
            epoch: 1,
            phase: Phase::Drawing,
            round: 0,
            mode: Mode::Animation { rotations: 2 },
            timer_ms: None,
            moderator: actor.id,
            members: vec![MemberView { participant: actor.clone(), connected: true, ready: true }],
            waiting: vec![],
            paused: None,
            deadline_ms: None,
            assignment: Some(AssignmentView {
                assignment: Assignment { id: assignment, player: actor.id, project: ProjectId::new(&"5".repeat(32)).unwrap() },
                submitted: false,
                reference: None,
            }),
            reveal: vec![],
        };
        app.ui.rooms.note = "Older preference snapshot".into();
        app.ui.rooms.note_assignment = Some(assignment);
        app.ui.rooms = serde_json::from_value(serde_json::to_value(&app.ui.rooms).unwrap()).unwrap();
        app.session.rooms.status.profile = Some(actor);
        app.session.rooms.status.room = Some(room.clone());
        app.session.rooms.status.note_draft = Some(photocraft_engine::rooms::NoteDraft { assignment, note: "Newer acknowledged note".into() });
        let ctx = egui::Context::default();
        ctx.run_ui(Default::default(), |ui| note_editor(&mut app, ui, &room)).textures_delta.clear();
        assert_eq!(app.ui.rooms.note, "Newer acknowledged note");
        assert_eq!(app.ui.rooms.note_ack, app.ui.rooms.note);
        assert_eq!(app.ui.status, initial_status, "A stale preference must not enqueue a write over acknowledged text");
    }
}

#[cfg(test)]
#[path = "komaphone_game_view_tests.rs"]
mod independent_tests;
