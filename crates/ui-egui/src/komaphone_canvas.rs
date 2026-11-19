//! Single-user continuous canvas workspace. Scene mutations cross the engine command
//! seam; this module keeps only controls, pointer gestures and presentation state.

use crate::{PhotocraftApp, theme::Tokens};
use egui::{Color32, Pos2, Rect, Sense};
use photocraft_engine::scene::{self, Camera, CanvasId, RegionKey, Srgba, Stroke, Viewport, coordinates};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::VecDeque;

/// The K5 drawing plane is opaque white artwork, independent of the shell theme.
/// Transparent layers and imported/remixed scene assets belong to later world work.
const PAPER: Color32 = Color32::WHITE;
const MAX_FRAME_SAMPLES: usize = 240;

/// Previous eframe CPU UI/tessellation duration and wall frame-start interval.
/// CPU time excludes surface acquisition, queue submission and presentation.
/// The interval includes scheduling/display waits, but neither value measures
/// GPU completion or input-to-display latency.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CanvasFrameSample {
    pub frame: u64,
    pub cpu_ms: f64,
    pub interval_ms: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CanvasUi {
    pub open: bool,
    pub name: String,
    pub rename: String,
    pub bookmark_name: String,
    pub brush_width: f64,
    pub color: [f32; 4],
    pub erase: bool,
    pub pan: bool,
    pub lock_width: bool,
    pub render_ms: f64,
    pub drawn_paths: usize,
    pub drawn_points: usize,
    #[serde(skip_deserializing)]
    pub close_prompt: bool,
    #[serde(skip)]
    pub(crate) close_approval: Option<crate::discard_ui::canvas_close::Approval>,
    #[serde(skip)]
    pub message: Option<String>,
    #[serde(skip)]
    pub frame_samples: VecDeque<CanvasFrameSample>,
    #[serde(skip)]
    previous_frame_time: Option<f64>,
    #[serde(skip)]
    selected: Option<CanvasId>,
    #[serde(skip)]
    locked_width: Option<WidthLock>,
    #[serde(skip)]
    gesture: Option<Gesture>,
    #[serde(skip)]
    unsaved_stroke: Option<DraftStroke>,
}

impl Default for CanvasUi {
    fn default() -> Self {
        Self {
            open: false,
            name: "My canvas".into(),
            rename: String::new(),
            bookmark_name: "A detail".into(),
            brush_width: 5.0,
            color: [0.08, 0.12, 0.18, 1.0],
            erase: false,
            pan: false,
            lock_width: false,
            render_ms: 0.0,
            drawn_paths: 0,
            drawn_points: 0,
            close_prompt: false,
            close_approval: None,
            message: None,
            frame_samples: VecDeque::new(),
            previous_frame_time: None,
            selected: None,
            locked_width: None,
            gesture: None,
            unsaved_stroke: None,
        }
    }
}

impl CanvasUi {
    pub(crate) fn has_uncommitted_drawing(&self) -> bool {
        self.unsaved_stroke.is_some() || matches!(self.gesture, Some(Gesture::Draw(_)))
    }

    pub(crate) fn prepare_close(&mut self) {
        if matches!(self.gesture, Some(Gesture::Pan { .. })) {
            self.gesture = None;
        } else {
            self.cancel_gesture_for_switch();
        }
        if self.has_uncommitted_drawing() {
            self.message = Some("An unfinished canvas stroke is still unsaved. Retry saving it on its canvas before closing.".into());
        }
    }

    pub(crate) fn drawing_snapshot(&self) -> DrawingSnapshot {
        DrawingSnapshot { gesture: self.gesture.clone(), unsaved: self.unsaved_stroke.clone() }
    }

    /// Keep an interrupted drawing available for explicit recovery on its canvas.
    pub(crate) fn cancel_gesture_for_switch(&mut self) {
        match self.gesture.take() {
            Some(Gesture::Draw(draft)) => {
                if self.unsaved_stroke.is_none() {
                    self.unsaved_stroke = Some(draft);
                } else {
                    // Supported input cannot start a second gesture while a draft
                    // awaits saving. Preserve both slots even in an inconsistent
                    // restored/test state; retrying the first promotes this one.
                    self.gesture = Some(Gesture::Draw(draft));
                }
                self.message = Some("Canvas changed during drawing. The unfinished stroke is retained; return to its canvas to save it.".into());
            }
            Some(Gesture::Pan { .. }) => {
                self.message = Some("Canvas changed during panning; the previous gesture was cancelled.".into());
            }
            None => {}
        }
    }

    /// Retain a bounded measurement ring without persisting transient timings.
    pub fn record_frame(&mut self, frame: u64, cpu_ms: f64, at_seconds: f64) {
        if !cpu_ms.is_finite()
            || cpu_ms < 0.0
            || !at_seconds.is_finite()
            || at_seconds < 0.0
            || self.frame_samples.back().is_some_and(|sample| sample.frame >= frame)
        {
            return;
        }
        let interval_ms = match self.previous_frame_time {
            Some(previous) => {
                let elapsed = (at_seconds - previous) * 1000.0;
                if !elapsed.is_finite() || elapsed <= 0.0 {
                    return;
                }
                Some(elapsed)
            }
            None => None,
        };
        if self.frame_samples.len() >= MAX_FRAME_SAMPLES {
            self.frame_samples.pop_front();
        }
        self.frame_samples.push_back(CanvasFrameSample { frame, cpu_ms, interval_ms });
        self.previous_frame_time = Some(at_seconds);
    }
}

