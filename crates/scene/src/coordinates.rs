//! Exact scene positions and bounded camera-relative transformations.
use crate::{
    CHILD_SCALE, Camera, CanvasId, Error, LOCAL_SUBPIXELS, MAX_DEPTH, MAX_SPATIAL_CELLS, MAX_VIEW_SIDE, Point, REGION_LOCAL_SIZE, ROOT_TILE_LIMIT, RegionKey,
    Result, Viewport, Wide,
};

/// At the largest supported 8192-square viewport this loads at most 17x17 root
/// cells. Lateral navigation is independent; fitting the entire canvas is not
/// promised by this bounded prototype.
pub const MIN_ROOT_ZOOM: f64 = 0.5;
pub const MAX_LOCAL_ZOOM: f64 = 100.0;

pub fn region_span(depth: u8) -> Result<i128> {
    if depth > MAX_DEPTH {
        return Err(Error::Limit("detail depth"));
    }
    CHILD_SCALE
        .checked_pow(u32::from(MAX_DEPTH - depth))
        .and_then(|v| v.checked_mul(REGION_LOCAL_SIZE * LOCAL_SUBPIXELS))
        .ok_or(Error::Limit("coordinate extent"))
}

pub fn local_unit(depth: u8) -> Result<i128> {
    Ok(region_span(depth)? / REGION_LOCAL_SIZE)
}

pub fn extent() -> Result<i128> {
    region_span(0)?.checked_mul(ROOT_TILE_LIMIT).ok_or(Error::Limit("root extent"))
}

pub fn validate_point(value: Point) -> Result<()> {
    let limit = extent()?;
    if value.x.0 < -limit || value.x.0 >= limit || value.y.0 < -limit || value.y.0 >= limit {
        return Err(Error::Limit("canvas coordinate range"));
    }
    Ok(())
}

pub fn region_origin(region: &RegionKey) -> Result<Point> {
    let span = region_span(region.depth)?;
    let point = Point {
        x: Wide(region.x.0.checked_mul(span).ok_or(Error::Limit("region coordinate"))?),
        y: Wide(region.y.0.checked_mul(span).ok_or(Error::Limit("region coordinate"))?),
    };
    validate_point(point)?;
    Ok(point)
}

/// Quantize only bounded local values; multiplication into the hierarchy is exact.
pub fn point(region: &RegionKey, local: [f64; 2]) -> Result<Point> {
    if local.iter().any(|v| !v.is_finite() || v.abs() > crate::MAX_STROKE_LOCAL + 2048.0) {
        return Err(Error::Invalid("local coordinate"));
    }
    let origin = region_origin(region)?;
    let step = local_unit(region.depth)? / LOCAL_SUBPIXELS;
    let axis = |base: i128, value: f64| -> Result<Wide> {
        let quantized = (value * LOCAL_SUBPIXELS as f64).round() as i128;
        Ok(Wide(quantized.checked_mul(step).and_then(|v| base.checked_add(v)).ok_or(Error::Limit("local coordinate"))?))
    };
    let result = Point { x: axis(origin.x.0, local[0])?, y: axis(origin.y.0, local[1])? };
    validate_point(result)?;
    Ok(result)
}

pub fn local_at(region: &RegionKey, value: Point) -> Result<[f64; 2]> {
    validate_point(value)?;
    let origin = region_origin(region)?;
    let unit = local_unit(region.depth)? as f64;
    Ok([(value.x.0 - origin.x.0) as f64 / unit, (value.y.0 - origin.y.0) as f64 / unit])
}

pub fn validate_camera(camera: &Camera) -> Result<()> {
    let span = region_span(camera.region.depth)?;
    region_origin(&camera.region)?;
    if camera.offset.iter().any(|n| n.0 < 0 || n.0 >= span) {
        return Err(Error::Invalid("camera region offset"));
    }
    if !camera.zoom.is_finite()
        || camera.zoom <= 0.0
        || camera.zoom >= MAX_LOCAL_ZOOM
        || (camera.region.depth == 0 && camera.zoom < MIN_ROOT_ZOOM)
        || (camera.region.depth > 0 && camera.zoom < 1.0)
    {
        return Err(Error::Invalid("camera scale"));
    }
    Ok(())
}

pub fn center(camera: &Camera) -> Result<Point> {
    validate_camera(camera)?;
    let origin = region_origin(&camera.region)?;
    let result = Point { x: Wide(origin.x.0 + camera.offset[0].0), y: Wide(origin.y.0 + camera.offset[1].0) };
    validate_point(result)?;
    Ok(result)
}

