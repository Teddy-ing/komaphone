use super::{Camera, Draft, Pan, PhotocraftApp, Rect, Viewport, active_world_tool, json, run, scene, sync_world_tool};
use scene::coordinates;
#[cfg(test)]
mod tests;

fn stroke_seed(id: scene::StrokeId) -> u64 {
    id.as_str().get(..16).and_then(|hex| u64::from_str_radix(hex, 16).ok()).filter(|&n| n != 0).unwrap_or(1)
}

fn pen_sample(pen: crate::stylus::PenSample, time_ms: f64) -> scene::brush::StrokeSample {
    scene::brush::StrokeSample {
        pressure: pen.pressure,
        tilt_x: pen.tilt_x,
        tilt_y: pen.tilt_y,
        rotation: pen.rotation,
        wheel: 1.0,
        time_ms: if time_ms.is_finite() && time_ms >= 0.0 { time_ms } else { 0.0 },
    }
}

fn brush_wants_time(b: &scene::brush::StrokeBrush) -> bool {
    b.build_up || !b.spacing_enabled || b.smoothing.as_ref().is_some_and(|sm| sm.catch_up && !sm.pulled_string)
}

fn samples_needed(brush: Option<&scene::brush::StrokeBrush>, samples: &[scene::brush::StrokeSample]) -> bool {
    brush.is_some_and(|b| b.build_up || !b.spacing_enabled || b.smoothing.is_some())
        || samples.iter().any(|s| {
            (s.pressure - 1.0).abs() > 1e-4 || s.tilt_x.abs() > 1e-4 || s.tilt_y.abs() > 1e-4 || s.rotation.abs() > 1e-4 || (s.wheel - 1.0).abs() > 1e-4
        })
}

fn begin_world_draft(
    app: &mut PhotocraftApp,
    account: photocraft_engine::worlds::Account,
    current: &Viewport,
    screen: [f64; 2],
    now: f64,
    pen: crate::stylus::PenSample,
) -> Result<Draft, String> {
    sync_world_tool(app);
    let tool = active_world_tool(app);
    let mut brush = app.session.tools.brush.clone();
    let _ = app.session.load_brush_tips(&mut brush);
    brush.color = app.session.tools.foreground;
    brush.background = app.session.tools.background;
    match tool {
        crate::Tool::Pencil => {
            brush.aliased = true;
            brush.hardness = 1.0;
            brush.flow = 1.0;
            brush.erase = crate::paint_mouse::clears(app, tool);
            if brush.erase {
                brush.behind = false;
                brush.mode = photocraft_color::BlendMode::Normal;
            }
        }
        crate::Tool::Eraser => {
            brush.erase = true;
            brush.behind = false;
            brush.mode = photocraft_color::BlendMode::Normal;
        }
        _ => {
            brush.erase = crate::paint_mouse::clears(app, tool);
            if brush.erase {
                brush.behind = false;
                brush.mode = photocraft_color::BlendMode::Normal;
            }
        }
    }
    let id = photocraft_engine::worlds::new_stroke_id().map_err(|e| e.to_string())?;
    brush.seed = stroke_seed(id);
    let width = if app.ui.world.tools.lock_width {
        app.ui.world.world_locked_width.map_or(f64::from(brush.size) / current.camera.zoom, |(depth, local)| {
            local * 100.0_f64.powi(i32::from(current.camera.region.depth) - i32::from(depth))
        })
    } else {
        f64::from(brush.size) / current.camera.zoom
    };
    if !width.is_finite() || width <= 0.0 || width > 2048.0 {
        return Err("This thickness exceeds the canvas stroke limit at this scale. Unlock thickness, reduce the brush size, or zoom out.".into());
    }
    let start = coordinates::screen_to_point(current, screen).map_err(|error| error.to_string())?;
    let anchor = coordinates::target(current.camera.canvas, start, current.camera.region.depth, current.camera.zoom).map_err(|error| error.to_string())?.region;
    let mut draft_stroke = crate::komaphone_canvas::DraftStroke {
        viewport: current.clone(),
        anchor,
        points: Vec::new(),
        width,
        color: scene::Srgba { rgba: [brush.color[0], brush.color[1], brush.color[2], brush.opacity.clamp(0.0, 1.0)] },
        erase: brush.erase,
        warning: None,
    };
    crate::komaphone_canvas::append_draft(&mut draft_stroke, screen)?;
    let smoothing_zoom = if current.camera.zoom.is_finite() && current.camera.zoom > 0.0 { current.camera.zoom as f32 } else { 1.0 };
    let stroke_brush = Box::new(photocraft_engine::world_brush::to_stroke_brush(&brush, smoothing_zoom, false).map_err(|e| e.to_string())?);
    let preview_brush = Box::new(photocraft_engine::world_brush::to_stroke_brush(&brush, smoothing_zoom, true).map_err(|e| e.to_string())?);
    Ok(Draft {
        account,
        id,
        stroke: draft_stroke,
        brush: Some(stroke_brush),
        preview_brush: Some(preview_brush),
        samples: vec![pen_sample(pen, 0.0)],
        start_time: now,
        last_screen: screen,
        last_tick_time: now,
        drawing: true,
        submitted: false,
    })
}

