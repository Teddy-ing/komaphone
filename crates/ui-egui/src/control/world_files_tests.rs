use super::*;
fn request(app: &mut PhotocraftApp, params: Value) -> Value {
    let (request, _receiver) = ControlRequest::new("app.worldFile", params);
    match handle(app, &egui::Context::default(), &request) {
        Outcome::Done(reply) => reply,
        _ => panic!("world export submission must acknowledge asynchronously"),
    }
}
#[test]
fn shared_canvas_exports_require_injected_launch_authority() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    for params in [
        json!({"command":"komaphone.world.export","params":{"format":"png","path":"synthetic.png"}}),
        json!({"command":"komaphone.world.restore","params":{"path":"synthetic-archive"}}),
    ] {
        let reply = request(&mut app, params);
        assert_eq!(reply["ok"], false);
        assert!(reply["error"].as_str().unwrap().contains("not granted"));
    }
}
#[test]
fn shared_canvas_exports_reject_claimed_capabilities_and_unrelated_commands() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    for params in [
        json!(null),
        json!({"command":42}),
        json!({"command":"komaphone.world.export","root":"C:/"}),
        json!({"command":"komaphone.world.export","trusted":true}),
        json!({"command":"file.open","params":{"path":"synthetic.png"}}),
    ] {
        let reply = request(&mut app, params);
        assert_eq!(reply["ok"], false, "{reply}");
    }
}
