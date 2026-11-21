use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;

use image::{ImageDecoder, ImageEncoder, ImageReader};
use photocraft_scene::shared::media::{AssetHash, MediaFrame};
use photocraft_scene::shared::{AcceptedOp, DeviceId, MediaId, WorldState};
use photocraft_scene::spatial::{WorldRect, viewport_world_rect, world_rect_to_screen};
use photocraft_scene::{StrokeId, Viewport, coordinates, render, round};
use sha2::{Digest, Sha256};

use super::{RenderStats, WorldImage};
use crate::world_tiles::{SceneObject, SummaryEpoch, SummaryKey, WorldSceneIndex};
use crate::{EngineError, Result};

mod exports;
pub use exports::*;

pub const MAX_RENDER_SIDE: u32 = 2048;
const MAX_STROKE_RASTER_SAMPLES: u64 = 268_435_456;

fn error(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}
fn other(message: impl std::fmt::Display) -> EngineError {
    error(message.to_string())
}

enum ExportItem {
    Stroke(StrokeId),
    Media(MediaId),
    Summary(SummaryKey, SummaryEpoch),
}

/// A pinned scene and octave-bucketed `WorldSceneIndex`, reused across camera,
/// incremental review/edit updates, tiled GPU presentation, and CPU exports.
/// Lifts the legacy `MAX_VISIBLE_STROKES = 4,096` overview ceiling via bounded
/// sparse micro-summaries while preserving exact `(DeviceId, created_revision)` order.
#[derive(Clone)]
pub struct WorldRenderer {
    index: Arc<WorldSceneIndex>,
    decoded: Option<(MediaFrame, image::RgbaImage)>,
    pub stats: RenderStats,
}

#[derive(Clone, Debug)]
pub enum VisibleObject {
    Stroke(Box<photocraft_scene::shared::SharedStroke>),
    Media(Box<photocraft_scene::shared::SharedMedia>),
}

impl WorldRenderer {
    pub fn new(world: &WorldState) -> Result<Self> {
        let index = Arc::new(WorldSceneIndex::new(world)?);
        Ok(Self { index, decoded: None, stats: RenderStats::default() })
    }

    pub fn from_index(index: Arc<WorldSceneIndex>) -> Self {
        Self { index, decoded: None, stats: RenderStats::default() }
    }

    pub fn world(&self) -> &WorldState {
        self.index.world()
    }

    /// Lock-free shared reference to the underlying octave-bucketed spatial index.
    pub fn index(&self) -> &Arc<WorldSceneIndex> {
        &self.index
    }

    /// Incrementally apply one accepted event in O(1), returning its dirty world rect.
    pub fn apply_accepted(&mut self, event: &AcceptedOp) -> Result<Option<WorldRect>> {
        Arc::make_mut(&mut self.index).apply_accepted(event)
    }

    /// Incrementally advance or rewind this renderer to `target_rev` relative to `canonical`.
    pub fn step_review_to(&mut self, canonical: &WorldState, target_rev: u64) -> Result<Vec<WorldRect>> {
        Arc::make_mut(&mut self.index).step_review_to(canonical, target_rev)
    }

    /// Prefetch current image descriptors without copying retained strokes or
    /// hitting the legacy 4,096-stroke overview cap.
    pub fn media_frames(&mut self, view: &Viewport, elapsed_ms: u64) -> Result<(Vec<MediaFrame>, bool)> {
        coordinates::validate_view(view).map_err(other)?;
        if view.camera.canvas != self.world().canvas {
            return Err(error("Viewport belongs to another canvas"));
        }
        let rect = viewport_world_rect(view).map_err(other)?;
        let ids = self.index.query_world_rect_media(rect);
        let mut frames = Vec::new();
        let mut animated = false;
        for id in ids {
            let Some(shared) = self.world().media.get(&id) else {
                continue;
            };
            let media = &shared.media;
            let start = coordinates::point_to_screen(view, coordinates::point(&media.anchor, media.offset).map_err(other)?).map_err(other)?;
            let end = coordinates::point_to_screen(
                view,
                coordinates::point(&media.anchor, [media.offset[0] + media.width, media.offset[1] + media.height]).map_err(other)?,
            )
            .map_err(other)?;
            if end[0] <= 0.0 || end[1] <= 0.0 || start[0] >= f64::from(view.width) || start[1] >= f64::from(view.height) {
                continue;
            }
            animated |= media.frames.len() > 1;
            frames.push(media.frame_at(elapsed_ms).map_err(other)?.1.clone());
        }
        Ok((frames, animated))
    }

