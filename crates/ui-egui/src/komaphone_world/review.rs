use super::{CanvasId, PhotocraftApp, StrokeId, json, remember, run, scene};
use scene::shared::{
    Change, Edit, MediaId, WorldState,
    review::{ChangePage, Interval},
};

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Highlights {
    author: Option<scene::OwnerId>,
    include_own: bool,
    key: Option<(CanvasId, u64, u64, Option<scene::OwnerId>, bool)>,
    page: Vec<Change>,
    scanned: u64,
    pub stroke: Option<StrokeId>,
    pub media: Option<MediaId>,
}
pub(super) fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui, interval: &Interval) {
    let Some(canvas) = app.session.worlds.active.as_ref().map(|a| a.info.canvas) else { return };
    let name = app.ui.world.highlights.author.map_or_else(|| "Everyone".to_owned(), |id| super::panels::credit_name(app, id));
    craft_ui::ComboBox::from_id_salt("world-review-author").selected_text(name).show_ui(ui, |ui| {
        ui.selectable_value(&mut app.ui.world.highlights.author, None, "Everyone");
        super::panels::list_rows(ui, "world-review-author-list", 1, app.session.worlds.people.len(), |ui, index| {
            if let Some(person) = app.session.worlds.people.get(index) {
                ui.selectable_value(&mut app.ui.world.highlights.author, Some(person.profile), &person.display_name);
            }
        });
        let imported = app.session.worlds.active.as_ref().and_then(|active| active.canonical.import_origin.as_ref()).map(|origin| origin.actors.clone());
        if let Some(actors) = imported {
            super::panels::list_rows(ui, "world-review-archive-authors", 1, actors.len(), |ui, index| {
                if let Some(actor) = actors.get(index) {
                    let label = super::panels::credit_name(app, actor.imported);
                    ui.selectable_value(&mut app.ui.world.highlights.author, Some(actor.imported), label);
                }
            });
        }
    });
    ui.checkbox(&mut app.ui.world.highlights.include_own, "Include my changes");
    let key = (canvas, interval.start_exclusive, interval.end_inclusive, app.ui.world.highlights.author, app.ui.world.highlights.include_own);
    if app.ui.world.highlights.key != Some(key) {
        app.ui.world.highlights.key = Some(key);
        app.ui.world.highlights.stroke = None;
        app.ui.world.highlights.media = None;
        load(app, interval.start_exclusive);
    }
    for change in app.ui.world.highlights.page.clone() {
        let author = super::panels::credit_name(app, change.author);
        let response = ui.button(format!("{author} · change {}", change.revision));
        remember(ui, &format!("world.review.change.{}", change.revision), &response);
        if response.clicked() {
            navigate(app, &change);
        }
    }
    if app.ui.world.highlights.scanned < interval.end_inclusive && ui.button("More highlights").clicked() {
        load(app, app.ui.world.highlights.scanned);
    }
    ui.small("The author filter changes highlights; playback still includes every causal edit.");
}
fn load(app: &mut PhotocraftApp, after: u64) {
    let authors = app.ui.world.highlights.author.map(|author| vec![author]);
    let result =
        app.run("komaphone.world.review.changes", json!({"after":after,"limit":128,"authors":authors,"include_own":app.ui.world.highlights.include_own}));
    match result.and_then(|value| serde_json::from_value::<ChangePage>(value).map_err(|e| e.to_string())) {
        Ok(page) => {
            app.ui.world.highlights.page = page.changes;
            app.ui.world.highlights.scanned = page.scanned_through;
        }
        Err(error) => app.ui.world.message = Some(error),
    }
}
fn navigate(app: &mut PhotocraftApp, change: &Change) {
    let Some(active) = app.session.worlds.active.as_ref() else { return };
    match target(&active.canonical, change) {
        Ok(Some(target)) => {
            app.ui.world.highlights.stroke = target.stroke;
            app.ui.world.highlights.media = target.media;
            run(app, "komaphone.world.camera", json!({"camera":target.camera}));
        }
        Ok(None) => {
            app.ui.world.highlights.stroke = None;
            app.ui.world.highlights.media = None;
        }
        Err(error) => app.ui.world.message = Some(error.to_string()),
    }
}

struct Target {
    camera: scene::Camera,
    stroke: Option<StrokeId>,
    media: Option<MediaId>,
}
/// One bounded immutable object lookup on navigation, including hidden originals.
fn target(world: &WorldState, change: &Change) -> scene::Result<Option<Target>> {
    let Some(event) = world.events_after(change.revision.saturating_sub(1), 1).map_err(|_| scene::Error::Invalid("review change"))?.first() else {
        return Ok(None);
    };
    if event.revision != change.revision || event.op.author != change.author {
        return Err(scene::Error::Invalid("review change identity"));
    }
    let (stroke, media) = match &event.op.action {
        Edit::AddStroke { stroke } | Edit::CopyStroke { stroke, .. } => (Some(stroke.id), None),
        Edit::SetStrokeVisible { stroke, .. } => (Some(*stroke), None),
        Edit::PlaceMedia { media } => (None, Some(media.id)),
        Edit::SetMediaVisible { media, .. } => (None, Some(*media)),
        _ => (None, None),
    };
    let location = if let Some(shared) = stroke.and_then(|id| world.strokes.get(&id)) {
        let body = &shared.stroke;
        let Some(first) = body.points.first() else { return Ok(None) };
        let (mut min, mut max) = (*first, *first);
        for point in &body.points {
            min[0] = min[0].min(point[0]);
            min[1] = min[1].min(point[1]);
            max[0] = max[0].max(point[0]);
            max[1] = max[1].max(point[1]);
        }
        Some((body.anchor, [(min[0] + max[0]) / 2., (min[1] + max[1]) / 2.], (max[0] - min[0]).max(max[1] - min[1]).max(body.width)))
    } else if let Some(shared) = media.and_then(|id| world.media.get(&id)) {
        let body = &shared.media;
        Some((body.anchor, [body.offset[0] + body.width / 2., body.offset[1] + body.height / 2.], body.width.max(body.height)))
    } else {
        None
    };
    let Some((region, local, extent)) = location else { return Ok(None) };
    let point = scene::coordinates::point(&region, local)?;
    let camera = scene::coordinates::target(world.canvas, point, region.depth, (500. / extent.max(1.)).clamp(0.5, 10.))?;
    Ok(Some(Target { camera, stroke, media }))
}

#[cfg(test)]
mod tests;