#[derive(Clone, Debug, PartialEq)]
struct WidthLock {
    depth: u8,
    local: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DraftStroke {
    pub(crate) viewport: Viewport,
    pub(crate) anchor: RegionKey,
    pub(crate) points: Vec<[f64; 2]>,
    pub(crate) width: f64,
    pub(crate) color: Srgba,
    pub(crate) erase: bool,
    pub(crate) warning: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DrawingSnapshot {
    gesture: Option<Gesture>,
    unsaved: Option<DraftStroke>,
}

#[derive(Clone, Debug, PartialEq)]
enum Gesture {
    Draw(DraftStroke),
    Pan { camera: Camera, start: [f64; 2], button: egui::PointerButton },
}

fn run(app: &mut PhotocraftApp, command: &str, params: Value) -> bool {
    match app.run(command, params) {
        Ok(_) => {
            app.ui.canvas.message = None;
            true
        }
        Err(error) => {
            set_error(app, error);
            false
        }
    }
}

fn set_error(app: &mut PhotocraftApp, error: String) {
    app.ui.status = error.clone();
    app.ui.canvas.message = Some(error);
}

fn remember(ui: &egui::Ui, id: &str, response: &egui::Response) {
    crate::komaphone_workspace::remember(ui, id, response);
}

fn button(app: &mut PhotocraftApp, ui: &mut egui::Ui, label: &str, command: &str, params: Value, enabled: bool) {
    let response = ui.add_enabled(enabled && app.session.is_enabled(command), egui::Button::new(label));
    remember(ui, command, &response);
    if response.clicked() {
        run(app, command, params);
    }
}

pub fn open(app: &mut PhotocraftApp) {
    if app.ui.transform.is_some() || app.ui.cam_transform.is_some() || app.ui.type_transform.is_some() || app.ui.symmetry_transform.is_some() {
        set_error(app, "Finish or cancel the current editor transform before opening Canvases.".into());
        return;
    }
    if let Some(drag) = app.drag.take() {
        crate::canvas::finish_gesture(app, drag);
    }
    crate::type_tool::commit(app);
    app.ui.canvas.open = true;
    app.ui.gallery.open = false;
    app.ui.rooms.open = false;
    if !app.session.canvases.busy() {
        run(app, "komaphone.canvas.list", json!({}));
    }
}

/// Own the whole Komaphone central workspace while open, including its controls.
/// The caller must not draw finite-document chrome or process its shortcuts here.
pub fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    if app.ui.canvas.close_prompt {
        ui.disable();
    }
    sync_active(app);
    let tokens = Tokens::get(ui.ctx());
    let (caption_left, caption_right) = crate::titlebar::reserved(app, ui.ctx());
    egui::Panel::top("komaphone-canvas-heading")
        .exact_size(crate::titlebar::bar_height(app, ui.ctx(), 42.0))
        .frame(egui::Frame::NONE.fill(tokens.chrome).inner_margin(egui::Margin::symmetric(10, 0)))
        .show(ui, |ui| {
            let full = ui.max_rect();
            ui.horizontal_centered(|ui| {
                ui.add_space(caption_left);
                ui.strong("Komaphone · Canvases");
                let response = ui.add_enabled(app.ui.canvas.gesture.is_none() && app.ui.canvas.unsaved_stroke.is_none(), egui::Button::new("Party & drawings"));
                remember(ui, "canvas.return", &response);
                if response.clicked() {
                    app.ui.canvas.open = false;
                    if app.session.canvases.flight.is_some() {
                        run(app, "komaphone.canvas.flight", json!({"state":"cancel"}));
                    }
                }
                ui.separator();
                ui.label(egui::RichText::new("Local only").color(tokens.text_dim));
                let response = ui.add_enabled(!app.ui.canvas.has_uncommitted_drawing(), egui::Button::new("Shared canvases"));
                remember(ui, "canvas.shared", &response);
                if response.clicked() {
                    crate::komaphone_world::open(app);
                }
                let (rect, response) = ui.allocate_exact_size(egui::vec2((ui.available_width() - caption_right - 6.0).max(0.0), 32.0), Sense::click_and_drag());
                ui.ctx().data_mut(|data| data.insert_temp(crate::titlebar::modal_drag_area_id(), rect));
                if response.drag_started() && app.custom_titlebar {
                    crate::titlebar::start_os_move(ui.ctx());
                }
                if response.double_clicked() && app.custom_titlebar {
                    let maximized = ui.ctx().input(|input| input.viewport().maximized.unwrap_or(false));
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                }
            });
            if app.custom_titlebar {
                crate::titlebar::caption_buttons(app, ui, full);
            }
        });
    sidebar(app, ui);
    toolbar(app, ui);
    egui::Panel::bottom("komaphone-canvas-status").exact_size(64.0).frame(egui::Frame::NONE.fill(tokens.chrome).inner_margin(5)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(if app.ui.canvas.unsaved_stroke.is_some() {
                "Unsaved stroke · retry to save it".into()
            } else if matches!(app.ui.canvas.gesture, Some(Gesture::Draw(_))) {
                "Drawing · current stroke not saved".into()
            } else {
                app.session.canvases.status_text()
            });
            if let Some(camera) = &app.session.canvases.camera {
                let zoom = camera.zoom * 100.0_f64.powi(i32::from(camera.region.depth));
                ui.label(if zoom < 100_000.0 { format!("Zoom {zoom:.2}×") } else { format!("Zoom {zoom:.1e}×") });
            }
            if app.session.canvases.error.is_some() {
                button(app, ui, "Retry save", "komaphone.canvas.retry", json!({}), true);
            }
        });
        if let Some(error) = app.session.canvases.error.as_ref().or(app.ui.canvas.message.as_ref()) {
            ui.colored_label(tokens.danger, error);
        } else if let Some(notice) = &app.session.canvases.notice {
            ui.add(egui::Label::new(egui::RichText::new(notice).color(tokens.warning)).truncate()).on_hover_text(notice);
        } else {
            ui.label(egui::RichText::new("Wheel: zoom at pointer · Middle drag or Space: pan").color(tokens.text_dim));
        }
    });
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(tokens.canvas)).show(ui, |ui| {
        if !app.session.canvases.available() {
            ui.centered_and_justified(|ui| {
                ui.label("Local canvas storage is unavailable. Reopen the Windows app with a writable local data folder.");
            });
        } else if app.session.canvases.camera.is_some() {
            drawing_area(app, ui);
        } else {
            ui.centered_and_justified(|ui| {
                ui.label("Create a canvas or choose one from the list. Pan, zoom and draw directly at each new scale.");
            });
        }
    });
}

fn sync_active(app: &mut PhotocraftApp) {
    let active = app.session.canvases.active.as_ref();
    let id = active.map(|info| info.id);
    if app.ui.canvas.selected != id {
        app.ui.canvas.selected = id;
        app.ui.canvas.rename = active.map_or_else(String::new, |info| info.name.clone());
        // A stale pointer may never finish in a different canvas. Preserve its
        // unfinished drawing for recovery instead of treating it as a saved edit.
        app.ui.canvas.cancel_gesture_for_switch();
        app.ui.canvas.locked_width = None;
        app.ui.canvas.lock_width = false;
        app.ui.canvas.frame_samples.clear();
        app.ui.canvas.previous_frame_time = None;
    }
}

fn sidebar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let tokens = Tokens::get(ui.ctx());
    egui::Panel::left("komaphone-canvas-list").exact_size(238.0).frame(egui::Frame::NONE.fill(tokens.card).inner_margin(10)).show(ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Your canvases");
            ui.label(egui::RichText::new("Private on this computer").color(tokens.text_dim));
            let name_label = ui.label("New canvas name");
            let response = ui.text_edit_singleline(&mut app.ui.canvas.name).labelled_by(name_label.id);
            remember(ui, "canvas.name", &response);
            let idle = !app.session.canvases.busy() && app.ui.canvas.gesture.is_none() && app.ui.canvas.unsaved_stroke.is_none();
            button(app, ui, "Create canvas", "komaphone.canvas.create", json!({"name":app.ui.canvas.name}), idle);
            ui.separator();
            for item in app.session.canvases.items.clone() {
                let selected = app.session.canvases.active.as_ref().is_some_and(|active| active.id == item.id);
                ui.push_id(item.id.as_str(), |ui| {
                    let response = ui.add_enabled(idle, egui::Button::new(&item.name).selected(selected));
                    remember(ui, &format!("canvas.open.{}", item.id.as_str()), &response);
                    if response.clicked() {
                        run(app, "komaphone.canvas.open", json!({"canvas":item.id}));
                    }
                    ui.small(format!("{} strokes · {}", item.stroke_count, item.id.as_str().get(..6).unwrap_or_default()));
                });
            }
            let Some(active) = app.session.canvases.active.clone() else { return };
            ui.separator();
            let rename_label = ui.label("Canvas name");
            let response = ui.text_edit_singleline(&mut app.ui.canvas.rename).labelled_by(rename_label.id);
            remember(ui, "canvas.rename", &response);
            button(app, ui, "Rename", "komaphone.canvas.rename", json!({"canvas":active.id,"name":app.ui.canvas.rename}), idle);
            ui.separator();
            ui.heading("Bookmarks");
            let bookmark_label = ui.label("Bookmark name");
            let response = ui.text_edit_singleline(&mut app.ui.canvas.bookmark_name).labelled_by(bookmark_label.id);
            remember(ui, "canvas.bookmarkName", &response);
            button(app, ui, "Save this view", "komaphone.canvas.bookmark", json!({"name":app.ui.canvas.bookmark_name}), idle);
            for (index, bookmark) in active.bookmarks.iter().enumerate() {
                ui.push_id(index, |ui| {
                    ui.label(&bookmark.name);
                    ui.horizontal(|ui| {
                        let response = ui.add_enabled(app.ui.canvas.gesture.is_none(), egui::Button::new("Go"));
                        remember(ui, &format!("canvas.bookmark.{index}.go"), &response);
                        if response.clicked() {
                            run(app, "komaphone.canvas.go", json!({"name":bookmark.name,"animate":false}));
                        }
                        let response = ui.add_enabled(app.ui.canvas.gesture.is_none(), egui::Button::new("Auto zoom"));
                        remember(ui, &format!("canvas.bookmark.{index}.play"), &response);
                        if response.clicked() {
                            run(app, "komaphone.canvas.go", json!({"name":bookmark.name,"animate":true}));
                        }
                    });
                });
            }
            ui.horizontal_wrapped(|ui| {
                for (label, state) in [("Pause", "pause"), ("Resume", "resume"), ("Cancel", "cancel")] {
                    let available = app.session.canvases.flight.as_ref().is_some_and(|flight| match state {
                        "pause" => !flight.paused,
                        "resume" => flight.paused,
                        _ => true,
                    });
                    let response = ui.add_enabled(available, egui::Button::new(label));
                    remember(ui, &format!("canvas.flight.{state}"), &response);
                    if response.clicked() {
                        run(app, "komaphone.canvas.flight", json!({"state":state}));
                    }
                }
            });
            if let Some(notice) = &app.session.canvases.notice {
                ui.label(notice);
            }
            ui.add_space(8.0);
            ui.small("Local canvases use opaque colors and stay on this computer.");
            ui.collapsing("Account link", |ui| {
                ui.label("Link this computer’s local canvas ownership to your connected profile. The original drawings and their IDs stay intact.");
                let busy = app.session.canvases.link.busy;
                let linked = app.session.canvases.link.binding.as_ref().is_some_and(|binding| binding.active);
                if linked {
                    ui.label("Local ownership is linked to a profile.");
                }
                button(app, ui, "Link to current profile", "komaphone.canvas.link", json!({}), !busy && app.session.worlds.connected());
                button(app, ui, "Undo account link", "komaphone.canvas.link.rollback", json!({}), !busy && linked && app.session.worlds.connected());
                if busy {
                    ui.label("Checking ownership and saving a recovery backup…");
                }
                if let Some(error) = &app.session.canvases.link.error {
                    ui.colored_label(tokens.danger, error);
                }
            });
        });
    });
}

