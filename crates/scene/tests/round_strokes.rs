//! Independent coverage checks: the oracle is distance to the original polyline,
//! rather than the renderer's joins, tessellation, or triangle construction.
use photocraft_scene::{
    MAX_POINTS, MAX_VIEW_POINTS,
    round::{self, StrokeMesh},
};

fn distance_to_path(point: [f64; 2], path: &[[f32; 2]]) -> f64 {
    let mut distance = f64::INFINITY;
    for endpoint in path {
        distance = distance.min((point[0] - f64::from(endpoint[0])).hypot(point[1] - f64::from(endpoint[1])));
    }
    for pair in path.windows(2) {
        let a = [f64::from(pair[0][0]), f64::from(pair[0][1])];
        let delta = [f64::from(pair[1][0]) - a[0], f64::from(pair[1][1]) - a[1]];
        let length_squared = delta[0] * delta[0] + delta[1] * delta[1];
        if length_squared > 0.0 {
            let along = (((point[0] - a[0]) * delta[0] + (point[1] - a[1]) * delta[1]) / length_squared).clamp(0.0, 1.0);
            distance = distance.min((point[0] - a[0] - along * delta[0]).hypot(point[1] - a[1] - along * delta[1]));
        }
    }
    distance
}

fn validate_mesh(mesh: &StrokeMesh, budget: usize) {
    assert!(mesh.vertices.len() <= budget);
    assert_eq!(mesh.indices.len() % 3, 0);
    for vertex in &mesh.vertices {
        assert!(vertex.position.iter().all(|component| component.is_finite()));
        assert!(vertex.coverage.is_finite() && (0.0..=1.0).contains(&vertex.coverage));
    }
    assert!(mesh.indices.iter().all(|&index| (index as usize) < mesh.vertices.len()));
}

/// Rasterize only the queried sample, using source-over coverage as wgpu does.
/// Non-grid-aligned probe positions avoid double-counting shared triangle edges.
fn coverage_at(mesh: &StrokeMesh, point: [f64; 2]) -> f64 {
    let mut result = 0.0;
    for indices in mesh.indices.chunks_exact(3) {
        let vertices = [&mesh.vertices[indices[0] as usize], &mesh.vertices[indices[1] as usize], &mesh.vertices[indices[2] as usize]];
        let a = vertices[0].position.map(f64::from);
        let b = vertices[1].position.map(f64::from);
        let c = vertices[2].position.map(f64::from);
        let denominator = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1]);
        if denominator.abs() < 1e-14 {
            continue;
        }
        let u = ((b[1] - c[1]) * (point[0] - c[0]) + (c[0] - b[0]) * (point[1] - c[1])) / denominator;
        let v = ((c[1] - a[1]) * (point[0] - c[0]) + (a[0] - c[0]) * (point[1] - c[1])) / denominator;
        let w = 1.0 - u - v;
        if u < -1e-10 || v < -1e-10 || w < -1e-10 {
            continue;
        }
        let alpha = (u * f64::from(vertices[0].coverage) + v * f64::from(vertices[1].coverage) + w * f64::from(vertices[2].coverage)).clamp(0.0, 1.0);
        result += (1.0 - result) * alpha;
    }
    result
}

fn assert_round_coverage(paths: &[Vec<[f32; 2]>], width: f32, viewport: [u32; 2], pixels_per_point: f32) {
    assert_round_coverage_with_stride(paths, width, viewport, pixels_per_point, 3);
}

fn assert_round_coverage_with_stride(paths: &[Vec<[f32; 2]>], width: f32, viewport: [u32; 2], pixels_per_point: f32, stride: usize) {
    let original = paths.to_vec();
    let mesh = round::mesh(paths, width, viewport, pixels_per_point, MAX_VIEW_POINTS).unwrap();
    validate_mesh(&mesh, MAX_VIEW_POINTS);
    assert_eq!(paths, original, "rendering must leave retained samples unchanged");
    let radius = f64::from(width) * 0.5;
    let band = 1.0 / f64::from(pixels_per_point);
    let mut interior = 0;
    let mut exterior = 0;
    for y in (0..viewport[1]).step_by(stride) {
        for x in (0..viewport[0]).step_by(stride) {
            let point = [f64::from(x) + 0.371, f64::from(y) + 0.619];
            let distance = paths.iter().map(|path| distance_to_path(point, path)).fold(f64::INFINITY, f64::min);
            let coverage = coverage_at(&mesh, point);
            if distance < radius - band {
                interior += 1;
                assert!(
                    coverage >= 0.98,
                    "missing brush interior at {point:?}: distance={distance}, radius={radius}, coverage={coverage}, density={pixels_per_point}"
                );
            } else if distance > radius + band {
                exterior += 1;
                assert!(
                    coverage <= 0.01,
                    "ink outside round brush at {point:?}: distance={distance}, radius={radius}, coverage={coverage}, density={pixels_per_point}"
                );
            }
        }
    }
    assert!(interior > 0 && exterior > 0, "fixture must exercise both sides of the silhouette");
}

