//! Read-only creature references and reveals. Artwork remains in the engine/service;
//! this module paints granted textures and keeps only viewer navigation state.
use crate::{canvas::ViewXform, theme::Tokens};
use egui::{Align2, Color32, FontId, Mesh, Pos2, Rect, Sense, Stroke, StrokeKind, TextureId, Vec2, pos2, vec2};
use photocraft_engine::party::room::RoomView;
use photocraft_engine::party::{AssetHash, GameId};
use photocraft_engine::rooms::corpse::{self, BAND_HEIGHT, HEIGHT, SEAM_HEIGHT, WIDTH};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CorpseUi {
    pub game: Option<GameId>,
    pub chain: u8,
    /// Viewer-only mask. It never modifies or exports an artwork buffer.
    pub visible_parts: u8,
}
impl Default for CorpseUi {
    fn default() -> Self {
        Self { game: None, chain: 0, visible_parts: 1 }
    }
}

pub fn seam_caption(ui: &mut egui::Ui, round: u8, missing: bool) {
    ui.strong(format!("Draw the {}", corpse::part_name(round).to_lowercase()));
    if round == 0 {
        ui.label("Draw down to the bottom edge so the next player can connect the torso.");
    } else if missing {
        ui.label("The preceding part was not submitted. Draw your part independently; the reveal will label the missing part.");
    } else {
        ui.label("Match the locked 24 px strip above your drawing. Only your 1024 × 512 band is submitted.");
        if round == 1 {
            ui.label("Draw down to the bottom edge so the next player can connect the legs.");
        }
    }
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

/// Full and part-by-part views use precisely adjacent 512-row rectangles. Missing
/// labels and authors are UI metadata, so they cannot enter final artwork.
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
        state.chain = 0;
    }
    state.visible_parts = state.visible_parts.clamp(1, 3);
    ui.horizontal_wrapped(|ui| {
        for index in 0..room.members.len().min(3) {
            tracked_choice(ui, &mut state.chain, index as u8, &format!("Creature {}", index + 1), &format!("corpse.chain.{index}"));
        }
    });
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
    let tokens = Tokens::get(ui.ctx());
    let width = ui.available_width().clamp(1.0, 300.0);
    let size = vec2(width, width * HEIGHT as f32 / WIDTH as f32);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, tokens.canvas);
    for (index, part) in chain.parts.iter().enumerate() {
        let band = band_rect(rect, index as u8);
        if index >= usize::from(state.visible_parts) {
            painter.rect_filled(band, 0.0, tokens.card);
            painter.text(band.center(), Align2::CENTER_CENTER, "Not revealed yet", FontId::proportional(13.0), tokens.text_dim);
        } else if let Some(image) = part.image {
            if let Some((id, dimensions)) = texture(image.hash) {
                if dimensions == vec2(WIDTH as f32, BAND_HEIGHT as f32) {
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
    ui.weak("1024 × 1536 · three original bands · credits stay outside artwork");
}

fn band_rect(rect: Rect, round: u8) -> Rect {
    let step = rect.height() / 3.0;
    let top = rect.top() + f32::from(round) * step;
    let bottom = if round == 2 { rect.bottom() } else { rect.top() + f32::from(round.saturating_add(1)) * step };
    Rect::from_min_max(pos2(rect.left(), top), pos2(rect.right(), bottom))
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
    fn reveal_bands_touch_without_gaps_and_keep_full_canvas_height() {
        let outer = Rect::from_min_size(pos2(17.0, 19.0), vec2(257.0, 385.5));
        let bands = [0, 1, 2].map(|round| band_rect(outer, round));
        assert_eq!(bands[0].top(), outer.top());
        assert_eq!(bands[0].bottom(), bands[1].top());
        assert_eq!(bands[1].bottom(), bands[2].top());
        assert_eq!(bands[2].bottom(), outer.bottom());
        assert!(bands.iter().all(|band| band.width() == outer.width()));
    }

    #[test]
    fn viewer_navigation_round_trips_without_serializing_textures_or_artwork() {
        let state = CorpseUi { game: Some(GameId::new(&format!("{:032x}", 4)).unwrap()), chain: 2, visible_parts: 2 };
        let encoded = serde_json::to_string(&state).unwrap();
        let decoded: CorpseUi = serde_json::from_str(&encoded).unwrap();
        assert_eq!(state, decoded);
        assert!(!encoded.contains("rgba"));
        assert!(!encoded.contains("texture"));
    }
}
