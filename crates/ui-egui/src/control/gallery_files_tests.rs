use super::*;

fn request(app: &mut PhotocraftApp, params: Value) -> Value {
    let (req, _receiver) = ControlRequest::new("app.galleryFile", params);
    match handle(app, &egui::Context::default(), &req) {
        Outcome::Done(reply) => reply,
        _ => panic!("gallery file submission must acknowledge asynchronously"),
    }
}

#[test]
fn gallery_files_require_injected_launch_authority() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    let reply = request(&mut app, json!({"command":"komaphone.gallery.backup","params":{"path":"new-backup"}}));
    assert_eq!(reply["ok"], false);
    assert!(reply["error"].as_str().unwrap().contains("not granted"));
}

#[test]
fn gallery_files_reject_request_fields_that_could_claim_authority() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    for params in [
        json!(null),
        json!({"command":"komaphone.gallery.backup","params":{"path":"new"},"root":"C:/"}),
        json!({"command":"komaphone.gallery.backup","params":{"path":"new"},"trusted":true}),
        json!({"command":42}),
    ] {
        let reply = request(&mut app, params);
        assert_eq!(reply["ok"], false, "{reply}");
    }
}
