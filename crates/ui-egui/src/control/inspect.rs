use super::*;

/// The open canvas context menu's rows for `ui.state`: the spelling suggestions first (type edit
/// menu over a misspelled word), then the menu's own rows.
fn context_entries(app: &PhotocraftApp, menu: &crate::canvas_tool_menu::CanvasToolMenu) -> Vec<Value> {
    use crate::canvas_tool_menu::{crop_rows, entry_enabled, rows, spelling_rows};
    let spelling = spelling_rows(menu);
    let mut entries: Vec<Value> = spelling.iter().map(|(label, id)| json!({"label": label, "id": id, "enabled": entry_enabled(app, menu, id)})).collect();
    if !spelling.is_empty() {
        entries.push(json!({"separator": true}));
    }
    entries.extend(crop_rows(menu).iter().map(|row| match row {
        Some((label, id)) => json!({"label": label, "id": id, "enabled": entry_enabled(app, menu, id)}),
        None => json!({"separator": true}),
    }));
    entries.extend(rows(menu).iter().map(|row| match row {
        Some((label, id)) => json!({"label": label, "id": id, "enabled": entry_enabled(app, menu, id)}),
        None => json!({"separator": true}),
    }));
    entries
}

/// Snapshot of everything on screen, addressable by id.
pub fn inspect(app: &PhotocraftApp, ctx: &egui::Context) -> Value {
    with_world_hash(app, ctx, true)
}

