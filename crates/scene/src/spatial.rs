//! Exact signed-128-bit depth-aligned world-space tile coordinates, LOD selection,
//! and expanded object bounding boxes across depths `0..=12`.
//!
//! Within each decimal authoring depth `d ∈ 0..=12`, LOD levels step by exact powers
//! of 2 (`local_unit(d) / 64 ..= local_unit(d)`), so standard zooms (`0.5×, 1×, 2×,
//! 4×, 8×, 16×, 32×, 64×`) and `1× / 2×` DPIs achieve exact 1:1 pixel-center lock at
//! every depth. Across depth boundaries the step is `100 / 64 = 1.5625×` (`< 2.0×`).
//!
//! Pure L1 math (no OS/UI/thread dependencies; 100% WASM-compatible).
use crate::coordinates::{center, extent, local_unit, point, validate_view};
use crate::shared::MediaPlacement;
use crate::{CHILD_SCALE, Error, LOCAL_SUBPIXELS, MAX_DEPTH, Result, Stroke, Viewport};

/// Logical interior width/height of a world-space tile in pixels.
pub const TILE_LOGICAL: u32 = 256;
/// Directly rasterized border apron on all four sides of each tile (pixels).
/// Guarantees bilinear filtering stays strictly inside directly rasterized texels.
pub const TILE_APRON: u32 = 2;
/// Stored physical tile dimension (`256 + 2 * 2 = 260`).
pub const TILE_PHYSICAL: u32 = TILE_LOGICAL + 2 * TILE_APRON;
/// Total number of LOD levels (`11` at depth 12 + `12 * 7` across depths 11..=0 + `3` overview levels).
pub const TILE_LOD_COUNT: usize = 98;
/// Maximum valid LOD level index (`0..=97`).
pub const MAX_TILE_LOD: u8 = (TILE_LOD_COUNT - 1) as u8;

/// Reconciled desktop GPU tile-array capacity (`256` layers = `66.02 MiB` VRAM):
/// `144` visible + `32` fallback + `32` dynamic + `48` prefetch/LRU = `256`.
pub const GPU_LAYERS_DESKTOP: u32 = 256;
pub const MAX_VISIBLE_TILES_DESKTOP: usize = 144;

/// Reconciled Safe-GPU / conservative GPU tile-array capacity (`128` layers = `33.01 MiB` VRAM):
/// `72` visible + `16` fallback + `16` dynamic + `24` prefetch/LRU = `128`.
pub const GPU_LAYERS_CONSERVATIVE: u32 = 128;
pub const MAX_VISIBLE_TILES_CONSERVATIVE: usize = 72;

/// Zoom hysteresis band (in octaves) applied when continuously zooming to prevent
/// rapid LOD switching at exact level boundaries.
const LOD_HYSTERESIS_OCTAVES: f64 = 0.10;

/// Exact world-tick pixel pitches for every LOD `0..=97`.
const LOD_PIXEL_TICKS: [i128; TILE_LOD_COUNT] = build_lod_table();

const fn build_lod_table() -> [i128; TILE_LOD_COUNT] {
    let mut table = [1_i128; TILE_LOD_COUNT];
    // Depth 12 (`local_unit(12) = 1024`): exact powers of 2 from 1 to 1024 ticks/px.
    let mut i = 0usize;
    while i <= 10 {
        table[i] = 1_i128 << i;
        i += 1;
    }
    // Depths 11 down to 0: exact dyadic divisions `local_unit(d) / {64, 32, 16, 8, 4, 2, 1}`.
    let mut unit = LOCAL_SUBPIXELS;
    let mut depth_from_bottom = 1u32;
    let mut idx = 11usize;
    while depth_from_bottom <= MAX_DEPTH as u32 {
        unit *= CHILD_SCALE;
        table[idx] = unit / 64;
        table[idx + 1] = unit / 32;
        table[idx + 2] = unit / 16;
        table[idx + 3] = unit / 8;
        table[idx + 4] = unit / 4;
        table[idx + 5] = unit / 2;
        table[idx + 6] = unit;
        idx += 7;
        depth_from_bottom += 1;
    }
    // Overview levels above depth 0 (`0.5×`, `0.25×`, `0.125×`).
    table[idx] = unit * 2;
    table[idx + 1] = unit * 4;
    table[idx + 2] = unit * 8;
    table
}

