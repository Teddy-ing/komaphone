use photocraft_scene::{coordinates::*, render::*, *};

fn stroke(points: Vec<[f64; 2]>, width: f64) -> Stroke {
    Stroke {
        id: StrokeId::new("11111111111111111111111111111111").unwrap(),
        canvas: CanvasId::new("22222222222222222222222222222222").unwrap(),
        owner: OwnerId::new("33333333333333333333333333333333").unwrap(),
        revision: 1,
        anchor: RegionKey::default(),
        points,
        width,
        color: Srgba { rgba: [0.1, 0.2, 0.3, 1.0] },
        erase: false,
        brush: None,
        samples: None,
    }
}
fn view(stroke: &Stroke, local: [f64; 2], depth: u8) -> Viewport {
    Viewport { camera: target(stroke.canvas, point(&stroke.anchor, local).unwrap(), depth, 1.0).unwrap(), width: 1024, height: 768 }
}

#[test]
fn spatial_references_cross_seams_without_duplicating_the_stroke() {
    let line = stroke(vec![[1000.0, 300.0], [1100.0, 300.0]], 8.0);
    let cells = spatial_cells(&line, MAX_SPATIAL_CELLS).unwrap();
    assert_eq!(cells.len(), 2);
    assert_eq!(cells[0].x, Wide(0));
    assert_eq!(cells[1].x, Wide(1));
    let picture = project(&line, &view(&line, [1024.0, 300.0], 0)).unwrap().unwrap();
    assert_eq!(picture.paths.len(), 1);
    assert_eq!(picture.id, line.id);
    assert_eq!(picture.width, 8.0);
    assert_eq!(picture.paths[0], vec![[488.0, 384.0], [588.0, 384.0]]);
}

#[test]
fn subquantum_width_on_a_seam_is_indexed_on_both_sides() {
    let line = stroke(vec![[0.0, 10.0], [0.0, 20.0]], 1e-24);
    let cells = spatial_cells(&line, MAX_SPATIAL_CELLS).unwrap();
    assert!(cells.iter().any(|c| c.x == Wide(-1)));
    assert!(cells.iter().any(|c| c.x == Wide(0)));
}

#[test]
fn disjoint_clips_do_not_connect_through_the_view() {
    let line = stroke(vec![[-1000.0, -100.0], [1000.0, -100.0], [1000.0, 1000.0], [-1000.0, 100.0]], 2.0);
    let picture = project(&line, &view(&line, [0.0, 0.0], 0)).unwrap().unwrap();
    assert!(picture.paths.len() >= 2);
    assert!(picture.paths.iter().flatten().flatten().all(|v| v.is_finite() && v.abs() < 2048.0));
}

#[test]
fn at_scale_drawing_retains_constant_new_width_and_old_width() {
    for depth in 0..=6 {
        let mut line = stroke(vec![[10.0, 20.0], [30.0, 20.0]], 6.0);
        line.anchor = RegionKey { depth, x: Wide(100_i128.pow(u32::from(depth))), y: Wide(-100_i128.pow(u32::from(depth))) };
        let v = view(&line, [20.0, 20.0], depth);
        let picture = project(&line, &v).unwrap().unwrap();
        assert_eq!(picture.width, 6.0);
        assert_eq!(picture.paths[0], vec![[502.0, 384.0], [522.0, 384.0]]);
        let zoom = Viewport { camera: zoom_at(&v, [512.0, 384.0], 2.0).unwrap(), ..v };
        assert_eq!(project(&line, &zoom).unwrap().unwrap().width, 12.0);
    }
}

#[test]
fn huge_ancestor_line_edge_stays_at_the_same_pixel_through_depth_twelve() {
    let line = stroke(vec![[-1000.0, 0.0], [1000.0, 0.0]], 10.0);
    for depth in [3, 6, 9, 12] {
        let picture = project(&line, &view(&line, [0.0, 5.0], depth)).unwrap().unwrap();
        assert!(picture.paths.is_empty());
        assert_eq!(picture.fills.len(), 1);
        let polygon = &picture.fills[0];
        let bottom = polygon.iter().map(|p| p[1]).fold(f32::NEG_INFINITY, f32::max);
        assert!((bottom - 384.0).abs() < 0.1, "depth {depth}, boundary {bottom}, {polygon:?}");
        assert!(polygon.iter().flatten().all(|v| v.is_finite() && *v >= 0.0 && *v <= 1024.0));
    }
}

#[test]
fn huge_ancestor_dot_edge_and_diagonal_line_are_stable() {
    let dot = stroke(vec![[0.0, 0.0]], 200.0);
    for depth in [3, 6, 12] {
        let picture = project(&dot, &view(&dot, [100.0, 0.0], depth)).unwrap().unwrap();
        let right = picture.fills[0].iter().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max);
        assert!((right - 512.0).abs() < 0.1, "depth {depth}: {right}");
    }
    let line = stroke(vec![[0.0, 0.0], [300.0, 400.0]], 20.0);
    for depth in [6, 12] {
        let picture = project(&line, &view(&line, [158.0, 194.0], depth)).unwrap().unwrap();
        assert!(!picture.fills.is_empty());
        // This point lies on the line's 3:4:5 exact normal offset.
        assert!(picture.fills[0].iter().any(|p| p[0] < 512.0));
        assert!(picture.fills[0].iter().any(|p| p[0] > 512.0));
    }
}

#[test]
fn outside_and_wrong_canvas_are_not_projected() {
    let line = stroke(vec![[0.0, 0.0], [100.0, 100.0]], 2.0);
    assert!(project(&line, &view(&line, [2000.0, 2000.0], 0)).unwrap().is_none());
    let mut v = view(&line, [0.0, 0.0], 0);
    v.camera.canvas = CanvasId::new("44444444444444444444444444444444").unwrap();
    assert_eq!(project(&line, &v), Err(Error::Canvas));
}

#[test]
fn extremely_thin_ancestor_diagonal_keeps_its_screen_intersection() {
    let line = stroke(vec![[-1000.0, -1000.0], [1000.0, 1000.0]], 1e-24);
    let picture = project(&line, &view(&line, [0.0, 0.0], 12)).unwrap().unwrap();
    let path = picture.paths.first().unwrap();
    let first = path.first().unwrap();
    let last = path.last().unwrap();
    // y = x - 128 on this 1024x768 view.
    assert!((first[1] - first[0] + 128.0).abs() < 0.1, "{path:?}");
    assert!((last[1] - last[0] + 128.0).abs() < 0.1, "{path:?}");
}

#[test]
fn summary_is_bounded_and_retains_identity_order_and_endpoints() {
    let original = stroke((0..1000).map(|i| [f64::from(i), f64::from(i % 17)]).collect(), 4.0);
    let summary = decimate(&original, 24).unwrap();
    assert_eq!(summary.points.len(), 24);
    assert_eq!(summary.points.first(), original.points.first());
    assert_eq!(summary.points.last(), original.points.last());
    assert_eq!(summary.id, original.id);
    assert_eq!(summary.revision, original.revision);
    assert_eq!(original.points.len(), 1000);
    assert!(decimate(&original, 1).is_err());
    assert!(spatial_cells(&stroke(vec![[-8192.0, -8192.0], [8192.0, 8192.0]], 2048.0), 1).is_err());
}
