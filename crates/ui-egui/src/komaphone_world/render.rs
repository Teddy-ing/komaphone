use super::{PhotocraftApp, Rect, Tokens, Viewport, scene};
use photocraft_engine::{scene::shared::SharedStroke, world_media::WorldRenderer};
use std::sync::Arc;
mod output;
mod preparation;
use output::{OutputCache, OutputKey};
use preparation::{Key, Revision, Scope, Worker};

#[derive(Default)]
pub struct Presentation {
    activation: u64,
    key: Option<Key>,
    ready: Option<Key>,
    worker: Option<Worker>,
    prepare_error: Option<String>,
    renderer: Option<WorldRenderer>,
    gpu: Option<crate::komaphone_world_gpu::WorldGpu>,
    health: Option<photocraft_gpu::DeviceHealth>,
    output: OutputCache,
    texture: Option<egui::TextureId>,
    pub render_ms: f64,
    pub preparing: bool,
    pub rendered_revision: Option<u64>,
    pub stats: crate::komaphone_world_gpu::Stats,
}
impl Presentation {
    fn can_render(&self, wanted: &Key) -> bool {
        self.renderer.is_some() && self.ready.as_ref().is_some_and(|ready| ready == wanted || ready.may_show_before(wanted))
    }
    fn install(&mut self, renderer: WorldRenderer, key: Key) {
        // Equal revision numbers can still represent different optimistic
        // sources. Replacing a source always invalidates its completed image.
        self.output.clear();
        self.rendered_revision = Some(renderer.world().revision);
        self.renderer = Some(renderer);
        self.ready = Some(key);
    }
}
pub(super) fn prune(app: &mut PhotocraftApp) {
    if app.world_render.activation != app.session.worlds.activation {
        let worker = app.world_render.worker.take();
        if let Some(worker) = &worker {
            worker.invalidate();
        }
        app.world_render = Presentation { activation: app.session.worlds.activation, worker, ..Default::default() };
    }
}
pub(super) fn paint(app: &mut PhotocraftApp, ui: &egui::Ui, rect: Rect, view: &Viewport) {
    prune(app);
    let mut presentation = std::mem::take(&mut app.world_render);
    let start = web_time::Instant::now();
    let result = paint_inner(app, ui, rect, view, &mut presentation);
    presentation.render_ms = start.elapsed().as_secs_f64() * 1000.0;
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::WHITE);
    match result {
        Ok(Some(texture)) => {
            presentation.texture = Some(texture);
            painter.image(texture, rect, Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        }
        Ok(None) => {}
        Err(error) => {
            app.ui.world.message = Some(error);
        }
    }
    let tokens = Tokens::get(ui.ctx());
    if let Some(active) = app.session.worlds.active.as_ref()
        && active.viewer.review.as_ref().is_some_and(|review| review.interval.is_some())
        && let Some(stroke) = app.ui.world.highlights.stroke.and_then(|id| active.canonical.strokes.get(&id))
        && let Ok(Some(projected)) = scene::render::project(&stroke.stroke, view)
    {
        for path in projected.paths {
            let points = path.into_iter().map(|[x, y]| rect.min + egui::vec2(x, y)).collect::<Vec<_>>();
            painter.add(egui::Shape::line(points, egui::Stroke::new(2.0, tokens.accent)));
        }
    }
    if let Some(active) = app.session.worlds.active.as_ref()
        && active.viewer.review.as_ref().is_some_and(|review| review.interval.is_some())
        && let Some(shared) = app.ui.world.highlights.media.and_then(|id| active.canonical.media.get(&id))
    {
        let media = &shared.media;
        let corner = |local| scene::coordinates::point(&media.anchor, local).and_then(|point| scene::coordinates::point_to_screen(view, point));
        if let (Ok(min), Ok(max)) = (corner(media.offset), corner([media.offset[0] + media.width, media.offset[1] + media.height])) {
            let position = |[x, y]: [f64; 2]| rect.min + egui::vec2(x.clamp(-1_000_000., 1_000_000.) as f32, y.clamp(-1_000_000., 1_000_000.) as f32);
            painter.rect_stroke(Rect::from_min_max(position(min), position(max)), 0.0, egui::Stroke::new(2.0, tokens.accent), egui::StrokeKind::Middle);
        }
    }
    for peer in &app.session.worlds.peers {
        if peer.camera.canvas != view.camera.canvas || app.session.worlds.account.as_ref().is_some_and(|account| account.device == peer.device) {
            continue;
        }
        if let Some(cursor) = peer.cursor
            && let Ok([x, y]) = scene::coordinates::point_to_screen(view, cursor)
        {
            let position = rect.min + egui::vec2(x as f32, y as f32);
            if rect.contains(position) {
                painter.circle_stroke(position, 5.0, egui::Stroke::new(1.5, tokens.accent));
                let label = super::panels::display_name(app, peer.profile);
                let label = painter.layout_no_wrap(label.to_owned(), egui::FontId::proportional(12.0), tokens.text);
                let at = position + egui::vec2(8.0, -8.0 - label.size().y);
                let backing = Rect::from_min_size(at, label.size()).expand(3.0);
                app.world_probe.obscured(Rect::from_center_size(position, egui::vec2(16.0, 16.0)));
                app.world_probe.obscured(backing);
                painter.rect_filled(backing, 3.0, tokens.card);
                painter.rect_stroke(backing, 3.0, egui::Stroke::new(1.0, tokens.card_border), egui::StrokeKind::Inside);
                painter.galley(at, label, tokens.text);
            }
        }
    }
    if presentation.preparing || presentation.stats.pending_assets > 0 {
        let message = if presentation.preparing { "Preparing the selected canvas revision…" } else { "Loading shared media…" };
        painter.text(rect.min + egui::vec2(12.0, 12.0), egui::Align2::LEFT_TOP, message, egui::FontId::proportional(13.0), tokens.text);
    }
    app.world_render = presentation;
}
fn paint_inner(app: &mut PhotocraftApp, ui: &egui::Ui, rect: Rect, view: &Viewport, p: &mut Presentation) -> Result<Option<egui::TextureId>, String> {
    let active = app.session.worlds.active.as_ref().ok_or_else(|| "Open a shared canvas".to_owned())?;
    let account = app.session.worlds.account.clone().ok_or_else(|| "Connect to this canvas's account".to_owned())?;
    let interval = active.viewer.review.as_ref().and_then(|r| r.interval.as_ref());
    let cursor = interval.map(|interval| interval.position);
    let revision = interval.map_or_else(
        || Revision::Live { canonical: active.canonical.revision, display: active.display.revision, pending: active.pending },
        |interval| Revision::Review { end: interval.end_inclusive, position: interval.position },
    );
    let key = Key { scope: Scope { activation: app.session.worlds.activation, account, canvas: active.info.canvas }, revision };
    if p.key.as_ref().is_some_and(|previous| previous.scope != key.scope) {
        if let Some(worker) = &p.worker {
            worker.invalidate();
        }
        p.renderer = None;
        p.gpu = None;
        p.health = None;
        p.output.clear();
        p.texture = None;
        p.ready = None;
        p.rendered_revision = None;
        p.stats = Default::default();
    }
    if p.worker.is_none() {
        p.worker = Some(Worker::start()?);
    }
    if p.key.as_ref() != Some(&key) {
        let source = if cursor.is_some() { Arc::clone(&active.canonical) } else { Arc::clone(&active.display) };
        p.worker.as_ref().ok_or_else(|| "Canvas preparation worker unavailable".to_owned())?.request(key.clone(), source)?;
        p.key = Some(key.clone());
        p.prepare_error = None;
        p.preparing = true;
    }
    if let Some(completed) = p.worker.as_ref().and_then(Worker::take)
        && (completed.key == key || completed.key.may_show_before(&key))
    {
        p.preparing = completed.key != key;
        match completed.result {
            Ok(renderer) => {
                p.install(renderer, completed.key);
            }
            Err(error) => p.prepare_error = Some(error),
        }
    }
    if let Some(error) = &p.prepare_error {
        return Err(error.clone());
    }
    if p.preparing {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
    }
    // A historical cursor is exact: never substitute the current live scene or
    // another review position while its CPU projection is still being prepared.
    if !p.can_render(&key) {
        return Ok(None);
    }
    if p.gpu.is_none() {
        let rs = app.gpu_render_state.as_ref().ok_or_else(|| "Shared drawing requires the native GPU renderer. Saved artwork is unchanged.".to_owned())?;
        let health = app
            .window_device
            .as_ref()
            .map(|(_, health)| health.clone())
            .or_else(|| app.gpu_health())
            .ok_or_else(|| "The shared canvas GPU is unavailable".to_owned())?;
        p.output.clear();
        p.gpu = Some(crate::komaphone_world_gpu::WorldGpu::new(rs, health.clone())?);
        p.health = Some(health);
    }
    if let Some(fault) = p.health.as_ref().and_then(photocraft_gpu::DeviceHealth::fault) {
        p.output.clear();
        return Err(fault.to_string());
    }
    let ready = p.ready.clone().ok_or_else(|| "Canvas render source unavailable".to_owned())?;
    let renderer = p.renderer.as_mut().ok_or_else(|| "Canvas renderer unavailable".to_owned())?;
    let elapsed_ms = ui.input(|input| (input.time.max(0.0) * 1000.0) as u64);
    let (frames, animated) = renderer.media_frames(view, elapsed_ms).map_err(|e| e.to_string())?;
    for frame in &frames {
        app.session.world_media_bytes(frame).map_err(|e| e.to_string())?;
    }
    let mut previews = Vec::new();
    if cursor.is_none() {
        for peer in &app.session.worlds.peers {
            if peer.camera.canvas != view.camera.canvas || app.session.worlds.account.as_ref().is_some_and(|a| a.device == peer.device) {
                continue;
            }
            if let Some(style) = &peer.stroke
                && !peer.preview.is_empty()
            {
                previews.push(SharedStroke {
                    stroke: scene::Stroke {
                        id: style.id,
                        canvas: view.camera.canvas,
                        owner: peer.profile,
                        revision: 0,
                        anchor: style.anchor,
                        points: peer.preview.clone(),
                        width: style.width,
                        color: style.color,
                        erase: style.erase,
                    },
                    device: peer.device,
                    provenance: None,
                    visible: true,
                    created_revision: 0,
                    changed_revision: 0,
                });
            }
        }
        if let Some(draft) = &app.ui.world.draft
            && app.session.worlds.account.as_ref() == Some(&draft.account)
            && draft.stroke.viewport.camera.canvas == view.camera.canvas
        {
            let s = &draft.stroke;
            previews.push(SharedStroke {
                stroke: scene::Stroke {
                    id: draft.id,
                    canvas: view.camera.canvas,
                    owner: draft.account.profile,
                    revision: 0,
                    anchor: s.anchor,
                    points: s.points.clone(),
                    width: s.width,
                    color: s.color,
                    erase: s.erase,
                },
                device: draft.account.device,
                provenance: None,
                visible: true,
                created_revision: 0,
                changed_revision: 0,
            });
        }
    }
    let gpu = p.gpu.as_mut().ok_or_else(|| "Canvas GPU unavailable".to_owned())?;
    let density = ui.ctx().pixels_per_point();
    let output_key = OutputKey::new(ready, view.clone(), density, frames);
    let reusable = previews.is_empty() && p.stats.pending_assets == 0;
    let (texture, stats) = p.output.render(output_key, reusable, || {
        let texture = gpu.render_available(
            renderer,
            view,
            density,
            elapsed_ms,
            &mut |frame| app.session.worlds.assets.bytes(frame).map(|bytes| bytes.map(|b| b.as_ref().clone())),
            &previews,
        )?;
        Ok((texture, gpu.stats.clone()))
    })?;
    p.stats = stats;
    // Ordinary live browsing may show an older projection while preparation
    // catches up. Pixel evidence must bind the exact pinned scene instead.
    if p.ready.as_ref() == Some(&key) {
        crate::world_probe::rendered(app, ui, rect, view, &previews);
    }
    if animated || !previews.is_empty() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
    }
    Ok(Some(texture))
}
