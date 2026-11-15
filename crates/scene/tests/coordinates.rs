use photocraft_scene::{coordinates::*, *};

fn canvas() -> CanvasId {
    CanvasId::new("11111111111111111111111111111111").unwrap()
}
fn view(camera: Camera) -> Viewport {
    Viewport { camera, width: 1024, height: 768 }
}

#[test]
fn six_hundredfold_steps_keep_exact_center_and_mouse_anchor() {
    let start = point(&RegionKey { depth: 0, x: Wide(12345), y: Wide(-54321) }, [123.25, 876.5]).unwrap();
    let mut camera = target(canvas(), start, 0, 1.0).unwrap();
    for depth in 1..=6 {
        camera = zoom_at(&view(camera), [512.0, 384.0], 100.0).unwrap();
        assert_eq!(camera.region.depth, depth);
        assert_eq!(center(&camera).unwrap(), start);
        let v = view(camera.clone());
        let p = screen_to_point(&v, [157.0, 623.0]).unwrap();
        let zoomed = zoom_at(&v, [157.0, 623.0], 1.7).unwrap();
        let after = point_to_screen(&view(zoomed), p).unwrap();
        assert!((after[0] - 157.0).abs() < 0.002);
        assert!((after[1] - 623.0).abs() < 0.002);
    }
    for depth in (0..6).rev() {
        camera = zoom_at(&view(camera), [512.0, 384.0], 0.01).unwrap();
        assert_eq!(camera.region.depth, depth);
        assert_eq!(center(&camera).unwrap(), start);
    }
}

#[test]
fn deepest_position_survives_outward_and_inward_navigation() {
    let initial = point(&RegionKey { depth: 0, x: Wide(987654), y: Wide(-987654) }, [200.0, 300.0]).unwrap();
    let start = Point { x: Wide(initial.x.0 + 123), y: Wide(initial.y.0 + 456) };
    let close = target(canvas(), start, MAX_DEPTH, 1.0).unwrap();
    let far = target(canvas(), center(&close).unwrap(), 0, 0.01).unwrap();
    let back = target(canvas(), center(&far).unwrap(), MAX_DEPTH, 1.0).unwrap();
    assert_eq!(close, back);
    let encoded = serde_json::to_string(&back).unwrap();
    assert_eq!(serde_json::from_str::<Camera>(&encoded).unwrap(), back);
}

#[test]
fn negative_coordinates_have_euclidean_ancestry_and_cross_seam_pan() {
    let cell = RegionKey { depth: 2, x: Wide(-1), y: Wide(-101) };
    let parents = ancestors(&cell).unwrap();
    assert_eq!(parents[0], RegionKey { depth: 1, x: Wide(-1), y: Wide(-2) });
    assert_eq!(parents[1], RegionKey { depth: 0, x: Wide(-1), y: Wide(-1) });
    let camera = target(canvas(), Point { x: Wide(0), y: Wide(0) }, 6, 1.0).unwrap();
    let moved = pan(&camera, [25.5, -17.25]).unwrap();
    assert_eq!(moved.region.x, Wide(-1));
    let restored = pan(&moved, [-25.5, 17.25]).unwrap();
    assert_eq!(center(&restored).unwrap(), center(&camera).unwrap());
}

#[test]
fn viewport_queries_are_bounded_and_position_specific() {
    let camera = home(canvas());
    let cells = visible_regions(&view(camera), 0, MAX_SPATIAL_CELLS).unwrap();
    assert_eq!(cells.len(), 4);
    assert!(cells.contains(&RegionKey { depth: 0, x: Wide(-1), y: Wide(-1) }));
    assert!(visible_regions(&view(home(canvas())), 12, MAX_SPATIAL_CELLS).is_err());
    assert!(visible_regions(&view(home(canvas())), 0, 1).is_err());
}

