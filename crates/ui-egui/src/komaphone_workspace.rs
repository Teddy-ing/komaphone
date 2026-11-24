//! The compact Komaphone drawing shell. Every edit uses the existing editor command seam;
//! this module owns only presentation and transient view state.

use egui::{Rect, Response, RichText, Sense};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{PhotocraftApp, Tool, menus, panels, theme::Tokens};

/// Serializable view state, embedded in `UiState`. Native Komaphone enables the workspace;
/// reusable editor hosts retain their existing layout until they explicitly select it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceState {
    pub enabled: bool,
    pub drafts_open: bool,
    pub rename_layer: Option<u64>,
    pub rename_document: Option<u64>,
    pub layer_name: String,
    /// On-demand Layer Properties window for adjustment/fill/mask controls opened from the
    /// reused PhotoCraft Layers panel without expanding Komaphone into the full Advanced dock.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub properties_open: bool,
    /// Tracks an in-flight Save to gallery request started from the Workroom header so its
    /// confirmation or error surfaces in the Workroom status bar.
    #[serde(skip)]
    pub saving_to_gallery: bool,
}

/// Tools deliberately visible without a flyout or a keyboard shortcut.
pub const TOOLS: &[(Tool, &str)] = &[
    (Tool::Brush, "Brush"),
    (Tool::Pencil, "Pencil"),
    (Tool::Eraser, "Eraser"),
    (Tool::PaintBucket, "Fill"),
    (Tool::Eyedropper, "Pick color"),
    (Tool::RectMarquee, "Select"),
    (Tool::Lasso, "Lasso"),
    (Tool::Move, "Move"),
    (Tool::Rectangle, "Rectangle"),
    (Tool::EllipseShape, "Ellipse"),
    (Tool::Line, "Line"),
    (Tool::Type, "Text"),
    (Tool::Hand, "Pan"),
    (Tool::Zoom, "Zoom"),
];

#[derive(Clone, Copy)]
struct ControlRect {
    rect: Rect,
    frame: u64,
}

pub(crate) fn remember(ui: &egui::Ui, name: &str, response: &Response) {
    remember_rect(ui.ctx(), ui.clip_rect(), name, response.rect);
}

pub(crate) fn remember_rect(ctx: &egui::Context, clip: egui::Rect, name: &str, rect: egui::Rect) {
    let key = egui::Id::unique(("komaphone-control", name));
    let frame = ctx.cumulative_frame_nr();
    ctx.data_mut(|data| {
        if clip.contains(rect.center()) {
            data.insert_temp(key, ControlRect { rect: rect.intersect(clip), frame });
        } else {
            data.remove::<ControlRect>(key);
        }
    });
}

pub(crate) fn forget_rect(ctx: &egui::Context, name: &str) {
    let key = egui::Id::unique(("komaphone-control", name));
    ctx.data_mut(|data| {
        data.remove::<ControlRect>(key);
    });
}

/// Current or last completed frame's rectangle, useful for mouse-driven acceptance and
/// automation. Coordinates are points in the current egui viewport, never document pixels.
pub fn control_rect(ctx: &egui::Context, name: &str) -> Option<Rect> {
    let frame = ctx.cumulative_frame_nr();
    ctx.data(|data| data.get_temp::<ControlRect>(egui::Id::unique(("komaphone-control", name))))
        .filter(|control| control.frame == frame || control.frame.checked_add(1) == Some(frame))
        .map(|control| control.rect)
}

fn command(app: &mut PhotocraftApp, ui: &mut egui::Ui, label: &str, id: &str, params: Value) {
    let enabled = menus::is_enabled(app, id);
    let response = ui.add_enabled(enabled, egui::Button::new(label));
    remember(ui, id, &response);
    if response.clicked()
        && let Err(error) = menus::invoke(app, ui.ctx(), id, params)
    {
        app.ui.status = error;
    }
}