    /// Indexed, ordered direct display derivatives intersecting `view`.
    /// Never fails on > 4,096 overview strokes.
    pub fn visible_content(&mut self, view: &Viewport) -> Result<Vec<VisibleObject>> {
        coordinates::validate_view(view).map_err(other)?;
        if view.camera.canvas != self.world().canvas {
            return Err(error("Viewport belongs to another canvas"));
        }
        let rect = viewport_world_rect(view).map_err(other)?;
        let tpp = view_pixel_pitch(view)?;
        let query = self.index.query_rect_at_pitch(rect, tpp);
        self.stats = RenderStats {
            indexed_references: self.index.indexed_references(),
            queried_cells: 1 + query.summary_nodes_visited,
            candidates: query.direct.len(),
            ..Default::default()
        };
        let mut seen = std::collections::BTreeSet::new();
        let mut ordered = Vec::with_capacity(query.direct.len());
        for obj in query.direct {
            if !seen.insert(obj) {
                continue;
            }
            if let SceneObject::Media(id) = obj
                && !media_intersects_screen(self.world(), id, view)?
            {
                continue;
            }
            if let Some(entry) = self.index.entry(obj) {
                ordered.push((entry.device, entry.created_rev, obj));
            }
        }
        if !query.summaries.is_empty() && ordered.len() < 4096 {
            for obj in self.index.query_rect_any(rect, 4096) {
                if ordered.len() >= 4096 {
                    break;
                }
                if !seen.insert(obj) {
                    continue;
                }
                if let SceneObject::Media(id) = obj
                    && !media_intersects_screen(self.world(), id, view)?
                {
                    continue;
                }
                if let Some(entry) = self.index.entry(obj) {
                    ordered.push((entry.device, entry.created_rev, obj));
                }
            }
        }
        ordered.sort_unstable();
        let mut result = Vec::with_capacity(ordered.len());
        for (_, _, obj) in ordered {
            match obj {
                SceneObject::Stroke(id) => {
                    let mut shared = self.world().strokes.get(&id).ok_or_else(|| error("Missing visible stroke"))?.clone();
                    if stroke_extent(&shared.stroke) * coordinates::scale_ratio(shared.stroke.anchor.depth, &view.camera).map_err(other)? <= 2.0 {
                        shared.stroke = render::decimate(&shared.stroke, photocraft_scene::MAX_SUMMARY_POINTS).map_err(other)?;
                    }
                    self.stats.source_points += shared.stroke.points.len();
                    result.push(VisibleObject::Stroke(Box::new(shared)));
                }
                SceneObject::Media(id) => {
                    result.push(VisibleObject::Media(Box::new(self.world().media.get(&id).ok_or_else(|| error("Missing visible media"))?.clone())));
                }
            }
        }
        Ok(result)
    }

    pub fn render(&mut self, view: &Viewport, elapsed_ms: u64, resolve: &mut dyn FnMut(&MediaFrame) -> Result<Vec<u8>>) -> Result<WorldImage> {
        self.render_with(view, elapsed_ms, resolve, &mut || true)
    }

