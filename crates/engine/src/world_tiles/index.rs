//! Octave-bucketed world spatial index and bounded sparse `8×8` micro-summary tree.
//!
//! Eliminates the legacy `MAX_VISIBLE_STROKES = 4,096` depth-0 overview ceiling and
//! supports O(1) incremental event apply/undo (powering live edits and catch-up
//! playback) while preserving exact `(DeviceId, created_revision)` ordering between
//! direct objects and sub-pixel deep summaries.
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use photocraft_scene::shared::{AcceptedOp, DeviceId, Edit, MediaId, SharedMedia, SharedStroke, WorldState};
use photocraft_scene::spatial::{TILE_LOGICAL, TileKey, WorldRect, media_world_rect, pixel_ticks, stroke_world_rect};
use photocraft_scene::{StrokeId, coordinates};

use crate::{EngineError, Result};

/// Maximum sparse summary level (`cell_span(32) = 1 << 99`, covering up to depth-0 overview).
pub const MAX_SUMMARY_LEVEL: u8 = 32;
/// Hard bound on summary nodes visited per tile during cold overview rendering.
pub const MAX_SUMMARY_NODES_PER_TILE: usize = 512;
/// Maximum interleaved sub-pixel objects expanded when a coarse direct stroke
/// straddles a summary epoch on the same device.
pub const MAX_INTERLEAVED_EXPAND: usize = 256;

