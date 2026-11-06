use egui::{Event, PointerButton, Pos2, Rect, Vec2};
use photocraft_engine::Session;
use photocraft_ui_egui::{PhotocraftApp, komaphone_workspace};

#[test]
fn room_is_mouse_accessible_and_invitation_is_not_in_state_or_debug() {
    let mut app = PhotocraftApp::new(Session::new(), Default::default());
    app.ui.komaphone.enabled = true;
    let ctx = egui::Context::default();
    PhotocraftApp::setup_context(&ctx, app.ui.theme);
    let frame = |app: &mut PhotocraftApp, events: Vec<Event>| {
        let mut output =
            ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 720.0))), events, ..Default::default() }, |ui| {
                komaphone_workspace::chrome(app, ui);
                egui::CentralPanel::default().show(ui, |ui| {
                    komaphone_workspace::home(app, ui);
                });
            });
        output.textures_delta.clear();
    };
    frame(&mut app, vec![]);
    frame(&mut app, vec![]);
    let rect = komaphone_workspace::control_rect(&ctx, "workspace.room").unwrap();
    assert!(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 720.0)).contains_rect(rect));
    let pos = rect.center();
    frame(&mut app, vec![Event::PointerMoved(pos), Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Default::default() }]);
    frame(&mut app, vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Default::default() }]);
    assert!(app.ui.rooms.open);
    app.ui.rooms.open = false;
    frame(&mut app, vec![]);
    let pos = komaphone_workspace::control_rect(&ctx, "home.room").unwrap().center();
    frame(&mut app, vec![Event::PointerMoved(pos), Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Default::default() }]);
    frame(&mut app, vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Default::default() }]);
    assert!(app.ui.rooms.open, "The empty workspace offers the same room entry point");
    app.ui.rooms.invitation = "abcdef0123456789".repeat(4);
    let state = serde_json::to_string(&app.ui).unwrap();
    let debug = format!("{:?}", app.ui);
    assert!(!state.contains(&app.ui.rooms.invitation));
    assert!(!debug.contains(&app.ui.rooms.invitation));
}
