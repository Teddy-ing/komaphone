//! Direct `260×260` (`TILE_PHYSICAL`) CPU tile compositor with exact W4 per-device
//! isolation, straddling summary epoch ordering, and bounded multi-band dynamic
//! composition (`DynamicSplitCache`) for overlapping local/peer strokes and multiple
//! animated `SharedMedia` objects.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use photocraft_scene::shared::media::{AssetHash, MediaFrame};
use photocraft_scene::shared::{DeviceId, MediaId, SharedMedia, SharedStroke};
use photocraft_scene::spatial::{TILE_APRON, TILE_PHYSICAL, TileKey, WorldRect, pixel_ticks, stroke_world_rect, tile_span};
use photocraft_scene::{Stroke, StrokeId, coordinates};

use super::index::{SceneObject, SummaryEpoch, SummaryKey, TileQuery, WorldSceneIndex};
use crate::world_brush::{
    MAX_BRUSH_WORK_SAMPLES, RasterizedBrushStroke, composite_brush_onto_layer, composite_brush_onto_rgba8_tile, pack_world_tile_rgba8,
    rasterize_world_stroke_in_tile,
};
use crate::{EngineError, Result};

const SIDE: usize = TILE_PHYSICAL as usize;
const TILE_PIXELS: usize = SIDE * SIDE;
const BAND_BYTES: usize = TILE_PIXELS * std::mem::size_of::<[f32; 4]>();
/// Cooperative cancellation check stride for dense direct stroke lists.
const CANCEL_STROKE_STRIDE: usize = 32;

fn err(msg: impl Into<String>) -> EngineError {
    EngineError::Other(msg.into())
}

/// Platform-neutral decoded RGBA8 media frame (WASM- and native-compatible).
#[derive(Clone, Debug)]
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[cfg(not(target_arch = "wasm32"))]
impl From<image::RgbaImage> for RgbaFrame {
    fn from(img: image::RgbaImage) -> Self {
        let (width, height) = img.dimensions();
        Self { width, height, rgba: img.into_raw() }
    }
}

/// Output of composing a single `260×260` world tile.
pub struct TileComposeResult {
    pub rgba8: Vec<u8>,
    pub pending_assets: BTreeSet<AssetHash>,
    pub device_layers: usize,
    pub rendered_objects: usize,
    pub cancelled: bool,
    /// True when a UI-thread live-brush composition reused the last valid cached animation
    /// frame from `DynamicSplitCache` instead of blocking on animated-media recomposition;
    /// the caller should schedule an async dynamic tile job to warm the new animation frame.
    pub used_stale_anim_cache: bool,
    /// True when a UI-thread live-brush composition encountered a cold `DynamicSplitCache`
    /// on a tile requiring W4 split-band isolation (devices ordered after the local author,
    /// peer previews, or Erase/Behind/non-Normal blend modes) and returned a fast non-blocking
    /// preview while `WarmSplitBands` populates the exact `[f32; 4]` bands off-thread.
    pub used_cold_split_fallback: bool,
}

/// Double-double (`106`-bit) helper matching `world_brush.rs` and `photocraft_scene::render`
/// for exact tile-relative coordinates across depths `0..=12`.
#[derive(Clone, Copy)]
struct Double {
    hi: f64,
    lo: f64,
}

impl Double {
    fn scalar(v: f64) -> Self {
        Self { hi: v, lo: 0.0 }
    }

    fn integer(v: i128) -> Self {
        let hi = v as f64;
        let lo = v.saturating_sub(hi as i128) as f64;
        Self::sum(hi, lo)
    }

    fn sum(a: f64, b: f64) -> Self {
        let s = a + b;
        let v = s - a;
        Self { hi: s, lo: (a - (s - v)) + (b - v) }
    }

    fn add(self, o: Self) -> Self {
        let s = Self::sum(self.hi, o.hi);
        Self::sum(s.hi, s.lo + self.lo + o.lo)
    }

    fn sub(self, o: Self) -> Self {
        self.add(Self { hi: -o.hi, lo: -o.lo })
    }

    fn mul(self, o: Self) -> Self {
        let p = self.hi * o.hi;
        let e = self.hi.mul_add(o.hi, -p);
        Self::sum(p, e + self.hi * o.lo + self.lo * o.hi)
    }

    fn div(self, o: Self) -> Self {
        let q1 = self.hi / o.hi;
        let r = self.sub(o.mul(Self::scalar(q1)));
        let q2 = (r.hi + r.lo) / o.hi;
        Self::sum(q1, q2)
    }

    fn value(self) -> f64 {
        self.hi + self.lo
    }
}

#[inline]
fn tile_origin_ticks(tile: TileKey) -> (i128, i128, i128) {
    let pt = pixel_ticks(tile.lod);
    let span = tile_span(tile.lod);
    let apron = pt.saturating_mul(i128::from(TILE_APRON));
    let ox = tile.tx.saturating_mul(span).saturating_sub(apron);
    let oy = tile.ty.saturating_mul(span).saturating_sub(apron);
    (ox, oy, pt)
}

#[derive(Clone)]
enum DeviceItem {
    DirectStroke(StrokeId),
    DirectMedia(MediaId),
    Summary(SummaryKey, SummaryEpoch),
    PreviewStroke(usize),
}

struct DeviceWork {
    items: Vec<(u64, u8, DeviceItem)>,
    has_preview: bool,
    earliest_anim_rev: Option<u64>,
}

/// Bounded LRU cache of pre-composited premultiplied `[f32; 4]` static bands and
/// per-dynamic-device static prefixes (`<= 80 MiB` desktop = 77 bands / `<= 48 MiB` conservative = 46 bands),
/// preserving bit-exact equivalence with single-pass composition across all dynamic tiles.
pub struct DynamicSplitCache {
    max_bytes: usize,
    used_bytes: usize,
    entries: BTreeMap<SplitBandKey, Arc<Vec<[f32; 4]>>>,
    latest_anim: BTreeMap<(u64, u64, TileKey, DeviceId), SplitBandKey>,
    order: VecDeque<SplitBandKey>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SplitBandKey {
    scope_gen: u64,
    validated_seq: u64,
    tile: TileKey,
    band_hash: [u8; 32],
}

impl DynamicSplitCache {
    pub fn new(max_bytes: usize) -> Self {
        Self { max_bytes, used_bytes: 0, entries: BTreeMap::new(), latest_anim: BTreeMap::new(), order: VecDeque::new() }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.latest_anim.clear();
        self.order.clear();
        self.used_bytes = 0;
    }

    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    fn contains_key(&self, key: &SplitBandKey) -> bool {
        self.entries.contains_key(key)
    }

    fn has_latest_anim(&self, scope_gen: u64, validated_seq: u64, tile: TileKey, dev: DeviceId) -> bool {
        self.latest_anim.get(&(scope_gen, validated_seq, tile, dev)).is_some_and(|k| self.entries.contains_key(k))
    }

    fn get(&mut self, key: &SplitBandKey) -> Option<Arc<Vec<[f32; 4]>>> {
        let hit = self.entries.get(key).cloned()?;
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
            self.order.push_back(key.clone());
        }
        Some(hit)
    }

    fn get_latest_anim(&mut self, scope_gen: u64, validated_seq: u64, tile: TileKey, dev: DeviceId) -> Option<Arc<Vec<[f32; 4]>>> {
        let key = self.latest_anim.get(&(scope_gen, validated_seq, tile, dev))?.clone();
        self.get(&key)
    }