fn toolbar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let tokens = Tokens::get(ui.ctx());
    egui::Panel::top("komaphone-canvas-tools").exact_size(42.0).frame(egui::Frame::NONE.fill(tokens.chrome).inner_margin(6)).show(ui, |ui| {
        egui::ScrollArea::horizontal().id_salt("canvas-tools-scroll").show(ui, |ui| {
            ui.horizontal(|ui| {
                for (name, erase, pan) in [("Brush", false, false), ("Eraser", true, false), ("Pan", false, true)] {
                    let response = ui.selectable_label(app.ui.canvas.erase == erase && app.ui.canvas.pan == pan, name);
                    remember(ui, &format!("canvas.tool.{}", name.to_ascii_lowercase()), &response);
                    if response.clicked() && app.ui.canvas.gesture.is_none() {
                        app.ui.canvas.erase = erase;
                        app.ui.canvas.pan = pan;
                    }
                }
                ui.separator();
                let response = ui.add(egui::DragValue::new(&mut app.ui.canvas.brush_width).range(1.0..=128.0).speed(0.5).suffix(" px"));
                remember(ui, "canvas.width", &response);
                let width_changed = response.changed();
                let response = ui
                    .checkbox(&mut app.ui.canvas.lock_width, "Lock thickness")
                    .on_hover_text("Keep this stroke thickness in canvas coordinates while zooming. Unlocked strokes begin at the selected screen width.");
                remember(ui, "canvas.lockWidth", &response);
                if (response.changed() || width_changed)
                    && let Some(camera) = &app.session.canvases.camera
                {
                    app.ui.canvas.locked_width =
                        app.ui.canvas.lock_width.then_some(WidthLock { depth: camera.region.depth, local: app.ui.canvas.brush_width / camera.zoom });
                }
                let mut opaque = [
                    (app.ui.canvas.color[0] * 255.0).round() as u8,
                    (app.ui.canvas.color[1] * 255.0).round() as u8,
                    (app.ui.canvas.color[2] * 255.0).round() as u8,
                ];
                let response = ui.color_edit_button_srgb(&mut opaque);
                remember(ui, "canvas.color", &response);
                if response.changed() {
                    app.ui.canvas.color = [f32::from(opaque[0]) / 255.0, f32::from(opaque[1]) / 255.0, f32::from(opaque[2]) / 255.0, 1.0];
                }
                let idle = !app.session.canvases.busy() && app.ui.canvas.gesture.is_none() && app.ui.canvas.unsaved_stroke.is_none();
                button(app, ui, "Undo", "komaphone.canvas.undo", json!({}), idle);
                button(app, ui, "Redo", "komaphone.canvas.redo", json!({}), idle);
                button(app, ui, "Home", "komaphone.canvas.home", json!({}), app.ui.canvas.gesture.is_none());
            });
        });
    });
}

/// Called instead of finite-editor shortcut dispatch while this workspace owns input.
pub fn shortcuts(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if app.ui.canvas.close_prompt {
        return;
    }
    if !app.ui.canvas.open || ctx.text_edit_focused() || !app.ui.dialogs.is_empty() {
        return;
    }
    if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        app.ui.canvas.gesture = None;
        run(app, "komaphone.canvas.flight", json!({"state":"cancel"}));
    }
    if app.ui.canvas.gesture.is_none() && app.ui.canvas.unsaved_stroke.is_none() {
        for (mods, key, command) in [
            (egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z, "komaphone.canvas.redo"),
            (egui::Modifiers::COMMAND, egui::Key::Z, "komaphone.canvas.undo"),
            (egui::Modifiers::COMMAND, egui::Key::Y, "komaphone.canvas.redo"),
            (egui::Modifiers::NONE, egui::Key::Home, "komaphone.canvas.home"),
        ] {
            if ctx.input_mut(|input| input.consume_key(mods, key)) {
                run(app, command, json!({}));
            }
        }
    }
}

fn drawing_area(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    if let Some(draft) = app.ui.canvas.unsaved_stroke.clone() {
        ui.horizontal(|ui| {
            ui.colored_label(Tokens::get(ui.ctx()).danger, "This stroke is not saved. Its drawing is retained here.");
            if app.session.canvases.active.as_ref().is_some_and(|active| active.id == draft.viewport.camera.canvas) {
                let response = ui.button("Retry stroke");
                remember(ui, "canvas.retryStroke", &response);
                if response.clicked() && commit(app, &draft) {
                    app.ui.canvas.unsaved_stroke = None;
                    app.ui.canvas.cancel_gesture_for_switch();
                }
            } else {
                let response = ui.add_enabled(!app.session.canvases.busy(), egui::Button::new("Return to its canvas"));
                remember(ui, "canvas.recoverStroke", &response);
                if response.clicked() {
                    run(app, "komaphone.canvas.open", json!({"canvas":draft.viewport.camera.canvas}));
                }
            }
        });
    }
    let (rect, response) = ui.allocate_exact_size(ui.available_size().max(egui::vec2(1.0, 1.0)), Sense::click_and_drag());
    remember(ui, "canvas.drawing", &response);
    let width = (rect.width().floor() as u32).clamp(1, scene::MAX_VIEW_SIDE);
    let height = (rect.height().floor() as u32).clamp(1, scene::MAX_VIEW_SIDE);
    app.session.set_canvas_viewport(width, height);
    let Some(camera) = app.session.canvases.camera.clone() else { return };
    let viewport = Viewport { camera, width, height };
    pointer(app, ui, rect, &response, &viewport);
    let viewport = Viewport { camera: app.session.canvases.camera.clone().unwrap_or(viewport.camera), width, height };
    let start = web_time::Instant::now();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, PAPER);
    let mut counts = (0usize, 0usize);
    let mut render_error = None;
    if let Some(view) = &app.session.canvases.view {
        for stroke in &view.strokes {
            if stroke.canvas == viewport.camera.canvas
                && let Err(error) = paint_stroke(&painter, rect.min, stroke, &viewport, &mut counts)
            {
                render_error.get_or_insert(error);
                break;
            }
        }
    }
    if render_error.is_none() {
        for stroke in &app.session.canvases.pending_strokes {
            if stroke.canvas == viewport.camera.canvas
                && let Err(error) = paint_stroke(&painter, rect.min, stroke, &viewport, &mut counts)
            {
                render_error.get_or_insert(error);
                break;
            }
        }
    }
    let draft = match app.ui.canvas.gesture.as_ref() {
        Some(Gesture::Draw(draft)) => Some(draft),
        _ => app.ui.canvas.unsaved_stroke.as_ref(),
    };
    if let Some(draft) = draft
        && render_error.is_none()
        && draft.viewport.camera.canvas == viewport.camera.canvas
        && let Some(active) = &app.session.canvases.active
        && let Ok(id) = scene::StrokeId::new("ffffffffffffffffffffffffffffffff")
    {
        let stroke = Stroke {
            id,
            canvas: active.id,
            owner: active.owner,
            revision: 0,
            anchor: draft.anchor,
            points: draft.points.clone(),
            width: draft.width,
            color: draft.color,
            erase: draft.erase,
            brush: None,
            samples: None,
        };
        if let Err(error) = paint_stroke(&painter, rect.min, &stroke, &viewport, &mut counts) {
            render_error.get_or_insert(error);
        }
    }
    app.ui.canvas.render_ms = start.elapsed().as_secs_f64() * 1000.0;
    app.ui.canvas.drawn_paths = counts.0;
    app.ui.canvas.drawn_points = counts.1;
    if let Some(error) = render_error {
        set_error(app, format!("Cannot render this canvas view: {error}. Try Home or reopen this canvas; saved artwork remains unchanged."));
    }
    if response.hovered() && !app.ui.canvas.pan {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    } else if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
}