/// Exact world-space axis-aligned bounding box in signed 128-bit ticks (`[min_x, max_x] × [min_y, max_y]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WorldRect {
    pub min_x: i128,
    pub min_y: i128,
    pub max_x: i128,
    pub max_y: i128,
}

impl WorldRect {
    pub fn new(min_x: i128, min_y: i128, max_x: i128, max_y: i128) -> Self {
        Self { min_x: min_x.min(max_x), min_y: min_y.min(max_y), max_x: min_x.max(max_x), max_y: min_y.max(max_y) }
    }

    #[inline]
    pub fn intersects(&self, other: &Self) -> bool {
        self.min_x <= other.max_x && self.max_x >= other.min_x && self.min_y <= other.max_y && self.max_y >= other.min_y
    }

    pub fn intersection(&self, other: &Self) -> Option<Self> {
        let min_x = self.min_x.max(other.min_x);
        let min_y = self.min_y.max(other.min_y);
        let max_x = self.max_x.min(other.max_x);
        let max_y = self.max_y.min(other.max_y);
        (min_x < max_x && min_y < max_y).then_some(Self { min_x, min_y, max_x, max_y })
    }

    pub fn union(&self, other: &Self) -> Self {
        Self { min_x: self.min_x.min(other.min_x), min_y: self.min_y.min(other.min_y), max_x: self.max_x.max(other.max_x), max_y: self.max_y.max(other.max_y) }
    }

    pub fn expand(&self, margin: i128) -> Self {
        let m = margin.max(0);
        Self {
            min_x: self.min_x.saturating_sub(m),
            min_y: self.min_y.saturating_sub(m),
            max_x: self.max_x.saturating_add(m),
            max_y: self.max_y.saturating_add(m),
        }
    }

    /// Maximum side length in ticks (`>= 1`).
    pub fn max_extent_ticks(&self) -> u128 {
        let dx = self.max_x.abs_diff(self.min_x);
        let dy = self.max_y.abs_diff(self.min_y);
        dx.max(dy).max(1)
    }

    /// Largest LOD `0..=MAX_TILE_LOD` whose `pixel_ticks(lod) <= max_extent_ticks`.
    pub fn extent_lod(&self) -> u8 {
        let ext = self.max_extent_ticks().min(i128::MAX as u128) as i128;
        match LOD_PIXEL_TICKS.binary_search(&ext) {
            Ok(i) => i as u8,
            Err(i) => i.saturating_sub(1) as u8,
        }
    }
}

/// Exact world ticks spanned by one tile pixel at `lod`.
#[inline]
pub fn pixel_ticks(lod: u8) -> i128 {
    LOD_PIXEL_TICKS.get(usize::from(lod)).copied().unwrap_or(LOD_PIXEL_TICKS[TILE_LOD_COUNT - 1])
}

/// Exact world ticks spanned by one logical `256×256` tile at `lod`.
#[inline]
pub fn tile_span(lod: u8) -> i128 {
    pixel_ticks(lod).saturating_mul(i128::from(TILE_LOGICAL))
}

/// Stable world-space tile identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TileKey {
    pub lod: u8,
    pub tx: i128,
    pub ty: i128,
}

impl TileKey {
    pub fn new(lod: u8, tx: i128, ty: i128) -> Result<Self> {
        if lod > MAX_TILE_LOD {
            return Err(Error::Limit("tile LOD"));
        }
        Ok(Self { lod, tx, ty })
    }