    pub fn render_with(
        &mut self,
        view: &Viewport,
        elapsed_ms: u64,
        resolve: &mut dyn FnMut(&MediaFrame) -> Result<Vec<u8>>,
        progress: &mut dyn FnMut() -> bool,
    ) -> Result<WorldImage> {
        coordinates::validate_view(view).map_err(other)?;
        if view.camera.canvas != self.world().canvas || view.width > MAX_RENDER_SIDE || view.height > MAX_RENDER_SIDE {
            return Err(error("Choose a same-canvas output no larger than 2048 × 2048"));
        }
        let rect = viewport_world_rect(view).map_err(other)?;
        let tpp = view_pixel_pitch(view)?;
        let query = self.index.query_rect_at_pitch(rect, tpp);
        let plan = build_export_plan(&self.index, &query, view)?;

        self.stats = RenderStats {
            indexed_references: self.index.indexed_references(),
            queried_cells: 1 + query.summary_nodes_visited,
            candidates: query.direct.len() + query.summaries.len(),
            ..Default::default()
        };

        let count = usize::try_from(u64::from(view.width) * u64::from(view.height)).map_err(other)?;
        let mut output = vec![[1.0; 4]; count];
        let mut layer = LayerBuffer::new(count);

        for items in plan.values() {
            for (_, _, item) in items {
                if !progress() {
                    return Err(error("Export cancelled; saved artwork is unchanged"));
                }
                match item {
                    ExportItem::Stroke(id) => {
                        let source = &self.index.world().strokes.get(id).ok_or_else(|| error("Missing stroke"))?.stroke;
                        let extent = stroke_extent(source);
                        let scale = coordinates::scale_ratio(source.anchor.depth, &view.camera).map_err(other)?;
                        let summary;
                        let stroke = if extent * scale <= 2.0 {
                            summary = render::decimate(source, photocraft_scene::MAX_SUMMARY_POINTS).map_err(other)?;
                            &summary
                        } else {
                            source
                        };
                        self.stats.source_points += stroke.points.len();
                        let mut stroke_work = 0u64;
                        if stroke.brush.is_some() {
                            if let Some(raster) = crate::world_brush::rasterize_world_stroke(stroke, view, 1.0, &mut stroke_work, MAX_STROKE_RASTER_SAMPLES)? {
                                self.stats.raster_samples = self.stats.raster_samples.saturating_add(stroke_work);
                                layer.composite_brush(&raster, view.width);
                                self.stats.rendered_objects += 1;
                            }
                        } else if let Some(projected) = render::project(stroke, view).map_err(other)? {
                            let mesh =
                                round::mesh(&projected.paths, projected.width, [view.width, view.height], 1.0, round::MAX_MESH_VERTICES).map_err(other)?;
                            for triangle in mesh.indices.chunks_exact(3) {
                                let a = mesh.vertices.get(triangle[0] as usize).ok_or_else(|| error("Invalid stroke triangle"))?;
                                let b = mesh.vertices.get(triangle[1] as usize).ok_or_else(|| error("Invalid stroke triangle"))?;
                                let c = mesh.vertices.get(triangle[2] as usize).ok_or_else(|| error("Invalid stroke triangle"))?;
                                layer.triangle([a.position, b.position, c.position], [a.coverage, b.coverage, c.coverage], view, &mut stroke_work)?;
                            }
                            for polygon in &projected.fills {
                                if let Some(&first) = polygon.first() {
                                    for edge in polygon.get(1..).unwrap_or_default().windows(2) {
                                        layer.triangle([first, edge[0], edge[1]], [1.0; 3], view, &mut stroke_work)?;
                                    }
                                }
                            }
                            self.stats.raster_samples = self.stats.raster_samples.saturating_add(stroke_work);
                            layer.apply_coverage(projected.color.rgba, projected.erase);
                            self.stats.rendered_objects += 1;
                        }
                    }
                    ExportItem::Media(id) => {
                        let media = self.world().media.get(id).ok_or_else(|| error("Missing media"))?.media.clone();
                        let (_, descriptor) = media.frame_at(elapsed_ms).map_err(other)?;
                        if self.decoded.as_ref().is_none_or(|(cached, _)| cached != descriptor) {
                            self.decoded = Some((descriptor.clone(), decode_frame(descriptor, &resolve(descriptor)?)?));
                        }
                        let image = &self.decoded.as_ref().ok_or_else(|| error("Missing decoded frame"))?.1;
                        self.stats.decoded_asset_bytes = image.as_raw().len();
                        let start = coordinates::point_to_screen(view, coordinates::point(&media.anchor, media.offset).map_err(other)?).map_err(other)?;
                        let end = coordinates::point_to_screen(
                            view,
                            coordinates::point(&media.anchor, [media.offset[0] + media.width, media.offset[1] + media.height]).map_err(other)?,
                        )
                        .map_err(other)?;
                        let mut media_work = 0u64;
                        layer.image(image, start, end, view, &mut media_work)?;
                        self.stats.raster_samples = self.stats.raster_samples.saturating_add(media_work);
                        self.stats.rendered_objects += 1;
                    }
                    ExportItem::Summary(skey, epoch) => {
                        layer.summary(*skey, epoch, view)?;
                        self.stats.rendered_objects += 1;
                    }
                }
            }
            layer.composite(&mut output);
        }
        Ok(WorldImage { width: view.width, height: view.height, pixels: output })
    }
}

