use super::{PhotocraftApp, Rect, Tokens, Viewport, scene};
use photocraft_engine::{
    scene::{
        shared::{SharedStroke, media::MediaFrame},
        spatial::{WorldRect, select_target_lod, visible_tiles},
    },
    world_brush::LiveTileBrush,
    world_media::{WorldRenderer, decode_frame},
    world_tiles::{RgbaFrame, ScopeKey, TileBudget, WorldTileManager},
};
use std::sync::Arc;
mod output;
mod preparation;
use output::OutputCache;
use preparation::{Key, Revision, Scope, Worker};

#[derive(Default)]
pub struct Presentation {
    activation: u64,
    key: Option<Key>,
    ready: Option<Key>,
    worker: Option<Worker>,
    prepare_error: Option<String>,
    renderer: Option<WorldRenderer>,
    gpu: Option<Box<crate::komaphone_world_gpu::WorldGpu>>,
    health: Option<photocraft_gpu::DeviceHealth>,
    tile_manager: Option<WorldTileManager>,
    cpu_presenter: crate::komaphone_world_gpu::CpuTilePresenter,
    output: OutputCache,
    texture: Option<egui::TextureId>,
    live_tile: Option<Box<(scene::StrokeId, LiveTileBrush)>>,
    smoothing_string: Option<([f64; 2], [f64; 2])>,
    pub render_ms: f64,
    pub preparing: bool,
    pub rendered_revision: Option<u64>,
    pub stats: crate::komaphone_world_gpu::Stats,
}

impl Presentation {
    fn can_render(&self, wanted: &Key) -> bool {
        self.renderer.is_some() && self.ready.as_ref().is_some_and(|ready| ready == wanted || ready.may_show_before(wanted))
    }

    #[cfg(test)]
    fn install(&mut self, renderer: WorldRenderer, key: Key) {
        self.install_with_dirty(renderer, key, None);
    }

