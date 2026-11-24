//! Read-only creature references, per-segment bottom-edge height resize UI, and variable-height
//! sequential reveals. Artwork remains in the engine/service; this module paints granted textures
//! and keeps only viewer navigation and transient drag/input state.
use crate::{canvas::ViewXform, theme::Tokens};
use egui::{Align2, Color32, CursorIcon, FontId, Key, Mesh, Pos2, Rect, Sense, Stroke, StrokeKind, TextureId, Vec2, pos2, vec2};
use photocraft_engine::doc::{DocId, Document};
use photocraft_engine::party::room::RoomView;
use photocraft_engine::party::{AssetHash, GameId};
use photocraft_engine::rooms::corpse::{self, BAND_HEIGHT, MAX_BAND_HEIGHT, MIN_BAND_HEIGHT, SEAM_HEIGHT, WIDTH};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorpseHeightDrag {
    pub doc_id: DocId,
    pub start_height: u32,
    pub safe_min: u32,
    pub preview_height: u32,
    pub grab_offset_doc_y_tenths: i32,
    pub clamped_by_artwork: bool,
    pub clamped_by_limit: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorpseHeightAction {
    Apply { height: u32, allow_crop: bool },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CorpseUi {
    pub game: Option<GameId>,
    pub chain: u8,
    /// Viewer-only mask. It never modifies or exports an artwork buffer.
    pub visible_parts: u8,
    /// Active bottom-edge resize drag preview (never persisted or serialized).
    #[serde(skip)]
    pub height_drag: Option<CorpseHeightDrag>,
    /// Numeric height input in the left reference panel.
    #[serde(skip)]
    pub draft_height: u32,
    /// Last synchronized `(DocId, height)` for `draft_height`.
    #[serde(skip)]
    pub draft_height_sync: Option<(DocId, u32)>,
    /// Target height awaiting explicit user crop confirmation when shrinking past artwork.
    #[serde(skip)]
    pub pending_crop_height: Option<u32>,
    /// Contextual explanation when a drag hits the artwork bound or min/max limits.
    #[serde(skip)]
    pub height_notice: Option<String>,
}
impl Default for CorpseUi {
    fn default() -> Self {
        Self {
            game: None,
            chain: 0,
            visible_parts: 1,
            height_drag: None,
            draft_height: BAND_HEIGHT,
            draft_height_sync: None,
            pending_crop_height: None,
            height_notice: None,
        }
    }
}
impl CorpseUi {
    /// Switch to another creature chain, restarting sequential reveal at the head.
    pub fn select_chain(&mut self, chain: u8) {
        if self.chain != chain {
            self.chain = chain;
            self.visible_parts = 1;
        }
    }
    pub fn cancel_height_drag(&mut self) {
        self.height_drag = None;
    }
}

pub fn seam_caption(ui: &mut egui::Ui, round: u8, missing: bool, current_height: u32) {
    ui.strong(format!("Draw the {}", corpse::part_name(round).to_lowercase()));
    if round == 0 {
        ui.label("Draw down to the bottom edge so the next player can connect the torso.");
    } else if missing {
        ui.label("The preceding part was not submitted. Draw your part independently; the reveal will label the missing part.");
    } else {
        ui.label(format!("Match the locked 24 px strip above your drawing. Only your {WIDTH} × {current_height} segment is submitted."));
        if round == 1 {
            ui.label("Draw down to the bottom edge so the next player can connect the legs.");
        }
    }
}

/// Accessible numeric segment height control in the left reference panel, with explicit
/// undoable confirmation whenever shrinking would remove existing artwork.
pub fn segment_height_controls(
    ui: &mut egui::Ui,
    state: &mut CorpseUi,
    doc: &Document,
    enabled: bool,
    disabled_reason: Option<&str>,
) -> Option<CorpseHeightAction> {
    let current = doc.size.height;
    let sync_key = (doc.id, current);
    if state.draft_height_sync != Some(sync_key) {
        state.draft_height = current.clamp(MIN_BAND_HEIGHT, MAX_BAND_HEIGHT);
        state.draft_height_sync = Some(sync_key);
        state.pending_crop_height = None;
    }
    if !enabled {
        state.pending_crop_height = None;
    }

    ui.separator();
    ui.strong("Segment height");
    ui.weak(format!("{WIDTH} × {current} px · width fixed"));

    let mut requested: Option<u32> = None;
    let tip = disabled_reason.unwrap_or("Resize only your current segment height (128–2048 px). Top seam stays anchored.");
    ui.horizontal(|ui| {
        ui.label("Height");
        let input = ui
            .add_enabled(enabled, egui::DragValue::new(&mut state.draft_height).range(MIN_BAND_HEIGHT..=MAX_BAND_HEIGHT).speed(2.0).suffix(" px"))
            .on_hover_text(tip)
            .on_disabled_hover_text(tip);
        crate::komaphone_workspace::remember(ui, "corpse.height.input", &input);
        let enter_apply = enabled && input.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        let apply = ui.add_enabled(enabled && state.draft_height != current, egui::Button::new("Apply")).on_hover_text(tip).on_disabled_hover_text(tip);
        crate::komaphone_workspace::remember(ui, "corpse.height.apply", &apply);
        if (apply.clicked() || enter_apply) && state.draft_height != current {
            requested = Some(state.draft_height);
        }
    });

    let reset = ui
        .add_enabled(enabled && current != BAND_HEIGHT, egui::Button::new(format!("Default ({BAND_HEIGHT} px)")))
        .on_hover_text("Return this segment to the default 512 px height")
        .on_disabled_hover_text(tip);
    crate::komaphone_workspace::remember(ui, "corpse.height.reset", &reset);
    if reset.clicked() && current != BAND_HEIGHT {
        state.draft_height = BAND_HEIGHT;
        requested = Some(BAND_HEIGHT);
    }

    if let Some(target) = requested {
        let clamped = target.clamp(MIN_BAND_HEIGHT, MAX_BAND_HEIGHT);
        if clamped < current {
            let safe_min = corpse::safe_min_height(doc);
            if clamped < safe_min {
                state.pending_crop_height = Some(clamped);
                state.height_notice = None;
            } else {
                state.pending_crop_height = None;
                state.height_notice = None;
                return Some(CorpseHeightAction::Apply { height: clamped, allow_crop: false });
            }
        } else {
            state.pending_crop_height = None;
            state.height_notice = None;
            return Some(CorpseHeightAction::Apply { height: clamped, allow_crop: false });
        }
    }

    let tokens = Tokens::get(ui.ctx());
    if let Some(pending) = state.pending_crop_height {
        let safe_min = corpse::safe_min_height(doc);
        if !enabled || pending >= safe_min {
            state.pending_crop_height = None;
        } else {
            let trimmed = safe_min.saturating_sub(pending);
            let mut action = None;
            egui::Frame::new().fill(tokens.card).stroke(Stroke::new(1.0, tokens.warning)).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
                ui.colored_label(tokens.warning, "Confirm crop");
                ui.label(format!("Artwork reaches {safe_min} px. Shrinking to {pending} px removes {trimmed} px at the bottom (undoable with Ctrl+Z)."));
                ui.horizontal_wrapped(|ui| {
                    let confirm = ui.button("Crop & resize");
                    crate::komaphone_workspace::remember(ui, "corpse.height.confirmCrop", &confirm);
                    if confirm.clicked() {
                        state.pending_crop_height = None;
                        state.height_notice = None;
                        action = Some(CorpseHeightAction::Apply { height: pending, allow_crop: true });
                    }
                    if safe_min < current {
                        let fit = ui.button(format!("Keep artwork ({safe_min} px)"));
                        crate::komaphone_workspace::remember(ui, "corpse.height.fitArtwork", &fit);
                        if fit.clicked() {
                            state.draft_height = safe_min;
                            state.pending_crop_height = None;
                            state.height_notice = None;
                            action = Some(CorpseHeightAction::Apply { height: safe_min, allow_crop: false });
                        }
                    }
                    let cancel = ui.button("Cancel");
                    crate::komaphone_workspace::remember(ui, "corpse.height.cancelCrop", &cancel);
                    if cancel.clicked() {
                        state.draft_height = current;
                        state.pending_crop_height = None;
                    }
                });
            });
            if action.is_some() {
                return action;
            }
        }
    }

    if let Some(notice) = &state.height_notice {
        ui.colored_label(tokens.warning, notice);
    } else if enabled {
        ui.weak(format!("Drag the bottom canvas handle or enter {MIN_BAND_HEIGHT}–{MAX_BAND_HEIGHT} px. Top seam stays anchored."));
    } else if let Some(reason) = disabled_reason {
        ui.weak(reason);
    }
    None
}