    fn insert(&mut self, key: SplitBandKey, buf: Arc<Vec<[f32; 4]>>) {
        if BAND_BYTES > self.max_bytes {
            return;
        }
        if self.entries.contains_key(&key) {
            return;
        }
        while self.used_bytes.saturating_add(BAND_BYTES) > self.max_bytes {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if self.entries.remove(&oldest).is_some() {
                self.used_bytes = self.used_bytes.saturating_sub(BAND_BYTES);
                self.latest_anim.retain(|_, v| *v != oldest);
            }
        }
        if self.used_bytes.saturating_add(BAND_BYTES) <= self.max_bytes {
            self.entries.insert(key.clone(), buf);
            self.order.push_back(key);
            self.used_bytes = self.used_bytes.saturating_add(BAND_BYTES);
        }
    }

    fn insert_anim_accepted(&mut self, dev: DeviceId, key: SplitBandKey, buf: Arc<Vec<[f32; 4]>>) {
        let lookup = (key.scope_gen, key.validated_seq, key.tile, dev);
        self.insert(key.clone(), buf);
        if self.entries.contains_key(&key) {
            self.latest_anim.insert(lookup, key);
        }
    }
}

enum SplitCacheRef<'a> {
    Direct(&'a mut DynamicSplitCache),
    Shared(&'a std::sync::Mutex<DynamicSplitCache>),
}

impl SplitCacheRef<'_> {
    fn get(&mut self, key: &SplitBandKey) -> Option<Arc<Vec<[f32; 4]>>> {
        match self {
            SplitCacheRef::Direct(c) => c.get(key),
            SplitCacheRef::Shared(m) => m.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(key),
        }
    }

    fn get_latest_anim(&mut self, scope_gen: u64, validated_seq: u64, tile: TileKey, dev: DeviceId) -> Option<Arc<Vec<[f32; 4]>>> {
        match self {
            SplitCacheRef::Direct(c) => c.get_latest_anim(scope_gen, validated_seq, tile, dev),
            SplitCacheRef::Shared(m) => m.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_latest_anim(scope_gen, validated_seq, tile, dev),
        }
    }

    fn insert(&mut self, key: SplitBandKey, buf: Arc<Vec<[f32; 4]>>) {
        match self {
            SplitCacheRef::Direct(c) => c.insert(key, buf),
            SplitCacheRef::Shared(m) => m.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key, buf),
        }
    }

    fn insert_anim_accepted(&mut self, dev: DeviceId, key: SplitBandKey, buf: Arc<Vec<[f32; 4]>>) {
        match self {
            SplitCacheRef::Direct(c) => c.insert_anim_accepted(dev, key, buf),
            SplitCacheRef::Shared(m) => m.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert_anim_accepted(dev, key, buf),
        }
    }
}

/// Optional dynamic overlay inputs for composing one `260×260` tile.
pub struct TileComposeParams<'a> {
    pub index: &'a WorldSceneIndex,
    pub tile: TileKey,
    pub scope_gen: u64,
    pub validated_seq: u64,
    pub elapsed_ms: u64,
    pub previews: &'a [SharedStroke],
    pub live_brush: Option<(StrokeId, &'a RasterizedBrushStroke)>,
    pub allow_parallel_brush: bool,
    pub cancel: Option<&'a AtomicBool>,
}

/// Directly compose one `260×260` world tile in exact `(DeviceId, created_revision)` order.
/// Uses `DynamicSplitCache` automatically when any overlapping device has live/peer previews
/// or animated `SharedMedia` (Codex Correction #3), with clean single-pass fallback if cache is full.
pub fn compose_world_tile(
    params: &TileComposeParams<'_>,
    split_cache: Option<&mut DynamicSplitCache>,
    resolve_media: &mut dyn FnMut(&MediaFrame) -> Result<Option<Arc<RgbaFrame>>>,
) -> Result<TileComposeResult> {
    compose_world_tile_inner(params, split_cache.map(SplitCacheRef::Direct), false, resolve_media)
}

/// Thread-safe counterpart of `compose_world_tile` sharing a `Mutex<DynamicSplitCache>`
/// across parallel dynamic-tile workers (acquires the lock only for brief `get`/`insert`).
pub fn compose_world_tile_shared(
    params: &TileComposeParams<'_>,
    split_cache: Option<&std::sync::Mutex<DynamicSplitCache>>,
    resolve_media: &mut dyn FnMut(&MediaFrame) -> Result<Option<Arc<RgbaFrame>>>,
) -> Result<TileComposeResult> {
    compose_world_tile_inner(params, split_cache.map(SplitCacheRef::Shared), false, resolve_media)
}

/// UI-thread live-brush composition helper: reuses the last valid cached animated-media
/// layer from `DynamicSplitCache` when an animation tick misses so animated-media recomposition
/// stays off the UI thread even while actively drawing over animated media.
pub(crate) fn compose_world_tile_shared_prefer_cached_anim(
    params: &TileComposeParams<'_>,
    split_cache: Option<&std::sync::Mutex<DynamicSplitCache>>,
    resolve_media: &mut dyn FnMut(&MediaFrame) -> Result<Option<Arc<RgbaFrame>>>,
) -> Result<TileComposeResult> {
    compose_world_tile_inner(params, split_cache.map(SplitCacheRef::Shared), true, resolve_media)
}

/// Cached committed RGBA8 tile passed into non-blocking UI-thread live-brush composition so
/// cold tiles never block the UI thread rasterizing thousands of committed strokes.
#[derive(Clone, Copy)]
pub struct LiveColdBase<'a> {
    pub rgba8: Option<&'a [u8]>,
    pub is_exact_anim: bool,
}

