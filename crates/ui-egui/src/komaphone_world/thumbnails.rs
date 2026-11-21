use super::{PhotocraftApp, Tokens, remember, scene};
use photocraft_engine::{world_thumbnails::Thumbnail, worlds::Account};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

#[derive(Clone)]
struct Texture {
    image: Arc<Thumbnail>,
    handle: egui::TextureHandle,
}
#[derive(Clone, Default)]
struct Cache {
    account: Option<Account>,
    images: BTreeMap<scene::CanvasId, Texture>,
    recent: VecDeque<scene::CanvasId>,
}
fn cache_id() -> egui::Id {
    egui::Id::unique("shared-canvas-thumbnail-textures")
}

pub(super) fn prune(app: &PhotocraftApp, ctx: &egui::Context) {
    ctx.data_mut(|data| {
        let cache = data.get_temp_mut_or_default::<Cache>(cache_id());
        if !app.ui.world.open || cache.account != app.session.worlds.account {
            *cache = Cache { account: app.session.worlds.account.clone(), ..Default::default() };
        } else {
            cache.images.retain(|canvas, _| app.session.worlds.thumbnails.contains(*canvas));
            cache.recent.retain(|canvas| cache.images.contains_key(canvas));
        }
    });
}

pub(super) fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui, canvas: scene::CanvasId) {
    let image = app.session.world_thumbnail(canvas).ok().flatten();
    let response = if let Some(image) = image.filter(|image| image.width == 160 && image.height == 100 && image.rgba.len() == 64_000) {
        if app.session.worlds.thumbnails.is_pending(canvas) {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        let cached = ui.ctx().data(|data| data.get_temp::<Cache>(cache_id()).and_then(|cache| cache.images.get(&canvas).cloned()));
        let handle = match cached {
            Some(cached) if Arc::ptr_eq(&cached.image, &image) => cached.handle,
            _ => ui.ctx().load_texture(
                format!("shared-canvas-preview-{}", canvas.as_str()),
                egui::ColorImage::from_rgba_unmultiplied([160, 100], &image.rgba),
                egui::TextureOptions::LINEAR,
            ),
        };
        ui.ctx().data_mut(|data| {
            let cache = data.get_temp_mut_or_default::<Cache>(cache_id());
            cache.images.insert(canvas, Texture { image, handle: handle.clone() });
            cache.recent.retain(|id| *id != canvas);
            cache.recent.push_back(canvas);
            while cache.recent.len() > 32 {
                if let Some(old) = cache.recent.pop_front() {
                    cache.images.remove(&old);
                }
            }
        });
        ui.add(egui::Image::new(&handle).fit_to_exact_size(egui::vec2(64.0, 40.0)))
    } else {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        let (rect, response) = ui.allocate_exact_size(egui::vec2(64.0, 40.0), egui::Sense::hover());
        let tokens = Tokens::get(ui.ctx());
        let text = if app.session.worlds.thumbnails.error(canvas).is_some() { "Unavailable" } else { "Loading…" };
        ui.painter().rect_filled(rect, tokens.radius_sm, tokens.card);
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(10.0), tokens.text_dim);
        response
    };
    remember(ui, &format!("world.thumbnail.{}", canvas.as_str()), &response);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_textures_are_removed_on_account_change_and_missing_grant() {
        let ctx = egui::Context::default();
        let canvas = scene::CanvasId::new("11111111111111111111111111111111").unwrap();
        let account = Account {
            endpoint: "https://example.invalid".into(),
            profile: scene::OwnerId::new("22222222222222222222222222222222").unwrap(),
            device: scene::shared::DeviceId::new("33333333333333333333333333333333").unwrap(),
        };
        let image = Arc::new(Thumbnail { width: 160, height: 100, rgba: vec![255; 64_000], revision: 1 });
        let handle = ctx.load_texture("private-synthetic-preview", egui::ColorImage::from_rgba_unmultiplied([160, 100], &image.rgba), Default::default());
        let cached = Cache { account: Some(account.clone()), images: BTreeMap::from([(canvas, Texture { image, handle })]), recent: VecDeque::from([canvas]) };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.ui.world.open = true;
        for same_account in [false, true] {
            ctx.data_mut(|data| data.insert_temp(cache_id(), cached.clone()));
            app.session.worlds.account = Some(account.clone());
            if !same_account {
                app.session.worlds.account.as_mut().unwrap().profile = scene::OwnerId::new("44444444444444444444444444444444").unwrap();
            }
            prune(&app, &ctx);
            let remaining = ctx.data(|data| data.get_temp::<Cache>(cache_id())).unwrap();
            assert!(remaining.images.is_empty() && remaining.recent.is_empty());
        }
    }
}
