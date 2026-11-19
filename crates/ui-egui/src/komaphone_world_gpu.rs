//! Native shared-canvas GPU compositor. Each device's drawing is rasterized into
//! an isolated transparent scratch layer; erase never touches another device.
//! The scratch allocation is reused after compositing, so texture memory does not
//! multiply by the number of members. Scene projection remains indexed and bounded.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use eframe::{egui_wgpu::RenderState, wgpu};
use photocraft_engine::{
    scene::{
        StrokeId, Viewport, coordinates, render, round,
        shared::{
            DeviceId, SharedStroke,
            media::{AssetHash, MediaFrame},
        },
    },
    world_brush::{MAX_BRUSH_WORK_SAMPLES, RasterizedBrushStroke, WORLD_BRUSH_TILE, blend_mode_to_paint, composite_brush_onto_layer, rasterize_world_stroke},
    world_media::{VisibleObject, WorldRenderer},
};

mod geometry;
#[cfg(test)]
pub(crate) mod tests;
use geometry::{BrushOp, Geometry, Kind, TileCopy};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const MAX_PIXELS: u64 = 8_388_608;
const MAX_ASSET_BYTES: u64 = 128 * 1024 * 1024;
const MAX_BRUSH_CACHE_BYTES: usize = 64 * 1024 * 1024;
const MAX_STAGING_BYTES: usize = 128 * 1024 * 1024;
const TILE_STRIDE_BYTES: u32 = WORLD_BRUSH_TILE * 4;
const TILE_BYTES: usize = (WORLD_BRUSH_TILE * WORLD_BRUSH_TILE * 4) as usize;
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Stats {
    pub vertices: usize,
    pub indices: usize,
    pub layers: usize,
    pub texture_bytes: u64,
    pub uploaded_asset_bytes: u64,
    pub pending_assets: usize,
}

struct Surface {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
}

struct Asset {
    _surface: Surface,
    descriptor: MediaFrame,
    bytes: u64,
}

struct Targets {
    size: [u32; 2],
    scratch: Surface,
    output: Surface,
    /// Lazily allocated only when brush strokes are present; preserves the legacy
    /// `MAX_PIXELS * 8` memory budget for scenes with opaque round strokes only.
    stroke: Option<Surface>,
    id: egui::TextureId,
}

struct CachedTile {
    x0: u32,
    y0: u32,
    width: u32,
    height: u32,
    rgba8: Vec<u8>,
}

struct CachedBrush {
    op: BrushOp,
    tiles: Vec<CachedTile>,
    bytes: usize,
}

#[derive(Default)]
struct BrushCache {
    view: Option<Viewport>,
    density: u32,
    entries: BTreeMap<(StrokeId, u64), Arc<CachedBrush>>,
    bytes: usize,
}

impl BrushCache {
    fn sync_view(&mut self, view: &Viewport, density: f32) {
        let bits = density.to_bits();
        if self.view.as_ref() != Some(view) || self.density != bits {
            self.entries.clear();
            self.bytes = 0;
            self.view = Some(view.clone());
            self.density = bits;
        }
    }

    fn retain_ids(&mut self, keep: &BTreeSet<(StrokeId, u64)>) {
        self.entries.retain(|k, _| keep.contains(k));
        self.bytes = self.entries.values().map(|e| e.bytes).sum();
    }

    fn insert(&mut self, key: (StrokeId, u64), value: Arc<CachedBrush>) {
        if value.bytes > MAX_BRUSH_CACHE_BYTES {
            return;
        }
        while self.bytes.saturating_add(value.bytes) > MAX_BRUSH_CACHE_BYTES {
            let Some(first_key) = self.entries.keys().next().copied() else { break };
            if let Some(removed) = self.entries.remove(&first_key) {
                self.bytes = self.bytes.saturating_sub(removed.bytes);
            }
        }
        self.bytes = self.bytes.saturating_add(value.bytes);
        self.entries.insert(key, value);
    }
}

