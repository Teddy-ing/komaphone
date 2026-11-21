//! Bounded CPU tile cache, scope-isolated lazy dirty-rect validation, optional
//! non-authoritative disk tile cache, and cooperative-cancel background worker pool.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use photocraft_scene::shared::media::{AssetHash, MediaFrame};
use photocraft_scene::shared::{DeviceId, SharedStroke};
use photocraft_scene::spatial::{
    GPU_LAYERS_CONSERVATIVE, GPU_LAYERS_DESKTOP, MAX_TILE_LOD, MAX_VISIBLE_TILES_CONSERVATIVE, MAX_VISIBLE_TILES_DESKTOP, TILE_PHYSICAL, TileKey, WorldRect,
    tile_bounds_for_rect, viewport_world_rect,
};
use photocraft_scene::{CanvasId, StrokeId, Viewport};

use super::compose::{
    DynamicSplitCache, LiveColdBase, RgbaFrame, TileComposeParams, TileComposeResult, compose_world_tile, compose_world_tile_nonblocking_live,
    compose_world_tile_shared, compose_world_tile_shared_prefer_cached_anim, warm_dynamic_split_bands,
};
use super::index::WorldSceneIndex;
use crate::world_brush::RasterizedBrushStroke;
use crate::{EngineError, Result};

const TILE_RGBA_BYTES: usize = (TILE_PHYSICAL * TILE_PHYSICAL * 4) as usize;
const MAX_DIRTY_LOG: usize = 256;
const MAX_QUEUED_JOBS: usize = 256;
const MAX_QUEUED_DYNAMIC_JOBS: usize = 64;

/// Process-wide monotonic scope generation so distinct `WorldTileManager` instances
/// never collide on `scope_gen` when sharing a `WorldGpu` presenter.
static NEXT_SCOPE_GEN: AtomicU64 = AtomicU64::new(1);

fn err(msg: impl Into<String>) -> EngineError {
    EngineError::Other(msg.into())
}

/// Reconciled GPU + CPU + Disk capacity budget (Desktop 256-layer vs Safe-GPU 128-layer).
/// Total CPU tile + split + dynamic caches stay within `<= 192 MiB` desktop / `<= 112 MiB` conservative.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileBudget {
    pub gpu_layers: u32,
    pub max_visible_tiles: usize,
    pub reserved_fallback_slots: usize,
    pub reserved_dynamic_slots: usize,
    pub max_uploads_per_frame: usize,
    pub staging_ring_bytes: u64,
    pub cpu_cache_bytes: usize,
    pub dynamic_split_bytes: usize,
    pub dynamic_tile_cache_bytes: usize,
    pub decoded_media_bytes: usize,
    pub disk_cache_bytes: u64,
    pub worker_threads: usize,
}

impl TileBudget {
    pub fn desktop() -> Self {
        let cores = available_cores();
        Self {
            gpu_layers: GPU_LAYERS_DESKTOP,
            max_visible_tiles: MAX_VISIBLE_TILES_DESKTOP,
            reserved_fallback_slots: 32,
            reserved_dynamic_slots: 32,
            max_uploads_per_frame: 6,
            staging_ring_bytes: 4 * 1024 * 1024,
            cpu_cache_bytes: 112 * 1024 * 1024,
            dynamic_split_bytes: 64 * 1024 * 1024,
            dynamic_tile_cache_bytes: 16 * 1024 * 1024,
            decoded_media_bytes: 32 * 1024 * 1024,
            disk_cache_bytes: 256 * 1024 * 1024,
            worker_threads: cores.saturating_sub(2).clamp(1, 6),
        }
    }

    pub fn conservative() -> Self {
        let cores = available_cores();
        Self {
            gpu_layers: GPU_LAYERS_CONSERVATIVE,
            max_visible_tiles: MAX_VISIBLE_TILES_CONSERVATIVE,
            reserved_fallback_slots: 16,
            reserved_dynamic_slots: 16,
            max_uploads_per_frame: 4,
            staging_ring_bytes: 2 * 1024 * 1024,
            cpu_cache_bytes: 56 * 1024 * 1024,
            dynamic_split_bytes: 44 * 1024 * 1024,
            dynamic_tile_cache_bytes: 12 * 1024 * 1024,
            decoded_media_bytes: 16 * 1024 * 1024,
            disk_cache_bytes: 128 * 1024 * 1024,
            worker_threads: cores.saturating_sub(2).clamp(1, 4),
        }
    }
}

fn available_cores() -> usize {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get)
    }
    #[cfg(target_arch = "wasm32")]
    {
        1
    }
}

/// Hard isolation boundary separating accounts, activations, canvases, and live vs review modes.
/// Any change to `ScopeKey` immediately cancels jobs and purges cached tiles.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ScopeKey {
    pub activation: u64,
    pub account_hash: [u8; 32],
    pub canvas: CanvasId,
    pub review_end: Option<u64>,
}

impl ScopeKey {
    pub fn new(activation: u64, account_endpoint: &str, profile: &str, device: &str, canvas: CanvasId, review_end: Option<u64>) -> Self {
        let mut h = blake3::Hasher::new();
        h.update(account_endpoint.as_bytes());
        h.update(&[0]);
        h.update(profile.as_bytes());
        h.update(&[0]);
        h.update(device.as_bytes());
        Self { activation, account_hash: *h.finalize().as_bytes(), canvas, review_end }
    }
}

/// Bounded per-scope dirty-rect log (Codex Correction #4).
/// Preserves unaffected tiles across edits in O(1) push time even for giant dirty rects.
#[derive(Clone, Debug)]
pub struct DirtyLog {
    edit_seq: u64,
    full_invalidation_seq: u64,
    rects: VecDeque<(u64, WorldRect)>,
}

impl Default for DirtyLog {
    fn default() -> Self {
        Self { edit_seq: 1, full_invalidation_seq: 1, rects: VecDeque::new() }
    }
}

impl DirtyLog {
    pub fn reset(&mut self) {
        self.edit_seq = self.edit_seq.saturating_add(1);
        self.full_invalidation_seq = self.edit_seq;
        self.rects.clear();
    }

    pub fn current_seq(&self) -> u64 {
        self.edit_seq
    }

    pub fn push_rect(&mut self, rect: WorldRect) -> u64 {
        self.edit_seq = self.edit_seq.saturating_add(1);
        self.rects.push_back((self.edit_seq, rect));
        while self.rects.len() > MAX_DIRTY_LOG {
            if let Some((evicted_seq, _)) = self.rects.pop_front() {
                self.full_invalidation_seq = self.full_invalidation_seq.max(evicted_seq);
            }
        }
        self.edit_seq
    }

    pub fn invalidate_all(&mut self) -> u64 {
        self.edit_seq = self.edit_seq.saturating_add(1);
        self.full_invalidation_seq = self.edit_seq;
        self.rects.clear();
        self.edit_seq
    }

    /// Check whether a cached tile validated at `validated_seq` is still valid for `current_seq()`.
    /// O(new dirty rects) and never enumerates offscreen tiles.
    pub fn is_tile_valid(&self, tile: TileKey, validated_seq: u64) -> bool {
        if validated_seq < self.full_invalidation_seq {
            return false;
        }
        if validated_seq >= self.edit_seq {
            return true;
        }
        let phys = tile.physical_rect();
        for &(seq, rect) in &self.rects {
            if seq > validated_seq && rect.intersects(&phys) {
                return false;
            }
        }
        true
    }
}

/// Cached CPU RGBA8 tile (`260×260` premultiplied) with per-tile lazy validity sequence.
#[derive(Clone)]
pub struct CachedCpuTile {
    pub rgba8: Arc<Vec<u8>>,
    pub validated_seq: u64,
    pub pending_assets: usize,
    pub device_layers: usize,
    pub rendered_objects: usize,
}

/// Cached CPU RGBA8 dynamic tile (animated media and/or peer previews) prepared asynchronously
/// off the UI thread, fenced by `validated_seq`, `dynamic_sig`, and monotonic `request_gen`.
#[derive(Clone)]
pub struct CachedDynamicTile {
    pub rgba8: Arc<Vec<u8>>,
    pub validated_seq: u64,
    pub dynamic_sig: u64,
    pub request_gen: u64,
    pub pending_assets: usize,
}

#[derive(Clone, Debug, Default)]
pub struct TileManagerStats {
    pub cpu_cache_tiles: usize,
    pub cpu_cache_bytes: usize,
    pub dynamic_tile_cache_bytes: usize,
    pub dynamic_split_bytes: usize,
    pub pending_jobs: usize,
    pub pending_dynamic_jobs: usize,
    pub disk_hits: u64,
    pub disk_writes: u64,
    pub preserved_tiles_validated: u64,
}