struct BottomHandleGeometry {
    bl: Pos2,
    br: Pos2,
    rect: Rect,
    vertical_cursor: bool,
}

fn bottom_handle_geometry(xf: &ViewXform, height: u32) -> Option<BottomHandleGeometry> {
    let bl = xf.to_screen(0.0, height as f32);
    let br = xf.to_screen(WIDTH as f32, height as f32);
    if !bl.is_finite() || !br.is_finite() {
        return None;
    }
    let mid = bl.lerp(br, 0.5);
    let step = xf.to_screen(WIDTH as f32 * 0.5, height as f32 + 1.0) - mid;
    let normal = if step.is_finite() && step.length_sq() > 1e-6 { step.normalized() } else { vec2(0.0, 1.0) };
    let edge_len = (br - bl).length();
    let vertical_cursor = normal.y.abs() >= normal.x.abs();
    // Place the grabber pill cleanly outside the bottom pixel edge (3 pt gap + 22 pt thickness).
    let pill_len = (edge_len * 0.55).clamp(132.0, 224.0);
    let pill_thickness = 22.0;
    let center = mid + normal * (3.0 + pill_thickness * 0.5);
    let size = if vertical_cursor { vec2(pill_len, pill_thickness) } else { vec2(pill_thickness, pill_len) };
    let rect = Rect::from_center_size(center, size);
    if !rect.min.is_finite() || !rect.max.is_finite() {
        return None;
    }
    Some(BottomHandleGeometry { bl, br, rect, vertical_cursor })
}