pub struct WorldGpu {
    rs: RenderState,
    health: photocraft_gpu::DeviceHealth,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    normal: wgpu::RenderPipeline,
    erase: wgpu::RenderPipeline,
    image: wgpu::RenderPipeline,
    brush_normal: wgpu::RenderPipeline,
    brush_erase: wgpu::RenderPipeline,
    brush_behind: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
    targets: Option<Targets>,
    assets: BTreeMap<AssetHash, Asset>,
    brush_cache: BrushCache,
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
        if let Some(fault) = health.fault() {
            return Err(fault.to_string());
        }
        let device = &rs.device;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("world texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
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
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("world pixels"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("world author layers"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let plain = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("world plain"), bind_group_layouts: &[], immediate_size: 0 });
        let textured = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("world textured"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let normal = pipeline(device, &plain, &shader, "vs", "color", wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING, true);
        let clear =
            wgpu::BlendComponent { src_factor: wgpu::BlendFactor::Zero, dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha, operation: wgpu::BlendOperation::Add };
        let erase = pipeline(device, &plain, &shader, "vs", "color", wgpu::BlendState { color: clear, alpha: clear }, true);
        let image = pipeline(device, &textured, &shader, "vs", "image", wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING, true);
        let brush_normal = pipeline(device, &textured, &shader, "vs", "composite", wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING, true);
        let brush_erase = pipeline(device, &textured, &shader, "vs", "composite", wgpu::BlendState { color: clear, alpha: clear }, true);
        let dst_over =
            wgpu::BlendComponent { src_factor: wgpu::BlendFactor::OneMinusDstAlpha, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add };
        let brush_behind = pipeline(device, &textured, &shader, "vs", "composite", wgpu::BlendState { color: dst_over, alpha: dst_over }, true);
        let composite = pipeline(device, &textured, &shader, "quad", "composite", wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING, false);
        if let Some(fault) = health.fault() {
            return Err(fault.to_string());
        }
        Ok(Self {
            rs: rs.clone(),
            health,
            layout,
            sampler,
            normal,
            erase,
            image,
            brush_normal,
            brush_erase,
            brush_behind,
            composite,
            targets: None,
            assets: BTreeMap::new(),
            brush_cache: BrushCache::default(),
            stats: Stats::default(),
        })
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

    #[allow(clippy::too_many_arguments)]
    fn encode_brush_stroke(
        &mut self,
        shared: &SharedStroke,
        view: &Viewport,
        density: f32,
        size: [u32; 2],
        is_preview: bool,
        live: Option<(StrokeId, &RasterizedBrushStroke)>,
        used_cache_keys: &mut BTreeSet<(StrokeId, u64)>,
        brush_work: &mut u64,
        staging_bytes: &mut Vec<u8>,
        geometry: &mut Geometry,
    ) -> Result<()> {
        if let Some((live_id, live_raster)) = live
            && live_id == shared.stroke.id
            && live_raster.phys_w == size[0]
            && live_raster.phys_h == size[1]
        {
            let cached = pack_raster_direct(live_raster);
            let copies = push_cached_tiles(&cached, staging_bytes)?;
            return geometry.brush_tiles(shared.device, cached.op, copies, size);
        }
        if !is_preview {
            let cache_key = (shared.stroke.id, shared.created_revision);
            used_cache_keys.insert(cache_key);
            let cached = if let Some(hit) = self.brush_cache.entries.get(&cache_key) {
                Arc::clone(hit)
            } else {
                let raster = rasterize_world_stroke(&shared.stroke, view, density, brush_work, MAX_BRUSH_WORK_SAMPLES).map_err(|e| e.to_string())?;
                let packed = Arc::new(match raster.as_ref() {
                    Some(r) => pack_raster_direct(r),
                    None => CachedBrush { op: BrushOp::Normal, tiles: Vec::new(), bytes: 0 },
                });
                self.brush_cache.insert(cache_key, Arc::clone(&packed));
                packed
            };
            let copies = push_cached_tiles(&cached, staging_bytes)?;
            return geometry.brush_tiles(shared.device, cached.op, copies, size);
        }
        if let Some(raster) = rasterize_world_stroke(&shared.stroke, view, density, brush_work, MAX_BRUSH_WORK_SAMPLES).map_err(|e| e.to_string())? {
            let cached = pack_raster_direct(&raster);
            let copies = push_cached_tiles(&cached, staging_bytes)?;
            geometry.brush_tiles(shared.device, cached.op, copies, size)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_media_object(
        &mut self,
        shared: &photocraft_engine::scene::shared::SharedMedia,
        view: &Viewport,
        elapsed_ms: u64,
        resolve: &mut dyn FnMut(&MediaFrame) -> photocraft_engine::Result<Option<Vec<u8>>>,
        needed: &mut BTreeSet<AssetHash>,
        pending: &mut BTreeSet<AssetHash>,
        geometry: &mut Geometry,
    ) -> Result<()> {
        let (_, frame) = shared.media.frame_at(elapsed_ms).map_err(|e| e.to_string())?;
        needed.insert(frame.hash);
        if self
            .assets
            .get(&frame.hash)
            .is_none_or(|asset| asset.descriptor.byte_len != frame.byte_len || asset.descriptor.width != frame.width || asset.descriptor.height != frame.height)
        {
            self.assets.remove(&frame.hash);
            let bytes = u64::from(frame.width) * u64::from(frame.height) * 4;
            if self.assets.values().map(|asset| asset.bytes).sum::<u64>().saturating_add(bytes) > MAX_ASSET_BYTES {
                self.assets.retain(|hash, _| needed.contains(hash));
                if self.assets.values().map(|asset| asset.bytes).sum::<u64>().saturating_add(bytes) > MAX_ASSET_BYTES {
                    return Err("Visible media exceeds the 128 MiB GPU asset cache".into());
                }
            }
            let Some(encoded) = resolve(frame).map_err(|e| e.to_string())? else {
                pending.insert(frame.hash);
                return Ok(());
            };
            let decoded = photocraft_engine::world_media::decode_frame(frame, &encoded).map_err(|e| e.to_string())?;
            if frame.width > self.rs.device.limits().max_texture_dimension_2d || frame.height > self.rs.device.limits().max_texture_dimension_2d {
                return Err("Media exceeds this GPU's texture dimensions".into());
            }
            let surface = self.surface([frame.width, frame.height], false);
            self.rs.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &surface._texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                decoded.as_raw(),
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(frame.width * 4), rows_per_image: Some(frame.height) },
                wgpu::Extent3d { width: frame.width, height: frame.height, depth_or_array_layers: 1 },
            );
            self.assets.insert(frame.hash, Asset { _surface: surface, descriptor: frame.clone(), bytes });
        }
        geometry.media(shared, view, frame.hash)
    }

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
        if let Some(fault) = self.health.fault() {
            return Err(fault.to_string());
        }
        if !density.is_finite() || !(0.5..=4.0).contains(&density) {
            return Err("Invalid world display density".into());
        }
        let size = [(view.width as f64 * f64::from(density)).ceil() as u32, (view.height as f64 * f64::from(density)).ceil() as u32];
        let max_side = self.rs.device.limits().max_texture_dimension_2d.min(4096);
        if size.contains(&0) || size.iter().any(|n| *n > max_side) || u64::from(size[0]) * u64::from(size[1]) > MAX_PIXELS {
            return Err("Shared canvas GPU view exceeds its 4096-side / 8-megapixel surface budget".into());
        }
        if previews.len() > 257
            || previews.iter().map(|s| s.stroke.points.len()).fold(0usize, usize::saturating_add) > 256 * 128 + photocraft_engine::scene::MAX_POINTS
        {
            return Err("Shared drawing previews exceed their bounded sample budget".into());
        }
        let mut content = renderer.visible_content(view).map_err(|e| e.to_string())?;
        let mut preview_ids = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for shared in previews {
            shared.stroke.validate().map_err(|e| e.to_string())?;
            if shared.stroke.canvas != view.camera.canvas || !shared.visible || !ids.insert(shared.stroke.id) {
                return Err("Invalid or duplicate shared drawing preview".into());
            }
            if renderer.world().strokes.contains_key(&shared.stroke.id) {
                continue;
            }
            preview_ids.insert(shared.stroke.id);
            let mut transient = shared.clone();
            transient.created_revision = renderer.world().revision.saturating_add(1);
            transient.changed_revision = transient.created_revision;
            content.push(VisibleObject::Stroke(Box::new(transient)));
        }
        content.sort_by(|a, b| content_key(a).cmp(&content_key(b)));
        self.brush_cache.sync_view(view, density);

        let mut geometry = Geometry::default();
        let mut staging_bytes: Vec<u8> = Vec::new();
        let mut needed = BTreeSet::new();
        let mut pending = BTreeSet::new();
        let mut used_cache_keys = BTreeSet::new();
        let mut brush_work = 0u64;

        // Group content by device so any device layer that uses non-Normal/non-Dissolve/non-Behind
        // blend modes (e.g., Multiply, Screen, Overlay) can composite on CPU cleanly while every
        // other device layer runs directly through hardware GPU pipelines.
        let mut first_obj = 0usize;
        while first_obj < content.len() {
            let device = content.get(first_obj).map(content_device).ok_or("Missing content item")?;
            let mut end_obj = first_obj + 1;
            while content.get(end_obj).is_some_and(|item| content_device(item) == device) {
                end_obj += 1;
            }
            let slice = content.get(first_obj..end_obj).ok_or("Invalid device object slice")?;
            let needs_cpu_blend = slice.iter().any(|item| match item {
                VisibleObject::Stroke(s) => s.stroke.brush.as_deref().is_some_and(|b| {
                    !s.stroke.erase
                        && !b.behind
                        && !matches!(blend_mode_to_paint(b.mode), photocraft_color::BlendMode::Normal | photocraft_color::BlendMode::Dissolve)
                }),
                VisibleObject::Media(_) => false,
            });

            if needs_cpu_blend {
                let copies =
                    cpu_composite_device_layer(slice, view, density, size, elapsed_ms, resolve, live, &mut pending, &mut brush_work, &mut staging_bytes)?;
                geometry.brush_tiles(device, BrushOp::Normal, copies, size)?;
            } else {
                for object in slice {
                    match object {
                        VisibleObject::Stroke(shared) => {
                            if shared.stroke.brush.is_some() {
                                let is_preview = preview_ids.contains(&shared.stroke.id);
                                self.encode_brush_stroke(
                                    shared,
                                    view,
                                    density,
                                    size,
                                    is_preview,
                                    live,
                                    &mut used_cache_keys,
                                    &mut brush_work,
                                    &mut staging_bytes,
                                    &mut geometry,
                                )?;
                            } else {
                                geometry.stroke(shared, view, density)?;
                            }
                        }
                        VisibleObject::Media(shared) => {
                            self.encode_media_object(shared, view, elapsed_ms, resolve, &mut needed, &mut pending, &mut geometry)?;
                        }
                    }
                }
            }
            first_obj = end_obj;
        }
        self.assets.retain(|hash, _| needed.contains(hash));
        self.brush_cache.retain_ids(&used_cache_keys);

        let max_buf = self.rs.device.limits().max_buffer_size;
        if geometry.vertices.len() as u64 > max_buf || geometry.indices.len() as u64 > max_buf || staging_bytes.len() as u64 > max_buf {
            return Err("Visible shared-canvas geometry exceeds this GPU's buffer limit".into());
        }
        if self.targets.as_ref().is_none_or(|targets| targets.size != size) {
            if let Some(old) = self.targets.take() {
                self.rs.renderer.write().free_texture(&old.id);
            }
            let scratch = self.surface(size, true);
            let output = self.surface(size, true);
            let id = self.rs.renderer.write().register_native_texture(&self.rs.device, &output.view, wgpu::FilterMode::Nearest);
            self.targets = Some(Targets { size, scratch, output, stroke: None, id });
        }
        let has_brush_uploads = !staging_bytes.is_empty();
        if has_brush_uploads && self.targets.as_ref().is_some_and(|targets| targets.stroke.is_none()) {
            let stroke_surface = self.surface(size, false);
            if let Some(targets) = self.targets.as_mut() {
                targets.stroke = Some(stroke_surface);
            }
        }
        let targets = self.targets.as_ref().ok_or("Missing GPU world surfaces")?;
        let vertex = buffer(&self.rs.device, "world vertices", &geometry.vertices, wgpu::BufferUsages::VERTEX)?;
        let indices = buffer(&self.rs.device, "world indices", &geometry.indices, wgpu::BufferUsages::INDEX)?;
        let staging =
            if has_brush_uploads { Some(buffer(&self.rs.device, "world brush staging", &staging_bytes, wgpu::BufferUsages::COPY_SRC)?) } else { None };

        let mut encoder = self.rs.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("world isolated author layers") });
        clear_surface(&mut encoder, &targets.output.view, wgpu::Color::WHITE);
        let mut layers = 0usize;
        let mut first = 0usize;
        while first < geometry.batches.len() {
            let device = geometry.batches.get(first).ok_or("Missing GPU draw")?.device;
            let mut end = first + 1;
            while geometry.batches.get(end).is_some_and(|batch| batch.device == device) {
                end += 1;
            }
            let layer_batches = geometry.batches.get(first..end).ok_or("Invalid GPU layer range")?;
            let layer_has_brushes = layer_batches.iter().any(|b| matches!(b.kind, Kind::Brush { .. }));
            if !layer_has_brushes {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("world author layer"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &targets.scratch.view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_vertex_buffer(0, vertex.slice(..));
                pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                for batch in layer_batches {
                    match &batch.kind {
                        Kind::Color { erase } => pass.set_pipeline(if *erase { &self.erase } else { &self.normal }),
                        Kind::Image(hash) => {
                            pass.set_pipeline(&self.image);
                            pass.set_bind_group(0, &self.assets.get(hash).ok_or("Missing media texture")?._surface.bind, &[]);
                        }
                        Kind::Brush { .. } => {}
                    }
                    pass.draw_indexed(batch.indices.clone(), 0, 0..1);
                }
            } else {
                clear_surface(&mut encoder, &targets.scratch.view, wgpu::Color::TRANSPARENT);
                let stroke_surface = targets.stroke.as_ref().ok_or("Missing GPU brush staging texture")?;
                let staging_buf = staging.as_ref().ok_or("Missing GPU brush staging buffer")?;
                let mut idx = 0usize;
                while idx < layer_batches.len() {
                    let Some(batch) = layer_batches.get(idx) else { break };
                    match &batch.kind {
                        Kind::Brush { op, copies } => {
                            for copy in copies {
                                encoder.copy_buffer_to_texture(
                                    wgpu::TexelCopyBufferInfo {
                                        buffer: staging_buf,
                                        layout: wgpu::TexelCopyBufferLayout {
                                            offset: copy.staging_offset,
                                            bytes_per_row: Some(TILE_STRIDE_BYTES),
                                            rows_per_image: Some(WORLD_BRUSH_TILE),
                                        },
                                    },
                                    wgpu::TexelCopyTextureInfo {
                                        texture: &stroke_surface._texture,
                                        mip_level: 0,
                                        origin: wgpu::Origin3d { x: copy.x0, y: copy.y0, z: 0 },
                                        aspect: wgpu::TextureAspect::All,
                                    },
                                    wgpu::Extent3d { width: copy.width, height: copy.height, depth_or_array_layers: 1 },
                                );
                            }
                            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                                label: Some("world brush stroke"),
                                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                    view: &targets.scratch.view,
                                    resolve_target: None,
                                    depth_slice: None,
                                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                                })],
                                depth_stencil_attachment: None,
                                timestamp_writes: None,
                                occlusion_query_set: None,
                                multiview_mask: None,
                            });
                            pass.set_vertex_buffer(0, vertex.slice(..));
                            pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                            pass.set_pipeline(match op {
                                BrushOp::Normal => &self.brush_normal,
                                BrushOp::Erase => &self.brush_erase,
                                BrushOp::Behind => &self.brush_behind,
                            });
                            pass.set_bind_group(0, &stroke_surface.bind, &[]);
                            pass.draw_indexed(batch.indices.clone(), 0, 0..1);
                            idx += 1;
                        }
                        Kind::Color { .. } | Kind::Image(_) => {
                            let start_plain = idx;
                            while layer_batches.get(idx).is_some_and(|b| !matches!(b.kind, Kind::Brush { .. })) {
                                idx += 1;
                            }
                            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                                label: Some("world author layer segment"),
                                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                    view: &targets.scratch.view,
                                    resolve_target: None,
                                    depth_slice: None,
                                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                                })],
                                depth_stencil_attachment: None,
                                timestamp_writes: None,
                                occlusion_query_set: None,
                                multiview_mask: None,
                            });
                            pass.set_vertex_buffer(0, vertex.slice(..));
                            pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                            for plain in layer_batches.get(start_plain..idx).unwrap_or_default() {
                                match &plain.kind {
                                    Kind::Color { erase } => pass.set_pipeline(if *erase { &self.erase } else { &self.normal }),
                                    Kind::Image(hash) => {
                                        pass.set_pipeline(&self.image);
                                        pass.set_bind_group(0, &self.assets.get(hash).ok_or("Missing media texture")?._surface.bind, &[]);
                                    }
                                    Kind::Brush { .. } => {}
                                }
                                pass.draw_indexed(plain.indices.clone(), 0, 0..1);
                            }
                        }
                    }
                }
            }
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("world layer composite"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &targets.output.view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.composite);
                pass.set_bind_group(0, &targets.scratch.bind, &[]);
                pass.draw(0..3, 0..1);
            }
            layers += 1;
            first = end;
        }
        self.rs.queue.submit([encoder.finish()]);
        let target_multiplier = if targets.stroke.is_some() { 12 } else { 8 };
        self.stats = Stats {
            vertices: geometry.vertex_count(),
            indices: geometry.indices.len() / 4,
            layers,
            texture_bytes: u64::from(size[0]) * u64::from(size[1]) * target_multiplier,
            uploaded_asset_bytes: self.assets.values().map(|asset| asset.bytes).sum(),
            pending_assets: pending.len(),
        };
        if let Some(fault) = self.health.fault() {
            return Err(fault.to_string());
        }
        Ok(targets.id)
    }

    fn surface(&self, size: [u32; 2], target: bool) -> Surface {
        let usages = wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC
            | if target { wgpu::TextureUsages::RENDER_ATTACHMENT } else { wgpu::TextureUsages::empty() };
        let texture = self.rs.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("isolated world layer"),
            size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: usages,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let bind = self.rs.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("world texture"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        Surface { _texture: texture, view, bind }
    }
}