/// Decoded media frame LRU shared with background tile workers.
struct DecodedMediaStore {
    max_bytes: usize,
    used_bytes: usize,
    entries: BTreeMap<AssetHash, (Arc<RgbaFrame>, usize)>,
    order: VecDeque<AssetHash>,
}

impl DecodedMediaStore {
    fn new(max_bytes: usize) -> Self {
        Self { max_bytes, used_bytes: 0, entries: BTreeMap::new(), order: VecDeque::new() }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.used_bytes = 0;
    }

    fn get(&self, hash: &AssetHash) -> Option<Arc<RgbaFrame>> {
        self.entries.get(hash).map(|(f, _)| Arc::clone(f))
    }

    fn insert(&mut self, hash: AssetHash, frame: Arc<RgbaFrame>) {
        let bytes = frame.rgba.len();
        if bytes > self.max_bytes {
            return;
        }
        if self.entries.contains_key(&hash) {
            return;
        }
        while self.used_bytes.saturating_add(bytes) > self.max_bytes {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some((_, old_b)) = self.entries.remove(&oldest) {
                self.used_bytes = self.used_bytes.saturating_sub(old_b);
            }
        }
        self.entries.insert(hash, (frame, bytes));
        self.order.push_back(hash);
        self.used_bytes = self.used_bytes.saturating_add(bytes);
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod pool {
    use super::{
        Arc, AtomicBool, AtomicU64, BTreeMap, BTreeSet, DecodedMediaStore, DeviceId, DynamicSplitCache, MAX_QUEUED_DYNAMIC_JOBS, MAX_QUEUED_JOBS, Ordering,
        SharedStroke, TILE_RGBA_BYTES, TileComposeParams, TileKey, VecDeque, WorldSceneIndex, compose_world_tile, compose_world_tile_shared,
        warm_dynamic_split_bands,
    };
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::sync::{Condvar, Mutex};

    const DISK_MAGIC: &[u8; 8] = b"KTILE01\0";
    const KIND_STATIC: u8 = 0;
    const KIND_DYNAMIC: u8 = 1;
    const KIND_WARM_SPLIT: u8 = 2;

    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    pub(super) enum JobPriority {
        LiveSplitWarm(u128),
        ColdCoarse(u128),
        AnimatedMedia(u128),
        VisibleTarget(u128),
        LiveSplitPrefetch(u128),
        Prefetch(u128),
    }

    pub(super) enum JobKind {
        Static,
        Dynamic { elapsed_ms: u64, dynamic_sig: u64, request_gen: u64, previews: Arc<[SharedStroke]> },
        WarmSplitBands { elapsed_ms: u64, warm_sig: u64, split_devices: Arc<[DeviceId]> },
    }

    pub(super) struct TileJob {
        pub priority: JobPriority,
        pub tile: TileKey,
        pub scope_gen: u64,
        pub view_gen: u64,
        pub validated_seq: u64,
        pub scene: Arc<WorldSceneIndex>,
        pub cancel: Arc<AtomicBool>,
        pub kind: JobKind,
    }

    pub(super) struct CompletedTile {
        pub tile: TileKey,
        pub scope_gen: u64,
        pub validated_seq: u64,
        pub dynamic_sig: Option<(u64, u64)>,
        pub split_warm_only: bool,
        pub rgba8: Arc<Vec<u8>>,
        pub pending_assets: usize,
        pub device_layers: usize,
        pub rendered_objects: usize,
        pub from_disk: bool,
    }

    struct SharedState {
        pending: Vec<TileJob>,
        completed: VecDeque<CompletedTile>,
        active_tiles: BTreeSet<TileKey>,
        active_dynamic_tiles: BTreeSet<TileKey>,
        active_warm_tiles: BTreeSet<TileKey>,
        active_jobs: BTreeSet<(TileKey, u64, u8, u64)>,
        latest_dynamic_gen: BTreeMap<TileKey, u64>,
        closed: bool,
    }

    struct ActiveJobGuard {
        state: Arc<(Mutex<SharedState>, Condvar)>,
        key: (TileKey, u64, u8, u64),
    }

    impl Drop for ActiveJobGuard {
        fn drop(&mut self) {
            let mut guard = self.state.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.active_jobs.remove(&self.key);
        }
    }

    pub(super) struct WorkerPool {
        state: Arc<(Mutex<SharedState>, Condvar)>,
        media: Arc<Mutex<DecodedMediaStore>>,
        disk_dir: Option<PathBuf>,
        disk_max_bytes: u64,
        handles: Vec<std::thread::JoinHandle<()>>,
    }

    impl WorkerPool {
        pub(super) fn start(
            threads: usize,
            scope_gen: Arc<AtomicU64>,
            view_gen: Arc<AtomicU64>,
            media: Arc<Mutex<DecodedMediaStore>>,
            split_cache: Arc<Mutex<DynamicSplitCache>>,
            disk_dir: Option<PathBuf>,
            disk_max_bytes: u64,
        ) -> Self {
            let state = Arc::new((
                Mutex::new(SharedState {
                    pending: Vec::new(),
                    completed: VecDeque::new(),
                    active_tiles: BTreeSet::new(),
                    active_dynamic_tiles: BTreeSet::new(),
                    active_warm_tiles: BTreeSet::new(),
                    active_jobs: BTreeSet::new(),
                    latest_dynamic_gen: BTreeMap::new(),
                    closed: false,
                }),
                Condvar::new(),
            ));
            let mut handles = Vec::with_capacity(threads);
            for i in 0..threads.max(1) {
                let st = Arc::clone(&state);
                let sg = Arc::clone(&scope_gen);
                let vg = Arc::clone(&view_gen);
                let med = Arc::clone(&media);
                let sc = Arc::clone(&split_cache);
                let ddir = disk_dir.clone();
                if let Ok(handle) =
                    std::thread::Builder::new().name(format!("komaphone-tile-{i}")).spawn(move || worker_loop(st, sg, vg, med, sc, ddir, disk_max_bytes))
                {
                    handles.push(handle);
                }
            }
            Self { state, media, disk_dir, disk_max_bytes, handles }
        }

        pub(super) fn set_active_tiles(&self, active: BTreeSet<TileKey>) {
            let mut guard = self.state.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let SharedState { pending, active_tiles, active_jobs, .. } = &mut *guard;
            *active_tiles = active;
            pending.retain(|j| {
                let keep = match j.kind {
                    JobKind::Dynamic { .. } | JobKind::WarmSplitBands { .. } => true,
                    JobKind::Static => active_tiles.contains(&j.tile) || matches!(j.priority, JobPriority::ColdCoarse(_)),
                };
                if !keep {
                    j.cancel.store(true, Ordering::Relaxed);
                    active_jobs.remove(&(j.tile, j.validated_seq, KIND_STATIC, 0));
                }
                keep
            });
        }

        pub(super) fn set_active_dynamic_tiles(&self, active: BTreeSet<TileKey>) {
            let mut guard = self.state.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let SharedState { pending, active_dynamic_tiles, active_jobs, .. } = &mut *guard;
            *active_dynamic_tiles = active;
            pending.retain(|j| match j.kind {
                JobKind::Static | JobKind::WarmSplitBands { .. } => true,
                JobKind::Dynamic { dynamic_sig, .. } => {
                    let keep = active_dynamic_tiles.contains(&j.tile);
                    if !keep {
                        j.cancel.store(true, Ordering::Relaxed);
                        active_jobs.remove(&(j.tile, j.validated_seq, KIND_DYNAMIC, dynamic_sig));
                    }
                    keep
                }
            });
        }

        pub(super) fn set_active_warm_tiles(&self, active: BTreeSet<TileKey>) {
            let mut guard = self.state.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let SharedState { pending, active_warm_tiles, active_jobs, .. } = &mut *guard;
            *active_warm_tiles = active;
            pending.retain(|j| match j.kind {
                JobKind::Static | JobKind::Dynamic { .. } => true,
                JobKind::WarmSplitBands { warm_sig, .. } => {
                    let keep = active_warm_tiles.contains(&j.tile);
                    if !keep {
                        j.cancel.store(true, Ordering::Relaxed);
                        active_jobs.remove(&(j.tile, j.validated_seq, KIND_WARM_SPLIT, warm_sig));
                    }
                    keep
                }
            });
        }

        pub(super) fn cancel_all(&self) {
            let mut guard = self.state.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            for job in guard.pending.drain(..) {
                job.cancel.store(true, Ordering::Relaxed);
            }
            guard.completed.clear();
            guard.active_tiles.clear();
            guard.active_dynamic_tiles.clear();
            guard.active_warm_tiles.clear();
            guard.active_jobs.clear();
            guard.latest_dynamic_gen.clear();
        }

        pub(super) fn enqueue(&self, job: TileJob) {
            let mut guard = self.state.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match &job.kind {
                JobKind::Static => {
                    let static_queued = guard.pending.iter().filter(|j| matches!(j.kind, JobKind::Static)).count();
                    if static_queued >= MAX_QUEUED_JOBS {
                        return;
                    }
                    let key = (job.tile, job.validated_seq, KIND_STATIC, 0);
                    if !guard.active_jobs.insert(key) {
                        return;
                    }
                    guard.pending.push(job);
                    self.state.1.notify_one();
                }
                JobKind::Dynamic { dynamic_sig, request_gen, .. } => {
                    let sig = *dynamic_sig;
                    let rgen = *request_gen;
                    let tile = job.tile;
                    let key = (tile, job.validated_seq, KIND_DYNAMIC, sig);
                    if guard.active_jobs.contains(&key) {
                        return;
                    }
                    // Coalesce/cancel any older queued dynamic job for the same tile so animation
                    // ticks and rapid peer updates never build an unbounded stale backlog.
                    let SharedState { pending, active_jobs, latest_dynamic_gen, .. } = &mut *guard;
                    pending.retain(|j| match j.kind {
                        JobKind::Dynamic { dynamic_sig: old_sig, .. } if j.tile == tile => {
                            j.cancel.store(true, Ordering::Relaxed);
                            active_jobs.remove(&(j.tile, j.validated_seq, KIND_DYNAMIC, old_sig));
                            false
                        }
                        _ => true,
                    });
                    let dyn_queued = pending.iter().filter(|j| matches!(j.kind, JobKind::Dynamic { .. })).count();
                    if dyn_queued >= MAX_QUEUED_DYNAMIC_JOBS {
                        return;
                    }
                    latest_dynamic_gen.insert(tile, rgen);
                    active_jobs.insert(key);
                    pending.push(job);
                    self.state.1.notify_one();
                }
                JobKind::WarmSplitBands { warm_sig, .. } => {
                    let sig = *warm_sig;
                    let tile = job.tile;
                    let key = (tile, job.validated_seq, KIND_WARM_SPLIT, sig);
                    if guard.active_jobs.contains(&key) {
                        // Promote an already-queued prefetch job to urgent if needed.
                        if matches!(job.priority, JobPriority::LiveSplitWarm(_))
                            && let Some(existing) =
                                guard.pending.iter_mut().find(|j| j.tile == tile && matches!(j.kind, JobKind::WarmSplitBands { warm_sig: s, .. } if s == sig))
                        {
                            existing.priority = existing.priority.min(job.priority);
                        }
                        return;
                    }
                    let SharedState { pending, active_jobs, .. } = &mut *guard;
                    pending.retain(|j| match j.kind {
                        JobKind::WarmSplitBands { warm_sig: old_sig, .. } if j.tile == tile => {
                            j.cancel.store(true, Ordering::Relaxed);
                            active_jobs.remove(&(j.tile, j.validated_seq, KIND_WARM_SPLIT, old_sig));
                            false
                        }
                        _ => true,
                    });
                    let warm_queued = pending.iter().filter(|j| matches!(j.kind, JobKind::WarmSplitBands { .. })).count();
                    if warm_queued >= MAX_QUEUED_DYNAMIC_JOBS {
                        return;
                    }
                    active_jobs.insert(key);
                    pending.push(job);
                    self.state.1.notify_one();
                }
            }
        }

        pub(super) fn pending_count(&self) -> usize {
            self.state.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active_jobs.len()
        }

        pub(super) fn pending_dynamic_count(&self) -> usize {
            self.state.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active_jobs.iter().filter(|(_, _, k, _)| *k != KIND_STATIC).count()
        }

        pub(super) fn drain_completed(&self) -> Vec<CompletedTile> {
            let mut guard = self.state.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.completed.drain(..).collect()
        }

        pub(super) fn media_store(&self) -> &Arc<Mutex<DecodedMediaStore>> {
            &self.media
        }

        pub(super) fn disk_dir(&self) -> Option<&Path> {
            self.disk_dir.as_deref()
        }

        pub(super) fn disk_max_bytes(&self) -> u64 {
            self.disk_max_bytes
        }
    }

    impl Drop for WorkerPool {
        fn drop(&mut self) {
            {
                let mut guard = self.state.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                guard.closed = true;
                for job in guard.pending.drain(..) {
                    job.cancel.store(true, Ordering::Relaxed);
                }
                self.state.1.notify_all();
            }
            for h in self.handles.drain(..) {
                let _ = h.join();
            }
        }
    }

    fn worker_loop(
        state: Arc<(Mutex<SharedState>, Condvar)>,
        scope_gen: Arc<AtomicU64>,
        view_gen: Arc<AtomicU64>,
        media: Arc<Mutex<DecodedMediaStore>>,
        split_cache: Arc<Mutex<DynamicSplitCache>>,
        disk_dir: Option<PathBuf>,
        disk_max_bytes: u64,
    ) {
        loop {
            let job = {
                let mut guard = state.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                while guard.pending.is_empty() && !guard.closed {
                    guard = state.1.wait(guard).unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                if guard.closed {
                    return;
                }
                let best_idx = guard.pending.iter().enumerate().min_by_key(|(_, j)| j.priority).map(|(i, _)| i);
                let Some(idx) = best_idx else { continue };
                guard.pending.swap_remove(idx)
            };
            let (kind_tag, sig_key) = match &job.kind {
                JobKind::Static => (KIND_STATIC, 0),
                JobKind::Dynamic { dynamic_sig, .. } => (KIND_DYNAMIC, *dynamic_sig),
                JobKind::WarmSplitBands { warm_sig, .. } => (KIND_WARM_SPLIT, *warm_sig),
            };
            let _active_guard = ActiveJobGuard { state: Arc::clone(&state), key: (job.tile, job.validated_seq, kind_tag, sig_key) };

            if job.cancel.load(Ordering::Relaxed) || scope_gen.load(Ordering::Acquire) != job.scope_gen {
                continue;
            }
            match &job.kind {
                JobKind::Static => {
                    if view_gen.load(Ordering::Acquire) != job.view_gen && !matches!(job.priority, JobPriority::ColdCoarse(_)) {
                        let guard = state.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        if !guard.active_tiles.contains(&job.tile) {
                            continue;
                        }
                    }

                    let query = job.scene.query_tile(job.tile);
                    let content_hash = job.scene.tile_content_hash(job.tile, &query);
                    if let Some(dir) = disk_dir.as_deref()
                        && let Some(rgba8) = read_disk_tile(dir, &content_hash)
                    {
                        if scope_gen.load(Ordering::Acquire) == job.scope_gen {
                            let mut guard = state.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            if !guard.closed {
                                guard.completed.push_back(CompletedTile {
                                    tile: job.tile,
                                    scope_gen: job.scope_gen,
                                    validated_seq: job.validated_seq,
                                    dynamic_sig: None,
                                    split_warm_only: false,
                                    rgba8: Arc::new(rgba8),
                                    pending_assets: 0,
                                    device_layers: 0,
                                    rendered_objects: 0,
                                    from_disk: true,
                                });
                            }
                        }
                        continue;
                    }

                    let params = TileComposeParams {
                        index: &job.scene,
                        tile: job.tile,
                        scope_gen: job.scope_gen,
                        validated_seq: job.validated_seq,
                        elapsed_ms: 0,
                        previews: &[],
                        live_brush: None,
                        allow_parallel_brush: false,
                        cancel: Some(&job.cancel),
                    };
                    let result = compose_world_tile(&params, None, &mut |frame| {
                        let store = media.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        Ok(store.get(&frame.hash))
                    });
                    let Ok(composed) = result else {
                        continue;
                    };
                    if composed.cancelled || scope_gen.load(Ordering::Acquire) != job.scope_gen {
                        continue;
                    }
                    if composed.pending_assets.is_empty()
                        && let Some(dir) = disk_dir.as_deref()
                    {
                        write_disk_tile(dir, &content_hash, &composed.rgba8, disk_max_bytes);
                    }
                    let mut guard = state.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if !guard.closed && scope_gen.load(Ordering::Acquire) == job.scope_gen {
                        guard.completed.push_back(CompletedTile {
                            tile: job.tile,
                            scope_gen: job.scope_gen,
                            validated_seq: job.validated_seq,
                            dynamic_sig: None,
                            split_warm_only: false,
                            rgba8: Arc::new(composed.rgba8),
                            pending_assets: composed.pending_assets.len(),
                            device_layers: composed.device_layers,
                            rendered_objects: composed.rendered_objects,
                            from_disk: false,
                        });
                    }
                }
                JobKind::Dynamic { elapsed_ms, dynamic_sig, request_gen, previews } => {
                    {
                        let guard = state.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        if !guard.active_dynamic_tiles.contains(&job.tile) {
                            continue;
                        }
                        if guard.latest_dynamic_gen.get(&job.tile).copied().unwrap_or(0) > *request_gen {
                            continue;
                        }
                    }
                    let params = TileComposeParams {
                        index: &job.scene,
                        tile: job.tile,
                        scope_gen: job.scope_gen,
                        validated_seq: job.validated_seq,
                        elapsed_ms: *elapsed_ms,
                        previews,
                        live_brush: None,
                        allow_parallel_brush: false,
                        cancel: Some(&job.cancel),
                    };
                    let result = compose_world_tile_shared(&params, Some(&split_cache), &mut |frame| {
                        let store = media.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        Ok(store.get(&frame.hash))
                    });
                    let Ok(composed) = result else {
                        continue;
                    };
                    if composed.cancelled || scope_gen.load(Ordering::Acquire) != job.scope_gen {
                        continue;
                    }
                    let mut guard = state.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if !guard.closed && scope_gen.load(Ordering::Acquire) == job.scope_gen {
                        guard.completed.push_back(CompletedTile {
                            tile: job.tile,
                            scope_gen: job.scope_gen,
                            validated_seq: job.validated_seq,
                            dynamic_sig: Some((*dynamic_sig, *request_gen)),
                            split_warm_only: false,
                            rgba8: Arc::new(composed.rgba8),
                            pending_assets: composed.pending_assets.len(),
                            device_layers: composed.device_layers,
                            rendered_objects: composed.rendered_objects,
                            from_disk: false,
                        });
                    }
                }
                JobKind::WarmSplitBands { elapsed_ms, split_devices, .. } => {
                    {
                        let guard = state.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        if !guard.active_warm_tiles.contains(&job.tile) {
                            continue;
                        }
                    }
                    let params = TileComposeParams {
                        index: &job.scene,
                        tile: job.tile,
                        scope_gen: job.scope_gen,
                        validated_seq: job.validated_seq,
                        elapsed_ms: *elapsed_ms,
                        previews: &[],
                        live_brush: None,
                        allow_parallel_brush: false,
                        cancel: Some(&job.cancel),
                    };
                    let ok = warm_dynamic_split_bands(&params, split_devices, &split_cache, &mut |frame| {
                        let store = media.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        Ok(store.get(&frame.hash))
                    });
                    if ok.unwrap_or(false) && scope_gen.load(Ordering::Acquire) == job.scope_gen {
                        let mut guard = state.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        if !guard.closed {
                            guard.completed.push_back(CompletedTile {
                                tile: job.tile,
                                scope_gen: job.scope_gen,
                                validated_seq: job.validated_seq,
                                dynamic_sig: None,
                                split_warm_only: true,
                                rgba8: Arc::new(Vec::new()),
                                pending_assets: 0,
                                device_layers: 0,
                                rendered_objects: 0,
                                from_disk: false,
                            });
                        }
                    }
                }
            }
        }
    }

    fn hex_hash(hash: &[u8; 32]) -> String {
        let mut s = String::with_capacity(64);
        for b in hash {
            use std::fmt::Write;
            let _ = write!(&mut s, "{b:02x}");
        }
        s
    }

    pub(super) fn read_disk_tile(dir: &Path, hash: &[u8; 32]) -> Option<Vec<u8>> {
        let path = dir.join(format!("{}.ktile", hex_hash(hash)));
        let raw = std::fs::read(&path).ok()?;
        if raw.len() < 40 || raw.get(..8) != Some(DISK_MAGIC.as_slice()) {
            return None;
        }
        let expected = raw.get(8..40)?;
        let compressed = raw.get(40..)?;
        let mut decoder = flate2::read::ZlibDecoder::new(compressed).take((TILE_RGBA_BYTES + 1) as u64);
        let mut out = Vec::with_capacity(TILE_RGBA_BYTES);
        decoder.read_to_end(&mut out).ok()?;
        if out.len() != TILE_RGBA_BYTES {
            return None;
        }
        let actual = blake3::hash(&out);
        if actual.as_bytes().as_slice() != expected {
            return None;
        }
        Some(out)
    }

    pub(super) fn write_disk_tile(dir: &Path, hash: &[u8; 32], rgba8: &[u8], max_bytes: u64) {
        if max_bytes == 0 || rgba8.len() != TILE_RGBA_BYTES {
            return;
        }
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
        use std::io::Write;
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        if enc.write_all(rgba8).is_err() {
            return;
        }
        let Ok(compressed) = enc.finish() else { return };
        let digest = blake3::hash(rgba8);
        let mut payload = Vec::with_capacity(40 + compressed.len());
        payload.extend_from_slice(DISK_MAGIC);
        payload.extend_from_slice(digest.as_bytes());
        payload.extend_from_slice(&compressed);

        let name = hex_hash(hash);
        let tmp = dir.join(format!("{name}.{}.tmp", std::process::id()));
        let dst = dir.join(format!("{name}.ktile"));
        if std::fs::write(&tmp, &payload).is_ok() {
            let _ = std::fs::rename(&tmp, &dst);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
        prune_disk_dir_if_needed(dir, max_bytes);
    }

    fn prune_disk_dir_if_needed(dir: &Path, max_bytes: u64) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut files = Vec::new();
        let mut total = 0u64;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("ktile") {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                let len = meta.len();
                total = total.saturating_add(len);
                let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                files.push((modified, len, path));
            }
        }
        if total <= max_bytes {
            return;
        }
        files.sort_by_key(|(m, _, _)| *m);
        let target = max_bytes.saturating_mul(3) / 4;
        for (_, len, path) in files {
            if total <= target {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(len);
            }
        }
    }
}

/// Manages world-space LOD tiles across scope boundaries, dirty-rect edits,
/// background workers, CPU LRU cache, async dynamic tile cache, dynamic split cache,
/// and optional disk cache.
pub struct WorldTileManager {
    budget: TileBudget,
    scope: Option<ScopeKey>,
    scope_gen: Arc<AtomicU64>,
    view_gen: Arc<AtomicU64>,
    dynamic_req_gen: u64,
    split_warm_gen: u64,
    scene: Option<Arc<WorldSceneIndex>>,
    dirty_log: DirtyLog,
    cpu_cache: BTreeMap<TileKey, CachedCpuTile>,
    cpu_lru: VecDeque<TileKey>,
    cpu_bytes: usize,
    dynamic_cache: BTreeMap<TileKey, CachedDynamicTile>,
    dynamic_lru: VecDeque<TileKey>,
    dynamic_bytes: usize,
    split_cache: Arc<std::sync::Mutex<DynamicSplitCache>>,
    #[cfg(not(target_arch = "wasm32"))]
    pool: pool::WorkerPool,
    #[cfg(target_arch = "wasm32")]
    media_wasm: DecodedMediaStore,
    stats: TileManagerStats,
}

/// One off-thread `DynamicSplitCache` warming request: `(tile, warm_sig, elapsed_ms, split_devices)`,
/// where `split_devices` contains ONLY the exact preview devices intersecting `tile.physical_rect()`.
pub type SplitWarmJob = (TileKey, u64, u64, Arc<[DeviceId]>);

impl WorldTileManager {
    pub fn new(budget: TileBudget, disk_cache_dir: Option<std::path::PathBuf>) -> Self {
        let scope_gen = Arc::new(AtomicU64::new(NEXT_SCOPE_GEN.fetch_add(1, Ordering::AcqRel)));
        let view_gen = Arc::new(AtomicU64::new(1));
        let split_cache = Arc::new(std::sync::Mutex::new(DynamicSplitCache::new(budget.dynamic_split_bytes)));
        #[cfg(not(target_arch = "wasm32"))]
        let pool = {
            let media = Arc::new(std::sync::Mutex::new(DecodedMediaStore::new(budget.decoded_media_bytes)));
            pool::WorkerPool::start(
                budget.worker_threads,
                Arc::clone(&scope_gen),
                Arc::clone(&view_gen),
                media,
                Arc::clone(&split_cache),
                disk_cache_dir,
                budget.disk_cache_bytes,
            )
        };
        #[cfg(target_arch = "wasm32")]
        let _ = disk_cache_dir;
        Self {
            budget,
            scope: None,
            scope_gen,
            view_gen,
            dynamic_req_gen: 1,
            split_warm_gen: 0,
            scene: None,
            dirty_log: DirtyLog::default(),
            cpu_cache: BTreeMap::new(),
            cpu_lru: VecDeque::new(),
            cpu_bytes: 0,
            dynamic_cache: BTreeMap::new(),
            dynamic_lru: VecDeque::new(),
            dynamic_bytes: 0,
            split_cache,
            #[cfg(not(target_arch = "wasm32"))]
            pool,
            #[cfg(target_arch = "wasm32")]
            media_wasm: DecodedMediaStore::new(budget.decoded_media_bytes),
            stats: TileManagerStats::default(),
        }
    }

    pub fn budget(&self) -> TileBudget {
        self.budget
    }

    pub fn scope_gen(&self) -> u64 {
        self.scope_gen.load(Ordering::Acquire)
    }

    pub fn edit_seq(&self) -> u64 {
        self.dirty_log.current_seq()
    }

    pub fn split_warm_gen(&self) -> u64 {
        self.split_warm_gen
    }

    pub fn scene(&self) -> Option<&Arc<WorldSceneIndex>> {
        self.scene.as_ref()
    }

    pub fn stats(&self) -> TileManagerStats {
        #[cfg(not(target_arch = "wasm32"))]
        let (pending_jobs, pending_dynamic_jobs) = (self.pool.pending_count(), self.pool.pending_dynamic_count());
        #[cfg(target_arch = "wasm32")]
        let (pending_jobs, pending_dynamic_jobs) = (0, 0);
        TileManagerStats {
            cpu_cache_tiles: self.cpu_cache.len(),
            cpu_cache_bytes: self.cpu_bytes,
            dynamic_tile_cache_bytes: self.dynamic_bytes,
            dynamic_split_bytes: self.split_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).used_bytes(),
            pending_jobs,
            pending_dynamic_jobs,
            disk_hits: self.stats.disk_hits,
            disk_writes: self.stats.disk_writes,
            preserved_tiles_validated: self.stats.preserved_tiles_validated,
        }
    }

    /// Hard purge on account, activation, canvas, or live-vs-review mode change (Codex Correction #4).
    pub fn set_scope(&mut self, scope: ScopeKey, scene: Arc<WorldSceneIndex>) {
        if self.scope.as_ref() != Some(&scope) {
            self.purge_scope();
            self.scope = Some(scope);
            self.scene = Some(scene);
        } else {
            self.update_scene(scene, None);
        }
    }

    /// Immediately cancel all jobs and discard all cached tiles (called on account/canvas/activation switch).
    pub fn purge_scope(&mut self) {
        self.scope_gen.store(NEXT_SCOPE_GEN.fetch_add(1, Ordering::AcqRel), Ordering::Release);
        self.view_gen.fetch_add(1, Ordering::AcqRel);
        self.dynamic_req_gen = self.dynamic_req_gen.saturating_add(1);
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.pool.cancel_all();
            self.pool.media_store().lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
        }
        #[cfg(target_arch = "wasm32")]
        self.media_wasm.clear();
        self.scope = None;
        self.scene = None;
        self.dirty_log.reset();
        self.cpu_cache.clear();
        self.cpu_lru.clear();
        self.cpu_bytes = 0;
        self.dynamic_cache.clear();
        self.dynamic_lru.clear();
        self.dynamic_bytes = 0;
        self.split_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
    }

