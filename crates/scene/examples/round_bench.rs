//! Native CPU tessellation microbenchmark; no GPU, input-latency or projection claim.
//!
//! cargo run -p photocraft-scene --release --example round_bench -- --json log/round-bench.json
use photocraft_scene::{MAX_VIEW_POINTS, round};
use serde::Serialize;
use std::{error::Error, f64::consts::TAU, hint::black_box, io, mem::size_of, path::PathBuf, time::Instant};

const VIEWPORT: [u32; 2] = [1920, 1080];
const WARMUPS: usize = 20;
const SAMPLES: usize = 200;

type BenchResult<T> = Result<T, Box<dyn Error>>;

struct Stroke {
    paths: Vec<Vec<[f32; 2]>>,
    width: f32,
}

struct Case {
    name: &'static str,
    pixels_per_point: f32,
    strokes: Vec<Stroke>,
}

#[derive(Serialize)]
struct Measurement {
    name: &'static str,
    pixels_per_point: f32,
    width_points: f32,
    strokes: usize,
    input_points: usize,
    vertices: usize,
    indices: usize,
    mesh_payload_bytes: usize,
    mesh_allocated_capacity_bytes: usize,
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
}

#[derive(Serialize)]
struct Report {
    scope: &'static str,
    operational_budget: &'static str,
    profile: &'static str,
    operating_system: &'static str,
    architecture: &'static str,
    viewport_points: [u32; 2],
    source_point_budget: usize,
    vertex_budget: usize,
    vertex_size_bytes: usize,
    index_size_bytes: usize,
    warmups: usize,
    samples: usize,
    percentile_method: &'static str,
    measurements: Vec<Measurement>,
}

#[derive(Clone, Copy, Default)]
struct GeometrySize {
    vertices: usize,
    indices: usize,
    payload_bytes: usize,
    allocated_capacity_bytes: usize,
}

fn smooth_curve() -> Vec<[f32; 2]> {
    (0..8192)
        .map(|index| {
            let t = f64::from(index) / 8191.0;
            [(60.0 + 1800.0 * t) as f32, (540.0 + 260.0 * (TAU * t).sin() + 60.0 * (3.0 * TAU * t).sin()) as f32]
        })
        .collect()
}

fn quantized_circle() -> Vec<[f32; 2]> {
    (0..4096)
        .map(|index| {
            let angle = TAU * f64::from(index) / 4095.0;
            [(960.0 + 380.0 * angle.cos()).round() as f32, (540.0 + 380.0 * angle.sin()).round() as f32]
        })
        .collect()
}

fn short_strokes() -> Vec<Stroke> {
    (0..256)
        .map(|index| {
            let x = 45.0 + 115.0 * f64::from(index % 16);
            let y = 30.0 + 64.0 * f64::from(index / 16);
            let points = (0..16)
                .map(|sample| {
                    let t = f64::from(sample) / 15.0;
                    [(x + 24.0 * t) as f32, (y + 3.0 * (TAU * t).sin()) as f32]
                })
                .collect();
            Stroke { paths: vec![points], width: 5.0 }
        })
        .collect()
}

fn cases() -> Vec<Case> {
    let curve = |name, width, pixels_per_point| Case { name, pixels_per_point, strokes: vec![Stroke { paths: vec![smooth_curve()], width }] };
    vec![
        curve("smooth_8192_width5_density1", 5.0, 1.0),
        curve("smooth_8192_width64_density1", 64.0, 1.0),
        curve("smooth_8192_width64_density2", 64.0, 2.0),
        Case { name: "quantized_circle_4096_width16_density2", pixels_per_point: 2.0, strokes: vec![Stroke { paths: vec![quantized_circle()], width: 16.0 }] },
        Case { name: "separate_256_strokes_16_points_width5_density1", pixels_per_point: 1.0, strokes: short_strokes() },
        Case {
            name: "full_view_16_curves_8192_points_width64_density2",
            pixels_per_point: 2.0,
            strokes: (0..16)
                .map(|index| {
                    let points = smooth_curve().into_iter().map(|[x, y]| [x, y + index as f32 - 7.5]).collect();
                    Stroke { paths: vec![points], width: 64.0 }
                })
                .collect(),
        },
    ]
}