fn pack_raster_direct(raster: &RasterizedBrushStroke) -> CachedBrush {
    let op = if raster.erase {
        BrushOp::Erase
    } else if raster.behind {
        BrushOp::Behind
    } else {
        BrushOp::Normal
    };
    let mut tiles = Vec::with_capacity(raster.tiles.len());
    let mut bytes = 0usize;
    for tile in &raster.tiles {
        let rgba8 = tile.to_premul_rgba8_padded(raster.aliased);
        bytes = bytes.saturating_add(rgba8.len());
        tiles.push(CachedTile { x0: tile.x0, y0: tile.y0, width: tile.width, height: tile.height, rgba8 });
    }
    CachedBrush { op, tiles, bytes }
}

fn push_cached_tiles(cached: &CachedBrush, staging: &mut Vec<u8>) -> Result<Vec<TileCopy>> {
    let mut copies = Vec::with_capacity(cached.tiles.len());
    for tile in &cached.tiles {
        if staging.len().saturating_add(tile.rgba8.len()) > MAX_STAGING_BYTES {
            return Err("Visible brush tiles exceed the GPU staging budget; zoom closer".into());
        }
        let offset = staging.len() as u64;
        staging.extend_from_slice(&tile.rgba8);
        copies.push(TileCopy { staging_offset: offset, x0: tile.x0, y0: tile.y0, width: tile.width, height: tile.height });
    }
    Ok(copies)
}