#[test]
fn invalid_and_exhausted_transforms_return_errors() {
    assert!(region_span(MAX_DEPTH + 1).is_err());
    assert!(point(&RegionKey { depth: 0, x: Wide(i128::MAX), y: Wide(0) }, [0.0, 0.0]).is_err());
    assert!(point(&RegionKey::default(), [f64::NAN, 0.0]).is_err());
    for zoom in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e300] {
        assert!(target(canvas(), Point::default(), 0, zoom).is_err());
    }
    assert!(target(canvas(), Point::default(), MAX_DEPTH, 100.0).is_err());
    assert!(pan(&home(canvas()), [f64::INFINITY, 0.0]).is_err());
    assert!(screen_to_point(&view(home(canvas())), [f64::NAN, 0.0]).is_err());
    assert!(serde_json::from_str::<Wide>("\"+1\"").is_err());
    assert!(serde_json::from_str::<Wide>("\"01\"").is_err());
    assert!(serde_json::from_str::<Wide>("123").is_err());
}

#[test]
fn flight_is_pure_bounded_continuous_and_canvas_scoped() {
    let from = home(canvas());
    let point = point(&RegionKey { depth: 0, x: Wide(2), y: Wide(-3) }, [20.0, 30.0]).unwrap();
    let to = target(canvas(), point, 6, 3.0).unwrap();
    assert_eq!(flight(&from, &to, 0.0).unwrap(), from);
    assert_eq!(flight(&from, &to, 1.0).unwrap(), to);
    for step in 0..=1000 {
        validate_camera(&flight(&from, &to, f64::from(step) / 1000.0).unwrap()).unwrap();
    }
    let mut other = to.clone();
    other.canvas = CanvasId::new("22222222222222222222222222222222").unwrap();
    assert_eq!(flight(&from, &other, 0.5), Err(Error::Canvas));
    assert!(flight(&from, &to, f64::NAN).is_err());
    let paused = flight(&from, &to, 0.43).unwrap();
    assert_eq!(paused, flight(&from, &to, 0.43).unwrap());
}

#[test]
fn far_flights_and_maximum_viewport_keep_root_queries_bounded() {
    let from = target(canvas(), point(&RegionKey { depth: 0, x: Wide(-900000), y: Wide(900000) }, [100.0, 200.0]).unwrap(), 6, 1.0).unwrap();
    let to = target(canvas(), point(&RegionKey { depth: 0, x: Wide(900000), y: Wide(-900000) }, [300.0, 400.0]).unwrap(), 6, 1.0).unwrap();
    for index in 0..=100 {
        let camera = flight(&from, &to, f64::from(index) / 100.0).unwrap();
        let depth = camera.region.depth;
        let view = Viewport { camera, width: MAX_VIEW_SIDE, height: MAX_VIEW_SIDE };
        let cells = visible_regions(&view, depth, MAX_SPATIAL_CELLS).unwrap();
        assert!(cells.len() <= 289);
    }
    let camera = target(canvas(), Point::default(), 0, 1e-30).unwrap();
    assert_eq!(camera.zoom, MIN_ROOT_ZOOM);
    let viewport = Viewport { camera, width: MAX_VIEW_SIDE, height: MAX_VIEW_SIDE };
    assert!(visible_regions(&viewport, 0, MAX_SPATIAL_CELLS).unwrap().len() <= 289);
}

#[test]
fn flight_interpolation_never_overshoots_the_last_valid_coordinate() {
    let limit = extent().unwrap();
    let from = target(canvas(), Point { x: Wide(-limit), y: Wide(limit - 1) }, MAX_DEPTH, 1.0).unwrap();
    let to = target(canvas(), Point { x: Wide(limit - 1), y: Wide(-limit) }, MAX_DEPTH, 1.0).unwrap();
    for t in [1.0 / 3.0, 0.5, 0.666666666, 0.6666666666666, 0.6666666666666665, 2.0 / 3.0, 0.9999999999999999, 1.0] {
        let camera = flight(&from, &to, t).unwrap();
        let point = center(&camera).unwrap();
        assert!(point.x.0 >= -limit && point.x.0 < limit);
        assert!(point.y.0 >= -limit && point.y.0 < limit);
    }
}