fn view_pixel_pitch(view: &Viewport) -> Result<i128> {
    let unit = coordinates::local_unit(view.camera.region.depth).map_err(other)? as f64;
    let tpp = (unit / view.camera.zoom).max(1.0);
    if !tpp.is_finite() {
        return Err(error("Invalid viewport pixel pitch"));
    }
    Ok(tpp.floor() as i128)
}

fn media_intersects_screen(world: &WorldState, id: MediaId, view: &Viewport) -> Result<bool> {
    let Some(shared) = world.media.get(&id) else {
        return Ok(false);
    };
    let media = &shared.media;
    let start = coordinates::point_to_screen(view, coordinates::point(&media.anchor, media.offset).map_err(other)?).map_err(other)?;
    let end =
        coordinates::point_to_screen(view, coordinates::point(&media.anchor, [media.offset[0] + media.width, media.offset[1] + media.height]).map_err(other)?)
            .map_err(other)?;
    Ok(!(end[0] <= 0.0 || end[1] <= 0.0 || start[0] >= f64::from(view.width) || start[1] >= f64::from(view.height)))
}

type ExportDeviceMap = BTreeMap<DeviceId, Vec<(u64, u8, ExportItem)>>;

fn build_export_plan(index: &WorldSceneIndex, query: &crate::world_tiles::TileQuery, view: &Viewport) -> Result<ExportDeviceMap> {
    let mut plan: ExportDeviceMap = BTreeMap::new();
    let mut direct_revs: BTreeMap<DeviceId, Vec<(u64, WorldRect)>> = BTreeMap::new();
    for &obj in &query.direct {
        let Some(entry) = index.entry(obj) else { continue };
        match obj {
            SceneObject::Stroke(id) => {
                plan.entry(entry.device).or_default().push((entry.created_rev, 0, ExportItem::Stroke(id)));
                direct_revs.entry(entry.device).or_default().push((entry.created_rev, entry.rect));
            }
            SceneObject::Media(id) => {
                if !media_intersects_screen(index.world(), id, view)? {
                    continue;
                }
                plan.entry(entry.device).or_default().push((entry.created_rev, 1, ExportItem::Media(id)));
                direct_revs.entry(entry.device).or_default().push((entry.created_rev, entry.rect));
            }
        }
    }
    for (skey, dev, epoch) in &query.summaries {
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
            plan.entry(*dev).or_default().push((epoch.min_rev, 2, ExportItem::Summary(*skey, epoch.clone())));
        } else {
            splits.sort_unstable();
            splits.dedup();
            for sub in index.split_summary_epochs(*skey, *dev, &splits) {
                plan.entry(*dev).or_default().push((sub.min_rev, 2, ExportItem::Summary(*skey, sub)));
            }
        }
    }
    for items in plan.values_mut() {
        items.sort_by_key(|&(rev, kind, _)| (rev, kind));
    }
    Ok(plan)
}

