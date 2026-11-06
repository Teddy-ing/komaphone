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
    let key = egui::Id::unique(("komaphone-control", name));
    let frame = ui.ctx().cumulative_frame_nr();
    ui.ctx().data_mut(|data| {
        if ui.clip_rect().contains(response.rect.center()) {
            data.insert_temp(key, ControlRect { rect: response.rect.intersect(ui.clip_rect()), frame });
        } else {
            data.remove::<ControlRect>(key);
        }
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
        ui.add_space(12.0);
        ui.label(RichText::new("Your local drawings stay saved when the service is unavailable.").color(tokens.text_dim));
        ui.add_space(24.0);
        ui.label(RichText::new("An independent drawing game based on PhotoCraft.").color(tokens.text_dim));
    });
}

/// Draws curated chrome around the unmodified document canvas. Call once before the central
/// canvas panel. Shared dialogs, brush pickers and editor overlays still render afterward.
pub fn chrome(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    header(app, ui);
    if matches!(app.ui.tool, Tool::Brush | Tool::Pencil | Tool::Eraser) && app.ui.transform.is_none() {
        brush_options(app, ui);
    } else {
        panels::options_bar(app, ui);
    }
    footer(app, ui);
    tools(app, ui);
    sidebar(app, ui);
    drafts_window(app, ui.ctx());
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

fn header(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let tokens = Tokens::get(ui.ctx());
    let (caption_left, caption_right) = crate::titlebar::reserved(app, ui.ctx());
    egui::Panel::top("komaphone-header")
        .exact_size(crate::titlebar::bar_height(app, ui.ctx(), 42.0))
        .frame(egui::Frame::NONE.fill(tokens.chrome).inner_margin(egui::Margin::symmetric(10, 0)))
        .show(ui, |ui| {
            let full = ui.max_rect();
            ui.horizontal_centered(|ui| {
                ui.add_space(caption_left);
                ui.strong("Komaphone");
                ui.separator();
                let response = ui.button("Private room");
                remember(ui, "workspace.room", &response);
                if response.clicked() {
                    app.ui.rooms.open = true;
                }
                let response = ui.button("New drawing");
                remember(ui, "komaphone.project.new", &response);
                if response.clicked() {
                    project_command(app, "komaphone.project.new", json!({}));
                }
                let response = ui.button("Local drafts");
                remember(ui, "komaphone.project.list", &response);
                if response.clicked() {
                    app.ui.komaphone.drafts_open = true;
                    project_command(app, "komaphone.project.list", json!({}));
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
    percent_control(ui, value, None)
}

fn percent_control(ui: &mut egui::Ui, value: &mut f32, name: Option<&str>) -> bool {
    let mut percentage = *value * 100.0;
    let response = ui.add(egui::DragValue::new(&mut percentage).range(0.0..=100.0).speed(1.0).suffix("%"));
    if let Some(name) = name {
        remember(ui, name, &response);
    }
    let changed = response.changed();
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
    egui::Panel::right("komaphone-sidebar")
        .default_size(300.0)
        .min_size(280.0)
        .max_size(320.0)
        .frame(egui::Frame::NONE.fill(tokens.chrome).inner_margin(egui::Margin::same(10)))
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
    ui.strong("Layers");
    ui.horizontal_wrapped(|ui| {
        command(app, ui, "Add", "layer.new.layer", json!({"name":"Layer"}));
        command(app, ui, "Duplicate", "layer.duplicate", json!({"inPlace":true}));
        command(app, ui, "Delete", "layer.delete", json!({}));
    });
    let Some(state) = app.session.active() else {
        ui.label("Create or reopen a drawing to begin.");
        return;
    };
    let document = state.doc.clone();
    let active = state.active_layer;
    if let Some(layer) = active.and_then(|id| document.layer(id)) {
        ui.horizontal(|ui| {
            ui.label("Opacity");
            let mut opacity = layer.opacity;
            if percent_control(ui, &mut opacity, Some("layer.opacity")) {
                project_command(app, "layer.setProps", json!({"layer":layer.id.0,"opacity":opacity}));
            }
            let mut locked = layer.locks.any();
            let response = ui.checkbox(&mut locked, "Lock");
            remember(ui, "layer.lock", &response);
            if response.changed() {
                if !locked && crate::doc_props_ui::is_background(&document, layer) {
                    project_command(app, "layer.new.layerFromBackground", json!({}));
                    return;
                }
                let locks =
                    if locked { json!({"all":true}) } else { json!({"all":false,"pixels":false,"position":false,"transparency":false,"artboard":false}) };
                project_command(app, "layer.setProps", json!({"layer":layer.id.0,"locks":locks}));
            }
        });
        ui.horizontal(|ui| {
            command(app, ui, "Up", "layer.arrange.bringForward", json!({}));
            command(app, ui, "Down", "layer.arrange.sendBackward", json!({}));
            let response = ui.button("Rename");
            remember(ui, "layer.rename", &response);
            if response.clicked() {
                app.ui.komaphone.rename_layer = Some(layer.id.0);
                app.ui.komaphone.rename_document = Some(document.id.0);
                app.ui.komaphone.layer_name = layer.name.clone();
            }
        });
    }
    rename(app, ui);
    egui::ScrollArea::vertical().id_salt("komaphone-layers-scroll").auto_shrink([false, false]).show(ui, |ui| {
        for (depth, layer) in panels::layer_tree_ui::display_rows(&document, true) {
            ui.push_id(layer.id.0, |ui| {
                ui.horizontal(|ui| {
                    let mut visible = layer.visible;
                    let response = ui.checkbox(&mut visible, "").on_hover_text("Show layer");
                    remember(ui, &format!("layer.visible.{}", layer.id.0), &response);
                    if response.changed() {
                        project_command(app, "layer.setProps", json!({"layer":layer.id.0,"visible":visible}));
                    }
                    ui.add_space((depth.min(8) * 8) as f32);
                    let label = if layer.locks.any() { format!("{}  (locked)", layer.name) } else { layer.name.clone() };
                    let response = ui.add(egui::Button::selectable(active == Some(layer.id), label).truncate());
                    remember(ui, &format!("layer.select.{}", layer.id.0), &response);
                    if response.clicked() {
                        project_command(app, "layer.select", json!({"layer":layer.id.0}));
                    }
                    if response.double_clicked() {
                        app.ui.komaphone.rename_layer = Some(layer.id.0);
                        app.ui.komaphone.rename_document = Some(document.id.0);
                        app.ui.komaphone.layer_name = layer.name.clone();
                    }
                });
            });
        }
    });
}

fn rename(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let Some(layer) = app.ui.komaphone.rename_layer else { return };
    if app.session.active().map(|state| state.doc.id.0) != app.ui.komaphone.rename_document {
        app.ui.komaphone.rename_layer = None;
        app.ui.komaphone.rename_document = None;
        return;
    }
    let response = ui.add(egui::TextEdit::singleline(&mut app.ui.komaphone.layer_name).hint_text("Layer name").desired_width(ui.available_width()));
    remember(ui, "layer.rename.text", &response);
    ui.horizontal(|ui| {
        let apply = ui.button("Apply name");
        remember(ui, "layer.rename.apply", &apply);
        if apply.clicked() || (response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter))) {
            let name = app.ui.komaphone.layer_name.clone();
            if app.run("layer.setProps", json!({"layer":layer,"name":name})).is_ok() {
                app.ui.komaphone.rename_layer = None;
            }
        }
        if ui.button("Cancel").clicked() {
            app.ui.komaphone.rename_layer = None;
        }
    });
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
        || crate::tools::gradient_ui::editing(app)
}

fn footer(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let tokens = Tokens::get(ui.ctx());
    egui::Panel::bottom("komaphone-status").exact_size(46.0).frame(egui::Frame::NONE.fill(tokens.chrome).inner_margin(egui::Margin::symmetric(10, 2))).show(
        ui,
        |ui| {
            ui.horizontal(|ui| {
                if let Some(document) = app.session.active() {
                    let id = document.doc.id;
                    let status = local_save_label(app);
                    ui.add(egui::Label::new(&status).truncate()).on_hover_text(&status);
                    ui.separator();
                    ui.label(RichText::new(app.session.room_sync_label(id)).color(tokens.text_dim));
                    if (app.session.local_projects.status(id).error.is_some() || app.session.local_projects.error.is_some())
                        && ui.button("Retry save").clicked()
                    {
                        project_command(app, "komaphone.project.retry", json!({}));
                    }
                } else {
                    ui.label("Your local drawings stay on this computer.");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("Wheel: zoom · Space + drag: pan").color(tokens.text_dim));
                });
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
                            ui.small(format!("Revision {revision} · {date}"));
                        });
                    });
                    ui.separator();
                });
            }
        });
    });
    app.ui.komaphone.drafts_open = open;
}