pub(super) fn pointer(app: &mut PhotocraftApp, ui: &mut egui::Ui, rect: Rect, response: &egui::Response, view: &Viewport) {
    let (events, now) = ui.input(|i| (i.events.clone(), i.time));
    app.stylus.update_for_frame(app.frame, &events);
    let pen = app.stylus.sample().unwrap_or_default();
    let mut space = events
        .iter()
        .find_map(|event| match event {
            egui::Event::Key { key: egui::Key::Space, pressed, repeat: false, .. } => Some(!pressed),
            _ => None,
        })
        .unwrap_or_else(|| ui.input(|i| i.key_down(egui::Key::Space)));
    let reviewing = app.session.worlds.active.as_ref().is_some_and(|a| a.viewer.review.as_ref().is_some_and(|r| r.interval.is_some()));
    let mut camera = view.camera.clone();
    let mut moved_draft = false;
    let screen = |position: egui::Pos2| [f64::from(position.x - rect.left()), f64::from(position.y - rect.top())];
    for event in events {
        match event {
            egui::Event::Key { key: egui::Key::Space, pressed, .. } => space = pressed,
            egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed: true, .. }
                if !app.ui.world.tools.pan
                    && !reviewing
                    && app.ui.world.draft.is_none()
                    && app.ui.world.pan.is_none()
                    && response.enabled()
                    && rect.intersect(ui.clip_rect()).contains(pos)
                    && ui.ctx().layer_id_at(pos) == Some(response.layer_id) =>
            {
                sync_world_tool(app);
                crate::brush_picker::open(&mut app.ui, ui.ctx(), [pos.x, pos.y]);
            }
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
                    match begin_world_draft(app, account, &current, screen(pos), now, pen) {
                        Ok(draft) => {
                            app.ui.world.draft = Some(Box::new(draft));
                            moved_draft = true;
                        }
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
            egui::Event::PointerMoved(pos) => {
                moved_draft |= move_to(app, screen(pos), now, pen, &mut camera);
            }
            egui::Event::PointerButton { pos, button, pressed: false, .. } => {
                if app.ui.world.pan.as_ref().is_some_and(|pan| pan.button == button) {
                    move_to(app, screen(pos), now, pen, &mut camera);
                    app.ui.world.pan = None;
                }
                if button == egui::PointerButton::Primary && app.ui.world.draft.as_ref().is_some_and(|d| d.drawing) {
                    move_to(app, screen(pos), now, pen, &mut camera);
                    finish(app);
                    moved_draft = true;
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
    if !moved_draft
        && let Some(draft) = app.ui.world.draft.as_mut()
        && draft.drawing
        && draft.brush.as_deref().is_some_and(brush_wants_time)
        && app.session.worlds.account.as_ref() == Some(&draft.account)
        && app.session.worlds.active.as_ref().is_some_and(|a| a.info.canvas == draft.stroke.viewport.camera.canvas)
    {
        if ((now - draft.last_tick_time) * 1000.0) >= 16.0
            && draft.stroke.points.len() < scene::MAX_POINTS
            && let Some(&last_point) = draft.stroke.points.last()
        {
            let min_time = draft.samples.last().map_or(0.0, |s| s.time_ms);
            let mut s = pen_sample(pen, ((now - draft.start_time).max(0.0)) * 1000.0);
            s.time_ms = s.time_ms.max(min_time);
            draft.stroke.points.push(last_point);
            draft.samples.push(s);
            draft.last_tick_time = now;
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
    }
    if camera != view.camera {
        run(app, "komaphone.world.camera", json!({"camera":camera}));
    }
    presence(app, ui, rect, &Viewport { camera, ..view.clone() });
}
fn move_to(app: &mut PhotocraftApp, point: [f64; 2], now: f64, pen: crate::stylus::PenSample, camera: &mut Camera) -> bool {
    let mut appended = false;
    if let Some(draft) = app.ui.world.draft.as_mut()
        && draft.drawing
    {
        if app.session.worlds.account.as_ref() != Some(&draft.account)
            || app.session.worlds.active.as_ref().is_none_or(|a| a.info.canvas != draft.stroke.viewport.camera.canvas)
        {
            draft.drawing = false;
            app.ui.world.message = Some("Return to the original account and canvas to save this unfinished stroke.".into());
            return false;
        }
        let prev_len = draft.stroke.points.len();
        if let Err(error) = crate::komaphone_canvas::append_draft(&mut draft.stroke, point) {
            draft.stroke.warning = Some(error.clone());
            app.ui.world.message = Some(error);
        } else if draft.stroke.points.len() > prev_len {
            let min_time = draft.samples.last().map_or(0.0, |s| s.time_ms);
            let mut s = pen_sample(pen, ((now - draft.start_time).max(0.0)) * 1000.0);
            s.time_ms = s.time_ms.max(min_time);
            draft.samples.push(s);
            draft.last_screen = point;
            draft.last_tick_time = now;
            appended = true;
        }
    }
    if let Some(pan) = &app.ui.world.pan {
        match coordinates::pan(&pan.camera, [point[0] - pan.start[0], point[1] - pan.start[1]]) {
            Ok(value) => *camera = value,
            Err(error) => app.ui.world.message = Some(error.to_string()),
        }
    }
    appended
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
    let attach_samples = draft.samples.len() == stroke.points.len() && samples_needed(draft.brush.as_deref(), &draft.samples);
    let mut payload = json!({
        "id": draft.id,
        "canvas": stroke.viewport.camera.canvas,
        "anchor": stroke.anchor,
        "points": stroke.points,
        "width": stroke.width,
        "color": stroke.color,
        "erase": stroke.erase,
    });
    if let Some(brush) = &draft.brush {
        payload["brush"] = json!(brush);
    }
    if attach_samples {
        payload["samples"] = json!(draft.samples);
    }
    draft.submitted = run(app, "komaphone.world.stroke", payload);
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
        let include_samples = draft.samples.len() == points.len() && samples_needed(draft.preview_brush.as_deref(), &draft.samples);
        let mut preview_samples = if include_samples { Some(Vec::new()) } else { None };
        let count = points.len().min(128);
        for n in 0..count {
            let index = if count <= 1 { 0 } else { n.saturating_mul(points.len().saturating_sub(1)) / (count - 1) };
            if let Some(point) = points.get(index) {
                preview.push(*point);
                if let (Some(dest), Some(sample)) = (preview_samples.as_mut(), draft.samples.get(index)) {
                    dest.push(*sample);
                }
            }
        }
        style = Some(scene::world_protocol::PreviewStyle {
            id: draft.id,
            anchor: draft.stroke.anchor,
            width: draft.stroke.width,
            color: draft.stroke.color,
            erase: draft.stroke.erase,
            brush: draft.preview_brush.clone(),
            samples: preview_samples,
        });
    }
    let _ = app.run("komaphone.world.presence", json!({"camera":view.camera,"preview":preview,"cursor":cursor,"stroke":style}));
}