fn paint_stroke(painter: &egui::Painter, origin: Pos2, stroke: &Stroke, viewport: &Viewport, counts: &mut (usize, usize)) -> scene::Result<()> {
    let Some(projected) = scene::render::project(stroke, viewport)? else { return Ok(()) };
    let vertices = projected.fills.iter().map(Vec::len).fold(0usize, usize::saturating_add);
    if counts.1.saturating_add(vertices) > scene::round::MAX_MESH_VERTICES {
        return Err(scene::Error::Limit("visible geometry; zoom in or move closer"));
    }
    let rgba = projected.color.rgba;
    let color = if projected.erase {
        Color32::from_rgba_unmultiplied(255, 255, 255, (rgba[3] * 255.0).round() as u8)
    } else {
        Color32::from_rgba_unmultiplied(
            (rgba[0] * 255.0).round() as u8,
            (rgba[1] * 255.0).round() as u8,
            (rgba[2] * 255.0).round() as u8,
            (rgba[3] * 255.0).round() as u8,
        )
    };
    for polygon in projected.fills {
        let points: Vec<Pos2> = polygon.into_iter().map(|[x, y]| origin + egui::vec2(x, y)).collect();
        counts.0 = counts.0.saturating_add(1);
        counts.1 = counts.1.saturating_add(points.len());
        if points.len() >= 3 {
            painter.add(egui::Shape::convex_polygon(points, color, egui::Stroke::NONE));
        }
    }
    if !projected.paths.is_empty() {
        let geometry = scene::round::mesh(
            &projected.paths,
            projected.width,
            [viewport.width, viewport.height],
            painter.ctx().pixels_per_point(),
            scene::round::MAX_MESH_VERTICES.saturating_sub(counts.1),
        )?;
        counts.0 = counts.0.saturating_add(projected.paths.len());
        counts.1 = counts.1.saturating_add(geometry.vertices.len());
        let mesh = egui::Mesh {
            indices: geometry.indices,
            vertices: geometry
                .vertices
                .into_iter()
                .map(|vertex| egui::epaint::Vertex {
                    pos: origin + egui::vec2(vertex.position[0], vertex.position[1]),
                    uv: egui::epaint::WHITE_UV,
                    color: color.gamma_multiply(vertex.coverage),
                })
                .collect(),
            ..Default::default()
        };
        painter.add(egui::Shape::mesh(mesh));
    }
    Ok(())
}

fn pointer(app: &mut PhotocraftApp, ui: &mut egui::Ui, rect: Rect, response: &egui::Response, viewport: &Viewport) {
    if app.ui.canvas.close_prompt {
        return;
    }
    if app.ui.canvas.unsaved_stroke.is_some() && matches!(app.ui.canvas.gesture, Some(Gesture::Draw(_))) {
        return;
    }
    let (events, space_at_end) = ui.input(|input| (input.events.clone(), input.key_down(egui::Key::Space)));
    let mut space = events
        .iter()
        .find_map(|event| match event {
            egui::Event::Key { key: egui::Key::Space, pressed, repeat: false, .. } => Some(!pressed),
            _ => None,
        })
        .unwrap_or(space_at_end);
    let pressed = events
        .iter()
        .any(|event| matches!(event, egui::Event::PointerButton { button: egui::PointerButton::Primary | egui::PointerButton::Middle, pressed: true, .. }));
    if app.ui.canvas.gesture.is_none() && response.hovered() && !pressed {
        wheel(app, ui, rect, viewport);
    }
    let same_canvas = app.ui.canvas.gesture.as_ref().is_none_or(|gesture| match gesture {
        Gesture::Draw(draft) => draft.viewport.camera.canvas == viewport.camera.canvas,
        Gesture::Pan { camera, .. } => camera.canvas == viewport.camera.canvas,
    });
    if !same_canvas {
        app.ui.canvas.cancel_gesture_for_switch();
        return;
    }

    // A native frame can contain many actual positions, including a complete
    // press/move/release gesture. The final PointerState position loses that
    // ordering and turns curved motion into chords. Keep the measured events;
    // append_draft still bounds the saved samples and local coordinate extent.
    let mut pending_camera: Option<Camera> = None;
    for event in events {
        match event {
            egui::Event::Key { key: egui::Key::Space, pressed, .. } => space = pressed,
            egui::Event::PointerButton { pos, button, pressed: true, .. }
                if matches!(button, egui::PointerButton::Primary | egui::PointerButton::Middle)
                    && app.ui.canvas.gesture.is_none()
                    && app.ui.canvas.unsaved_stroke.is_none()
                    && response.enabled()
                    && rect.intersect(ui.clip_rect()).contains(pos)
                    && ui.ctx().layer_id_at(pos) == Some(response.layer_id) =>
            {
                run(app, "komaphone.canvas.flight", json!({"state":"cancel"}));
                let point = [f64::from(pos.x - rect.left()), f64::from(pos.y - rect.top())];
                let mut view = viewport.clone();
                if let Some(camera) = &pending_camera {
                    view.camera = camera.clone();
                }
                if app.ui.canvas.pan || space || button == egui::PointerButton::Middle {
                    app.ui.canvas.gesture = Some(Gesture::Pan { camera: view.camera, start: point, button });
                } else {
                    match begin_draft(&app.ui.canvas, &view, point) {
                        Ok(draft) => app.ui.canvas.gesture = Some(Gesture::Draw(draft)),
                        Err(error) => set_error(app, error),
                    }
                }
            }
            egui::Event::PointerMoved(pos) => move_pointer(app, rect, pos, &mut pending_camera),
            egui::Event::PointerButton { pos, button, pressed: false, .. } => {
                let ends_gesture = match &app.ui.canvas.gesture {
                    Some(Gesture::Draw(_)) => button == egui::PointerButton::Primary,
                    Some(Gesture::Pan { button: held, .. }) => button == *held,
                    None => false,
                };
                if ends_gesture {
                    // Release coordinates need not be preceded by PointerMoved.
                    move_pointer(app, rect, pos, &mut pending_camera);
                    finish_pointer(app);
                }
            }
            // Do not connect a later re-entry to an interrupted stroke. Save the
            // valid prefix; a refused save keeps the usual recoverable draft.
            egui::Event::PointerGone | egui::Event::WindowFocused(false) => finish_pointer(app),
            _ => {}
        }
    }

    // Camera motion only needs its latest measured position. Coalesce its
    // command dispatch and viewport refresh while retaining all drawing samples.
    if let Some(camera) = pending_camera {
        run(app, "komaphone.canvas.camera", json!({"camera":camera}));
    }
}

fn move_pointer(app: &mut PhotocraftApp, rect: Rect, position: Pos2, pending_camera: &mut Option<Camera>) {
    let Some(mut gesture) = app.ui.canvas.gesture.take() else { return };
    let point = [f64::from(position.x - rect.left()), f64::from(position.y - rect.top())];
    match &mut gesture {
        Gesture::Draw(draft) => {
            if let Err(error) = append_draft(draft, point) {
                draft.warning = Some(error.clone());
                set_error(app, error);
            }
        }
        Gesture::Pan { camera, start, .. } => match coordinates::pan(camera, [point[0] - start[0], point[1] - start[1]]) {
            Ok(camera) => *pending_camera = Some(camera),
            Err(error) => set_error(app, error.to_string()),
        },
    }
    app.ui.canvas.gesture = Some(gesture);
}

fn finish_pointer(app: &mut PhotocraftApp) {
    if let Some(Gesture::Draw(draft)) = app.ui.canvas.gesture.take()
        && !commit(app, &draft)
    {
        app.ui.canvas.unsaved_stroke = Some(draft);
    }
}

fn wheel(app: &mut PhotocraftApp, ui: &egui::Ui, rect: Rect, viewport: &Viewport) {
    let events = ui.input(|input| input.events.clone());
    let Some(position) = ui.input(|input| input.pointer.hover_pos()) else { return };
    let screen = [f64::from(position.x - rect.left()), f64::from(position.y - rect.top())];
    let mut current = viewport.clone();
    let mut error = None;
    for event in events {
        let camera = match event {
            egui::Event::MouseWheel { unit, delta, modifiers, .. } => {
                let multiplier = match unit {
                    egui::MouseWheelUnit::Point => 1.0,
                    egui::MouseWheelUnit::Line => 40.0,
                    egui::MouseWheelUnit::Page => f64::from(current.height),
                };
                let dx = f64::from(delta.x) * multiplier;
                let dy = f64::from(delta.y) * multiplier;
                if modifiers.alt {
                    coordinates::pan(&current.camera, [dx, dy])
                } else {
                    coordinates::zoom_at(&current, screen, (dy.clamp(-4096.0, 4096.0) * 0.0025).exp())
                }
            }
            egui::Event::Zoom(factor) => coordinates::zoom_at(&current, screen, f64::from(factor)),
            _ => continue,
        };
        match camera {
            Ok(camera) => current.camera = camera,
            Err(failure) => error = Some(failure.to_string()),
        }
    }
    if current.camera != viewport.camera {
        run(app, "komaphone.canvas.camera", json!({"camera":current.camera}));
    }
    if let Some(error) = error {
        set_error(app, error);
    }
}