fn project_command(app: &mut PhotocraftApp, id: &str, params: Value) {
    if let Err(error) = app.run(id, params) {
        app.ui.status = error;
    }
}

/// Empty drawing workspace; upstream credits remain in the editor's About window.
pub fn home(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let tokens = Tokens::get(ui.ctx());
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.28).max(16.0));
        ui.heading("Komaphone");
        ui.label("Draw with one or two friends.");
        ui.add_space(16.0);
        let response = ui.button("Private room");
        remember(ui, "home.room", &response);
        if response.clicked() {
            app.ui.rooms.open = true;
        }
        let response = ui.button("New drawing");
        remember(ui, "home.new", &response);
        if response.clicked() {
            project_command(app, "komaphone.project.new", json!({}));
        }
        let response = ui.button("Gallery");
        remember(ui, "home.gallery", &response);
        if response.clicked() {
            crate::komaphone_gallery::open(app);
        }
        let response = ui.button("Canvases");
        remember(ui, "home.canvases", &response);
        if response.clicked() {
            crate::komaphone_canvas::open(app);
        }
        let response = ui.button("Shared canvases");
        remember(ui, "home.worlds", &response);
        if response.clicked() {
            crate::komaphone_world::open(app);
        }
        ui.add_space(12.0);
        ui.label(RichText::new("Your local drawings stay saved when the service is unavailable.").color(tokens.text_dim));
        ui.add_space(24.0);
        ui.label(RichText::new("An independent drawing game based on PhotoCraft.").color(tokens.text_dim));
    });
}

/// Draws curated chrome around the unmodified document canvas. Call once before the central
/// canvas panel. Shared dialogs, brush pickers and editor overlays still render afterward.
pub fn chrome(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    if ui.ctx().data(|data| data.get_temp::<Tokens>(egui::Id::unique("photocraft-theme"))).is_none() {
        PhotocraftApp::setup_context(ui.ctx(), app.ui.theme);
    }
    header(app, ui);
    if app.ui.gallery.open {
        crate::komaphone_rooms::window(app, ui.ctx());
        return;
    }
    if matches!(app.ui.tool, Tool::Brush | Tool::Pencil | Tool::Eraser) && app.ui.transform.is_none() {
        brush_options(app, ui);
    } else {
        panels::options_bar(app, ui);
    }
    footer(app, ui);
    tools(app, ui);
    sidebar(app, ui);
    drafts_window(app, ui.ctx());
    properties_window(app, ui.ctx());
    crate::komaphone_rooms::window(app, ui.ctx());
}

/// A discoverable route back from the unchanged Advanced editor.
pub fn advanced_return(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let tokens = Tokens::get(ui.ctx());
    egui::Panel::top("komaphone-return").exact_size(28.0).frame(egui::Frame::NONE.fill(tokens.chrome).inner_margin(egui::Margin::symmetric(10, 2))).show(
        ui,
        |ui| {
            ui.horizontal(|ui| {
                let response = ui.button("Return to Komaphone drawing");
                remember(ui, "workspace.drawing", &response);
                if response.clicked() {
                    app.ui.komaphone.enabled = true;
                }
                ui.label(RichText::new("Advanced workspace").color(tokens.text_dim));
            });
        },
    );
}

/// Explicit dimension label for deep-zoom continuous canvases that have no fixed pixel bounds.
pub(crate) const UNBOUNDED_CANVAS_LABEL: &str = "Unbounded canvas";

/// Accurate scale readout for deep-zoom continuous canvases, retaining exact depth semantics
/// (`100^depth * zoom`) without inventing finite full-world dimensions or misleading percentages.
pub(crate) fn deep_canvas_scale_label(camera: &photocraft_engine::scene::Camera) -> String {
    let scale = camera.zoom * 100.0_f64.powi(i32::from(camera.region.depth));
    if !scale.is_finite() || scale <= 0.0 {
        return "Scale unavailable".into();
    }
    if scale < 100_000.0 { format!("Zoom {scale:.2}×") } else { format!("Zoom {scale:.1e}×") }
}

