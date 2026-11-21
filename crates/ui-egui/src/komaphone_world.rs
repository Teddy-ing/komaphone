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
#[cfg(test)]
mod qol009_evidence;
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
    world_locked_width: Option<(u8, f64)>,
    #[serde(skip)]
    draft: Option<Box<Draft>>,
    #[serde(skip)]
    pan: Option<Pan>,
    #[serde(skip)]
    next_view_checkpoint: f64,
    #[serde(skip)]
    view_save_due: Option<f64>,
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
            world_locked_width: None,
            draft: None,
            pan: None,
            next_view_checkpoint: 0.0,
            view_save_due: None,
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
    brush: Option<Box<scene::brush::StrokeBrush>>,
    preview_brush: Option<Box<scene::brush::StrokeBrush>>,
    samples: Vec<scene::brush::StrokeSample>,
    start_time: f64,
    last_screen: [f64; 2],
    last_tick_time: f64,
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
    draft: Option<Box<Draft>>,
}
impl WorldUi {
    pub(crate) fn unfinished(&self) -> bool {
        self.draft.is_some()
    }
    /// True only when an unfinished stroke is stranded (interrupted or failed
    /// to save) rather than actively being drawn or briefly awaiting its local
    /// SQLite commit. Used for visual control disabling so the chrome does not
    /// grey out and flash on every normal stroke while `unfinished()` still
    /// blocks clicks, shortcuts, and close-time exits.
    pub(crate) fn has_stranded_draft(&self) -> bool {
        self.draft.as_ref().is_some_and(|draft| !draft.drawing && !draft.submitted)
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
    if response.clicked() && (id == "komaphone.world.export.cancel" || !app.ui.world.unfinished()) {
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
                let visual_enabled = !app.ui.world.has_stranded_draft();
                let response = ui.add_enabled(visual_enabled, egui::Button::new("Local canvases"));
                remember(ui, "world.local", &response);
                if response.clicked() && !app.ui.world.unfinished() {
                    app.ui.world.open = false;
                    crate::komaphone_canvas::open(app);
                }
                let response = ui.add_enabled(visual_enabled, egui::Button::new("Party & drawings"));
                remember(ui, "world.return", &response);
                if response.clicked() && !app.ui.world.unfinished() {
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
    toolbar(app, ui);
    panels::sidebar(app, ui);
    egui::Panel::bottom("world-status").exact_size(64.0).frame(egui::Frame::NONE.fill(tokens.chrome).inner_margin(5)).show(ui, |ui| {
        ui.label(if app.ui.world.draft.is_some() { "Current stroke has not been saved".into() } else { app.session.worlds.status() });
        if let Some(message) = app.ui.world.message.as_ref().or(app.session.worlds.error.as_ref()) {
            ui.colored_label(tokens.danger, message);
        } else if let Some(notice) = app.session.worlds.active.as_ref().and_then(|a| a.notice.as_ref()).or(app.session.worlds.notice.as_ref()) {
            ui.label(notice);
        } else {
            ui.label(
                egui::RichText::new(
                    "Wheel: zoom · Middle drag or Space: pan · Right-click: brush presets · Eraser and undo affect your device’s drawing layer",
                )
                .color(tokens.text_dim),
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
        app.ui.world.view_save_due = None;
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
    if let Some(due) = app.ui.world.view_save_due {
        if app.ui.world.pan.is_none() && now >= due {
            app.ui.world.view_save_due = None;
            if app.session.worlds.view_needs_save()
                && let Err(error) = app.session.world_save_view()
            {
                app.ui.world.message = Some(error.to_string());
            }
        } else {
            let wait = (due - now).clamp(0.016, 0.30);
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(wait));
        }
    }
}
pub(super) fn active_world_tool(app: &PhotocraftApp) -> crate::Tool {
    if app.ui.world.tools.erase {
        crate::Tool::Eraser
    } else if app.ui.tool == crate::Tool::Pencil {
        crate::Tool::Pencil
    } else {
        crate::Tool::Brush
    }
}

pub(super) fn sync_world_tool(app: &mut PhotocraftApp) {
    if !app.ui.world.tools.pan {
        let wanted = match app.ui.tool {
            crate::Tool::Brush | crate::Tool::Pencil | crate::Tool::Eraser => app.ui.tool,
            _ if app.ui.world.tools.erase => crate::Tool::Eraser,
            _ => crate::Tool::Brush,
        };
        if app.ui.tool != wanted {
            crate::tool_memory::select(app, wanted);
        }
        crate::paint_mouse::sync_tool_brush(app);
        app.ui.world.tools.erase = app.ui.tool == crate::Tool::Eraser;
    }
    app.ui.world.tools.brush_width = f64::from(app.session.tools.brush.size).clamp(1.0, f64::from(photocraft_paint::MAX_BRUSH_SIZE));
    app.ui.world.tools.color = app.session.tools.foreground;
}

fn select_world_tool(app: &mut PhotocraftApp, tool: crate::Tool) {
    app.ui.world.tools.pan = false;
    app.ui.world.tools.erase = tool == crate::Tool::Eraser;
    crate::tool_memory::select(app, tool);
    crate::paint_mouse::sync_tool_brush(app);
    sync_world_tool(app);
}

fn percent_control(ui: &mut egui::Ui, value: &mut f32, name: &str) -> bool {
    let mut percentage = (*value * 100.0).round();
    let response = ui.add(egui::DragValue::new(&mut percentage).range(0.0..=100.0).speed(1.0).suffix("%"));
    remember(ui, name, &response);
    let changed = response.changed();
    if changed {
        *value = (percentage / 100.0).clamp(0.0, 1.0);
    }
    changed
}

fn toolbar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    sync_world_tool(app);
    let tokens = Tokens::get(ui.ctx());
    egui::Panel::top("world-tools").exact_size(42.0).frame(egui::Frame::NONE.fill(tokens.chrome).inner_margin(6)).show(ui, |ui| {
        egui::ScrollArea::horizontal().id_salt("world-tools-scroll").auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.spacing_mut().button_padding.x = 5.0;
                ui.spacing_mut().interact_size.x = 34.0;
                let idle = !app.ui.world.unfinished();
                let visual_idle = !app.ui.world.has_stranded_draft();
                let visual_editing = visual_idle && !reviewing(app);
                let tool = active_world_tool(app);
                for (label, target, id) in [
                    ("Brush", Some(crate::Tool::Brush), "world.tool.brush"),
                    ("Pencil", Some(crate::Tool::Pencil), "world.tool.pencil"),
                    ("Eraser", Some(crate::Tool::Eraser), "world.tool.eraser"),
                    ("Pan", None, "world.tool.pan"),
                ] {
                    let selected = match target {
                        Some(t) => !app.ui.world.tools.pan && tool == t,
                        None => app.ui.world.tools.pan,
                    };
                    let response = ui.add_enabled(visual_idle && (target.is_none() || visual_editing), egui::Button::new(label).selected(selected));
                    remember(ui, id, &response);
                    if response.clicked() && idle {
                        match target {
                            Some(t) => select_world_tool(app, t),
                            None => app.ui.world.tools.pan = true,
                        }
                    }
                }
                ui.separator();
                let before = app.session.tools.brush.clone();
                let mut brush = before.clone();
                let active_tool = active_world_tool(app);
                ui.label("Size");
                let size_resp = ui
                    .add_enabled(visual_editing, egui::DragValue::new(&mut brush.size).range(1.0..=photocraft_paint::MAX_BRUSH_SIZE).speed(1.0).suffix(" px"));
                remember(ui, "world.width", &size_resp);
                let size_changed = size_resp.changed();
                ui.label("Opacity");
                ui.add_enabled_ui(visual_editing, |ui| {
                    percent_control(ui, &mut brush.opacity, "world.opacity");
                });
                if active_tool != crate::Tool::Pencil {
                    ui.label("Flow");
                    ui.add_enabled_ui(visual_editing, |ui| {
                        percent_control(ui, &mut brush.flow, "world.flow");
                    });
                }
                if active_tool != crate::Tool::Pencil && matches!(brush.tip, photocraft_paint::TipShape::Round) {
                    ui.label("Hard").on_hover_text("Brush hardness");
                    ui.add_enabled_ui(visual_editing, |ui| {
                        percent_control(ui, &mut brush.hardness, "world.hardness");
                    });
                }
                ui.label("Smooth").on_hover_text("Stroke smoothing");
                let mut smoothing = if brush.smoothing.enabled { brush.smoothing.amount } else { 0.0 };
                ui.add_enabled_ui(visual_editing, |ui| {
                    if percent_control(ui, &mut smoothing, "world.smoothing") {
                        brush.smoothing.enabled = smoothing > 0.0;
                        brush.smoothing.amount = smoothing;
                    }
                });
                let presets_resp = ui.add_enabled(visual_editing, egui::Button::new("Presets")).on_hover_text("Brush presets (or right-click canvas)");
                remember(ui, "world.brush.presets", &presets_resp);
                if presets_resp.clicked() && idle {
                    crate::brush_picker::open(&mut app.ui, ui.ctx(), [presets_resp.rect.left(), presets_resp.rect.bottom()]);
                }
                let settings_resp = ui
                    .add_enabled(visual_editing, egui::Button::new("Settings").selected(app.ui.panels.brush_settings))
                    .on_hover_text("Open Brush Settings (F5)");
                remember(ui, "world.brush.settings", &settings_resp);
                if settings_resp.clicked() && idle {
                    app.ui.panels.brush_settings = !app.ui.panels.brush_settings;
                }
                if idle {
                    crate::brush_panel::commit_gesture(app, ui.ctx(), &before, &brush);
                }
                ui.separator();
                let [fg_r, fg_g, fg_b, _] = app.session.tools.foreground;
                let mut fg = [fg_r, fg_g, fg_b].map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
                let fg_resp = ui.color_edit_button_srgb(&mut fg).on_hover_text("Foreground color");
                remember(ui, "world.color", &fg_resp);
                if fg_resp.changed() {
                    let _ =
                        app.run("tools.setColors", json!({"foreground": [f32::from(fg[0]) / 255.0, f32::from(fg[1]) / 255.0, f32::from(fg[2]) / 255.0, 1.0]}));
                }
                let swap_resp = ui.button("Swap").on_hover_text("Swap foreground and background colors (X)");
                remember(ui, "world.color.swap", &swap_resp);
                if swap_resp.clicked() {
                    let _ = app.run("tools.swapColors", json!({}));
                }
                let [bg_r, bg_g, bg_b, _] = app.session.tools.background;
                let mut bg = [bg_r, bg_g, bg_b].map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
                let bg_resp = ui.color_edit_button_srgb(&mut bg).on_hover_text("Background color");
                remember(ui, "world.color.bg", &bg_resp);
                if bg_resp.changed() {
                    let _ =
                        app.run("tools.setColors", json!({"background": [f32::from(bg[0]) / 255.0, f32::from(bg[1]) / 255.0, f32::from(bg[2]) / 255.0, 1.0]}));
                }
                let colors_resp = ui.button("Colors").on_hover_text("Color picker and palette");
                remember(ui, "world.colors.panel", &colors_resp);
                egui::Popup::from_toggle_button_response(&colors_resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                    ui.set_min_width(240.0);
                    ui.set_max_width(260.0);
                    ui.data_mut(|data| data.insert_temp(crate::panels::color_field_fill_id(), 160.0));
                    crate::panels::color_field(app, ui);
                    ui.separator();
                    ui.collapsing("Palette", |ui| crate::panels::swatches_ui::panel(app, ui));
                });
                let mut lock_width = app.ui.world.tools.lock_width;
                let lock_resp = ui
                    .add_enabled(visual_editing, egui::Checkbox::new(&mut lock_width, "Lock"))
                    .on_hover_text("Keep this stroke thickness in canvas coordinates while zooming.");
                remember(ui, "world.lockWidth", &lock_resp);
                if idle && (lock_resp.changed() || size_changed) {
                    app.ui.world.tools.lock_width = lock_width;
                    if let Some(active) = app.session.worlds.active.as_ref() {
                        let cam = &active.viewer.camera;
                        app.ui.world.world_locked_width =
                            app.ui.world.tools.lock_width.then_some((cam.region.depth, f64::from(app.session.tools.brush.size) / cam.zoom));
                    }
                }
                ui.separator();
                button(app, ui, "Undo mine", "komaphone.world.undo", json!({}), visual_editing);
                button(app, ui, "Redo mine", "komaphone.world.redo", json!({}), visual_editing);
                button(app, ui, "Home", "komaphone.world.home", json!({}), visual_idle);
                if app.ui.world.has_stranded_draft() && ui.button("Retry unfinished stroke").clicked() {
                    input::finish(app);
                }
                app.ui.world.tools.brush_width = f64::from(app.session.tools.brush.size).clamp(1.0, f64::from(photocraft_paint::MAX_BRUSH_SIZE));
                app.ui.world.tools.color = app.session.tools.foreground;
            });
        });
    });
}
fn drawing_area(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let (rect, response) = ui.allocate_exact_size(ui.available_size().max(egui::vec2(1.0, 1.0)), Sense::click_and_drag());
    app.last_canvas_rect = rect;
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
    let redo_shift = ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::Z));
    let undo = ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::Z));
    let redo = ctx.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::Y));
    if undo {
        run(app, "komaphone.world.undo", json!({}));
    }
    if redo || redo_shift {
        run(app, "komaphone.world.redo", json!({}));
    }
    if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::B)) {
        select_world_tool(app, crate::Tool::Brush);
    }
    if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::E)) {
        select_world_tool(app, crate::Tool::Eraser);
    }
    if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::X)) {
        let _ = app.run("tools.swapColors", json!({}));
    }
    if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::D)) {
        let _ = app.run("tools.defaultColors", json!({}));
    }
    if ctx.input_mut(|input| input.consume_key(egui::Modifiers::SHIFT, egui::Key::OpenBracket)) {
        let _ = app.run("tools.decreaseBrushHardness", json!({}));
    } else if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::OpenBracket)) {
        let _ = app.run("tools.decreaseBrushSize", json!({}));
    }
    if ctx.input_mut(|input| input.consume_key(egui::Modifiers::SHIFT, egui::Key::CloseBracket)) {
        let _ = app.run("tools.increaseBrushHardness", json!({}));
    } else if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::CloseBracket)) {
        let _ = app.run("tools.increaseBrushSize", json!({}));
    }
    if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::F5)) {
        app.ui.panels.brush_settings = !app.ui.panels.brush_settings;
    }
}
fn reviewing(app: &PhotocraftApp) -> bool {
    app.session.worlds.active.as_ref().is_some_and(|active| active.viewer.review.as_ref().is_some_and(|review| review.interval.is_some()))
}