    /// Exact logical world-tick rectangle `[tx * span, (tx + 1) * span]`.
    pub fn logical_rect(self) -> WorldRect {
        let span = tile_span(self.lod);
        let min_x = self.tx.saturating_mul(span);
        let min_y = self.ty.saturating_mul(span);
        WorldRect { min_x, min_y, max_x: min_x.saturating_add(span), max_y: min_y.saturating_add(span) }
    }

    /// World-tick rectangle covered by all `260×260` physical texels (including the `2 px` apron).
    pub fn physical_rect(self) -> WorldRect {
        let apron_ticks = pixel_ticks(self.lod).saturating_mul(i128::from(TILE_APRON));
        self.logical_rect().expand(apron_ticks)
    }

    /// Exact center of this tile in world ticks (`(tx * 256 + 128) * pixel_ticks(lod)`).
    pub fn center_ticks(self) -> [i128; 2] {
        let span = tile_span(self.lod);
        let half = span / 2;
        [self.tx.saturating_mul(span).saturating_add(half), self.ty.saturating_mul(span).saturating_add(half)]
    }

    /// Exact `2:1` dyadic parent when `lod + 1` has `2×` pixel pitch; `None` at a
    /// depth-transition (`1.5625×`) boundary or at `MAX_TILE_LOD`.
    pub fn dyadic_parent(self) -> Option<Self> {
        if self.lod >= MAX_TILE_LOD || pixel_ticks(self.lod + 1) != pixel_ticks(self.lod).saturating_mul(2) {
            return None;
        }
        Some(Self { lod: self.lod + 1, tx: self.tx.div_euclid(2), ty: self.ty.div_euclid(2) })
    }

    /// Exact `2:1` dyadic children when `lod - 1` has `0.5×` pixel pitch.
    pub fn dyadic_children(self) -> Option<[Self; 4]> {
        if self.lod == 0 || pixel_ticks(self.lod) != pixel_ticks(self.lod - 1).saturating_mul(2) {
            return None;
        }
        let lod = self.lod - 1;
        let bx = self.tx.checked_mul(2)?;
        let by = self.ty.checked_mul(2)?;
        Some([
            Self { lod, tx: bx, ty: by },
            Self { lod, tx: bx.checked_add(1)?, ty: by },
            Self { lod, tx: bx, ty: by.checked_add(1)? },
            Self { lod, tx: bx.checked_add(1)?, ty: by.checked_add(1)? },
        ])
    }

    /// Compute the exact UV sub-rectangle `[u0, v0, u1, v1]` of a `world_subrect`
    /// (which must lie within `self.logical_rect()`) inside this tile's `260×260`
    /// texture, strictly within `[2/260, 258/260]`.
    pub fn uv_for_subrect(self, subrect: WorldRect) -> Option<[f32; 4]> {
        let rect = self.logical_rect();
        let clipped = rect.intersection(&subrect)?;
        let span = tile_span(self.lod) as f64;
        if span <= 0.0 {
            return None;
        }
        let fx0 = ((clipped.min_x - rect.min_x) as f64 / span).clamp(0.0, 1.0);
        let fy0 = ((clipped.min_y - rect.min_y) as f64 / span).clamp(0.0, 1.0);
        let fx1 = ((clipped.max_x - rect.min_x) as f64 / span).clamp(0.0, 1.0);
        let fy1 = ((clipped.max_y - rect.min_y) as f64 / span).clamp(0.0, 1.0);
        let phys = f64::from(TILE_PHYSICAL);
        let apron = f64::from(TILE_APRON);
        let logical = f64::from(TILE_LOGICAL);
        Some([
            ((apron + fx0 * logical) / phys) as f32,
            ((apron + fy0 * logical) / phys) as f32,
            ((apron + fx1 * logical) / phys) as f32,
            ((apron + fy1 * logical) / phys) as f32,
        ])
    }
}

