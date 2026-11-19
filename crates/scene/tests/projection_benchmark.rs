//! Reproducible projection-only microbenchmark; native UI frame timing is separate.
use photocraft_scene::{coordinates::*, render::project, *};
use std::hint::black_box;
use std::time::Instant;

fn fixture() -> (CanvasId, Vec<Stroke>) {
    let canvas = CanvasId::new("11111111111111111111111111111111").unwrap();
    let owner = OwnerId::new("22222222222222222222222222222222").unwrap();
    let shapes = [
        vec![[-200.0, 180.0], [-200.0, -100.0], [0.0, -250.0], [200.0, -100.0], [200.0, 180.0], [-200.0, 180.0]],
        vec![[-40.0, 180.0], [-40.0, 0.0], [40.0, 0.0], [40.0, 180.0]],
        vec![[-150.0, -50.0], [-90.0, -50.0], [-90.0, 30.0], [-150.0, 30.0], [-150.0, -50.0]],
        vec![[90.0, -50.0], [150.0, -50.0], [150.0, 30.0], [90.0, 30.0], [90.0, -50.0]],
    ];
    let mut strokes = Vec::new();
    for depth in 0..=6 {
        for shape in &shapes {
            let revision = strokes.len() as u64 + 1;
            strokes.push(Stroke {
                id: StrokeId::new(&format!("{revision:032x}")).unwrap(),
                canvas,
                owner,
                revision,
                anchor: RegionKey { depth, x: Wide(0), y: Wide(0) },
                points: shape.clone(),
                width: 6.0,
                color: Srgba { rgba: [0.1, 0.2, 0.5, 1.0] },
                erase: false,
                brush: None,
                samples: None,
            });
        }
    }
    (canvas, strokes)
}

#[test]
fn nested_projection_measurements() {
    let (canvas, strokes) = fixture();
    let mut rows = Vec::new();
    for depth in 0..=6 {
        let viewport = Viewport { camera: target(canvas, Point::default(), depth, 1.0).unwrap(), width: 1536, height: 1024 };
        let mut durations = Vec::new();
        let mut primitives = 0;
        for iteration in 0..320 {
            let start = Instant::now();
            let results: Vec<_> = strokes.iter().map(|stroke| project(stroke, &viewport).unwrap()).collect();
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            primitives = results.iter().flatten().map(|stroke| stroke.paths.len() + stroke.fills.len()).sum::<usize>();
            black_box(results);
            if iteration >= 20 {
                durations.push(elapsed);
            }
        }
        durations.sort_by(f64::total_cmp);
        assert!(primitives > 0);
        rows.push(serde_json::json!({"depth":depth,"magnification":100_u64.pow(u32::from(depth)),"strokes":strokes.len(),"primitives":primitives,
            "samples":durations.len(),"p50_ms":durations[150],"p95_ms":durations[285],"max_ms":durations[299]}));
    }
    let result = serde_json::json!({"kind":"projection_only","viewport":[1536,1024],"fixture":"seven nested four-stroke houses, six 100x steps","warmup":20,"rows":rows});
    if let Ok(path) = std::env::var("K5_SCENE_BENCH_OUT") {
        std::fs::write(path, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
    }
    println!("{result}");
}