/// Return directly to the Komaphone drawing workroom from Gallery, Canvases, Shared canvases,
/// Party/Private room, or Reveal without creating a new draft, altering open tabs/selection/unsaved
/// work, or changing phase/assignment restrictions. Refuses cleanly while a continuous-canvas
/// stroke is still uncommitted.
pub fn open_workroom(app: &mut PhotocraftApp) -> bool {
    if app.ui.world.open && app.ui.world.unfinished() {
        app.ui.world.message = Some("Save or retry the unfinished stroke before returning to the workroom.".into());
        return false;
    }
    if app.ui.canvas.open && app.ui.canvas.has_uncommitted_drawing() {
        app.ui.canvas.message = Some("Save the unfinished local stroke before returning to the workroom.".into());
        return false;
    }
    if app.ui.canvas.open && app.session.canvases.flight.is_some() {
        let _ = app.run("komaphone.canvas.flight", json!({"state":"cancel"}));
    }
    app.ui.world.open = false;
    app.ui.canvas.open = false;
    app.ui.gallery.open = false;
    app.ui.rooms.open = false;
    app.ui.rooms.view_reveal = false;
    app.ui.komaphone.enabled = true;
    true
}

/// Readiness check for the Workroom header's explicit Save to gallery action.
pub fn workroom_save_to_gallery_state(app: &PhotocraftApp) -> Result<&'static str, &'static str> {
    if !app.session.gallery.available() {
        return Err("Gallery storage is unavailable");
    }
    if app.session.gallery.busy() {
        return Err("Gallery is busy; wait a moment");
    }
    let Some(doc) = app.session.active() else {
        return Err("Open or create a drawing first");
    };
    if app.session.local_replay.excluded_reason(doc.doc.id).is_some() {
        return Err("External documents use File > Save instead of Gallery");
    }
    if app.session.is_active_party_document(doc.doc.id) {
        return Err("Party room drawings save together when the game reveals");
    }
    if editing_in_progress(app) || !app.session.local_document_saved(doc.doc.id) {
        return Err("Wait for local saving to finish before saving to gallery");
    }
    let has_saved = app
        .session
        .local_projects
        .status(doc.doc.id)
        .project
        .is_some_and(|project| app.session.gallery.items.iter().any(|item| item.id == project && !item.draft && !item.trashed));
    if has_saved { Ok("Update the saved copy of this drawing in Gallery > Saved") } else { Ok("Save this drawing to Gallery > Saved") }
}

