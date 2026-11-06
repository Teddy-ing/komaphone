//! Read-only Animation references and whole-animation reveal playback. Drawing
//! timelapse is deliberately a separate K4 view, never this completed-frame clock.
use crate::{canvas::ViewXform, theme::Tokens};
use egui::{TextureId, Vec2};
use photocraft_engine::party::room::{Reference, RevealedEntry, RoomView};
use photocraft_engine::party::{AssetHash, GameId, ParticipantId};
use photocraft_engine::rooms::animation::{previous_reference, reveal_chain};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnimationUi {
    pub reference_visible: bool,
    pub onion_skin: bool,
    pub onion_opacity: f32,
    pub chain: u8,
    pub frame: usize,
    pub playing: bool,
    pub fps: u8,
    pub looping: bool,
    pub show_notes: bool,
    pub game: Option<GameId>,
    #[serde(skip)]
    clock: Option<f64>,
    #[serde(skip)]
    fraction: f64,
}

impl Default for AnimationUi {
    fn default() -> Self {
        Self {
            reference_visible: true,
            onion_skin: false,
            onion_opacity: 0.3,
            chain: 0,
            frame: 0,
            playing: false,
            fps: 6,
            looping: true,
            show_notes: false,
            game: None,
            clock: None,
            fraction: 0.0,
        }
    }
}

impl AnimationUi {
    fn sanitize(&mut self) {
        self.onion_opacity = if self.onion_opacity.is_finite() { self.onion_opacity.clamp(0.0, 1.0) } else { 0.3 };
        self.fps = self.fps.clamp(1, 24);
    }

    /// A bounded view clock. Restart retains the chosen frame and settings; its
    /// first paint establishes a fresh clock instead of skipping unseen frames.
    pub fn advance(&mut self, now: f64, frame_count: usize) {
        self.sanitize();
        if frame_count == 0 || frame_count > 12 || !now.is_finite() {
            self.playing = false;
            self.clock = None;
            self.fraction = 0.0;
            return;
        }
        self.frame = self.frame.min(frame_count - 1);
        let previous = self.clock.replace(now);
        if !self.playing {
            self.fraction = 0.0;
            return;
        }
        let Some(previous) = previous else { return };
        if now < previous {
            self.fraction = 0.0;
            return;
        }
        // Stop time accumulating while the app is suspended or the view hidden.
        let elapsed = (now - previous).min(1.0);
        let frames = self.fraction + elapsed * f64::from(self.fps);
        let step = frames.floor() as usize;
        self.fraction = frames.fract();
        let next = self.frame.saturating_add(step);
        if self.looping {
            self.frame = next % frame_count;
        } else if next >= frame_count {
            self.frame = frame_count - 1;
            self.playing = false;
            self.fraction = 0.0;
        } else {
            self.frame = next;
        }
    }

    pub fn pause(&mut self) {
        self.playing = false;
        self.clock = None;
        self.fraction = 0.0;
    }
}

fn control_rect(ui: &egui::Ui, id: &str, response: &egui::Response) {
    crate::komaphone_workspace::remember(ui, id, response);
}

fn picture(ui: &mut egui::Ui, texture: (TextureId, Vec2), max_height: f32) {
    let (id, size) = texture;
    if size.x <= 0.0 || size.y <= 0.0 || !size.is_finite() {
        ui.label("The frame preview has an invalid size. Refresh the room.");
        return;
    }
    let scale = (ui.available_width().max(1.0) / size.x).min(max_height / size.y).min(1.0);
    ui.add(egui::Image::new((id, size * scale)));
}

/// Renders the previous frame beside the editable canvas when hosted in a side
/// panel. Returns an explicit copy request for the engine command seam.
pub fn reference(
    ui: &mut egui::Ui,
    state: &mut AnimationUi,
    room: &RoomView,
    actor: ParticipantId,
    texture: Option<(TextureId, Vec2)>,
    can_copy: bool,
) -> bool {
    state.sanitize();
    let reference = match previous_reference(room, actor) {
        Ok(reference) => reference,
        Err(error) => {
            ui.label(error.to_string());
            return false;
        }
    };
    ui.strong("Previous frame");
    let Some((_, note)) = reference else {
        if matches!(room.assignment.as_ref().and_then(|a| a.reference.as_ref()), Some(Reference::Missing)) {
            ui.label("The previous frame is missing. Continue with your own idea.");
        } else {
            ui.label("First frame · start with a blank canvas.");
        }
        return false;
    };
    let show = ui.checkbox(&mut state.reference_visible, "Show previous frame");
    control_rect(ui, "komaphone.animation.reference", &show);
    if state.reference_visible {
        if let Some(texture) = texture {
            picture(ui, texture, 230.0);
        } else {
            ui.label("Loading the authorized previous frame…");
        }
    }
    let onion = ui.checkbox(&mut state.onion_skin, "Onion skin on canvas");
    control_rect(ui, "komaphone.animation.onion", &onion);
    if state.onion_skin {
        let response = ui.add(egui::Slider::new(&mut state.onion_opacity, 0.0..=1.0).text("Reference opacity"));
        control_rect(ui, "komaphone.animation.onionOpacity", &response);
    }
    let copy = ui.add_enabled(can_copy && texture.is_some(), egui::Button::new("Copy previous frame"));
    control_rect(ui, "komaphone.animation.copyPrevious", &copy);
    copy.clone().on_hover_text("Add an independent editable copy as a new layer. The accepted previous frame stays unchanged.");
    if !note.is_empty() {
        ui.separator();
        ui.strong("Note from the previous artist");
        ui.label(note);
    }
    ui.weak("The reference and note stay outside your drawing.");
    copy.clicked()
}

