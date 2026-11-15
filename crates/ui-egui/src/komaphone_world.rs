//! Shared canvases: controls and pointer presentation only. All durable changes
//! use the same engine commands as automation.
use crate::{PhotocraftApp, theme::Tokens};
use egui::{Rect, Sense};
use photocraft_engine::scene::{self, Camera, CanvasId, StrokeId, Viewport, shared::TourStop};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod input;
mod panels;
#[cfg(not(target_arch = "wasm32"))]
mod render;
mod review;
mod thumbnails;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use render::Presentation;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn prune(app: &mut PhotocraftApp, ctx: &egui::Context) {
    render::prune(app);
    thumbnails::prune(app, ctx);
    if app.ui.world.account != app.session.worlds.account {
        app.ui.world.cancel_gesture_for_switch();
        app.ui.world.account = app.session.worlds.account.clone();
        app.ui.world.selected = None;
        app.ui.world.name = WorldUi::default().name;
        app.ui.world.rename.clear();
        app.ui.world.bookmark = WorldUi::default().bookmark;
        app.ui.world.accept_code.clear();
        app.ui.world.invite_to = None;
        app.ui.world.tour_name = WorldUi::default().tour_name;
        app.ui.world.tour_stops.clear();
        app.ui.world.tour_edit = None;
        app.ui.world.export_path.clear();
        app.ui.world.export_tour = None;
        app.ui.world.source = None;
        if !app.ui.world.unfinished() {
            app.ui.world.message = None;
        }
        app.ui.world.highlights = Default::default();
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorldUi {
    pub open: bool,
    pub tools: crate::komaphone_canvas::CanvasUi,
    #[serde(skip)]
    pub name: String,
    #[serde(skip)]
    pub rename: String,
    #[serde(skip)]
    pub bookmark: String,
    #[serde(skip)]
    pub invite_to: Option<scene::OwnerId>,
    #[serde(skip)]
    pub accept_code: String,
    #[serde(skip)]
    pub tour_name: String,
    #[serde(skip)]
    pub tour_stops: Vec<TourStop>,
    #[serde(skip)]
    pub tour_edit: Option<(scene::shared::TourId, u64)>,
    #[serde(skip)]
    pub source: Option<scene::world_protocol::GameMediaSource>,
    #[serde(skip)]
    pub export_path: String,
    pub export_width: u32,
    pub export_height: u32,
    pub export_fps: u32,
    #[serde(skip)]
    pub export_tour: Option<scene::shared::TourId>,
    #[serde(skip)]
    highlights: review::Highlights,
    #[serde(skip)]
    pub message: Option<String>,
    #[serde(skip)]
    pub(crate) close_prompt: bool,
    #[serde(skip)]
    pub(crate) close_approval: Option<crate::discard_ui::world_close::Approval>,
    #[serde(skip)]
    selected: Option<CanvasId>,
    #[serde(skip)]
    draft: Option<Draft>,
    #[serde(skip)]
    pan: Option<Pan>,
    #[serde(skip)]
    next_view_checkpoint: f64,
    #[serde(skip)]
    last_time: Option<f64>,
    #[serde(skip)]
    account: Option<photocraft_engine::worlds::Account>,
}
impl Default for WorldUi {
    fn default() -> Self {
        Self {
            open: false,
            tools: Default::default(),
            name: "Our canvas".into(),
            rename: String::new(),
            bookmark: "A detail".into(),
            invite_to: None,
            accept_code: String::new(),
            tour_name: "A tour".into(),
            tour_stops: vec![],
            tour_edit: None,
            source: None,
            export_path: String::new(),
            export_width: 1024,
            export_height: 768,
            export_fps: 12,
            export_tour: None,
            highlights: Default::default(),
            message: None,
            close_prompt: false,
            close_approval: None,
            selected: None,
            draft: None,
            pan: None,
            next_view_checkpoint: 0.0,
            last_time: None,
            account: None,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
struct Draft {
    account: photocraft_engine::worlds::Account,
    id: StrokeId,
    stroke: crate::komaphone_canvas::DraftStroke,
    drawing: bool,
    submitted: bool,
}
#[derive(Clone, Debug, PartialEq)]
struct Pan {
    camera: Camera,
    start: [f64; 2],
    button: egui::PointerButton,
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DrawingSnapshot {
    draft: Option<Draft>,
}
impl WorldUi {
    pub(crate) fn unfinished(&self) -> bool {
        self.draft.is_some()
    }
    pub(crate) fn cancel_gesture_for_switch(&mut self) {
        self.pan = None;
        if let Some(draft) = self.draft.as_mut() {
            draft.drawing = false;
            self.message = Some("An unfinished stroke is retained. Return to its original account and canvas to save it.".into());
        }
    }
    pub(crate) fn drawing_snapshot(&self) -> DrawingSnapshot {
        DrawingSnapshot { draft: self.draft.clone() }
    }
    pub(crate) fn prepare_close(&mut self) {
        self.cancel_gesture_for_switch();
    }
}
pub(crate) fn can_retry_unfinished(app: &PhotocraftApp) -> bool {
    app.ui.world.draft.as_ref().is_some_and(|draft| {
        !draft.submitted
            && app.session.worlds.account.as_ref() == Some(&draft.account)
            && app.session.worlds.active.as_ref().is_some_and(|a| a.info.canvas == draft.stroke.viewport.camera.canvas)
    })
}
pub(crate) fn retry_unfinished(app: &mut PhotocraftApp) {
    input::finish(app);
}
fn run(app: &mut PhotocraftApp, id: &str, params: Value) -> bool {
    match app.run(id, params) {
        Ok(_) => {
            app.ui.world.message = None;
            true
        }
        Err(error) => {
            app.ui.world.message = Some(error);
            false
        }
    }
}
fn remember(ui: &egui::Ui, id: &str, response: &egui::Response) {
    crate::komaphone_workspace::remember(ui, id, response);
}
fn button(app: &mut PhotocraftApp, ui: &mut egui::Ui, label: &str, id: &str, params: Value, enabled: bool) {
    let response = ui.add_enabled(enabled && app.session.is_enabled(id), egui::Button::new(label));
    remember(ui, id, &response);
    if response.clicked() {
        run(app, id, params);
    }
}
pub fn open(app: &mut PhotocraftApp) {
    if app.ui.canvas.has_uncommitted_drawing() {
        app.ui.canvas.message = Some("Save the unfinished local stroke before opening shared canvases.".into());
        return;
    }
    app.ui.world.open = true;
    app.ui.canvas.open = false;
    app.ui.gallery.open = false;
    app.ui.rooms.open = false;
    if app.session.worlds.connected() {
        run(app, "komaphone.world.list", json!({}));
        run(app, "komaphone.world.people", json!({}));
    }
}
pub(crate) fn source_from_gallery(app: &mut PhotocraftApp, still: bool) {
    use photocraft_engine::party::{gallery::GalleryContent, room::RevealedEntry};
    let Some(manifest) = app.session.gallery.active.clone() else { return };
    let GalleryContent::Game { room } = &manifest.content else { return };
    let Some(game) = room.reveal.first().map(|entry| match entry {
        RevealedEntry::Artwork { assignment, .. } | RevealedEntry::Missing { assignment, .. } => assignment.id.game,
    }) else {
        return;
    };
    let (Ok(room), Ok(game)) = (scene::shared::SourceId::new(room.id.as_str()), scene::shared::SourceId::new(game.as_str())) else { return };
    open(app);
    if app.ui.world.open {
        app.ui.world.source = Some(scene::world_protocol::GameMediaSource {
            room,
            game,
            chain: app.ui.gallery.chain,
            frame: still.then_some(u16::from(app.ui.gallery.frame)),
            fps: app.ui.gallery.fps,
            looping: app.ui.gallery.looping,
        });
    }
}
pub fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    if app.ui.world.close_prompt {
        ui.disable();
    }
    sync(app, ui);
    let tokens = Tokens::get(ui.ctx());
    let (left, right) = crate::titlebar::reserved(app, ui.ctx());
    egui::Panel::top("world-heading")
        .exact_size(crate::titlebar::bar_height(app, ui.ctx(), 42.0))
        .frame(egui::Frame::NONE.fill(tokens.chrome).inner_margin(egui::Margin::symmetric(10, 0)))
        .show(ui, |ui| {
            let full = ui.max_rect();
            ui.horizontal_centered(|ui| {
                ui.add_space(left);
                ui.strong("Komaphone · Shared canvases");
                let enabled = !app.ui.world.unfinished();
                let response = ui.add_enabled(enabled, egui::Button::new("Local canvases"));
                remember(ui, "world.local", &response);
                if response.clicked() {
                    app.ui.world.open = false;
                    crate::komaphone_canvas::open(app);
                }
                let response = ui.add_enabled(enabled, egui::Button::new("Party & drawings"));
                remember(ui, "world.return", &response);
                if response.clicked() {
                    app.ui.world.open = false;
                }
                let (rect, response) = ui.allocate_exact_size(egui::vec2((ui.available_width() - right - 6.0).max(0.0), 32.0), Sense::click_and_drag());
                ui.ctx().data_mut(|data| data.insert_temp(crate::titlebar::modal_drag_area_id(), rect));
                if response.drag_started() && app.custom_titlebar {
                    crate::titlebar::start_os_move(ui.ctx());
                }
            });
            if app.custom_titlebar {
                crate::titlebar::caption_buttons(app, ui, full);
            }
        });
    panels::sidebar(app, ui);
    toolbar(app, ui);
    egui::Panel::bottom("world-status").exact_size(64.0).frame(egui::Frame::NONE.fill(tokens.chrome).inner_margin(5)).show(ui, |ui| {
        ui.label(if app.ui.world.draft.is_some() { "Current stroke has not been saved".into() } else { app.session.worlds.status() });
        if let Some(message) = app.ui.world.message.as_ref().or(app.session.worlds.error.as_ref()) {
            ui.colored_label(tokens.danger, message);
        } else if let Some(notice) = app.session.worlds.active.as_ref().and_then(|a| a.notice.as_ref()).or(app.session.worlds.notice.as_ref()) {
            ui.label(notice);
        } else {
            ui.label(
                egui::RichText::new("Wheel: zoom · Middle drag or Space: pan · Eraser and undo affect your device’s drawing layer").color(tokens.text_dim),
            );
        }
    });
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(tokens.canvas)).show(ui, |ui| {
        if !app.session.worlds.connected() {
            ui.centered_and_justified(|ui| {
                ui.label("Connect through Private room to share canvases. Your local canvases remain available offline.");
            });
        } else if app.session.worlds.active.is_some() {
            drawing_area(app, ui);
        } else {
            ui.centered_and_justified(|ui| {
                ui.label("Create a named canvas, open one from your list, or accept an invitation.");
            });
        }
    });
}
fn sync(app: &mut PhotocraftApp, ui: &egui::Ui) {
    if let Some(draft) = &app.ui.world.draft
        && app.session.worlds.account.as_ref() == Some(&draft.account)
    {
        let canvas = draft.stroke.viewport.camera.canvas;
        if app.session.worlds.durable_strokes.contains(&(canvas, draft.id))
            || app
                .session
                .worlds
                .active
                .as_ref()
                .is_some_and(|a| a.info.canvas == canvas && a.display.canvas == canvas && a.display.strokes.contains_key(&draft.id))
        {
            app.ui.world.draft = None;
        } else if !app.session.worlds.busy()
            && app.session.worlds.error.is_some()
            && let Some(draft) = app.ui.world.draft.as_mut()
        {
            draft.submitted = false;
        }
    }
    let selected = app.session.worlds.active.as_ref().map(|a| a.info.canvas);
    if app.ui.world.selected != selected {
        app.ui.world.selected = selected;
        app.ui.world.rename = app.session.worlds.active.as_ref().map_or_else(String::new, |a| a.info.name.clone());
        app.ui.world.pan = None;
        if let Some(draft) = app.ui.world.draft.as_mut() {
            draft.drawing = false;
        }
        app.ui.world.tour_stops.clear();
        app.ui.world.tour_edit = None;
        app.ui.world.export_tour = None;
        app.ui.world.highlights = Default::default();
        if selected.is_some() {
            run(app, "komaphone.world.members", json!({}));
            run(app, "komaphone.world.people", json!({}));
        }
    }
    let now = ui.input(|input| input.time);
    let delta = app.ui.world.last_time.replace(now).map_or(0.0, |last| (now - last).clamp(0.0, 0.25));
    let ms = (delta * 1000.0) as u64;
    let mut arrived = false;
    if app.session.worlds.tour_playback.is_some() {
        app.session.worlds.flight = None;
    }
    if let Some(flight) = app.session.worlds.flight.as_mut()
        && !flight.paused
    {
        flight.elapsed = (flight.elapsed + delta).min(flight.duration);
        let progress = if flight.duration > 0.0 { flight.elapsed / flight.duration } else { 1.0 };
        match scene::coordinates::flight(&flight.from, &flight.to, progress) {
            Ok(camera) => {
                if let Some(active) = app.session.worlds.active.as_mut() {
                    active.viewer.camera = camera;
                }
                arrived = progress >= 1.0;
            }
            Err(error) => {
                app.ui.world.message = Some(error.to_string());
                arrived = true;
            }
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
    }
    if arrived {
        app.session.worlds.flight = None;
        if let Err(error) = app.session.world_save_view() {
            app.ui.world.message = Some(error.to_string());
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let review_ready = !app.world_render.preparing;
    #[cfg(target_arch = "wasm32")]
    let review_ready = true;
    if let Some(active) = app.session.worlds.active.as_mut() {
        if let Some(review) = active.viewer.review.as_mut()
            && review.interval.is_some()
            && review_ready
            && let Err(error) = review.advance(ms, active.canonical.revision)
        {
            app.ui.world.message = Some(error.to_string());
        }
        if let Some(playback) = app.session.worlds.tour_playback.as_mut() {
            if let Some(tour) = active.display.tours.get(&playback.tour) {
                match playback.advance(tour, ms) {
                    Ok(frame) => active.viewer.camera = frame.camera,
                    Err(error) => {
                        playback.playing = false;
                        app.ui.world.message = Some(error.to_string());
                    }
                }
                if playback.playing {
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
                }
            } else {
                app.session.worlds.tour_playback = None;
                app.ui.world.message = Some("This tour was removed. Your current view is unchanged.".into());
            }
        }
    }
    if now >= app.ui.world.next_view_checkpoint
        && app.session.worlds.view_needs_save()
        && app.session.worlds.active.as_ref().is_some_and(|active| active.viewer.review.as_ref().is_some_and(|review| review.interval.is_some()))
    {
        app.ui.world.next_view_checkpoint = now + 0.5;
        if let Err(error) = app.session.world_save_view() {
            app.ui.world.message = Some(error.to_string());
        }
    }
}
fn toolbar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let tokens = Tokens::get(ui.ctx());
    egui::Panel::top("world-tools").exact_size(42.0).frame(egui::Frame::NONE.fill(tokens.chrome).inner_margin(6)).show(ui, |ui| {
        egui::ScrollArea::horizontal().id_salt("world-tools-scroll").show(ui, |ui| {
            ui.horizontal(|ui| {
                let idle = !app.ui.world.unfinished();
                let editing = idle && !reviewing(app);
                for (label, erase, pan) in [("Brush", false, false), ("Eraser", true, false), ("Pan", false, true)] {
                    let response = ui.add_enabled(
                        idle && (pan || editing),
                        egui::Button::new(label).selected(app.ui.world.tools.erase == erase && app.ui.world.tools.pan == pan),
                    );
                    remember(ui, &format!("world.tool.{}", label.to_ascii_lowercase()), &response);
                    if response.clicked() {
                        app.ui.world.tools.erase = erase;
                        app.ui.world.tools.pan = pan;
                    }
                }
                let response = ui.add(egui::DragValue::new(&mut app.ui.world.tools.brush_width).range(1.0..=128.0).suffix(" px"));
                remember(ui, "world.width", &response);
                let [red, green, blue, _] = app.ui.world.tools.color;
                let mut color = [red, green, blue].map(|value| (value * 255.0).round() as u8);
                if ui.color_edit_button_srgb(&mut color).changed() {
                    app.ui.world.tools.color = [f32::from(color[0]) / 255.0, f32::from(color[1]) / 255.0, f32::from(color[2]) / 255.0, 1.0];
                }
                button(app, ui, "Undo mine", "komaphone.world.undo", json!({}), editing);
                button(app, ui, "Redo mine", "komaphone.world.redo", json!({}), editing);
                button(app, ui, "Home", "komaphone.world.home", json!({}), idle);
                if app.ui.world.draft.as_ref().is_some_and(|d| !d.drawing) && ui.button("Retry unfinished stroke").clicked() {
                    input::finish(app);
                }
            });
        });
    });
}
fn drawing_area(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let (rect, response) = ui.allocate_exact_size(ui.available_size().max(egui::vec2(1.0, 1.0)), Sense::click_and_drag());
    remember(ui, "world.drawing", &response);
    let Some(camera) = app.session.worlds.active.as_ref().map(|a| a.viewer.camera.clone()) else { return };
    let viewport = Viewport {
        camera,
        width: (rect.width().floor() as u32).clamp(1, scene::MAX_VIEW_SIDE),
        height: (rect.height().floor() as u32).clamp(1, scene::MAX_VIEW_SIDE),
    };
    input::pointer(app, ui, rect, &response, &viewport);
    let viewport = Viewport {
        camera: app.session.worlds.active.as_ref().map_or(viewport.camera, |a| a.viewer.camera.clone()),
        width: viewport.width,
        height: viewport.height,
    };
    #[cfg(not(target_arch = "wasm32"))]
    render::paint(app, ui, rect, &viewport);
    #[cfg(target_arch = "wasm32")]
    {
        let _ = viewport;
        ui.painter().rect_filled(rect, 0.0, egui::Color32::WHITE);
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(if app.ui.world.tools.pan { egui::CursorIcon::Grab } else { egui::CursorIcon::Crosshair });
    }
}
pub fn shortcuts(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if ctx.egui_wants_keyboard_input() || app.ui.world.unfinished() || reviewing(app) {
        return;
    }
    let undo = ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::Z));
    let redo = ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::Y));
    if undo {
        run(app, "komaphone.world.undo", json!({}));
    }
    if redo {
        run(app, "komaphone.world.redo", json!({}));
    }
}
fn reviewing(app: &PhotocraftApp) -> bool {
    app.session.worlds.active.as_ref().is_some_and(|active| active.viewer.review.as_ref().is_some_and(|review| review.interval.is_some()))
}
