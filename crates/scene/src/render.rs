//! Viewport-bounded retained-stroke projection. No scene-wide scan or UI types.
pub use crate::coordinates::scale_ratio;
use crate::coordinates::{cells_between, center, local_unit, point, point_to_screen, validate_view};
use crate::{Error, MAX_SPATIAL_CELLS, Point, ProjectedStroke, RegionKey, Result, Stroke, Viewport, Wide};

pub fn spatial_cells(stroke: &Stroke, max_cells: usize) -> Result<Vec<RegionKey>> {
    stroke.validate()?;
    if max_cells == 0 || max_cells > MAX_SPATIAL_CELLS {
        return Err(Error::Invalid("spatial cell limit"));
    }
    let mut min = [i128::MAX; 2];
    let mut max = [i128::MIN; 2];
    for p in &stroke.points {
        let p = point(&stroke.anchor, *p)?;
        min[0] = min[0].min(p.x.0);
        min[1] = min[1].min(p.y.0);
        max[0] = max[0].max(p.x.0);
        max[1] = max[1].max(p.y.0);
    }
    // The spatial envelope must round outward, unlike authored point quantization.
    // Otherwise a very thin stroke on a seam disappears when approached from the
    // adjacent cell at a much deeper view scale.
    let radius = Double::integer(local_unit(stroke.anchor.depth)?).mul(Double::scalar(stroke.width * 0.5));
    let radius = radius.hi as i128 + radius.lo.ceil() as i128 + 1;
    let a = Point { x: Wide(min[0] - radius), y: Wide(min[1] - radius) };
    let b = Point { x: Wide(max[0] + radius), y: Wide(max[1] + radius) };
    cells_between(a, b, stroke.anchor.depth, max_cells)
}

/// Bounded display derivative. The original remains authoritative and unchanged.
/// Endpoints and original ordering survive; callers only use this below pixel scale.
pub fn decimate(stroke: &Stroke, max_points: usize) -> Result<Stroke> {
    stroke.validate()?;
    if !(2..=crate::MAX_POINTS).contains(&max_points) {
        return Err(Error::Invalid("summary points"));
    }
    if stroke.points.len() <= max_points {
        return Ok(stroke.clone());
    }
    let mut result = stroke.clone();
    result.points.clear();
    let end = stroke.points.len() - 1;
    for i in 0..max_points {
        let index = i.checked_mul(end).ok_or(Error::Limit("summary points"))? / (max_points - 1);
        result.points.push(*stroke.points.get(index).ok_or(Error::Invalid("summary point index"))?);
    }
    Ok(result)
}

fn clip_segment(a: [f64; 2], b: [f64; 2], bounds: [f64; 4]) -> Option<([f64; 2], [f64; 2])> {
    let d = [b[0] - a[0], b[1] - a[1]];
    let mut low: f64 = 0.0;
    let mut high: f64 = 1.0;
    for (p, q) in [(-d[0], a[0] - bounds[0]), (d[0], bounds[2] - a[0]), (-d[1], a[1] - bounds[1]), (d[1], bounds[3] - a[1])] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                low = low.max(t);
            } else {
                high = high.min(t);
            }
            if low > high {
                return None;
            }
        }
    }
    // Bound rounding on very long segments before any f32 conversion.
    let at = |t: f64| [(a[0] + d[0] * t).clamp(bounds[0], bounds[2]), (a[1] + d[1] * t).clamp(bounds[1], bounds[3])];
    Some((at(low), at(high)))
}

fn screen32(point: [f64; 2]) -> [f32; 2] {
    [point[0] as f32, point[1] as f32]
}

