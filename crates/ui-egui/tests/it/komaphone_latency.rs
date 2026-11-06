//! Real egui input to offscreen rendered pixels, with the K1 ten-layer workload.
use egui::{Event, PointerButton};
use photocraft_ui_egui::canvas::ViewXform;
use photocraft_ui_egui::{PhotocraftApp, Tool};
use serde_json::json;
use std::time::{Duration, Instant};

#[test]
fn ten_layers_pointer_to_rendered_stroke_latency() {
    let _gpu = photocraft_testkit::gpu_lock();
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("drafts.sqlite3");
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1280.0, 720.0))
        .with_pixels_per_point(1.0)
        .with_max_steps(64)
        .wgpu()
        // Match the desktop adapter policy; kittest's screenshot default prefers software WARP.
        .wgpu_setup(eframe::egui_wgpu::WgpuSetup::without_display_handle())
        .build_eframe(move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            let mut session = photocraft_engine::Session::new();
            session.enable_local_projects(database, Default::default()).unwrap();
            session.execute("komaphone.project.new", json!({})).unwrap();
            for n in 1..10 {
                session.execute("layer.new.layer", json!({"name":format!("Paint {n}")})).unwrap();
            }
            session.flush_local_projects(Duration::from_secs(30)).unwrap();
            let mut app = PhotocraftApp::new(session, Default::default());
            app.ui.komaphone.enabled = true;
            app.ui.tool = Tool::Brush;
            if let Some(state) = cc.wgpu_render_state.as_ref() {
                app.set_wgpu(state.clone());
            }
            app
        });
    harness.run_steps(5);
    harness.state_mut().run("tools.setBrush", json!({"brush":{"size":24,"hardness":1,"smoothing":{"enabled":false,"amount":0}}})).unwrap();
    harness.state_mut().run("tools.setColors", json!({"foreground":[0.1,0.2,0.7,1.0]})).unwrap();
    let context = harness.ctx.clone();
    photocraft_ui_egui::menus::invoke(harness.state_mut(), &context, "view.fitOnScreen", json!({})).unwrap();
    harness.run_steps(12);
    let transform = ViewXform::active(harness.state()).unwrap();
    let start = transform.to_screen(400.0, 500.0);
    assert!(harness.state().last_canvas_rect.contains(start), "stroke starts inside the visible canvas");
    harness.event(Event::PointerMoved(start));
    harness.event(Event::PointerButton { pos: start, button: PointerButton::Primary, pressed: true, modifiers: Default::default() });
    harness.step();
    harness.render().unwrap();
    let mut times = Vec::new();
    let mut frames = Vec::new();
    let mut last = start;
    for n in 1..=20 {
        last = transform.to_screen(400.0 + n as f32 * 25.0, 500.0);
        assert!(harness.state().last_canvas_rect.contains(last));
        let started = Instant::now();
        harness.event(Event::PointerMoved(last));
        // Read the new stroke behind the pointer, away from the cursor overlay.
        let sample = transform.to_screen(400.0 + n as f32 * 25.0 - 8.0, 500.0);
        let mut visible = false;
        for frame in 1..=4 {
            harness.step();
            let image = harness.render().unwrap();
            let pixel = image.get_pixel(sample.x.round() as u32, sample.y.round() as u32).0;
            if pixel[2] > pixel[0] {
                times.push(started.elapsed().as_secs_f64() * 1000.0);
                frames.push(frame);
                visible = true;
                break;
            }
            if frame == 4 {
                if let Ok(path) = std::env::var("KOMAPHONE_LATENCY_ARTIFACT") {
                    image.save(path).unwrap();
                }
                eprintln!("sample {n} at {sample:?}: {pixel:?}, tool={:?}, status={}", harness.state().ui.tool, harness.state().ui.status);
            } else {
                // Include a realistic frame interval when paint needs a subsequent repaint.
                std::thread::sleep(Duration::from_micros(16_667));
            }
        }
        assert!(visible, "stroke must become visible while the mouse remains down");
    }
    harness.event(Event::PointerButton { pos: last, button: PointerButton::Primary, pressed: false, modifiers: Default::default() });
    harness.step();
    harness.state_mut().session.flush_local_projects(Duration::from_secs(20)).unwrap();
    times.sort_by(f64::total_cmp);
    println!(
        "K1 1536x1024,10layers,1280x720: pointer-to-offscreen-readback p50={:.2}ms p95={:.2}ms max={:.2}ms frames={frames:?} adapter={:?}",
        times[10],
        times[18],
        times[19],
        harness.state().perf.gpu_info
    );
    assert!(times[18] <= 50.0, "p95 exceeded 50ms: {times:?}");
}