pub fn height_handle_cursor(state: &CorpseUi, xf: &ViewXform, height: u32, hover: Option<Pos2>, enabled: bool) -> Option<CursorIcon> {
    if let Some(drag) = &state.height_drag {
        let g = bottom_handle_geometry(xf, drag.preview_height)?;
        return Some(if g.vertical_cursor { CursorIcon::ResizeVertical } else { CursorIcon::ResizeHorizontal });
    }
    if !enabled {
        return None;
    }
    let pos = hover?;
    let g = bottom_handle_geometry(xf, height)?;
    g.rect.contains(pos).then_some(if g.vertical_cursor { CursorIcon::ResizeVertical } else { CursorIcon::ResizeHorizontal })
}

/// Handle primary-pointer drag on the Exquisite Corpse bottom-edge resize handle.
/// Returns `(pointer_consumed, optional_commit_action)`.
pub fn handle_height_drag(
    ui: &egui::Ui,
    response: &egui::Response,
    xf: &ViewXform,
    doc: &Document,
    enabled: bool,
    state: &mut CorpseUi,
) -> (bool, Option<CorpseHeightAction>) {
    if !enabled {
        state.height_drag = None;
        return (false, None);
    }
    if state.height_drag.as_ref().is_some_and(|drag| drag.doc_id != doc.id) {
        state.height_drag = None;
    }
    if let Some(drag) = state.height_drag.as_mut() {
        let cancel = ui.input(|i| i.key_pressed(Key::Escape) || i.pointer.secondary_pressed());
        if cancel {
            state.height_drag = None;
            return (true, None);
        }
        let pointer_pos = ui.ctx().pointer_latest_pos().or_else(|| response.interact_pointer_pos());
        let primary_down = ui.input(|i| i.pointer.primary_down());
        if let Some(pos) = pointer_pos {
            let doc_y = xf.to_doc(pos)[1] - (f64::from(drag.grab_offset_doc_y_tenths) / 10.0);
            if doc_y.is_finite() {
                let raw = doc_y.round() as i32;
                let limit_note = if raw < MIN_BAND_HEIGHT as i32 {
                    Some("Min height 128 px")
                } else if raw > MAX_BAND_HEIGHT as i32 {
                    Some("Max height 2048 px")
                } else {
                    None
                };
                let clamped = raw.clamp(MIN_BAND_HEIGHT as i32, MAX_BAND_HEIGHT as i32) as u32;
                if clamped < drag.safe_min {
                    drag.preview_height = drag.safe_min;
                    drag.clamped_by_artwork = true;
                    drag.clamped_by_limit = None;
                } else {
                    drag.preview_height = clamped;
                    drag.clamped_by_artwork = false;
                    drag.clamped_by_limit = limit_note;
                }
            }
        }
        if !primary_down {
            let finished = state.height_drag.take();
            if let Some(finished) = finished {
                if finished.clamped_by_artwork {
                    state.height_notice = Some(format!(
                        "Stopped at {} px to keep existing artwork intact. Use the left panel height control to crop explicitly (undoable).",
                        finished.safe_min
                    ));
                } else {
                    state.height_notice = None;
                }
                if finished.preview_height != finished.start_height {
                    return (true, Some(CorpseHeightAction::Apply { height: finished.preview_height, allow_crop: false }));
                }
            }
            return (true, None);
        }
        ui.ctx().request_repaint();
        return (true, None);
    }

    let primary_pressed = ui.input(|i| i.pointer.primary_pressed());
    if !primary_pressed {
        return (false, None);
    }
    let Some(pos) = response.hover_pos().or_else(|| ui.input(|i| i.pointer.interact_pos())) else {
        return (false, None);
    };
    if !xf.rect.contains(pos) {
        return (false, None);
    }
    let Some(g) = bottom_handle_geometry(xf, doc.size.height) else {
        return (false, None);
    };
    if !g.rect.contains(pos) {
        return (false, None);
    }
    let doc_y = xf.to_doc(pos)[1];
    let grab_offset = if doc_y.is_finite() { (doc_y - f64::from(doc.size.height)).clamp(-64.0, 64.0) } else { 0.0 };
    let safe_min = corpse::safe_min_height(doc);
    state.pending_crop_height = None;
    state.height_drag = Some(CorpseHeightDrag {
        doc_id: doc.id,
        start_height: doc.size.height,
        safe_min,
        preview_height: doc.size.height,
        grab_offset_doc_y_tenths: (grab_offset * 10.0).round() as i32,
        clamped_by_artwork: false,
        clamped_by_limit: None,
    });
    ui.ctx().request_repaint();
    (true, None)
}