fn err(msg: impl Into<String>) -> EngineError {
    EngineError::Other(msg.into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SceneObject {
    Stroke(StrokeId),
    Media(MediaId),
}

#[derive(Clone, Copy, Debug)]
pub struct IndexedEntry {
    pub object: SceneObject,
    pub rect: WorldRect,
    pub device: DeviceId,
    pub created_rev: u64,
    pub changed_rev: u64,
    /// `floor(log2(rect.max_extent_ticks()))`, clamped to `0..=119`.
    pub extent_bit: u8,
    /// `extent_bit / 3`, clamped to `0..=MAX_SUMMARY_LEVEL`.
    pub leaf_level: u8,
    pub visible: bool,
}

/// Sparse `8×8` premultiplied RGBA `[f32; 4]` micro-summary for one device over a
/// contiguous revision interval `[min_rev, max_rev]`.
#[derive(Clone, Debug)]
pub struct SummaryEpoch {
    pub min_rev: u64,
    pub max_rev: u64,
    pub object_count: u32,
    /// Sorted `(subcell_idx 0..64, premultiplied [r, g, b, a])` for populated sub-cells only.
    pub cells: Vec<(u8, [f32; 4])>,
}

impl SummaryEpoch {
    fn average_pixel(&self) -> [f32; 4] {
        let mut sum = [0.0f32; 4];
        for &(_, px) in &self.cells {
            for c in 0..4 {
                sum[c] += px[c];
            }
        }
        [sum[0] / 64.0, sum[1] / 64.0, sum[2] / 64.0, sum[3] / 64.0]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SummaryKey {
    pub level: u8,
    pub cx: i128,
    pub cy: i128,
}

impl SummaryKey {
    #[inline]
    pub fn cell_span(level: u8) -> i128 {
        1_i128 << (u32::from(level.min(MAX_SUMMARY_LEVEL)) * 3 + 3)
    }

    #[inline]
    pub fn subcell_span(level: u8) -> i128 {
        1_i128 << (u32::from(level.min(MAX_SUMMARY_LEVEL)) * 3)
    }

    pub fn world_rect(self) -> WorldRect {
        let span = Self::cell_span(self.level);
        let min_x = self.cx.saturating_mul(span);
        let min_y = self.cy.saturating_mul(span);
        WorldRect { min_x, min_y, max_x: min_x.saturating_add(span), max_y: min_y.saturating_add(span) }
    }

    pub fn subcell_rect(self, sub_idx: u8) -> WorldRect {
        let sub = Self::subcell_span(self.level);
        let base = self.world_rect();
        let sx = i128::from(sub_idx & 7);
        let sy = i128::from((sub_idx >> 3) & 7);
        let min_x = base.min_x.saturating_add(sx.saturating_mul(sub));
        let min_y = base.min_y.saturating_add(sy.saturating_mul(sub));
        WorldRect { min_x, min_y, max_x: min_x.saturating_add(sub), max_y: min_y.saturating_add(sub) }
    }

    pub fn parent(self) -> Option<(Self, u8)> {
        if self.level >= MAX_SUMMARY_LEVEL {
            return None;
        }
        let px = self.cx.div_euclid(8);
        let py = self.cy.div_euclid(8);
        let sx = self.cx.rem_euclid(8) as u8;
        let sy = self.cy.rem_euclid(8) as u8;
        Some((Self { level: self.level + 1, cx: px, cy: py }, sy * 8 + sx))
    }

    pub fn child(self, sub_idx: u8) -> Option<Self> {
        if self.level == 0 {
            return None;
        }
        let sx = i128::from(sub_idx & 7);
        let sy = i128::from((sub_idx >> 3) & 7);
        Some(Self { level: self.level - 1, cx: self.cx.checked_mul(8)?.checked_add(sx)?, cy: self.cy.checked_mul(8)?.checked_add(sy)? })
    }
}

#[derive(Clone, Default)]
struct SummaryNode {
    /// Visible objects whose `leaf_level == this node's level`.
    leaves: BTreeSet<SceneObject>,
    /// Bit `sy * 8 + sx` is set iff child `(level - 1, cx*8+sx, cy*8+sy)` exists and is non-empty.
    child_mask: u64,
    /// Cached `8×8` summary of ONLY the strictly deeper subtree (`level - 1` children,
    /// i.e., objects with `leaf_level < self.level`). Disjoint from `leaves` by construction.
    children_summary: BTreeMap<DeviceId, SummaryEpoch>,
    /// Cached 1-pixel total average of `children_summary` + `leaves` per device,
    /// fed upward into `parent.children_summary`.
    total_pixel: BTreeMap<DeviceId, (u64, u64, u32, [f32; 4])>,
    dirty: bool,
}

/// Resolved direct objects and sub-pixel summary items intersecting one world tile.
pub struct TileQuery {
    pub direct: Vec<SceneObject>,
    pub summaries: Vec<(SummaryKey, DeviceId, SummaryEpoch)>,
    pub summary_nodes_visited: usize,
}

#[inline]
fn floor_log2_u128(v: u128) -> u8 {
    let bit = 127u32.saturating_sub(v.max(1).leading_zeros());
    bit.min(119) as u8
}

#[inline]
fn direct_bucket_span(extent_bit: u8) -> i128 {
    // Span is 16× the object's extent octave so each object spans at most 1..=4 cells.
    let shift = (u32::from(extent_bit) + 4).min(125);
    1_i128 << shift
}

#[inline]
fn direct_cell_range(rect: WorldRect, extent_bit: u8) -> (i128, i128, i128, i128) {
    let span = direct_bucket_span(extent_bit);
    (rect.min_x.div_euclid(span), rect.max_x.div_euclid(span), rect.min_y.div_euclid(span), rect.max_y.div_euclid(span))
}

#[inline]
fn summary_cell_for_rect(rect: WorldRect, level: u8) -> SummaryKey {
    let span = SummaryKey::cell_span(level);
    let mid_x = rect.min_x + (rect.max_x - rect.min_x) / 2;
    let mid_y = rect.min_y + (rect.max_y - rect.min_y) / 2;
    SummaryKey { level, cx: mid_x.div_euclid(span), cy: mid_y.div_euclid(span) }
}

/// Immutable-friendly world scene index with O(1) incremental updates and
/// bounded sparse micro-summaries for cold-cache deep-zoom overviews.
#[derive(Clone)]
pub struct WorldSceneIndex {
    world: WorldState,
    entries: BTreeMap<SceneObject, IndexedEntry>,
    /// Octave-bucketed direct spatial grid: `(extent_bit, gx, gy) -> objects`.
    direct_grid: BTreeMap<(u8, i128, i128), Vec<SceneObject>>,
    /// Bit `b` set iff `direct_grid` has any visible object at `extent_bit == b`.
    populated_bits: u128,
    /// Sparse `8×8` micro-summary tree across levels `0..=MAX_SUMMARY_LEVEL`.
    summary_nodes: BTreeMap<SummaryKey, SummaryNode>,
    /// Dirty summary nodes pending bottom-up refresh (always flushed before sharing).
    dirty_summary: BTreeSet<SummaryKey>,
    total_references: usize,
}

type DevSummaryAccum = (u64, u64, u32, Vec<(u8, [f32; 4])>);

impl WorldSceneIndex {
    pub fn new(world: &WorldState) -> Result<Self> {
        world.validate().map_err(|e| err(e.to_string()))?;
        let mut idx = Self {
            world: world.clone(),
            entries: BTreeMap::new(),
            direct_grid: BTreeMap::new(),
            populated_bits: 0,
            summary_nodes: BTreeMap::new(),
            dirty_summary: BTreeSet::new(),
            total_references: 0,
        };
        for (&id, shared) in &world.strokes {
            idx.index_stroke(id, shared)?;
        }
        for (&id, shared) in &world.media {
            idx.index_media(id, shared)?;
        }
        idx.rebuild_dirty_summaries();
        Ok(idx)
    }

    pub fn world(&self) -> &WorldState {
        &self.world
    }

    pub fn indexed_references(&self) -> usize {
        self.total_references
    }

    pub fn entry(&self, obj: SceneObject) -> Option<&IndexedEntry> {
        self.entries.get(&obj)
    }

    /// Bounded lookup of any visible indexed objects intersecting `rect` regardless of
    /// scale/pitch (including sub-pixel deep-zoom objects summarized at overview scale).
    pub fn query_rect_any(&self, rect: WorldRect, limit: usize) -> Vec<SceneObject> {
        let mut out = Vec::new();
        for (&obj, entry) in &self.entries {
            if out.len() >= limit {
                break;
            }
            if entry.visible && entry.rect.intersects(&rect) {
                out.push(obj);
            }
        }
        out
    }

    /// Incrementally apply one `AcceptedOp` in O(1), updating direct cells and
    /// dirtying only `<= 33` summary ancestor cells. Returns the dirty world rect if visible art changed.
    pub fn apply_accepted(&mut self, event: &AcceptedOp) -> Result<Option<WorldRect>> {
        self.world.apply(event).map_err(|e| err(e.to_string()))?;
        let dirty = match &event.op.action {
            Edit::AddStroke { stroke } | Edit::CopyStroke { stroke, .. } => {
                if let Some(shared) = self.world.strokes.get(&stroke.id).cloned() {
                    let rect = self.index_stroke(stroke.id, &shared)?;
                    Some(rect)
                } else {
                    None
                }
            }
            Edit::SetStrokeVisible { stroke, visible } => self.set_object_visible(SceneObject::Stroke(*stroke), *visible, event.revision),
            Edit::PlaceMedia { media } => {
                if let Some(shared) = self.world.media.get(&media.id).cloned() {
                    let rect = self.index_media(media.id, &shared)?;
                    Some(rect)
                } else {
                    None
                }
            }
            Edit::SetMediaVisible { media, visible } => self.set_object_visible(SceneObject::Media(*media), *visible, event.revision),
            Edit::Rename { .. } | Edit::PutTour { .. } | Edit::RemoveTour { .. } => None,
        };
        if dirty.is_some() {
            self.rebuild_dirty_summaries();
        }
        Ok(dirty)
    }

    /// Incrementally advance or rewind this index to `target_rev` relative to `canonical`,
    /// returning all affected `WorldRect`s (powering catch-up playback without O(N) rebuilds).
    pub fn step_review_to(&mut self, canonical: &WorldState, target_rev: u64) -> Result<Vec<WorldRect>> {
        if canonical.canvas != self.world.canvas || target_rev > canonical.revision {
            return Err(err("Invalid incremental review target"));
        }
        let mut dirty = Vec::new();
        if target_rev == self.world.revision {
            return Ok(dirty);
        }
        if target_rev > self.world.revision {
            let count = usize::try_from(target_rev - self.world.revision).map_err(|_| err("Review step overflow"))?;
            let slice = canonical.events_after(self.world.revision, count).map_err(|e| err(e.to_string()))?;
            for event in slice {
                if let Some(rect) = self.apply_accepted(event)? {
                    dirty.push(rect);
                }
            }
            return Ok(dirty);
        }
        // Rewind: only undo the exact events in `target_rev..self.world.revision` if small,
        // or rebuild cleanly if rewinding a huge jump.
        let steps = self.world.revision.saturating_sub(target_rev);
        if steps > 1024 {
            let replayed = canonical.replay_to(target_rev).map_err(|e| err(e.to_string()))?;
            *self = Self::new(&replayed)?;
            let limit = coordinates::extent().map_err(|e| err(e.to_string()))?;
            dirty.push(WorldRect { min_x: -limit, min_y: -limit, max_x: limit - 1, max_y: limit - 1 });
            return Ok(dirty);
        }
        let replayed = canonical.replay_to(target_rev).map_err(|e| err(e.to_string()))?;
        let undone = canonical.events_after(target_rev, steps as usize).map_err(|e| err(e.to_string()))?;
        let mut touched_strokes = BTreeSet::new();
        let mut touched_media = BTreeSet::new();
        for event in undone {
            match &event.op.action {
                Edit::AddStroke { stroke } | Edit::CopyStroke { stroke, .. } => {
                    touched_strokes.insert(stroke.id);
                }
                Edit::SetStrokeVisible { stroke, .. } => {
                    touched_strokes.insert(*stroke);
                }
                Edit::PlaceMedia { media } => {
                    touched_media.insert(media.id);
                }
                Edit::SetMediaVisible { media, .. } => {
                    touched_media.insert(*media);
                }
                Edit::Rename { .. } | Edit::PutTour { .. } | Edit::RemoveTour { .. } => {}
            }
        }
        self.world = replayed;
        for id in touched_strokes {
            let obj = SceneObject::Stroke(id);
            if let Some(old) = self.entries.get(&obj).copied() {
                dirty.push(old.rect);
                self.remove_entry(obj);
            }
            if let Some(shared) = self.world.strokes.get(&id).cloned() {
                let rect = self.index_stroke(id, &shared)?;
                dirty.push(rect);
            }
        }
        for id in touched_media {
            let obj = SceneObject::Media(id);
            if let Some(old) = self.entries.get(&obj).copied() {
                dirty.push(old.rect);
                self.remove_entry(obj);
            }
            if let Some(shared) = self.world.media.get(&id).cloned() {
                let rect = self.index_media(id, &shared)?;
                dirty.push(rect);
            }
        }
        self.rebuild_dirty_summaries();
        Ok(dirty)
    }

    fn index_stroke(&mut self, id: StrokeId, shared: &SharedStroke) -> Result<WorldRect> {
        let rect = stroke_world_rect(&shared.stroke).map_err(|e| err(e.to_string()))?;
        let obj = SceneObject::Stroke(id);
        let extent_bit = floor_log2_u128(rect.max_extent_ticks());
        let leaf_level = (extent_bit / 3).min(MAX_SUMMARY_LEVEL);
        let entry = IndexedEntry {
            object: obj,
            rect,
            device: shared.device,
            created_rev: shared.created_revision,
            changed_rev: shared.changed_revision,
            extent_bit,
            leaf_level,
            visible: shared.visible,
        };
        self.insert_entry(entry);
        Ok(rect)
    }

    fn index_media(&mut self, id: MediaId, shared: &SharedMedia) -> Result<WorldRect> {
        let rect = media_world_rect(&shared.media).map_err(|e| err(e.to_string()))?;
        let obj = SceneObject::Media(id);
        let extent_bit = floor_log2_u128(rect.max_extent_ticks());
        let leaf_level = (extent_bit / 3).min(MAX_SUMMARY_LEVEL);
        let entry = IndexedEntry {
            object: obj,
            rect,
            device: shared.device,
            created_rev: shared.created_revision,
            changed_rev: shared.changed_revision,
            extent_bit,
            leaf_level,
            visible: shared.visible,
        };
        self.insert_entry(entry);
        Ok(rect)
    }

    fn insert_entry(&mut self, entry: IndexedEntry) {
        self.entries.insert(entry.object, entry);
        if entry.visible {
            self.add_to_grids(&entry);
        }
    }

    fn remove_entry(&mut self, obj: SceneObject) {
        if let Some(entry) = self.entries.remove(&obj)
            && entry.visible
        {
            self.remove_from_grids(&entry);
        }
    }

    fn set_object_visible(&mut self, obj: SceneObject, visible: bool, rev: u64) -> Option<WorldRect> {
        let mut entry = self.entries.get(&obj).copied()?;
        if entry.visible == visible {
            entry.changed_rev = rev;
            self.entries.insert(obj, entry);
            return None;
        }
        if entry.visible {
            self.remove_from_grids(&entry);
        }
        entry.visible = visible;
        entry.changed_rev = rev;
        self.entries.insert(obj, entry);
        if visible {
            self.add_to_grids(&entry);
        }
        Some(entry.rect)
    }

    fn add_to_grids(&mut self, entry: &IndexedEntry) {
        let (gx0, gx1, gy0, gy1) = direct_cell_range(entry.rect, entry.extent_bit);
        for gy in gy0..=gy1 {
            for gx in gx0..=gx1 {
                self.direct_grid.entry((entry.extent_bit, gx, gy)).or_default().push(entry.object);
                self.total_references = self.total_references.saturating_add(1);
            }
        }
        if entry.extent_bit < 128 {
            self.populated_bits |= 1_u128 << entry.extent_bit;
        }
        let leaf_key = summary_cell_for_rect(entry.rect, entry.leaf_level);
        self.summary_nodes.entry(leaf_key).or_default().leaves.insert(entry.object);
        self.mark_summary_chain_dirty(leaf_key, true);
    }

    fn remove_from_grids(&mut self, entry: &IndexedEntry) {
        let (gx0, gx1, gy0, gy1) = direct_cell_range(entry.rect, entry.extent_bit);
        for gy in gy0..=gy1 {
            for gx in gx0..=gx1 {
                let key = (entry.extent_bit, gx, gy);
                if let Some(vec) = self.direct_grid.get_mut(&key)
                    && let Some(pos) = vec.iter().position(|o| *o == entry.object)
                {
                    vec.swap_remove(pos);
                    self.total_references = self.total_references.saturating_sub(1);
                    if vec.is_empty() {
                        self.direct_grid.remove(&key);
                    }
                }
            }
        }
        let leaf_key = summary_cell_for_rect(entry.rect, entry.leaf_level);
        if let Some(node) = self.summary_nodes.get_mut(&leaf_key) {
            node.leaves.remove(&entry.object);
        }
        self.mark_summary_chain_dirty(leaf_key, false);
    }

    fn mark_summary_chain_dirty(&mut self, leaf: SummaryKey, adding: bool) {
        let mut cur = leaf;
        loop {
            let node = self.summary_nodes.entry(cur).or_default();
            node.dirty = true;
            self.dirty_summary.insert(cur);
            let Some((parent_key, sub_idx)) = cur.parent() else {
                break;
            };
            if adding {
                self.summary_nodes.entry(parent_key).or_default().child_mask |= 1_u64 << sub_idx;
            }
            cur = parent_key;
        }
    }

    fn rebuild_dirty_summaries(&mut self) {
        if self.dirty_summary.is_empty() {
            return;
        }
        // `SummaryKey` orders by `level` ascending, so draining a `BTreeSet` processes strictly bottom-up!
        let dirty: Vec<SummaryKey> = std::mem::take(&mut self.dirty_summary).into_iter().collect();
        for key in dirty {
            self.recompute_summary_node(key);
        }
    }

    fn recompute_summary_node(&mut self, key: SummaryKey) {
        let Some(existing) = self.summary_nodes.get(&key) else {
            return;
        };
        let leaves: Vec<SceneObject> = existing.leaves.iter().copied().collect();
        let old_mask = existing.child_mask;
        let mut new_mask = 0u64;
        let mut children_by_dev: BTreeMap<DeviceId, DevSummaryAccum> = BTreeMap::new();

        if key.level > 0 {
            let mut m = old_mask;
            while m != 0 {
                let sub = m.trailing_zeros() as u8;
                m &= m - 1;
                let Some(ckey) = key.child(sub) else { continue };
                let Some(cnode) = self.summary_nodes.get(&ckey) else { continue };
                if cnode.total_pixel.is_empty() {
                    continue;
                }
                new_mask |= 1_u64 << sub;
                for (&dev, &(min_r, max_r, count, px)) in &cnode.total_pixel {
                    let acc = children_by_dev.entry(dev).or_insert((u64::MAX, 0, 0, Vec::new()));
                    acc.0 = acc.0.min(min_r);
                    acc.1 = acc.1.max(max_r);
                    acc.2 = acc.2.saturating_add(count);
                    acc.3.push((sub, px));
                }
            }
        }

        let mut children_summary: BTreeMap<DeviceId, SummaryEpoch> = BTreeMap::new();
        for (dev, (min_rev, max_rev, object_count, cells)) in children_by_dev {
            children_summary.insert(dev, SummaryEpoch { min_rev, max_rev, object_count, cells });
        }

        // Compute total 1-pixel summary per device (combining `children_summary` + `leaves` in revision order).
        let mut total_pixel: BTreeMap<DeviceId, (u64, u64, u32, [f32; 4])> = BTreeMap::new();
        let mut devices: BTreeSet<DeviceId> = children_summary.keys().copied().collect();
        let mut leaf_items: Vec<(DeviceId, u64, SceneObject)> = Vec::with_capacity(leaves.len());
        for obj in &leaves {
            if let Some(e) = self.entries.get(obj)
                && e.visible
            {
                devices.insert(e.device);
                leaf_items.push((e.device, e.created_rev, *obj));
            }
        }
        leaf_items.sort_unstable();

        for dev in devices {
            let mut grid = [[0.0f32; 4]; 64];
            let mut min_rev = u64::MAX;
            let mut max_rev = 0u64;
            let mut count = 0u32;
            if let Some(ch) = children_summary.get(&dev) {
                for &(sub, px) in &ch.cells {
                    if let Some(slot) = grid.get_mut(usize::from(sub)) {
                        *slot = px;
                    }
                }
                min_rev = min_rev.min(ch.min_rev);
                max_rev = max_rev.max(ch.max_rev);
                count = count.saturating_add(ch.object_count);
            }
            for &(d, rev, obj) in &leaf_items {
                if d != dev {
                    continue;
                }
                min_rev = min_rev.min(rev);
                max_rev = max_rev.max(rev);
                count = count.saturating_add(1);
                self.splat_object_into_grid(obj, key, &mut grid);
            }
            let mut sum = [0.0f32; 4];
            for px in &grid {
                for c in 0..4 {
                    sum[c] += px[c];
                }
            }
            let avg = [sum[0] / 64.0, sum[1] / 64.0, sum[2] / 64.0, sum[3] / 64.0];
            if avg[3] > 0.0 || count > 0 {
                total_pixel.insert(dev, (min_rev, max_rev, count, avg));
            }
        }

        if leaves.is_empty() && new_mask == 0 {
            self.summary_nodes.remove(&key);
            if let Some((pkey, sub)) = key.parent()
                && let Some(pnode) = self.summary_nodes.get_mut(&pkey)
            {
                pnode.child_mask &= !(1_u64 << sub);
            }
        } else if let Some(node) = self.summary_nodes.get_mut(&key) {
            node.child_mask = new_mask;
            node.children_summary = children_summary;
            node.total_pixel = total_pixel;
            node.dirty = false;
        }
    }

    fn splat_object_into_grid(&self, obj: SceneObject, key: SummaryKey, grid: &mut [[f32; 4]; 64]) {
        let Some(entry) = self.entries.get(&obj) else {
            return;
        };
        let cell_rect = key.world_rect();
        let sub_span = SummaryKey::subcell_span(key.level);
        if sub_span <= 0 {
            return;
        }
        let sx0 = (entry.rect.min_x - cell_rect.min_x).div_euclid(sub_span).clamp(0, 7) as usize;
        let sx1 = (entry.rect.max_x - cell_rect.min_x).div_euclid(sub_span).clamp(0, 7) as usize;
        let sy0 = (entry.rect.min_y - cell_rect.min_y).div_euclid(sub_span).clamp(0, 7) as usize;
        let sy1 = (entry.rect.max_y - cell_rect.min_y).div_euclid(sub_span).clamp(0, 7) as usize;
        let sub_f = sub_span as f64;
        let coverage_ratio = ((entry.rect.max_extent_ticks() as f64) / sub_f).clamp(0.05, 1.0) as f32;

        match obj {
            SceneObject::Stroke(id) => {
                let Some(shared) = self.world.strokes.get(&id) else {
                    return;
                };
                let stroke = &shared.stroke;
                let opacity = stroke.brush.as_deref().map_or(stroke.color.rgba[3], |b| b.opacity * stroke.color.rgba[3]).clamp(0.0, 1.0);
                let behind = stroke.brush.as_deref().is_some_and(|b| b.behind && !stroke.erase);
                let k = (opacity * coverage_ratio).clamp(0.0, 1.0);
                for sy in sy0..=sy1 {
                    for sx in sx0..=sx1 {
                        let Some(dst) = grid.get_mut(sy * 8 + sx) else { continue };
                        if stroke.erase {
                            for ch in dst.iter_mut() {
                                *ch *= 1.0 - k;
                            }
                        } else if behind {
                            let inv = 1.0 - dst[3];
                            let sa = k;
                            dst[0] += stroke.color.rgba[0] * sa * inv;
                            dst[1] += stroke.color.rgba[1] * sa * inv;
                            dst[2] += stroke.color.rgba[2] * sa * inv;
                            dst[3] += sa * inv;
                        } else {
                            let inv = 1.0 - k;
                            dst[0] = stroke.color.rgba[0] * k + dst[0] * inv;
                            dst[1] = stroke.color.rgba[1] * k + dst[1] * inv;
                            dst[2] = stroke.color.rgba[2] * k + dst[2] * inv;
                            dst[3] = k + dst[3] * inv;
                        }
                    }
                }
            }
            SceneObject::Media(_) => {
                // Neutral subtle placeholder color for sub-pixel media clusters until zoomed closer.
                let k = (0.5 * coverage_ratio).clamp(0.0, 1.0);
                let inv = 1.0 - k;
                for sy in sy0..=sy1 {
                    for sx in sx0..=sx1 {
                        let Some(dst) = grid.get_mut(sy * 8 + sx) else { continue };
                        dst[0] = 0.5 * k + dst[0] * inv;
                        dst[1] = 0.5 * k + dst[1] * inv;
                        dst[2] = 0.5 * k + dst[2] * inv;
                        dst[3] = k + dst[3] * inv;
                    }
                }
            }
        }
    }

    /// Query all direct objects AND bounded sub-pixel micro-summaries intersecting `tile.physical_rect()`.
    /// Direct and summary sets are strictly disjoint (`extent_bit >= cut_level * 3` vs `< cut_level * 3`).
    pub fn query_tile(&self, tile: TileKey) -> TileQuery {
        self.query_rect_at_pitch(tile.physical_rect(), pixel_ticks(tile.lod))
    }

    /// Query all direct objects AND bounded sub-pixel micro-summaries intersecting `rect`
    /// at pixel pitch `pixel_pitch_ticks` (used by both world tiles and lifted-ceiling CPU exports).
    pub fn query_rect_at_pitch(&self, rect: WorldRect, pixel_pitch_ticks: i128) -> TileQuery {
        let tile_bit = floor_log2_u128(pixel_pitch_ticks.max(1) as u128);
        let cut_level = (tile_bit / 3).min(MAX_SUMMARY_LEVEL);
        let direct_min_bit = cut_level.saturating_mul(3);

        // 1. Query octave-bucketed direct spatial grid for `extent_bit >= direct_min_bit`.
        let mut seen = BTreeSet::new();
        let mut direct = Vec::new();
        let mut bits = self.populated_bits >> direct_min_bit;
        let mut bit = direct_min_bit;
        while bits != 0 {
            let tz = bits.trailing_zeros() as u8;
            bit = bit.saturating_add(tz);
            bits >>= tz;
            let (gx0, gx1, gy0, gy1) = direct_cell_range(rect, bit);
            for gy in gy0..=gy1 {
                for gx in gx0..=gx1 {
                    if let Some(list) = self.direct_grid.get(&(bit, gx, gy)) {
                        for &obj in list {
                            if let Some(e) = self.entries.get(&obj)
                                && e.visible
                                && e.rect.intersects(&rect)
                                && seen.insert(obj)
                            {
                                direct.push(obj);
                            }
                        }
                    }
                }
            }
            bits >>= 1;
            bit = bit.saturating_add(1);
        }

        // 2. Bounded sparse walk for sub-pixel objects (`leaf_level < cut_level`).
        let mut summaries = Vec::new();
        let mut summary_nodes_visited = 0usize;
        if cut_level > 0 {
            let start_level = cut_level.saturating_add(2).min(MAX_SUMMARY_LEVEL);
            let span = SummaryKey::cell_span(start_level);
            let cx0 = rect.min_x.div_euclid(span);
            let cx1 = rect.max_x.div_euclid(span);
            let cy0 = rect.min_y.div_euclid(span);
            let cy1 = rect.max_y.div_euclid(span);
            let mut frontier = VecDeque::new();
            for cy in cy0..=cy1 {
                for cx in cx0..=cx1 {
                    let key = SummaryKey { level: start_level, cx, cy };
                    if self.summary_nodes.contains_key(&key) {
                        frontier.push_back(key);
                    }
                }
            }
            while let Some(key) = frontier.pop_front() {
                let Some(node) = self.summary_nodes.get(&key) else { continue };
                summary_nodes_visited += 1;
                if key.level == cut_level || summary_nodes_visited + frontier.len() >= MAX_SUMMARY_NODES_PER_TILE {
                    if key.level == cut_level {
                        for (&dev, epoch) in &node.children_summary {
                            summaries.push((key, dev, epoch.clone()));
                        }
                    } else {
                        self.collect_capped_subpixel_summary(key, cut_level, &mut summaries);
                    }
                    continue;
                }
                let mut m = node.child_mask;
                while m != 0 {
                    let sub = m.trailing_zeros() as u8;
                    m &= m - 1;
                    if let Some(child) = key.child(sub)
                        && child.world_rect().intersects(&rect)
                        && self.summary_nodes.contains_key(&child)
                    {
                        frontier.push_back(child);
                    }
                }
            }
        }

        TileQuery { direct, summaries, summary_nodes_visited }
    }

    fn collect_capped_subpixel_summary(&self, key: SummaryKey, cut_level: u8, out: &mut Vec<(SummaryKey, DeviceId, SummaryEpoch)>) {
        // When the tile has > 512 populated cells at `cut_level`, emit `key`'s cached
        // `children_summary` only if `key.level == cut_level` or aggregate down safely without
        // including objects at `leaf_level >= cut_level`.
        let Some(node) = self.summary_nodes.get(&key) else {
            return;
        };
        if key.level <= cut_level {
            for (&dev, epoch) in &node.children_summary {
                out.push((key, dev, epoch.clone()));
            }
            return;
        }
        // Build an 8×8 summary over `key` using only children whose subtree contains `< cut_level` objects.
        let mut by_dev: BTreeMap<DeviceId, DevSummaryAccum> = BTreeMap::new();
        let mut m = node.child_mask;
        while m != 0 {
            let sub = m.trailing_zeros() as u8;
            m &= m - 1;
            let Some(child) = key.child(sub) else { continue };
            let Some(cnode) = self.summary_nodes.get(&child) else { continue };
            for (&dev, epoch) in &cnode.children_summary {
                let px = epoch.average_pixel();
                if px[3] > 0.0 {
                    let acc = by_dev.entry(dev).or_insert((u64::MAX, 0, 0, Vec::new()));
                    acc.0 = acc.0.min(epoch.min_rev);
                    acc.1 = acc.1.max(epoch.max_rev);
                    acc.2 = acc.2.saturating_add(epoch.object_count);
                    acc.3.push((sub, px));
                }
            }
        }
        for (dev, (min_rev, max_rev, object_count, cells)) in by_dev {
            out.push((key, dev, SummaryEpoch { min_rev, max_rev, object_count, cells }));
        }
    }

    /// Split a summary cell's sub-pixel objects on `device` around `split_revs` when one
    /// or more direct strokes on the same device straddle `(epoch.min_rev, epoch.max_rev)`
    /// (preserving exact W4 chronological order for interleaved coarse/fine strokes & erases).
    pub fn split_summary_epochs(&self, key: SummaryKey, device: DeviceId, split_revs: &[u64]) -> Vec<SummaryEpoch> {
        let mut objs = Vec::new();
        self.collect_subtree_objects(key, device, &mut objs, MAX_INTERLEAVED_EXPAND);
        objs.sort_unstable_by_key(|&(rev, obj)| (rev, obj));
        let num_buckets = split_revs.len() + 1;
        let mut buckets: Vec<Vec<(u64, SceneObject)>> = vec![Vec::new(); num_buckets];
        for (rev, obj) in objs {
            let b = split_revs.partition_point(|&s| s <= rev);
            if let Some(slot) = buckets.get_mut(b) {
                slot.push((rev, obj));
            }
        }
        let mut out = Vec::new();
        for bucket in buckets {
            if bucket.is_empty() {
                continue;
            }
            let min_rev = bucket.first().map_or(0, |x| x.0);
            let max_rev = bucket.last().map_or(0, |x| x.0);
            let mut grid = [[0.0f32; 4]; 64];
            for &(_, obj) in &bucket {
                self.splat_object_into_grid(obj, key, &mut grid);
            }
            let cells: Vec<(u8, [f32; 4])> = grid.iter().enumerate().filter(|(_, px)| px[3] > 0.0).map(|(i, &px)| (i as u8, px)).collect();
            if !cells.is_empty() {
                out.push(SummaryEpoch { min_rev, max_rev, object_count: bucket.len() as u32, cells });
            }
        }
        out
    }

    fn collect_subtree_objects(&self, root: SummaryKey, device: DeviceId, out: &mut Vec<(u64, SceneObject)>, limit: usize) {
        let mut stack = Vec::new();
        if let Some(node) = self.summary_nodes.get(&root) {
            let mut m = node.child_mask;
            while m != 0 {
                let sub = m.trailing_zeros() as u8;
                m &= m - 1;
                if let Some(child) = root.child(sub) {
                    stack.push(child);
                }
            }
        }
        while let Some(k) = stack.pop() {
            if out.len() >= limit {
                return;
            }
            let Some(node) = self.summary_nodes.get(&k) else { continue };
            for &obj in &node.leaves {
                if let Some(e) = self.entries.get(&obj)
                    && e.visible
                    && e.device == device
                {
                    out.push((e.created_rev, obj));
                    if out.len() >= limit {
                        return;
                    }
                }
            }
            let mut m = node.child_mask;
            while m != 0 {
                let sub = m.trailing_zeros() as u8;
                m &= m - 1;
                if let Some(child) = k.child(sub) {
                    stack.push(child);
                }
            }
        }
    }

    /// Query all visible objects intersecting a viewport world rect (used for media frame discovery).
    pub fn query_world_rect_media(&self, rect: WorldRect) -> Vec<MediaId> {
        let mut out = Vec::new();
        for (&obj, entry) in &self.entries {
            if let SceneObject::Media(id) = obj
                && entry.visible
                && entry.rect.intersects(&rect)
            {
                out.push(id);
            }
        }
        out
    }

    /// Compute a stable content fingerprint for one world tile (used to key the optional local disk tile cache).
    pub fn tile_content_hash(&self, tile: TileKey, query: &TileQuery) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(b"komaphone-world-tile-v1:");
        h.update(self.world.canvas.as_str().as_bytes());
        h.update(&[tile.lod]);
        h.update(&tile.tx.to_le_bytes());
        h.update(&tile.ty.to_le_bytes());
        h.update(&(TILE_LOGICAL).to_le_bytes());
        for &obj in &query.direct {
            if let Some(e) = self.entries.get(&obj) {
                match obj {
                    SceneObject::Stroke(id) => {
                        h.update(&[1u8]);
                        h.update(id.as_str().as_bytes());
                    }
                    SceneObject::Media(id) => {
                        h.update(&[2u8]);
                        h.update(id.as_str().as_bytes());
                    }
                }
                h.update(&e.changed_rev.to_le_bytes());
            }
        }
        for (skey, dev, epoch) in &query.summaries {
            h.update(&[3u8, skey.level]);
            h.update(&skey.cx.to_le_bytes());
            h.update(&skey.cy.to_le_bytes());
            h.update(dev.as_str().as_bytes());
            h.update(&epoch.min_rev.to_le_bytes());
            h.update(&epoch.max_rev.to_le_bytes());
            h.update(&epoch.object_count.to_le_bytes());
            for &(idx, px) in &epoch.cells {
                h.update(&[idx]);
                for ch in px {
                    h.update(&ch.to_bits().to_le_bytes());
                }
            }
        }
        *h.finalize().as_bytes()
    }
}