/// Normalize scale and region while retaining every tick of the camera center.
pub fn target(canvas: CanvasId, value: Point, mut depth: u8, mut zoom: f64) -> Result<Camera> {
    validate_point(value)?;
    region_span(depth)?;
    if !zoom.is_finite() || zoom <= 0.0 {
        return Err(Error::Invalid("camera scale"));
    }
    while zoom >= MAX_LOCAL_ZOOM && depth < MAX_DEPTH {
        depth += 1;
        zoom /= CHILD_SCALE as f64;
    }
    while zoom < 1.0 && depth > 0 {
        depth -= 1;
        zoom *= CHILD_SCALE as f64;
    }
    if depth == 0 {
        zoom = zoom.max(MIN_ROOT_ZOOM);
    }
    if !(MIN_ROOT_ZOOM..MAX_LOCAL_ZOOM).contains(&zoom) {
        return Err(Error::Limit("zoom range"));
    }
    let span = region_span(depth)?;
    let camera = Camera {
        canvas,
        region: RegionKey { depth, x: Wide(value.x.0.div_euclid(span)), y: Wide(value.y.0.div_euclid(span)) },
        offset: [Wide(value.x.0.rem_euclid(span)), Wide(value.y.0.rem_euclid(span))],
        zoom,
    };
    validate_camera(&camera)?;
    Ok(camera)
}

pub fn home(canvas: CanvasId) -> Camera {
    // Literal values are valid at every supported depth; this helper performs no fallible I/O.
    Camera { canvas, region: RegionKey::default(), offset: [Wide(0), Wide(0)], zoom: 1.0 }
}

pub fn validate_view(view: &Viewport) -> Result<()> {
    validate_camera(&view.camera)?;
    if view.width == 0 || view.height == 0 || view.width > MAX_VIEW_SIDE || view.height > MAX_VIEW_SIDE {
        return Err(Error::Invalid("viewport dimensions"));
    }
    Ok(())
}

fn tick_delta(camera: &Camera, screen: [f64; 2]) -> Result<[i128; 2]> {
    validate_camera(camera)?;
    if screen.iter().any(|v| !v.is_finite() || v.abs() > 1_000_000.0) {
        return Err(Error::Invalid("screen displacement"));
    }
    let unit = local_unit(camera.region.depth)? as f64 / camera.zoom;
    let limit = extent()? as f64;
    let x = screen[0] * unit;
    let y = screen[1] * unit;
    if !x.is_finite() || !y.is_finite() || x.abs() > limit || y.abs() > limit {
        return Err(Error::Limit("camera displacement"));
    }
    Ok([x.round() as i128, y.round() as i128])
}

pub fn screen_to_point(view: &Viewport, screen: [f64; 2]) -> Result<Point> {
    validate_view(view)?;
    let c = center(&view.camera)?;
    let delta = tick_delta(&view.camera, [screen[0] - f64::from(view.width) * 0.5, screen[1] - f64::from(view.height) * 0.5])?;
    let value = Point { x: Wide(c.x.0 + delta[0]), y: Wide(c.y.0 + delta[1]) };
    validate_point(value)?;
    Ok(value)
}

pub fn point_to_screen(view: &Viewport, value: Point) -> Result<[f64; 2]> {
    validate_view(view)?;
    validate_point(value)?;
    let c = center(&view.camera)?;
    let scale = view.camera.zoom / local_unit(view.camera.region.depth)? as f64;
    Ok([(value.x.0 - c.x.0) as f64 * scale + f64::from(view.width) * 0.5, (value.y.0 - c.y.0) as f64 * scale + f64::from(view.height) * 0.5])
}

/// Positive drag displacement moves the paper right/down.
pub fn pan(camera: &Camera, drag: [f64; 2]) -> Result<Camera> {
    let c = center(camera)?;
    let delta = tick_delta(camera, drag)?;
    target(camera.canvas, Point { x: Wide(c.x.0 - delta[0]), y: Wide(c.y.0 - delta[1]) }, camera.region.depth, camera.zoom)
}

pub fn zoom_at(view: &Viewport, screen: [f64; 2], factor: f64) -> Result<Camera> {
    validate_view(view)?;
    if !factor.is_finite() || factor <= 0.0 {
        return Err(Error::Invalid("zoom factor"));
    }
    let anchor = screen_to_point(view, screen)?;
    let next = target(view.camera.canvas, center(&view.camera)?, view.camera.region.depth, view.camera.zoom * factor)?;
    let delta = tick_delta(&next, [screen[0] - f64::from(view.width) * 0.5, screen[1] - f64::from(view.height) * 0.5])?;
    target(next.canvas, Point { x: Wide(anchor.x.0 - delta[0]), y: Wide(anchor.y.0 - delta[1]) }, next.region.depth, next.zoom)
}