/// UI-thread non-blocking live-brush composition:
/// - Fast path A: associative `SrcOver` on topmost `local_dev` with no peer previews composites
///   directly onto `base.rgba8` in `~0.1 ms` (exact W4 order, zero `DynamicSplitCache` churn).
/// - Fast path B: when all required `DynamicSplitCache` split bands are warm, runs bit-exact
///   `[f32; 4]` W4 composition in `~0.5–1.5 ms`.
/// - Cold path C: never rasterizes committed static bands on the UI thread; returns an immediate
///   non-blocking preview (`used_cold_split_fallback = true`) so the caller schedules off-thread
///   `WarmSplitBands` and snaps to bit-exact W4 output as soon as the worker finishes.
pub(crate) fn compose_world_tile_nonblocking_live(
    params: &TileComposeParams<'_>,
    local_stroke_id: StrokeId,
    base: LiveColdBase<'_>,
    split_cache: &std::sync::Mutex<DynamicSplitCache>,
    resolve_media: &mut dyn FnMut(&MediaFrame) -> Result<Option<Arc<RgbaFrame>>>,
) -> Result<TileComposeResult> {
    let query = params.index.query_tile(params.tile);
    let tile_rect = params.tile.physical_rect();
    let per_device = build_device_plan(params.index, &query, tile_rect, params.previews)?;
    let Some(local_shared) = params.previews.iter().find(|p| p.visible && p.stroke.id == local_stroke_id) else {
        let rgba8 = base.rgba8.map_or_else(|| vec![255u8; TILE_PIXELS * 4], |b| b.to_vec());
        return Ok(TileComposeResult {
            rgba8,
            pending_assets: BTreeSet::new(),
            device_layers: per_device.len(),
            rendered_objects: 0,
            cancelled: false,
            used_stale_anim_cache: !base.is_exact_anim,
            used_cold_split_fallback: false,
        });
    };
    let local_dev = local_shared.device;
    let has_devices_after_local = per_device.keys().any(|&d| d > local_dev);
    let has_peer_previews = per_device.iter().any(|(&d, w)| d != local_dev && w.has_preview);
    let local_has_committed = per_device.get(&local_dev).is_some_and(|w| w.items.iter().any(|&(_, kind, _)| kind != 3));
    let any_committed = per_device.values().any(|w| w.items.iter().any(|&(_, kind, _)| kind != 3));
    let is_pure_src_over =
        !local_shared.stroke.erase
            && local_shared.stroke.brush.as_deref().is_none_or(|b| {
                !b.behind && matches!(b.mode, photocraft_scene::brush::BrushBlendMode::Normal | photocraft_scene::brush::BrushBlendMode::Dissolve)
            });
    let all_aliased = is_tile_all_aliased(params.index, &query, params.previews);

    // Exact no-op W4 erase when `local_dev` has no committed content on this tile.
    if local_shared.stroke.erase
        && !local_has_committed
        && !has_peer_previews
        && let Some(base_bytes) = base.rgba8
    {
        return Ok(TileComposeResult {
            rgba8: base_bytes.to_vec(),
            pending_assets: BTreeSet::new(),
            device_layers: per_device.len(),
            rendered_objects: 0,
            cancelled: false,
            used_stale_anim_cache: !base.is_exact_anim,
            used_cold_split_fallback: false,
        });
    }

    // Associative pure SrcOver on topmost device with no overlapping peer previews:
    // `SrcOver(committed_tile, live_stroke)` is W4-exact and never touches `DynamicSplitCache`.
    if is_pure_src_over
        && !has_devices_after_local
        && !has_peer_previews
        && let Some(base_bytes) = base.rgba8
    {
        let rgba8 = composite_live_stroke_onto_rgba8(params.tile, &local_shared.stroke, params.live_brush, base_bytes, all_aliased)?;
        return Ok(TileComposeResult {
            rgba8,
            pending_assets: BTreeSet::new(),
            device_layers: per_device.len(),
            rendered_objects: 1,
            cancelled: false,
            used_stale_anim_cache: !base.is_exact_anim,
            used_cold_split_fallback: false,
        });
    }

    // Check if all required `DynamicSplitCache` bands are already warm (or tile has no committed items).
    let (cache_disabled, bands_warm) = {
        let guard = split_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let disabled = guard.max_bytes() < BAND_BYTES;
        let warm = !any_committed || all_required_split_bands_warm(params, &per_device, &guard);
        (disabled, warm)
    };
    if cache_disabled || bands_warm {
        return compose_world_tile_inner(params, Some(SplitCacheRef::Shared(split_cache)), true, resolve_media);
    }

    // Cold tile requiring W4 split-band isolation: NEVER rasterize committed strokes on UI thread!
    // Show immediate non-blocking preview (or hold base untouched for Erase so peers are never
    // falsely erased) while `WarmSplitBands` populates the exact `[f32; 4]` bands off-thread.
    let base_bytes = base.rgba8.unwrap_or(&[]);
    let rgba8 = if local_shared.stroke.erase {
        if base_bytes.len() == TILE_PIXELS * 4 { base_bytes.to_vec() } else { vec![255u8; TILE_PIXELS * 4] }
    } else {
        composite_live_stroke_onto_rgba8(params.tile, &local_shared.stroke, params.live_brush, base_bytes, all_aliased)?
    };
    Ok(TileComposeResult {
        rgba8,
        pending_assets: BTreeSet::new(),
        device_layers: per_device.len(),
        rendered_objects: 1,
        cancelled: false,
        used_stale_anim_cache: !base.is_exact_anim,
        used_cold_split_fallback: true,
    })
}

/// Populate `DynamicSplitCache` static runs and accepted device prefix/anim layers off-thread
/// for `(tile, split_devices, elapsed_ms)` so live drawing hits warm exact W4 bands with zero UI stall.
pub(crate) fn warm_dynamic_split_bands(
    params: &TileComposeParams<'_>,
    split_devices: &[DeviceId],
    split_cache: &std::sync::Mutex<DynamicSplitCache>,
    resolve_media: &mut dyn FnMut(&MediaFrame) -> Result<Option<Arc<RgbaFrame>>>,
) -> Result<bool> {
    if params.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        return Ok(false);
    }
    let query = params.index.query_tile(params.tile);
    let tile_rect = params.tile.physical_rect();
    let mut per_device = build_device_plan(params.index, &query, tile_rect, &[])?;
    for &dev in split_devices {
        let w = per_device.entry(dev).or_insert(DeviceWork { items: Vec::new(), has_preview: false, earliest_anim_rev: None });
        w.has_preview = true;
    }
    if !per_device.values().any(|w| w.items.iter().any(|&(_, kind, _)| kind != 3)) {
        return Ok(true);
    }
    let mut cache_ref = SplitCacheRef::Shared(split_cache);
    let mut used_stale = false;
    let mut pending_assets = BTreeSet::new();
    let mut rendered_objects = 0usize;
    let mut brush_work = 0u64;
    let out = compose_with_dynamic_splits(
        params,
        &per_device,
        &mut cache_ref,
        false,
        &mut used_stale,
        &mut pending_assets,
        &mut rendered_objects,
        &mut brush_work,
        resolve_media,
    )?;
    Ok(!out.is_empty() && !params.cancel.is_some_and(|c| c.load(Ordering::Relaxed)))
}

fn all_required_split_bands_warm(params: &TileComposeParams<'_>, per_device: &BTreeMap<DeviceId, DeviceWork>, cache: &DynamicSplitCache) -> bool {
    let devices: Vec<(DeviceId, &DeviceWork)> = per_device.iter().map(|(&d, w)| (d, w)).collect();
    let is_dyn = |w: &DeviceWork| w.has_preview || w.earliest_anim_rev.is_some();
    let mut idx = 0usize;
    let mut band_index = 0u32;
    while idx < devices.len() {
        if !is_dyn(devices[idx].1) {
            let start = idx;
            while idx < devices.len() && !is_dyn(devices[idx].1) {
                idx += 1;
            }
            let run = devices.get(start..idx).unwrap_or_default();
            let key = SplitBandKey {
                scope_gen: params.scope_gen,
                validated_seq: params.validated_seq,
                tile: params.tile,
                band_hash: hash_static_run(band_index, run),
            };
            if !cache.contains_key(&key) {
                return false;
            }
            band_index += 1;
        } else {
            let (dev, work) = devices[idx];
            idx += 1;
            let accepted_end = work.items.partition_point(|&(_, kind, _)| kind != 3);
            let accepted_items = work.items.get(..accepted_end).unwrap_or_default();
            if !accepted_items.is_empty() {
                if work.earliest_anim_rev.is_some() {
                    let exact_key = SplitBandKey {
                        scope_gen: params.scope_gen,
                        validated_seq: params.validated_seq,
                        tile: params.tile,
                        band_hash: hash_device_accepted_anim(params.index, dev, accepted_items, params.elapsed_ms),
                    };
                    if !cache.contains_key(&exact_key) && !cache.has_latest_anim(params.scope_gen, params.validated_seq, params.tile, dev) {
                        return false;
                    }
                } else {
                    let prefix_key = SplitBandKey {
                        scope_gen: params.scope_gen,
                        validated_seq: params.validated_seq,
                        tile: params.tile,
                        band_hash: hash_device_prefix(dev, u64::MAX, accepted_items.len()),
                    };
                    if !cache.contains_key(&prefix_key) {
                        return false;
                    }
                }
            }
            band_index += 1;
        }
    }
    true
}

