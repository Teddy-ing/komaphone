//! Private-room presentation. All room mutations use the engine command seam.
use crate::{PhotocraftApp, theme::Tokens};
use egui::RichText;
use photocraft_engine::party::room::{Phase, RoomView};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoomUi {
    pub open: bool,
    pub endpoint: String,
    pub display_name: String,
    pub room_id: String,
    #[serde(skip)]
    pub invitation: String,
    pub corpse: bool,
    pub rotations: u8,
    pub timed: bool,
    pub minutes: u16,
    pub animation: crate::komaphone_animation::AnimationUi,
    pub corpse_view: crate::komaphone_corpse::CorpseUi,
    pub note: String,
    #[serde(skip)]
    pub note_assignment: Option<photocraft_engine::party::AssignmentId>,
    #[serde(skip)]
    pub note_scope: Option<(Option<String>, photocraft_engine::party::RoomId)>,
    #[serde(skip)]
    pub note_ack: String,
    pub view_reveal: bool,
    #[serde(skip)]
    pub restored_reveal: Option<photocraft_engine::party::GameId>,
    #[serde(skip)]
    pub announced_reveal: Option<photocraft_engine::party::GameId>,
    #[serde(skip)]
    pub last_reveal_save_time: f64,
    #[serde(skip)]
    pub requested_notes: Option<(photocraft_engine::party::RoomId, photocraft_engine::party::GameId, u64)>,
}
impl std::fmt::Debug for RoomUi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoomUi").finish_non_exhaustive()
    }
}
impl Default for RoomUi {
    fn default() -> Self {
        Self {
            open: false,
            endpoint: "http://127.0.0.1:43127".into(),
            display_name: String::new(),
            room_id: String::new(),
            invitation: String::new(),
            corpse: false,
            rotations: 2,
            timed: false,
            minutes: 5,
            animation: Default::default(),
            corpse_view: Default::default(),
            note: String::new(),
            note_assignment: None,
            note_scope: None,
            note_ack: String::new(),
            view_reveal: true,
            restored_reveal: None,
            announced_reveal: None,
            last_reveal_save_time: 0.0,
            requested_notes: None,
        }
    }
}
fn run(app: &mut PhotocraftApp, id: &str, params: Value) {
    if let Err(error) = app.run(id, params) {
        app.ui.status = error;
    }
}
pub(crate) fn button(app: &mut PhotocraftApp, ui: &mut egui::Ui, label: &str, id: &str, params: Value) {
    let response = ui.add_enabled(app.session.is_enabled(id), egui::Button::new(label));
    crate::komaphone_workspace::remember(ui, id, &response);
    if response.clicked() {
        run(app, id, params);
        if id == "komaphone.room.openDraft" {
            app.ui.rooms.open = false;
        }
    }
}
pub fn window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.ui.rooms.open {
        return;
    }
    let mut open = true;
    egui::Window::new("Private room").id(egui::Id::unique("komaphone-rooms")).open(&mut open).default_width(650.0).max_height(560.0).show(ctx, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            let tokens = Tokens::get(ctx);
            let status = app.session.rooms.status.clone();
            ui.label(RichText::new("Private games for two or three players").strong());
            ui.label("Your local drawings stay saved when the service is unavailable.");
            ui.separator();
            connection(app, ui);
            if let Some(error) = status.error.as_ref() {
                ui.colored_label(tokens.text, error);
            }
            if status.pending > 0 {
                ui.label(format!("{} request(s) waiting for service confirmation", status.pending));
            }
            if status.blocked {
                button(app, ui, "Retry pending request", "komaphone.room.retry", json!({}));
                button(app, ui, "Keep rejected work and refresh", "komaphone.room.retainRejected", json!({}));
            }
            if let Some(room) = status.room {
                room_view(app, ui, &room);
            } else if status.paired {
                create_join(app, ui);
            }
            ui.separator();
            let response = ui.button("Return to drawing");
            crate::komaphone_workspace::remember(ui, "workspace.return", &response);
            if response.clicked() {
                app.ui.rooms.open = false;
            }
        });
    });
    app.ui.rooms.open &= open;
}
fn connection(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let status = &app.session.rooms.status;
    if status.paired {
        ui.horizontal_wrapped(|ui| {
            ui.strong(status.profile.as_ref().map_or("Your profile", |p| p.display_name.as_str()));
            ui.label(if status.connected { "Connected" } else { "Service unavailable · reconnecting" });
        });
    }
    if !status.paired {
        egui::Grid::new("room-connection").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label("Service address");
            ui.text_edit_singleline(&mut app.ui.rooms.endpoint);
            ui.end_row();
            ui.label("Your name");
            ui.add(egui::TextEdit::singleline(&mut app.ui.rooms.display_name).char_limit(96));
            ui.end_row();
        });
        button(app, ui, "Connect", "komaphone.room.connect", json!({"endpoint":app.ui.rooms.endpoint,"display_name":app.ui.rooms.display_name}));
    }
}
fn options(ui: &mut egui::Ui, state: &mut RoomUi) {
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(&mut state.corpse, false, "Animation");
        ui.selectable_value(&mut state.corpse, true, "Exquisite Corpse");
        if !state.corpse {
            ui.add(egui::DragValue::new(&mut state.rotations).range(1..=4).suffix(" rotations"));
        }
    });
    if state.corpse {
        ui.label("Three parts per creature. With two players, the head author also draws the legs.");
    }
    ui.horizontal(|ui| {
        ui.checkbox(&mut state.timed, "Round timer");
        if state.timed {
            ui.add(egui::DragValue::new(&mut state.minutes).range(1..=60).suffix(" min"));
        } else {
            ui.label("Untimed · submit when ready");
        }
    });
}
fn option_params(state: &RoomUi) -> Value {
    json!({"mode":if state.corpse {json!({"kind":"exquisite_corpse"})} else {json!({"kind":"animation","rotations":state.rotations})},"timer_ms":state.timed.then_some(u64::from(state.minutes)*60_000)})
}
fn create_join(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    ui.separator();
    ui.strong("Create a room");
    options(ui, &mut app.ui.rooms);
    button(app, ui, "Create room", "komaphone.room.create", option_params(&app.ui.rooms));
    ui.separator();
    ui.strong("Join a friend");
    egui::Grid::new("room-join").num_columns(2).show(ui, |ui| {
        ui.label("Room ID");
        ui.add(egui::TextEdit::singleline(&mut app.ui.rooms.room_id).char_limit(32));
        ui.end_row();
        ui.label("Private invitation");
        ui.add(egui::TextEdit::singleline(&mut app.ui.rooms.invitation).password(true).char_limit(64));
        ui.end_row();
    });
    let response = ui.add_enabled(app.session.is_enabled("komaphone.room.join"), egui::Button::new("Join room"));
    crate::komaphone_workspace::remember(ui, "komaphone.room.join", &response);
    if response.clicked() {
        let invitation = std::mem::take(&mut app.ui.rooms.invitation);
        run(app, "komaphone.room.join", json!({"room":app.ui.rooms.room_id,"invitation":invitation}));
    }
}
fn room_view(app: &mut PhotocraftApp, ui: &mut egui::Ui, room: &RoomView) {
    ui.separator();
    ui.horizontal_wrapped(|ui| {
        ui.strong(match room.phase {
            Phase::Lobby => "Lobby",
            Phase::Drawing => "Drawing",
            Phase::Transfer => "Finishing submissions",
            Phase::Reveal => "Game finished",
            Phase::Archived => "Archived game",
        });
        if ui.button("Copy room ID").clicked() {
            ui.ctx().copy_text(room.id.as_str().into());
        }
    });
    let actor = app.session.rooms.status.profile.as_ref().map(|p| p.id);
    let moderator = actor == Some(room.moderator);
    for member in &room.members {
        ui.horizontal(|ui| {
            ui.label(&member.participant.display_name);
            if member.participant.id == room.moderator {
                ui.weak("Host");
            }
            ui.label(if member.connected { "Connected" } else { "Disconnected" });
            if room.phase == Phase::Lobby {
                ui.label(if member.ready { "Ready" } else { "Not ready" });
            }
        });
    }
    for member in &room.waiting {
        ui.label(format!("{} · waiting for next game", member.participant.display_name));
    }
    if let Some(reason) = room.paused {
        let reason = match reason {
            photocraft_engine::party::room::PauseReason::Moderator => "Paused by the host",
            photocraft_engine::party::room::PauseReason::Disconnected => "Waiting for a disconnected player",
            photocraft_engine::party::room::PauseReason::Restart => "Service restarted — waiting for the host to resume",
        };
        ui.label(reason);
    }
    phase_controls(app, ui, room, moderator);
    if !moderator && room.members.iter().any(|m| m.participant.id == room.moderator && !m.connected) {
        button(app, ui, "Claim host role", "komaphone.room.claim", json!({}));
    }
    if moderator {
        invitation_controls(app, ui);
    }
    button(app, ui, "Leave room", "komaphone.room.leave", json!({}));
}
fn phase_controls(app: &mut PhotocraftApp, ui: &mut egui::Ui, room: &RoomView, moderator: bool) {
    if room.phase == Phase::Lobby {
        lobby_controls(app, ui, room, moderator);
    } else if matches!(room.phase, Phase::Drawing | Phase::Transfer) {
        drawing_controls(app, ui, room, moderator);
    } else {
        if !room.reveal.is_empty() && ui.button("View finished game").clicked() {
            app.ui.rooms.view_reveal = true;
            app.ui.rooms.open = false;
            app.ui.komaphone.enabled = true;
        }
        if moderator {
            button(app, ui, "Archive game", "komaphone.room.archive", json!({}));
            button(app, ui, "Next game", "komaphone.room.next", json!({}));
        }
    }
}
fn lobby_controls(app: &mut PhotocraftApp, ui: &mut egui::Ui, room: &RoomView, moderator: bool) {
    let mode = match room.mode {
        photocraft_engine::party::Mode::Animation { rotations } => format!("Animation · {rotations} {}", if rotations == 1 { "rotation" } else { "rotations" }),
        photocraft_engine::party::Mode::ExquisiteCorpse => "Exquisite Corpse".into(),
    };
    ui.label(format!("{mode} · {}", room.timer_ms.map_or("Untimed".into(), |ms| format!("{} second rounds", ms / 1000))));
    if room.mode == photocraft_engine::party::Mode::ExquisiteCorpse {
        ui.label("With two players, the head author also draws the legs.");
    }
    if moderator {
        ui.collapsing("Change game options", |ui| {
            options(ui, &mut app.ui.rooms);
            button(app, ui, "Apply options", "komaphone.room.options", option_params(&app.ui.rooms));
        });
    }
    let ready = room.members.iter().find(|m| Some(m.participant.id) == app.session.rooms.status.profile.as_ref().map(|p| p.id)).is_some_and(|m| m.ready);
    ui.horizontal_wrapped(|ui| {
        button(app, ui, if ready { "Not ready" } else { "Ready" }, "komaphone.room.ready", json!({"ready":!ready}));
        if moderator {
            button(app, ui, "Start game", "komaphone.room.start", json!({}));
        }
    });
}
fn drawing_controls(app: &mut PhotocraftApp, ui: &mut egui::Ui, room: &RoomView, moderator: bool) {
    ui.label(format!("Round {}", u16::from(room.round) + 1));
    if let Some(assignment) = &room.assignment {
        ui.label(if assignment.submitted { "Submission accepted" } else { "Waiting for your submission" });
        if !assignment.submitted {
            ui.horizontal_wrapped(|ui| {
                button(app, ui, "Open assigned drawing", "komaphone.room.openDraft", json!({}));
                button(app, ui, "Submit saved drawing", "komaphone.room.submit", json!({"note":crate::komaphone_game_view::submission_note(app,room)}));
            });
        }
    }
    if moderator {
        ui.horizontal_wrapped(|ui| {
            if room.paused.is_some() {
                button(app, ui, "Resume", "komaphone.room.resume", json!({}));
            } else {
                button(app, ui, "Pause", "komaphone.room.pause", json!({}));
            }
            button(app, ui, "End and keep incomplete game", "komaphone.room.end", json!({"reason":"Host ended the game"}));
        });
        for member in room.members.iter().filter(|m| !m.connected) {
            button(
                app,
                ui,
                &format!("Skip {} with placeholder", member.participant.display_name),
                "komaphone.room.skip",
                json!({"participant":member.participant.id,"reason":"Host skipped disconnected participant"}),
            );
        }
    }
    crate::komaphone_game_view::note_editor(app, ui, room);
}
fn invitation_controls(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    ui.separator();
    ui.horizontal_wrapped(|ui| {
        button(app, ui, "Create private invitation", "komaphone.room.invite", json!({"expires_in_ms":3_600_000,"uses":2}));
        if let Some(invitation) = app.session.rooms.invitation()
            && ui.button("Copy invitation").clicked()
        {
            ui.ctx().copy_text(invitation.secret.clone());
        }
        if let Some(id) = app.session.rooms.status.invitation_id {
            button(app, ui, "Revoke invitation", "komaphone.room.revoke", json!({"invitation_id":id}));
        }
    });
    ui.weak("Invitations expire after one hour. Share the service address, room ID and invitation privately.");
}