fn draw_dashed_segment(painter: &egui::Painter, a: Pos2, b: Pos2, dash: f32, gap: f32, stroke: Stroke) {
    let delta = b - a;
    let total = delta.length();
    if !total.is_finite() || total <= 1e-3 || dash <= 0.0 {
        return;
    }
    let dir = delta / total;
    let step = dash + gap.max(1.0);
    let mut t = 0.0;
    while t < total {
        let end = (t + dash).min(total);
        painter.line_segment([a + dir * t, a + dir * end], stroke);
        t += step;
    }
}

/// Paint the Exquisite Corpse bottom-edge drag handle and live non-scaling height preview.
pub fn height_handle_overlay(
    ctx: &egui::Context,
    painter: &egui::Painter,
    xf: &ViewXform,
    current_height: u32,
    enabled: bool,
    state: &CorpseUi,
    hover_pos: Option<Pos2>,
) {
    if !enabled && state.height_drag.is_none() {
        crate::komaphone_workspace::forget_rect(ctx, "corpse.height.handle");
        return;
    }
    let active_height = state.height_drag.as_ref().map_or(current_height, |drag| drag.preview_height);
    let Some(g) = bottom_handle_geometry(xf, active_height) else {
        crate::komaphone_workspace::forget_rect(ctx, "corpse.height.handle");
        return;
    };
    if enabled {
        crate::komaphone_workspace::remember_rect(ctx, painter.clip_rect(), "corpse.height.handle", g.rect);
    } else {
        crate::komaphone_workspace::forget_rect(ctx, "corpse.height.handle");
    }
    let tokens = Tokens::get(ctx);
    let hovered = state.height_drag.is_some() || hover_pos.is_some_and(|p| g.rect.contains(p));

    if let Some(drag) = &state.height_drag {
        let cur_l = xf.to_screen(0.0, current_height as f32);
        let cur_r = xf.to_screen(WIDTH as f32, current_height as f32);
        let new_l = g.bl;
        let new_r = g.br;
        if drag.preview_height != current_height && cur_l.is_finite() && cur_r.is_finite() {
            let fill = if drag.preview_height > current_height { tokens.accent_soft.gamma_multiply(0.55) } else { tokens.scrim.gamma_multiply(0.45) };
            let quad = vec![cur_l, cur_r, new_r, new_l];
            painter.add(egui::Shape::convex_polygon(quad, fill, Stroke::NONE));
            let rail = Stroke::new(1.5, tokens.accent);
            draw_dashed_segment(painter, cur_l, new_l, 6.0, 4.0, rail);
            draw_dashed_segment(painter, cur_r, new_r, 6.0, 4.0, rail);
        }
        let guide_color = if drag.clamped_by_artwork { tokens.warning } else { tokens.accent };
        painter.line_segment([new_l, new_r], Stroke::new(2.0, guide_color));

        let label = if drag.clamped_by_artwork {
            format!("↕ {WIDTH} × {} px · artwork limit", drag.preview_height)
        } else if let Some(limit) = drag.clamped_by_limit {
            format!("↕ {WIDTH} × {} px · {limit}", drag.preview_height)
        } else {
            let delta = drag.preview_height as i32 - drag.start_height as i32;
            format!("↕ {WIDTH} × {} px ({delta:+})", drag.preview_height)
        };
        let badge_w = if g.vertical_cursor { g.rect.width().max(220.0) } else { 220.0 };
        let clip = painter.clip_rect().shrink(3.0);
        let badge_h = 24.0;
        let center_x = g.rect.center().x.clamp(clip.left() + badge_w * 0.5, (clip.right() - badge_w * 0.5).max(clip.left() + badge_w * 0.5));
        let center_y = g.rect.center().y.clamp(clip.top() + badge_h * 0.5, (clip.bottom() - badge_h * 0.5).max(clip.top() + badge_h * 0.5));
        let badge = Rect::from_center_size(pos2(center_x, center_y), vec2(badge_w, badge_h));
        painter.rect_filled(badge, 12.0, tokens.card);
        painter.rect_stroke(badge, 12.0, Stroke::new(1.5, guide_color), StrokeKind::Inside);
        painter.text(badge.center(), Align2::CENTER_CENTER, label, FontId::proportional(11.5), tokens.text);
        return;
    }

    let edge_stroke = Stroke::new(if hovered { 1.5 } else { 1.0 }, if hovered { tokens.accent } else { tokens.accent_border });
    painter.line_segment([g.bl, g.br], edge_stroke);

    let bg = if hovered { tokens.hover } else { tokens.card };
    let border = if hovered { tokens.accent } else { tokens.card_border };
    painter.rect_filled(g.rect, 11.0, bg);
    painter.rect_stroke(g.rect, 11.0, Stroke::new(1.0, border), StrokeKind::Inside);
    let label = if g.vertical_cursor {
        if g.rect.width() >= 180.0 { format!("↕ Drag height · {current_height} px") } else { format!("↕ {current_height} px") }
    } else {
        "↔".into()
    };
    painter.text(g.rect.center(), Align2::CENTER_CENTER, label, FontId::proportional(11.0), if hovered { tokens.text } else { tokens.text_dim });
}