fn composite_live_stroke_onto_rgba8(
    tile: TileKey,
    stroke: &Stroke,
    live_brush: Option<(StrokeId, &RasterizedBrushStroke)>,
    base_rgba8: &[u8],
    all_aliased: bool,
) -> Result<Vec<u8>> {
    if stroke.brush.is_some() {
        if let Some((live_id, live_raster)) = live_brush
            && live_id == stroke.id
            && live_raster.phys_w == TILE_PHYSICAL
            && live_raster.phys_h == TILE_PHYSICAL
        {
            return Ok(composite_brush_onto_rgba8_tile(tile, live_raster, base_rgba8, all_aliased));
        }
        let mut work = 0u64;
        if let Some(raster) = rasterize_world_stroke_in_tile(stroke, tile, &mut work, MAX_BRUSH_WORK_SAMPLES, false, None)? {
            return Ok(composite_brush_onto_rgba8_tile(tile, &raster, base_rgba8, all_aliased));
        }
        return Ok(if base_rgba8.len() == TILE_PIXELS * 4 { base_rgba8.to_vec() } else { vec![255u8; TILE_PIXELS * 4] });
    }
    // Legacy non-brush polyline live stroke: rasterize only this single stroke and SrcOver onto base.
    let mut layer = vec![[0.0f32; 4]; TILE_PIXELS];
    if !rasterize_legacy_stroke_in_tile(stroke, tile, &mut layer)? {
        return Ok(if base_rgba8.len() == TILE_PIXELS * 4 { base_rgba8.to_vec() } else { vec![255u8; TILE_PIXELS * 4] });
    }
    let mut out = if base_rgba8.len() == TILE_PIXELS * 4 {
        base_rgba8
            .chunks_exact(4)
            .map(|c| [f32::from(c[0]) / 255.0, f32::from(c[1]) / 255.0, f32::from(c[2]) / 255.0, f32::from(c[3]) / 255.0])
            .collect::<Vec<_>>()
    } else {
        vec![[1.0f32; 4]; TILE_PIXELS]
    };
    src_over_layer(&mut out, &layer);
    Ok(pack_world_tile_rgba8(tile, &out, all_aliased))
}

fn compose_world_tile_inner(
    params: &TileComposeParams<'_>,
    mut split_cache: Option<SplitCacheRef<'_>>,
    prefer_cached_anim: bool,
    resolve_media: &mut dyn FnMut(&MediaFrame) -> Result<Option<Arc<RgbaFrame>>>,
) -> Result<TileComposeResult> {
    if params.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        return Ok(cancelled_result());
    }
    let query = params.index.query_tile(params.tile);
    let tile_rect = params.tile.physical_rect();
    let per_device = build_device_plan(params.index, &query, tile_rect, params.previews)?;
    if per_device.is_empty() {
        return Ok(TileComposeResult {
            rgba8: vec![255u8; TILE_PIXELS * 4],
            pending_assets: BTreeSet::new(),
            device_layers: 0,
            rendered_objects: 0,
            cancelled: false,
            used_stale_anim_cache: false,
            used_cold_split_fallback: false,
        });
    }

    let any_dynamic = per_device.values().any(|w| w.has_preview || w.earliest_anim_rev.is_some());
    let mut pending_assets = BTreeSet::new();
    let mut rendered_objects = 0usize;
    let mut brush_work = 0u64;
    let mut used_stale_anim_cache = false;

    let output = if !any_dynamic || split_cache.is_none() {
        let mut out = vec![[1.0f32; 4]; TILE_PIXELS];
        let mut scratch = vec![[0.0f32; 4]; TILE_PIXELS];
        for work in per_device.values() {
            if params.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return Ok(cancelled_result());
            }
            scratch.fill([0.0f32; 4]);
            let ok = run_device_items(params, &work.items, &mut scratch, &mut pending_assets, &mut rendered_objects, &mut brush_work, resolve_media)?;
            if !ok {
                return Ok(cancelled_result());
            }
            src_over_layer(&mut out, &scratch);
        }
        out
    } else if let Some(cache) = split_cache.as_mut() {
        compose_with_dynamic_splits(
            params,
            &per_device,
            cache,
            prefer_cached_anim,
            &mut used_stale_anim_cache,
            &mut pending_assets,
            &mut rendered_objects,
            &mut brush_work,
            resolve_media,
        )?
    } else {
        vec![[1.0f32; 4]; TILE_PIXELS]
    };

    if params.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        return Ok(cancelled_result());
    }
    let all_aliased = is_tile_all_aliased(params.index, &query, params.previews);
    let rgba8 = pack_world_tile_rgba8(params.tile, &output, all_aliased);
    Ok(TileComposeResult {
        rgba8,
        pending_assets,
        device_layers: per_device.len(),
        rendered_objects,
        cancelled: false,
        used_stale_anim_cache,
        used_cold_split_fallback: false,
    })
}

fn cancelled_result() -> TileComposeResult {
    TileComposeResult {
        rgba8: Vec::new(),
        pending_assets: BTreeSet::new(),
        device_layers: 0,
        rendered_objects: 0,
        cancelled: true,
        used_stale_anim_cache: false,
        used_cold_split_fallback: false,
    }
}

fn is_tile_all_aliased(index: &WorldSceneIndex, query: &TileQuery, previews: &[SharedStroke]) -> bool {
    let mut has_any_brush = false;
    for &obj in &query.direct {
        if let SceneObject::Stroke(id) = obj
            && let Some(shared) = index.world().strokes.get(&id)
            && let Some(b) = shared.stroke.brush.as_deref()
        {
            has_any_brush = true;
            if !b.aliased {
                return false;
            }
        }
    }
    for p in previews {
        if let Some(b) = p.stroke.brush.as_deref() {
            has_any_brush = true;
            if !b.aliased {
                return false;
            }
        }
    }
    has_any_brush
}

fn build_device_plan(index: &WorldSceneIndex, query: &TileQuery, tile_rect: WorldRect, previews: &[SharedStroke]) -> Result<BTreeMap<DeviceId, DeviceWork>> {
    let mut per_device: BTreeMap<DeviceId, DeviceWork> = BTreeMap::new();
    let mut direct_revs: BTreeMap<DeviceId, Vec<(u64, WorldRect)>> = BTreeMap::new();

    for &obj in &query.direct {
        let Some(entry) = index.entry(obj) else { continue };
        let w = per_device.entry(entry.device).or_insert(DeviceWork { items: Vec::new(), has_preview: false, earliest_anim_rev: None });
        match obj {
            SceneObject::Stroke(id) => {
                w.items.push((entry.created_rev, 0, DeviceItem::DirectStroke(id)));
                direct_revs.entry(entry.device).or_default().push((entry.created_rev, entry.rect));
            }
            SceneObject::Media(id) => {
                if let Some(m) = index.world().media.get(&id)
                    && m.media.frames.len() > 1
                {
                    w.earliest_anim_rev = Some(w.earliest_anim_rev.map_or(entry.created_rev, |prev| prev.min(entry.created_rev)));
                }
                w.items.push((entry.created_rev, 1, DeviceItem::DirectMedia(id)));
                direct_revs.entry(entry.device).or_default().push((entry.created_rev, entry.rect));
            }
        }
    }

    for (skey, dev, epoch) in &query.summaries {
        let w = per_device.entry(*dev).or_insert(DeviceWork { items: Vec::new(), has_preview: false, earliest_anim_rev: None });
        let srect = skey.world_rect();
        let mut splits = Vec::new();
        if let Some(revs) = direct_revs.get(dev) {
            for &(rev, drect) in revs {
                if rev > epoch.min_rev && rev < epoch.max_rev && drect.intersects(&srect) {
                    splits.push(rev);
                }
            }
        }
        if splits.is_empty() {
            w.items.push((epoch.min_rev, 2, DeviceItem::Summary(*skey, epoch.clone())));
        } else {
            splits.sort_unstable();
            splits.dedup();
            for sub_epoch in index.split_summary_epochs(*skey, *dev, &splits) {
                w.items.push((sub_epoch.min_rev, 2, DeviceItem::Summary(*skey, sub_epoch)));
            }
        }
    }

    let preview_rev = index.world().revision.saturating_add(1);
    for (pidx, shared) in previews.iter().enumerate() {
        if !shared.visible || index.world().strokes.contains_key(&shared.stroke.id) {
            continue;
        }
        let rect = stroke_world_rect(&shared.stroke).map_err(|e| err(e.to_string()))?;
        if !rect.intersects(&tile_rect) {
            continue;
        }
        let w = per_device.entry(shared.device).or_insert(DeviceWork { items: Vec::new(), has_preview: false, earliest_anim_rev: None });
        w.has_preview = true;
        w.items.push((preview_rev.saturating_add(pidx as u64), 3, DeviceItem::PreviewStroke(pidx)));
    }

    for work in per_device.values_mut() {
        work.items.sort_by_key(|&(rev, kind, _)| (rev, kind));
    }
    Ok(per_device)
}