/// Frame probes may omit the full retained-history digest. This never changes
/// the scene or the default inspection response's exact canonical hash.
pub(super) fn with_world_hash(app: &PhotocraftApp, ctx: &egui::Context, world_hash: bool) -> Value {
    let screen = ctx.content_rect();
    let mut dialogs: Vec<Value> =
        app.ui.dialogs.iter().map(|d| json!({"id": d.id, "kind": d.kind, "title": crate::dialogs::title(d), "fields": d.fields})).collect();
    if let Some((kind, fields)) = &app.ui.shell.dialog {
        dialogs.push(json!({"id": SHELL_DIALOG, "kind": kind, "title": crate::workspace_ui::title(kind), "fields": fields}));
    }
    dialogs.extend(crate::discard_ui::inspect(app));
    let mut room_controls = [
        "home.room",
        "workspace.room",
        "workspace.gallery",
        "home.gallery",
        "home.canvases",
        "workspace.canvases",
        "home.worlds",
        "workspace.worlds",
        "world.local",
        "world.return",
        "world.name",
        "world.invitation",
        "world.invitation.copy",
        "world.invitation.accept",
        "world.section.accept",
        "world.section.people",
        "world.people.refresh",
        "world.section.review",
        "world.section.tours",
        "world.section.media",
        "world.section.restore",
        "world.restore.path",
        "komaphone.world.restore",
        "world.section.bookmarks",
        "world.section.recovery",
        "world.section.trash",
        "komaphone.world.trash",
        "komaphone.world.delete",
        "komaphone.world.stroke.paste",
        "world.export.path",
        "world.export.png",
        "world.export.gif",
        "world.export.sequence",
        "world.export.archive",
        "gallery.world",
        "gallery.worldStill",
        "world.tool.brush",
        "world.tool.pencil",
        "world.tool.eraser",
        "world.tool.pan",
        "world.width",
        "world.opacity",
        "world.flow",
        "world.hardness",
        "world.smoothing",
        "world.brush.presets",
        "world.brush.settings",
        "world.color",
        "world.color.bg",
        "world.color.swap",
        "world.colors.panel",
        "world.lockWidth",
        "world.drawing",
        "world.review.scrub",
        "world.review.speed",
        "komaphone.world.create",
        "komaphone.world.list",
        "komaphone.world.rename",
        "komaphone.world.invite",
        "komaphone.world.remove",
        "komaphone.world.leave",
        "komaphone.world.home",
        "komaphone.world.undo",
        "komaphone.world.redo",
        "komaphone.world.bookmark",
        "komaphone.world.go",
        "komaphone.world.bookmark.remove",
        "komaphone.world.review.begin",
        "komaphone.world.review.control",
        "komaphone.world.review.markSeen",
        "komaphone.world.tour.put",
        "komaphone.world.tour.play",
        "komaphone.world.tour.remove",
        "komaphone.world.tour.control",
        "komaphone.world.export",
        "komaphone.world.export.cancel",
        "komaphone.world.remix.prepare",
        "komaphone.world.remix.place",
        "komaphone.world.remix.copy",
        "komaphone.world.remix.paste",
        "canvas.return",
        "canvas.shared",
        "canvas.name",
        "canvas.rename",
        "canvas.bookmarkName",
        "canvas.tool.brush",
        "canvas.tool.eraser",
        "canvas.tool.pan",
        "canvas.width",
        "canvas.lockWidth",
        "canvas.drawing",
        "canvas.retryStroke",
        "canvas.close.keep",
        "canvas.close.discard",
        "canvas.close.retry",
        "canvas.recoverStroke",
        "canvas.color",
        "canvas.flight.pause",
        "canvas.flight.resume",
        "canvas.flight.cancel",
        "komaphone.canvas.create",
        "komaphone.canvas.rename",
        "komaphone.canvas.bookmark",
        "komaphone.canvas.home",
        "komaphone.canvas.undo",
        "komaphone.canvas.redo",
        "komaphone.canvas.retry",
        "gallery.finishedMode",
        "gallery.storage",
        "gallery.storagePath",
        "gallery.restoreDestination",
        "gallery.draw",
        "gallery.rename",
        "gallery.trash",
        "gallery.remixStill",
        "gallery.favorites",
        "gallery.filter",
        "gallery.trashFilter",
        "gallery.source",
        "gallery.continue",
        "gallery.kind",
        "gallery.notes",
        "gallery.gifPreview",
        "gallery.replaySpeed",
        "komaphone.gallery.import",
        "komaphone.gallery.list",
        "komaphone.gallery.update",
        "komaphone.gallery.history",
        "komaphone.gallery.cancelDownload",
        "komaphone.gallery.backup",
        "komaphone.gallery.restore",
        "komaphone.gallery.download",
        "gallery.replayMode",
        "gallery.animationPlay",
        "gallery.replayPlay",
        "gallery.replayStep",
        "gallery.frame",
        "gallery.fps",
        "gallery.loop",
        "gallery.path",
        "gallery.name",
        "gallery.export.gif",
        "gallery.export.png",
        "gallery.export.sequence",
        "gallery.export.archive",
        "komaphone.gallery.remix",
        "komaphone.room.connect",
        "komaphone.room.create",
        "komaphone.room.invite",
        "komaphone.room.claim",
        "komaphone.room.leave",
        "komaphone.room.retry",
        "komaphone.room.retainRejected",
        "komaphone.room.dismissError",
        "komaphone.account.signIn",
        "komaphone.account.signOut",
        "komaphone.room.ready",
        "komaphone.room.start",
        "komaphone.room.openDraft",
        "komaphone.room.submit",
        "workspace.return",
        "komaphone.room.join",
        "komaphone.room.archive",
        "komaphone.room.next",
        "komaphone.room.resume",
        "komaphone.room.skip",
        "komaphone.room.end",
        "komaphone.room.note",
        "komaphone.animation.reference",
        "komaphone.animation.onion",
        "komaphone.animation.onionOpacity",
        "komaphone.animation.copyPrevious",
        "komaphone.animation.chain.0",
        "komaphone.animation.chain.1",
        "komaphone.animation.chain.2",
        "komaphone.animation.play",
        "komaphone.animation.loop",
        "komaphone.animation.speed",
        "komaphone.animation.frame",
        "komaphone.animation.notes",
        "corpse.chain.0",
        "corpse.chain.1",
        "corpse.chain.2",
        "corpse.reveal.head",
        "corpse.reveal.torso",
        "corpse.reveal.full",
    ]
    .into_iter()
    .filter_map(|id| crate::komaphone_workspace::control_rect(ctx, id).map(|rect| (id.to_owned(), json!([rect.min.x, rect.min.y, rect.max.x, rect.max.y]))))
    .collect::<serde_json::Map<String, Value>>();
    for item in &app.session.gallery.items {
        for id in [format!("gallery.item.{}", item.id.as_str()), format!("gallery.item.{}.{}", item.id.as_str(), if item.draft { "draft" } else { "original" })]
        {
            if let Some(rect) = crate::komaphone_workspace::control_rect(ctx, &id) {
                room_controls.insert(id, json!([rect.min.x, rect.min.y, rect.max.x, rect.max.y]));
            }
        }
    }
    for game in &app.session.gallery.remote {
        let id = format!("gallery.remote.{}", game.game.as_str());
        if let Some(rect) = crate::komaphone_workspace::control_rect(ctx, &id) {
            room_controls.insert(id, json!([rect.min.x, rect.min.y, rect.max.x, rect.max.y]));
        }
    }
    let canvas_ids = app.session.canvases.items.iter().map(|item| format!("canvas.open.{}", item.id.as_str())).chain(
        app.session
            .canvases
            .active
            .iter()
            .flat_map(|a| a.bookmarks.iter().enumerate().flat_map(|(i, _)| [format!("canvas.bookmark.{i}.go"), format!("canvas.bookmark.{i}.play")])),
    );
    for id in canvas_ids {
        if let Some(rect) = crate::komaphone_workspace::control_rect(ctx, &id) {
            room_controls.insert(id, json!([rect.min.x, rect.min.y, rect.max.x, rect.max.y]));
        }
    }
    for item in &app.session.worlds.items {
        for id in [format!("world.open.{}", item.canvas.as_str()), format!("world.thumbnail.{}", item.canvas.as_str())] {
            if let Some(rect) = crate::komaphone_workspace::control_rect(ctx, &id) {
                room_controls.insert(id, json!([rect.min.x, rect.min.y, rect.max.x, rect.max.y]));
            }
        }
    }
    for item in &app.session.worlds.trash {
        let id = format!("world.undelete.{}", item.canvas.as_str());
        if let Some(rect) = crate::komaphone_workspace::control_rect(ctx, &id) {
            room_controls.insert(id, json!([rect.min.x, rect.min.y, rect.max.x, rect.max.y]));
        }
    }
    if let Some(active) = &app.session.worlds.active {
        for stroke in active.canonical.strokes.keys() {
            let id = format!("world.stroke.copy.{}", stroke.as_str());
            if let Some(rect) = crate::komaphone_workspace::control_rect(ctx, &id) {
                room_controls.insert(id, json!([rect.min.x, rect.min.y, rect.max.x, rect.max.y]));
            }
        }
    }
    // Explicit inspection only, never the paint loop. Keep sampled metadata bounded;
    // invitation capabilities and local credential material are deliberately absent.
    let world_scene = app.session.worlds.active.as_ref().map(|active| {
        json!({
            "canvas":active.info.canvas,"revision":active.canonical.revision,
            "canonicalHash":world_hash.then(|| active.canonical.canonical_hash().ok()).flatten(),
            "hashIncluded":world_hash,"strokeCount":active.display.strokes.len(),
            "sampleLimit":128,"strokes":active.display.strokes.values().take(128).map(|stroke|json!({
                "id":stroke.stroke.id,"owner":stroke.stroke.owner,"device":stroke.device,"samples":stroke.stroke.points.len(),
                "visible":stroke.visible,"createdRevision":stroke.created_revision,"changedRevision":stroke.changed_revision,
                "erase":stroke.stroke.erase,"provenance":stroke.provenance
            })).collect::<Vec<_>>()
        })
    });
    let mut world_ui = json!(app.ui.world);
    world_ui["frameSamples"] = json!(app.ui.world.tools.frame_samples);
    #[cfg(not(target_arch = "wasm32"))]
    let world_render = json!({"render_ms":app.world_render.render_ms,"preparing":app.world_render.preparing,"rendered_revision":app.world_render.rendered_revision,"stats":app.world_render.stats});
    #[cfg(target_arch = "wasm32")]
    let world_render = Value::Null;
    json!({
        "window": {"width": screen.width(), "height": screen.height(), "pixelsPerPoint": ctx.pixels_per_point()},
        "scratch": photocraft_raster::spill::stats(),
        "memory": photocraft_raster::memory::stats(),
        "tool": app.ui.tool,
        "komaphone": app.ui.komaphone,
        "gallery": app.ui.gallery,
        "canvases": app.ui.canvas,
        "world": world_ui,
        "worldRender": world_render,
        "worldScene": world_scene,
        "canvasFrameSamples": app.ui.canvas.frame_samples,
        "rooms": app.ui.rooms,
        "roomStatus": app.session.rooms.status,
        "roomImageCache": crate::komaphone_game_view::cached_hashes(ctx),
        "roomControls": room_controls,
        "toolOptions": app.ui.tool_options,
        "perspectiveCrop": app.ui.perspective_crop,
        "strokeEditor": app.ui.stroke_editor,
        "magnetic": app.ui.magnetic,
        "textEdit": app.ui.text_edit,
        // The paragraph box's overflow flag (set by the overlay each frame): agents
        // learn the overflow without a screen; never persisted (serde skip). Gated on the
        // session: the flag is a per-frame snapshot, and an `ui.inspect` running between the
        // frame that ended the edit and the one that resets the flag would otherwise report
        // overflow beside `"textEdit": null`.
        "typeBoxOverflow": app.ui.text_edit.is_some() && app.ui.type_box_overflow,
        "typeTransform": app.ui.type_transform,
        "camTransform": app.ui.cam_transform,
        "layerMenu": app.ui.layer_menu,
        "layerPicker": app.ui.layer_picker,
        "brushModeMenu": app.ui.brush_mode_menu.map(|pos| {
            use crate::brush_mode_menu as m;
            let tool = app.ui.tool;
            let current = m::current(app, tool);
            json!({
                "pos": pos,
                "tool": tool,
                "mode": current.and_then(m::Item::label),
                "choices": m::labels(tool),
                "items": m::items(tool).into_iter().map(|i| match i.label() {
                    Some(label) => json!({"label": label, "enabled": m::enabled(app, i), "checked": Some(i) == current}),
                    None => json!({"separator": true}),
                }).collect::<Vec<_>>(),
            })
        }),
        "brushPicker": app.ui.brush_picker.map(|pos| json!({
            "pos": pos,
            // The picker's content size once its corner grip was dragged (null: the default).
            "size": app.ui.brush_picker_size,
            "list": {
                "collapsed": app.ui.brush_picker_list.collapsed,
                "filter": app.ui.brush_picker_list.filter,
                "renaming": app.ui.brush_picker_list.renaming,
                // The card's parts (`view` is the Brushes panel's listing and doesn't apply).
                "showName": app.ui.brush_picker_list.show_name,
                "showStroke": app.ui.brush_picker_list.show_stroke,
                "showTip": app.ui.brush_picker_list.show_tip,
                // The footer slider: the cards' width scale.
                "scale": app.ui.brush_picker_list.scale,
            },
        })),
        "canvasToolMenu": app.ui.canvas_tool_menu.as_ref().map(|menu| {
            json!({
                "pos": menu.pos,
                "tool": menu.tool,
                "pasteboard": menu.pasteboard,
                "guide": menu.guide,
                "crop": menu.crop.is_some(),
                "entries": context_entries(app, menu)
            })
        }),
        "panels": app.ui.panels,
        "dockTabs": app.ui.dock_tabs,
        "dock": app.ui.dock,
        "docking": app.ui.docking,
        "dockingReturns": app.ui.docking_returns,
        "dockingCollapsed": app.ui.docking_collapsed,
        "view": app.ui.view,
        "views": app.ui.views,
        "dialogs": dialogs,
        "windows": app.ui.windows,
        "theme": app.ui.theme,
        "status": app.ui.status,
        "statusError": app.ui.status_error,
        "notices": app.ui.notices,
        "updatesOpen": app.ui.updates_open,
        "updates": {"supported": app.session.updates.check.is_some(), "currentVersion": photocraft_engine::build_info::VERSION, "result": app.session.updates.status},
        "gpuFallbackNotice": app.ui.gpu_fallback_notice,
        "kysOffer": app.ui.kys_offer,
        "frame": app.frame,
        "session": photocraft_engine::inspect::session(&app.session),
        "document": app.session.active().map(photocraft_engine::inspect::document),
        "perf": {
            "fps": app.fps,
            "timings": app.perf,
            "strokeTrace": app.stroke_trace.last,
            "renderPath": crate::render_path::inspect(app),
            "slowFrames": app.slow_frames,
        },
        "brush": {"size": app.session.tools.brush.size, "hardness": app.session.tools.brush.hardness, "opacity": app.session.tools.brush.opacity},
        "distort": app.distort.describe(),
        "jobs": crate::jobs_ui::inspect(app),
        "histogram": crate::histogram_panel::inspect(app),
        "cameraRaw": app.camera_raw.as_ref().map(|d| d.describe(&app.ui.camera_raw_scope, &app.ui.camera_raw_preview)),
    })
}