#[allow(clippy::too_many_arguments)]
fn cpu_composite_device_layer(
    objects: &[VisibleObject],
    view: &Viewport,
    density: f32,
    size: [u32; 2],
    elapsed_ms: u64,
    resolve: &mut dyn FnMut(&MediaFrame) -> photocraft_engine::Result<Option<Vec<u8>>>,
    live: Option<(StrokeId, &RasterizedBrushStroke)>,
    pending: &mut BTreeSet<AssetHash>,
    work: &mut u64,
    staging: &mut Vec<u8>,
) -> Result<Vec<TileCopy>> {
    let count = usize::try_from(u64::from(size[0]) * u64::from(size[1])).map_err(|_| "Layer pixel overflow".to_string())?;
    let mut pixels = vec![[0.0f32; 4]; count];
    let mut dirty_tiles = BTreeSet::<(u32, u32)>::new();
    let w = size[0];
    let h = size[1];
    let sx_scale = f64::from(w) / f64::from(view.width);
    let sy_scale = f64::from(h) / f64::from(view.height);

    for object in objects {
        match object {
            VisibleObject::Stroke(shared) => {
                if shared.stroke.brush.is_some() {
                    let owned;
                    let raster = if let Some((live_id, live_r)) = live
                        && live_id == shared.stroke.id
                        && live_r.phys_w == w
                        && live_r.phys_h == h
                    {
                        Some(live_r)
                    } else {
                        owned = rasterize_world_stroke(&shared.stroke, view, density, work, MAX_BRUSH_WORK_SAMPLES).map_err(|e| e.to_string())?;
                        owned.as_ref()
                    };
                    if let Some(raster) = raster {
                        for tile in &raster.tiles {
                            dirty_tiles.insert((tile.x0 / WORLD_BRUSH_TILE, tile.y0 / WORLD_BRUSH_TILE));
                        }
                        composite_brush_onto_layer(raster, &mut pixels, w, |_| {});
                    }
                } else if let Some(projected) = render::project(&shared.stroke, view).map_err(|e| e.to_string())? {
                    let mesh = round::mesh(&projected.paths, projected.width, [view.width, view.height], density, round::MAX_MESH_VERTICES)
                        .map_err(|e| e.to_string())?;
                    let mut cov = vec![0.0f32; count];
                    let mut touched = Vec::new();
                    let scale_pt = |p: [f32; 2]| [f64::from(p[0]) * sx_scale, f64::from(p[1]) * sy_scale];
                    let mut raster_tri = |pts: [[f64; 2]; 3], alpha: [f32; 3]| -> Result<()> {
                        let [a, b, c] = pts;
                        let edge = |p: [f64; 2], q: [f64; 2], r: [f64; 2]| (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0]);
                        let area = edge(a, b, c);
                        if !area.is_finite() || area.abs() < 1e-12 {
                            return Ok(());
                        }
                        let min_x = a[0].min(b[0]).min(c[0]).floor().clamp(0.0, f64::from(w)) as u32;
                        let max_x = a[0].max(b[0]).max(c[0]).ceil().clamp(0.0, f64::from(w)) as u32;
                        let min_y = a[1].min(b[1]).min(c[1]).floor().clamp(0.0, f64::from(h)) as u32;
                        let max_y = a[1].max(b[1]).max(c[1]).ceil().clamp(0.0, f64::from(h)) as u32;
                        *work = work.saturating_add(u64::from(max_x - min_x) * u64::from(max_y - min_y));
                        if *work > MAX_BRUSH_WORK_SAMPLES {
                            return Err("World blend render budget reached; zoom closer".into());
                        }
                        for py in min_y..max_y {
                            for px in min_x..max_x {
                                let sample = [f64::from(px) + 0.5, f64::from(py) + 0.5];
                                let u = edge(b, c, sample) / area;
                                let v = edge(c, a, sample) / area;
                                let ww = 1.0 - u - v;
                                if u >= -1e-8 && v >= -1e-8 && ww >= -1e-8 {
                                    let cval = (u * f64::from(alpha[0]) + v * f64::from(alpha[1]) + ww * f64::from(alpha[2])).clamp(0.0, 1.0) as f32;
                                    let idx = py as usize * w as usize + px as usize;
                                    if let Some(slot) = cov.get_mut(idx)
                                        && cval > *slot
                                    {
                                        if *slot == 0.0 {
                                            touched.push((px, py, idx));
                                        }
                                        *slot = cval;
                                    }
                                }
                            }
                        }
                        Ok(())
                    };
                    for tri in mesh.indices.chunks_exact(3) {
                        let a = mesh.vertices.get(tri[0] as usize).ok_or("Invalid triangle")?;
                        let b = mesh.vertices.get(tri[1] as usize).ok_or("Invalid triangle")?;
                        let c = mesh.vertices.get(tri[2] as usize).ok_or("Invalid triangle")?;
                        raster_tri([scale_pt(a.position), scale_pt(b.position), scale_pt(c.position)], [a.coverage, b.coverage, c.coverage])?;
                    }
                    for poly in &projected.fills {
                        if let Some(&first) = poly.first() {
                            for edge in poly.get(1..).unwrap_or_default().windows(2) {
                                raster_tri([scale_pt(first), scale_pt(edge[0]), scale_pt(edge[1])], [1.0; 3])?;
                            }
                        }
                    }
                    let color = projected.color.rgba;
                    for (px, py, idx) in touched {
                        let cval = cov.get(idx).copied().unwrap_or(0.0);
                        dirty_tiles.insert((px / WORLD_BRUSH_TILE, py / WORLD_BRUSH_TILE));
                        if let Some(dst) = pixels.get_mut(idx) {
                            if projected.erase {
                                for ch in dst.iter_mut() {
                                    *ch *= 1.0 - cval;
                                }
                            } else {
                                let a = cval * color[3];
                                for ch in 0..3 {
                                    dst[ch] = color[ch] * a + dst[ch] * (1.0 - a);
                                }
                                dst[3] = a + dst[3] * (1.0 - a);
                            }
                        }
                    }
                }
            }
            VisibleObject::Media(shared) => {
                let (_, frame) = shared.media.frame_at(elapsed_ms).map_err(|e| e.to_string())?;
                let Some(encoded) = resolve(frame).map_err(|e| e.to_string())? else {
                    pending.insert(frame.hash);
                    continue;
                };
                let image = photocraft_engine::world_media::decode_frame(frame, &encoded).map_err(|e| e.to_string())?;
                let media = &shared.media;
                let start = coordinates::point_to_screen(view, coordinates::point(&media.anchor, media.offset).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
                let end = coordinates::point_to_screen(
                    view,
                    coordinates::point(&media.anchor, [media.offset[0] + media.width, media.offset[1] + media.height]).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                let s0 = [start[0] * sx_scale, start[1] * sy_scale];
                let s1 = [end[0] * sx_scale, end[1] * sy_scale];
                let span = [s1[0] - s0[0], s1[1] - s0[1]];
                if span[0] <= 0.0 || span[1] <= 0.0 || !span[0].is_finite() || !span[1].is_finite() {
                    continue;
                }
                let x0 = s0[0].floor().clamp(0.0, f64::from(w)) as u32;
                let x1 = s1[0].ceil().clamp(0.0, f64::from(w)) as u32;
                let y0 = s0[1].floor().clamp(0.0, f64::from(h)) as u32;
                let y1 = s1[1].ceil().clamp(0.0, f64::from(h)) as u32;
                *work = work.saturating_add(u64::from(x1 - x0) * u64::from(y1 - y0));
                if *work > MAX_BRUSH_WORK_SAMPLES {
                    return Err("World blend media render budget reached; zoom closer".into());
                }
                for py in y0..y1 {
                    for px in x0..x1 {
                        let u = (f64::from(px) + 0.5 - s0[0]) / span[0];
                        let v = (f64::from(py) + 0.5 - s0[1]) / span[1];
                        if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
                            continue;
                        }
                        let ix = ((u * f64::from(image.width())).floor() as u32).min(image.width().saturating_sub(1));
                        let iy = ((v * f64::from(image.height())).floor() as u32).min(image.height().saturating_sub(1));
                        let src = image.get_pixel_checked(ix, iy).ok_or("Invalid media pixel")?.0;
                        let a = f32::from(src[3]) / 255.0;
                        let idx = py as usize * w as usize + px as usize;
                        dirty_tiles.insert((px / WORLD_BRUSH_TILE, py / WORLD_BRUSH_TILE));
                        if let Some(dst) = pixels.get_mut(idx) {
                            for ch in 0..3 {
                                dst[ch] = f32::from(src[ch]) / 255.0 * a + dst[ch] * (1.0 - a);
                            }
                            dst[3] = a + dst[3] * (1.0 - a);
                        }
                    }
                }
            }
        }
    }

    let mut copies = Vec::with_capacity(dirty_tiles.len());
    for (tx, ty) in dirty_tiles {
        let x0 = tx * WORLD_BRUSH_TILE;
        let y0 = ty * WORLD_BRUSH_TILE;
        let tw = w.saturating_sub(x0).min(WORLD_BRUSH_TILE);
        let th = h.saturating_sub(y0).min(WORLD_BRUSH_TILE);
        if tw == 0 || th == 0 {
            continue;
        }
        if staging.len().saturating_add(TILE_BYTES) > MAX_STAGING_BYTES {
            return Err("Visible blend layer tiles exceed the GPU staging budget; zoom closer".into());
        }
        let offset = staging.len() as u64;
        let mut buf = vec![0u8; TILE_BYTES];
        let mut any = false;
        for ly in 0..th {
            let py = (y0 + ly) as usize;
            for lx in 0..tw {
                let px = (x0 + lx) as usize;
                let Some(&v) = pixels.get(py * w as usize + px) else { continue };
                if v[3] <= 0.0 {
                    continue;
                }
                any = true;
                let a = (v[3].clamp(0.0, 1.0) * 255.0).round() as u8;
                let r = ((v[0].clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
                let g = ((v[1].clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
                let b = ((v[2].clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
                let o = (ly as usize * WORLD_BRUSH_TILE as usize + lx as usize) * 4;
                if let Some(slot) = buf.get_mut(o..o + 4) {
                    slot[0] = r;
                    slot[1] = g;
                    slot[2] = b;
                    slot[3] = a;
                }
            }
        }
        if any {
            staging.extend_from_slice(&buf);
            copies.push(TileCopy { staging_offset: offset, x0, y0, width: tw, height: th });
        }
    }
    Ok(copies)
}

fn content_device(item: &VisibleObject) -> DeviceId {
    match item {
        VisibleObject::Stroke(s) => s.device,
        VisibleObject::Media(m) => m.device,
    }
}

fn content_key(item: &VisibleObject) -> (DeviceId, u64, &str) {
    match item {
        VisibleObject::Stroke(s) => (s.device, s.created_revision, s.stroke.id.as_str()),
        VisibleObject::Media(m) => (m.device, m.created_revision, m.media.id.as_str()),
    }
}

fn buffer(device: &wgpu::Device, label: &str, bytes: &[u8], usage: wgpu::BufferUsages) -> Result<wgpu::Buffer> {
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

fn pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    vs: &str,
    fs: &str,
    blend: wgpu::BlendState,
    vertices: bool,
) -> wgpu::RenderPipeline {
    let attributes = wgpu::vertex_attr_array![0=>Float32x2,1=>Float32x2,2=>Float32x4];
    let buffers = [Some(wgpu::VertexBufferLayout { array_stride: 32, step_mode: wgpu::VertexStepMode::Vertex, attributes: &attributes })];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("world layer pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState { module, entry_point: Some(vs), buffers: if vertices { &buffers } else { &[] }, compilation_options: Default::default() },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(fs),
            targets: &[Some(wgpu::ColorTargetState { format: FORMAT, blend: Some(blend), write_mask: wgpu::ColorWrites::ALL })],
            compilation_options: Default::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}

fn clear_surface(encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, color: wgpu::Color) {
    let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("world clear"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(color), store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
}

const SHADER: &str = r#"
struct V { @builtin(position) position:vec4<f32>, @location(0) uv:vec2<f32>, @location(1) color:vec4<f32> };
@vertex fn vs(@location(0) position:vec2<f32>,@location(1) uv:vec2<f32>,@location(2) color:vec4<f32>)->V {return V(vec4(position,0.0,1.0),uv,color);}
@vertex fn quad(@builtin(vertex_index) id:u32)->V {let p=array<vec2<f32>,3>(vec2(-1.0,-1.0),vec2(3.0,-1.0),vec2(-1.0,3.0));let position=p[id];return V(vec4(position,0.0,1.0),vec2((position.x+1.0)*0.5,(1.0-position.y)*0.5),vec4(1.0));}
@group(0) @binding(0) var tex:texture_2d<f32>;
@group(0) @binding(1) var tex_sampler:sampler;
@fragment fn color(v:V)->@location(0) vec4<f32> {return v.color;}
@fragment fn image(v:V)->@location(0) vec4<f32> {let c=textureSample(tex,tex_sampler,v.uv);return vec4(c.rgb*c.a,c.a)*v.color;}
@fragment fn composite(v:V)->@location(0) vec4<f32> {return textureSample(tex,tex_sampler,v.uv);}
"#;
