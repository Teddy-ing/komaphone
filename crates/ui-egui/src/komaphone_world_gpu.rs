//! Native shared-canvas GPU compositor. Each device's drawing is rasterized into
//! an isolated transparent scratch layer; erase never touches another device.
//! The scratch allocation is reused after compositing, so texture memory does not
//! multiply by the number of members. Scene projection remains indexed and bounded.
use std::collections::{BTreeMap, BTreeSet};

use eframe::{egui_wgpu::RenderState, wgpu};
use photocraft_engine::{
    scene::{
        Viewport,
        shared::media::{AssetHash, MediaFrame},
    },
    world_media::{VisibleObject, WorldRenderer},
};

mod geometry;
#[cfg(test)]
pub(crate) mod tests;
use geometry::{Geometry, Kind};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const MAX_PIXELS: u64 = 8_388_608;
const MAX_ASSET_BYTES: u64 = 128 * 1024 * 1024;
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
    id: egui::TextureId,
}

pub struct WorldGpu {
    rs: RenderState,
    health: photocraft_gpu::DeviceHealth,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    normal: wgpu::RenderPipeline,
    erase: wgpu::RenderPipeline,
    image: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
    targets: Option<Targets>,
    assets: BTreeMap<AssetHash, Asset>,
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
        let composite = pipeline(device, &textured, &shader, "quad", "composite", wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING, false);
        if let Some(fault) = health.fault() {
            return Err(fault.to_string());
        }
        Ok(Self { rs: rs.clone(), health, layout, sampler, normal, erase, image, composite, targets: None, assets: BTreeMap::new(), stats: Stats::default() })
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
        previews: &[photocraft_engine::scene::shared::SharedStroke],
    ) -> Result<egui::TextureId> {
        self.render_available(renderer, view, density, elapsed_ms, &mut |frame| resolve(frame).map(Some), previews)
    }
    pub fn render_available(
        &mut self,
        renderer: &mut WorldRenderer,
        view: &Viewport,
        density: f32,
        elapsed_ms: u64,
        resolve: &mut dyn FnMut(&MediaFrame) -> photocraft_engine::Result<Option<Vec<u8>>>,
        previews: &[photocraft_engine::scene::shared::SharedStroke],
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
        let mut ids = BTreeSet::new();
        for shared in previews {
            shared.stroke.validate().map_err(|e| e.to_string())?;
            if shared.stroke.canvas != view.camera.canvas || !shared.visible || !ids.insert(shared.stroke.id) {
                return Err("Invalid or duplicate shared drawing preview".into());
            }
            // An accepted stable ID supersedes its transient preview immediately.
            if renderer.world().strokes.contains_key(&shared.stroke.id) {
                continue;
            }
            let mut transient = shared.clone();
            transient.created_revision = renderer.world().revision.saturating_add(1);
            transient.changed_revision = transient.created_revision;
            content.push(VisibleObject::Stroke(Box::new(transient)));
        }
        content.sort_by(|a, b| content_key(a).cmp(&content_key(b)));
        let mut geometry = Geometry::default();
        let mut needed = BTreeSet::new();
        let mut pending = BTreeSet::new();
        for object in content {
            match object {
                VisibleObject::Stroke(stroke) => geometry.stroke(&stroke, view, density)?,
                VisibleObject::Media(shared) => {
                    let (_, frame) = shared.media.frame_at(elapsed_ms).map_err(|e| e.to_string())?;
                    needed.insert(frame.hash);
                    // Prune between frames before upload, never keep unrelated artwork.
                    if self.assets.get(&frame.hash).is_none_or(|asset| {
                        asset.descriptor.byte_len != frame.byte_len || asset.descriptor.width != frame.width || asset.descriptor.height != frame.height
                    }) {
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
                            continue;
                        };
                        let decoded = photocraft_engine::world_media::decode_frame(frame, &encoded).map_err(|e| e.to_string())?;
                        if frame.width > self.rs.device.limits().max_texture_dimension_2d || frame.height > self.rs.device.limits().max_texture_dimension_2d {
                            return Err("Media exceeds this GPU's texture dimensions".into());
                        }
                        let surface = self.surface([frame.width, frame.height], false);
                        self.rs.queue.write_texture(
                            wgpu::TexelCopyTextureInfo {
                                texture: &surface._texture,
                                mip_level: 0,
                                origin: wgpu::Origin3d::ZERO,
                                aspect: wgpu::TextureAspect::All,
                            },
                            decoded.as_raw(),
                            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(frame.width * 4), rows_per_image: Some(frame.height) },
                            wgpu::Extent3d { width: frame.width, height: frame.height, depth_or_array_layers: 1 },
                        );
                        self.assets.insert(frame.hash, Asset { _surface: surface, descriptor: frame.clone(), bytes });
                    }
                    geometry.media(&shared, view, frame.hash)?;
                }
            }
        }
        self.assets.retain(|hash, _| needed.contains(hash));
        if geometry.vertices.len() as u64 > self.rs.device.limits().max_buffer_size || geometry.indices.len() as u64 > self.rs.device.limits().max_buffer_size {
            return Err("Visible shared-canvas geometry exceeds this GPU's buffer limit".into());
        }
        if self.targets.as_ref().is_none_or(|targets| targets.size != size) {
            if let Some(old) = self.targets.take() {
                self.rs.renderer.write().free_texture(&old.id);
            }
            let scratch = self.surface(size, true);
            let output = self.surface(size, true);
            let id = self.rs.renderer.write().register_native_texture(&self.rs.device, &output.view, wgpu::FilterMode::Nearest);
            self.targets = Some(Targets { size, scratch, output, id });
        }
        let targets = self.targets.as_ref().ok_or("Missing GPU world surfaces")?;
        let vertex = buffer(&self.rs.device, "world vertices", &geometry.vertices, wgpu::BufferUsages::VERTEX)?;
        let indices = buffer(&self.rs.device, "world indices", &geometry.indices, wgpu::BufferUsages::INDEX)?;
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
            {
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
                for batch in geometry.batches.get(first..end).ok_or("Invalid GPU layer range")? {
                    match batch.kind {
                        Kind::Color { erase } => pass.set_pipeline(if erase { &self.erase } else { &self.normal }),
                        Kind::Image(hash) => {
                            pass.set_pipeline(&self.image);
                            pass.set_bind_group(0, &self.assets.get(&hash).ok_or("Missing media texture")?._surface.bind, &[]);
                        }
                    }
                    pass.draw_indexed(batch.indices.clone(), 0, 0..1);
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
        self.stats = Stats {
            vertices: geometry.vertex_count(),
            indices: geometry.indices.len() / 4,
            layers,
            texture_bytes: u64::from(size[0]) * u64::from(size[1]) * 8,
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

fn content_key(item: &VisibleObject) -> (photocraft_engine::scene::shared::DeviceId, u64, &str) {
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
        label: Some("white world paper"),
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