#[allow(clippy::too_many_arguments)]
fn compose_with_dynamic_splits(
    params: &TileComposeParams<'_>,
    per_device: &BTreeMap<DeviceId, DeviceWork>,
    cache: &mut SplitCacheRef<'_>,
    prefer_cached_anim: bool,
    used_stale_anim_cache: &mut bool,
    pending_assets: &mut BTreeSet<AssetHash>,
    rendered_objects: &mut usize,
    brush_work: &mut u64,
    resolve_media: &mut dyn FnMut(&MediaFrame) -> Result<Option<Arc<RgbaFrame>>>,
) -> Result<Vec<[f32; 4]>> {
    let devices: Vec<(DeviceId, &DeviceWork)> = per_device.iter().map(|(&d, w)| (d, w)).collect();
    let mut output = vec![[1.0f32; 4]; TILE_PIXELS];
    let mut scratch = vec![[0.0f32; 4]; TILE_PIXELS];
    let mut idx = 0usize;
    let mut band_index = 0u32;

    while idx < devices.len() {
        if params.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Ok(Vec::new());
        }
        let is_dyn = |w: &DeviceWork| w.has_preview || w.earliest_anim_rev.is_some();
        if !is_dyn(devices[idx].1) {
            let start = idx;
            while idx < devices.len() && !is_dyn(devices[idx].1) {
                idx += 1;
            }
            let run = devices.get(start..idx).unwrap_or_default();
            let key = SplitBandKey {
                scope_gen: params.scope_gen,
                validated_seq: params.validated_seq,
                tile: params.tile,
                band_hash: hash_static_run(band_index, run),
            };
            let band = if let Some(hit) = cache.get(&key) {
                hit
            } else {
                let mut band_buf = vec![if band_index == 0 { [1.0f32; 4] } else { [0.0f32; 4] }; TILE_PIXELS];
                let mut run_pending = BTreeSet::new();
                for &(_, work) in run {
                    scratch.fill([0.0f32; 4]);
                    let ok = run_device_items(params, &work.items, &mut scratch, &mut run_pending, rendered_objects, brush_work, resolve_media)?;
                    if !ok {
                        return Ok(Vec::new());
                    }
                    src_over_layer(&mut band_buf, &scratch);
                }
                let arc = Arc::new(band_buf);
                if run_pending.is_empty() {
                    cache.insert(key, Arc::clone(&arc));
                } else {
                    pending_assets.extend(run_pending);
                }
                arc
            };
            if band_index == 0 {
                output.copy_from_slice(&band);
            } else {
                src_over_layer(&mut output, &band);
            }
            band_index += 1;
        } else {
            let (dev, work) = devices[idx];
            idx += 1;
            let ok = compose_dynamic_device(
                params,
                dev,
                work,
                &mut scratch,
                cache,
                prefer_cached_anim,
                used_stale_anim_cache,
                pending_assets,
                rendered_objects,
                brush_work,
                resolve_media,
            )?;
            if !ok {
                return Ok(Vec::new());
            }
            src_over_layer(&mut output, &scratch);
            band_index += 1;
        }
    }
    Ok(output)
}

#[allow(clippy::too_many_arguments)]
fn compose_dynamic_device(
    params: &TileComposeParams<'_>,
    dev: DeviceId,
    work: &DeviceWork,
    scratch: &mut [[f32; 4]],
    cache: &mut SplitCacheRef<'_>,
    prefer_cached_anim: bool,
    used_stale_anim_cache: &mut bool,
    pending_assets: &mut BTreeSet<AssetHash>,
    rendered_objects: &mut usize,
    brush_work: &mut u64,
    resolve_media: &mut dyn FnMut(&MediaFrame) -> Result<Option<Arc<RgbaFrame>>>,
) -> Result<bool> {
    let accepted_end = work.items.partition_point(|&(_, kind, _)| kind != 3);
    let accepted_items = work.items.get(..accepted_end).unwrap_or_default();
    let preview_items = work.items.get(accepted_end..).unwrap_or_default();

    // When a dynamic device contains animated media, cache its entire accepted layer keyed by
    // the exact active frame indices of its animated media so live brush strokes and peer
    // updates hitting the same tile never re-rasterize accepted Multiply/Erase/Behind strokes
    // between animation ticks.
    let anim_accepted_key = if work.earliest_anim_rev.is_some() && !accepted_items.is_empty() {
        Some(SplitBandKey {
            scope_gen: params.scope_gen,
            validated_seq: params.validated_seq,
            tile: params.tile,
            band_hash: hash_device_accepted_anim(params.index, dev, accepted_items, params.elapsed_ms),
        })
    } else {
        None
    };

    if let Some(ref key) = anim_accepted_key
        && let Some(hit) = cache.get(key)
    {
        scratch.copy_from_slice(&hit);
    } else if prefer_cached_anim
        && anim_accepted_key.is_some()
        && let Some(stale_hit) = cache.get_latest_anim(params.scope_gen, params.validated_seq, params.tile, dev)
    {
        // Keep the last valid cached animation frame on the UI thread while live-drawing so
        // animated-media recomposition stays strictly off the UI thread; the caller schedules
        // a background dynamic tile job to warm the new animation frame into DynamicSplitCache.
        scratch.copy_from_slice(&stale_hit);
        *used_stale_anim_cache = true;
    } else {
        let split_rev = work.earliest_anim_rev.unwrap_or(u64::MAX);
        let split_pos = accepted_items.partition_point(|&(rev, _, _)| rev < split_rev);
        let prefix_items = accepted_items.get(..split_pos).unwrap_or_default();
        let suffix_accepted = accepted_items.get(split_pos..).unwrap_or_default();
        let mut accepted_pending = BTreeSet::new();

        if prefix_items.is_empty() {
            scratch.fill([0.0f32; 4]);
        } else {
            let key = SplitBandKey {
                scope_gen: params.scope_gen,
                validated_seq: params.validated_seq,
                tile: params.tile,
                band_hash: hash_device_prefix(dev, split_rev, prefix_items.len()),
            };
            if let Some(hit) = cache.get(&key) {
                scratch.copy_from_slice(&hit);
            } else {
                scratch.fill([0.0f32; 4]);
                let mut pref_pending = BTreeSet::new();
                let ok = run_device_items(params, prefix_items, scratch, &mut pref_pending, rendered_objects, brush_work, resolve_media)?;
                if !ok {
                    return Ok(false);
                }
                if pref_pending.is_empty() {
                    cache.insert(key, Arc::new(scratch.to_vec()));
                } else {
                    accepted_pending.extend(pref_pending);
                }
            }
        }
        let ok = run_suffix_with_associative_cache(
            params,
            dev,
            suffix_accepted,
            scratch,
            cache,
            &mut accepted_pending,
            rendered_objects,
            brush_work,
            resolve_media,
        )?;
        if !ok {
            return Ok(false);
        }
        if let Some(key) = anim_accepted_key {
            if accepted_pending.is_empty() {
                cache.insert_anim_accepted(dev, key, Arc::new(scratch.to_vec()));
            } else {
                pending_assets.extend(accepted_pending);
            }
        } else {
            pending_assets.extend(accepted_pending);
        }
    }

    if !preview_items.is_empty() {
        let ok = run_device_items(params, preview_items, scratch, pending_assets, rendered_objects, brush_work, resolve_media)?;
        if !ok {
            return Ok(false);
        }
    }
    Ok(true)
}