// Keep all generated meshes alive until the entire view is built, as in a frame.
// Each stroke receives the remaining shared viewport budget, not its own budget.
fn tessellate(case: &Case) -> BenchResult<GeometrySize> {
    let mut meshes = Vec::with_capacity(case.strokes.len());
    let mut size = GeometrySize::default();
    for stroke in &case.strokes {
        let remaining = round::MAX_MESH_VERTICES.checked_sub(size.vertices).ok_or_else(|| io::Error::other("vertex budget exhausted"))?;
        let mesh = round::mesh(black_box(&stroke.paths), stroke.width, VIEWPORT, case.pixels_per_point, remaining)?;
        size.vertices = size.vertices.checked_add(mesh.vertices.len()).ok_or_else(|| io::Error::other("vertex count overflow"))?;
        size.indices = size.indices.checked_add(mesh.indices.len()).ok_or_else(|| io::Error::other("index count overflow"))?;
        let vertex_bytes = mesh.vertices.capacity().checked_mul(size_of::<round::StrokeVertex>());
        let index_bytes = mesh.indices.capacity().checked_mul(size_of::<u32>());
        size.allocated_capacity_bytes = vertex_bytes
            .and_then(|bytes| index_bytes.and_then(|other| bytes.checked_add(other)))
            .and_then(|bytes| size.allocated_capacity_bytes.checked_add(bytes))
            .ok_or_else(|| io::Error::other("allocated geometry byte count overflow"))?;
        meshes.push(mesh);
    }
    size.payload_bytes = size
        .vertices
        .checked_mul(size_of::<round::StrokeVertex>())
        .and_then(|bytes| size.indices.checked_mul(size_of::<u32>()).and_then(|other| bytes.checked_add(other)))
        .ok_or_else(|| io::Error::other("geometry payload byte count overflow"))?;
    black_box(&meshes);
    // Allocation and destruction are included in the timed call; fixture creation is not.
    drop(meshes);
    Ok(size)
}

fn percentile(sorted: &[f64], percent: usize) -> BenchResult<f64> {
    let index = sorted.len().saturating_mul(percent).div_ceil(100).saturating_sub(1);
    sorted.get(index).copied().ok_or_else(|| io::Error::other("missing timing sample").into())
}

fn measure(case: &Case) -> BenchResult<Measurement> {
    let input_points: usize = case.strokes.iter().flat_map(|stroke| &stroke.paths).map(Vec::len).sum();
    if input_points > MAX_VIEW_POINTS {
        return Err(io::Error::other("fixture exceeds the unchanged source point budget").into());
    }
    for _ in 0..WARMUPS {
        black_box(tessellate(case)?);
    }
    let mut times = Vec::with_capacity(SAMPLES);
    let mut counts = GeometrySize::default();
    for _ in 0..SAMPLES {
        let start = Instant::now();
        counts = black_box(tessellate(case)?);
        times.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    Ok(Measurement {
        name: case.name,
        pixels_per_point: case.pixels_per_point,
        width_points: case.strokes.first().map_or(0.0, |stroke| stroke.width),
        strokes: case.strokes.len(),
        input_points,
        vertices: counts.vertices,
        indices: counts.indices,
        mesh_payload_bytes: counts.payload_bytes,
        mesh_allocated_capacity_bytes: counts.allocated_capacity_bytes,
        p50_ms: percentile(&times, 50)?,
        p95_ms: percentile(&times, 95)?,
        max_ms: times.last().copied().ok_or_else(|| io::Error::other("missing maximum sample"))?,
    })
}

fn output_path() -> BenchResult<Option<PathBuf>> {
    let mut args = std::env::args_os().skip(1);
    let Some(flag) = args.next() else { return Ok(None) };
    if flag != "--json" {
        return Err(io::Error::other("usage: round_bench [--json PATH]").into());
    }
    let path = args.next().ok_or_else(|| io::Error::other("--json requires an output path"))?;
    if args.next().is_some() {
        return Err(io::Error::other("usage: round_bench [--json PATH]").into());
    }
    Ok(Some(PathBuf::from(path)))
}

fn main() -> BenchResult<()> {
    let path = output_path()?;
    let measurements = cases().iter().map(measure).collect::<BenchResult<Vec<_>>>()?;
    let report = Report {
        scope: "CPU round-brush tessellation, allocations and destruction only; deterministic fixtures built before timing. Excludes coordinate projection, input processing, GPU upload/rasterization and input-to-display latency. No machine-independent timing assertion.",
        operational_budget: "The retained source-point cap is unchanged. Round outlines need a separate, larger generated-vertex budget to preserve full legal views. Mesh bytes use actual Rust vertex/index sizes and Vec capacities; they exclude container overhead, source paths, egui conversion copies and GPU buffers. Increased geometry consumes CPU time and memory, measured here rather than hidden by lowering source capacity.",
        profile: if cfg!(debug_assertions) { "debug; not release performance evidence" } else { "release" },
        operating_system: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        viewport_points: VIEWPORT,
        source_point_budget: MAX_VIEW_POINTS,
        vertex_budget: round::MAX_MESH_VERTICES,
        vertex_size_bytes: size_of::<round::StrokeVertex>(),
        index_size_bytes: size_of::<u32>(),
        warmups: WARMUPS,
        samples: SAMPLES,
        percentile_method: "nearest rank",
        measurements,
    };
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(path) = path {
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, format!("{json}\n"))?;
    }
    println!("{json}");
    Ok(())
}
