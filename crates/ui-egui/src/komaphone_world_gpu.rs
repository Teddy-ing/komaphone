//! Single-pass `260×260×N` `Texture2DArray` shared-canvas GPU tile presenter.
//!
//! Replaces per-stroke GPU render passes and screen-space cache wipes with
//! world-space LOD tiles (`256` interior + `2 px` directly rasterized apron),
//! a reusable staging ring, reconciled desktop (`256`-layer, `66.02 MiB`) and
//! Safe-GPU/WARP (`128`-layer, `33.01 MiB`) budgets, non-blocking progressive
//! refinement with temporary coarse/fine fallback, and a pure-CPU fallback when
//! `gpu_render_state` is unavailable.
#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;

use eframe::{egui_wgpu::RenderState, wgpu};
#[cfg(test)]
use photocraft_engine::{
    scene::shared::media::AssetHash,
    world_brush::RasterizedBrushStroke,
    world_media::WorldRenderer,
    world_tiles::{TileComposeParams, compose_world_tile},
};
use photocraft_engine::{
    scene::{
        StrokeId, Viewport,
        shared::{DeviceId, SharedStroke, media::MediaFrame},
        spatial::{MAX_TILE_LOD, TILE_PHYSICAL, TileKey, WorldRect, fallback_quads, select_target_lod, stroke_world_rect, visible_tiles},
    },
    world_brush::LiveTileBrush,
    world_tiles::{RgbaFrame, SplitWarmJob, TileBudget, WorldTileManager},
};

#[cfg(all(test, not(target_arch = "wasm32")))]
mod cp6_bench;
mod geometry;
#[cfg(test)]
pub(crate) mod tests;

pub use geometry::CpuTilePresenter;
use geometry::{ALIGNED_ROW_BYTES, STAGING_TILE_BYTES, SlotKind, SlotMeta, TileAtlas, TileQuadBatch, pad_tile_rows};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const MAX_PIXELS: u64 = 8_388_608;
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Stats {
    pub vertices: usize,
    pub indices: usize,
    pub layers: usize,
    pub texture_bytes: u64,
    pub uploaded_asset_bytes: u64,
    pub pending_assets: usize,
    pub tile_array_bytes: u64,
    pub visible_tiles: usize,
    pub resident_tiles: usize,
    pub fallback_tiles: usize,
    pub pending_tiles: usize,
    pub anim_pending_tiles: usize,
    pub uploads_this_frame: usize,
    pub target_lod: u8,
    pub is_refining: bool,
}

struct OutputSurface {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

struct Targets {
    size: [u32; 2],
    output: OutputSurface,
    id: egui::TextureId,
}

struct PendingUpload {
    layer: u32,
    rgba8: Arc<Vec<u8>>,
}

pub struct WorldGpu {
    rs: RenderState,
    health: photocraft_gpu::DeviceHealth,
    budget: TileBudget,
    tile_array: wgpu::Texture,
    tile_bind: wgpu::BindGroup,
    tile_pipeline: wgpu::RenderPipeline,
    staging_ring: wgpu::Buffer,
    staging_slots: usize,
    pad_scratch: Vec<u8>,
    atlas: TileAtlas,
    quads: TileQuadBatch,
    targets: Option<Targets>,
    last_scope_gen: u64,
    last_lod: Option<u8>,
    frame_counter: u64,
    pub stats: Stats,
}

impl Drop for WorldGpu {
    fn drop(&mut self) {
        if let Some(targets) = self.targets.take() {
            self.rs.renderer.write().free_texture(&targets.id);
        }
    }
}

impl WorldGpu {
    pub fn new(rs: &RenderState, health: photocraft_gpu::DeviceHealth) -> Result<Self> {
        let info = rs.adapter.get_info();
        let max_layers = rs.device.limits().max_texture_array_layers;
        let budget = if info.device_type == wgpu::DeviceType::Cpu || max_layers < 256 { TileBudget::conservative() } else { TileBudget::desktop() };
        Self::new_with_budget(rs, health, budget)
    }