fn stroke_extent(stroke: &photocraft_scene::Stroke) -> f64 {
    let mut min = [f64::INFINITY; 2];
    let mut max = [f64::NEG_INFINITY; 2];
    for point in &stroke.points {
        for axis in 0..2 {
            min[axis] = min[axis].min(point[axis]);
            max[axis] = max[axis].max(point[axis]);
        }
    }
    let reach = stroke.brush.as_ref().map_or(1.0, |brush| brush.reach_factor());
    (max[0] - min[0]).max(max[1] - min[1]) + stroke.width * reach
}

struct LayerBuffer {
    pixels: Vec<[f32; 4]>,
    coverage: Vec<f32>,
    covered: Vec<usize>,
    dirty: Vec<usize>,
    marked: Vec<bool>,
}
impl LayerBuffer {
    fn new(count: usize) -> Self {
        Self { pixels: vec![[0.0; 4]; count], coverage: vec![0.0; count], covered: Vec::new(), dirty: Vec::new(), marked: vec![false; count] }
    }
    fn touch(&mut self, index: usize) {
        if let Some(marked) = self.marked.get_mut(index)
            && !*marked
        {
            *marked = true;
            self.dirty.push(index);
        }
    }
    fn composite_brush(&mut self, raster: &crate::world_brush::RasterizedBrushStroke, width: u32) {
        let Self { pixels, dirty, marked, .. } = self;
        crate::world_brush::composite_brush_onto_layer(raster, pixels, width, |index| {
            if let Some(flag) = marked.get_mut(index)
                && !*flag
            {
                *flag = true;
                dirty.push(index);
            }
        });
    }
    fn summary(&mut self, skey: SummaryKey, epoch: &SummaryEpoch, view: &Viewport) -> Result<()> {
        let w = view.width as usize;
        let h = view.height as usize;
        for &(sub_idx, px) in &epoch.cells {
            if px[3] <= 0.0 {
                continue;
            }
            let [x0, y0, x1, y1] = world_rect_to_screen(skey.subcell_rect(sub_idx), view).map_err(other)?;
            let sx0 = f64::from(x0).clamp(0.0, f64::from(view.width));
            let sy0 = f64::from(y0).clamp(0.0, f64::from(view.height));
            let sx1 = f64::from(x1).clamp(0.0, f64::from(view.width));
            let sy1 = f64::from(y1).clamp(0.0, f64::from(view.height));
            if sx0 >= sx1 || sy0 >= sy1 {
                continue;
            }
            let ix0 = sx0.floor() as usize;
            let ix1 = (sx1.ceil() as usize).min(w);
            let iy0 = sy0.floor() as usize;
            let iy1 = (sy1.ceil() as usize).min(h);
            for py in iy0..iy1 {
                let oy_frac = (sy1.min((py + 1) as f64) - sy0.max(py as f64)).clamp(0.0, 1.0) as f32;
                let row = py * w;
                for pxx in ix0..ix1 {
                    let ox_frac = (sx1.min((pxx + 1) as f64) - sx0.max(pxx as f64)).clamp(0.0, 1.0) as f32;
                    let weight = ox_frac * oy_frac;
                    if weight <= 0.0 {
                        continue;
                    }
                    let idx = row + pxx;
                    self.touch(idx);
                    if let Some(dst) = self.pixels.get_mut(idx) {
                        let sa = (px[3] * weight).clamp(0.0, 1.0);
                        let inv = 1.0 - sa;
                        dst[0] = (px[0] * weight + dst[0] * inv).clamp(0.0, 1.0);
                        dst[1] = (px[1] * weight + dst[1] * inv).clamp(0.0, 1.0);
                        dst[2] = (px[2] * weight + dst[2] * inv).clamp(0.0, 1.0);
                        dst[3] = (sa + dst[3] * inv).clamp(0.0, 1.0);
                    }
                }
            }
        }
        Ok(())
    }
    fn triangle(&mut self, points: [[f32; 2]; 3], alpha: [f32; 3], view: &Viewport, work: &mut u64) -> Result<()> {
        let [a, b, c] = points.map(|point| [f64::from(point[0]), f64::from(point[1])]);
        let edge = |p: [f64; 2], q: [f64; 2], r: [f64; 2]| (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0]);
        let area = edge(a, b, c);
        if !area.is_finite() || area.abs() < 1e-12 {
            return Ok(());
        }
        let min_x = a[0].min(b[0]).min(c[0]).floor().clamp(0.0, f64::from(view.width)) as u32;
        let max_x = a[0].max(b[0]).max(c[0]).ceil().clamp(0.0, f64::from(view.width)) as u32;
        let min_y = a[1].min(b[1]).min(c[1]).floor().clamp(0.0, f64::from(view.height)) as u32;
        let max_y = a[1].max(b[1]).max(c[1]).ceil().clamp(0.0, f64::from(view.height)) as u32;
        spend(work, u64::from(max_x - min_x) * u64::from(max_y - min_y))?;
        for y in min_y..max_y {
            for x in min_x..max_x {
                let sample = [f64::from(x) + 0.5, f64::from(y) + 0.5];
                let u = edge(b, c, sample) / area;
                let v = edge(c, a, sample) / area;
                let w = 1.0 - u - v;
                if u >= -1e-8 && v >= -1e-8 && w >= -1e-8 {
                    let coverage = (u * f64::from(alpha[0]) + v * f64::from(alpha[1]) + w * f64::from(alpha[2])).clamp(0.0, 1.0) as f32;
                    let index = y as usize * view.width as usize + x as usize;
                    let current = self.coverage.get_mut(index).ok_or_else(|| error("Raster index overflow"))?;
                    if coverage > *current {
                        if *current == 0.0 {
                            self.covered.push(index);
                        }
                        *current = coverage;
                    }
                }
            }
        }
        Ok(())
    }
    fn apply_coverage(&mut self, color: [f32; 4], erase: bool) {
        let covered = std::mem::take(&mut self.covered);
        for &index in &covered {
            let coverage = self.coverage.get(index).copied().unwrap_or(0.0);
            self.touch(index);
            if let Some(pixel) = self.pixels.get_mut(index) {
                if erase {
                    for value in pixel {
                        *value *= 1.0 - coverage;
                    }
                } else {
                    let alpha = coverage * color[3];
                    for axis in 0..3 {
                        pixel[axis] = color[axis] * alpha + pixel[axis] * (1.0 - alpha);
                    }
                    pixel[3] = alpha + pixel[3] * (1.0 - alpha);
                }
            }
            if let Some(value) = self.coverage.get_mut(index) {
                *value = 0.0;
            }
        }
        self.covered = covered;
        self.covered.clear();
    }
    fn composite(&mut self, output: &mut [[f32; 4]]) {
        for index in self.dirty.drain(..) {
            if let (Some(source), Some(target)) = (self.pixels.get_mut(index), output.get_mut(index)) {
                for axis in 0..3 {
                    target[axis] = (source[axis] + target[axis] * (1.0 - source[3])).clamp(0.0, 1.0);
                }
                target[3] = (source[3] + target[3] * (1.0 - source[3])).clamp(0.0, 1.0);
                *source = [0.0; 4];
            }
            if let Some(marked) = self.marked.get_mut(index) {
                *marked = false;
            }
        }
    }
    fn image(&mut self, image: &image::RgbaImage, start: [f64; 2], end: [f64; 2], view: &Viewport, work: &mut u64) -> Result<()> {
        if start.iter().chain(end.iter()).any(|n| !n.is_finite()) {
            return Err(error("Invalid projected media bounds"));
        }
        let size = [end[0] - start[0], end[1] - start[1]];
        if size[0] <= 0.0 || size[1] <= 0.0 {
            return Ok(());
        }
        let x0 = start[0].floor().clamp(0.0, f64::from(view.width)) as u32;
        let x1 = end[0].ceil().clamp(0.0, f64::from(view.width)) as u32;
        let y0 = start[1].floor().clamp(0.0, f64::from(view.height)) as u32;
        let y1 = end[1].ceil().clamp(0.0, f64::from(view.height)) as u32;
        spend(work, u64::from(x1 - x0) * u64::from(y1 - y0))?;
        for y in y0..y1 {
            for x in x0..x1 {
                let u = (f64::from(x) + 0.5 - start[0]) / size[0];
                let v = (f64::from(y) + 0.5 - start[1]) / size[1];
                if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) {
                    continue;
                }
                let sx = ((u * f64::from(image.width())).floor() as u32).min(image.width().saturating_sub(1));
                let sy = ((v * f64::from(image.height())).floor() as u32).min(image.height().saturating_sub(1));
                let source = image.get_pixel_checked(sx, sy).ok_or_else(|| error("Invalid media pixel"))?.0;
                let alpha = f32::from(source[3]) / 255.0;
                let index = y as usize * view.width as usize + x as usize;
                self.touch(index);
                let target = self.pixels.get_mut(index).ok_or_else(|| error("Invalid output pixel"))?;
                for axis in 0..3 {
                    target[axis] = f32::from(source[axis]) / 255.0 * alpha + target[axis] * (1.0 - alpha);
                }
                target[3] = alpha + target[3] * (1.0 - alpha);
            }
        }
        Ok(())
    }
}