pub(crate) fn begin_draft(state: &CanvasUi, viewport: &Viewport, screen: [f64; 2]) -> Result<DraftStroke, String> {
    let width = if state.lock_width {
        state.locked_width.as_ref().map_or(state.brush_width / viewport.camera.zoom, |lock| {
            lock.local * 100.0_f64.powi(i32::from(viewport.camera.region.depth) - i32::from(lock.depth))
        })
    } else {
        state.brush_width / viewport.camera.zoom
    };
    if !width.is_finite() || width <= 0.0 || width > 2048.0 {
        return Err("This thickness exceeds the canvas stroke limit at this scale. Unlock thickness, reduce the brush size, or zoom out.".into());
    }
    // Pick the pointer's region automatically; the camera may be several regions
    // away from a screen edge when zoomed out. A visible starting point must not
    // exhaust the stroke's local-coordinate budget merely because of that offset.
    let start = coordinates::screen_to_point(viewport, screen).map_err(|error| error.to_string())?;
    let anchor =
        coordinates::target(viewport.camera.canvas, start, viewport.camera.region.depth, viewport.camera.zoom).map_err(|error| error.to_string())?.region;
    let mut draft =
        DraftStroke { viewport: viewport.clone(), anchor, points: Vec::new(), width, color: Srgba { rgba: state.color }, erase: state.erase, warning: None };
    append_draft(&mut draft, screen)?;
    Ok(draft)
}

pub(crate) fn append_draft(draft: &mut DraftStroke, screen: [f64; 2]) -> Result<(), String> {
    let point = coordinates::screen_to_point(&draft.viewport, screen)
        .and_then(|point| coordinates::local_at(&draft.anchor, point))
        .map_err(|error| error.to_string())?;
    if point.iter().any(|value| value.abs() > scene::MAX_STROKE_LOCAL) {
        return Err("This stroke reached its local extent. Release the mouse and start another stroke to continue.".into());
    }
    if draft.points.last().is_some_and(|last| ((last[0] - point[0]).hypot(last[1] - point[1]) * draft.viewport.camera.zoom) < 0.25) {
        return Ok(());
    }
    if draft.points.len() >= scene::MAX_POINTS {
        return Err("This stroke reached its sample limit. Release the mouse and start another stroke to continue.".into());
    }
    draft.points.push(point);
    Ok(())
}

fn commit(app: &mut PhotocraftApp, draft: &DraftStroke) -> bool {
    let accepted = run(
        app,
        "komaphone.canvas.stroke",
        json!({"canvas":draft.viewport.camera.canvas,"anchor":draft.anchor,"points":draft.points,"width":draft.width,"color":draft.color,"erase":draft.erase}),
    );
    if accepted && let Some(warning) = &draft.warning {
        set_error(app, warning.clone());
    }
    accepted
}

pub(crate) fn can_retry_retained(app: &PhotocraftApp) -> bool {
    !app.session.canvases.busy()
        && app
            .ui
            .canvas
            .unsaved_stroke
            .as_ref()
            .is_some_and(|draft| app.session.canvases.active.as_ref().is_some_and(|active| active.id == draft.viewport.camera.canvas))
}