fn header(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    if app.ui.komaphone.saving_to_gallery && !app.session.gallery.busy() {
        app.ui.komaphone.saving_to_gallery = false;
        if let Some(error) = &app.session.gallery.error {
            app.ui.status = error.clone();
            app.ui.status_error = true;
        } else if let Some(notice) = &app.session.gallery.notice {
            app.ui.status = notice.clone();
            app.ui.status_error = false;
        }
    }
    let tokens = Tokens::get(ui.ctx());
    let (caption_left, caption_right) = crate::titlebar::reserved(app, ui.ctx());
    egui::Panel::top("komaphone-header")
        .exact_size(crate::titlebar::bar_height(app, ui.ctx(), 42.0))
        .frame(egui::Frame::NONE.fill(tokens.chrome).inner_margin(egui::Margin::symmetric(10, 0)))
        .show(ui, |ui| {
            let full = ui.max_rect();
            crate::zoom_field::set_title_bar_rect(ui.ctx(), full);
            ui.horizontal_centered(|ui| {
                ui.add_space(caption_left);
                ui.strong("Komaphone");
                ui.separator();
                let in_workroom = !app.ui.gallery.open && !crate::komaphone_game_view::has_reveal(app);
                let response = ui.add(egui::Button::new("Workroom").selected(in_workroom)).on_hover_text("Return to your drawing workroom with open drawings");
                remember(ui, "workspace.workroom", &response);
                if response.clicked() {
                    open_workroom(app);
                }
                let response = ui.add(egui::Button::new("Private room").selected(app.ui.rooms.open));
                remember(ui, "workspace.room", &response);
                if response.clicked() {
                    app.ui.rooms.open = true;
                }
                let response = ui.add(egui::Button::new("Gallery").selected(app.ui.gallery.open));
                remember(ui, "workspace.gallery", &response);
                if response.clicked() {
                    crate::komaphone_gallery::open(app);
                }
                let response = ui.button("Canvases");
                remember(ui, "workspace.canvases", &response);
                if response.clicked() {
                    crate::komaphone_canvas::open(app);
                }
                let response = ui.button("Shared canvases");
                remember(ui, "workspace.worlds", &response);
                if response.clicked() {
                    crate::komaphone_world::open(app);
                }
                ui.separator();
                let response = ui.button("New drawing");
                remember(ui, "komaphone.project.new", &response);
                if response.clicked() {
                    match app.run("komaphone.project.new", json!({})) {
                        Ok(_) => {
                            open_workroom(app);
                        }
                        Err(error) => app.ui.status = error,
                    }
                }
                if in_workroom {
                    let save_state = workroom_save_to_gallery_state(app);
                    let (enabled, tooltip) = match save_state {
                        Ok(tip) => (true, tip),
                        Err(reason) => (false, reason),
                    };
                    let response = ui.add_enabled(enabled, egui::Button::new("Save to gallery")).on_hover_text(tooltip).on_disabled_hover_text(tooltip);
                    remember(ui, "workspace.saveToGallery", &response);
                    if response.clicked() {
                        match app.run("komaphone.gallery.save", json!({})) {
                            Ok(_) => {
                                app.ui.komaphone.saving_to_gallery = true;
                                app.ui.status = "Saving to gallery…".into();
                                app.ui.status_error = false;
                            }
                            Err(error) => {
                                app.ui.status = error;
                                app.ui.status_error = true;
                            }
                        }
                    }
                }
                command(app, ui, "Import image", "file.placeEmbedded", json!({}));
                ui.separator();
                command(app, ui, "Undo", "edit.undo", json!({}));
                command(app, ui, "Redo", "edit.redo", json!({}));
                let response = ui.button("Advanced").on_hover_text("Open the full editor without changing your drawing");
                remember(ui, "workspace.advanced", &response);
                if response.clicked() {
                    app.ui.komaphone.enabled = false;
                }
                let remaining = (ui.available_width() - caption_right - 6.0).max(0.0);
                let (rect, response) = ui.allocate_exact_size(egui::vec2(remaining, 32.0), Sense::click_and_drag());
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
}

fn brush_options(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let tokens = Tokens::get(ui.ctx());
    egui::Panel::top("komaphone-brush-options")
        .exact_size(40.0)
        .frame(egui::Frame::NONE.fill(tokens.chrome).inner_margin(egui::Margin::symmetric(10, 0)))
        .show(ui, |ui| {
            egui::ScrollArea::horizontal().id_salt("komaphone-brush-scroll").auto_shrink([false, false]).show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    crate::paint_mouse::sync_tool_brush(app);
                    let before = app.session.tools.brush.clone();
                    let mut brush = before.clone();
                    ui.strong(TOOLS.iter().find(|(tool, _)| *tool == app.ui.tool).map_or("Brush", |(_, label)| *label));
                    ui.separator();
                    ui.label("Size");
                    ui.add(egui::DragValue::new(&mut brush.size).range(1.0..=photocraft_paint::MAX_BRUSH_SIZE).speed(1.0).suffix(" px"));
                    ui.label("Opacity");
                    percent(ui, &mut brush.opacity);
                    if app.ui.tool != Tool::Pencil && matches!(brush.tip, photocraft_paint::TipShape::Round) {
                        ui.label("Hardness");
                        percent(ui, &mut brush.hardness);
                    }
                    ui.label("Smoothing");
                    let mut amount = if brush.smoothing.enabled { brush.smoothing.amount } else { 0.0 };
                    if percent(ui, &mut amount) {
                        brush.smoothing.enabled = amount > 0.0;
                        brush.smoothing.amount = amount;
                    }
                    let response = ui.button("Brush presets");
                    remember(ui, "brush.presets", &response);
                    if response.clicked() {
                        crate::brush_picker::open(&mut app.ui, ui.ctx(), [response.rect.left(), response.rect.bottom()]);
                    }
                    crate::brush_panel::commit_gesture(app, ui.ctx(), &before, &brush);
                });
            });
        });
}