fn hash_device_accepted_anim(index: &WorldSceneIndex, dev: DeviceId, accepted_items: &[(u64, u8, DeviceItem)], elapsed_ms: u64) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"dyn-accepted-anim:");
    h.update(dev.as_str().as_bytes());
    h.update(&(accepted_items.len() as u64).to_le_bytes());
    for (_, _, item) in accepted_items {
        if let DeviceItem::DirectMedia(id) = item
            && let Some(shared) = index.world().media.get(id)
            && shared.media.frames.len() > 1
        {
            h.update(id.as_str().as_bytes());
            let frame_idx = shared.media.frame_at(elapsed_ms).map_or(usize::MAX, |(idx, _)| idx);
            h.update(&(frame_idx as u64).to_le_bytes());
        }
    }
    *h.finalize().as_bytes()
}

fn is_static_src_over(index: &WorldSceneIndex, item: &DeviceItem) -> bool {
    match item {
        DeviceItem::DirectStroke(id) => index.world().strokes.get(id).is_some_and(|s| {
            !s.stroke.erase && s.stroke.brush.as_deref().is_none_or(|b| !b.behind && matches!(b.mode, photocraft_scene::brush::BrushBlendMode::Normal))
        }),
        DeviceItem::DirectMedia(id) => index.world().media.get(id).is_some_and(|m| m.media.frames.len() <= 1),
        DeviceItem::Summary(..) => true,
        DeviceItem::PreviewStroke(_) => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn run_suffix_with_associative_cache(
    params: &TileComposeParams<'_>,
    dev: DeviceId,
    suffix_items: &[(u64, u8, DeviceItem)],
    layer: &mut [[f32; 4]],
    cache: &mut SplitCacheRef<'_>,
    pending_assets: &mut BTreeSet<AssetHash>,
    rendered_objects: &mut usize,
    brush_work: &mut u64,
    resolve_media: &mut dyn FnMut(&MediaFrame) -> Result<Option<Arc<RgbaFrame>>>,
) -> Result<bool> {
    const MIN_CACHEABLE_RUN: usize = 8;
    let mut pos = 0usize;
    let mut run_idx = 0u32;
    while pos < suffix_items.len() {
        if is_static_src_over(params.index, &suffix_items[pos].2) {
            let start = pos;
            while pos < suffix_items.len() && is_static_src_over(params.index, &suffix_items[pos].2) {
                pos += 1;
            }
            let slice = suffix_items.get(start..pos).unwrap_or_default();
            if slice.len() >= MIN_CACHEABLE_RUN {
                let first_rev = slice.first().map_or(0, |x| x.0);
                let last_rev = slice.last().map_or(0, |x| x.0);
                let key = SplitBandKey {
                    scope_gen: params.scope_gen,
                    validated_seq: params.validated_seq,
                    tile: params.tile,
                    band_hash: hash_suffix_run(dev, run_idx, first_rev, last_rev, slice.len()),
                };
                if let Some(hit) = cache.get(&key) {
                    src_over_layer(layer, &hit);
                } else {
                    let mut band = vec![[0.0f32; 4]; TILE_PIXELS];
                    let mut run_pending = BTreeSet::new();
                    let ok = run_device_items(params, slice, &mut band, &mut run_pending, rendered_objects, brush_work, resolve_media)?;
                    if !ok {
                        return Ok(false);
                    }
                    src_over_layer(layer, &band);
                    if run_pending.is_empty() {
                        cache.insert(key, Arc::new(band));
                    } else {
                        pending_assets.extend(run_pending);
                    }
                }
                run_idx = run_idx.saturating_add(1);
                continue;
            }
            let ok = run_device_items(params, slice, layer, pending_assets, rendered_objects, brush_work, resolve_media)?;
            if !ok {
                return Ok(false);
            }
        } else {
            let slice = suffix_items.get(pos..pos + 1).unwrap_or_default();
            let ok = run_device_items(params, slice, layer, pending_assets, rendered_objects, brush_work, resolve_media)?;
            if !ok {
                return Ok(false);
            }
            pos += 1;
        }
    }
    Ok(true)
}

fn hash_suffix_run(dev: DeviceId, run_idx: u32, first_rev: u64, last_rev: u64, count: usize) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"dyn-suffix-run:");
    h.update(dev.as_str().as_bytes());
    h.update(&run_idx.to_le_bytes());
    h.update(&first_rev.to_le_bytes());
    h.update(&last_rev.to_le_bytes());
    h.update(&(count as u64).to_le_bytes());
    *h.finalize().as_bytes()
}

fn hash_static_run(band_index: u32, run: &[(DeviceId, &DeviceWork)]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"static-run:");
    h.update(&band_index.to_le_bytes());
    for (dev, w) in run {
        h.update(dev.as_str().as_bytes());
        h.update(&(w.items.len() as u64).to_le_bytes());
    }
    *h.finalize().as_bytes()
}

fn hash_device_prefix(dev: DeviceId, split_rev: u64, count: usize) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"dyn-prefix:");
    h.update(dev.as_str().as_bytes());
    h.update(&split_rev.to_le_bytes());
    h.update(&(count as u64).to_le_bytes());
    *h.finalize().as_bytes()
}

fn run_device_items(
    params: &TileComposeParams<'_>,
    items: &[(u64, u8, DeviceItem)],
    layer: &mut [[f32; 4]],
    pending_assets: &mut BTreeSet<AssetHash>,
    rendered_objects: &mut usize,
    brush_work: &mut u64,
    resolve_media: &mut dyn FnMut(&MediaFrame) -> Result<Option<Arc<RgbaFrame>>>,
) -> Result<bool> {
    for (step, (_, _, item)) in items.iter().enumerate() {
        if (step % CANCEL_STROKE_STRIDE) == 0 && params.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Ok(false);
        }
        match item {
            DeviceItem::DirectStroke(id) => {
                if let Some(shared) = params.index.world().strokes.get(id) {
                    composite_stroke_in_tile(
                        &shared.stroke,
                        params.tile,
                        params.live_brush,
                        params.allow_parallel_brush,
                        params.cancel,
                        brush_work,
                        layer,
                        rendered_objects,
                    )?;
                }
            }
            DeviceItem::PreviewStroke(pidx) => {
                if let Some(shared) = params.previews.get(*pidx) {
                    composite_stroke_in_tile(
                        &shared.stroke,
                        params.tile,
                        params.live_brush,
                        params.allow_parallel_brush,
                        params.cancel,
                        brush_work,
                        layer,
                        rendered_objects,
                    )?;
                }
            }
            DeviceItem::DirectMedia(id) => {
                if let Some(shared) = params.index.world().media.get(id) {
                    composite_media_in_tile(shared, params.tile, params.elapsed_ms, pending_assets, resolve_media, layer, rendered_objects)?;
                }
            }
            DeviceItem::Summary(skey, epoch) => {
                composite_summary_in_tile(*skey, epoch, params.tile, layer);
                *rendered_objects = rendered_objects.saturating_add(1);
            }
        }
    }
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn composite_stroke_in_tile(
    stroke: &Stroke,
    tile: TileKey,
    live_brush: Option<(StrokeId, &RasterizedBrushStroke)>,
    allow_parallel: bool,
    cancel: Option<&AtomicBool>,
    brush_work: &mut u64,
    layer: &mut [[f32; 4]],
    rendered_objects: &mut usize,
) -> Result<()> {
    if stroke.brush.is_some() {
        if let Some((live_id, live_raster)) = live_brush
            && live_id == stroke.id
            && live_raster.phys_w == TILE_PHYSICAL
            && live_raster.phys_h == TILE_PHYSICAL
        {
            composite_brush_onto_layer(live_raster, layer, TILE_PHYSICAL, |_| {});
            *rendered_objects = rendered_objects.saturating_add(1);
            return Ok(());
        }
        // Per-tile generous budget: a 260×260 tile caps individual dab coverage to 67,600 samples,
        // so even 10,000 stacked strokes stay bounded while respecting cancellation.
        let mut local_work = 0u64;
        if let Some(raster) = rasterize_world_stroke_in_tile(stroke, tile, &mut local_work, MAX_BRUSH_WORK_SAMPLES, allow_parallel, cancel)? {
            *brush_work = brush_work.saturating_add(local_work);
            composite_brush_onto_layer(&raster, layer, TILE_PHYSICAL, |_| {});
            *rendered_objects = rendered_objects.saturating_add(1);
        }
    } else if rasterize_legacy_stroke_in_tile(stroke, tile, layer)? {
        *rendered_objects = rendered_objects.saturating_add(1);
    }
    Ok(())
}