/// Partition `target.logical_rect()` across overlapping tiles at `fallback_lod`
/// (`fallback_lod > target.lod` for coarse ancestor fallback, or `fallback_lod < target.lod`
/// for resident finer children). Returns `(tile_key, clipped_world_rect, uv_in_tile)`.
/// Handles both same-depth `2:1` steps and cross-depth `1.5625:1` boundaries seamlessly.
pub fn fallback_quads(target: TileKey, fallback_lod: u8, max_quads: usize) -> Vec<(TileKey, WorldRect, [f32; 4])> {
    if fallback_lod > MAX_TILE_LOD || fallback_lod == target.lod || max_quads == 0 {
        return Vec::new();
    }
    let rect = target.logical_rect();
    // Exclusive upper bound (`max_x - 1`, `max_y - 1`) prevents touching an adjacent tile at exact edges.
    let inclusive = WorldRect {
        min_x: rect.min_x,
        min_y: rect.min_y,
        max_x: rect.max_x.saturating_sub(1).max(rect.min_x),
        max_y: rect.max_y.saturating_sub(1).max(rect.min_y),
    };
    let count = tile_count_for_rect(inclusive, fallback_lod);
    if count == 0 || count > max_quads {
        return Vec::new();
    }
    let (tx0, tx1, ty0, ty1) = tile_bounds_for_rect(inclusive, fallback_lod);
    let mut out = Vec::with_capacity(count);
    for ty in ty0..=ty1 {
        for tx in tx0..=tx1 {
            let key = TileKey { lod: fallback_lod, tx, ty };
            if let Some(clipped) = rect.intersection(&key.logical_rect())
                && let Some(uv) = key.uv_for_subrect(clipped)
            {
                out.push((key, clipped, uv));
            }
        }
    }
    out
}

/// Exact world-tick bounds visible in `view`, clamped to the valid world `extent()`.
pub fn viewport_world_rect(view: &Viewport) -> Result<WorldRect> {
    validate_view(view)?;
    let c = center(&view.camera)?;
    let unit = local_unit(view.camera.region.depth)? as f64 / view.camera.zoom;
    let limit = extent()?;
    let hx = (f64::from(view.width) * 0.5 * unit).ceil().min(limit as f64) as i128;
    let hy = (f64::from(view.height) * 0.5 * unit).ceil().min(limit as f64) as i128;
    Ok(WorldRect {
        min_x: c.x.0.saturating_sub(hx).max(-limit),
        min_y: c.y.0.saturating_sub(hy).max(-limit),
        max_x: c.x.0.saturating_add(hx).min(limit - 1),
        max_y: c.y.0.saturating_add(hy).min(limit - 1),
    })
}

/// Inclusive tile coordinate bounds `(tx0, tx1, ty0, ty1)` covering `rect` at `lod`.
pub fn tile_bounds_for_rect(rect: WorldRect, lod: u8) -> (i128, i128, i128, i128) {
    let span = tile_span(lod);
    (rect.min_x.div_euclid(span), rect.max_x.div_euclid(span), rect.min_y.div_euclid(span), rect.max_y.div_euclid(span))
}

/// Exact number of tiles covering `rect` at `lod` (`usize::MAX` on overflow).
pub fn tile_count_for_rect(rect: WorldRect, lod: u8) -> usize {
    let (tx0, tx1, ty0, ty1) = tile_bounds_for_rect(rect, lod);
    let nx = tx1.saturating_sub(tx0).saturating_add(1);
    let ny = ty1.saturating_sub(ty0).saturating_add(1);
    if nx <= 0 || ny <= 0 {
        return 0;
    }
    nx.checked_mul(ny).and_then(|n| usize::try_from(n).ok()).unwrap_or(usize::MAX)
}