/// Project one logical stroke. Tiny derivatives preserve identity and operation order.
/// Huge ancestor capsules become clipped fills rather than unstable enormous GPU paths.
pub fn project(stroke: &Stroke, view: &Viewport) -> Result<Option<ProjectedStroke>> {
    stroke.validate()?;
    validate_view(view)?;
    if stroke.canvas != view.camera.canvas {
        return Err(Error::Canvas);
    }
    let ratio = scale_ratio(stroke.anchor.depth, &view.camera)?;
    let width = stroke.width * ratio;
    let mut output = ProjectedStroke {
        id: stroke.id,
        revision: stroke.revision,
        paths: Vec::new(),
        fills: Vec::new(),
        width: width.min(32768.0) as f32,
        color: stroke.color,
        erase: stroke.erase,
    };
    if width > 32768.0 {
        let points: Vec<_> = stroke.points.iter().map(|p| point(&stroke.anchor, *p)).collect::<Result<_>>()?;
        let radius = Double::integer(local_unit(stroke.anchor.depth)?).mul(Double::scalar(stroke.width * 0.5));
        let mut fill_points = 0usize;
        if let Some(&first) = points.first() {
            if points.len() == 1 {
                if let Some(poly) = capsule_polygon(first, first, radius, view)? {
                    output.fills.push(poly);
                }
            } else {
                for pair in points.windows(2) {
                    if let [a, b] = pair
                        && let Some(poly) = capsule_polygon(*a, *b, radius, view)?
                    {
                        // A single capsule covers the entire view: no remaining piece
                        // can add coverage to this same logical stroke.
                        if full_view(&poly, view) {
                            output.fills = vec![poly];
                            break;
                        }
                        fill_points = fill_points.saturating_add(poly.len());
                        output.fills.push(poly);
                        if fill_points > crate::MAX_VIEW_POINTS {
                            return Err(Error::Limit("projected curve points"));
                        }
                    }
                }
            }
        }
    } else {
        let radius = width * 0.5 + 2.0;
        let bounds = [-radius, -radius, f64::from(view.width) + radius, f64::from(view.height) + radius];
        let worlds: Vec<_> = stroke.points.iter().map(|p| point(&stroke.anchor, *p)).collect::<Result<_>>()?;
        let points: Vec<_> = worlds.iter().map(|p| point_to_screen(view, *p)).collect::<Result<_>>()?;
        if let Some(&first) = points.first() {
            if points.len() == 1 {
                if first[0] >= bounds[0] && first[0] <= bounds[2] && first[1] >= bounds[1] && first[1] <= bounds[3] {
                    output.paths.push(vec![screen32(first)]);
                }
            } else {
                let mut path: Vec<[f32; 2]> = Vec::new();
                for (index, pair) in points.windows(2).enumerate() {
                    let [a, b] = pair else {
                        continue;
                    };
                    let clipped = if a.iter().chain(b).any(|v| v.abs() > 10_000_000.0) {
                        let first = worlds.get(index).ok_or(Error::Invalid("projection point"))?;
                        let second = worlds.get(index + 1).ok_or(Error::Invalid("projection point"))?;
                        clip_segment_precise(*first, *second, view, bounds)?
                    } else {
                        clip_segment(*a, *b, bounds)
                    };
                    if let Some((from, to)) = clipped {
                        let from = screen32(from);
                        let to = screen32(to);
                        if path.last().is_some_and(|last| *last != from) {
                            output.paths.push(std::mem::take(&mut path));
                        }
                        if path.is_empty() {
                            path.push(from);
                        }
                        if path.last().is_none_or(|last| *last != to) {
                            path.push(to);
                        }
                    } else if !path.is_empty() {
                        output.paths.push(std::mem::take(&mut path));
                    }
                }
                if !path.is_empty() {
                    output.paths.push(path);
                }
            }
        }
    }
    Ok((!output.paths.is_empty() || !output.fills.is_empty()).then_some(output))
}

fn full_view(poly: &[[f32; 2]], view: &Viewport) -> bool {
    poly.len() == 4
        && poly.contains(&[0.0, 0.0])
        && poly.contains(&[view.width as f32, 0.0])
        && poly.contains(&[view.width as f32, view.height as f32])
        && poly.contains(&[0.0, view.height as f32])
}

