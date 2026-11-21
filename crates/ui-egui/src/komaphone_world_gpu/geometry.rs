//! GPU `Texture2DArray` slot allocator (reconciled across visible, fallback, dynamic,
//! and prefetch partitions), single-pass quad vertex/index encoder, and CPU software
//! fallback presenter when `gpu_render_state` is unavailable.
use std::collections::BTreeMap;

use photocraft_engine::scene::Viewport;
use photocraft_engine::scene::spatial::{TILE_APRON, TILE_LOGICAL, TILE_PHYSICAL, TileKey, WorldRect, world_rect_to_screen};
use photocraft_engine::world_tiles::TileBudget;

use super::Result;

pub(super) const ALIGNED_ROW_BYTES: u32 = (TILE_PHYSICAL * 4).next_multiple_of(256);
pub(super) const STAGING_TILE_BYTES: usize = (ALIGNED_ROW_BYTES * TILE_PHYSICAL) as usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SlotKind {
    Static,
    CoarseFallback,
    Dynamic,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct SlotMeta {
    pub tile: TileKey,
    pub dynamic: bool,
    pub validated_seq: u64,
    pub scope_gen: u64,
    pub last_frame: u64,
    pub kind: SlotKind,
    pub dynamic_sig: u64,
    pub provisional: bool,
}

/// Manages `N` (`256` desktop / `128` conservative) array layers of the `260×260×N`
/// GPU tile texture array with protected coarse-fallback and dynamic partitions.
pub(super) struct TileAtlas {
    budget: TileBudget,
    slots: Vec<Option<SlotMeta>>,
    lookup: BTreeMap<(TileKey, bool), u32>,
}

impl TileAtlas {
    pub fn new(budget: TileBudget) -> Self {
        Self { budget, slots: vec![None; budget.gpu_layers as usize], lookup: BTreeMap::new() }
    }

    pub fn clear(&mut self) {
        self.slots.fill(None);
        self.lookup.clear();
    }

    pub fn resident_count(&self) -> usize {
        self.lookup.len()
    }

    pub fn peek_slot(&self, tile: TileKey, dynamic: bool) -> Option<(u32, SlotMeta)> {
        let layer = *self.lookup.get(&(tile, dynamic))?;
        self.slots.get(layer as usize).and_then(|slot| *slot).map(|meta| (layer, meta))
    }

    pub fn get_slot(&mut self, tile: TileKey, dynamic: bool, frame: u64) -> Option<(u32, SlotMeta)> {
        let layer = *self.lookup.get(&(tile, dynamic))?;
        if let Some(Some(meta)) = self.slots.get_mut(layer as usize) {
            meta.last_frame = frame;
            Some((layer, *meta))
        } else {
            None
        }
    }

    pub fn update_validated_seq(&mut self, layer: u32, seq: u64) {
        if let Some(Some(meta)) = self.slots.get_mut(layer as usize) {
            meta.validated_seq = seq;
        }
    }

    pub fn set_dynamic_sig(&mut self, layer: u32, sig: u64) {
        if let Some(Some(meta)) = self.slots.get_mut(layer as usize) {
            meta.dynamic_sig = sig;
        }
    }

    pub fn set_provisional(&mut self, layer: u32, provisional: bool) {
        if let Some(Some(meta)) = self.slots.get_mut(layer as usize) {
            meta.provisional = provisional;
        }
    }

    /// Allocate or reuse a GPU texture array layer for `(tile, dynamic)`.
    /// Never evicts a layer touched during `frame`; protects reserved fallback and dynamic quotas.
    pub fn allocate(&mut self, tile: TileKey, dynamic: bool, kind: SlotKind, validated_seq: u64, scope_gen: u64, frame: u64) -> Option<u32> {
        if let Some(&existing) = self.lookup.get(&(tile, dynamic)) {
            if let Some(slot) = self.slots.get_mut(existing as usize) {
                *slot = Some(SlotMeta { tile, dynamic, validated_seq, scope_gen, last_frame: frame, kind, dynamic_sig: 0, provisional: false });
            }
            return Some(existing);
        }
        // 1. Prefer any completely free slot.
        if let Some(idx) = self.slots.iter().position(Option::is_none) {
            let layer = idx as u32;
            self.slots[idx] = Some(SlotMeta { tile, dynamic, validated_seq, scope_gen, last_frame: frame, kind, dynamic_sig: 0, provisional: false });
            self.lookup.insert((tile, dynamic), layer);
            return Some(layer);
        }
        // 2. Count current partition usage so coarse fallback & dynamic reservations stay intact.
        let mut fallback_count = 0usize;
        let mut dynamic_count = 0usize;
        for s in self.slots.iter().flatten() {
            match s.kind {
                SlotKind::CoarseFallback => fallback_count += 1,
                SlotKind::Dynamic => dynamic_count += 1,
                SlotKind::Static => {}
            }
        }
        let victim = self.find_eviction_victim(frame, kind, fallback_count, dynamic_count)?;
        if let Some(Some(old)) = self.slots.get(victim) {
            self.lookup.remove(&(old.tile, old.dynamic));
        }
        let layer = victim as u32;
        if let Some(slot) = self.slots.get_mut(victim) {
            *slot = Some(SlotMeta { tile, dynamic, validated_seq, scope_gen, last_frame: frame, kind, dynamic_sig: 0, provisional: false });
        }
        self.lookup.insert((tile, dynamic), layer);
        Some(layer)
    }

    fn find_eviction_victim(&self, frame: u64, incoming: SlotKind, fallback_count: usize, dynamic_count: usize) -> Option<usize> {
        let mut best: Option<(u8, u64, usize)> = None;
        for (idx, slot) in self.slots.iter().enumerate() {
            let Some(meta) = slot else { continue };
            if meta.last_frame == frame {
                continue;
            }
            // Lower priority tier = preferred eviction victim.
            let tier = match meta.kind {
                SlotKind::Static => 0u8,
                SlotKind::Dynamic => {
                    if incoming == SlotKind::Dynamic || dynamic_count > self.budget.reserved_dynamic_slots {
                        1
                    } else {
                        3
                    }
                }
                SlotKind::CoarseFallback => {
                    if incoming == SlotKind::CoarseFallback || fallback_count > self.budget.reserved_fallback_slots {
                        2
                    } else {
                        4
                    }
                }
            };
            let cand = (tier, meta.last_frame, idx);
            if best.is_none_or(|b| cand < b) {
                best = Some(cand);
            }
        }
        best.map(|(_, _, idx)| idx)
    }
}

/// Compact vertex/index buffer for the single-pass `Texture2DArray` tile quad pass.
/// Vertex layout: `pos: [f32; 2]` + `uv: [f32; 2]` + `layer: u32` = `20` bytes/vertex.
#[derive(Default)]
pub(super) struct TileQuadBatch {
    pub vertices: Vec<u8>,
    pub indices: Vec<u8>,
}

impl TileQuadBatch {
    pub fn clear(&mut self) {
        self.vertices.clear();
        self.indices.clear();
    }

    pub fn vertex_count(&self) -> usize {
        self.vertices.len() / 20
    }

    pub fn index_count(&self) -> u32 {
        (self.indices.len() / 4) as u32
    }

    /// Push one textured quad covering world `rect` inside `view`, sampling `uv` from `layer`.
    pub fn push_rect(&mut self, rect: WorldRect, uv: [f32; 4], layer: u32, view: &Viewport) -> Result<()> {
        let [x0, y0, x1, y1] = world_rect_to_screen(rect, view).map_err(|e| e.to_string())?;
        if x1 <= 0.0 || y1 <= 0.0 || x0 >= view.width as f32 || y0 >= view.height as f32 {
            return Ok(());
        }
        let base = self.vertex_count() as u32;
        let vw = view.width as f32;
        let vh = view.height as f32;
        let nx0 = x0 * 2.0 / vw - 1.0;
        let nx1 = x1 * 2.0 / vw - 1.0;
        let ny0 = 1.0 - y0 * 2.0 / vh;
        let ny1 = 1.0 - y1 * 2.0 / vh;
        for (px, py, u, v) in [(nx0, ny0, uv[0], uv[1]), (nx1, ny0, uv[2], uv[1]), (nx0, ny1, uv[0], uv[3]), (nx1, ny1, uv[2], uv[3])] {
            self.vertices.extend_from_slice(&px.to_le_bytes());
            self.vertices.extend_from_slice(&py.to_le_bytes());
            self.vertices.extend_from_slice(&u.to_le_bytes());
            self.vertices.extend_from_slice(&v.to_le_bytes());
            self.vertices.extend_from_slice(&layer.to_le_bytes());
        }
        for i in [0u32, 1, 2, 2, 1, 3] {
            self.indices.extend_from_slice(&(base + i).to_le_bytes());
        }
        Ok(())
    }

    /// Push one full target-LOD logical tile quad (`UV = [2/260, 258/260]`).
    pub fn push_tile(&mut self, tile: TileKey, layer: u32, view: &Viewport) -> Result<()> {
        let u0 = TILE_APRON as f32 / TILE_PHYSICAL as f32;
        let u1 = (TILE_APRON + TILE_LOGICAL) as f32 / TILE_PHYSICAL as f32;
        self.push_rect(tile.logical_rect(), [u0, u0, u1, u1], layer, view)
    }
}

/// Copy tightly packed `260×260×4` premultiplied RGBA8 bytes into a 256-byte-row-aligned
/// staging slice (`1280 * 260 = 332,800` B).
pub(super) fn pad_tile_rows(src: &[u8], dst: &mut [u8]) -> Result<()> {
    let src_row = (TILE_PHYSICAL * 4) as usize;
    let dst_row = ALIGNED_ROW_BYTES as usize;
    let rows = TILE_PHYSICAL as usize;
    if src.len() < src_row * rows || dst.len() < dst_row * rows {
        return Err("Invalid tile staging buffer dimensions".into());
    }
    for y in 0..rows {
        let s0 = y * src_row;
        let d0 = y * dst_row;
        if let (Some(s), Some(d)) = (src.get(s0..s0 + src_row), dst.get_mut(d0..d0 + src_row)) {
            d.copy_from_slice(s);
        }
    }
    Ok(())
}

/// Pure-CPU egui texture fallback used ONLY when `app.gpu_render_state` is `None`.
#[derive(Default)]
pub struct CpuTilePresenter {
    texture: Option<egui::TextureHandle>,
}

impl CpuTilePresenter {
    pub fn present_tiles(&mut self, ctx: &egui::Context, view: &Viewport, density: f32, tiles: &[(WorldRect, [f32; 4], &[u8])]) -> Result<egui::TextureId> {
        let w = ((f64::from(view.width) * f64::from(density)).ceil() as usize).clamp(1, 2048);
        let h = ((f64::from(view.height) * f64::from(density)).ceil() as usize).clamp(1, 2048);
        let mut pixels = vec![egui::Color32::WHITE; w * h];
        let sx_scale = w as f64 / f64::from(view.width);
        let sy_scale = h as f64 / f64::from(view.height);
        let side = TILE_PHYSICAL as usize;

        for &(rect, uv, rgba) in tiles {
            let [x0, y0, x1, y1] = world_rect_to_screen(rect, view).map_err(|e| e.to_string())?;
            let px0 = (f64::from(x0) * sx_scale).floor().clamp(0.0, w as f64) as usize;
            let px1 = (f64::from(x1) * sx_scale).ceil().clamp(0.0, w as f64) as usize;
            let py0 = (f64::from(y0) * sy_scale).floor().clamp(0.0, h as f64) as usize;
            let py1 = (f64::from(y1) * sy_scale).ceil().clamp(0.0, h as f64) as usize;
            let span_x = (f64::from(x1) - f64::from(x0)) * sx_scale;
            let span_y = (f64::from(y1) - f64::from(y0)) * sy_scale;
            if px0 >= px1 || py0 >= py1 || span_x <= 0.0 || span_y <= 0.0 {
                continue;
            }
            for py in py0..py1 {
                let fy = ((py as f64 + 0.5 - f64::from(y0) * sy_scale) / span_y).clamp(0.0, 1.0);
                let v = f64::from(uv[1]) + fy * f64::from(uv[3] - uv[1]);
                let ty = ((v * side as f64).floor() as usize).min(side.saturating_sub(1));
                let row = py * w;
                for px in px0..px1 {
                    let fx = ((px as f64 + 0.5 - f64::from(x0) * sx_scale) / span_x).clamp(0.0, 1.0);
                    let u = f64::from(uv[0]) + fx * f64::from(uv[2] - uv[0]);
                    let tx = ((u * side as f64).floor() as usize).min(side.saturating_sub(1));
                    let off = (ty * side + tx) * 4;
                    if let (Some(src), Some(dst)) = (rgba.get(off..off + 4), pixels.get_mut(row + px)) {
                        *dst = egui::Color32::from_rgba_premultiplied(src[0], src[1], src[2], src[3]);
                    }
                }
            }
        }
        let image = egui::ColorImage { size: [w, h], source_size: egui::vec2(view.width as f32, view.height as f32), pixels };
        let handle = match self.texture.as_mut() {
            Some(tex) => {
                tex.set(image, egui::TextureOptions::LINEAR);
                tex
            }
            None => {
                let tex = ctx.load_texture("komaphone-world-cpu-fallback", image, egui::TextureOptions::LINEAR);
                self.texture.insert(tex)
            }
        };
        Ok(handle.id())
    }
}
