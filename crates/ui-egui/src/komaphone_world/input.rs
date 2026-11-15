use super::{Camera, Draft, Pan, PhotocraftApp, Rect, Viewport, json, run, scene};
use scene::coordinates;
#[cfg(test)]
mod tests;

pub(super) fn pointer(app: &mut PhotocraftApp, ui: &mut egui::Ui, rect: Rect, response: &egui::Response, view: &Viewport) {
    let events = ui.input(|i| i.events.clone());
    let mut space = events
        .iter()
        .find_map(|event| match event {
            egui::Event::Key { key: egui::Key::Space, pressed, repeat: false, .. } => Some(!pressed),
            _ => None,
        })
        .unwrap_or_else(|| ui.input(|i| i.key_down(egui::Key::Space)));
    let reviewing = app.session.worlds.active.as_ref().is_some_and(|a| a.viewer.review.as_ref().is_some_and(|r| r.interval.is_some()));
    let mut camera = view.camera.clone();
    let screen = |position: egui::Pos2| [f64::from(position.x - rect.left()), f64::from(position.y - rect.top())];
    for event in events {
        match event {
            egui::Event::Key { key: egui::Key::Space, pressed, .. } => space = pressed,
            egui::Event::PointerButton { pos, button, pressed: true, .. }
                if matches!(button, egui::PointerButton::Primary | egui::PointerButton::Middle)
                    && app.ui.world.draft.is_none()
                    && app.ui.world.pan.is_none()
                    && response.enabled()
                    && rect.intersect(ui.clip_rect()).contains(pos)
                    && ui.ctx().layer_id_at(pos) == Some(response.layer_id) =>
            {
                if app.ui.world.tools.pan || space || button == egui::PointerButton::Middle {
                    app.ui.world.pan = Some(Pan { camera: camera.clone(), start: screen(pos), button });
                } else if !reviewing && let Some(account) = app.session.worlds.account.clone() {
                    let current = Viewport { camera: camera.clone(), ..view.clone() };
                    match crate::komaphone_canvas::begin_draft(&app.ui.world.tools, &current, screen(pos)).and_then(|stroke| {
                        photocraft_engine::worlds::new_stroke_id()
                            .map(|id| Draft { account, id, stroke, drawing: true, submitted: false })
                            .map_err(|e| e.to_string())
                    }) {
                        Ok(draft) => app.ui.world.draft = Some(draft),
                        Err(error) => app.ui.world.message = Some(error),
                    }
                }
                if app.session.worlds.tour_playback.is_some() {
                    run(app, "komaphone.world.tour.control", json!({"state":"cancel"}));
                }
                if app.session.worlds.flight.is_some() {
                    run(app, "komaphone.world.flight", json!({"state":"cancel"}));
                }
            }
            egui::Event::PointerMoved(pos) => move_to(app, screen(pos), &mut camera),
            egui::Event::PointerButton { pos, button, pressed: false, .. } => {
                if app.ui.world.pan.as_ref().is_some_and(|pan| pan.button == button) {
                    move_to(app, screen(pos), &mut camera);
                    app.ui.world.pan = None;
                }
                if button == egui::PointerButton::Primary && app.ui.world.draft.as_ref().is_some_and(|d| d.drawing) {
                    move_to(app, screen(pos), &mut camera);
                    finish(app);
                }
            }
            egui::Event::PointerGone | egui::Event::WindowFocused(false) => {
                app.ui.world.pan = None;
                finish(app);
            }
            egui::Event::MouseWheel { unit, delta, modifiers, .. } if response.hovered() && app.ui.world.draft.is_none() && app.ui.world.pan.is_none() => {
                if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                    let factor = match unit {
                        egui::MouseWheelUnit::Point => 1.0,
                        egui::MouseWheelUnit::Line => 40.0,
                        egui::MouseWheelUnit::Page => f64::from(view.height),
                    };
                    let result = if modifiers.alt {
                        coordinates::pan(&camera, [f64::from(delta.x) * factor, f64::from(delta.y) * factor])
                    } else {
                        coordinates::zoom_at(
                            &Viewport { camera: camera.clone(), ..view.clone() },
                            screen(pos),
                            (f64::from(delta.y).mul_add(factor, 0.0).clamp(-4096.0, 4096.0) * 0.0025).exp(),
                        )
                    };
                    match result {
                        Ok(value) => camera = value,
                        Err(error) => app.ui.world.message = Some(error.to_string()),
                    }
                }
            }
            egui::Event::Zoom(factor) if response.hovered() && app.ui.world.draft.is_none() && app.ui.world.pan.is_none() => {
                if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
                    match coordinates::zoom_at(&Viewport { camera: camera.clone(), ..view.clone() }, screen(pos), f64::from(factor)) {
                        Ok(value) => camera = value,
                        Err(error) => app.ui.world.message = Some(error.to_string()),
                    }
                }
            }
            _ => {}
        }
    }
    if camera != view.camera {
        run(app, "komaphone.world.camera", json!({"camera":camera}));
    }
    presence(app, ui, rect, &Viewport { camera, ..view.clone() });
}
fn move_to(app: &mut PhotocraftApp, point: [f64; 2], camera: &mut Camera) {
    if let Some(draft) = app.ui.world.draft.as_mut()
        && draft.drawing
    {
        if app.session.worlds.account.as_ref() != Some(&draft.account)
            || app.session.worlds.active.as_ref().is_none_or(|a| a.info.canvas != draft.stroke.viewport.camera.canvas)
        {
            draft.drawing = false;
            app.ui.world.message = Some("Return to the original account and canvas to save this unfinished stroke.".into());
            return;
        }
        if let Err(error) = crate::komaphone_canvas::append_draft(&mut draft.stroke, point) {
            draft.stroke.warning = Some(error.clone());
            app.ui.world.message = Some(error);
        }
    }
    if let Some(pan) = &app.ui.world.pan {
        match coordinates::pan(&pan.camera, [point[0] - pan.start[0], point[1] - pan.start[1]]) {
            Ok(value) => *camera = value,
            Err(error) => app.ui.world.message = Some(error.to_string()),
        }
    }
}
pub(super) fn finish(app: &mut PhotocraftApp) {
    let Some(mut draft) = app.ui.world.draft.take() else { return };
    draft.drawing = false;
    if draft.submitted {
        app.ui.world.draft = Some(draft);
        return;
    }
    if app.session.worlds.account.as_ref() != Some(&draft.account)
        || app.session.worlds.active.as_ref().is_none_or(|a| a.info.canvas != draft.stroke.viewport.camera.canvas)
    {
        app.ui.world.message = Some("Return to the original account and canvas to save this unfinished stroke.".into());
        app.ui.world.draft = Some(draft);
        return;
    }
    let stroke = &draft.stroke;
    draft.submitted = run(
        app,
        "komaphone.world.stroke",
        json!({"id":draft.id,"canvas":stroke.viewport.camera.canvas,"anchor":stroke.anchor,"points":stroke.points,"width":stroke.width,"color":stroke.color,"erase":stroke.erase}),
    );
    if let Some(warning) = &draft.stroke.warning {
        app.ui.world.message = Some(warning.clone());
    }
    // Keep the only input copy until the worker confirms the SQLite commit.
    app.ui.world.draft = Some(draft);
}
fn presence(app: &mut PhotocraftApp, ui: &egui::Ui, rect: Rect, view: &Viewport) {
    // Sample the latest input on every real frame. The bounded native worker
    // alone controls network cadence, so a second UI timer cannot stale a draft.
    let cursor = ui
        .input(|i| i.pointer.hover_pos())
        .filter(|position| rect.contains(*position))
        .and_then(|position| coordinates::screen_to_point(view, [f64::from(position.x - rect.left()), f64::from(position.y - rect.top())]).ok());
    let mut preview = Vec::new();
    let mut style = None;
    if let Some(draft) = &app.ui.world.draft
        && app.session.worlds.account.as_ref() == Some(&draft.account)
        && draft.stroke.viewport.camera.canvas == view.camera.canvas
    {
        let points = &draft.stroke.points;
        let count = points.len().min(128);
        for n in 0..count {
            let index = if count <= 1 { 0 } else { n.saturating_mul(points.len().saturating_sub(1)) / (count - 1) };
            if let Some(point) = points.get(index) {
                preview.push(*point);
            }
        }
        style = Some(scene::world_protocol::PreviewStyle {
            id: draft.id,
            anchor: draft.stroke.anchor,
            width: draft.stroke.width,
            color: draft.stroke.color,
            erase: draft.stroke.erase,
        });
    }
    let _ = app.run("komaphone.world.presence", json!({"camera":view.camera,"preview":preview,"cursor":cursor,"stroke":style}));
}