/// Approximately 106 mantissa bits, used only for extreme ancestor edges. Integer
/// subtraction has already removed the distant canvas origin before conversion.
#[derive(Clone, Copy)]
struct Double {
    hi: f64,
    lo: f64,
}
impl Double {
    fn scalar(hi: f64) -> Self {
        Self { hi, lo: 0.0 }
    }
    fn integer(n: i128) -> Self {
        let hi = n as f64;
        Self { hi, lo: (n - hi as i128) as f64 }
    }
    fn normalized(hi: f64, lo: f64) -> Self {
        let sum = hi + lo;
        Self { hi: sum, lo: lo - (sum - hi) }
    }
    fn add(self, b: Self) -> Self {
        let sum = self.hi + b.hi;
        let v = sum - self.hi;
        let error = (self.hi - (sum - v)) + (b.hi - v) + self.lo + b.lo;
        Self::normalized(sum, error)
    }
    fn neg(self) -> Self {
        Self { hi: -self.hi, lo: -self.lo }
    }
    fn sub(self, b: Self) -> Self {
        self.add(b.neg())
    }
    fn mul(self, b: Self) -> Self {
        let product = self.hi * b.hi;
        Self::normalized(product, self.hi.mul_add(b.hi, -product) + self.hi * b.lo + self.lo * b.hi + self.lo * b.lo)
    }
    fn div(self, b: Self) -> Self {
        let q = self.hi / b.hi;
        let remainder = self.sub(b.mul(Self::scalar(q)));
        Self::normalized(q, (remainder.hi + remainder.lo) / b.hi)
    }
    fn sqrt(self) -> Self {
        let root = self.hi.max(0.0).sqrt();
        if root == 0.0 {
            return Self::scalar(0.0);
        }
        Self::scalar(root).add(self.sub(Self::scalar(root).mul(Self::scalar(root))).div(Self::scalar(2.0 * root)))
    }
    fn value(self) -> f64 {
        self.hi + self.lo
    }
}

fn clip_segment_precise(a: Point, b: Point, view: &Viewport, bounds: [f64; 4]) -> Result<Option<([f64; 2], [f64; 2])>> {
    let c = center(&view.camera)?;
    let factor = Double::scalar(view.camera.zoom / local_unit(view.camera.region.depth)? as f64);
    let screen = |p: Point| {
        [
            Double::integer(p.x.0 - c.x.0).mul(factor).add(Double::scalar(f64::from(view.width) * 0.5)),
            Double::integer(p.y.0 - c.y.0).mul(factor).add(Double::scalar(f64::from(view.height) * 0.5)),
        ]
    };
    let a = screen(a);
    let b = screen(b);
    let d = [b[0].sub(a[0]), b[1].sub(a[1])];
    let mut low = Double::scalar(0.0);
    let mut high = Double::scalar(1.0);
    for (p, q) in [
        (d[0].neg(), a[0].sub(Double::scalar(bounds[0]))),
        (d[0], Double::scalar(bounds[2]).sub(a[0])),
        (d[1].neg(), a[1].sub(Double::scalar(bounds[1]))),
        (d[1], Double::scalar(bounds[3]).sub(a[1])),
    ] {
        if p.value() == 0.0 {
            if q.value() < 0.0 {
                return Ok(None);
            }
        } else {
            let t = q.div(p);
            if p.value() < 0.0 {
                if t.sub(low).value() > 0.0 {
                    low = t;
                }
            } else if t.sub(high).value() < 0.0 {
                high = t;
            }
            if low.sub(high).value() > 0.0 {
                return Ok(None);
            }
        }
    }
    let at = |t: Double| [a[0].add(d[0].mul(t)).value().clamp(bounds[0], bounds[2]), a[1].add(d[1].mul(t)).value().clamp(bounds[1], bounds[3])];
    Ok(Some((at(low), at(high))))
}