    pub fn new_with_budget(rs: &RenderState, health: photocraft_gpu::DeviceHealth, mut budget: TileBudget) -> Result<Self> {
        if let Some(fault) = health.fault() {
            return Err(fault.to_string());
        }
        let max_layers = rs.device.limits().max_texture_array_layers;
        if budget.gpu_layers > max_layers {
            budget = TileBudget::conservative();
            budget.gpu_layers = budget.gpu_layers.min(max_layers).max(16);
        }
        let device = &rs.device;
        let tile_array = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("world tile array"),
            size: wgpu::Extent3d { width: TILE_PHYSICAL, height: TILE_PHYSICAL, depth_or_array_layers: budget.gpu_layers },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let array_view = tile_array.create_view(&wgpu::TextureViewDescriptor { dimension: Some(wgpu::TextureViewDimension::D2Array), ..Default::default() });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("world tile linear"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("world tile array layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let tile_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("world tile array bind"),
            layout: &bind_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&array_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("world tile array shader"),
            source: wgpu::ShaderSource::Wgsl(TILE_SHADER.into()),
        });
        let pipe_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("world tile array pipeline layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let attrs = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Uint32];
        let vbufs = [Some(wgpu::VertexBufferLayout { array_stride: 20, step_mode: wgpu::VertexStepMode::Vertex, attributes: &attrs })];
        let tile_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("world tile array pipeline"),
            layout: Some(&pipe_layout),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), buffers: &vbufs, compilation_options: Default::default() },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });

        let staging_slots = (budget.staging_ring_bytes as usize / STAGING_TILE_BYTES).max(4);
        let staging_bytes = (staging_slots * STAGING_TILE_BYTES) as u64;
        let staging_ring = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("world tile staging ring"),
            size: staging_bytes,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        if let Some(fault) = health.fault() {
            return Err(fault.to_string());
        }
        Ok(Self {
            rs: rs.clone(),
            health,
            budget,
            tile_array,
            tile_bind,
            tile_pipeline,
            staging_ring,
            staging_slots,
            pad_scratch: vec![0u8; STAGING_TILE_BYTES],
            atlas: TileAtlas::new(budget),
            quads: TileQuadBatch::default(),
            targets: None,
            last_scope_gen: 0,
            last_lod: None,
            frame_counter: 0,
            stats: Stats::default(),
        })
    }

    pub fn budget(&self) -> TileBudget {
        self.budget
    }

    /// Resolve the active target LOD using the presenter's hysteresis state so
    /// `LiveTileBrush` and `present_interactive` always target the exact same LOD.
    pub fn target_lod(&self, view: &Viewport, density: f32) -> Result<u8> {
        select_target_lod(view, density, self.budget.max_visible_tiles, self.last_lod).map_err(|e| e.to_string())
    }

    pub fn purge_slots(&mut self) {
        self.atlas.clear();
        self.last_lod = None;
    }

    /// Non-blocking interactive tile presentation: never rasterizes static committed
    /// tiles, animated-media tiles, or peer-preview tiles on the UI thread; uploads
    /// within per-frame caps, keeps the last valid presentation while async work
    /// completes, and executes 1 single GPU quad pass.
    #[allow(clippy::too_many_arguments)]
    pub fn present_interactive(
        &mut self,
        mgr: &mut WorldTileManager,
        view: &Viewport,
        density: f32,
        elapsed_ms: u64,
        previews: &[SharedStroke],
        mut live_tile_brush: Option<&mut LiveTileBrush>,
        resolve_media: &mut dyn FnMut(&MediaFrame) -> photocraft_engine::Result<Option<Arc<RgbaFrame>>>,
        retain_stale_for_review: bool,
    ) -> Result<egui::TextureId> {
        let size = self.validate_frame_inputs(view, density, previews)?;
        let scope_gen = mgr.scope_gen();
        if self.last_scope_gen != scope_gen {
            self.atlas.clear();
            self.last_scope_gen = scope_gen;
            self.last_lod = None;
        }
        self.frame_counter = self.frame_counter.saturating_add(1);
        let frame = self.frame_counter;

        // Drain completed worker static AND async dynamic tiles into CPU caches.
        let _completed = mgr.poll_completed();

        let target_lod = select_target_lod(view, density, self.budget.max_visible_tiles, self.last_lod).map_err(|e| e.to_string())?;
        self.last_lod = Some(target_lod);
        let visible = visible_tiles(view, target_lod, self.budget.max_visible_tiles).map_err(|e| e.to_string())?;
        if let Some(lb) = live_tile_brush.as_deref_mut() {
            lb.ensure_lod(target_lod);
        }
        let live_stroke_id = live_tile_brush.as_deref().map(LiveTileBrush::stroke_id);

        // Pre-register any already-available decoded frames for visible animated media BEFORE
        // computing dynamic signatures so `has_media_frame` is stable from the first frame.
        preregister_visible_anim_frames(mgr, view, elapsed_ms, resolve_media)?;

        // Partition visible tiles into:
        // 1. Local active-brush tiles (incremental LiveTileBrush path).
        // 2. Async dynamic tiles (animated media and/or peer previews — NEVER recomposed on UI thread).
        // 3. Static tiles (async static worker pool).
        let dynamic_items = collect_dynamic_items(mgr, view, previews, elapsed_ms)?;
        let mut local_brush_tiles: Vec<TileKey> = Vec::new();
        let mut async_dynamic_tiles: Vec<TileKey> = Vec::new();
        let mut static_visible: Vec<TileKey> = Vec::with_capacity(visible.len());
        let mut active_async_set: BTreeSet<TileKey> = BTreeSet::new();
        for &tile in &visible {
            let phys = tile.physical_rect();
            let has_local_brush = live_stroke_id.is_some_and(|lsid| dynamic_items.previews.iter().any(|(r, (sid, _))| *sid == lsid && r.intersects(&phys)));
            if has_local_brush {
                local_brush_tiles.push(tile);
                continue;
            }
            let has_peer_or_anim =
                dynamic_items.previews.iter().any(|(r, _)| r.intersects(&phys)) || dynamic_items.anim_media.iter().any(|(r, _)| r.intersects(&phys));
            if has_peer_or_anim {
                async_dynamic_tiles.push(tile);
                active_async_set.insert(tile);
            } else {
                static_visible.push(tile);
            }
        }
        if !local_brush_tiles.is_empty()
            && let Some(lb) = live_tile_brush.as_deref_mut()
        {
            lb.prepare_tiles(&local_brush_tiles).map_err(|e| e.to_string())?;
        }

        self.quads.clear();
        let mut uploads: Vec<PendingUpload> = Vec::new();
        let mut static_uploads_used = 0usize;
        let mut async_dyn_uploads_used = 0usize;
        let mut fallback_tiles = 0usize;
        let mut pending_tiles = 0usize;
        let mut anim_pending_tiles = 0usize;
        let mut pending_assets = 0usize;
        let mut missing_no_fallback = Vec::new();
        let mut async_jobs_to_schedule: Vec<(TileKey, u64, u64)> = Vec::new();
        let edit_seq = mgr.edit_seq();

        let _ = retain_stale_for_review;
        let local_counts = self.present_local_brush_tiles(
            mgr,
            view,
            scope_gen,
            edit_seq,
            frame,
            elapsed_ms,
            &visible,
            &local_brush_tiles,
            &dynamic_items,
            live_stroke_id,
            live_tile_brush.as_deref(),
            previews,
            resolve_media,
            &mut uploads,
            &mut active_async_set,
            &mut async_jobs_to_schedule,
        )?;
        pending_assets = pending_assets.saturating_add(local_counts.pending_assets);
        anim_pending_tiles = anim_pending_tiles.saturating_add(local_counts.anim_pending_tiles);
        pending_tiles = pending_tiles.saturating_add(local_counts.pending_tiles);

        // 2. Async dynamic tiles (animated media & peer previews): NEVER block the UI thread!
        // Present exact match if ready, otherwise keep the last valid presentation while
        // bounded background workers prepare the next frame off-thread.
        let dyn_counts = self.present_async_dynamic_tiles(
            mgr,
            view,
            scope_gen,
            edit_seq,
            frame,
            elapsed_ms,
            &async_dynamic_tiles,
            &dynamic_items,
            &mut uploads,
            &mut static_uploads_used,
            &mut async_dyn_uploads_used,
            &mut missing_no_fallback,
            &mut async_jobs_to_schedule,
        )?;
        pending_assets = pending_assets.saturating_add(dyn_counts.pending_assets);
        anim_pending_tiles = anim_pending_tiles.saturating_add(dyn_counts.anim_pending_tiles);
        pending_tiles = pending_tiles.saturating_add(dyn_counts.pending_tiles);
        fallback_tiles = fallback_tiles.saturating_add(dyn_counts.fallback_tiles);

        if !async_jobs_to_schedule.is_empty() || !active_async_set.is_empty() {
            if let Some(lsid) = live_stroke_id {
                let peer_only: Vec<SharedStroke> = previews.iter().filter(|p| p.stroke.id != lsid).cloned().collect();
                mgr.schedule_dynamic_tiles(active_async_set, &async_jobs_to_schedule, &peer_only);
            } else {
                mgr.schedule_dynamic_tiles(active_async_set, &async_jobs_to_schedule, previews);
            }
        } else {
            mgr.schedule_dynamic_tiles(active_async_set, &async_jobs_to_schedule, &[]);
        }

        // 3. Static tiles (non-dynamic visible tiles).
        for &tile in &static_visible {
            // 3a. Check resident GPU static slot.
            if let Some((layer, meta)) = self.atlas.get_slot(tile, false, frame)
                && meta.scope_gen == scope_gen
                && mgr.is_tile_valid_at(tile, meta.validated_seq)
            {
                self.atlas.update_validated_seq(layer, mgr.edit_seq());
                self.quads.push_tile(tile, layer, view)?;
                continue;
            }

            // 3b. Check ready CPU cache (respecting per-frame upload cap).
            if static_uploads_used < self.budget.max_uploads_per_frame
                && let Some(cpu_tile) = mgr.get_valid_cpu_tile(tile)
            {
                pending_assets = pending_assets.saturating_add(cpu_tile.pending_assets);
                if let Some(layer) = self.atlas.allocate(tile, false, SlotKind::Static, cpu_tile.validated_seq, scope_gen, frame) {
                    uploads.push(PendingUpload { layer, rgba8: cpu_tile.rgba8 });
                    static_uploads_used += 1;
                    self.quads.push_tile(tile, layer, view)?;
                    continue;
                }
            }

            // 3c. Target tile is not yet uploaded: hold fresher same-tile resident GPU pixels
            // (never dropping out to white during live edits or review!) or draw layered fallback.
            pending_tiles += 1;
            let stat_peek = self.atlas.peek_slot(tile, false).filter(|(_, m)| m.scope_gen == scope_gen);
            let dyn_peek = self.atlas.peek_slot(tile, true).filter(|(_, m)| m.scope_gen == scope_gen);
            let (drew_fallback, fully_covered) = if let Some((_, dmeta)) = dyn_peek
                && !dmeta.provisional
                && stat_peek.is_none_or(|(_, smeta)| dmeta.validated_seq >= smeta.validated_seq)
                && let Some((layer, _)) = self.atlas.get_slot(tile, true, frame)
            {
                // Seamless bridge right after mouse-up while committed static tile finishes.
                self.quads.push_tile(tile, layer, view)?;
                (true, true)
            } else if stat_peek.is_some()
                && let Some((layer, _)) = self.atlas.get_slot(tile, false, frame)
            {
                // Hold the tile's own previous resident VRAM pixels instead of flashing white!
                self.quads.push_tile(tile, layer, view)?;
                (true, true)
            } else {
                let fb = self.try_draw_fallback(mgr, tile, view, scope_gen, frame, &mut uploads, &mut static_uploads_used)?;
                let mut drew = fb.drew_any;
                if !drew
                    && dyn_peek.is_some()
                    && let Some((layer, _)) = self.atlas.get_slot(tile, true, frame)
                {
                    self.quads.push_tile(tile, layer, view)?;
                    drew = true;
                }
                (drew, fb.fully_covered)
            };
            if drew_fallback {
                fallback_tiles += 1;
            }
            if !fully_covered {
                missing_no_fallback.push(tile);
            }
        }

        // Keep static workers alive for non-animated `local_brush_tiles` missing a valid committed base
        // so rapid consecutive strokes never cancel the previous stroke's static tile render!
        let mut static_schedule = static_visible;
        for &tile in &local_brush_tiles {
            let phys = tile.physical_rect();
            let has_anim = dynamic_items.anim_media.iter().any(|(r, _)| r.intersects(&phys));
            if !has_anim && !mgr.has_valid_cpu_tile(tile) {
                static_schedule.push(tile);
            }
        }
        // Schedule missing static tiles, cold `lod + 2` preview tiles, and (when idle) zoom-out parents + pan ring.
        mgr.schedule_view_interactive(view, target_lod, &static_schedule, &missing_no_fallback, local_brush_tiles.is_empty());

        let uploads_count = uploads.len();
        let id = self.submit_uploads_and_quads(size, &uploads)?;

        let tile_array_bytes = u64::from(TILE_PHYSICAL) * u64::from(TILE_PHYSICAL) * 4 * u64::from(self.budget.gpu_layers);
        self.stats = Stats {
            vertices: self.quads.vertex_count(),
            indices: self.quads.index_count() as usize,
            layers: 0,
            texture_bytes: u64::from(size[0]) * u64::from(size[1]) * 4,
            uploaded_asset_bytes: 0,
            pending_assets,
            tile_array_bytes,
            visible_tiles: visible.len(),
            resident_tiles: self.atlas.resident_count(),
            fallback_tiles,
            pending_tiles,
            anim_pending_tiles,
            uploads_this_frame: uploads_count,
            target_lod,
            is_refining: pending_tiles > 0,
        };
        if let Some(fault) = self.health.fault() {
            return Err(fault.to_string());
        }
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    fn present_local_brush_tiles(
        &mut self,
        mgr: &mut WorldTileManager,
        view: &Viewport,
        scope_gen: u64,
        edit_seq: u64,
        frame: u64,
        elapsed_ms: u64,
        visible: &[TileKey],
        local_brush_tiles: &[TileKey],
        dynamic_items: &DynamicItems,
        live_stroke_id: Option<StrokeId>,
        live_tile_brush: Option<&LiveTileBrush>,
        previews: &[SharedStroke],
        resolve_media: &mut dyn FnMut(&MediaFrame) -> photocraft_engine::Result<Option<Arc<RgbaFrame>>>,
        uploads: &mut Vec<PendingUpload>,
        active_async_set: &mut BTreeSet<TileKey>,
        async_jobs_to_schedule: &mut Vec<(TileKey, u64, u64)>,
    ) -> Result<AsyncDynCounts> {
        let mut counts = AsyncDynCounts::default();
        if local_brush_tiles.is_empty() {
            mgr.schedule_split_warm_tiles(BTreeSet::new(), &[], &[]);
            return Ok(counts);
        }
        let Some(lsid) = live_stroke_id else {
            mgr.schedule_split_warm_tiles(BTreeSet::new(), &[], &[]);
            return Ok(counts);
        };
        let mut dirty_local: Vec<(TileKey, u64, u64, bool)> = Vec::new();
        for &tile in local_brush_tiles {
            let phys = tile.physical_rect();
            let live_rev = live_tile_brush.and_then(|lb| lb.get_prepared_tile(tile)).map_or(0, |(rev, _)| rev);
            let dyn_sig = compute_dynamic_tile_sig(scope_gen, edit_seq, Some(lsid), live_rev, phys, dynamic_items);
            if dyn_sig != 0
                && let Some((layer, meta)) = self.atlas.get_slot(tile, true, frame)
                && meta.scope_gen == scope_gen
                && meta.dynamic_sig == dyn_sig
                && mgr.is_tile_valid_at(tile, meta.validated_seq)
            {
                self.quads.push_tile(tile, layer, view)?;
            } else {
                let has_anim = dynamic_items.anim_media.iter().any(|(r, _)| r.intersects(&phys));
                let bg_sig = if has_anim { compute_bg_anim_tile_sig(scope_gen, edit_seq, Some(lsid), phys, dynamic_items) } else { 0 };
                dirty_local.push((tile, dyn_sig, bg_sig, has_anim));
            }
        }
        let preview_rects = collect_preview_device_rects(mgr, previews)?;
        let local_dev = previews.iter().find(|p| p.visible && p.stroke.id == lsid).map(|p| p.device);
        let mut active_warm_set = BTreeSet::new();
        let mut urgent_warm: Vec<SplitWarmJob> = Vec::new();
        let mut prefetch_warm: Vec<SplitWarmJob> = Vec::new();
        let mut any_needs_split = false;
        if !dirty_local.is_empty() {
            let batch_inputs: Vec<_> = dirty_local
                .iter()
                .map(|&(tile, _, bg_sig, has_anim)| {
                    let arg = live_tile_brush.and_then(|lb| lb.get_prepared_tile(tile)).map(|(_, r)| (lsid, r));
                    (tile, bg_sig, has_anim, arg)
                })
                .collect();
            let composed_batch = mgr.render_live_tiles_nonblocking(&batch_inputs, lsid, elapsed_ms, previews, resolve_media).map_err(|e| e.to_string())?;
            for ((tile, dyn_sig, bg_sig, _), (_, composed)) in dirty_local.into_iter().zip(composed_batch) {
                counts.pending_assets = counts.pending_assets.saturating_add(composed.pending_assets.len());
                let rgba8 = Arc::new(composed.rgba8);
                let is_provisional = composed.used_stale_base || composed.used_cold_split_fallback;
                let cached_sig = if composed.pending_assets.is_empty() && !composed.used_stale_anim_cache && !is_provisional { dyn_sig } else { 0 };
                if composed.used_stale_anim_cache {
                    active_async_set.insert(tile);
                    async_jobs_to_schedule.push((tile, bg_sig, elapsed_ms));
                    counts.anim_pending_tiles += 1;
                }
                if composed.used_stale_base {
                    counts.pending_tiles += 1;
                }
                if composed.used_cold_split_fallback {
                    any_needs_split = true;
                    let devs = tile_split_devices_for(tile.physical_rect(), &preview_rects, None);
                    if !devs.is_empty() {
                        active_warm_set.insert(tile);
                        let wsig = compute_warm_sig(scope_gen, edit_seq, tile, bg_sig, &devs);
                        urgent_warm.push((tile, wsig, elapsed_ms, devs));
                        counts.anim_pending_tiles += 1;
                    }
                }
                if !is_provisional && composed.pending_assets.is_empty() && !composed.used_stale_anim_cache {
                    mgr.record_live_handoff(tile, lsid, edit_seq, Arc::clone(&rgba8));
                }
                if let Some(layer) = self.atlas.allocate(tile, true, SlotKind::Dynamic, edit_seq, scope_gen, frame) {
                    self.atlas.set_dynamic_sig(layer, cached_sig);
                    self.atlas.set_provisional(layer, is_provisional);
                    uploads.push(PendingUpload { layer, rgba8 });
                    self.quads.push_tile(tile, layer, view)?;
                }
            }
        }
        // Proactively warm up to 4 immediate cardinal neighbors ONLY when no urgent cold tiles are
        // pending, so prefetch never evicts active live-brush tiles from `DynamicSplitCache`.
        let local_needs_split_prefetch = any_needs_split
            || previews.iter().any(|p| {
                p.visible
                    && (p.stroke.id != lsid
                        || p.stroke.erase
                        || p.stroke.brush.as_deref().is_some_and(|b| {
                            b.behind
                                || !matches!(
                                    b.mode,
                                    photocraft_engine::scene::brush::BrushBlendMode::Normal | photocraft_engine::scene::brush::BrushBlendMode::Dissolve
                                )
                        }))
            });
        if urgent_warm.is_empty() && local_needs_split_prefetch && local_dev.is_some() {
            let visible_set: BTreeSet<TileKey> = visible.iter().copied().collect();
            let active_local_set: BTreeSet<TileKey> = local_brush_tiles.iter().copied().collect();
            for &tile in local_brush_tiles.iter().rev() {
                if prefetch_warm.len() >= 4 {
                    break;
                }
                for &(dx, dy) in &[(-1_i128, 0_i128), (1, 0), (0, -1), (0, 1)] {
                    if prefetch_warm.len() >= 4 {
                        break;
                    }
                    let nb = TileKey { lod: tile.lod, tx: tile.tx.saturating_add(dx), ty: tile.ty.saturating_add(dy) };
                    if visible_set.contains(&nb) && !active_local_set.contains(&nb) && active_warm_set.insert(nb) {
                        let nb_phys = nb.physical_rect();
                        let devs = tile_split_devices_for(nb_phys, &preview_rects, local_dev);
                        let nb_bg = if dynamic_items.anim_media.iter().any(|(r, _)| r.intersects(&nb_phys)) {
                            compute_bg_anim_tile_sig(scope_gen, edit_seq, Some(lsid), nb_phys, dynamic_items)
                        } else {
                            0
                        };
                        let wsig = compute_warm_sig(scope_gen, edit_seq, nb, nb_bg, &devs);
                        prefetch_warm.push((nb, wsig, elapsed_ms, devs));
                    }
                }
            }
        }
        mgr.schedule_split_warm_tiles(active_warm_set, &urgent_warm, &prefetch_warm);
        Ok(counts)
    }

    #[allow(clippy::too_many_arguments)]
    fn present_async_dynamic_tiles(
        &mut self,
        mgr: &mut WorldTileManager,
        view: &Viewport,
        scope_gen: u64,
        edit_seq: u64,
        frame: u64,
        elapsed_ms: u64,
        async_dynamic_tiles: &[TileKey],
        dynamic_items: &DynamicItems,
        uploads: &mut Vec<PendingUpload>,
        static_uploads_used: &mut usize,
        async_dyn_uploads_used: &mut usize,
        missing_no_fallback: &mut Vec<TileKey>,
        async_jobs_to_schedule: &mut Vec<(TileKey, u64, u64)>,
    ) -> Result<AsyncDynCounts> {
        let mut counts = AsyncDynCounts::default();
        for &tile in async_dynamic_tiles {
            let phys = tile.physical_rect();
            let dyn_sig = compute_dynamic_tile_sig(scope_gen, edit_seq, None, 0, phys, dynamic_items);

            // 2a. Resident GPU dynamic slot with exact up-to-date signature.
            if dyn_sig != 0
                && let Some((layer, meta)) = self.atlas.get_slot(tile, true, frame)
                && meta.scope_gen == scope_gen
                && meta.dynamic_sig == dyn_sig
                && mgr.is_tile_valid_at(tile, meta.validated_seq)
            {
                self.atlas.update_validated_seq(layer, edit_seq);
                self.quads.push_tile(tile, layer, view)?;
                continue;
            }

            // 2b. Completed async dynamic tile ready in CPU cache (within per-frame upload cap).
            if *async_dyn_uploads_used < self.budget.max_uploads_per_frame
                && let Some(dyn_cpu) = mgr.get_valid_dynamic_tile(tile, dyn_sig)
            {
                counts.pending_assets = counts.pending_assets.saturating_add(dyn_cpu.pending_assets);
                if let Some(layer) = self.atlas.allocate(tile, true, SlotKind::Dynamic, dyn_cpu.validated_seq, scope_gen, frame) {
                    let cached_sig = if dyn_cpu.pending_assets == 0 { dyn_sig } else { 0 };
                    self.atlas.set_dynamic_sig(layer, cached_sig);
                    uploads.push(PendingUpload { layer, rgba8: dyn_cpu.rgba8 });
                    *async_dyn_uploads_used += 1;
                    self.quads.push_tile(tile, layer, view)?;
                    continue;
                }
            }

            // 2c. Not ready yet: schedule async recomposition and keep the last valid presentation!
            async_jobs_to_schedule.push((tile, dyn_sig, elapsed_ms));
            counts.anim_pending_tiles += 1;

            // Prefer the last valid GPU dynamic slot for this scope & edit_seq (seamless animation tick).
            if let Some((layer, meta)) = self.atlas.get_slot(tile, true, frame)
                && meta.scope_gen == scope_gen
                && !meta.provisional
                && mgr.is_tile_valid_at(tile, meta.validated_seq)
            {
                self.quads.push_tile(tile, layer, view)?;
                continue;
            }
            // Next prefer any valid-edit_seq previous dynamic frame still in CPU cache.
            if *async_dyn_uploads_used < self.budget.max_uploads_per_frame
                && let Some(prev_dyn) = mgr.get_any_valid_dynamic_tile(tile)
                && let Some(layer) = self.atlas.allocate(tile, true, SlotKind::Dynamic, prev_dyn.validated_seq, scope_gen, frame)
            {
                self.atlas.set_dynamic_sig(layer, prev_dyn.dynamic_sig);
                uploads.push(PendingUpload { layer, rgba8: prev_dyn.rgba8 });
                *async_dyn_uploads_used += 1;
                self.quads.push_tile(tile, layer, view)?;
                continue;
            }
            // Cold/post-edit tile with no valid dynamic presentation yet: hold previous GPU slot
            // (during live edits or review swaps) or layered coarse/fine fallback while first frame builds.
            counts.pending_tiles += 1;
            let stat_peek = self.atlas.peek_slot(tile, false).filter(|(_, m)| m.scope_gen == scope_gen);
            let dyn_peek = self.atlas.peek_slot(tile, true).filter(|(_, m)| m.scope_gen == scope_gen);
            let (drew_fallback, fully_covered) = if let Some((_, dmeta)) = dyn_peek
                && !dmeta.provisional
                && stat_peek.is_none_or(|(_, smeta)| dmeta.validated_seq >= smeta.validated_seq)
                && let Some((layer, _)) = self.atlas.get_slot(tile, true, frame)
            {
                self.quads.push_tile(tile, layer, view)?;
                (true, true)
            } else if stat_peek.is_some()
                && let Some((layer, _)) = self.atlas.get_slot(tile, false, frame)
            {
                self.quads.push_tile(tile, layer, view)?;
                (true, true)
            } else {
                let fb = self.try_draw_fallback(mgr, tile, view, scope_gen, frame, uploads, static_uploads_used)?;
                (fb.drew_any, fb.fully_covered)
            };
            if drew_fallback {
                counts.fallback_tiles += 1;
            }
            if !fully_covered {
                missing_no_fallback.push(tile);
            }
        }
        Ok(counts)
    }

    #[allow(clippy::too_many_arguments)]
    fn try_draw_fallback(
        &mut self,
        mgr: &mut WorldTileManager,
        target: TileKey,
        view: &Viewport,
        scope_gen: u64,
        frame: u64,
        uploads: &mut Vec<PendingUpload>,
        static_uploads_used: &mut usize,
    ) -> Result<FallbackResult> {
        let cur_seq = mgr.edit_seq();
        // Layer A: pick the best coarse ancestor level (`lod + 1 ..= lod + 8`) without mutating
        // `atlas` during candidate search, strictly ranking valid ancestors above stale ones so
        // an ancient rev-0 white tile can never mask newer artwork.
        struct BackdropCandidate {
            valid_full: bool,
            full: bool,
            all_valid: bool,
            min_seq: u64,
            lod: u8,
            quads: Vec<(TileKey, WorldRect, [f32; 4])>,
        }

        let max_up = target.lod.saturating_add(8).min(MAX_TILE_LOD);
        let remaining_uploads = self.budget.max_uploads_per_frame.saturating_sub(*static_uploads_used);
        let mut best_backdrop: Option<BackdropCandidate> = None;
        for f_lod in (target.lod + 1)..=max_up {
            let quads = fallback_quads(target, f_lod, 4);
            if quads.is_empty() {
                continue;
            }
            let mut resolved_count = 0usize;
            let mut all_valid = true;
            let mut min_seq = u64::MAX;
            let mut uploads_needed = 0usize;
            for &(fkey, _, _) in &quads {
                let stat = self.atlas.peek_slot(fkey, false).filter(|(_, m)| m.scope_gen == scope_gen);
                let dynm = self.atlas.peek_slot(fkey, true).filter(|(_, m)| m.scope_gen == scope_gen && !m.provisional);
                let stat_valid = stat.is_some_and(|(_, m)| mgr.is_tile_valid_at(fkey, m.validated_seq));
                let dyn_valid = dynm.is_some_and(|(_, m)| mgr.is_tile_valid_at(fkey, m.validated_seq));
                if stat_valid || dyn_valid {
                    resolved_count += 1;
                    min_seq = min_seq.min(cur_seq);
                } else if uploads_needed < remaining_uploads && mgr.has_valid_cpu_tile(fkey) {
                    resolved_count += 1;
                    uploads_needed += 1;
                    min_seq = min_seq.min(cur_seq);
                } else if let Some((_, m)) = best_resident_meta(stat, dynm) {
                    resolved_count += 1;
                    all_valid = false;
                    min_seq = min_seq.min(m.validated_seq);
                } else {
                    all_valid = false;
                }
            }
            if resolved_count == 0 {
                continue;
            }
            let full = resolved_count == quads.len();
            let valid_full = full && all_valid;
            let rank = (valid_full, full, all_valid, min_seq, std::cmp::Reverse(f_lod));
            let better = best_backdrop.as_ref().is_none_or(|b| rank > (b.valid_full, b.full, b.all_valid, b.min_seq, std::cmp::Reverse(b.lod)));
            if better {
                best_backdrop = Some(BackdropCandidate { valid_full, full, all_valid, min_seq, lod: f_lod, quads });
                if valid_full {
                    break;
                }
            }
        }

        let mut backdrop_quads: Vec<(WorldRect, [f32; 4], u32)> = Vec::new();
        let mut backdrop_all_valid = false;
        let mut backdrop_min_seq = 0u64;
        if let Some(BackdropCandidate { quads, .. }) = best_backdrop {
            let expected = quads.len();
            let mut all_valid = true;
            let mut min_seq = u64::MAX;
            for (fkey, rect, uv) in quads {
                let stat = self.atlas.peek_slot(fkey, false).filter(|(_, m)| m.scope_gen == scope_gen);
                let dynm = self.atlas.peek_slot(fkey, true).filter(|(_, m)| m.scope_gen == scope_gen && !m.provisional);
                if let Some((_, m)) = stat
                    && mgr.is_tile_valid_at(fkey, m.validated_seq)
                    && let Some((layer, _)) = self.atlas.get_slot(fkey, false, frame)
                {
                    backdrop_quads.push((rect, uv, layer));
                    min_seq = min_seq.min(cur_seq);
                } else if let Some((_, m)) = dynm
                    && mgr.is_tile_valid_at(fkey, m.validated_seq)
                    && let Some((layer, _)) = self.atlas.get_slot(fkey, true, frame)
                {
                    backdrop_quads.push((rect, uv, layer));
                    min_seq = min_seq.min(cur_seq);
                } else if *static_uploads_used < self.budget.max_uploads_per_frame
                    && let Some(cpu) = mgr.get_valid_cpu_tile(fkey)
                    && let Some(layer) = self.atlas.allocate(fkey, false, SlotKind::CoarseFallback, cpu.validated_seq, scope_gen, frame)
                {
                    uploads.push(PendingUpload { layer, rgba8: cpu.rgba8 });
                    *static_uploads_used += 1;
                    backdrop_quads.push((rect, uv, layer));
                    min_seq = min_seq.min(cpu.validated_seq);
                } else if let Some((_, meta)) = best_resident_meta(stat, dynm)
                    && let Some((layer, _)) = self.atlas.get_slot(fkey, meta.dynamic, frame)
                {
                    backdrop_quads.push((rect, uv, layer));
                    all_valid = false;
                    min_seq = min_seq.min(meta.validated_seq);
                } else {
                    all_valid = false;
                }
            }
            let backdrop_full = backdrop_quads.len() == expected && expected > 0;
            backdrop_all_valid = backdrop_full && all_valid;
            backdrop_min_seq = if backdrop_quads.is_empty() { 0 } else { min_seq };
        }

        // Layer B: paint sharper/fresher finer children (`lod - 1`, or `lod - 2` on fast wheel zoom-out)
        // ON TOP of the coarse backdrop using `max_quads = 16` so `1.5625×` cross-depth boundaries
        // (`3×3 = 9` children) never silently return empty!
        let mut child_quads: Vec<(WorldRect, [f32; 4], u32)> = Vec::new();
        let mut children_full = false;
        let mut children_all_valid = false;
        for step in [1u8, 2u8] {
            let Some(child_lod) = target.lod.checked_sub(step) else {
                continue;
            };
            let quads = fallback_quads(target, child_lod, 16);
            if quads.is_empty() {
                continue;
            }
            let expected = quads.len();
            let mut all_valid = true;
            for (fkey, rect, uv) in quads {
                let stat = self.atlas.peek_slot(fkey, false).filter(|(_, m)| m.scope_gen == scope_gen);
                let dynm = self.atlas.peek_slot(fkey, true).filter(|(_, m)| m.scope_gen == scope_gen && !m.provisional);
                let stat_valid = stat.is_some_and(|(_, m)| mgr.is_tile_valid_at(fkey, m.validated_seq));
                let dyn_valid = dynm.is_some_and(|(_, m)| mgr.is_tile_valid_at(fkey, m.validated_seq));
                if stat_valid && let Some((layer, _)) = self.atlas.get_slot(fkey, false, frame) {
                    child_quads.push((rect, uv, layer));
                } else if dyn_valid && let Some((layer, _)) = self.atlas.get_slot(fkey, true, frame) {
                    child_quads.push((rect, uv, layer));
                } else if *static_uploads_used < self.budget.max_uploads_per_frame
                    && let Some(cpu) = mgr.get_valid_cpu_tile(fkey)
                    && let Some(layer) = self.atlas.allocate(fkey, false, SlotKind::Static, cpu.validated_seq, scope_gen, frame)
                {
                    uploads.push(PendingUpload { layer, rgba8: cpu.rgba8 });
                    *static_uploads_used += 1;
                    child_quads.push((rect, uv, layer));
                } else if let Some((_, meta)) = best_resident_meta(stat, dynm)
                    && (backdrop_quads.is_empty() || (!backdrop_all_valid && meta.validated_seq > backdrop_min_seq))
                    && let Some((layer, _)) = self.atlas.get_slot(fkey, meta.dynamic, frame)
                {
                    child_quads.push((rect, uv, layer));
                    all_valid = false;
                } else {
                    all_valid = false;
                }
            }
            if !child_quads.is_empty() {
                children_full = child_quads.len() == expected;
                children_all_valid = children_full && all_valid;
                break;
            }
        }

        // Emit Layer A backdrop (unless children already cover 100% of `target`), then Layer B on top.
        if !children_full {
            for (rect, uv, layer) in &backdrop_quads {
                self.quads.push_rect(*rect, *uv, *layer, view)?;
            }
        }
        for (rect, uv, layer) in &child_quads {
            self.quads.push_rect(*rect, *uv, *layer, view)?;
        }
        Ok(FallbackResult { drew_any: !backdrop_quads.is_empty() || !child_quads.is_empty(), fully_covered: backdrop_all_valid || children_all_valid })
    }

    #[cfg(test)]
    pub fn render(
        &mut self,
        renderer: &mut WorldRenderer,
        view: &Viewport,
        density: f32,
        elapsed_ms: u64,
        resolve: &mut dyn FnMut(&MediaFrame) -> photocraft_engine::Result<Vec<u8>>,
    ) -> Result<egui::TextureId> {
        self.render_with_previews(renderer, view, density, elapsed_ms, resolve, &[])
    }

    #[cfg(test)]
    pub fn render_with_previews(
        &mut self,
        renderer: &mut WorldRenderer,
        view: &Viewport,
        density: f32,
        elapsed_ms: u64,
        resolve: &mut dyn FnMut(&MediaFrame) -> photocraft_engine::Result<Vec<u8>>,
        previews: &[SharedStroke],
    ) -> Result<egui::TextureId> {
        self.render_available(renderer, view, density, elapsed_ms, &mut |frame| resolve(frame).map(Some), previews)
    }

    #[cfg(test)]
    pub fn render_available(
        &mut self,
        renderer: &mut WorldRenderer,
        view: &Viewport,
        density: f32,
        elapsed_ms: u64,
        resolve: &mut dyn FnMut(&MediaFrame) -> photocraft_engine::Result<Option<Vec<u8>>>,
        previews: &[SharedStroke],
    ) -> Result<egui::TextureId> {
        self.render_with_live(renderer, view, density, elapsed_ms, resolve, previews, None)
    }

    /// Deterministic blocking render path (used by GPU readback tests, parity benchmarks,
    /// and headless verification). Directly rasterizes all visible target-LOD tiles and
    /// presents them in a single `Texture2DArray` quad pass.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_live(
        &mut self,
        renderer: &mut WorldRenderer,
        view: &Viewport,
        density: f32,
        elapsed_ms: u64,
        resolve: &mut dyn FnMut(&MediaFrame) -> photocraft_engine::Result<Option<Vec<u8>>>,
        previews: &[SharedStroke],
        live: Option<(StrokeId, &RasterizedBrushStroke)>,
    ) -> Result<egui::TextureId> {
        let size = self.validate_frame_inputs(view, density, previews)?;
        if view.camera.canvas != renderer.world().canvas {
            return Err("Viewport belongs to another canvas".into());
        }
        let target_lod = select_target_lod(view, density, self.budget.max_visible_tiles, None).map_err(|e| e.to_string())?;
        let visible = visible_tiles(view, target_lod, self.budget.max_visible_tiles).map_err(|e| e.to_string())?;

        self.frame_counter = self.frame_counter.saturating_add(1);
        let frame = self.frame_counter;
        self.quads.clear();

        let mut decoded_cache: BTreeMap<AssetHash, Arc<RgbaFrame>> = BTreeMap::new();
        let mut pending_assets: BTreeSet<AssetHash> = BTreeSet::new();
        let mut devices_seen: BTreeSet<DeviceId> = BTreeSet::new();
        let mut uploads: Vec<PendingUpload> = Vec::new();
        let index = renderer.index();
        let live_tile_arg = live.filter(|(_, r)| r.phys_w == TILE_PHYSICAL && r.phys_h == TILE_PHYSICAL);

        for &tile in &visible {
            let query = index.query_tile(tile);
            for &obj in &query.direct {
                if let Some(e) = index.entry(obj) {
                    devices_seen.insert(e.device);
                }
            }
            for (_, dev, _) in &query.summaries {
                devices_seen.insert(*dev);
            }
            for p in previews {
                if p.visible && !index.world().strokes.contains_key(&p.stroke.id) {
                    devices_seen.insert(p.device);
                }
            }
            let params = TileComposeParams {
                index,
                tile,
                scope_gen: frame,
                validated_seq: frame,
                elapsed_ms,
                previews,
                live_brush: live_tile_arg,
                allow_parallel_brush: true,
                cancel: None,
            };
            let composed = compose_world_tile(&params, None, &mut |mf| {
                if let Some(hit) = decoded_cache.get(&mf.hash) {
                    return Ok(Some(Arc::clone(hit)));
                }
                let Some(encoded) = resolve(mf)? else {
                    return Ok(None);
                };
                let decoded = photocraft_engine::world_media::decode_frame(mf, &encoded)?;
                let arc = Arc::new(RgbaFrame::from(decoded));
                decoded_cache.insert(mf.hash, Arc::clone(&arc));
                Ok(Some(arc))
            })
            .map_err(|e| e.to_string())?;
            pending_assets.extend(composed.pending_assets);
            let layer = self.atlas.allocate(tile, false, SlotKind::Static, frame, frame, frame).ok_or("GPU tile array exhausted for visible viewport")?;
            uploads.push(PendingUpload { layer, rgba8: Arc::new(composed.rgba8) });
            self.quads.push_tile(tile, layer, view)?;
        }

        let uploads_count = uploads.len();
        let id = self.submit_uploads_and_quads(size, &uploads)?;
        let tile_array_bytes = u64::from(TILE_PHYSICAL) * u64::from(TILE_PHYSICAL) * 4 * u64::from(self.budget.gpu_layers);
        let uploaded_asset_bytes: u64 = decoded_cache.values().map(|f| f.rgba.len() as u64).sum();
        self.stats = Stats {
            vertices: self.quads.vertex_count(),
            indices: self.quads.index_count() as usize,
            layers: devices_seen.len(),
            texture_bytes: u64::from(size[0]) * u64::from(size[1]) * 4,
            uploaded_asset_bytes,
            pending_assets: pending_assets.len(),
            tile_array_bytes,
            visible_tiles: visible.len(),
            resident_tiles: self.atlas.resident_count(),
            fallback_tiles: 0,
            pending_tiles: 0,
            anim_pending_tiles: 0,
            uploads_this_frame: uploads_count,
            target_lod,
            is_refining: false,
        };
        if let Some(fault) = self.health.fault() {
            return Err(fault.to_string());
        }
        Ok(id)
    }

    fn validate_frame_inputs(&self, view: &Viewport, density: f32, previews: &[SharedStroke]) -> Result<[u32; 2]> {
        if let Some(fault) = self.health.fault() {
            return Err(fault.to_string());
        }
        if !density.is_finite() || !(0.5..=4.0).contains(&density) {
            return Err("Invalid world display density".into());
        }
        let size = [(f64::from(view.width) * f64::from(density)).ceil() as u32, (f64::from(view.height) * f64::from(density)).ceil() as u32];
        let max_side = self.rs.device.limits().max_texture_dimension_2d.min(4096);
        if size.contains(&0) || size.iter().any(|n| *n > max_side) || u64::from(size[0]) * u64::from(size[1]) > MAX_PIXELS {
            return Err("Shared canvas GPU view exceeds its 4096-side / 8-megapixel surface budget".into());
        }
        if previews.len() > 257
            || previews.iter().map(|s| s.stroke.points.len()).fold(0usize, usize::saturating_add) > 256 * 128 + photocraft_engine::scene::MAX_POINTS
        {
            return Err("Shared drawing previews exceed their bounded sample budget".into());
        }
        let mut ids = BTreeSet::new();
        for shared in previews {
            shared.stroke.validate().map_err(|e| e.to_string())?;
            if shared.stroke.canvas != view.camera.canvas || !shared.visible || !ids.insert(shared.stroke.id) {
                return Err("Invalid or duplicate shared drawing preview".into());
            }
        }
        Ok(size)
    }

    fn ensure_output(&mut self, size: [u32; 2]) -> Result<egui::TextureId> {
        if self.targets.as_ref().is_none_or(|t| t.size != size) {
            if let Some(old) = self.targets.take() {
                self.rs.renderer.write().free_texture(&old.id);
            }
            let texture = self.rs.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("world tile output"),
                size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let id = self.rs.renderer.write().register_native_texture(&self.rs.device, &view, wgpu::FilterMode::Nearest);
            self.targets = Some(Targets { size, output: OutputSurface { _texture: texture, view }, id });
        }
        self.targets.as_ref().map(|t| t.id).ok_or_else(|| "Missing GPU world output surface".into())
    }

    fn submit_uploads_and_quads(&mut self, size: [u32; 2], uploads: &[PendingUpload]) -> Result<egui::TextureId> {
        let id = self.ensure_output(size)?;
        // Upload in staging-ring chunks so even a blocking multi-tile upload never allocates unbounded staging memory.
        for chunk in uploads.chunks(self.staging_slots.max(1)) {
            let mut encoder = self.rs.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("world tile upload") });
            for (slot_idx, item) in chunk.iter().enumerate() {
                pad_tile_rows(&item.rgba8, &mut self.pad_scratch)?;
                let offset = (slot_idx * STAGING_TILE_BYTES) as u64;
                self.rs.queue.write_buffer(&self.staging_ring, offset, &self.pad_scratch);
                encoder.copy_buffer_to_texture(
                    wgpu::TexelCopyBufferInfo {
                        buffer: &self.staging_ring,
                        layout: wgpu::TexelCopyBufferLayout { offset, bytes_per_row: Some(ALIGNED_ROW_BYTES), rows_per_image: Some(TILE_PHYSICAL) },
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.tile_array,
                        mip_level: 0,
                        origin: wgpu::Origin3d { x: 0, y: 0, z: item.layer },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d { width: TILE_PHYSICAL, height: TILE_PHYSICAL, depth_or_array_layers: 1 },
                );
            }
            self.rs.queue.submit([encoder.finish()]);
        }

        let targets = self.targets.as_ref().ok_or("Missing GPU output target")?;
        let index_count = self.quads.index_count();
        let mut encoder = self.rs.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("world tile quad pass") });
        if index_count == 0 {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("world clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.output.view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::WHITE), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        } else {
            let vbuf = upload_buffer(&self.rs.device, "world tile verts", &self.quads.vertices, wgpu::BufferUsages::VERTEX)?;
            let ibuf = upload_buffer(&self.rs.device, "world tile indices", &self.quads.indices, wgpu::BufferUsages::INDEX)?;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("world tile quad draw"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.output.view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::WHITE), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.tile_pipeline);
            pass.set_bind_group(0, &self.tile_bind, &[]);
            pass.set_vertex_buffer(0, vbuf.slice(..));
            pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..index_count, 0, 0..1);
        }
        self.rs.queue.submit([encoder.finish()]);
        Ok(id)
    }
}