pub(crate) fn retry_retained(app: &mut PhotocraftApp) -> bool {
    if !can_retry_retained(app) {
        return false;
    }
    let Some(draft) = app.ui.canvas.unsaved_stroke.clone() else { return false };
    if !commit(app, &draft) {
        return false;
    }
    app.ui.canvas.unsaved_stroke = None;
    app.ui.canvas.cancel_gesture_for_switch();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewport() -> Viewport {
        Viewport { camera: coordinates::home(CanvasId::new("11111111111111111111111111111111").unwrap()), width: 1024, height: 768 }
    }

    fn pointer_fixture() -> (PhotocraftApp, egui::Context, Rect) {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.ui.canvas.open = true;
        app.session.canvases.camera = Some(viewport().camera);
        let ctx = egui::Context::default();
        let rect = pointer_frame(&mut app, &ctx, Vec::new(), 0.0);
        (app, ctx, rect)
    }

    fn pointer_frame(app: &mut PhotocraftApp, ctx: &egui::Context, events: Vec<egui::Event>, time: f64) -> Rect {
        ctx.run_ui(
            egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1024.0, 768.0))), events, time: Some(time), ..Default::default() },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| drawing_area(app, ui));
            },
        )
        .textures_delta
        .clear();
        crate::komaphone_workspace::control_rect(ctx, "canvas.drawing").unwrap()
    }

    fn pointer_button(pos: Pos2, button: egui::PointerButton, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button, pressed, modifiers: egui::Modifiers::NONE }
    }

    fn screen_points(draft: &DraftStroke, rect: Rect) -> Vec<Pos2> {
        draft
            .points
            .iter()
            .map(|point| {
                let world = coordinates::point(&draft.anchor, *point).unwrap();
                let [x, y] = coordinates::point_to_screen(&draft.viewport, world).unwrap();
                rect.min + egui::vec2(x as f32, y as f32)
            })
            .collect()
    }

    #[test]
    fn coalesced_pointer_events_keep_the_curve_and_release_endpoint() {
        let (mut app, ctx, rect) = pointer_fixture();
        let start = rect.center();
        let a = start + egui::vec2(12.0, -25.0);
        let b = start + egui::vec2(25.0, -12.0);
        let release = start + egui::vec2(40.0, 10.0);
        // A complete stroke arrives between paints. The release has no preceding
        // move at its position, and the later hover must not enter the stroke.
        pointer_frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(start),
                pointer_button(start, egui::PointerButton::Primary, true),
                egui::Event::PointerMoved(a),
                egui::Event::PointerMoved(b),
                pointer_button(release, egui::PointerButton::Primary, false),
                egui::Event::PointerMoved(start),
            ],
            0.1,
        );
        assert!(app.ui.canvas.gesture.is_none());
        let draft = app.ui.canvas.unsaved_stroke.as_ref().unwrap();
        assert_eq!(screen_points(draft, rect), vec![start, a, b, release]);
        assert!(app.ui.canvas.message.is_some(), "An unavailable store must retain the complete failed stroke");
    }

    #[test]
    fn each_actual_move_survives_between_pointer_frames_without_duplicate_up() {
        let (mut app, ctx, rect) = pointer_fixture();
        let start = rect.center();
        pointer_frame(&mut app, &ctx, vec![pointer_button(start, egui::PointerButton::Primary, true)], 0.1);
        let positions = [start + egui::vec2(10.0, 10.0), start + egui::vec2(20.0, -10.0), start + egui::vec2(30.0, 10.0)];
        pointer_frame(&mut app, &ctx, positions.into_iter().map(egui::Event::PointerMoved).collect(), 0.2);
        let Some(Gesture::Draw(draft)) = &app.ui.canvas.gesture else { panic!("Missing active stroke") };
        assert_eq!(screen_points(draft, rect), [vec![start], positions.to_vec()].concat());
        pointer_frame(&mut app, &ctx, vec![pointer_button(positions[2], egui::PointerButton::Primary, false)], 0.3);
        assert_eq!(app.ui.canvas.unsaved_stroke.as_ref().unwrap().points.len(), 4);
    }

    #[test]
    fn stroke_start_uses_press_position_even_when_release_is_outside_canvas() {
        let (mut app, ctx, rect) = pointer_fixture();
        let start = rect.center();
        let outside = egui::pos2(rect.right() + 20.0, start.y);
        pointer_frame(
            &mut app,
            &ctx,
            vec![pointer_button(start, egui::PointerButton::Primary, true), pointer_button(outside, egui::PointerButton::Primary, false)],
            0.1,
        );
        assert_eq!(screen_points(app.ui.canvas.unsaved_stroke.as_ref().unwrap(), rect), vec![start, outside]);
        let (mut app, ctx, rect) = pointer_fixture();
        let outside = egui::pos2(rect.left() - 20.0, rect.center().y);
        pointer_frame(
            &mut app,
            &ctx,
            vec![
                pointer_button(outside, egui::PointerButton::Primary, true),
                egui::Event::PointerMoved(rect.center()),
                pointer_button(rect.center(), egui::PointerButton::Primary, false),
            ],
            0.1,
        );
        assert!(app.ui.canvas.gesture.is_none());
        assert!(app.ui.canvas.unsaved_stroke.is_none(), "Dragging in from outside is not a drawing start");
    }

    #[test]
    fn pointer_interruption_retains_prefix_and_does_not_bridge_reentry() {
        for interruption in [egui::Event::PointerGone, egui::Event::WindowFocused(false)] {
            let (mut app, ctx, rect) = pointer_fixture();
            let start = rect.center();
            let end = start + egui::vec2(30.0, -10.0);
            pointer_frame(&mut app, &ctx, vec![pointer_button(start, egui::PointerButton::Primary, true), egui::Event::PointerMoved(end), interruption], 0.1);
            assert!(app.ui.canvas.gesture.is_none());
            let retained = app.ui.canvas.unsaved_stroke.clone().unwrap();
            assert_eq!(screen_points(&retained, rect), vec![start, end]);
            pointer_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(start + egui::vec2(100.0, 100.0))], 0.2);
            assert_eq!(app.ui.canvas.unsaved_stroke.as_ref(), Some(&retained));
            assert!(app.ui.canvas.gesture.is_none());
        }
    }

    #[test]
    fn unrelated_button_release_does_not_finish_drawing_or_middle_pan() {
        for (held, other) in [(egui::PointerButton::Primary, egui::PointerButton::Middle), (egui::PointerButton::Middle, egui::PointerButton::Primary)] {
            let (mut app, ctx, rect) = pointer_fixture();
            let start = rect.center();
            pointer_frame(&mut app, &ctx, vec![pointer_button(start, held, true)], 0.1);
            pointer_frame(
                &mut app,
                &ctx,
                vec![pointer_button(start, other, true), pointer_button(start, other, false), egui::Event::PointerMoved(start + egui::vec2(20.0, 10.0))],
                0.2,
            );
            assert!(app.ui.canvas.gesture.is_some(), "{other:?} must not end {held:?}");
            pointer_frame(&mut app, &ctx, vec![pointer_button(start, held, false)], 0.3);
            assert!(app.ui.canvas.gesture.is_none());
            assert_eq!(app.ui.canvas.unsaved_stroke.is_some(), held == egui::PointerButton::Primary);
        }
    }

    #[test]
    fn space_pan_uses_key_state_at_press_instead_of_end_of_frame() {
        let space = |pressed| egui::Event::Key { key: egui::Key::Space, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE };
        let (mut app, ctx, rect) = pointer_fixture();
        pointer_frame(&mut app, &ctx, vec![space(true), pointer_button(rect.center(), egui::PointerButton::Primary, true), space(false)], 0.1);
        assert!(matches!(app.ui.canvas.gesture, Some(Gesture::Pan { button: egui::PointerButton::Primary, .. })));
        let (mut app, ctx, rect) = pointer_fixture();
        pointer_frame(&mut app, &ctx, vec![pointer_button(rect.center(), egui::PointerButton::Primary, true), space(true)], 0.1);
        assert!(matches!(app.ui.canvas.gesture, Some(Gesture::Draw(_))));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn coalesced_middle_pan_keeps_final_release_position_and_saves_camera() {
        use std::time::Duration;
        let temp = tempfile::tempdir().unwrap();
        let mut session = photocraft_engine::Session::new();
        session.enable_canvases(temp.path().join("canvas.sqlite3"), Default::default()).unwrap();
        session.flush_canvases(Duration::from_secs(15)).unwrap();
        session.execute("komaphone.canvas.create", json!({"name":"Pan fixture"})).unwrap();
        session.flush_canvases(Duration::from_secs(15)).unwrap();
        let mut app = PhotocraftApp::new(session, crate::Services::default());
        app.ui.canvas.open = true;
        let ctx = egui::Context::default();
        let rect = pointer_frame(&mut app, &ctx, Vec::new(), 0.0);
        let before = app.session.canvases.camera.clone().unwrap();
        let start = rect.center();
        let end = start + egui::vec2(140.0, 80.0);
        let mut events = vec![pointer_button(start, egui::PointerButton::Middle, true)];
        events.extend((1..=256).map(|step| egui::Event::PointerMoved(start + egui::vec2(step as f32 * 0.5, step as f32 * 0.25))));
        events.push(pointer_button(end, egui::PointerButton::Middle, false));
        pointer_frame(&mut app, &ctx, events, 0.1);
        assert_eq!(app.session.canvases.camera, Some(coordinates::pan(&before, [140.0, 80.0]).unwrap()));
        assert!(app.ui.canvas.gesture.is_none());
        assert!(app.ui.canvas.unsaved_stroke.is_none());
        assert!(app.ui.canvas.message.is_none());
        app.session.flush_canvases(Duration::from_secs(15)).unwrap();
        assert_eq!(app.session.canvases.active.as_ref().unwrap().stroke_count, 0);
        let camera = app.session.canvases.camera.clone().unwrap();
        app.session.execute("komaphone.canvas.open", json!({"canvas":camera.canvas})).unwrap();
        app.session.flush_canvases(Duration::from_secs(15)).unwrap();
        assert_eq!(app.session.canvases.camera, Some(camera));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn complete_coalesced_stroke_saves_every_ordered_sample_and_reopens_unchanged() {
        use std::time::Duration;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("canvas.sqlite3");
        let mut session = photocraft_engine::Session::new();
        session.enable_canvases(&path, Default::default()).unwrap();
        session.flush_canvases(Duration::from_secs(15)).unwrap();
        session.execute("komaphone.canvas.create", json!({"name":"Coalesced stroke fixture"})).unwrap();
        session.flush_canvases(Duration::from_secs(15)).unwrap();
        let mut app = PhotocraftApp::new(session, crate::Services::default());
        app.ui.canvas.open = true;
        let ctx = egui::Context::default();
        let rect = pointer_frame(&mut app, &ctx, Vec::new(), 0.0);
        let start = rect.center();
        let positions = [start, start + egui::vec2(10.0, -30.0), start + egui::vec2(25.0, 20.0), start + egui::vec2(45.0, -5.0)];
        // One actual egui frame, with a distinct mouse-up position and no
        // intervening paint/save calls between samples.
        pointer_frame(
            &mut app,
            &ctx,
            vec![
                pointer_button(positions[0], egui::PointerButton::Primary, true),
                egui::Event::PointerMoved(positions[1]),
                egui::Event::PointerMoved(positions[2]),
                pointer_button(positions[3], egui::PointerButton::Primary, false),
            ],
            0.1,
        );
        assert!(app.ui.canvas.gesture.is_none());
        assert!(app.ui.canvas.unsaved_stroke.is_none());
        assert!(app.ui.canvas.message.is_none());
        app.session.flush_canvases(Duration::from_secs(15)).unwrap();
        assert_eq!(app.session.canvases.active.as_ref().unwrap().stroke_count, 1);
        let saved = app.session.canvases.view.clone().unwrap();
        assert_eq!(saved.strokes.len(), 1);
        assert_eq!(saved.stats.full_strokes, 1);
        let stroke = &saved.strokes[0];
        let actual: Vec<_> = stroke
            .points
            .iter()
            .map(|point| {
                let world = coordinates::point(&stroke.anchor, *point).unwrap();
                let [x, y] = coordinates::point_to_screen(&saved.viewport, world).unwrap();
                rect.min + egui::vec2(x as f32, y as f32)
            })
            .collect();
        assert_eq!(actual.len(), positions.len());
        assert_eq!(stroke.anchor.depth, saved.viewport.camera.region.depth);
        // Authored coordinates are quantized to 1/1024 local units; conversion
        // back through f32 screen positions adds rounding at the viewport scale.
        let authored_pixel = saved.viewport.camera.zoom / scene::LOCAL_SUBPIXELS as f64;
        let screen_rounding = 4.0 * f64::from(f32::EPSILON) * f64::from(rect.max.x.abs().max(rect.max.y.abs()).max(1.0));
        let tolerance = authored_pixel + screen_rounding;
        for (index, (actual, expected)) in actual.iter().zip(positions).enumerate() {
            let error = f64::from((actual.x - expected.x).abs().max((actual.y - expected.y).abs()));
            assert!(error <= tolerance, "sample {index}: expected {expected:?}, actual {actual:?}, error {error}, bound {tolerance}");
        }
        let before = app.session.execute("komaphone.canvas.view", json!({})).unwrap();
        drop(app);

        let mut reopened = photocraft_engine::Session::new();
        reopened.enable_canvases(&path, Default::default()).unwrap();
        reopened.flush_canvases(Duration::from_secs(15)).unwrap();
        reopened.set_canvas_viewport(saved.viewport.width, saved.viewport.height);
        reopened.flush_canvases(Duration::from_secs(15)).unwrap();
        assert_eq!(reopened.canvases.view.as_ref().unwrap().strokes, saved.strokes);
        let after = reopened.execute("komaphone.canvas.view", json!({})).unwrap();
        assert_eq!(after["contentHash"], before["contentHash"]);
        assert_eq!(after["strokes"][0]["samples"], 4);
    }

    #[test]
    fn canvas_text_fields_are_named_at_standard_and_high_dpi() {
        use egui_kittest::kittest::Queryable;
        for scale in [1.0, 2.0] {
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
            let camera = viewport().camera;
            app.ui.canvas.open = true;
            app.session.canvases.active = Some(scene::CanvasInfo {
                id: camera.canvas,
                owner: scene::OwnerId::new("22222222222222222222222222222222").unwrap(),
                name: "Accessible canvas".into(),
                created_at: String::new(),
                updated_at: String::new(),
                revision: 0,
                camera,
                bookmarks: Vec::new(),
                stroke_count: 0,
                undo_count: 0,
                redo_count: 0,
            });
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(1280.0, 720.0))
                .with_pixels_per_point(scale)
                .with_max_steps(64)
                .build_ui_state(|ui, app| show(app, ui), app);
            // Harness::run retains the default accessibility audit. The original
            // unnamed fields failed that audit even though they rendered text.
            harness.run();
            for (label, value) in [("New canvas name", "My canvas"), ("Canvas name", "Accessible canvas"), ("Bookmark name", "A detail")] {
                assert_eq!(harness.get_by_role_and_label(egui::accesskit::Role::TextInput, label).value().as_deref(), Some(value), "scale {scale}: {label}");
            }
        }
    }

    #[test]
    fn brush_keeps_screen_width_when_starting_at_new_scale() {
        let state = CanvasUi::default();
        let mut view = viewport();
        for _ in 0..7 {
            let draft = begin_draft(&state, &view, [512.0, 384.0]).unwrap();
            assert!((draft.width * view.camera.zoom - state.brush_width).abs() < 1e-10);
            view.camera = coordinates::zoom_at(&view, [512.0, 384.0], 100.0).unwrap();
        }
    }

    #[test]
    fn a_stroke_crosses_internal_region_edges_in_one_gesture() {
        let view = viewport();
        let mut draft = begin_draft(&CanvasUi::default(), &view, [500.0, 384.0]).unwrap();
        append_draft(&mut draft, [530.0, 384.0]).unwrap();
        assert_eq!(draft.points.len(), 2);
        assert_eq!(draft.anchor.x.0, -1);
        assert!(draft.points.first().unwrap()[0] < 1024.0);
        assert!(draft.points.last().unwrap()[0] > 1024.0);
        assert_eq!(draft.viewport, view);
    }

    #[test]
    fn locked_width_preserves_canvas_thickness_and_refuses_excess() {
        let state = CanvasUi { lock_width: true, locked_width: Some(WidthLock { depth: 0, local: 5.0 }), ..Default::default() };
        let mut view = viewport();
        view.camera = coordinates::zoom_at(&view, [512.0, 384.0], 100.0).unwrap();
        assert_eq!(begin_draft(&state, &view, [512.0, 384.0]).unwrap().width, 500.0);
        view.camera = coordinates::zoom_at(&view, [512.0, 384.0], 100.0).unwrap();
        assert!(begin_draft(&state, &view, [512.0, 384.0]).is_err());
    }

    #[test]
    fn actual_egui_pointer_frames_keep_failed_stroke_and_do_not_edit_finite_document() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width":32,"height":32})).unwrap();
        let finite_revision = app.session.active().unwrap().revision;
        let view = viewport();
        app.ui.canvas.open = true;
        app.session.canvases.camera = Some(view.camera);
        let ctx = egui::Context::default();
        let paint = |app: &mut PhotocraftApp, events: Vec<egui::Event>, time: f64| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1024.0, 768.0))),
                    events,
                    time: Some(time),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| drawing_area(app, ui));
                },
            )
            .textures_delta
            .clear();
        };
        paint(&mut app, Vec::new(), 0.0);
        let rect = crate::komaphone_workspace::control_rect(&ctx, "canvas.drawing").unwrap();
        let start = rect.center();
        paint(
            &mut app,
            vec![
                egui::Event::PointerMoved(start),
                egui::Event::PointerButton { pos: start, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
            ],
            0.1,
        );
        assert!(matches!(&app.ui.canvas.gesture, Some(Gesture::Draw(draft)) if draft.points.len() == 1));
        let end = start + egui::vec2(100.0, 40.0);
        paint(&mut app, vec![egui::Event::PointerMoved(end)], 0.2);
        assert!(matches!(&app.ui.canvas.gesture, Some(Gesture::Draw(draft)) if draft.points.len() == 2));
        paint(
            &mut app,
            vec![egui::Event::PointerButton { pos: end, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }],
            0.3,
        );
        // This intentionally unconfigured store refuses persistence. The UI must retain
        // the complete unsaved gesture, rather than reporting or discarding a saved edit.
        assert!(app.ui.canvas.gesture.is_none());
        assert_eq!(app.ui.canvas.unsaved_stroke.as_ref().unwrap().points.len(), 2);
        assert!(app.ui.canvas.message.is_some());
        assert_eq!(app.session.active().unwrap().revision, finite_revision);
    }

    #[test]
    fn projection_failure_is_visible_and_preserves_the_scene() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let view = viewport();
        app.ui.canvas.open = true;
        app.session.canvases.camera = Some(view.camera.clone());
        let stroke = Stroke {
            id: scene::StrokeId::new("22222222222222222222222222222222").unwrap(),
            canvas: view.camera.canvas,
            owner: scene::OwnerId::new("33333333333333333333333333333333").unwrap(),
            revision: 1,
            anchor: view.camera.region,
            points: vec![[0.0, 0.0]],
            width: -1.0,
            color: Srgba { rgba: [0.1, 0.2, 0.3, 1.0] },
            erase: false,
            brush: None,
            samples: None,
        };
        app.session.canvases.view = Some(scene::SceneView { viewport: view, revision: 1, strokes: vec![stroke.clone()], stats: Default::default() });
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1024.0, 768.0))), ..Default::default() }, |ui| {
            egui::CentralPanel::default().show(ui, |ui| drawing_area(&mut app, ui));
        })
        .textures_delta
        .clear();
        let message = app.ui.canvas.message.as_deref().unwrap();
        assert!(message.contains("Cannot render") && message.contains("saved artwork remains unchanged"), "{message}");
        assert_eq!(app.session.canvases.view.as_ref().unwrap().strokes, vec![stroke]);
    }

    #[test]
    fn frame_samples_are_bounded_finite_monotonic_and_not_persisted() {
        let mut state = CanvasUi::default();
        for frame in 0..300 {
            state.record_frame(frame, 12.5, frame as f64 * 0.016);
        }
        assert_eq!(state.frame_samples.len(), MAX_FRAME_SAMPLES);
        assert_eq!(state.frame_samples.front().unwrap().frame, 60);
        for invalid in [f64::NAN, f64::INFINITY, -1.0] {
            state.record_frame(300, invalid, 4.8);
        }
        for invalid in [f64::NAN, f64::INFINITY, -1.0, 4.7, 299.0 * 0.016, f64::MAX] {
            state.record_frame(300, 1.0, invalid);
        }
        state.record_frame(299, 1.0, 4.8);
        assert_eq!(state.frame_samples.back().unwrap().frame, 299);
        state.record_frame(300, 1.0, 4.8);
        let sample = state.frame_samples.back().unwrap();
        assert_eq!(sample.frame, 300);
        assert!((sample.interval_ms.unwrap() - 16.0).abs() < 1e-9);
        let restored: CanvasUi = serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
        assert!(restored.frame_samples.is_empty());
        assert_eq!(restored.previous_frame_time, None);
    }

    #[test]
    fn selection_change_resets_frame_interval_history() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.ui.canvas.selected = Some(viewport().camera.canvas);
        app.ui.canvas.record_frame(1, 2.0, 1.0);
        assert_eq!(app.ui.canvas.frame_samples.back().unwrap().interval_ms, None);
        app.ui.canvas.record_frame(2, 2.0, 1.016);
        assert!(app.ui.canvas.frame_samples.back().unwrap().interval_ms.is_some());
        // Losing or switching the active canvas starts a fresh timing history.
        sync_active(&mut app);
        assert!(app.ui.canvas.frame_samples.is_empty());
        assert_eq!(app.ui.canvas.previous_frame_time, None);
        app.ui.canvas.record_frame(3, 2.0, 5.0);
        assert_eq!(app.ui.canvas.frame_samples.back().unwrap().interval_ms, None);
    }

    #[test]
    fn switching_retains_unfinished_drawings_and_cancels_pan() {
        let view = viewport();
        let draft = begin_draft(&CanvasUi::default(), &view, [500.0, 300.0]).unwrap();
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.ui.canvas.selected = Some(view.camera.canvas);
        app.ui.canvas.gesture = Some(Gesture::Draw(draft.clone()));
        sync_active(&mut app);
        assert_eq!(app.ui.canvas.unsaved_stroke.as_ref(), Some(&draft));
        assert!(app.ui.canvas.gesture.is_none());
        assert!(app.ui.canvas.message.as_deref().unwrap().contains("unfinished stroke is retained"));
        app.ui.canvas.cancel_gesture_for_switch();
        assert_eq!(app.ui.canvas.unsaved_stroke.as_ref(), Some(&draft));
        app.ui.canvas.gesture = Some(Gesture::Pan { camera: view.camera.clone(), start: [0.0; 2], button: egui::PointerButton::Primary });
        app.ui.canvas.cancel_gesture_for_switch();
        assert!(app.ui.canvas.gesture.is_none());
        assert_eq!(app.ui.canvas.unsaved_stroke.as_ref(), Some(&draft));

        // Both drafts survive even if an inconsistent state bypassed the normal
        // rule that drawing cannot start while a rejected draft awaits recovery.
        let second = begin_draft(&CanvasUi::default(), &view, [700.0, 400.0]).unwrap();
        app.ui.canvas.gesture = Some(Gesture::Draw(second.clone()));
        app.ui.canvas.cancel_gesture_for_switch();
        assert_eq!(app.ui.canvas.unsaved_stroke.as_ref(), Some(&draft));
        assert_eq!(app.ui.canvas.gesture, Some(Gesture::Draw(second.clone())));
        app.ui.canvas.unsaved_stroke = None;
        app.ui.canvas.cancel_gesture_for_switch();
        assert_eq!(app.ui.canvas.unsaved_stroke.as_ref(), Some(&second));
        assert!(app.ui.canvas.gesture.is_none());
    }

    #[test]
    fn preparing_close_keeps_refused_strokes_and_finishes_only_the_pointer_gesture() {
        let view = viewport();
        let first = begin_draft(&CanvasUi::default(), &view, [500.0, 300.0]).unwrap();
        let second = begin_draft(&CanvasUi::default(), &view, [700.0, 400.0]).unwrap();
        let mut state = CanvasUi { unsaved_stroke: Some(first.clone()), gesture: Some(Gesture::Draw(second.clone())), ..Default::default() };
        state.prepare_close();
        assert!(state.has_uncommitted_drawing());
        assert_eq!(state.unsaved_stroke.as_ref(), Some(&first));
        assert_eq!(state.gesture, Some(Gesture::Draw(second)));
        let before = state.drawing_snapshot();
        state.prepare_close();
        assert_eq!(state.drawing_snapshot(), before);
        state.gesture = Some(Gesture::Pan { camera: view.camera, start: [0.0; 2], button: egui::PointerButton::Primary });
        state.prepare_close();
        assert!(state.gesture.is_none());
        assert_eq!(state.unsaved_stroke.as_ref(), Some(&first));
    }

    #[test]
    fn maximum_viewport_corner_selects_its_region_automatically() {
        let mut view = viewport();
        view.width = scene::MAX_VIEW_SIDE;
        view.height = scene::MAX_VIEW_SIDE;
        let center = coordinates::point(&view.camera.region, [1023.0, 1023.0]).unwrap();
        view.camera = coordinates::target(view.camera.canvas, center, 0, 0.5).unwrap();
        let draft = begin_draft(&CanvasUi::default(), &view, [8191.0, 8191.0]).unwrap();
        let point = draft.points.first().unwrap();
        assert!(point.iter().all(|coordinate| (0.0..1024.0).contains(coordinate)));
        assert_ne!(draft.anchor, view.camera.region);
    }

    #[test]
    fn aggregate_projection_budget_rejects_before_painting_a_partial_stroke() {
        let view = viewport();
        let stroke = Stroke {
            id: scene::StrokeId::new("22222222222222222222222222222222").unwrap(),
            canvas: view.camera.canvas,
            owner: scene::OwnerId::new("33333333333333333333333333333333").unwrap(),
            revision: 1,
            anchor: view.camera.region,
            points: vec![[0.0, 0.0], [20.0, 10.0], [30.0, 40.0]],
            width: 5.0,
            color: Srgba { rgba: [0.1, 0.2, 0.3, 1.0] },
            erase: false,
            brush: None,
            samples: None,
        };
        let ctx = egui::Context::default();
        let mut counts = (1, scene::round::MAX_MESH_VERTICES - 1);
        let mut failure = None;
        ctx.run_ui(egui::RawInput::default(), |ui| {
            failure = paint_stroke(ui.painter(), Pos2::ZERO, &stroke, &view, &mut counts).err();
        })
        .textures_delta
        .clear();
        assert!(matches!(failure, Some(scene::Error::Limit(_))));
        assert_eq!(counts, (1, scene::round::MAX_MESH_VERTICES - 1));
    }

    #[test]
    fn two_dense_saved_curves_share_the_view_without_exhausting_mesh_capacity() {
        let view = viewport();
        let strokes: Vec<_> = [-100.0, 100.0]
            .into_iter()
            .enumerate()
            .map(|(index, offset)| Stroke {
                id: scene::StrokeId::new(&format!("{:032x}", index + 1)).unwrap(),
                canvas: view.camera.canvas,
                owner: scene::OwnerId::new("33333333333333333333333333333333").unwrap(),
                revision: index as u64 + 1,
                anchor: view.camera.region,
                points: (0..scene::MAX_POINTS)
                    .map(|sample| {
                        // Every tooth exceeds the display simplification error;
                        // this remains a mesh-capacity regression after smooth
                        // paths gain a bounded display-only simplification.
                        let x = -400.0 + sample as f64 / (scene::MAX_POINTS - 1) as f64 * 800.0;
                        let y = offset + if sample % 2 == 0 { -1.5 } else { 1.5 };
                        [x, y]
                    })
                    .collect(),
                width: 5.0,
                color: Srgba { rgba: [0.1, 0.2, 0.3, 1.0] },
                erase: false,
                brush: None,
                samples: None,
            })
            .collect();
        let source_counts: Vec<_> = strokes.iter().map(|stroke| stroke.points.len()).collect();
        assert_eq!(source_counts, vec![scene::MAX_POINTS; 2]);
        let ctx = egui::Context::default();
        let mut counts = (0, 0);
        ctx.run_ui(egui::RawInput::default(), |ui| {
            for stroke in &strokes {
                paint_stroke(ui.painter(), Pos2::ZERO, stroke, &view, &mut counts).unwrap();
            }
        })
        .textures_delta
        .clear();
        assert_eq!(counts.0, 2);
        assert!(counts.1 > scene::MAX_VIEW_POINTS, "Regression must exceed the old mistaken source-point-as-mesh budget");
        assert!(counts.1 <= scene::round::MAX_MESH_VERTICES);
        assert_eq!(strokes.iter().map(|stroke| stroke.points.len()).collect::<Vec<_>>(), source_counts);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn release_at_sample_limit_preserves_warning_after_committing_prefix() {
        use std::time::Duration;
        let temp = tempfile::tempdir().unwrap();
        let mut session = photocraft_engine::Session::new();
        session.enable_canvases(temp.path().join("canvas.sqlite3"), Default::default()).unwrap();
        session.flush_canvases(Duration::from_secs(15)).unwrap();
        session.execute("komaphone.canvas.create", json!({"name":"Limit fixture"})).unwrap();
        session.flush_canvases(Duration::from_secs(15)).unwrap();
        let mut app = PhotocraftApp::new(session, crate::Services::default());
        app.ui.canvas.open = true;
        let ctx = egui::Context::default();
        let paint = |app: &mut PhotocraftApp, events: Vec<egui::Event>, time: f64| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1024.0, 768.0))),
                    events,
                    time: Some(time),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| drawing_area(app, ui));
                },
            )
            .textures_delta
            .clear();
        };
        paint(&mut app, Vec::new(), 0.0);
        let start = crate::komaphone_workspace::control_rect(&ctx, "canvas.drawing").unwrap().center();
        paint(
            &mut app,
            vec![
                egui::Event::PointerMoved(start),
                egui::Event::PointerButton { pos: start, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
            ],
            0.1,
        );
        let Some(Gesture::Draw(draft)) = &mut app.ui.canvas.gesture else { panic!("No native pointer gesture") };
        draft.points.resize(scene::MAX_POINTS, *draft.points.first().unwrap());
        let end = start + egui::vec2(10.0, 10.0);
        paint(
            &mut app,
            vec![
                egui::Event::PointerMoved(end),
                egui::Event::PointerButton { pos: end, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE },
            ],
            0.2,
        );
        assert!(app.ui.canvas.gesture.is_none());
        assert!(app.ui.canvas.unsaved_stroke.is_none(), "Valid prefix was accepted for storage");
        let warning = app.ui.canvas.message.as_deref().unwrap();
        assert!(warning.contains("sample limit") && warning.contains("start another stroke"), "{warning}");
        app.session.flush_canvases(Duration::from_secs(15)).unwrap();
        assert_eq!(app.session.canvases.active.as_ref().unwrap().stroke_count, 1);
        assert!(app.session.canvases.error.is_none());
    }
}