/// Exact tile-space antialiased round-capsule rasterizer for legacy (`brush: None`) strokes.
/// Handles both normal scale and `10^24×` deep-zoom ancestor strokes cleanly in `106`-bit `Double`.
fn rasterize_legacy_stroke_in_tile(stroke: &Stroke, tile: TileKey, layer: &mut [[f32; 4]]) -> Result<bool> {
    if stroke.points.is_empty() {
        return Ok(false);
    }
    let (ox, oy, pt) = tile_origin_ticks(tile);
    let anchor_unit = coordinates::local_unit(stroke.anchor.depth).map_err(|e| err(e.to_string()))?;
    let pt_d = Double::integer(pt);
    let scale = Double::integer(anchor_unit).div(pt_d).value();
    if !scale.is_finite() || scale <= 0.0 {
        return Ok(false);
    }
    let width = stroke.width * scale;
    if !width.is_finite() || width <= 0.0 {
        return Ok(false);
    }

    let worlds: Vec<photocraft_scene::Point> =
        stroke.points.iter().map(|p| coordinates::point(&stroke.anchor, *p)).collect::<std::result::Result<_, _>>().map_err(|e| err(e.to_string()))?;

    let mut cov = vec![0.0f32; TILE_PIXELS];
    let mut touched = false;

    if width > 2048.0 {
        // High-precision `Double` path for hugely magnified ancestor strokes (depths 6..12 inside depth 0).
        let radius = Double::integer(anchor_unit).mul(Double::scalar(stroke.width * 0.5)).div(pt_d);
        if worlds.len() == 1 {
            if let Some(&w0) = worlds.first() {
                touched |= raster_huge_capsule(w0, w0, ox, oy, pt_d, radius, &mut cov);
            }
        } else {
            for pair in worlds.windows(2) {
                if let [a, b] = pair {
                    touched |= raster_huge_capsule(*a, *b, ox, oy, pt_d, radius, &mut cov);
                }
            }
        }
    } else {
        let pts: Vec<[f64; 2]> =
            worlds.iter().map(|w| [Double::integer(w.x.0 - ox).div(pt_d).value(), Double::integer(w.y.0 - oy).div(pt_d).value()]).collect();
        let outer = if width < 1.0 { 1.0 } else { width * 0.5 + 0.5 };
        let sub_scale = if width < 1.0 { width } else { 1.0 };
        if pts.len() == 1 {
            if let Some(&p) = pts.first() {
                touched |= raster_capsule_f64(p, p, outer, sub_scale, &mut cov);
            }
        } else {
            for pair in pts.windows(2) {
                if let [a, b] = pair {
                    touched |= raster_capsule_f64(*a, *b, outer, sub_scale, &mut cov);
                }
            }
        }
    }

    if !touched {
        return Ok(false);
    }
    let color = stroke.color.rgba;
    for (dst, &c) in layer.iter_mut().zip(&cov) {
        if c <= 0.0 {
            continue;
        }
        if stroke.erase {
            let inv = 1.0 - c;
            for ch in dst.iter_mut() {
                *ch *= inv;
            }
        } else {
            let a = (c * color[3]).clamp(0.0, 1.0);
            let inv = 1.0 - a;
            dst[0] = color[0] * a + dst[0] * inv;
            dst[1] = color[1] * a + dst[1] * inv;
            dst[2] = color[2] * a + dst[2] * inv;
            dst[3] = a + dst[3] * inv;
        }
    }
    Ok(true)
}

fn raster_capsule_f64(a: [f64; 2], b: [f64; 2], outer: f64, sub_scale: f64, cov: &mut [f32]) -> bool {
    let min_x = (a[0].min(b[0]) - outer).floor().clamp(0.0, f64::from(TILE_PHYSICAL)) as usize;
    let max_x = (a[0].max(b[0]) + outer).ceil().clamp(0.0, f64::from(TILE_PHYSICAL)) as usize;
    let min_y = (a[1].min(b[1]) - outer).floor().clamp(0.0, f64::from(TILE_PHYSICAL)) as usize;
    let max_y = (a[1].max(b[1]) + outer).ceil().clamp(0.0, f64::from(TILE_PHYSICAL)) as usize;
    if min_x >= max_x || min_y >= max_y {
        return false;
    }
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let len2 = dx * dx + dy * dy;
    let outer2 = outer * outer;
    let mut touched = false;
    for py in min_y..max_y {
        let y = py as f64 + 0.5;
        let row = py * SIDE;
        for px in min_x..max_x {
            let x = px as f64 + 0.5;
            let d2 = if len2 <= 1e-18 {
                let rx = x - a[0];
                let ry = y - a[1];
                rx * rx + ry * ry
            } else {
                let t = (((x - a[0]) * dx + (y - a[1]) * dy) / len2).clamp(0.0, 1.0);
                let rx = x - (a[0] + dx * t);
                let ry = y - (a[1] + dy * t);
                rx * rx + ry * ry
            };
            if d2 >= outer2 {
                continue;
            }
            let c = ((outer - d2.sqrt()).clamp(0.0, 1.0) * sub_scale) as f32;
            if let Some(slot) = cov.get_mut(row + px)
                && c > *slot
            {
                *slot = c;
                touched = true;
            }
        }
    }
    touched
}

/// Exact `106`-bit `Double` capsule distance against a `260×260` tile for huge ancestor strokes.
fn raster_huge_capsule(wa: photocraft_scene::Point, wb: photocraft_scene::Point, ox: i128, oy: i128, pt_d: Double, radius: Double, cov: &mut [f32]) -> bool {
    let ax = Double::integer(wa.x.0 - ox).div(pt_d);
    let ay = Double::integer(wa.y.0 - oy).div(pt_d);
    let bx = Double::integer(wb.x.0 - ox).div(pt_d);
    let by = Double::integer(wb.y.0 - oy).div(pt_d);
    let dx = bx.sub(ax);
    let dy = by.sub(ay);
    let len2 = dx.mul(dx).add(dy.mul(dy));
    let len_val = len2.value();

    let dist_at = |x: f64, y: f64| -> f64 {
        let px = Double::scalar(x);
        let py = Double::scalar(y);
        let (rx, ry) = if len_val <= 1e-18 {
            (px.sub(ax), py.sub(ay))
        } else {
            let num = px.sub(ax).mul(dx).add(py.sub(ay).mul(dy));
            let t = num.div(len2).value().clamp(0.0, 1.0);
            let td = Double::scalar(t);
            (px.sub(ax.add(dx.mul(td))), py.sub(ay.add(dy.mul(td))))
        };
        let d2 = rx.mul(rx).add(ry.mul(ry)).value();
        if d2 <= 0.0 { 0.0 } else { d2.sqrt() }
    };

    let r_val = radius.value();
    let s = f64::from(TILE_PHYSICAL);
    let c0 = dist_at(0.5, 0.5);
    let c1 = dist_at(s - 0.5, 0.5);
    let c2 = dist_at(0.5, s - 0.5);
    let c3 = dist_at(s - 0.5, s - 0.5);
    let max_c = c0.max(c1).max(c2).max(c3);
    let min_c = c0.min(c1).min(c2).min(c3);
    if max_c <= r_val - 1.0 {
        // Entire `260×260` tile lies strictly inside the convex capsule core.
        cov.fill(1.0);
        return true;
    }
    // Conservative reject if tile diagonal cannot reach the capsule boundary.
    let tile_diag = s * std::f64::consts::SQRT_2;
    if min_c >= r_val + tile_diag + 2.0 {
        return false;
    }

    let outer = r_val + 0.5;
    let mut touched = false;
    for py in 0..SIDE {
        let y = py as f64 + 0.5;
        let row = py * SIDE;
        for px in 0..SIDE {
            let d = dist_at(px as f64 + 0.5, y);
            if d >= outer {
                continue;
            }
            let c = (outer - d).clamp(0.0, 1.0) as f32;
            if let Some(slot) = cov.get_mut(row + px)
                && c > *slot
            {
                *slot = c;
                touched = true;
            }
        }
    }
    touched
}

