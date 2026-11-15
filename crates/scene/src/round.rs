//! Screen-space round brushes. Only display geometry changes; saved samples do not.
//!
//! Segment ribbons meet round outside sectors, with semicircular end caps. Feather
//! only the exposed edges: independently antialiased filled wedges leave seams.
//! K5 colors are opaque. Overlapping ribbons are not a transparency compositor.
use crate::{Error, MAX_VIEW_POINTS, MAX_VIEW_SIDE, Result};
use std::f64::consts::{PI, TAU};

mod simplify;

/// Maximum circular chord error, measured in physical display pixels.
pub const ARC_ERROR_PIXELS: f64 = 0.125;
/// Display-only centerline error. Combined with arc approximation, the geometric
/// error stays within a quarter of a physical pixel, independent of zoom/DPI.
pub const PATH_ERROR_PIXELS: f64 = 0.125;

/// Generated vertices are not authored samples. A typical round join and ribbon
/// use thirteen vertices per sample; this budget accommodates a full 131,072-point
/// source view without reducing its previous practical drawing capacity. Highly
/// magnified pathological turns still fail explicitly instead of coarsening arcs.
pub const MAX_MESH_VERTICES: usize = 2_097_152;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrokeVertex {
    pub position: [f32; 2],
    pub coverage: f32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StrokeMesh {
    pub vertices: Vec<StrokeVertex>,
    pub indices: Vec<u32>,
}

type Point = [f64; 2];

struct Builder {
    mesh: StrokeMesh,
    budget: usize,
    inner: f64,
    outer: f64,
    coverage: f32,
    step: f64,
    bounds: [f64; 2],
}

impl Builder {
    fn reserve(&self, vertices: usize) -> Result<u32> {
        if self.mesh.vertices.len().saturating_add(vertices) > self.budget {
            return Err(Error::Limit("visible round-brush geometry; zoom in or move closer"));
        }
        u32::try_from(self.mesh.vertices.len()).map_err(|_| Error::Limit("round-brush indices"))
    }

    fn vertex(&mut self, point: Point, coverage: f32) {
        self.mesh.vertices.push(StrokeVertex { position: [point[0] as f32, point[1] as f32], coverage });
    }

    fn visible(&self, points: &[Point]) -> bool {
        !points.iter().all(|p| p[0] < 0.0)
            && !points.iter().all(|p| p[1] < 0.0)
            && !points.iter().all(|p| p[0] > self.bounds[0])
            && !points.iter().all(|p| p[1] > self.bounds[1])
    }

    fn ribbon(&mut self, a: Point, b: Point, direction: Point) -> Result<()> {
        let normal = [-direction[1], direction[0]];
        let offset = |p: Point, distance: f64| [p[0] + normal[0] * distance, p[1] + normal[1] * distance];
        let points = [
            offset(a, -self.outer),
            offset(a, -self.inner),
            offset(a, self.inner),
            offset(a, self.outer),
            offset(b, -self.outer),
            offset(b, -self.inner),
            offset(b, self.inner),
            offset(b, self.outer),
        ];
        if !self.visible(&points) {
            return Ok(());
        }
        let base = self.reserve(8)?;
        for (index, point) in points.into_iter().enumerate() {
            self.vertex(point, if index % 4 == 0 || index % 4 == 3 { 0.0 } else { self.coverage });
        }
        for strip in 0..3 {
            self.mesh.indices.extend_from_slice(&[base + strip, base + strip + 1, base + strip + 4, base + strip + 1, base + strip + 5, base + strip + 4]);
        }
        Ok(())
    }

    fn arc(&mut self, center: Point, start: f64, sweep: f64) -> Result<()> {
        if sweep.abs() < 1e-12 {
            return Ok(());
        }
        // Width and display-density validation bound this to 4,551 subdivisions.
        // Cull whole offscreen disks before trigonometry and individual wedges
        // before allocation, so zooming an arc does not allocate its invisible rim.
        if !self.visible(&[[center[0] - self.outer, center[1] - self.outer], [center[0] + self.outer, center[1] + self.outer]]) {
            return Ok(());
        }
        let segments = (sweep.abs() / self.step).ceil().max(1.0) as usize;
        let direction = |angle: f64| [angle.cos(), angle.sin()];
        let at = |normal: Point, radius: f64| [center[0] + normal[0] * radius, center[1] + normal[1] * radius];
        let mut previous = direction(start);
        // Reuse the previous wedge's rim when visible. No radial edge is feathered.
        let mut previous_indices = None;
        let mut center_index = None;
        for index in 1..=segments {
            let next = direction(start + sweep * index as f64 / segments as f64);
            let outer_a = at(previous, self.outer);
            let outer_b = at(next, self.outer);
            if self.visible(&[center, outer_a, outer_b]) {
                let needed = 2 + usize::from(previous_indices.is_none()) * 2 + usize::from(center_index.is_none());
                self.reserve(needed)?;
                let center_id = if let Some(id) = center_index {
                    id
                } else {
                    let id = self.mesh.vertices.len() as u32;
                    self.vertex(center, self.coverage);
                    center_index = Some(id);
                    id
                };
                let (inner_a_id, outer_a_id) = if let Some(ids) = previous_indices {
                    ids
                } else {
                    let id = self.mesh.vertices.len() as u32;
                    self.vertex(at(previous, self.inner), self.coverage);
                    self.vertex(outer_a, 0.0);
                    (id, id + 1)
                };
                let inner_b_id = self.mesh.vertices.len() as u32;
                self.vertex(at(next, self.inner), self.coverage);
                self.vertex(outer_b, 0.0);
                self.mesh.indices.extend_from_slice(&[
                    center_id,
                    inner_a_id,
                    inner_b_id,
                    inner_a_id,
                    outer_a_id,
                    inner_b_id,
                    outer_a_id,
                    inner_b_id + 1,
                    inner_b_id,
                ]);
                previous_indices = Some((inner_b_id, inner_b_id + 1));
            } else {
                previous_indices = None;
            }
            previous = next;
        }
        Ok(())
    }
}

fn direction(a: Point, b: Point) -> Point {
    let delta = [b[0] - a[0], b[1] - a[1]];
    let length = delta[0].hypot(delta[1]);
    [delta[0] / length, delta[1] / length]
}

/// Tessellate already projected, clipped centerlines, using one physical pixel of
/// edge coverage and a screen-error-bounded circular outline. Budget exhaustion is
/// explicit; never make the brush angular or drop points to fit a vertex budget.
pub fn mesh(paths: &[Vec<[f32; 2]>], width: f32, viewport: [u32; 2], pixels_per_point: f32, vertex_budget: usize) -> Result<StrokeMesh> {
    let source_points = paths.iter().map(Vec::len).fold(0usize, usize::saturating_add);
    if source_points > MAX_VIEW_POINTS || paths.len() > MAX_VIEW_POINTS || vertex_budget > MAX_MESH_VERTICES {
        return Err(Error::Limit("round-brush input"));
    }
    if !width.is_finite()
        || !(0.0..=32768.0).contains(&width)
        || !pixels_per_point.is_finite()
        || !(0.125..=16.0).contains(&pixels_per_point)
        || viewport.iter().any(|v| *v == 0 || *v > MAX_VIEW_SIDE)
        || paths.iter().flatten().flatten().any(|v| !v.is_finite() || v.abs() > 65536.0)
    {
        return Err(Error::Invalid("projected round brush"));
    }
    if width == 0.0 {
        return Ok(StrokeMesh::default());
    }
    let feather = 1.0 / f64::from(pixels_per_point);
    let width = f64::from(width);
    let (inner, outer, coverage) =
        if width < feather { (0.0, feather, (width / feather) as f32) } else { ((width - feather) * 0.5, (width + feather) * 0.5, 1.0) };
    let error = ARC_ERROR_PIXELS / f64::from(pixels_per_point);
    let step = (2.0 * (1.0 - error / outer).clamp(-1.0, 1.0).acos()).min(PI * 0.25);
    let mut builder =
        Builder { mesh: StrokeMesh::default(), budget: vertex_budget, inner, outer, coverage, step, bounds: [f64::from(viewport[0]), f64::from(viewport[1])] };
    for path in paths {
        let mut points: Vec<Point> = Vec::with_capacity(path.len());
        for p in path {
            let p = [f64::from(p[0]), f64::from(p[1])];
            if points.last().is_none_or(|last| *last != p) {
                points.push(p);
            }
        }
        let mut points = simplify::points(points, PATH_ERROR_PIXELS / f64::from(pixels_per_point))?;
        // A subpixel closed loop can collapse to its coincident first/last
        // samples. Treat that display derivative as a dot, never a zero-length
        // ribbon with an undefined direction.
        points.dedup();
        let Some(&first) = points.first() else { continue };
        if points.len() == 1 {
            builder.arc(first, 0.0, TAU)?;
            continue;
        }
        let mut previous_direction = None;
        for pair in points.windows(2) {
            let [a, b] = pair else { continue };
            let d = direction(*a, *b);
            builder.ribbon(*a, *b, d)?;
            if let Some(previous) = previous_direction {
                let previous: Point = previous;
                let cross = previous[0] * d[1] - previous[1] * d[0];
                let dot = previous[0] * d[0] + previous[1] * d[1];
                let turn = cross.atan2(dot);
                let side = if turn >= 0.0 { -1.0 } else { 1.0 };
                let start = (previous[0] * side).atan2(-previous[1] * side);
                builder.arc(*a, start, turn)?;
            } else {
                builder.arc(*a, d[1].atan2(d[0]) + PI * 0.5, PI)?;
            }
            previous_direction = Some(d);
        }
        if let (Some(&last), Some(d)) = (points.last(), previous_direction) {
            builder.arc(last, d[1].atan2(d[0]) - PI * 0.5, PI)?;
        }
    }
    Ok(builder.mesh)
}