fn spend(work: &mut u64, samples: u64) -> Result<()> {
    *work = work.saturating_add(samples);
    if *work > MAX_STROKE_RASTER_SAMPLES {
        return Err(error("World export work budget reached; choose a closer view or smaller output"));
    }
    Ok(())
}

pub fn decode_frame(descriptor: &MediaFrame, bytes: &[u8]) -> Result<image::RgbaImage> {
    descriptor.validate().map_err(other)?;
    if bytes.len() as u64 != descriptor.byte_len || format!("{:x}", Sha256::digest(bytes)) != descriptor.hash.as_str() {
        return Err(error("Media frame checksum or length does not match its accepted reference"));
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(descriptor.width);
    limits.max_image_height = Some(descriptor.height);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(other)?;
    if decoder.dimensions() != (descriptor.width, descriptor.height) || decoder.color_type() != image::ColorType::Rgba8 {
        return Err(error("Media frame is not the canonical RGBA8 PNG described by its reference"));
    }
    Ok(image::DynamicImage::from_decoder(decoder).map_err(other)?.into_rgba8())
}

pub fn render_view(world: &WorldState, view: &Viewport, elapsed_ms: u64, resolve: &mut dyn FnMut(&MediaFrame) -> Result<Vec<u8>>) -> Result<WorldImage> {
    WorldRenderer::new(world)?.render(view, elapsed_ms, resolve)
}
pub fn rgba8(image: &WorldImage) -> Result<Vec<u8>> {
    if image.width == 0
        || image.height == 0
        || image.width > MAX_RENDER_SIDE
        || image.height > MAX_RENDER_SIDE
        || u64::from(image.width) * u64::from(image.height) != image.pixels.len() as u64
    {
        return Err(error("Invalid world output image"));
    }
    if image.pixels.iter().flatten().any(|n| !n.is_finite() || !(0.0..=1.0).contains(n)) {
        return Err(error("Invalid world output samples"));
    }
    Ok(image.pixels.iter().flatten().map(|n| (n * 255.0).round() as u8).collect())
}
pub fn encode_png(image: &WorldImage) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes).write_image(&rgba8(image)?, image.width, image.height, image::ExtendedColorType::Rgba8).map_err(other)?;
    Ok(bytes)
}

pub fn media_asset_hash(bytes: &[u8]) -> Result<AssetHash> {
    AssetHash::new(&format!("{:x}", Sha256::digest(bytes))).map_err(other)
}