/// Select the target LOD for `(view, density)` reconciled strictly against `max_visible_tiles`
/// (`144` for the 256-layer desktop budget, `72` for the 128-layer Safe-GPU/conservative budget).
///
/// Selects the finest LOD with `pixel_ticks(lod) <= tpp * (1 + 1e-9)`, giving exact 1:1
/// pixel-center lock at standard dyadic zooms (`0.5×, 1×, 2×, 4×, 8×, 16×, 32×, 64×`)
/// and `[1.0×, 2.0×)` supersampling at arbitrary fractional zooms.
pub fn select_target_lod(view: &Viewport, density: f32, max_visible_tiles: usize, previous_lod: Option<u8>) -> Result<u8> {
    validate_view(view)?;
    if !density.is_finite() || !(0.5..=4.0).contains(&density) || max_visible_tiles == 0 {
        return Err(Error::Invalid("tile LOD parameters"));
    }
    let unit = local_unit(view.camera.region.depth)? as f64;
    let tpp = unit / (view.camera.zoom * f64::from(density));
    if !tpp.is_finite() || tpp <= 0.0 {
        return Err(Error::Invalid("tile pixel scale"));
    }
    let threshold = tpp * (1.0 + 1e-9);
    let mut lod = 0u8;
    for (idx, &pt) in LOD_PIXEL_TICKS.iter().enumerate() {
        if (pt as f64) <= threshold {
            lod = idx as u8;
        } else {
            break;
        }
    }
    if let Some(prev) = previous_lod
        && prev <= MAX_TILE_LOD
        && prev != lod
    {
        let log_tpp = tpp.log2();
        let prev_log = (pixel_ticks(prev) as f64).log2();
        let lod_log = (pixel_ticks(lod) as f64).log2();
        if (prev == lod.saturating_add(1) && log_tpp >= prev_log - LOD_HYSTERESIS_OCTAVES)
            || (lod == prev.saturating_add(1) && log_tpp <= lod_log + LOD_HYSTERESIS_OCTAVES)
        {
            lod = prev;
        }
    }
    let rect = viewport_world_rect(view)?;
    while lod < MAX_TILE_LOD && tile_count_for_rect(rect, lod) > max_visible_tiles {
        lod += 1;
    }
    Ok(lod)
}

/// Return all visible `TileKey`s covering `view` at `lod`, sorted by proximity to viewport center.
pub fn visible_tiles(view: &Viewport, lod: u8, max_tiles: usize) -> Result<Vec<TileKey>> {
    if lod > MAX_TILE_LOD {
        return Err(Error::Limit("tile LOD"));
    }
    let rect = viewport_world_rect(view)?;
    let count = tile_count_for_rect(rect, lod);
    if count > max_tiles {
        return Err(Error::Limit("visible tiles"));
    }
    let (tx0, tx1, ty0, ty1) = tile_bounds_for_rect(rect, lod);
    let c = center(&view.camera)?;
    let span = tile_span(lod);
    let ctx = c.x.0.div_euclid(span);
    let cty = c.y.0.div_euclid(span);
    let mut out = Vec::with_capacity(count);
    for ty in ty0..=ty1 {
        for tx in tx0..=tx1 {
            out.push(TileKey { lod, tx, ty });
        }
    }
    out.sort_by_key(|k| {
        let dx = k.tx.abs_diff(ctx);
        let dy = k.ty.abs_diff(cty);
        (dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy)), k.ty, k.tx)
    });
    Ok(out)
}

/// Project any `WorldRect` into logical screen pixels `[x0, y0, x1, y1]`.
pub fn world_rect_to_screen(rect: WorldRect, view: &Viewport) -> Result<[f32; 4]> {
    validate_view(view)?;
    let c = center(&view.camera)?;
    let scale = view.camera.zoom / local_unit(view.camera.region.depth)? as f64;
    let half_w = f64::from(view.width) * 0.5;
    let half_h = f64::from(view.height) * 0.5;
    let x0 = (rect.min_x - c.x.0) as f64 * scale + half_w;
    let y0 = (rect.min_y - c.y.0) as f64 * scale + half_h;
    let x1 = (rect.max_x - c.x.0) as f64 * scale + half_w;
    let y1 = (rect.max_y - c.y.0) as f64 * scale + half_h;
    Ok([x0 as f32, y0 as f32, x1 as f32, y1 as f32])
}