fn percent(ui: &mut egui::Ui, value: &mut f32) -> bool {
    let mut percentage = *value * 100.0;
    let changed = ui.add(egui::DragValue::new(&mut percentage).range(0.0..=100.0).speed(1.0).suffix("%")).changed();
    if changed {
        *value = percentage / 100.0;
    }
    changed
}

fn tools(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let tokens = Tokens::get(ui.ctx());
    egui::Panel::left("komaphone-tools")
        .exact_size(108.0)
        .resizable(false)
        .frame(egui::Frame::NONE.fill(tokens.chrome).inner_margin(egui::Margin::symmetric(6, 8)))
        .show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("komaphone-tools-scroll").show(ui, |ui| {
                // Keep all tools and view actions visible at the 1280×720 acceptance size.
                // Preserve the 28-point mouse targets while tightening only the gaps.
                ui.spacing_mut().item_spacing.y = 3.0;
                for &(tool, label) in TOOLS {
                    let response = ui.add_sized([94.0, 28.0], egui::Button::selectable(app.ui.tool == tool, label)).on_hover_text(tool.label());
                    remember(ui, &format!("tool.{label}"), &response);
                    if response.clicked() {
                        crate::tool_memory::select(app, tool);
                    }
                }
                ui.separator();
                command(app, ui, "Transform", "edit.freeTransform", json!({}));
                command(app, ui, "Deselect", "select.deselect", json!({}));
                command(app, ui, "Fit view", "view.fitOnScreen", json!({}));
                command(app, ui, "100%", "view.actualPixels", json!({}));
            });
        });
}

fn sidebar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let tokens = Tokens::get(ui.ctx());
    let margin = crate::widgets::body_margin(tokens.pro);
    egui::Panel::right("komaphone-sidebar")
        .default_size(300.0)
        .min_size(260.0)
        .max_size(360.0)
        .frame(egui::Frame::NONE.fill(tokens.chrome).inner_margin(margin))
        .show(ui, |ui| {
            egui::CollapsingHeader::new("Colors").default_open(true).show(ui, |ui| {
                ui.data_mut(|data| data.insert_temp(panels::color_field_fill_id(), 160.0));
                panels::color_field(app, ui);
                ui.collapsing("Palette", |ui| panels::swatches_ui::panel(app, ui));
            });
            ui.separator();
            layers(app, ui);
        });
}

fn layers(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let tokens = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.strong(tl!("Layers"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let menu_btn = crate::widgets::panel_menu_button(ui);
            remember(ui, "layers.menu", &menu_btn);
            egui::Popup::menu(&menu_btn).show(|ui| {
                crate::widgets::style_spectrum_popup_menu(ui);
                ui.set_min_width(190.0);
                panels::layer_row_ui::panel_menu(app, ui);
            });
        });
    });
    if let Some(reason) = app.session.room_active_edit_refusal() {
        egui::Frame::NONE.fill(tokens.field).corner_radius(tokens.radius_sm).inner_margin(egui::Margin::symmetric(8, 5)).show(ui, |ui| {
            ui.label(RichText::new(reason).small().color(tokens.text_dim));
        });
        ui.add_space(4.0);
    }
    if app.session.active().is_none() {
        ui.label(RichText::new("Create or reopen a drawing to begin.").color(tokens.text_dim));
        return;
    }
    ui.scope(|ui| {
        // Match PhotoCraft's dock body spacing so layer rows, Blend/Opacity/Fill and the footer
        // lay out identically in both Pro and Studio themes.
        ui.spacing_mut().item_spacing.y = if tokens.pro { 0.0 } else { 6.0 };
        panels::layers(app, ui);
    });
}