fn composite_media_in_tile(
    shared: &SharedMedia,
    tile: TileKey,
    elapsed_ms: u64,
    pending_assets: &mut BTreeSet<AssetHash>,
    resolve_media: &mut dyn FnMut(&MediaFrame) -> Result<Option<Arc<RgbaFrame>>>,
    layer: &mut [[f32; 4]],
    rendered_objects: &mut usize,
) -> Result<()> {
    let media = &shared.media;
    let (_, descriptor) = media.frame_at(elapsed_ms).map_err(|e| err(e.to_string()))?;
    let (ox, oy, pt) = tile_origin_ticks(tile);
    let pt_d = Double::integer(pt);
    let p0 = coordinates::point(&media.anchor, media.offset).map_err(|e| err(e.to_string()))?;
    let p1 = coordinates::point(&media.anchor, [media.offset[0] + media.width, media.offset[1] + media.height]).map_err(|e| err(e.to_string()))?;
    let start_x = Double::integer(p0.x.0 - ox).div(pt_d).value();
    let start_y = Double::integer(p0.y.0 - oy).div(pt_d).value();
    let end_x = Double::integer(p1.x.0 - ox).div(pt_d).value();
    let end_y = Double::integer(p1.y.0 - oy).div(pt_d).value();
    let size_x = end_x - start_x;
    let size_y = end_y - start_y;
    if !size_x.is_finite() || !size_y.is_finite() || size_x <= 0.0 || size_y <= 0.0 {
        return Ok(());
    }
    let x0 = start_x.floor().clamp(0.0, f64::from(TILE_PHYSICAL)) as usize;
    let x1 = end_x.ceil().clamp(0.0, f64::from(TILE_PHYSICAL)) as usize;
    let y0 = start_y.floor().clamp(0.0, f64::from(TILE_PHYSICAL)) as usize;
    let y1 = end_y.ceil().clamp(0.0, f64::from(TILE_PHYSICAL)) as usize;
    if x0 >= x1 || y0 >= y1 {
        return Ok(());
    }

    let Some(frame) = resolve_media(descriptor)? else {
        pending_assets.insert(descriptor.hash);
        return Ok(());
    };
    if frame.width == 0 || frame.height == 0 {
        return Ok(());
    }

    for py in y0..y1 {
        let v = (py as f64 + 0.5 - start_y) / size_y;
        if !(0.0..1.0).contains(&v) {
            continue;
        }
        let sy = ((v * f64::from(frame.height)).floor() as usize).min(frame.height.saturating_sub(1) as usize);
        let row_base = py * SIDE;
        let src_row = sy.saturating_mul(frame.width as usize);
        for px in x0..x1 {
            let u = (px as f64 + 0.5 - start_x) / size_x;
            if !(0.0..1.0).contains(&u) {
                continue;
            }
            let sx = ((u * f64::from(frame.width)).floor() as usize).min(frame.width.saturating_sub(1) as usize);
            let off = src_row.saturating_add(sx).saturating_mul(4);
            let Some(src) = frame.rgba.get(off..off + 4) else { continue };
            let a = f32::from(src[3]) / 255.0;
            if a <= 0.0 {
                continue;
            }
            let inv = 1.0 - a;
            if let Some(dst) = layer.get_mut(row_base + px) {
                dst[0] = f32::from(src[0]) / 255.0 * a + dst[0] * inv;
                dst[1] = f32::from(src[1]) / 255.0 * a + dst[1] * inv;
                dst[2] = f32::from(src[2]) / 255.0 * a + dst[2] * inv;
                dst[3] = a + dst[3] * inv;
            }
        }
    }
    *rendered_objects = rendered_objects.saturating_add(1);
    Ok(())
}

fn composite_summary_in_tile(skey: SummaryKey, epoch: &SummaryEpoch, tile: TileKey, layer: &mut [[f32; 4]]) {
    let (ox, oy, pt) = tile_origin_ticks(tile);
    let pt_f = pt as f64;
    if pt_f <= 0.0 {
        return;
    }
    for &(sub_idx, px) in &epoch.cells {
        if px[3] <= 0.0 {
            continue;
        }
        let rect = skey.subcell_rect(sub_idx);
        let sx0 = ((rect.min_x - ox) as f64 / pt_f).clamp(0.0, f64::from(TILE_PHYSICAL));
        let sy0 = ((rect.min_y - oy) as f64 / pt_f).clamp(0.0, f64::from(TILE_PHYSICAL));
        let sx1 = ((rect.max_x - ox) as f64 / pt_f).clamp(0.0, f64::from(TILE_PHYSICAL));
        let sy1 = ((rect.max_y - oy) as f64 / pt_f).clamp(0.0, f64::from(TILE_PHYSICAL));
        if sx0 >= sx1 || sy0 >= sy1 {
            continue;
        }
        let ix0 = sx0.floor() as usize;
        let ix1 = (sx1.ceil() as usize).min(SIDE);
        let iy0 = sy0.floor() as usize;
        let iy1 = (sy1.ceil() as usize).min(SIDE);
        for py in iy0..iy1 {
            let oy_frac = (sy1.min((py + 1) as f64) - sy0.max(py as f64)).clamp(0.0, 1.0) as f32;
            let row = py * SIDE;
            for pxx in ix0..ix1 {
                let ox_frac = (sx1.min((pxx + 1) as f64) - sx0.max(pxx as f64)).clamp(0.0, 1.0) as f32;
                let w = ox_frac * oy_frac;
                if w <= 0.0 {
                    continue;
                }
                let sa = (px[3] * w).clamp(0.0, 1.0);
                let inv = 1.0 - sa;
                if let Some(dst) = layer.get_mut(row + pxx) {
                    dst[0] = (px[0] * w + dst[0] * inv).clamp(0.0, 1.0);
                    dst[1] = (px[1] * w + dst[1] * inv).clamp(0.0, 1.0);
                    dst[2] = (px[2] * w + dst[2] * inv).clamp(0.0, 1.0);
                    dst[3] = (sa + dst[3] * inv).clamp(0.0, 1.0);
                }
            }
        }
    }
}

#[inline]
fn src_over_layer(dst: &mut [[f32; 4]], src: &[[f32; 4]]) {
    for (d, s) in dst.iter_mut().zip(src) {
        let a = s[3];
        if a <= 0.0 {
            continue;
        }
        let inv = 1.0 - a;
        d[0] = (s[0] + d[0] * inv).clamp(0.0, 1.0);
        d[1] = (s[1] + d[1] * inv).clamp(0.0, 1.0);
        d[2] = (s[2] + d[2] * inv).clamp(0.0, 1.0);
        d[3] = (a + d[3] * inv).clamp(0.0, 1.0);
    }
}