fn seam_corners(xf: &ViewXform) -> [Pos2; 4] {
    [xf.to_screen(0.0, -(SEAM_HEIGHT as f32)), xf.to_screen(WIDTH as f32, -(SEAM_HEIGHT as f32)), xf.to_screen(WIDTH as f32, 0.0), xf.to_screen(0.0, 0.0)]
}

/// Draw above (never over) the editable band, following pan, zoom, view rotation and
/// flip exactly. This only paints: it registers no editable/interactable region.
pub fn seam_overlay(painter: &egui::Painter, xf: &ViewXform, texture: TextureId) {
    let corners = seam_corners(xf);
    if !corners.iter().all(|corner| corner.is_finite()) {
        return;
    }
    let mut mesh = Mesh::with_texture(texture);
    for (pos, uv) in corners.into_iter().zip([pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(1.0, 1.0), pos2(0.0, 1.0)]) {
        // White tint preserves the color-managed artwork; UI decoration uses tokens.
        mesh.vertices.push(egui::epaint::Vertex { pos, uv, color: Color32::WHITE });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
    let tokens = Tokens::get(painter.ctx());
    painter.line_segment([corners[3], corners[2]], Stroke::new(1.0, tokens.accent_border));
}

fn tracked_choice(ui: &mut egui::Ui, value: &mut u8, next: u8, label: &str, control: &str) {
    let response = ui.selectable_value(value, next, label);
    crate::komaphone_workspace::remember(ui, control, &response);
}

/// Full and part-by-part views use precisely adjacent rectangles proportional to each
/// accepted segment height. Missing labels and authors are UI metadata, so they cannot
/// enter final artwork.
pub fn reveal(ui: &mut egui::Ui, room: &RoomView, state: &mut CorpseUi, texture: &mut impl FnMut(AssetHash) -> Option<(TextureId, Vec2)>) {
    let first = match corpse::reveal_chain(room, 0) {
        Ok(chain) => chain,
        Err(error) => {
            ui.label(error.to_string());
            return;
        }
    };
    if state.game != Some(first.game) {
        *state = CorpseUi { game: Some(first.game), ..Default::default() };
    }
    if usize::from(state.chain) >= room.members.len() {
        state.select_chain(0);
    }
    state.visible_parts = state.visible_parts.clamp(1, 3);
    let mut selected_chain = state.chain;
    ui.horizontal_wrapped(|ui| {
        for index in 0..room.members.len().min(3) {
            tracked_choice(ui, &mut selected_chain, index as u8, &format!("Creature {}", index + 1), &format!("corpse.chain.{index}"));
        }
    });
    state.select_chain(selected_chain);
    ui.horizontal_wrapped(|ui| {
        tracked_choice(ui, &mut state.visible_parts, 1, "Head", "corpse.reveal.head");
        tracked_choice(ui, &mut state.visible_parts, 2, "Add torso", "corpse.reveal.torso");
        tracked_choice(ui, &mut state.visible_parts, 3, "Full creature", "corpse.reveal.full");
    });
    let chain = match corpse::reveal_chain(room, state.chain) {
        Ok(chain) => chain,
        Err(error) => {
            ui.label(error.to_string());
            return;
        }
    };
    let heights = chain.parts.each_ref().map(|part| part.height);
    let total_height = chain.total_height().max(1);
    let tokens = Tokens::get(ui.ctx());
    let width = ui.available_width().clamp(1.0, 300.0);
    let size = vec2(width, width * total_height as f32 / WIDTH as f32);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, tokens.canvas);
    let bands = band_rects(rect, heights);
    for (index, part) in chain.parts.iter().enumerate() {
        let band = bands[index];
        if index >= usize::from(state.visible_parts) {
            painter.rect_filled(band, 0.0, tokens.card);
            painter.text(band.center(), Align2::CENTER_CENTER, "Not revealed yet", FontId::proportional(13.0), tokens.text_dim);
        } else if let Some(image) = part.image {
            if let Some((id, dimensions)) = texture(image.hash) {
                if dimensions == vec2(WIDTH as f32, part.height as f32) {
                    painter.image(id, band, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
                } else {
                    painter.text(band.center(), Align2::CENTER_CENTER, "Invalid part dimensions", FontId::proportional(13.0), tokens.text);
                }
            } else {
                painter.text(band.center(), Align2::CENTER_CENTER, "Loading saved part…", FontId::proportional(13.0), tokens.text_dim);
            }
        } else {
            painter.text(band.center(), Align2::CENTER_CENTER, "Missing submission", FontId::proportional(13.0), tokens.text);
        }
    }
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, tokens.card_border), StrokeKind::Outside);
    for part in chain.parts.iter().take(usize::from(state.visible_parts)) {
        ui.label(format!("{} · {}", corpse::part_name(part.assignment.round), part.author_name));
        if let Some(reason) = &part.missing {
            ui.weak(format!("Missing: {reason}"));
        }
    }
    ui.weak(format!("{WIDTH} × {total_height} ({} + {} + {}) · credits stay outside artwork", heights[0], heights[1], heights[2]));
}