pub(crate) struct DynamicItems {
    pub(crate) previews: Vec<(WorldRect, (StrokeId, u64))>,
    pub(crate) anim_media: Vec<(WorldRect, u64)>,
}

#[inline]
fn fnv_u64(mut h: u64, val: u64) -> u64 {
    h ^= val;
    h.wrapping_mul(0x0000_0100_0000_01B3)
}

#[inline]
fn fnv_bytes(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

pub(crate) fn collect_dynamic_items(mgr: &WorldTileManager, view: &Viewport, previews: &[SharedStroke], elapsed_ms: u64) -> Result<DynamicItems> {
    let mut prev_list = Vec::new();
    for p in previews {
        if p.visible {
            let rect = stroke_world_rect(&p.stroke).map_err(|e| e.to_string())?;
            let mut sig = 0xCBF2_9CE4_8422_2325_u64;
            sig = fnv_bytes(sig, p.stroke.id.as_str().as_bytes());
            sig = fnv_u64(sig, p.changed_revision);
            sig = fnv_u64(sig, p.stroke.points.len() as u64);
            if let Some(last) = p.stroke.points.last() {
                sig = fnv_u64(sig, last[0].to_bits());
                sig = fnv_u64(sig, last[1].to_bits());
            }
            if let Some(last_s) = p.stroke.samples.as_ref().and_then(|s| s.last()) {
                sig = fnv_u64(sig, last_s.time_ms.to_bits());
            }
            prev_list.push((rect, (p.stroke.id, sig)));
        }
    }
    let mut anim_list = Vec::new();
    if let Some(scene) = mgr.scene()
        && let Ok(vrect) = photocraft_engine::scene::spatial::viewport_world_rect(view)
    {
        for id in scene.query_world_rect_media(vrect) {
            if let Some(shared) = scene.world().media.get(&id)
                && shared.media.frames.len() > 1
                && let Some(entry) = scene.entry(photocraft_engine::world_tiles::SceneObject::Media(id))
            {
                let mut sig = 0xCBF2_9CE4_8422_2325_u64;
                sig = fnv_bytes(sig, id.as_str().as_bytes());
                if let Ok((f_idx, f_desc)) = shared.media.frame_at(elapsed_ms) {
                    sig = fnv_u64(sig, f_idx as u64);
                    sig = fnv_bytes(sig, f_desc.hash.as_str().as_bytes());
                    // Include media-availability bit so newly downloaded frames immediately re-trigger recomposition.
                    sig = fnv_u64(sig, u64::from(mgr.has_media_frame(&f_desc.hash)));
                }
                anim_list.push((entry.rect, sig));
            }
        }
    }
    Ok(DynamicItems { previews: prev_list, anim_media: anim_list })
}

pub(crate) fn compute_dynamic_tile_sig(
    scope_gen: u64,
    edit_seq: u64,
    live_stroke_id: Option<StrokeId>,
    live_rev: u64,
    phys: WorldRect,
    items: &DynamicItems,
) -> u64 {
    let mut h = 0xCBF2_9CE4_8422_2325_u64;
    h = fnv_u64(h, scope_gen);
    h = fnv_u64(h, edit_seq);
    h = fnv_u64(h, live_rev);
    for (r, (sid, psig)) in &items.previews {
        if r.intersects(&phys) {
            if live_stroke_id == Some(*sid) {
                h = fnv_bytes(h, sid.as_str().as_bytes());
                h = fnv_u64(h, live_rev ^ 0xA5A5_5A5A_F0F0_0F0F);
            } else {
                h = fnv_u64(h, *psig);
            }
        }
    }
    for (r, msig) in &items.anim_media {
        if r.intersects(&phys) {
            h = fnv_u64(h, *msig);
        }
    }
    if h == 0 { 1 } else { h }
}

pub(crate) fn compute_bg_anim_tile_sig(scope_gen: u64, edit_seq: u64, exclude_stroke_id: Option<StrokeId>, phys: WorldRect, items: &DynamicItems) -> u64 {
    let mut h = 0xCBF2_9CE4_8422_2325_u64;
    h = fnv_u64(h, scope_gen);
    h = fnv_u64(h, edit_seq);
    h = fnv_u64(h, 0);
    for (r, (sid, psig)) in &items.previews {
        if exclude_stroke_id != Some(*sid) && r.intersects(&phys) {
            h = fnv_u64(h, *psig);
        }
    }
    for (r, msig) in &items.anim_media {
        if r.intersects(&phys) {
            h = fnv_u64(h, *msig);
        }
    }
    if h == 0 { 1 } else { h }
}

pub(crate) fn collect_preview_device_rects(mgr: &WorldTileManager, previews: &[SharedStroke]) -> Result<Vec<(WorldRect, DeviceId)>> {
    let scene = mgr.scene();
    let mut out = Vec::with_capacity(previews.len());
    for shared in previews {
        if !shared.visible {
            continue;
        }
        if scene.is_some_and(|s| s.world().strokes.contains_key(&shared.stroke.id)) {
            continue;
        }
        let rect = stroke_world_rect(&shared.stroke).map_err(|e| e.to_string())?;
        out.push((rect, shared.device));
    }
    Ok(out)
}

pub(crate) fn tile_split_devices_for(phys: WorldRect, preview_rects: &[(WorldRect, DeviceId)], force_dev: Option<DeviceId>) -> Arc<[DeviceId]> {
    let mut devs = Vec::new();
    if let Some(d) = force_dev {
        devs.push(d);
    }
    for &(r, d) in preview_rects {
        if r.intersects(&phys) && !devs.contains(&d) {
            devs.push(d);
        }
    }
    devs.sort_unstable();
    Arc::from(devs)
}

pub(crate) fn compute_warm_sig(scope_gen: u64, edit_seq: u64, tile: TileKey, bg_sig: u64, split_devices: &[DeviceId]) -> u64 {
    let mut h = 0xCBF2_9CE4_8422_2325_u64;
    h = fnv_u64(h, scope_gen);
    h = fnv_u64(h, edit_seq);
    h = fnv_u64(h, u64::from(tile.lod));
    h = fnv_u64(h, tile.tx as u64);
    h = fnv_u64(h, (tile.tx >> 64) as u64);
    h = fnv_u64(h, tile.ty as u64);
    h = fnv_u64(h, (tile.ty >> 64) as u64);
    h = fnv_u64(h, bg_sig);
    for d in split_devices {
        h = fnv_bytes(h, d.as_str().as_bytes());
    }
    if h == 0 { 1 } else { h }
}

#[derive(Default)]
struct AsyncDynCounts {
    pending_assets: usize,
    anim_pending_tiles: usize,
    pending_tiles: usize,
    fallback_tiles: usize,
}

#[derive(Clone, Copy, Default)]
struct FallbackResult {
    drew_any: bool,
    fully_covered: bool,
}

fn best_resident_meta(stat: Option<(u32, SlotMeta)>, dynm: Option<(u32, SlotMeta)>) -> Option<(u32, SlotMeta)> {
    match (stat, dynm) {
        (Some(s), Some(d)) => {
            if d.1.validated_seq >= s.1.validated_seq {
                Some(d)
            } else {
                Some(s)
            }
        }
        (Some(s), None) => Some(s),
        (None, Some(d)) => Some(d),
        (None, None) => None,
    }
}

fn preregister_visible_anim_frames(
    mgr: &mut WorldTileManager,
    view: &Viewport,
    elapsed_ms: u64,
    resolve_media: &mut dyn FnMut(&MediaFrame) -> photocraft_engine::Result<Option<Arc<RgbaFrame>>>,
) -> Result<()> {
    if let Some(scene) = mgr.scene().cloned()
        && let Ok(vrect) = photocraft_engine::scene::spatial::viewport_world_rect(view)
    {
        let mut frames_to_register: Vec<MediaFrame> = Vec::new();
        for id in scene.query_world_rect_media(vrect) {
            if let Some(shared) = scene.world().media.get(&id)
                && shared.media.frames.len() > 1
                && let Ok((_, mf)) = shared.media.frame_at(elapsed_ms)
                && !mgr.has_media_frame(&mf.hash)
                && !frames_to_register.iter().any(|f| f.hash == mf.hash)
            {
                frames_to_register.push(mf.clone());
            }
        }
        for mf in frames_to_register {
            if let Some(decoded) = resolve_media(&mf).map_err(|e| e.to_string())? {
                mgr.register_media_frame(mf.hash, decoded);
            }
        }
    }
    Ok(())
}

fn upload_buffer(device: &wgpu::Device, label: &str, bytes: &[u8], usage: wgpu::BufferUsages) -> Result<wgpu::Buffer> {
    let size = (bytes.len() as u64).next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT).max(wgpu::COPY_BUFFER_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size, usage, mapped_at_creation: true });
    let copied = match buffer.slice(..).get_mapped_range_mut() {
        Ok(mut view) if view.len() >= bytes.len() => {
            view.slice(..bytes.len()).copy_from_slice(bytes);
            Ok(())
        }
        _ => Err("Shared canvas GPU buffer upload failed; retained drawing was not changed".to_string()),
    };
    buffer.unmap();
    copied?;
    Ok(buffer)
}

const TILE_SHADER: &str = r#"
struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) layer: u32,
};

@vertex
fn vs(@location(0) pos: vec2<f32>, @location(1) uv: vec2<f32>, @location(2) layer: u32) -> VOut {
    return VOut(vec4<f32>(pos, 0.0, 1.0), uv, layer);
}

@group(0) @binding(0) var tiles: texture_2d_array<f32>;
@group(0) @binding(1) var tile_sampler: sampler;

@fragment
fn fs(v: VOut) -> @location(0) vec4<f32> {
    return textureSample(tiles, tile_sampler, v.uv, i32(v.layer));
}
"#;