pub fn ancestors(cell: &RegionKey) -> Result<Vec<RegionKey>> {
    region_origin(cell)?;
    let mut result = Vec::with_capacity(usize::from(cell.depth));
    let mut current = *cell;
    while current.depth > 0 {
        current = RegionKey { depth: current.depth - 1, x: Wide(current.x.0.div_euclid(CHILD_SCALE)), y: Wide(current.y.0.div_euclid(CHILD_SCALE)) };
        result.push(current);
    }
    Ok(result)
}

pub fn cells_between(min: Point, max: Point, depth: u8, max_cells: usize) -> Result<Vec<RegionKey>> {
    validate_point(min)?;
    validate_point(max)?;
    if min.x > max.x || min.y > max.y || max_cells == 0 || max_cells > MAX_SPATIAL_CELLS {
        return Err(Error::Invalid("cell range"));
    }
    let span = region_span(depth)?;
    let x0 = min.x.0.div_euclid(span);
    let x1 = max.x.0.div_euclid(span);
    let y0 = min.y.0.div_euclid(span);
    let y1 = max.y.0.div_euclid(span);
    let count = (x1 - x0 + 1).checked_mul(y1 - y0 + 1).filter(|n| *n <= max_cells as i128).ok_or(Error::Limit("visible regions"))?;
    let mut result = Vec::with_capacity(count as usize);
    for y in y0..=y1 {
        for x in x0..=x1 {
            result.push(RegionKey { depth, x: Wide(x), y: Wide(y) });
        }
    }
    Ok(result)
}

pub fn visible_regions(view: &Viewport, depth: u8, max_cells: usize) -> Result<Vec<RegionKey>> {
    validate_view(view)?;
    let c = center(&view.camera)?;
    let delta = tick_delta(&view.camera, [f64::from(view.width) * 0.5, f64::from(view.height) * 0.5])?;
    let limit = extent()?;
    let min = Point { x: Wide((c.x.0 - delta[0]).max(-limit)), y: Wide((c.y.0 - delta[1]).max(-limit)) };
    let max = Point { x: Wide((c.x.0 + delta[0]).min(limit - 1)), y: Wide((c.y.0 + delta[1]).min(limit - 1)) };
    cells_between(min, max, depth, max_cells)
}

pub fn scale_ratio(stroke_depth: u8, camera: &Camera) -> Result<f64> {
    validate_camera(camera)?;
    Ok(local_unit(stroke_depth)? as f64 / local_unit(camera.region.depth)? as f64 * camera.zoom)
}

fn smooth(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

/// A convex interpolation never leaves its exact integer endpoints. Splitting
/// quotient/remainder avoids an overflowing wide-coordinate multiplication and
/// avoids a rounded f64 displacement overshooting a world-boundary target.
fn interpolate(a: i128, b: i128, t: f64) -> i128 {
    const FRACTION: i128 = 1_i128 << 52;
    let weight = ((t * FRACTION as f64).round() as i128).clamp(0, FRACTION);
    let delta = b - a;
    a + (delta / FRACTION) * weight + ((delta % FRACTION) * weight) / FRACTION
}

/// Zoom out, travel in a common coarse frame, then zoom in. Endpoints remain exact.
pub fn flight(from: &Camera, to: &Camera, t: f64) -> Result<Camera> {
    validate_camera(from)?;
    validate_camera(to)?;
    if from.canvas != to.canvas {
        return Err(Error::Canvas);
    }
    if !t.is_finite() || !(0.0..=1.0).contains(&t) {
        return Err(Error::Invalid("camera flight progress"));
    }
    if t == 0.0 {
        return Ok(from.clone());
    }
    if t == 1.0 {
        return Ok(to.clone());
    }
    let a = center(from)?;
    let b = center(to)?;
    let dx = b.x.0 - a.x.0;
    let dy = b.y.0 - a.y.0;
    let start = local_unit(from.region.depth)? as f64 / from.zoom;
    let end = local_unit(to.region.depth)? as f64 / to.zoom;
    let bridge = start.max(end).max((dx as f64).abs().max((dy as f64).abs()) / 600.0);
    let (position, ticks_per_pixel) = if t < 1.0 / 3.0 {
        let u = smooth(t * 3.0);
        (a, (start.ln() + (bridge.ln() - start.ln()) * u).exp())
    } else if t < 2.0 / 3.0 {
        let u = smooth(t * 3.0 - 1.0);
        (Point { x: Wide(interpolate(a.x.0, b.x.0, u)), y: Wide(interpolate(a.y.0, b.y.0, u)) }, bridge)
    } else {
        let u = smooth(t * 3.0 - 2.0);
        (b, (bridge.ln() + (end.ln() - bridge.ln()) * u).exp())
    };
    target(from.canvas, position, 0, local_unit(0)? as f64 / ticks_per_pixel)
}