/// Project a tile's logical `[0, 256]` rectangle into logical screen pixels `[x0, y0, x1, y1]`.
pub fn tile_screen_rect(tile: TileKey, view: &Viewport) -> Result<[f32; 4]> {
    world_rect_to_screen(tile.logical_rect(), view)
}

/// Outward-rounded world-tick AABB of a `Stroke`, including soft brush, scattering,
/// dual brush, and wet-edge reach margin.
pub fn stroke_world_rect(stroke: &Stroke) -> Result<WorldRect> {
    if stroke.points.is_empty() {
        return Err(Error::Invalid("stroke has no points"));
    }
    let mut min_x = i128::MAX;
    let mut min_y = i128::MAX;
    let mut max_x = i128::MIN;
    let mut max_y = i128::MIN;
    for p in &stroke.points {
        let wp = point(&stroke.anchor, *p)?;
        min_x = min_x.min(wp.x.0);
        min_y = min_y.min(wp.y.0);
        max_x = max_x.max(wp.x.0);
        max_y = max_y.max(wp.y.0);
    }
    let reach = stroke.brush.as_ref().map_or(1.0, |b| b.reach_factor().max(1.0));
    let unit = local_unit(stroke.anchor.depth)?;
    let radius_f = (unit as f64) * (stroke.width * 0.5 * reach * 1.15);
    let radius = if radius_f.is_finite() && radius_f > 0.0 { (radius_f.ceil() as i128).saturating_add(unit / 4 + 2) } else { unit };
    let limit = extent()?;
    Ok(WorldRect {
        min_x: min_x.saturating_sub(radius).max(-limit),
        min_y: min_y.saturating_sub(radius).max(-limit),
        max_x: max_x.saturating_add(radius).min(limit - 1),
        max_y: max_y.saturating_add(radius).min(limit - 1),
    })
}

