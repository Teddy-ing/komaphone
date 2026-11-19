use super::Result;
use photocraft_engine::scene::{
    self, Viewport, coordinates,
    shared::{DeviceId, SharedMedia, SharedStroke, media::AssetHash},
};
use std::ops::Range;

#[derive(Clone, Copy)]
pub(super) enum BrushOp {
    Normal,
    Erase,
    Behind,
}

#[derive(Clone, Copy)]
pub(super) struct TileCopy {
    pub staging_offset: u64,
    pub x0: u32,
    pub y0: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone)]
pub(super) enum Kind {
    Color { erase: bool },
    Image(AssetHash),
    Brush { op: BrushOp, copies: Vec<TileCopy> },
}

pub(super) struct Batch {
    pub device: DeviceId,
    pub kind: Kind,
    pub indices: Range<u32>,
}

#[derive(Default)]
pub(super) struct Geometry {
    pub vertices: Vec<u8>,
    pub indices: Vec<u8>,
    pub batches: Vec<Batch>,
}

impl Geometry {
    pub fn vertex_count(&self) -> usize {
        self.vertices.len() / 32
    }
    fn reserve(&self, vertices: usize, indices: usize) -> Result<()> {
        if self.vertex_count().saturating_add(vertices) > scene::round::MAX_MESH_VERTICES || self.indices.len() / 4 + indices > 8_388_608 {
            return Err("Visible GPU geometry exceeds its bounded buffer budget; zoom closer".into());
        }
        Ok(())
    }
    fn vertex(&mut self, position: [f32; 2], uv: [f32; 2], color: [f32; 4], view: &Viewport) {
        let values =
            [position[0] * 2.0 / view.width as f32 - 1.0, 1.0 - position[1] * 2.0 / view.height as f32, uv[0], uv[1], color[0], color[1], color[2], color[3]];
        for value in values {
            self.vertices.extend_from_slice(&value.to_le_bytes());
        }
    }
    fn vertex_phys(&mut self, position: [f32; 2], uv: [f32; 2], color: [f32; 4], phys: [u32; 2]) {
        let values = [position[0] * 2.0 / phys[0] as f32 - 1.0, 1.0 - position[1] * 2.0 / phys[1] as f32, uv[0], uv[1], color[0], color[1], color[2], color[3]];
        for value in values {
            self.vertices.extend_from_slice(&value.to_le_bytes());
        }
    }
    fn index(&mut self, index: u32) {
        self.indices.extend_from_slice(&index.to_le_bytes());
    }
    pub fn stroke(&mut self, shared: &SharedStroke, view: &Viewport, density: f32) -> Result<()> {
        let Some(projected) = scene::render::project(&shared.stroke, view).map_err(|e| e.to_string())? else {
            return Ok(());
        };
        let first = (self.indices.len() / 4) as u32;
        let mesh = scene::round::mesh(&projected.paths, projected.width, [view.width, view.height], density, scene::round::MAX_MESH_VERTICES)
            .map_err(|e| e.to_string())?;
        self.reserve(mesh.vertices.len(), mesh.indices.len())?;
        let base = self.vertex_count() as u32;
        let color = if projected.erase { [0.0, 0.0, 0.0, 1.0] } else { projected.color.rgba };
        for vertex in mesh.vertices {
            self.vertex(vertex.position, [0.0; 2], color.map(|value| value * vertex.coverage), view);
        }
        for index in mesh.indices {
            self.index(base + index);
        }
        for polygon in projected.fills {
            if polygon.len() < 3 {
                continue;
            }
            self.reserve(polygon.len(), (polygon.len() - 2) * 3)?;
            let base = self.vertex_count() as u32;
            let count = polygon.len();
            for position in polygon {
                self.vertex(position, [0.0; 2], color, view);
            }
            for index in 1..count - 1 {
                self.index(base);
                self.index(base + index as u32);
                self.index(base + index as u32 + 1);
            }
        }
        let end = (self.indices.len() / 4) as u32;
        if first != end {
            self.batches.push(Batch { device: shared.device, kind: Kind::Color { erase: projected.erase }, indices: first..end });
        }
        Ok(())
    }
    pub fn brush_tiles(&mut self, device: DeviceId, op: BrushOp, copies: Vec<TileCopy>, phys: [u32; 2]) -> Result<()> {
        if copies.is_empty() {
            return Ok(());
        }
        self.reserve(copies.len().saturating_mul(4), copies.len().saturating_mul(6))?;
        let first = (self.indices.len() / 4) as u32;
        let pw = phys[0] as f32;
        let ph = phys[1] as f32;
        for copy in &copies {
            let base = self.vertex_count() as u32;
            let x0 = copy.x0 as f32;
            let y0 = copy.y0 as f32;
            let x1 = (copy.x0 + copy.width) as f32;
            let y1 = (copy.y0 + copy.height) as f32;
            for point in [[x0, y0], [x1, y0], [x0, y1], [x1, y1]] {
                self.vertex_phys(point, [point[0] / pw, point[1] / ph], [1.0; 4], phys);
            }
            for index in [0, 1, 2, 2, 1, 3] {
                self.index(base + index);
            }
        }
        let end = (self.indices.len() / 4) as u32;
        self.batches.push(Batch { device, kind: Kind::Brush { op, copies }, indices: first..end });
        Ok(())
    }
    pub fn media(&mut self, shared: &SharedMedia, view: &Viewport, hash: AssetHash) -> Result<()> {
        let media = &shared.media;
        let origin =
            coordinates::point_to_screen(view, coordinates::point(&media.anchor, media.offset).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let end = coordinates::point_to_screen(
            view,
            coordinates::point(&media.anchor, [media.offset[0] + media.width, media.offset[1] + media.height]).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let size = [end[0] - origin[0], end[1] - origin[1]];
        if size.iter().any(|n| !n.is_finite() || *n <= 0.0) {
            return Err("Invalid media screen projection".into());
        }
        let x0 = origin[0].clamp(0.0, f64::from(view.width));
        let x1 = end[0].clamp(0.0, f64::from(view.width));
        let y0 = origin[1].clamp(0.0, f64::from(view.height));
        let y1 = end[1].clamp(0.0, f64::from(view.height));
        if x0 >= x1 || y0 >= y1 {
            return Ok(());
        }
        self.reserve(4, 6)?;
        let base = self.vertex_count() as u32;
        let first = (self.indices.len() / 4) as u32;
        for point in [[x0, y0], [x1, y0], [x0, y1], [x1, y1]] {
            self.vertex(point.map(|n| n as f32), [((point[0] - origin[0]) / size[0]) as f32, ((point[1] - origin[1]) / size[1]) as f32], [1.0; 4], view);
        }
        for index in [0, 1, 2, 2, 1, 3] {
            self.index(base + index);
        }
        self.batches.push(Batch { device: shared.device, kind: Kind::Image(hash), indices: first..first + 6 });
        Ok(())
    }
}