    fn install_with_dirty(&mut self, renderer: WorldRenderer, key: Key, dirty_rects: Option<&[WorldRect]>) {
        // Equal revision numbers can still represent different optimistic
        // sources. Replacing a source always invalidates its completed output cache.
        self.output.clear();
        self.rendered_revision = Some(renderer.world().revision);
        let review_end = match key.revision {
            Revision::Live { .. } => None,
            Revision::Review { end, .. } => Some(end),
        };
        let scope_key = ScopeKey::new(
            key.scope.activation,
            &key.scope.account.endpoint,
            key.scope.account.profile.as_str(),
            key.scope.account.device.as_str(),
            key.scope.canvas,
            review_end,
        );
        if let Some(mgr) = self.tile_manager.as_mut() {
            let same_scope = self.ready.as_ref().is_some_and(|prev| {
                let prev_end = match prev.revision {
                    Revision::Live { .. } => None,
                    Revision::Review { end, .. } => Some(end),
                };
                prev.scope == key.scope && prev_end == review_end
            });
            if same_scope {
                mgr.update_scene(Arc::clone(renderer.index()), dirty_rects);
            } else {
                mgr.set_scope(scope_key, Arc::clone(renderer.index()));
            }
        }
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
        if let Some(mgr) = app.world_render.tile_manager.as_mut() {
            mgr.purge_scope();
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
    paint_overlays(app, ui, &painter, rect, view, &presentation);
    app.world_render = presentation;
}

fn paint_overlays(app: &mut PhotocraftApp, ui: &egui::Ui, painter: &egui::Painter, rect: Rect, view: &Viewport, presentation: &Presentation) {
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
    if let Some(([bx, by], [px, py])) = presentation.smoothing_string {
        let a = rect.min + egui::vec2(bx as f32, by as f32);
        let b = rect.min + egui::vec2(px as f32, py as f32);
        painter.line_segment([a, b], egui::Stroke::new(1.5, tokens.accent));
        painter.circle_stroke(a, 3.5, egui::Stroke::new(1.5, tokens.accent));
    }
    if presentation.preparing || presentation.stats.pending_assets > 0 {
        let message = if presentation.preparing { "Preparing the selected canvas revision…" } else { "Loading shared media…" };
        painter.text(rect.min + egui::vec2(12.0, 12.0), egui::Align2::LEFT_TOP, message, egui::FontId::proportional(13.0), tokens.text);
    }
}

fn tile_cache_dir_from_env() -> Option<std::path::PathBuf> {
    std::env::var_os("KOMAPHONE_DATA_DIR").filter(|v| !v.is_empty()).map(|d| std::path::PathBuf::from(d).join("tile-cache"))
}

fn paint_inner(app: &mut PhotocraftApp, ui: &egui::Ui, rect: Rect, view: &Viewport, p: &mut Presentation) -> Result<Option<egui::TextureId>, String> {
    let active = app.session.worlds.active.as_ref().ok_or_else(|| "Open a shared canvas".to_owned())?;
    let account = app.session.worlds.account.clone().ok_or_else(|| "Connect to this canvas's account".to_owned())?;
    let interval = active.viewer.review.as_ref().and_then(|r| r.interval.as_ref());
    let cursor = interval.map(|interval| interval.position);
    let review_end = interval.map(|interval| interval.end_inclusive);
    let revision = interval.map_or_else(
        || Revision::Live { canonical: active.canonical.revision, display: active.display.revision, pending: active.pending },
        |interval| Revision::Review { end: interval.end_inclusive, position: interval.position },
    );
    let key = Key { scope: Scope { activation: app.session.worlds.activation, account, canvas: active.info.canvas }, revision };

    if p.key.as_ref().is_some_and(|previous| previous.scope != key.scope) {
        if let Some(worker) = &p.worker {
            worker.invalidate();
        }
        if let Some(mgr) = p.tile_manager.as_mut() {
            mgr.purge_scope();
        }
        if let Some(gpu) = p.gpu.as_mut() {
            gpu.purge_slots();
        }
        p.renderer = None;
        p.output.clear();
        p.texture = None;
        p.live_tile = None;
        p.smoothing_string = None;
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
                p.install_with_dirty(renderer, completed.key, completed.dirty_rects.as_deref());
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
    if !p.can_render(&key) {
        return Ok(None);
    }

    // Lazily initialize GPU presenter when `gpu_render_state` is present; otherwise use CPU tile fallback.
    if p.gpu.is_none()
        && let Some(rs) = app.gpu_render_state.as_ref()
    {
        let health = app
            .window_device
            .as_ref()
            .map(|(_, health)| health.clone())
            .or_else(|| app.gpu_health())
            .ok_or_else(|| "The shared canvas GPU is unavailable".to_owned())?;
        p.output.clear();
        p.gpu = Some(Box::new(crate::komaphone_world_gpu::WorldGpu::new(rs, health.clone())?));
        p.health = Some(health);
    }
    if let Some(fault) = p.health.as_ref().and_then(photocraft_gpu::DeviceHealth::fault) {
        p.output.clear();
        return Err(fault.to_string());
    }

    let budget = p.gpu.as_ref().map_or_else(TileBudget::conservative, |g| g.budget());
    if p.tile_manager.is_none() {
        let mut mgr = WorldTileManager::new(budget, tile_cache_dir_from_env());
        if let (Some(ready), Some(renderer)) = (p.ready.as_ref(), p.renderer.as_ref()) {
            let r_end = match ready.revision {
                Revision::Live { .. } => None,
                Revision::Review { end, .. } => Some(end),
            };
            let sk = ScopeKey::new(
                ready.scope.activation,
                &ready.scope.account.endpoint,
                ready.scope.account.profile.as_str(),
                ready.scope.account.device.as_str(),
                ready.scope.canvas,
                r_end,
            );
            mgr.set_scope(sk, Arc::clone(renderer.index()));
        }
        p.tile_manager = Some(mgr);
    }

    let renderer = p.renderer.as_mut().ok_or_else(|| "Canvas renderer unavailable".to_owned())?;
    let elapsed_ms = ui.input(|input| (input.time.max(0.0) * 1000.0) as u64);
    let (frames, animated) = renderer.media_frames(view, elapsed_ms).map_err(|e| e.to_string())?;
    for frame in &frames {
        app.session.world_media_bytes(frame).map_err(|e| e.to_string())?;
        if let Some(mgr) = p.tile_manager.as_mut()
            && !mgr.has_media_frame(&frame.hash)
            && let Some(bytes) = app.session.worlds.assets.bytes(frame).map_err(|e| e.to_string())?
            && let Ok(decoded) = decode_frame(frame, &bytes)
        {
            mgr.register_media_frame(frame.hash, Arc::new(RgbaFrame::from(decoded)));
        }
    }

    let (previews, local_brush_draft) = collect_previews(app, view, cursor.is_some());
    let density = ui.ctx().pixels_per_point();
    let target_lod = match p.gpu.as_ref() {
        Some(gpu) => gpu.target_lod(view, density)?,
        None => select_target_lod(view, density, budget.max_visible_tiles, None).map_err(|e| e.to_string())?,
    };

    p.smoothing_string = None;
    if let Some((draft_id, drawing, draft_stroke)) = local_brush_draft {
        let reuse = p.live_tile.as_deref().is_some_and(|(id, live)| *id == draft_id && live.lod() == target_lod);
        if reuse {
            if let Some((_, live)) = p.live_tile.as_deref_mut() {
                live.sync_points(&draft_stroke.points, draft_stroke.samples.as_deref()).map_err(|e| e.to_string())?;
            }
        } else {
            let live = LiveTileBrush::new(&draft_stroke, target_lod).map_err(|e| e.to_string())?;
            p.live_tile = Some(Box::new((draft_id, live)));
        }
        if let Some((_, live)) = p.live_tile.as_deref()
            && drawing
        {
            p.smoothing_string = live.smoothing_string(view);
        }
    } else {
        p.live_tile = None;
    }

    // Guided tour playback lookahead prefetch (`t + 250 ms`).
    let lookahead_view = tour_lookahead_viewport(app, view);

    let mgr = p.tile_manager.as_mut().ok_or_else(|| "Canvas tile manager unavailable".to_owned())?;
    let retain_stale_for_review = review_end.is_some();

    let mut resolve_media = |frame: &MediaFrame| -> photocraft_engine::Result<Option<Arc<RgbaFrame>>> {
        let Some(bytes) = app.session.worlds.assets.bytes(frame)? else {
            return Ok(None);
        };
        let decoded = decode_frame(frame, &bytes)?;
        Ok(Some(Arc::new(RgbaFrame::from(decoded))))
    };

    let texture = if let Some(gpu) = p.gpu.as_mut() {
        let live_arg = p.live_tile.as_deref_mut().map(|(_, lb)| lb);
        let tex = gpu.present_interactive(mgr, view, density, elapsed_ms, &previews, live_arg, &mut resolve_media, retain_stale_for_review)?;
        p.stats = gpu.stats.clone();
        tex
    } else {
        present_cpu_fallback(
            ui.ctx(),
            &mut p.cpu_presenter,
            mgr,
            view,
            density,
            elapsed_ms,
            &previews,
            p.live_tile.as_deref_mut().map(|(_, lb)| lb),
            &mut resolve_media,
            &mut p.stats,
        )?
    };

    if let Some(la_view) = lookahead_view
        && !p.stats.is_refining
        && let Ok(la_lod) = select_target_lod(&la_view, density, budget.max_visible_tiles, None)
        && let Ok(la_tiles) = visible_tiles(&la_view, la_lod, budget.max_visible_tiles)
    {
        mgr.schedule_view(&la_view, la_lod, &la_tiles, &[]);
    }

    // Ordinary live browsing may show temporary fallback while tiles settle.
    // Pixel evidence binds ONLY the exact pinned scene once all target & animated tiles have settled.
    if p.ready.as_ref() == Some(&key) && !p.stats.is_refining && p.stats.anim_pending_tiles == 0 {
        crate::world_probe::rendered(app, ui, rect, view, &previews);
    }
    if p.stats.is_refining || p.stats.anim_pending_tiles > 0 {
        ui.ctx().request_repaint();
    } else if animated || !previews.is_empty() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
    }
    Ok(Some(texture))
}

fn collect_previews(app: &PhotocraftApp, view: &Viewport, in_review: bool) -> (Vec<SharedStroke>, Option<(scene::StrokeId, bool, scene::Stroke)>) {
    let mut previews = Vec::new();
    let mut local_brush_draft = None;
    if in_review {
        return (previews, local_brush_draft);
    }
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
                    brush: style.brush.clone(),
                    samples: style.samples.clone(),
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
        let draft_stroke = scene::Stroke {
            id: draft.id,
            canvas: view.camera.canvas,
            owner: draft.account.profile,
            revision: 0,
            anchor: s.anchor,
            points: s.points.clone(),
            width: s.width,
            color: s.color,
            erase: s.erase,
            brush: draft.brush.clone(),
            samples: (draft.samples.len() == s.points.len()).then(|| draft.samples.clone()),
        };
        if draft_stroke.brush.is_some() {
            local_brush_draft = Some((draft.id, draft.drawing, draft_stroke.clone()));
        }
        previews.push(SharedStroke {
            stroke: draft_stroke,
            device: draft.account.device,
            provenance: None,
            visible: true,
            created_revision: 0,
            changed_revision: 0,
        });
    }
    (previews, local_brush_draft)
}

fn tour_lookahead_viewport(app: &PhotocraftApp, current: &Viewport) -> Option<Viewport> {
    let playback = app.session.worlds.tour_playback.as_ref().filter(|p| p.playing)?;
    let active = app.session.worlds.active.as_ref()?;
    let tour = active.display.tours.get(&playback.tour)?;
    let frame = tour.frame_at(playback.elapsed_ms.saturating_add(250)).ok()?;
    Some(Viewport { camera: frame.camera, width: current.width, height: current.height })
}

#[allow(clippy::too_many_arguments)]
fn present_cpu_fallback(
    ctx: &egui::Context,
    presenter: &mut crate::komaphone_world_gpu::CpuTilePresenter,
    mgr: &mut WorldTileManager,
    view: &Viewport,
    density: f32,
    elapsed_ms: u64,
    previews: &[SharedStroke],
    mut live_tile_brush: Option<&mut LiveTileBrush>,
    resolve_media: &mut dyn FnMut(&MediaFrame) -> photocraft_engine::Result<Option<Arc<RgbaFrame>>>,
    stats: &mut crate::komaphone_world_gpu::Stats,
) -> Result<egui::TextureId, String> {
    let _ = mgr.poll_completed();
    let budget = mgr.budget();
    let scope_gen = mgr.scope_gen();
    let edit_seq = mgr.edit_seq();
    let target_lod = select_target_lod(view, density, budget.max_visible_tiles, None).map_err(|e| e.to_string())?;
    let visible = visible_tiles(view, target_lod, budget.max_visible_tiles).map_err(|e| e.to_string())?;
    if let Some(scene) = mgr.scene().cloned()
        && let Ok(vrect) = scene::spatial::viewport_world_rect(view)
    {
        let mut to_reg = Vec::new();
        for id in scene.query_world_rect_media(vrect) {
            if let Some(shared) = scene.world().media.get(&id)
                && shared.media.frames.len() > 1
                && let Ok((_, mf)) = shared.media.frame_at(elapsed_ms)
                && !mgr.has_media_frame(&mf.hash)
                && !to_reg.iter().any(|f: &MediaFrame| f.hash == mf.hash)
            {
                to_reg.push(mf.clone());
            }
        }
        for mf in to_reg {
            if let Some(decoded) = resolve_media(&mf).map_err(|e| e.to_string())? {
                mgr.register_media_frame(mf.hash, decoded);
            }
        }
    }
    let dynamic_items = crate::komaphone_world_gpu::collect_dynamic_items(mgr, view, previews, elapsed_ms)?;
    let preview_rects = crate::komaphone_world_gpu::collect_preview_device_rects(mgr, previews)?;
    let live_id = live_tile_brush.as_deref().map(LiveTileBrush::stroke_id);
    let u0 = scene::spatial::TILE_APRON as f32 / scene::spatial::TILE_PHYSICAL as f32;
    let u1 = (scene::spatial::TILE_APRON + scene::spatial::TILE_LOGICAL) as f32 / scene::spatial::TILE_PHYSICAL as f32;
    let mut owned_tiles: Vec<(WorldRect, [f32; 4], Arc<Vec<u8>>)> = Vec::with_capacity(visible.len());
    let mut static_visible = Vec::with_capacity(visible.len());
    let mut active_async = std::collections::BTreeSet::new();
    let mut async_jobs = Vec::new();
    let mut active_warm = std::collections::BTreeSet::new();
    let mut urgent_warm: Vec<photocraft_engine::world_tiles::SplitWarmJob> = Vec::new();
    let mut pending = 0usize;
    let mut anim_pending = 0usize;
    let mut missing = Vec::new();
    for &tile in &visible {
        let phys = tile.physical_rect();
        let has_local_brush = live_id.is_some_and(|lsid| dynamic_items.previews.iter().any(|(r, (sid, _))| *sid == lsid && r.intersects(&phys)));
        if has_local_brush && let Some(lsid) = live_id {
            let live_r = match live_tile_brush.as_deref_mut() {
                Some(lb) => {
                    lb.ensure_lod(tile.lod);
                    lb.raster_for_tile(tile).map_err(|e| e.to_string())?.map(|(_, r)| (lsid, r))
                }
                None => None,
            };
            let has_anim = dynamic_items.anim_media.iter().any(|(r, _)| r.intersects(&phys));
            let bg_sig = if has_anim { crate::komaphone_world_gpu::compute_bg_anim_tile_sig(scope_gen, edit_seq, Some(lsid), phys, &dynamic_items) } else { 0 };
            let batch =
                mgr.render_live_tiles_nonblocking(&[(tile, bg_sig, has_anim, live_r)], lsid, elapsed_ms, previews, resolve_media).map_err(|e| e.to_string())?;
            if let Some((_, res)) = batch.into_iter().next() {
                if res.used_stale_anim_cache {
                    active_async.insert(tile);
                    async_jobs.push((tile, bg_sig, elapsed_ms));
                    anim_pending += 1;
                }
                if res.used_cold_split_fallback {
                    let devs = crate::komaphone_world_gpu::tile_split_devices_for(phys, &preview_rects, None);
                    if !devs.is_empty() {
                        active_warm.insert(tile);
                        let wsig = crate::komaphone_world_gpu::compute_warm_sig(scope_gen, edit_seq, tile, bg_sig, &devs);
                        urgent_warm.push((tile, wsig, elapsed_ms, devs));
                        anim_pending += 1;
                    }
                }
                owned_tiles.push((tile.logical_rect(), [u0, u0, u1, u1], Arc::new(res.rgba8)));
            }
            continue;
        }
        let has_peer_or_anim =
            dynamic_items.previews.iter().any(|(r, _)| r.intersects(&phys)) || dynamic_items.anim_media.iter().any(|(r, _)| r.intersects(&phys));
        if has_peer_or_anim {
            active_async.insert(tile);
            let dyn_sig = crate::komaphone_world_gpu::compute_dynamic_tile_sig(scope_gen, edit_seq, None, 0, phys, &dynamic_items);
            if let Some(hit) = mgr.get_valid_dynamic_tile(tile, dyn_sig) {
                owned_tiles.push((tile.logical_rect(), [u0, u0, u1, u1], hit.rgba8));
            } else {
                async_jobs.push((tile, dyn_sig, elapsed_ms));
                anim_pending += 1;
                if let Some(prev) = mgr.get_any_valid_dynamic_tile(tile) {
                    owned_tiles.push((tile.logical_rect(), [u0, u0, u1, u1], prev.rgba8));
                } else if let Some(stat) = mgr.get_valid_cpu_tile(tile) {
                    owned_tiles.push((tile.logical_rect(), [u0, u0, u1, u1], stat.rgba8));
                    pending += 1;
                } else {
                    pending += 1;
                    missing.push(tile);
                }
            }
            continue;
        }
        static_visible.push(tile);
        if let Some(hit) = mgr.get_valid_cpu_tile(tile) {
            owned_tiles.push((tile.logical_rect(), [u0, u0, u1, u1], hit.rgba8));
        } else {
            pending += 1;
            missing.push(tile);
        }
    }
    if let Some(lsid) = live_id {
        let peer_only: Vec<SharedStroke> = previews.iter().filter(|p| p.stroke.id != lsid).cloned().collect();
        mgr.schedule_dynamic_tiles(active_async, &async_jobs, &peer_only);
        mgr.schedule_split_warm_tiles(active_warm, &urgent_warm, &[]);
    } else {
        mgr.schedule_dynamic_tiles(active_async, &async_jobs, previews);
        mgr.schedule_split_warm_tiles(std::collections::BTreeSet::new(), &[], &[]);
    }
    mgr.schedule_view(view, target_lod, &static_visible, &missing);
    let refs: Vec<(WorldRect, [f32; 4], &[u8])> = owned_tiles.iter().map(|(r, uv, b)| (*r, *uv, b.as_slice())).collect();
    let tex = presenter.present_tiles(ctx, view, density, &refs)?;
    *stats = crate::komaphone_world_gpu::Stats {
        visible_tiles: visible.len(),
        pending_tiles: pending,
        anim_pending_tiles: anim_pending,
        target_lod,
        is_refining: pending > 0,
        ..Default::default()
    };
    Ok(tex)
}