#[test]
fn tiny_mouse_steps_do_not_become_square_miter_protrusions() {
    let paths = vec![vec![[76.0, 76.0], [80.0, 76.0], [80.0, 80.0], [84.0, 80.0], [84.0, 84.0], [88.0, 84.0]]];
    for density in [1.0, 2.0] {
        for width in [32.0, 64.0, 128.0] {
            assert_round_coverage(&paths, width, [192, 192], density);
        }
    }
}

#[test]
fn corners_reversals_and_repeated_samples_remain_a_round_brush() {
    let cases = [
        vec![[40.0, 110.0], [90.0, 110.0], [90.0, 50.0]],
        vec![[50.0, 90.0], [110.0, 90.0], [50.0, 90.0]],
        vec![[50.0, 80.0], [95.0, 80.0], [95.0, 80.0], [95.0, 80.0], [70.0, 120.0]],
        vec![[95.0, 95.0], [95.0, 95.0], [95.0, 95.0]],
        vec![[40.0, 40.0], [140.0, 140.0], [40.0, 140.0], [140.0, 40.0]],
    ];
    for path in cases {
        for density in [1.0, 2.0] {
            assert_round_coverage(std::slice::from_ref(&path), 40.0, [192, 192], density);
        }
    }
}

#[test]
fn visible_reversals_and_closed_loops_survive_display_simplification() {
    let cases = [
        // Distance to the infinite endpoint line is zero; distance to the
        // finite endpoint segment preserves the far, visible turnaround.
        vec![[35.0, 70.0], [155.0, 70.0], [60.0, 70.0]],
        vec![[35.0, 120.0], [155.0, 120.0], [60.0, 120.0], [80.0, 120.08]],
        // Coincident endpoints must not collapse a complete closed outline.
        vec![[40.0, 40.0], [150.0, 40.0], [150.0, 150.0], [40.0, 150.0], [40.0, 40.0]],
        // A subpixel loop may become a dot, but two identical retained
        // endpoints must never enter direction normalization as a segment.
        vec![[96.0, 96.0], [96.03, 96.02], [96.0, 96.0]],
    ];
    for path in cases {
        for density in [1.0, 2.0] {
            assert_round_coverage(std::slice::from_ref(&path), 12.0, [192, 192], density);
        }
    }
}

#[test]
fn clipped_paths_do_not_join_across_the_view() {
    let paths = vec![vec![[-100.0, 40.0], [250.0, 40.0]], vec![[-100.0, 150.0], [250.0, 150.0]]];
    for density in [1.0, 2.0] {
        assert_round_coverage(&paths, 16.0, [192, 192], density);
    }
}

#[test]
fn enlarged_dot_has_no_fixed_polygon_facets() {
    // Center a magnified circle edge halfway between the directions used by a
    // fixed128-sided circle. That old approximation misses almost5pixels here.
    let radius = 16_384.0_f64;
    let angle = std::f64::consts::PI / 128.0;
    let path = vec![[(64.0 - radius * angle.cos()) as f32, (64.0 - radius * angle.sin()) as f32]];
    for density in [1.0, 2.0] {
        assert_round_coverage(std::slice::from_ref(&path), (2.0 * radius) as f32, [128, 128], density);
    }
}

#[test]
fn legal_dense_gestures_stay_within_the_geometry_budget() {
    let straight: Vec<_> = (0..MAX_POINTS).map(|i| [20.0 + i as f32 * 0.02, 100.0]).collect();
    let curve: Vec<_> = (0..MAX_POINTS)
        .map(|i| {
            let angle = i as f32 / (MAX_POINTS - 1) as f32 * std::f32::consts::PI;
            [128.0 + 80.0 * angle.cos(), 128.0 + 80.0 * angle.sin()]
        })
        .collect();
    for path in [straight, curve] {
        for width in [5.0, 64.0] {
            let mesh = round::mesh(std::slice::from_ref(&path), width, [256, 256], 2.0, MAX_VIEW_POINTS).unwrap();
            validate_mesh(&mesh, MAX_VIEW_POINTS);
            assert!(!mesh.indices.is_empty());
        }
    }
}