/// Exact world-tick AABB of a `MediaPlacement`.
pub fn media_world_rect(media: &MediaPlacement) -> Result<WorldRect> {
    let a = point(&media.anchor, media.offset)?;
    let b = point(&media.anchor, [media.offset[0] + media.width, media.offset[1] + media.height])?;
    Ok(WorldRect::new(a.x.0, a.y.0, b.x.0, b.y.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CanvasId, Point, Wide, coordinates};

    fn cid() -> CanvasId {
        CanvasId::new("11111111111111111111111111111111").unwrap()
    }

    #[test]
    fn lod_table_is_strictly_increasing_bounded_and_exact_at_all_depths() {
        for pair in LOD_PIXEL_TICKS.windows(2) {
            assert!(pair[0] > 0 && pair[1] > pair[0]);
            let ratio = pair[1] as f64 / pair[0] as f64;
            assert!((1.5..=2.0001).contains(&ratio), "unexpected step ratio {ratio}");
        }
        assert!(tile_span(MAX_TILE_LOD) < extent().unwrap());
        for depth in 0..=MAX_DEPTH {
            let unit = local_unit(depth).unwrap();
            assert!(LOD_PIXEL_TICKS.contains(&unit), "depth {depth} local_unit missing from LOD table");
            for div in [2_i128, 4, 8, 16, 32, 64] {
                assert!(LOD_PIXEL_TICKS.contains(&(unit / div)), "depth {depth} unit/{div} missing");
            }
            let cam = coordinates::target(cid(), Point::default(), depth, 1.0).unwrap();
            let view = Viewport { camera: cam, width: 1280, height: 720 };
            let lod = select_target_lod(&view, 1.0, MAX_VISIBLE_TILES_DESKTOP, None).unwrap();
            assert_eq!(pixel_ticks(lod), unit, "depth {depth} zoom=1.0 must lock 1:1 to local_unit");
        }
    }

    #[test]
    fn negative_coordinates_and_cross_depth_fallback_cover_target_exactly() {
        for lod in [0u8, 10, 11, 52, 53, 94, MAX_TILE_LOD - 1] {
            for tx in [-3_i128, -2, -1, 0, 1, 2] {
                for ty in [-3_i128, -1, 0, 1] {
                    let t = TileKey::new(lod, tx, ty).unwrap();
                    let min_uv = TILE_APRON as f32 / TILE_PHYSICAL as f32;
                    let max_uv = (TILE_APRON + TILE_LOGICAL) as f32 / TILE_PHYSICAL as f32;
                    let quads = fallback_quads(t, lod + 1, 8);
                    assert!((1..=4).contains(&quads.len()), "lod={lod} tx={tx} ty={ty} got {} quads", quads.len());
                    let mut total_area = 0u128;
                    let expected_span = tile_span(lod) as u128;
                    for (_, rect, uv) in &quads {
                        assert!(uv[0] >= min_uv - 1e-6 && uv[2] <= max_uv + 1e-6);
                        assert!(uv[1] >= min_uv - 1e-6 && uv[3] <= max_uv + 1e-6);
                        assert!(uv[2] > uv[0] && uv[3] > uv[1]);
                        let w = rect.max_x.abs_diff(rect.min_x);
                        let h = rect.max_y.abs_diff(rect.min_y);
                        // Scale down by pixel_ticks(lod) to avoid u128 overflow at depth 0.
                        let pt = pixel_ticks(lod) as u128;
                        total_area += (w / pt) * (h / pt);
                    }
                    assert_eq!(total_area, (expected_span / pixel_ticks(lod) as u128).pow(2));
                    if let Some(parent) = t.dyadic_parent() {
                        assert_eq!(quads.len(), 1);
                        assert_eq!(quads[0].0, parent);
                    }
                }
            }
        }
    }

    #[test]
    fn target_lod_reconciles_with_both_desktop_and_conservative_gpu_budgets() {
        for (w, h, dpi) in [(1280, 720, 1.0_f32), (1280, 720, 1.5), (1280, 720, 2.0), (1920, 1080, 1.0), (1920, 1080, 1.5)] {
            for depth in [0u8, 6, 12] {
                for zoom in [0.5, 1.0, 1.37, std::f64::consts::PI, 16.0, 87.5] {
                    if depth > 0 && zoom < 1.0 {
                        continue;
                    }
                    let origin = Point { x: Wide(-123_456), y: Wide(654_321) };
                    let cam = coordinates::target(cid(), origin, depth, zoom).unwrap();
                    let view = Viewport { camera: cam, width: w, height: h };
                    for max_vis in [MAX_VISIBLE_TILES_DESKTOP, MAX_VISIBLE_TILES_CONSERVATIVE] {
                        let lod = select_target_lod(&view, dpi, max_vis, None).unwrap();
                        let tiles = visible_tiles(&view, lod, max_vis).unwrap();
                        assert!(!tiles.is_empty() && tiles.len() <= max_vis, "w={w} h={h} dpi={dpi} d={depth} z={zoom} max={max_vis} got {}", tiles.len());
                    }
                }
            }
        }
    }

    #[test]
    fn adjacent_tiles_share_exact_physical_apron_overlap() {
        let a = TileKey::new(40, -1, 0).unwrap();
        let b = TileKey::new(40, 0, 0).unwrap();
        assert_eq!(a.logical_rect().max_x, b.logical_rect().min_x);
        assert_eq!(a.logical_rect().max_x, 0);
        assert!(a.physical_rect().max_x > b.logical_rect().min_x);
        assert!(b.physical_rect().min_x < a.logical_rect().max_x);
    }
}