    /// Update the scene snapshot within the SAME `ScopeKey`.
    /// Passing `Some(dirty_rects)` preserves 100% of unaffected cached tiles!
    /// Passing `None` marks a full content invalidation without changing the scope identity.
    pub fn update_scene(&mut self, scene: Arc<WorldSceneIndex>, dirty_rects: Option<&[WorldRect]>) {
        self.scene = Some(scene);
        match dirty_rects {
            Some(rects) => {
                for &rect in rects {
                    self.dirty_log.push_rect(rect);
                }
            }
            None => {
                self.dirty_log.invalidate_all();
                self.split_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
            }
        }
    }

    /// Register a decoded media frame so background tile workers can composite it lock-free.
    pub fn register_media_frame(&mut self, hash: AssetHash, frame: Arc<RgbaFrame>) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.pool.media_store().lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(hash, frame);
        }
        #[cfg(target_arch = "wasm32")]
        self.media_wasm.insert(hash, frame);
    }

    pub fn has_media_frame(&self, hash: &AssetHash) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.pool.media_store().lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(hash).is_some()
        }
        #[cfg(target_arch = "wasm32")]
        self.media_wasm.get(hash).is_some()
    }

    /// Check whether a tile is valid in the CPU cache, lazily advancing `validated_seq`
    /// if no recent dirty rects intersect it (preserving unaffected tiles across edits!).
    pub fn get_valid_cpu_tile(&mut self, tile: TileKey) -> Option<CachedCpuTile> {
        let cur_seq = self.dirty_log.current_seq();
        let valid = self.cpu_cache.get(&tile).is_some_and(|c| self.dirty_log.is_tile_valid(tile, c.validated_seq));
        if !valid {
            return None;
        }
        if let Some(entry) = self.cpu_cache.get_mut(&tile) {
            if entry.validated_seq < cur_seq {
                entry.validated_seq = cur_seq;
                self.stats.preserved_tiles_validated = self.stats.preserved_tiles_validated.saturating_add(1);
            }
            let out = entry.clone();
            self.touch_lru(tile);
            Some(out)
        } else {
            None
        }
    }

    /// Return any cached tile for `tile` regardless of `validated_seq` (used strictly for
    /// temporary fallback during catch-up step swaps or coarse ancestors).
    pub fn get_any_cpu_tile(&self, tile: TileKey) -> Option<CachedCpuTile> {
        self.cpu_cache.get(&tile).cloned()
    }

    pub fn is_tile_valid_at(&self, tile: TileKey, validated_seq: u64) -> bool {
        self.dirty_log.is_tile_valid(tile, validated_seq)
    }

    fn touch_lru(&mut self, tile: TileKey) {
        if let Some(pos) = self.cpu_lru.iter().position(|t| *t == tile) {
            self.cpu_lru.remove(pos);
        }
        self.cpu_lru.push_back(tile);
    }

    fn insert_cpu_tile(&mut self, tile: TileKey, entry: CachedCpuTile) {
        if TILE_RGBA_BYTES > self.budget.cpu_cache_bytes {
            return;
        }
        let already = self.cpu_cache.contains_key(&tile);
        if !already {
            while self.cpu_bytes.saturating_add(TILE_RGBA_BYTES) > self.budget.cpu_cache_bytes {
                let Some(oldest) = self.cpu_lru.pop_front() else {
                    break;
                };
                if self.cpu_cache.remove(&oldest).is_some() {
                    self.cpu_bytes = self.cpu_bytes.saturating_sub(TILE_RGBA_BYTES);
                }
            }
            self.cpu_bytes = self.cpu_bytes.saturating_add(TILE_RGBA_BYTES);
        }
        self.cpu_cache.insert(tile, entry);
        self.touch_lru(tile);
    }

    fn touch_dynamic_lru(&mut self, tile: TileKey) {
        if let Some(pos) = self.dynamic_lru.iter().position(|t| *t == tile) {
            self.dynamic_lru.remove(pos);
        }
        self.dynamic_lru.push_back(tile);
    }

    fn insert_dynamic_tile(&mut self, tile: TileKey, entry: CachedDynamicTile) {
        if TILE_RGBA_BYTES > self.budget.dynamic_tile_cache_bytes {
            return;
        }
        // Monotonic fencing: never let an older async dynamic result overwrite a newer request_gen
        // that is still valid for the current edit sequence.
        if let Some(existing) = self.dynamic_cache.get(&tile)
            && self.dirty_log.is_tile_valid(tile, existing.validated_seq)
            && entry.request_gen < existing.request_gen
        {
            return;
        }
        let already = self.dynamic_cache.contains_key(&tile);
        if !already {
            while self.dynamic_bytes.saturating_add(TILE_RGBA_BYTES) > self.budget.dynamic_tile_cache_bytes {
                let Some(oldest) = self.dynamic_lru.pop_front() else {
                    break;
                };
                if self.dynamic_cache.remove(&oldest).is_some() {
                    self.dynamic_bytes = self.dynamic_bytes.saturating_sub(TILE_RGBA_BYTES);
                }
            }
            self.dynamic_bytes = self.dynamic_bytes.saturating_add(TILE_RGBA_BYTES);
        }
        self.dynamic_cache.insert(tile, entry);
        self.touch_dynamic_lru(tile);
    }

    /// Look up an exact-signature dynamic tile prepared asynchronously on the worker pool.
    pub fn get_valid_dynamic_tile(&mut self, tile: TileKey, dynamic_sig: u64) -> Option<CachedDynamicTile> {
        let cur_seq = self.dirty_log.current_seq();
        let valid = self.dynamic_cache.get(&tile).is_some_and(|c| c.dynamic_sig == dynamic_sig && self.dirty_log.is_tile_valid(tile, c.validated_seq));
        if !valid {
            return None;
        }
        if let Some(entry) = self.dynamic_cache.get_mut(&tile) {
            if entry.validated_seq < cur_seq {
                entry.validated_seq = cur_seq;
            }
            let out = entry.clone();
            self.touch_dynamic_lru(tile);
            Some(out)
        } else {
            None
        }
    }

    /// Look up the most recent valid-scope, valid-edit_seq dynamic tile for `tile` regardless of
    /// animation frame signature, so the UI can keep showing the last valid presentation while
    /// the next animation tick prepares asynchronously.
    pub fn get_any_valid_dynamic_tile(&mut self, tile: TileKey) -> Option<CachedDynamicTile> {
        let valid = self.dynamic_cache.get(&tile).is_some_and(|c| self.dirty_log.is_tile_valid(tile, c.validated_seq));
        if !valid {
            return None;
        }
        let out = self.dynamic_cache.get(&tile).cloned();
        if out.is_some() {
            self.touch_dynamic_lru(tile);
        }
        out
    }

    /// Record a freshly composed dynamic tile (e.g., from an active local brush stroke) so
    /// subsequent frames can reuse or fall back to it without re-composing.
    pub fn record_dynamic_tile(&mut self, tile: TileKey, dynamic_sig: u64, rgba8: Arc<Vec<u8>>, pending_assets: usize) {
        self.dynamic_req_gen = self.dynamic_req_gen.saturating_add(1);
        let entry = CachedDynamicTile { rgba8, validated_seq: self.dirty_log.current_seq(), dynamic_sig, request_gen: self.dynamic_req_gen, pending_assets };
        self.insert_dynamic_tile(tile, entry);
    }

    /// Schedule bounded asynchronous dynamic tile recomposition (animated media and/or peer
    /// previews) off the UI thread, coalescing superseded frames and cancelling offscreen jobs.
    pub fn schedule_dynamic_tiles(&mut self, active_tiles: BTreeSet<TileKey>, jobs: &[(TileKey, u64, u64)], previews: &[SharedStroke]) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.pool.set_active_dynamic_tiles(active_tiles);
            if jobs.is_empty() {
                return;
            }
            let Some(scene) = self.scene.as_ref().map(Arc::clone) else {
                return;
            };
            let sg = self.scope_gen.load(Ordering::Acquire);
            let vg = self.view_gen.load(Ordering::Acquire);
            let cur_seq = self.dirty_log.current_seq();
            let shared_previews: Arc<[SharedStroke]> = Arc::from(previews);
            for (rank, &(tile, dynamic_sig, elapsed_ms)) in jobs.iter().enumerate() {
                if self.get_valid_dynamic_tile(tile, dynamic_sig).is_some_and(|c| c.pending_assets == 0) {
                    continue;
                }
                self.dynamic_req_gen = self.dynamic_req_gen.saturating_add(1);
                let rgen = self.dynamic_req_gen;
                self.pool.enqueue(pool::TileJob {
                    priority: pool::JobPriority::AnimatedMedia(rank as u128),
                    tile,
                    scope_gen: sg,
                    view_gen: vg,
                    validated_seq: cur_seq,
                    scene: Arc::clone(&scene),
                    cancel: Arc::new(AtomicBool::new(false)),
                    kind: pool::JobKind::Dynamic { elapsed_ms, dynamic_sig, request_gen: rgen, previews: Arc::clone(&shared_previews) },
                });
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = active_tiles;
            let Some(scene) = self.scene.as_ref().map(Arc::clone) else {
                return;
            };
            let sg = self.scope_gen.load(Ordering::Acquire);
            let cur_seq = self.dirty_log.current_seq();
            let mut done = 0usize;
            for &(tile, dynamic_sig, elapsed_ms) in jobs {
                if done >= 2 {
                    break;
                }
                if self.get_valid_dynamic_tile(tile, dynamic_sig).is_some_and(|c| c.pending_assets == 0) {
                    continue;
                }
                let params = TileComposeParams {
                    index: &scene,
                    tile,
                    scope_gen: sg,
                    validated_seq: cur_seq,
                    elapsed_ms,
                    previews,
                    live_brush: None,
                    allow_parallel_brush: false,
                    cancel: None,
                };
                if let Ok(composed) = compose_world_tile_shared(&params, Some(&self.split_cache), &mut |frame| Ok(self.media_wasm.get(&frame.hash))) {
                    self.record_dynamic_tile(tile, dynamic_sig, Arc::new(composed.rgba8), composed.pending_assets.len());
                    done += 1;
                }
            }
        }
    }

    /// Drain completed background static AND dynamic tiles into their respective fenced CPU caches.
    /// Returns newly ready static `(TileKey, CachedCpuTile)`.
    pub fn poll_completed(&mut self) -> Vec<(TileKey, CachedCpuTile)> {
        let mut ready = Vec::new();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let cur_sg = self.scope_gen.load(Ordering::Acquire);
            for done in self.pool.drain_completed() {
                if done.scope_gen != cur_sg {
                    continue;
                }
                if !self.dirty_log.is_tile_valid(done.tile, done.validated_seq) {
                    continue;
                }
                if done.split_warm_only {
                    self.split_warm_gen = self.split_warm_gen.saturating_add(1);
                    continue;
                }
                if let Some((dynamic_sig, request_gen)) = done.dynamic_sig {
                    let cached = CachedDynamicTile {
                        rgba8: done.rgba8,
                        validated_seq: done.validated_seq,
                        dynamic_sig,
                        request_gen,
                        pending_assets: done.pending_assets,
                    };
                    self.insert_dynamic_tile(done.tile, cached);
                    continue;
                }
                if done.from_disk {
                    self.stats.disk_hits = self.stats.disk_hits.saturating_add(1);
                }
                let cached = CachedCpuTile {
                    rgba8: done.rgba8,
                    validated_seq: done.validated_seq,
                    pending_assets: done.pending_assets,
                    device_layers: done.device_layers,
                    rendered_objects: done.rendered_objects,
                };
                self.insert_cpu_tile(done.tile, cached.clone());
                ready.push((done.tile, cached));
            }
        }
        ready
    }

    /// Schedule missing visible tiles, cold `lod + 2` preview tiles (when no fallback exists),
    /// and a 1-tile pan prefetch ring around `view` without blocking the UI thread.
    pub fn schedule_view(&mut self, view: &Viewport, target_lod: u8, visible: &[TileKey], missing_no_fallback: &[TileKey]) {
        let Some(scene) = self.scene.as_ref().map(Arc::clone) else {
            return;
        };
        let vg = self.view_gen.fetch_add(1, Ordering::AcqRel).saturating_add(1);
        let sg = self.scope_gen.load(Ordering::Acquire);
        let cur_seq = self.dirty_log.current_seq();

        let mut active_set: BTreeSet<TileKey> = visible.iter().copied().collect();

        // Compute 1-tile prefetch ring around viewport at `target_lod`.
        let mut prefetch = Vec::new();
        if let Ok(rect) = viewport_world_rect(view) {
            let (tx0, tx1, ty0, ty1) = tile_bounds_for_rect(rect, target_lod);
            for ty in (ty0 - 1)..=(ty1 + 1) {
                for tx in (tx0 - 1)..=(tx1 + 1) {
                    if tx >= tx0 && tx <= tx1 && ty >= ty0 && ty <= ty1 {
                        continue;
                    }
                    let k = TileKey { lod: target_lod, tx, ty };
                    active_set.insert(k);
                    prefetch.push(k);
                }
            }
        }

        // Compute cold coarse `lod + 2` preview tiles ONLY for visible tiles lacking any fallback.
        let mut cold_coarse = BTreeSet::new();
        if !missing_no_fallback.is_empty() && target_lod.saturating_add(2) <= MAX_TILE_LOD {
            let coarse_lod = target_lod.saturating_add(2);
            for &vis in missing_no_fallback {
                for (ckey, _, _) in photocraft_scene::spatial::fallback_quads(vis, coarse_lod, 4) {
                    if self.get_valid_cpu_tile(ckey).is_none() {
                        cold_coarse.insert(ckey);
                    }
                }
            }
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            self.pool.set_active_tiles(active_set);
            let mut missing_visible_count = 0usize;
            for (rank, &ckey) in cold_coarse.iter().enumerate() {
                self.pool.enqueue(pool::TileJob {
                    priority: pool::JobPriority::ColdCoarse(rank as u128),
                    tile: ckey,
                    scope_gen: sg,
                    view_gen: vg,
                    validated_seq: cur_seq,
                    scene: Arc::clone(&scene),
                    cancel: Arc::new(AtomicBool::new(false)),
                    kind: pool::JobKind::Static,
                });
            }
            for (rank, &tkey) in visible.iter().enumerate() {
                if self.get_valid_cpu_tile(tkey).is_some() {
                    continue;
                }
                missing_visible_count += 1;
                self.pool.enqueue(pool::TileJob {
                    priority: pool::JobPriority::VisibleTarget(rank as u128),
                    tile: tkey,
                    scope_gen: sg,
                    view_gen: vg,
                    validated_seq: cur_seq,
                    scene: Arc::clone(&scene),
                    cancel: Arc::new(AtomicBool::new(false)),
                    kind: pool::JobKind::Static,
                });
            }
            if missing_visible_count == 0 {
                for (rank, &pkey) in prefetch.iter().enumerate() {
                    if self.get_valid_cpu_tile(pkey).is_some() {
                        continue;
                    }
                    self.pool.enqueue(pool::TileJob {
                        priority: pool::JobPriority::Prefetch(rank as u128),
                        tile: pkey,
                        scope_gen: sg,
                        view_gen: vg,
                        validated_seq: cur_seq,
                        scene: Arc::clone(&scene),
                        cancel: Arc::new(AtomicBool::new(false)),
                        kind: pool::JobKind::Static,
                    });
                }
            }
        }

        #[cfg(target_arch = "wasm32")]
        {
            let _ = (sg, vg, cold_coarse, prefetch, active_set);
            // WASM is single-threaded: rasterize up to 2 missing visible tiles per frame progressively.
            let mut done = 0usize;
            for &tkey in visible {
                if done >= 2 {
                    break;
                }
                if self.get_valid_cpu_tile(tkey).is_some() {
                    continue;
                }
                let params = TileComposeParams {
                    index: &scene,
                    tile: tkey,
                    scope_gen: sg,
                    validated_seq: cur_seq,
                    elapsed_ms: 0,
                    previews: &[],
                    live_brush: None,
                    allow_parallel_brush: false,
                    cancel: None,
                };
                if let Ok(composed) = compose_world_tile(&params, None, &mut |frame| Ok(self.media_wasm.get(&frame.hash))) {
                    self.insert_cpu_tile(
                        tkey,
                        CachedCpuTile {
                            rgba8: Arc::new(composed.rgba8),
                            validated_seq: cur_seq,
                            pending_assets: composed.pending_assets.len(),
                            device_layers: composed.device_layers,
                            rendered_objects: composed.rendered_objects,
                        },
                    );
                    done += 1;
                }
            }
        }
    }

    /// Synchronously compose one static tile (used by deterministic tests and exports),
    /// populating the CPU (and optional disk) cache.
    pub fn render_static_tile_blocking(
        &mut self,
        tile: TileKey,
        elapsed_ms: u64,
        resolve_media: &mut dyn FnMut(&MediaFrame) -> Result<Option<Arc<RgbaFrame>>>,
    ) -> Result<CachedCpuTile> {
        if elapsed_ms == 0
            && let Some(hit) = self.get_valid_cpu_tile(tile)
            && hit.pending_assets == 0
        {
            return Ok(hit);
        }
        let scene = self.scene.as_ref().map(Arc::clone).ok_or_else(|| err("No active world scene in tile manager"))?;
        let sg = self.scope_gen.load(Ordering::Acquire);
        let cur_seq = self.dirty_log.current_seq();

        #[cfg(not(target_arch = "wasm32"))]
        let query_hash = if elapsed_ms == 0 {
            let query = scene.query_tile(tile);
            let h = scene.tile_content_hash(tile, &query);
            if let Some(dir) = self.pool.disk_dir()
                && let Some(rgba8) = pool::read_disk_tile(dir, &h)
            {
                self.stats.disk_hits = self.stats.disk_hits.saturating_add(1);
                let cached = CachedCpuTile { rgba8: Arc::new(rgba8), validated_seq: cur_seq, pending_assets: 0, device_layers: 0, rendered_objects: 0 };
                self.insert_cpu_tile(tile, cached.clone());
                return Ok(cached);
            }
            Some(h)
        } else {
            None
        };

        let params = TileComposeParams {
            index: &scene,
            tile,
            scope_gen: sg,
            validated_seq: cur_seq,
            elapsed_ms,
            previews: &[],
            live_brush: None,
            allow_parallel_brush: true,
            cancel: None,
        };
        let composed = compose_world_tile(&params, None, resolve_media)?;
        #[cfg(not(target_arch = "wasm32"))]
        if composed.pending_assets.is_empty()
            && let (Some(h), Some(dir)) = (query_hash, self.pool.disk_dir())
        {
            pool::write_disk_tile(dir, &h, &composed.rgba8, self.pool.disk_max_bytes());
            self.stats.disk_writes = self.stats.disk_writes.saturating_add(1);
        }
        let cached = CachedCpuTile {
            rgba8: Arc::new(composed.rgba8),
            validated_seq: cur_seq,
            pending_assets: composed.pending_assets.len(),
            device_layers: composed.device_layers,
            rendered_objects: composed.rendered_objects,
        };
        if composed.pending_assets.is_empty() {
            self.insert_cpu_tile(tile, cached.clone());
        }
        Ok(cached)
    }

    /// Compose one dynamic tile (overlapping local/peer preview and/or animated media)
    /// using the bounded `DynamicSplitCache` (Codex Correction #3).
    pub fn render_dynamic_tile(
        &mut self,
        tile: TileKey,
        elapsed_ms: u64,
        previews: &[SharedStroke],
        live_brush: Option<(StrokeId, &RasterizedBrushStroke)>,
        resolve_media: &mut dyn FnMut(&MediaFrame) -> Result<Option<Arc<RgbaFrame>>>,
    ) -> Result<TileComposeResult> {
        let scene = self.scene.as_ref().map(Arc::clone).ok_or_else(|| err("No active world scene in tile manager"))?;
        let sg = self.scope_gen.load(Ordering::Acquire);
        let cur_seq = self.dirty_log.current_seq();
        let params = TileComposeParams {
            index: &scene,
            tile,
            scope_gen: sg,
            validated_seq: cur_seq,
            elapsed_ms,
            previews,
            live_brush,
            allow_parallel_brush: false,
            cancel: None,
        };
        if live_brush.is_some() {
            compose_world_tile_shared_prefer_cached_anim(&params, Some(&self.split_cache), resolve_media)
        } else {
            compose_world_tile_shared(&params, Some(&self.split_cache), resolve_media)
        }
    }

    /// Schedule bounded off-thread `DynamicSplitCache` warming (`LiveSplitWarm` for currently touched
    /// cold tiles and `LiveSplitPrefetch` for immediate neighbor tiles) so active live drawing never
    /// rasterizes thousands of committed strokes on the UI/input thread.
    pub fn schedule_split_warm_tiles(&mut self, active_tiles: BTreeSet<TileKey>, urgent: &[SplitWarmJob], prefetch: &[SplitWarmJob]) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.pool.set_active_warm_tiles(active_tiles);
            if urgent.is_empty() && prefetch.is_empty() {
                return;
            }
            let Some(scene) = self.scene.as_ref().map(Arc::clone) else {
                return;
            };
            let sg = self.scope_gen.load(Ordering::Acquire);
            let vg = self.view_gen.load(Ordering::Acquire);
            let cur_seq = self.dirty_log.current_seq();
            for (rank, (tile, warm_sig, elapsed_ms, split_devices)) in urgent.iter().enumerate() {
                if split_devices.is_empty() {
                    continue;
                }
                self.pool.enqueue(pool::TileJob {
                    priority: pool::JobPriority::LiveSplitWarm(rank as u128),
                    tile: *tile,
                    scope_gen: sg,
                    view_gen: vg,
                    validated_seq: cur_seq,
                    scene: Arc::clone(&scene),
                    cancel: Arc::new(AtomicBool::new(false)),
                    kind: pool::JobKind::WarmSplitBands { elapsed_ms: *elapsed_ms, warm_sig: *warm_sig, split_devices: Arc::clone(split_devices) },
                });
            }
            for (rank, (tile, warm_sig, elapsed_ms, split_devices)) in prefetch.iter().enumerate() {
                if split_devices.is_empty() {
                    continue;
                }
                self.pool.enqueue(pool::TileJob {
                    priority: pool::JobPriority::LiveSplitPrefetch(rank as u128),
                    tile: *tile,
                    scope_gen: sg,
                    view_gen: vg,
                    validated_seq: cur_seq,
                    scene: Arc::clone(&scene),
                    cancel: Arc::new(AtomicBool::new(false)),
                    kind: pool::JobKind::WarmSplitBands { elapsed_ms: *elapsed_ms, warm_sig: *warm_sig, split_devices: Arc::clone(split_devices) },
                });
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (active_tiles, prefetch);
            let Some(scene) = self.scene.as_ref().map(Arc::clone) else {
                return;
            };
            let sg = self.scope_gen.load(Ordering::Acquire);
            let cur_seq = self.dirty_log.current_seq();
            if let Some((tile, _, elapsed_ms, split_devices)) = urgent.first()
                && !split_devices.is_empty()
            {
                let params = TileComposeParams {
                    index: &scene,
                    tile: *tile,
                    scope_gen: sg,
                    validated_seq: cur_seq,
                    elapsed_ms: *elapsed_ms,
                    previews: &[],
                    live_brush: None,
                    allow_parallel_brush: false,
                    cancel: None,
                };
                if warm_dynamic_split_bands(&params, split_devices, &self.split_cache, &mut |frame| Ok(self.media_wasm.get(&frame.hash))).unwrap_or(false) {
                    self.split_warm_gen = self.split_warm_gen.saturating_add(1);
                }
            }
        }
    }

    /// Non-blocking UI-thread composition for active local live-brush tiles.
    /// Never rasterizes committed static bands on the UI thread: composites pure topmost `SrcOver`
    /// strokes directly onto cached RGBA8 tiles (`~0.1 ms`), uses bit-exact `[f32; 4]` W4 split
    /// bands whenever `DynamicSplitCache` is warm, and returns an immediate non-blocking preview
    /// (`used_cold_split_fallback = true`) when a tile requiring W4 split isolation is cold.
    #[allow(clippy::type_complexity)]
    pub fn render_live_tiles_nonblocking<'a>(
        &mut self,
        items: &[(TileKey, u64, bool, Option<(StrokeId, &'a RasterizedBrushStroke)>)],
        local_stroke_id: StrokeId,
        elapsed_ms: u64,
        previews: &[SharedStroke],
        resolve_media: &mut dyn FnMut(&MediaFrame) -> Result<Option<Arc<RgbaFrame>>>,
    ) -> Result<Vec<(TileKey, TileComposeResult)>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let scene = self.scene.as_ref().map(Arc::clone).ok_or_else(|| err("No active world scene in tile manager"))?;
        let sg = self.scope_gen.load(Ordering::Acquire);
        let cur_seq = self.dirty_log.current_seq();

        // Snapshot each tile's cached committed RGBA8 base before dispatching (touches LRU cleanly).
        let prepared: Vec<(TileKey, Option<Arc<Vec<u8>>>, bool, Option<(StrokeId, &'a RasterizedBrushStroke)>)> = items
            .iter()
            .map(|&(tile, bg_sig, has_anim, live_brush)| {
                let (base_arc, exact_anim) = if has_anim {
                    if bg_sig != 0
                        && let Some(hit) = self.get_valid_dynamic_tile(tile, bg_sig)
                    {
                        (Some(hit.rgba8), true)
                    } else if let Some(prev) = self.get_any_valid_dynamic_tile(tile) {
                        (Some(prev.rgba8), false)
                    } else if let Some(cpu) = self.get_valid_cpu_tile(tile) {
                        (Some(cpu.rgba8), false)
                    } else {
                        (None, false)
                    }
                } else if let Some(cpu) = self.get_valid_cpu_tile(tile) {
                    (Some(cpu.rgba8), true)
                } else {
                    (None, true)
                };
                (tile, base_arc, exact_anim, live_brush)
            })
            .collect();

        #[cfg(not(target_arch = "wasm32"))]
        if prepared.len() >= 2 {
            use rayon::prelude::*;
            for &(tile, _, _, _) in &prepared {
                for id in scene.query_world_rect_media(tile.physical_rect()) {
                    if let Some(shared) = scene.world().media.get(&id)
                        && let Ok((_, frame)) = shared.media.frame_at(elapsed_ms)
                        && !self.has_media_frame(&frame.hash)
                        && let Some(decoded) = resolve_media(frame)?
                    {
                        self.register_media_frame(frame.hash, decoded);
                    }
                }
            }
            let media_store = self.pool.media_store();
            let split_cache = &self.split_cache;
            let results: Vec<Result<(TileKey, TileComposeResult)>> = prepared
                .par_iter()
                .map(|(tile, base_arc, exact_anim, live_brush)| {
                    let params = TileComposeParams {
                        index: &scene,
                        tile: *tile,
                        scope_gen: sg,
                        validated_seq: cur_seq,
                        elapsed_ms,
                        previews,
                        live_brush: *live_brush,
                        allow_parallel_brush: false,
                        cancel: None,
                    };
                    let base = LiveColdBase { rgba8: base_arc.as_deref().map(Vec::as_slice), is_exact_anim: *exact_anim };
                    let mut resolver = |mf: &MediaFrame| Ok(media_store.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&mf.hash));
                    let res = compose_world_tile_nonblocking_live(&params, local_stroke_id, base, split_cache, &mut resolver)?;
                    Ok((*tile, res))
                })
                .collect();
            let mut out = Vec::with_capacity(results.len());
            for r in results {
                out.push(r?);
            }
            return Ok(out);
        }

        let mut out = Vec::with_capacity(prepared.len());
        for (tile, base_arc, exact_anim, live_brush) in prepared {
            let params = TileComposeParams {
                index: &scene,
                tile,
                scope_gen: sg,
                validated_seq: cur_seq,
                elapsed_ms,
                previews,
                live_brush,
                allow_parallel_brush: false,
                cancel: None,
            };
            let base = LiveColdBase { rgba8: base_arc.as_deref().map(Vec::as_slice), is_exact_anim: exact_anim };
            let res = compose_world_tile_nonblocking_live(&params, local_stroke_id, base, &self.split_cache, resolve_media)?;
            out.push((tile, res));
        }
        Ok(out)
    }

    /// Compose a batch of dirty dynamic tiles, parallelizing across CPU cores on native
    /// when `items.len() >= 2` while sharing `DynamicSplitCache` safely.
    pub fn render_dynamic_tiles_batch<'a>(
        &mut self,
        items: &[(TileKey, Option<(StrokeId, &'a RasterizedBrushStroke)>)],
        elapsed_ms: u64,
        previews: &[SharedStroke],
        resolve_media: &mut dyn FnMut(&MediaFrame) -> Result<Option<Arc<RgbaFrame>>>,
    ) -> Result<Vec<(TileKey, TileComposeResult)>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        if items.len() == 1 {
            let (tile, live_brush) = items[0];
            let res = self.render_dynamic_tile(tile, elapsed_ms, previews, live_brush, resolve_media)?;
            return Ok(vec![(tile, res)]);
        }
        let scene = self.scene.as_ref().map(Arc::clone).ok_or_else(|| err("No active world scene in tile manager"))?;
        let sg = self.scope_gen.load(Ordering::Acquire);
        let cur_seq = self.dirty_log.current_seq();

        #[cfg(not(target_arch = "wasm32"))]
        {
            use rayon::prelude::*;
            // Pre-resolve any visible media frames referenced by these tiles into `media_store`
            // so worker threads read decoded frames lock-free without calling `&mut resolve_media`.
            for &(tile, _) in items {
                for id in scene.query_world_rect_media(tile.physical_rect()) {
                    if let Some(shared) = scene.world().media.get(&id)
                        && let Ok((_, frame)) = shared.media.frame_at(elapsed_ms)
                        && !self.has_media_frame(&frame.hash)
                        && let Some(decoded) = resolve_media(frame)?
                    {
                        self.register_media_frame(frame.hash, decoded);
                    }
                }
            }
            let media_store = self.pool.media_store();
            let split_cache = &self.split_cache;
            let results: Vec<Result<(TileKey, TileComposeResult)>> = items
                .par_iter()
                .map(|&(tile, live_brush)| {
                    let params = TileComposeParams {
                        index: &scene,
                        tile,
                        scope_gen: sg,
                        validated_seq: cur_seq,
                        elapsed_ms,
                        previews,
                        live_brush,
                        allow_parallel_brush: false,
                        cancel: None,
                    };
                    let mut resolver = |mf: &MediaFrame| Ok(media_store.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&mf.hash));
                    let res = if live_brush.is_some() {
                        compose_world_tile_shared_prefer_cached_anim(&params, Some(split_cache), &mut resolver)?
                    } else {
                        compose_world_tile_shared(&params, Some(split_cache), &mut resolver)?
                    };
                    Ok((tile, res))
                })
                .collect();
            let mut out = Vec::with_capacity(results.len());
            for r in results {
                out.push(r?);
            }
            Ok(out)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let mut out = Vec::with_capacity(items.len());
            for &(tile, live_brush) in items {
                let res = self.render_dynamic_tile(tile, elapsed_ms, previews, live_brush, resolve_media)?;
                out.push((tile, res));
            }
            Ok(out)
        }
    }
}