#[test]
fn dense_smooth_path_uses_few_vertices_without_changing_retained_artwork() {
    let path: Vec<_> = (0..MAX_POINTS)
        .map(|i| {
            let t = i as f64 / (MAX_POINTS - 1) as f64;
            let angle = std::f64::consts::TAU * t;
            [(16.0 + 224.0 * t) as f32, (128.0 + 64.0 * angle.sin() + 12.0 * (3.0 * angle).sin()) as f32]
        })
        .collect();
    let original = path.clone();
    for density in [1.0, 2.0] {
        let mesh = round::mesh(std::slice::from_ref(&path), 8.0, [256, 256], density, MAX_VIEW_POINTS).unwrap();
        validate_mesh(&mesh, MAX_VIEW_POINTS);
        assert!(mesh.vertices.len() < 4_000, "smooth display geometry is unnecessarily large: {} vertices at density {density}", mesh.vertices.len());
        assert_eq!(path, original, "screen simplification changed retained samples");
        // A coarser probe grid keeps the independent 8192-segment distance oracle
        // affordable. The acceptance band remains exactly one physical pixel.
        assert_round_coverage_with_stride(std::slice::from_ref(&path), 8.0, [256, 256], density, 8);
    }
}

#[test]
fn near_threshold_features_use_physical_screen_error_at_each_density() {
    for height in [0.12, 0.13] {
        let physical_path = vec![[20.0, 30.0], [60.0, 30.0 + height], [100.0, 30.0]];
        let original = physical_path.clone();
        let reference = round::mesh(std::slice::from_ref(&physical_path), 12.0, [128, 80], 1.0, MAX_VIEW_POINTS).unwrap();
        let logical_path: Vec<_> = physical_path.iter().map(|point| [point[0] * 0.5, point[1] * 0.5]).collect();
        let high_dpi = round::mesh(std::slice::from_ref(&logical_path), 6.0, [64, 40], 2.0, MAX_VIEW_POINTS).unwrap();
        validate_mesh(&reference, MAX_VIEW_POINTS);
        validate_mesh(&high_dpi, MAX_VIEW_POINTS);
        // Both calls describe exactly the same physical stroke. A tolerance in
        // logical/world units would incorrectly discard the0.13px feature at2x.
        assert_eq!(reference.indices, high_dpi.indices, "physical feature height={height}");
        assert_eq!(reference.vertices.len(), high_dpi.vertices.len(), "physical feature height={height}");
        for (low, high) in reference.vertices.iter().zip(&high_dpi.vertices) {
            for axis in 0..2 {
                assert!((low.position[axis] - high.position[axis] * 2.0).abs() < 0.002, "DPI-dependent geometry at feature height={height}");
            }
            assert!((low.coverage - high.coverage).abs() < 1e-6);
        }
        assert_eq!(physical_path, original);
        assert_round_coverage(std::slice::from_ref(&physical_path), 12.0, [128, 80], 1.0);
        assert_round_coverage(std::slice::from_ref(&logical_path), 6.0, [64, 40], 2.0);
    }
}

#[test]
fn subpixel_strokes_keep_fractional_coverage() {
    let path = vec![[30.0, 50.0], [130.0, 50.0]];
    for density in [1.0, 2.0] {
        let width = 0.25 / density;
        let mesh = round::mesh(std::slice::from_ref(&path), width, [160, 100], density, MAX_VIEW_POINTS).unwrap();
        validate_mesh(&mesh, MAX_VIEW_POINTS);
        assert!(mesh.vertices.iter().all(|vertex| vertex.coverage <= 0.250_01));
        let center_coverage = coverage_at(&mesh, [75.371, 50.0 + 0.013 / f64::from(density)]);
        assert!((0.20..=0.30).contains(&center_coverage), "density={density}, coverage={center_coverage}");
        assert_eq!(coverage_at(&mesh, [75.371, 52.0]), 0.0);
    }
}

#[test]
fn malformed_input_and_insufficient_budget_fail_explicitly() {
    let path = vec![[40.0, 40.0], [100.0, 100.0]];
    let invisible = round::mesh(std::slice::from_ref(&path), 0.0, [160, 160], 1.0, MAX_VIEW_POINTS).unwrap();
    assert!(invisible.vertices.is_empty() && invisible.indices.is_empty());
    for width in [-1.0, f32::NAN, f32::INFINITY] {
        assert!(round::mesh(std::slice::from_ref(&path), width, [160, 160], 1.0, MAX_VIEW_POINTS).is_err());
    }
    for density in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert!(round::mesh(std::slice::from_ref(&path), 8.0, [160, 160], density, MAX_VIEW_POINTS).is_err());
    }
    assert!(round::mesh(&[vec![[f32::NAN, 0.0]]], 8.0, [160, 160], 1.0, MAX_VIEW_POINTS).is_err());
    assert!(round::mesh(std::slice::from_ref(&path), 8.0, [0, 160], 1.0, MAX_VIEW_POINTS).is_err());
    assert!(round::mesh(std::slice::from_ref(&path), 8.0, [160, 160], 1.0, 3).is_err());
}