fn onion_corners(xf: &ViewXform) -> [egui::Pos2; 4] {
    [xf.to_screen(0.0, 0.0), xf.to_screen(1536.0, 0.0), xf.to_screen(1536.0, 1024.0), xf.to_screen(0.0, 1024.0)]
}

/// Paint-only overlay follows the exact document transform, including flipped or
/// rotated views and pixel aspect. It never creates editable document data.
pub fn onion_overlay(painter: &egui::Painter, state: &AnimationUi, xf: &ViewXform, texture: TextureId) {
    let corners = onion_corners(xf);
    if !state.onion_skin || !state.onion_opacity.is_finite() || !corners.iter().all(|corner| corner.is_finite()) {
        return;
    }
    let opacity = state.onion_opacity.clamp(0.0, 1.0);
    let mut mesh = egui::Mesh::with_texture(texture);
    for (pos, uv) in corners.into_iter().zip([egui::pos2(0.0, 0.0), egui::pos2(1.0, 0.0), egui::pos2(1.0, 1.0), egui::pos2(0.0, 1.0)]) {
        mesh.vertices.push(egui::epaint::Vertex { pos, uv, color: egui::Color32::WHITE.gamma_multiply(opacity) });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

/// Reveals the finished frames of each chain with author order and independent
/// playback controls. Returns whether the viewer explicitly enabled metadata notes;
/// the caller may issue the separately authorized notes request when needed.
pub fn reveal(ui: &mut egui::Ui, state: &mut AnimationUi, room: &RoomView, mut texture: impl FnMut(AssetHash) -> Option<(TextureId, Vec2)>) -> bool {
    state.sanitize();
    let game = room.reveal.first().map(|entry| match entry {
        RevealedEntry::Artwork { assignment, .. } | RevealedEntry::Missing { assignment, .. } => assignment.id.game,
    });
    if state.game != game {
        state.game = game;
        state.chain = 0;
        state.frame = 0;
        state.show_notes = false;
        state.pause();
    }
    if usize::from(state.chain) >= room.members.len() {
        state.chain = 0;
    }
    ui.strong("Finished animations");
    ui.horizontal_wrapped(|ui| {
        for (chain, member) in room.members.iter().enumerate().take(3) {
            let response = ui.selectable_label(usize::from(state.chain) == chain, format!("{}’s chain", member.participant.display_name));
            control_rect(ui, &format!("komaphone.animation.chain.{chain}"), &response);
            if response.clicked() {
                state.chain = chain as u8;
                state.frame = 0;
                state.pause();
            }
        }
    });
    let entries = match reveal_chain(room, state.chain) {
        Ok(entries) => entries,
        Err(error) => {
            state.pause();
            ui.label(error.to_string());
            return false;
        }
    };
    let frames: Vec<_> = entries
        .iter()
        .map(|entry| match entry {
            RevealedEntry::Artwork { image, .. } => texture(image.hash),
            RevealedEntry::Missing { .. } => None,
        })
        .collect();
    let ready = entries.iter().zip(&frames).all(|(entry, frame)| matches!(entry, RevealedEntry::Missing { .. }) || frame.is_some());
    if !ready {
        state.pause();
        ui.label("Loading this animation’s saved frames before playback…");
    }
    let now = ui.input(|input| input.time);
    state.advance(now, entries.len());
    ui.horizontal_wrapped(|ui| {
        let play = ui.add_enabled(ready, egui::Button::new(if state.playing { "Pause animation" } else { "Play animation" }));
        control_rect(ui, "komaphone.animation.play", &play);
        if play.clicked() {
            if state.playing {
                state.pause();
            } else {
                if !state.looping && state.frame == entries.len().saturating_sub(1) {
                    state.frame = 0;
                }
                state.playing = true;
                state.clock = Some(now);
            }
        }
        let looping = ui.checkbox(&mut state.looping, "Loop");
        control_rect(ui, "komaphone.animation.loop", &looping);
        let speed = ui.add(egui::Slider::new(&mut state.fps, 1..=24).text("frames / second"));
        control_rect(ui, "komaphone.animation.speed", &speed);
    });
    let scrub = ui.add(
        egui::Slider::new(&mut state.frame, 0..=entries.len().saturating_sub(1))
            .text("Frame")
            .custom_formatter(|value, _| format!("{}", (value as u32).saturating_add(1))),
    );
    control_rect(ui, "komaphone.animation.frame", &scrub);
    if scrub.changed() {
        state.pause();
    }
    if state.playing {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(1.0 / f64::from(state.fps)));
    }
    if let Some(entry) = entries.get(state.frame) {
        let assignment = match entry {
            RevealedEntry::Artwork { assignment, .. } | RevealedEntry::Missing { assignment, .. } => assignment,
        };
        let artist = room
            .members
            .iter()
            .find(|member| member.participant.id == assignment.player)
            .map_or("Unknown artist", |member| member.participant.display_name.as_str());
        ui.label(format!("Frame {} of {} · {}", state.frame + 1, entries.len(), artist));
        match entry {
            RevealedEntry::Artwork { .. } => {
                if let Some(texture) = frames.get(state.frame).copied().flatten() {
                    picture(ui, texture, 380.0);
                } else {
                    ui.label("Loading the revealed frame…");
                }
            }
            RevealedEntry::Missing { reason, .. } => {
                let tokens = Tokens::get(ui.ctx());
                egui::Frame::new().fill(tokens.card).inner_margin(16.0).show(ui, |ui| {
                    ui.strong("Missing frame");
                    ui.label(reason);
                });
            }
        }
    }
    ui.collapsing("Frame order and credits", |ui| {
        for entry in &entries {
            let (assignment, missing) = match entry {
                RevealedEntry::Artwork { assignment, .. } => (assignment, false),
                RevealedEntry::Missing { assignment, .. } => (assignment, true),
            };
            let artist = room
                .members
                .iter()
                .find(|member| member.participant.id == assignment.player)
                .map_or("Unknown artist", |member| member.participant.display_name.as_str());
            ui.label(format!("{} · {}{}", u16::from(assignment.id.round) + 1, artist, if missing { " · missing" } else { "" }));
        }
    });
    let notes = ui.checkbox(&mut state.show_notes, "Open handoff notes");
    control_rect(ui, "komaphone.animation.notes", &notes);
    state.show_notes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_uses_completed_frame_clock_and_stops_or_loops() {
        let mut state = AnimationUi { playing: true, looping: false, ..Default::default() };
        state.advance(10.0, 4);
        state.advance(10.5, 4);
        assert_eq!(state.frame, 3);
        state.advance(10.7, 4);
        assert_eq!(state.frame, 3);
        assert!(!state.playing);
        state.looping = true;
        state.playing = true;
        state.advance(11.2, 4);
        assert_eq!(state.frame, 2);
        state.pause();
        state.advance(100.0, 4);
        assert_eq!(state.frame, 2);
    }

    #[test]
    fn restart_retains_frame_and_speed_without_accumulating_hidden_time() {
        let state = AnimationUi { frame: 2, fps: 12, playing: true, ..Default::default() };
        let json = serde_json::to_vec(&state).unwrap();
        let mut restored: AnimationUi = serde_json::from_slice(&json).unwrap();
        restored.advance(999_999.0, 6);
        assert_eq!((restored.frame, restored.fps), (2, 12));
        restored.advance(999_999.25, 6);
        assert_eq!(restored.frame, 5);
    }

    #[test]
    fn malformed_state_and_empty_or_excessive_chains_are_bounded() {
        let mut state = AnimationUi { frame: usize::MAX, fps: 0, playing: true, onion_opacity: f32::NAN, ..Default::default() };
        state.advance(1.0, 3);
        assert_eq!((state.frame, state.fps, state.onion_opacity), (2, 1, 0.3));
        state.advance(f64::NAN, 3);
        assert!(!state.playing);
        state.advance(2.0, 0);
        state.advance(3.0, usize::MAX);
        assert!(!state.playing);
        state.playing = true;
        state.advance(5.0, 3);
        state.advance(4.0, 3);
        assert_eq!(state.frame, 2);
    }
}
#[test]
fn onion_skin_follows_document_corners_under_rotation_flip_and_aspect() {
    for (zoom, rotation, flip, aspect) in [(0.25, 0.0, false, 1.0), (2.0, 90.0, false, 1.0), (0.5, 27.0, true, 1.4)] {
        let xf = ViewXform {
            rect: egui::Rect::from_min_size(egui::pos2(20.0, 30.0), egui::vec2(800.0, 600.0)),
            zoom,
            center: [768.0, 512.0],
            flip,
            rotation,
            aspect,
        };
        for (corner, expected) in onion_corners(&xf).into_iter().zip([[0.0, 0.0], [1536.0, 0.0], [1536.0, 1024.0], [0.0, 1024.0]]) {
            let actual = xf.to_doc(corner);
            assert!((actual[0] - expected[0]).abs() < 0.002);
            assert!((actual[1] - expected[1]).abs() < 0.002);
        }
    }
}