struct Capsule {
    a: [Double; 2],
    b: [Double; 2],
    radius: Double,
    ticks_per_pixel: f64,
    middle: [f64; 2],
}
impl Capsule {
    fn signed_distance(&self, p: [f64; 2]) -> (f64, [f64; 2]) {
        let q = [Double::scalar((p[0] - self.middle[0]) * self.ticks_per_pixel), Double::scalar((p[1] - self.middle[1]) * self.ticks_per_pixel)];
        let v = [self.b[0].sub(self.a[0]), self.b[1].sub(self.a[1])];
        let w = [q[0].sub(self.a[0]), q[1].sub(self.a[1])];
        let length = v[0].mul(v[0]).add(v[1].mul(v[1]));
        let t = if length.value() > 0.0 {
            let t = w[0].mul(v[0]).add(w[1].mul(v[1])).div(length);
            if t.value() < 0.0 {
                Double::scalar(0.0)
            } else if t.value() > 1.0 {
                Double::scalar(1.0)
            } else {
                t
            }
        } else {
            Double::scalar(0.0)
        };
        let residual = [w[0].sub(v[0].mul(t)), w[1].sub(v[1].mul(t))];
        let distance = residual[0].mul(residual[0]).add(residual[1].mul(residual[1])).sqrt();
        let magnitude = distance.value();
        let normal = if magnitude > 0.0 { [residual[0].value() / magnitude, residual[1].value() / magnitude] } else { [1.0, 0.0] };
        (distance.sub(self.radius).value() / self.ticks_per_pixel, normal)
    }
}

fn clip_halfplane(poly: &[[f64; 2]], normal: [f64; 2], offset: f64) -> Vec<[f64; 2]> {
    let Some(&last) = poly.last() else {
        return Vec::new();
    };
    let mut result = Vec::with_capacity(poly.len() + 1);
    let mut a = last;
    let mut da = a[0] * normal[0] + a[1] * normal[1] - offset;
    for &b in poly {
        let db = b[0] * normal[0] + b[1] * normal[1] - offset;
        if (da <= 0.0) != (db <= 0.0) {
            let t = da / (da - db);
            result.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
        }
        if db <= 0.0 {
            result.push(b);
        }
        a = b;
        da = db;
    }
    result
}

fn capsule_polygon(a: Point, b: Point, radius: Double, view: &Viewport) -> Result<Option<Vec<[f32; 2]>>> {
    let c = center(&view.camera)?;
    let relative = |p: Point| [Double::integer(p.x.0 - c.x.0), Double::integer(p.y.0 - c.y.0)];
    let capsule = Capsule {
        a: relative(a),
        b: relative(b),
        radius,
        ticks_per_pixel: local_unit(view.camera.region.depth)? as f64 / view.camera.zoom,
        middle: [f64::from(view.width) * 0.5, f64::from(view.height) * 0.5],
    };
    let w = f64::from(view.width);
    let h = f64::from(view.height);
    let mut poly = vec![[0.0, 0.0], [w, 0.0], [w, h], [0.0, h]];
    // Every tangent contains the true convex capsule. Refine only the visible arc.
    for _ in 0..128 {
        let worst = poly
            .iter()
            .map(|p| {
                let (distance, normal) = capsule.signed_distance(*p);
                (*p, distance, normal)
            })
            .filter(|(_, distance, _)| *distance > 0.05)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        let Some((p, distance, normal)) = worst else {
            return Ok((poly.len() >= 3).then(|| poly.into_iter().map(screen32).collect()));
        };
        poly = clip_halfplane(&poly, normal, p[0] * normal[0] + p[1] * normal[1] - distance);
        if poly.len() < 3 {
            return Ok(None);
        }
        if poly.len() > 256 {
            return Err(Error::Limit("ancestor curve geometry"));
        }
    }
    Err(Error::Limit("ancestor curve refinement"))
}