fn properties_window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.ui.komaphone.properties_open {
        return;
    }
    if app.session.active().is_none() {
        app.ui.komaphone.properties_open = false;
        return;
    }
    let mut open = true;
    let content = ctx.content_rect();
    let default_pos = egui::pos2((content.right() - 600.0).max(120.0), (content.top() + 92.0).max(56.0));
    egui::Window::new(tl!("Layer Properties"))
        .id(egui::Id::unique("komaphone-layer-properties"))
        .open(&mut open)
        .default_pos(default_pos)
        .default_width(280.0)
        .min_width(240.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height(460.0).show(ui, |ui| {
                panels::properties_body(app, ui);
            });
        });
    if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        open = false;
    }
    app.ui.komaphone.properties_open = open;
}

/// A local acknowledgment describes completed edits, never an in-progress preview. Keeping
/// this presentation predicate separate also lets input tests assert the visible guarantee.
pub fn local_save_label(app: &PhotocraftApp) -> String {
    let Some(document) = app.session.active() else { return "Your local drawings stay on this computer.".into() };
    if let Some(reason) = app.session.local_replay.excluded_reason(document.doc.id) {
        return format!("External document · use File > Save: {reason}");
    }
    if app.session.local_projects.status(document.doc.id).error.is_some() || app.session.local_projects.error.is_some() {
        return app.session.local_projects.status_text(document.doc.id);
    }
    if editing_in_progress(app) {
        "Drawing · Unsaved".into()
    } else if app.session.local_replay.pending_for(document.doc.id) > 0 {
        "Unsaved · waiting for local storage".into()
    } else {
        app.session.local_projects.status_text(document.doc.id)
    }
}

/// Previews have not necessarily entered the engine yet, so a preceding save acknowledgment
/// cannot certify them. The close guard uses the same rule as the visible save status.
pub(crate) fn editing_in_progress(app: &PhotocraftApp) -> bool {
    app.drag.as_ref().is_some_and(|drag| !matches!(drag.tool, Tool::Hand | Tool::Zoom | Tool::RotateView | Tool::Eyedropper | Tool::ColorSampler | Tool::Ruler))
        || app.live_stroke.is_some()
        || app.ui.text_edit.is_some()
        || app.ui.transform.is_some()
        || app.ui.pen.is_some()
        || app.ui.rooms.corpse_view.height_drag.is_some()
        || crate::tools::gradient_ui::editing(app)
}