fn band_rects(rect: Rect, heights: [u32; 3]) -> [Rect; 3] {
    let total = f64::from(heights[0]) + f64::from(heights[1]) + f64::from(heights[2]);
    let total = if total > 0.0 { total } else { 3.0 };
    let y0 = rect.top();
    let y1 = y0 + (f64::from(rect.height()) * (f64::from(heights[0]) / total)) as f32;
    let y2 = y0 + (f64::from(rect.height()) * ((f64::from(heights[0]) + f64::from(heights[1])) / total)) as f32;
    let y3 = rect.bottom();
    [
        Rect::from_min_max(pos2(rect.left(), y0), pos2(rect.right(), y1)),
        Rect::from_min_max(pos2(rect.left(), y1), pos2(rect.right(), y2)),
        Rect::from_min_max(pos2(rect.left(), y2), pos2(rect.right(), y3)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joining_strip_never_consumes_document_pixels_at_any_view_transform() {
        for (zoom, rotation, flip) in [(1.0, 0.0, false), (0.25, 0.0, false), (2.0, 90.0, false), (0.5, 27.0, true)] {
            let xf = ViewXform { rect: Rect::from_min_size(pos2(10.0, 20.0), vec2(800.0, 600.0)), zoom, center: [512.0, 256.0], flip, rotation, aspect: 1.0 };
            let corners = seam_corners(&xf);
            assert_eq!(corners[2], xf.to_screen(1024.0, 0.0));
            assert_eq!(corners[3], xf.to_screen(0.0, 0.0));
            for (corner, expected) in corners.into_iter().zip([[0.0, -24.0], [1024.0, -24.0], [1024.0, 0.0], [0.0, 0.0]]) {
                let result = xf.to_doc(corner);
                assert!((result[0] - expected[0]).abs() < 0.001);
                assert!((result[1] - expected[1]).abs() < 0.001);
            }
        }
    }

    #[test]
    fn reveal_bands_touch_without_gaps_and_match_variable_segment_ratios() {
        let outer = Rect::from_min_size(pos2(17.0, 19.0), vec2(256.0, 480.0));
        let bands = band_rects(outer, [192, 512, 1216]);
        assert_eq!(bands[0].top(), outer.top());
        assert_eq!(bands[0].bottom(), bands[1].top());
        assert_eq!(bands[1].bottom(), bands[2].top());
        assert_eq!(bands[2].bottom(), outer.bottom());
        assert!(bands.iter().all(|band| band.width() == outer.width()));
        assert!((bands[0].height() - 48.0).abs() < 0.01);
        assert!((bands[1].height() - 128.0).abs() < 0.01);
        assert!((bands[2].height() - 304.0).abs() < 0.01);
    }

    #[test]
    fn viewer_navigation_round_trips_without_serializing_textures_or_artwork() {
        let state = CorpseUi { game: Some(GameId::new(&format!("{:032x}", 4)).unwrap()), chain: 2, visible_parts: 2, ..Default::default() };
        let encoded = serde_json::to_string(&state).unwrap();
        let decoded: CorpseUi = serde_json::from_str(&encoded).unwrap();
        assert_eq!(state, decoded);
        assert!(!encoded.contains("rgba"));
        assert!(!encoded.contains("texture"));
        assert!(!encoded.contains("height_drag"));
    }

    #[test]
    fn selecting_another_creature_restarts_sequential_reveal_at_head() {
        let mut state = CorpseUi { game: Some(GameId::new(&format!("{:032x}", 9)).unwrap()), chain: 0, visible_parts: 3, ..Default::default() };
        // Re-selecting the current creature keeps its reveal progress.
        state.select_chain(0);
        assert_eq!((state.chain, state.visible_parts), (0, 3));
        // Advancing to the next creature restarts at Head.
        state.select_chain(1);
        assert_eq!((state.chain, state.visible_parts), (1, 1));
        // Switching mid-reveal (torso visible) to creature 3 or backward to creature 1 restarts at Head.
        state.visible_parts = 2;
        state.select_chain(2);
        assert_eq!((state.chain, state.visible_parts), (2, 1));
        state.visible_parts = 3;
        state.select_chain(0);
        assert_eq!((state.chain, state.visible_parts), (0, 1));
    }

    #[test]
    fn bottom_handle_sits_outside_canvas_pixels_under_zoom_and_rotation() {
        for (zoom, rotation, flip) in [(0.75, 0.0, false), (1.5, 0.0, false), (1.0, 90.0, false), (0.8, 0.0, true)] {
            let xf = ViewXform { rect: Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 800.0)), zoom, center: [512.0, 256.0], flip, rotation, aspect: 1.0 };
            let g = bottom_handle_geometry(&xf, 512).unwrap();
            let center_doc = xf.to_doc(g.rect.center());
            assert!(center_doc[1] > 512.0, "handle center must sit below bottom edge (doc_y={})", center_doc[1]);
        }
    }
}
