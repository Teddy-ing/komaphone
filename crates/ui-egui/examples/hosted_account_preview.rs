//! Offscreen layout fixtures only: these do not prove Google login or hosted play.
//! cargo run -p photocraft-ui-egui --example hosted_account_preview -- OUTPUT_DIR
use photocraft_ui_egui::{PhotocraftApp, Services};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args_os().nth(1).ok_or("Provide an output directory for layout PNGs")?;
    std::fs::create_dir_all(&output)?;
    let profile = json!({
        "id":"00000000000000000000000000000001",
        "device":"00000000000000000000000000000002",
        "display_name":"Test artist"
    });
    let fixtures = [
        ("signed-out", json!({"hosted":true})),
        ("browser-wait", json!({"hosted":true,"signing_in":true})),
        ("retry", json!({"hosted":true,"error":"Sign-in was canceled. Your local drawings are unchanged; try again when ready."})),
        ("signed-in", json!({"hosted":true,"signed_in":true,"paired":true,"connected":true,"profile":profile})),
        (
            "move-paused",
            json!({"hosted":true,"profile":profile,"relocation":{
                "id":"00000000000000000000000000000003",
                "profile":"00000000000000000000000000000001",
                "device":"00000000000000000000000000000002",
                "from_endpoint":"http://127.0.0.1:43127",
                "to_endpoint":"https://komaphone.tedsoftware.org"
            }}),
        ),
    ];
    for (name, mut fixture) in fixtures {
        fixture["endpoint"] = json!("https://komaphone.tedsoftware.org");
        let status = serde_json::from_value(fixture)?;
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1280.0, 720.0))
            .with_max_steps(64)
            .with_render_options(eframe::egui_wgpu::RendererOptions::PREDICTABLE)
            .wgpu()
            .build_eframe(move |cc| {
                PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
                app.session.rooms.status = status;
                app.ui.komaphone.enabled = true;
                app.ui.rooms.open = true;
                app.ui.rooms.display_name = "Test artist".into();
                app
            });
        harness.run_steps(16);
        let image = harness.render().map_err(std::io::Error::other)?;
        let path = std::path::Path::new(&output).join(format!("{name}.png"));
        image.save(&path)?;
        println!("Layout fixture: {}", path.display());
    }
    Ok(())
}