fn footer(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let tokens = Tokens::get(ui.ctx());
    egui::Panel::bottom("komaphone-status").exact_size(46.0).frame(egui::Frame::NONE.fill(tokens.chrome).inner_margin(egui::Margin::symmetric(10, 2))).show(
        ui,
        |ui| {
            ui.horizontal(|ui| {
                let show_doc_metrics = !crate::komaphone_game_view::has_reveal(app);
                if let Some((id, dims)) = app
                    .session
                    .active()
                    .map(|document| (document.doc.id, crate::chrome_ui::document_dimensions_text(&document.doc, &app.session.prefs().units_and_rulers)))
                {
                    if show_doc_metrics {
                        if let Some(field) = crate::chrome_ui::active_zoom_field(app, ui, 64.0) {
                            remember(ui, "workspace.zoom", &field);
                        }
                        ui.add_space(6.0);
                        let dim_text = tl!(&dims);
                        let dim_resp = ui.add(egui::Label::new(RichText::new(dim_text).color(tokens.text_dim).size(12.0)).truncate()).on_hover_text(dim_text);
                        remember(ui, "workspace.dimensions", &dim_resp);
                        ui.separator();
                    }
                    let status = local_save_label(app);
                    let sync_label = app.session.room_sync_label(id);
                    let has_retry = app.session.local_projects.status(id).error.is_some() || app.session.local_projects.error.is_some();
                    let hint = "Wheel: zoom · Space + drag: pan";
                    let hint_reserve = 220.0;
                    // Keep narrow layouts and high DPI free of overlap by bounding left status text
                    // before placing the right-aligned navigation hint.
                    let show_hint = ui.available_width() >= hint_reserve + 180.0;
                    let left_budget = if show_hint { (ui.available_width() - hint_reserve).max(60.0) } else { ui.available_width().max(60.0) };
                    ui.allocate_ui_with_layout(egui::vec2(left_budget, ui.spacing().interact_size.y), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(egui::Label::new(&status).truncate()).on_hover_text(&status);
                        if has_retry && ui.button("Retry save").clicked() {
                            project_command(app, "komaphone.project.retry", json!({}));
                        }
                        if ui.available_width() >= 70.0 {
                            ui.separator();
                            ui.add(egui::Label::new(RichText::new(sync_label).color(tokens.text_dim)).truncate()).on_hover_text(sync_label);
                        }
                    });
                    if show_hint {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new(hint).color(tokens.text_dim));
                        });
                    }
                } else {
                    ui.add(egui::Label::new("Your local drawings stay on this computer.").truncate());
                    if ui.available_width() >= 220.0 {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(RichText::new("Wheel: zoom · Space + drag: pan").color(tokens.text_dim));
                        });
                    }
                }
            });
            if !app.ui.status.is_empty() {
                ui.add(egui::Label::new(RichText::new(&app.ui.status).small().color(tokens.text_dim)).truncate()).on_hover_text(&app.ui.status);
            }
        },
    );
}

fn drafts_window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.ui.komaphone.drafts_open {
        return;
    }
    let mut open = true;
    egui::Window::new("Local drafts").id(egui::Id::unique("komaphone-drafts")).open(&mut open).default_width(500.0).show(ctx, |ui| {
        ui.label("Reopen the last confirmed local revision. Originals and replay history are retained.");
        if let Some(usage) = &app.session.local_projects.usage {
            ui.label(format!("Storage: {:.1} MiB of {:.0} MiB", usage.stored_bytes as f64 / 1_048_576.0, usage.quota_bytes as f64 / 1_048_576.0));
        }
        ui.small("Drafts and unsynced edits are never deleted automatically.");
        if !app.session.local_projects.available() {
            ui.label("Local storage is unavailable. Your drawing is not yet saved locally.");
        }
        if let Some(error) = &app.session.local_projects.error {
            ui.label(error);
        }
        if let Some(notice) = &app.session.local_projects.notice {
            ui.label(notice);
        }
        if ui.button("Refresh drafts").clicked() {
            project_command(app, "komaphone.project.list", json!({}));
        }
        if app.session.local_projects.loading() {
            ui.spinner();
        }
        let drafts: Vec<_> = app
            .session
            .local_projects
            .drafts
            .iter()
            .map(|draft| (draft.project.as_str().to_owned(), draft.title.clone(), draft.revision, draft.updated_at.clone()))
            .collect();
        if drafts.is_empty() && !app.session.local_projects.loading() {
            ui.label("No saved drafts yet. Start a new drawing to create one.");
        }
        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            for (project, title, revision, date) in drafts {
                ui.push_id(&project, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("Open").clicked() {
                            project_command(app, "komaphone.project.open", json!({"project":project}));
                        }
                        ui.vertical(|ui| {
                            ui.strong(title);
                            let formatted = crate::komaphone_time::format_compact(&date);
                            let response = ui.small(format!("Revision {revision} · {}", formatted.display));
                            if let Some(tooltip) = &formatted.tooltip {
                                response.on_hover_text(tooltip);
                            }
                        });
                    });
                    ui.separator();
                });
            }
        });
    });
    app.ui.komaphone.drafts_open = open;
}