pub fn save_screenshot(app: &mut PhotocraftApp, image: &egui::ColorImage, path: Option<&str>) -> Value {
    let [w, h] = image.size;
    let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
    save_screenshot_rgba(app, w, h, &rgba, path)
}

/// Encode already-flattened RGBA pixels. The capture lifecycle uses this after dropping the
/// retained [`egui::ColorImage`], so the encoder's input copy and the retained image do not
/// overlap in memory (see `PhotocraftApp::finish_captures`).
pub fn save_screenshot_rgba(app: &mut PhotocraftApp, w: usize, h: usize, rgba: &[u8], path: Option<&str>) -> Value {
    let png = match app.services.encode_png.as_ref() {
        Some(enc) => enc(w as u32, h as u32, rgba),
        None => Err("no PNG encoder configured".into()),
    };
    let Some(path) = path else {
        return match png {
            Ok(bytes) => json!({
                "ok": true,
                "result": {
                    "base64": base64::engine::general_purpose::STANDARD.encode(bytes),
                    "mime": "image/png",
                    "width": w,
                    "height": h,
                }
            }),
            Err(error) => json!({"ok": false, "error": error}),
        };
    };
    let result = png.and_then(|bytes| match app.services.automation_write.as_mut() {
        Some(write) => write(path, &bytes),
        None => Err("automation write authority is not configured".into()),
    });
    match result {
        Ok(()) => json!({"ok": true, "result": {"path": path, "width": w, "height": h}}),
        Err(e) => json!({"ok": false, "error": e}),
    }
}
