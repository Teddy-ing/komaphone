//! Gallery view over the engine command seam. Drawing replay and animation are separate clocks.
use crate::{PhotocraftApp, theme::Tokens};
use egui::{RichText, TextureHandle};
use photocraft_engine::gallery::GalleryItem;
use photocraft_engine::party::ProjectId;
use photocraft_engine::party::gallery::GalleryContent;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GallerySection {
    #[default]
    Saved,
    Completed,
    Temp,
    Trash,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletedFilter {
    #[default]
    All,
    Animation,
    Creature,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GalleryUi {
    pub open: bool,
    pub filter: String,
    pub section: GallerySection,
    pub completed_filter: CompletedFilter,
    pub chain: u8,
    pub frame: u8,
    pub replay_step: usize,
    pub replay_mode: bool,
    pub fps: u8,
    pub looping: bool,
    pub replay_speed: f32,
    pub name: String,
    pub export_name: String,
    pub path: String,
    pub restore_destination: String,
    pub storage_open: bool,
    pub gif_preview: bool,
    pub show_notes: bool,
    #[serde(skip)]
    export_name_source_title: Option<String>,
    #[serde(skip)]
    pub playing_animation: bool,
    #[serde(skip)]
    pub playing_replay: bool,
    #[serde(skip)]
    pub confirm_empty_trash: bool,
    #[serde(skip)]
    pub confirm_delete: Option<(ProjectId, bool)>,
    #[serde(skip)]
    last_time: f64,
    #[serde(skip)]
    accumulator: f64,
    #[serde(skip)]
    selected: Option<(ProjectId, bool)>,
    #[serde(skip)]
    requested: Option<(u8, u8, Option<usize>, bool, bool)>,
    #[serde(skip)]
    pending_playback: bool,
}
impl Default for GalleryUi {
    fn default() -> Self {
        Self {
            open: false,
            filter: String::new(),
            section: GallerySection::Saved,
            completed_filter: CompletedFilter::All,
            chain: 0,
            frame: 0,
            replay_step: 0,
            replay_mode: false,
            fps: 6,
            looping: true,
            replay_speed: 1.0,
            name: String::new(),
            export_name: String::new(),
            path: String::new(),
            restore_destination: String::new(),
            storage_open: false,
            gif_preview: false,
            show_notes: false,
            export_name_source_title: None,
            playing_animation: false,
            playing_replay: false,
            confirm_empty_trash: false,
            confirm_delete: None,
            last_time: 0.0,
            accumulator: 0.0,
            selected: None,
            requested: None,
            pending_playback: false,
        }
    }
}

/// Which Gallery section an item belongs to, or `None` when it should be hidden from the
/// Gallery list (active-game privacy guard, untouched blank canvas, completed-game round draft,
/// or a Temp copy already matching its Saved version).
pub fn item_section(app: &PhotocraftApp, item: &GalleryItem) -> Option<GallerySection> {
    if !app.session.gallery_item_allowed(item) {
        return None;
    }
    if item.trashed {
        return Some(GallerySection::Trash);
    }
    if item.draft {
        if item.untouched_blank || item.covered_by_game || item.saved_matches_head {
            return None;
        }
        return Some(GallerySection::Temp);
    }
    match item.kind.as_str() {
        "animation" | "creature" => Some(GallerySection::Completed),
        _ => Some(GallerySection::Saved),
    }
}

fn trash_origin_label(item: &GalleryItem) -> &'static str {
    if item.draft {
        "From Temp"
    } else if matches!(item.kind.as_str(), "animation" | "creature") {
        "From Completed"
    } else {
        "From Saved"
    }
}

fn item_kind_label(item: &GalleryItem) -> &'static str {
    if item.draft {
        if item.has_unsaved_edits { "Unsaved changes" } else { "Temporary draft" }
    } else {
        match item.kind.as_str() {
            "animation" => {
                if item.complete {
                    "Animation"
                } else {
                    "Animation · incomplete"
                }
            }
            "creature" => {
                if item.complete {
                    "Exquisite Corpse"
                } else {
                    "Exquisite Corpse · incomplete"
                }
            }
            _ => "Saved drawing",
        }
    }
}

fn run(app: &mut PhotocraftApp, id: &str, params: Value) {
    if let Err(error) = app.run(id, params) {
        app.ui.status = error;
    }
}
fn button(app: &mut PhotocraftApp, ui: &mut egui::Ui, label: &str, id: &str, params: Value) -> bool {
    let response = ui.add_enabled(app.session.is_enabled(id), egui::Button::new(label));
    crate::komaphone_workspace::remember(ui, id, &response);
    if response.clicked() {
        run(app, id, params);
        true
    } else {
        false
    }
}
fn remember(ui: &egui::Ui, id: &str, response: &egui::Response) {
    crate::komaphone_workspace::remember(ui, id, response);
}

pub fn open(app: &mut PhotocraftApp) {
    app.ui.gallery.open = true;
    app.ui.gallery.confirm_empty_trash = false;
    app.ui.gallery.confirm_delete = None;
    if !app.session.gallery.busy() {
        run(app, "komaphone.gallery.list", json!({}));
    }
}

#[derive(Clone)]
struct Texture {
    revision: u64,
    handle: TextureHandle,
}
#[derive(Clone, Default)]
struct TextureKeys(Vec<egui::Id>);

/// Drop private or evicted image handles, including while the gallery is closed.
pub(crate) fn prune_textures(app: &PhotocraftApp, ctx: &egui::Context) {
    let mut allowed = Vec::new();
    if app.ui.gallery.open {
        allowed.extend(app.session.gallery.thumbnails.keys().map(|(id, draft)| egui::Id::unique(("gallery-thumb", id.as_str(), *draft))));
        if app.session.gallery.active.is_some() && app.session.gallery.image.is_some() {
            allowed.push(egui::Id::unique("gallery-preview"));
        }
    }
    ctx.data_mut(|data| {
        let cache = egui::Id::unique("gallery-texture-keys");
        let mut keys = data.get_temp::<TextureKeys>(cache).unwrap_or_default();
        keys.0.retain(|key| {
            if allowed.contains(key) {
                true
            } else {
                data.remove::<Texture>(*key);
                false
            }
        });
        data.insert_temp(cache, keys);
    });
}
fn texture(ui: &egui::Ui, key: egui::Id, revision: u64, image: &photocraft_engine::rooms::media::RoomImage) -> TextureHandle {
    ui.ctx().data_mut(|data| {
        let keys = data.get_temp_mut_or_default::<TextureKeys>(egui::Id::unique("gallery-texture-keys"));
        if !keys.0.contains(&key) {
            keys.0.push(key);
        }
    });
    if let Some(texture) = ui.ctx().data(|data| data.get_temp::<Texture>(key))
        && texture.revision == revision
    {
        return texture.handle;
    }
    let handle = ui.ctx().load_texture(
        format!("gallery-{key:?}"),
        egui::ColorImage::from_rgba_unmultiplied([image.width as usize, image.height as usize], &image.rgba),
        egui::TextureOptions::LINEAR,
    );
    ui.ctx().data_mut(|data| data.insert_temp(key, Texture { revision, handle: handle.clone() }));
    handle
}

pub fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let tokens = Tokens::get(ui.ctx());
    if app.session.gallery.opened_document {
        app.session.gallery.opened_document = false;
        app.ui.gallery.open = false;
        app.ui.gallery.confirm_empty_trash = false;
        app.ui.gallery.confirm_delete = None;
        app.ui.rooms.view_reveal = false;
        return;
    }
    egui::Frame::NONE.inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.heading("Gallery");
            button(app, ui, "Refresh", "komaphone.gallery.list", json!({}));
            let response = ui.button("Import & backups");
            remember(ui, "gallery.storage", &response);
            if response.clicked() {
                app.ui.gallery.storage_open = !app.ui.gallery.storage_open;
            }
            if app.session.gallery.busy() {
                ui.spinner();
                ui.label("Working…");
            }
            if app.session.gallery.quota_bytes > 0 {
                ui.small(format!(
                    "{:.1} MiB / {:.0} MiB",
                    app.session.gallery.usage_bytes as f64 / 1_048_576.0,
                    app.session.gallery.quota_bytes as f64 / 1_048_576.0
                ));
            }
        });
        if let Some(error) = &app.session.gallery.error {
            ui.colored_label(tokens.text, error);
        }
        if let Some(notice) = &app.session.gallery.notice {
            ui.label(RichText::new(notice).color(tokens.text_dim));
        }
        if app.session.gallery.downloading {
            let ratio = if app.session.gallery.download_bytes == 0 {
                0.0
            } else {
                app.session.gallery.downloaded_bytes as f32 / app.session.gallery.download_bytes as f32
            };
            ui.add(egui::ProgressBar::new(ratio).text(if app.session.gallery.waiting {
                "Waiting for service · not yet saved"
            } else {
                "Downloading originals · not yet saved"
            }));
            button(app, ui, "Cancel download", "komaphone.gallery.cancelDownload", json!({}));
        }
        if app.ui.gallery.storage_open {
            storage(app, ui);
        }
        ui.separator();
        let height = ui.available_height().max(120.0);
        let sidebar_width = 344.0_f32.min((ui.available_width() * 0.44).max(240.0));
        let mut thumbnail = None;
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(egui::vec2(sidebar_width, height), egui::Layout::top_down(egui::Align::Min), |ui| thumbnail = list(app, ui));
            ui.separator();
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width().max(120.0), height), egui::Layout::top_down(egui::Align::Min), |ui| detail(app, ui));
        });
        tick(app, ui);
        if let Some((entry, draft)) = thumbnail
            && !app.session.gallery.busy()
        {
            run(app, "komaphone.gallery.thumbnail", json!({"entry":entry,"draft":draft}));
        }
    });
}

fn list(app: &mut PhotocraftApp, ui: &mut egui::Ui) -> Option<(ProjectId, bool)> {
    let tokens = Tokens::get(ui.ctx());
    let (mut saved_count, mut completed_count, mut temp_count, mut trash_count) = (0usize, 0usize, 0usize, 0usize);
    for item in &app.session.gallery.items {
        match item_section(app, item) {
            Some(GallerySection::Saved) => saved_count = saved_count.saturating_add(1),
            Some(GallerySection::Completed) => completed_count = completed_count.saturating_add(1),
            Some(GallerySection::Temp) => temp_count = temp_count.saturating_add(1),
            Some(GallerySection::Trash) => trash_count = trash_count.saturating_add(1),
            None => {}
        }
    }

    let tabs = [
        (GallerySection::Saved, "Saved", saved_count, "gallery.section.saved", "Drawings you explicitly saved to Gallery"),
        (GallerySection::Completed, "Completed", completed_count, "gallery.section.completed", "Finished Animation and Exquisite Corpse games"),
        (GallerySection::Temp, "Temp", temp_count, "gallery.section.temp", "Automatic local recovery drafts of unsaved work"),
        (GallerySection::Trash, "Trash", trash_count, "gallery.section.trash", "Trashed drawings and games you can restore or delete permanently"),
    ];
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (section, label, count, control_id, tooltip) in tabs {
            let text = if count > 0 { format!("{label} ({count})") } else { label.into() };
            let response = ui.add(egui::Button::selectable(app.ui.gallery.section == section, text)).on_hover_text(tooltip);
            remember(ui, control_id, &response);
            if response.clicked() && app.ui.gallery.section != section {
                app.ui.gallery.section = section;
                app.ui.gallery.confirm_empty_trash = false;
                app.ui.gallery.confirm_delete = None;
            }
        }
    });
    ui.add_space(4.0);

    let response = ui.add(egui::TextEdit::singleline(&mut app.ui.gallery.filter).desired_width(f32::INFINITY).hint_text("Search titles or authors"));
    remember(ui, "gallery.filter", &response);

    match app.ui.gallery.section {
        GallerySection::Saved => {}
        GallerySection::Completed => {
            ui.horizontal_wrapped(|ui| {
                for (filter, label, control_id) in [
                    (CompletedFilter::All, "All", "gallery.completed.all"),
                    (CompletedFilter::Animation, "Animations", "gallery.completed.animation"),
                    (CompletedFilter::Creature, "Creatures", "gallery.completed.creature"),
                ] {
                    let response = ui.selectable_value(&mut app.ui.gallery.completed_filter, filter, label);
                    remember(ui, control_id, &response);
                }
            });
        }
        GallerySection::Temp => {
            ui.label(RichText::new("Automatic local copies of unsaved drawings. Save to keep, or clear temp.").small().color(tokens.text_dim));
            let enabled = temp_count > 0 && app.session.is_enabled("komaphone.gallery.clearTemp");
            let response = ui
                .add_enabled(enabled, egui::Button::new("Clear temp"))
                .on_hover_text("Moves closed temporary drafts to Trash so you can restore or delete them permanently");
            remember(ui, "komaphone.gallery.clearTemp", &response);
            if response.clicked() {
                run(app, "komaphone.gallery.clearTemp", json!({}));
            }
        }
        GallerySection::Trash => {
            ui.label(RichText::new("Restore items anytime, or delete them permanently to free storage.").small().color(tokens.text_dim));
            if app.ui.gallery.confirm_empty_trash && trash_count > 0 {
                ui.label(RichText::new("Delete all trashed items permanently?").small());
                ui.horizontal(|ui| {
                    let enabled = app.session.is_enabled("komaphone.gallery.emptyTrash");
                    let confirm = ui.add_enabled(enabled, egui::Button::new("Confirm empty"));
                    remember(ui, "komaphone.gallery.emptyTrash", &confirm);
                    if confirm.clicked() {
                        app.ui.gallery.confirm_empty_trash = false;
                        run(app, "komaphone.gallery.emptyTrash", json!({}));
                    }
                    let cancel = ui.button("Cancel");
                    remember(ui, "gallery.emptyTrash.cancel", &cancel);
                    if cancel.clicked() {
                        app.ui.gallery.confirm_empty_trash = false;
                    }
                });
            } else {
                app.ui.gallery.confirm_empty_trash = false;
                let enabled = trash_count > 0 && app.session.is_enabled("komaphone.gallery.emptyTrash");
                let response = ui.add_enabled(enabled, egui::Button::new("Empty trash")).on_hover_text("Permanently remove all closed items in Trash");
                remember(ui, "gallery.emptyTrash", &response);
                if response.clicked() {
                    app.ui.gallery.confirm_empty_trash = true;
                }
            }
        }
    }
    ui.add_space(4.0);

    let filter = app.ui.gallery.filter.to_lowercase();
    let section = app.ui.gallery.section;
    let completed_filter = app.ui.gallery.completed_filter;
    let items: Vec<_> = app
        .session
        .gallery
        .items
        .iter()
        .filter(|item| {
            item_section(app, item) == Some(section)
                && (section != GallerySection::Completed
                    || match completed_filter {
                        CompletedFilter::All => true,
                        CompletedFilter::Animation => item.kind == "animation",
                        CompletedFilter::Creature => item.kind == "creature",
                    })
                && (filter.is_empty() || item.title.to_lowercase().contains(&filter) || item.authors.iter().any(|a| a.to_lowercase().contains(&filter)))
        })
        .cloned()
        .collect();

    let show_room_history = section == GallerySection::Completed && app.session.rooms.status.room.is_some();
    let footer_reserve = if show_room_history { 110.0 } else { 8.0 };
    let mut thumbnail = None;
    egui::ScrollArea::vertical().id_salt("gallery-list").max_height((ui.available_height() - footer_reserve).max(80.0)).show(ui, |ui| {
        if items.is_empty() {
            let empty_text = if !filter.is_empty() {
                "No items match your search in this section."
            } else {
                match section {
                    GallerySection::Saved => "No saved drawings yet. Click Save to gallery in the Workroom, or save a draft from Temp.",
                    GallerySection::Completed => "No completed games yet. Finish a Private room game and click Save to gallery at reveal.",
                    GallerySection::Temp => "No temporary drafts. Unsaved Workroom drawings appear here automatically.",
                    GallerySection::Trash => "Trash is empty.",
                }
            };
            ui.label(RichText::new(empty_text).color(tokens.text_dim));
        }
        for item in items {
            let row = ui.group(|ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    if let Some(image) = app.session.gallery.thumbnails.get(&(item.id, item.draft)) {
                        let texture = texture(ui, egui::Id::unique(("gallery-thumb", item.id.as_str(), item.draft)), item.revision, image);
                        ui.add(egui::Image::new(&texture).fit_to_exact_size(egui::vec2(58.0, 58.0)));
                    } else {
                        ui.allocate_space(egui::vec2(58.0, 58.0));
                    }
                    ui.vertical(|ui| {
                        let selected =
                            app.session.gallery.active.as_ref().is_some_and(|active| active.id == item.id) && app.session.gallery.active_draft == item.draft;
                        let response = ui.selectable_label(selected, &item.title);
                        remember(ui, &format!("gallery.item.{}.{}", item.id.as_str(), if item.draft { "draft" } else { "original" }), &response);
                        if !item.draft {
                            remember(ui, &format!("gallery.item.{}", item.id.as_str()), &response);
                        }
                        if response.clicked() && !app.session.gallery.busy() {
                            if !selected {
                                app.ui.gallery.playing_animation = false;
                                app.ui.gallery.playing_replay = false;
                                app.ui.gallery.accumulator = 0.0;
                                app.ui.gallery.confirm_delete = None;
                            }
                            run(app, "komaphone.gallery.open", json!({"entry":item.id,"draft":item.draft}));
                        }
                        if section == GallerySection::Trash {
                            ui.small(format!("{} · {}", item_kind_label(&item), trash_origin_label(&item)));
                        } else {
                            ui.small(item_kind_label(&item));
                        }
                        let authors = if item.authors.is_empty() { "You".into() } else { item.authors.join(", ") };
                        ui.small(authors);
                        let updated = crate::komaphone_time::format_compact(&item.updated_at);
                        let time_resp = ui.small(&updated.display);
                        if let Some(tooltip) = &updated.tooltip {
                            time_resp.on_hover_text(tooltip);
                        }
                        if item.has_unsaved_edits && !item.draft {
                            ui.small(RichText::new("Newer unsaved draft in Temp").color(tokens.text_dim));
                        }
                    });
                });
            });
            if thumbnail.is_none() && ui.is_rect_visible(row.response.rect) && !app.session.gallery.thumbnails.contains_key(&(item.id, item.draft)) {
                thumbnail = Some((item.id, item.draft));
            }
        }
    });
    if show_room_history {
        ui.separator();
        button(app, ui, "Previous room games", "komaphone.gallery.history", json!({}));
        for game in app.session.gallery.remote.clone() {
            let label = format!(
                "{} · {}",
                match game.mode {
                    photocraft_engine::party::Mode::Animation { .. } => "Animation",
                    _ => "Creature",
                },
                if game.complete { "finished" } else { "incomplete" }
            );
            let response = ui.add_enabled(!app.session.gallery.downloading, egui::Button::new(label));
            remember(ui, &format!("gallery.remote.{}", game.game.as_str()), &response);
            if response.clicked() {
                run(app, "komaphone.gallery.download", json!({"game":game.game}));
            }
        }
        if let Some(after_revision) = app.session.gallery.remote_cursor {
            button(app, ui, "More games", "komaphone.gallery.history", json!({"after_revision":after_revision}));
        }
    }
    thumbnail
}

fn detail(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let tokens = Tokens::get(ui.ctx());
    let Some(manifest) = app.session.gallery.active.clone() else {
        ui.heading("Keep what you make");
        match app.ui.gallery.section {
            GallerySection::Saved => {
                ui.label("Select a saved drawing to view, replay how it was drawn, reopen in the Workroom, or export.");
                ui.label(RichText::new("Use Save to gallery in the Workroom when you want to keep a drawing here.").color(tokens.text_dim));
            }
            GallerySection::Completed => {
                ui.label("Select a completed Animation or Exquisite Corpse game to play it back, inspect drawing replays, remix, or export.");
                ui.label(RichText::new("At reveal, click Save to gallery to store the finished game offline.").color(tokens.text_dim));
            }
            GallerySection::Temp => {
                ui.label("Temporary drafts keep automatic local backups of unsaved Workroom drawings.");
                ui.label(RichText::new("Select a draft to continue drawing, save it to Gallery, or move it to Trash.").color(tokens.text_dim));
            }
            GallerySection::Trash => {
                ui.label("Select a trashed item to restore it to its original section or delete it permanently.");
            }
        }
        return;
    };
    if app.ui.gallery.selected != Some((manifest.id, app.session.gallery.active_draft)) {
        app.ui.gallery.selected = Some((manifest.id, app.session.gallery.active_draft));
        app.ui.gallery.chain = 0;
        app.ui.gallery.frame = 0;
        app.ui.gallery.replay_step = 0;
        app.ui.gallery.replay_mode = false;
        app.ui.gallery.playing_animation = false;
        app.ui.gallery.playing_replay = false;
        app.ui.gallery.accumulator = 0.0;
        app.ui.gallery.show_notes = false;
        app.ui.gallery.gif_preview = false;
        app.ui.gallery.pending_playback = false;
        app.ui.gallery.confirm_delete = None;
        app.ui.gallery.fps = manifest.fps;
        app.ui.gallery.looping = manifest.looping;
        app.ui.gallery.name = manifest.title.clone();
        let suggested = photocraft_engine::gallery_exports::sanitize_default_export_name(&manifest.title);
        app.ui.gallery.export_name = suggested.clone();
        app.ui.gallery.export_name_source_title = Some(suggested);
        let is_creature_cover = matches!(
            &manifest.content,
            GalleryContent::Game { room } if room.mode == photocraft_engine::party::Mode::ExquisiteCorpse
        );
        app.ui.gallery.requested = Some((0, 0, None, is_creature_cover, false));
    } else {
        let suggested = photocraft_engine::gallery_exports::sanitize_default_export_name(&manifest.title);
        if app.ui.gallery.export_name_source_title.as_deref() == Some(app.ui.gallery.export_name.as_str()) && app.ui.gallery.export_name != suggested {
            app.ui.gallery.export_name = suggested.clone();
            app.ui.gallery.export_name_source_title = Some(suggested);
        }
    }
    let is_game = matches!(manifest.content, GalleryContent::Game { .. });
    let (chains, frames, creature) = match &manifest.content {
        GalleryContent::Game { room } => {
            (room.members.len(), room.mode.rounds(room.members.len()).unwrap_or(1), room.mode == photocraft_engine::party::Mode::ExquisiteCorpse)
        }
        GalleryContent::Project { .. } => (1, app.session.gallery.frame_count.clamp(1, 36) as u8, false),
    };
    let is_animation = !creature && frames > 1;
    let protected = app.session.active_protected_projects();
    egui::ScrollArea::vertical().id_salt("gallery-detail").show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.heading(&manifest.title);
            let item = app.session.gallery.items.iter().find(|item| item.id == manifest.id && item.draft == app.session.gallery.active_draft).cloned();
            if let Some(item) = item {
                if item.trashed {
                    let response = ui.add_enabled(!app.session.gallery.busy(), egui::Button::new("Restore"));
                    remember(ui, "gallery.trash", &response);
                    if response.clicked() {
                        app.ui.gallery.confirm_delete = None;
                        run(app, "komaphone.gallery.update", json!({"entry":item.id,"draft":item.draft,"trashed":false}));
                    }
                    let blocked_open = item.draft && protected.contains(&item.id);
                    if app.ui.gallery.confirm_delete == Some((item.id, item.draft)) && !blocked_open {
                        let confirm = ui.add_enabled(app.session.is_enabled("komaphone.gallery.delete"), egui::Button::new("Confirm delete"));
                        remember(ui, "komaphone.gallery.delete", &confirm);
                        if confirm.clicked() {
                            app.ui.gallery.confirm_delete = None;
                            run(app, "komaphone.gallery.delete", json!({"entry":item.id,"draft":item.draft}));
                        }
                        let cancel = ui.button("Cancel");
                        remember(ui, "gallery.delete.cancel", &cancel);
                        if cancel.clicked() {
                            app.ui.gallery.confirm_delete = None;
                        }
                    } else {
                        let delete_enabled = !blocked_open && app.session.is_enabled("komaphone.gallery.delete");
                        let tip = if blocked_open {
                            "Close this drawing in the Workroom before deleting it permanently"
                        } else {
                            "Permanently delete this item and reclaim its local storage"
                        };
                        let response = ui.add_enabled(delete_enabled, egui::Button::new("Delete permanently")).on_hover_text(tip).on_disabled_hover_text(tip);
                        remember(ui, "gallery.delete", &response);
                        if response.clicked() {
                            app.ui.gallery.confirm_delete = Some((item.id, item.draft));
                        }
                    }
                } else {
                    let response = ui.add_enabled(!app.session.gallery.busy(), egui::Button::new("Move to trash"));
                    remember(ui, "gallery.trash", &response);
                    if response.clicked() {
                        run(app, "komaphone.gallery.update", json!({"entry":item.id,"draft":item.draft,"trashed":true}));
                    }
                }
            }
        });
        ui.horizontal(|ui| {
            let response = ui.add(egui::TextEdit::singleline(&mut app.ui.gallery.name).desired_width(210.0));
            remember(ui, "gallery.name", &response);
            let response = ui.add_enabled(!app.session.gallery.busy(), egui::Button::new("Rename"));
            remember(ui, "gallery.rename", &response);
            if response.clicked() {
                run(app, "komaphone.gallery.update", json!({"entry":manifest.id,"draft":app.session.gallery.active_draft,"title":app.ui.gallery.name}));
            }
        });
        let authors =
            if manifest.authors.is_empty() { "You".into() } else { manifest.authors.iter().map(|a| a.display_name.as_str()).collect::<Vec<_>>().join(" · ") };
        ui.label(authors);
        let updated = crate::komaphone_time::format_compact(&manifest.updated_at);
        let detail_time = ui.small(format!("Updated {}", updated.display));
        if let Some(tooltip) = &updated.tooltip {
            detail_time.on_hover_text(tooltip);
        }
        if let Some(source) = &manifest.provenance {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "Remixed from revision {} · {}",
                    source.source_revision,
                    source.authors.iter().map(|a| a.display_name.as_str()).collect::<Vec<_>>().join(", ")
                ));
                let response = ui.link("Open original history");
                remember(ui, "gallery.source", &response);
                if response.clicked() {
                    run(app, "komaphone.gallery.open", json!({"entry":source.source_entry}));
                }
            });
        }
        if usize::from(app.ui.gallery.chain) >= chains {
            app.ui.gallery.chain = 0;
            app.ui.gallery.frame = 0;
            app.ui.gallery.replay_step = 0;
            app.ui.gallery.playing_animation = false;
            app.ui.gallery.playing_replay = false;
            app.ui.gallery.accumulator = 0.0;
        }
        ui.horizontal_wrapped(|ui| {
            if chains > 1 || is_game {
                for chain in 0..chains {
                    let response = ui.selectable_value(
                        &mut app.ui.gallery.chain,
                        chain as u8,
                        format!("{} {}", if creature { "Creature" } else { "Animation" }, chain + 1),
                    );
                    remember(ui, &format!("gallery.chain.{chain}"), &response);
                    if response.changed() {
                        app.ui.gallery.frame = 0;
                        app.ui.gallery.replay_step = 0;
                        app.ui.gallery.playing_animation = false;
                        app.ui.gallery.playing_replay = false;
                        app.ui.gallery.accumulator = 0.0;
                    }
                }
            }
            let before = app.ui.gallery.replay_mode;
            let response = ui.selectable_value(
                &mut app.ui.gallery.replay_mode,
                false,
                if creature {
                    "Full creature"
                } else if frames > 1 {
                    "Finished animation"
                } else {
                    "Finished drawing"
                },
            );
            remember(ui, "gallery.finishedMode", &response);
            let response = ui.selectable_value(&mut app.ui.gallery.replay_mode, true, "Drawing replay");
            remember(ui, "gallery.replayMode", &response);
            if before != app.ui.gallery.replay_mode {
                app.ui.gallery.playing_animation = false;
                app.ui.gallery.playing_replay = false;
                app.ui.gallery.replay_step = 0;
                app.ui.gallery.accumulator = 0.0;
            }
        });
        if app.ui.gallery.replay_mode {
            ui.horizontal_wrapped(|ui| {
                if frames > 1 || is_game {
                    ui.label(if creature { "Part" } else { "Frame" });
                    let response = ui.add(egui::DragValue::new(&mut app.ui.gallery.frame).range(0..=frames.saturating_sub(1)));
                    remember(ui, "gallery.frame", &response);
                    if response.changed() {
                        app.ui.gallery.replay_step = 0;
                        app.ui.gallery.playing_replay = false;
                        app.ui.gallery.accumulator = 0.0;
                    }
                }
                let can_play = !selected_missing(&manifest, app.ui.gallery.chain, app.ui.gallery.frame)
                    && app.session.gallery.replay.as_ref().is_some_and(|replay| replay.moments.len() > 1);
                let response = ui.add_enabled(can_play, egui::Button::new(if app.ui.gallery.playing_replay { "Pause drawing" } else { "Play drawing" }));
                remember(ui, "gallery.replayPlay", &response);
                if response.clicked() {
                    if !app.ui.gallery.playing_replay
                        && app.session.gallery.replay.as_ref().is_some_and(|replay| app.ui.gallery.replay_step.saturating_add(1) >= replay.moments.len())
                    {
                        app.ui.gallery.replay_step = 0;
                    }
                    app.ui.gallery.playing_replay = !app.ui.gallery.playing_replay;
                    app.ui.gallery.accumulator = 0.0;
                }
                ui.label("Speed");
                let response = ui.add(egui::DragValue::new(&mut app.ui.gallery.replay_speed).range(0.25..=8.0).speed(0.25).suffix("×"));
                remember(ui, "gallery.replaySpeed", &response);
            });
            if let Some(replay) = &app.session.gallery.replay {
                if replay.missing {
                    ui.label("Missing submission · no drawing history");
                } else if replay.baseline_only {
                    ui.label("Current-state baseline · no earlier drawing history");
                }
                let count = replay.moments.len();
                app.ui.gallery.replay_step = app.ui.gallery.replay_step.min(count.saturating_sub(1));
                let response = ui.add_enabled(
                    !selected_missing(&manifest, app.ui.gallery.chain, app.ui.gallery.frame) && count > 1,
                    egui::Slider::new(&mut app.ui.gallery.replay_step, 0..=count.saturating_sub(1)).text("Edit"),
                );
                remember(ui, "gallery.replayStep", &response);
                if response.changed() {
                    app.ui.gallery.playing_replay = false;
                    app.ui.gallery.accumulator = 0.0;
                }
                if let Some(moment) = replay.moments.get(app.ui.gallery.replay_step) {
                    ui.small(format!("{:.2}s · {} · {}", moment.elapsed_ms as f64 / 1000.0, moment.kind, moment.command));
                }
            }
        } else if is_animation {
            ui.horizontal_wrapped(|ui| {
                let response =
                    ui.add_enabled(frames > 1, egui::Button::new(if app.ui.gallery.playing_animation { "Pause animation" } else { "Play animation" }));
                remember(ui, "gallery.animationPlay", &response);
                if response.clicked() {
                    if !app.ui.gallery.playing_animation && app.ui.gallery.frame.saturating_add(1) >= frames {
                        app.ui.gallery.frame = 0;
                    }
                    app.ui.gallery.playing_animation = !app.ui.gallery.playing_animation;
                    app.ui.gallery.accumulator = 0.0;
                }
                let response = ui.add(egui::DragValue::new(&mut app.ui.gallery.frame).range(0..=frames.saturating_sub(1)).prefix("Frame "));
                remember(ui, "gallery.frame", &response);
                if response.changed() {
                    app.ui.gallery.playing_animation = false;
                    app.ui.gallery.accumulator = 0.0;
                }
                let fps = ui.add(egui::DragValue::new(&mut app.ui.gallery.fps).range(1..=24).suffix(" fps"));
                remember(ui, "gallery.fps", &fps);
                let looping = ui.checkbox(&mut app.ui.gallery.looping, "Loop");
                remember(ui, "gallery.loop", &looping);
                if fps.changed() || looping.changed() {
                    app.ui.gallery.pending_playback = true;
                }
                let response = ui.checkbox(&mut app.ui.gallery.gif_preview, "Preview GIF colors");
                remember(ui, "gallery.gifPreview", &response);
            });
        }
        if let Some(image) = &app.session.gallery.image {
            let handle = texture(ui, egui::Id::unique("gallery-preview"), app.session.gallery.image_revision, image);
            let preview_max_h = if is_game { (ui.ctx().content_rect().height() - 560.0).clamp(150.0, 340.0) } else { 340.0 };
            ui.add(egui::Image::new(&handle).max_width(ui.available_width()).max_height(preview_max_h).maintain_aspect_ratio(true));
        }
        ui.horizontal_wrapped(|ui| {
            if app.session.gallery.active_draft {
                let response = ui.button("Continue drawing");
                remember(ui, "gallery.continue", &response);
                if response.clicked() {
                    run(app, "komaphone.project.open", json!({"project":manifest.id}));
                    app.ui.gallery.open = false;
                    app.ui.rooms.view_reveal = false;
                }
                let save_enabled = app.session.is_enabled("komaphone.gallery.save");
                let response = ui.add_enabled(save_enabled, egui::Button::new("Save to gallery")).on_hover_text("Keep this drawing in Gallery > Saved");
                remember(ui, "gallery.saveToGallery", &response);
                if response.clicked() {
                    run(app, "komaphone.gallery.save", json!({"entry":manifest.id}));
                }
            } else if !is_game {
                let edit_enabled = app.session.is_enabled("komaphone.gallery.openEditable");
                let response = ui
                    .add_enabled(edit_enabled, egui::Button::new("Edit in Workroom"))
                    .on_hover_text("Open this saved drawing in the Workroom to continue editing");
                remember(ui, "gallery.editInWorkroom", &response);
                if response.clicked() {
                    run(app, "komaphone.gallery.openEditable", json!({"entry":manifest.id}));
                }
            }
            button(
                app,
                ui,
                if creature {
                    "Remix creature"
                } else if frames > 1 {
                    "Remix full animation"
                } else {
                    "Remix as new drawing"
                },
                "komaphone.gallery.remix",
                json!({"chain":app.ui.gallery.chain}),
            );
            if frames > 1 || creature {
                let response = ui.add_enabled(!app.session.gallery.busy(), egui::Button::new("Remix selected still"));
                remember(ui, "gallery.remixStill", &response);
                if response.clicked() {
                    run(app, "komaphone.gallery.remix", json!({"chain":app.ui.gallery.chain,"frame":app.ui.gallery.frame}));
                }
            }
        });
        ui.small("Remix creates an independent copy. Source artwork, credits and original drawing history stay intact.");
        if is_game {
            ui.horizontal_wrapped(|ui| {
                let response = ui.button("Place in a canvas");
                remember(ui, "gallery.world", &response);
                if response.clicked() {
                    crate::komaphone_world::source_from_gallery(app, false);
                }
                if !creature {
                    let response = ui.button("Place selected still in a canvas");
                    remember(ui, "gallery.worldStill", &response);
                    if response.clicked() {
                        crate::komaphone_world::source_from_gallery(app, true);
                    }
                }
            });
            let response = ui.checkbox(&mut app.ui.gallery.show_notes, "Open handoff notes");
            remember(ui, "gallery.notes", &response);
            if response.clicked() && app.ui.gallery.show_notes && manifest.notes.is_empty() {
                run(app, "komaphone.gallery.notes", json!({}));
            }
            if app.ui.gallery.show_notes {
                for (assignment, note) in &manifest.notes {
                    if assignment.chain == app.ui.gallery.chain && !note.is_empty() {
                        ui.label(format!("Frame {}: {note}", assignment.round + 1));
                    }
                }
            }
        }
        ui.separator();
        exports(app, ui, creature, frames);
    });
}

fn exports(app: &mut PhotocraftApp, ui: &mut egui::Ui, creature: bool, frames: u8) {
    let tokens = Tokens::get(ui.ctx());
    let is_animation = !creature && frames > 1;
    let hint = if is_animation {
        "Animation name"
    } else if creature {
        "Creature name"
    } else {
        "Drawing name"
    };
    ui.label("Export name");
    let response = ui.add(egui::TextEdit::singleline(&mut app.ui.gallery.export_name).desired_width(f32::INFINITY).hint_text(hint));
    remember(ui, "gallery.exportName", &response);
    let location = ui.small(RichText::new("Saves automatically to your Komaphone Exports folder").color(tokens.text_dim));
    if let Some(dir) = app.session.gallery.exports_dir.as_deref() {
        location.on_hover_text(dir);
    }
    let validation = photocraft_engine::gallery_exports::validate_export_basename(&app.ui.gallery.export_name);
    if let Err(reason) = &validation
        && !app.ui.gallery.export_name.trim().is_empty()
    {
        ui.colored_label(tokens.danger, reason.to_string());
    }
    let disabled_tip = if app.session.gallery.busy() {
        "Gallery is working".to_owned()
    } else if let Err(reason) = &validation {
        reason.to_string()
    } else {
        String::new()
    };
    let can_export = !app.session.gallery.busy() && validation.is_ok();
    ui.horizontal_wrapped(|ui| {
        let mut formats: Vec<(&str, &str)> = vec![("Editable archive", "archive"), ("PNG image", "png")];
        if creature || frames > 1 {
            formats.push(("PNG frames", "sequence"));
        }
        if is_animation {
            formats.push(("Animated GIF", "gif"));
        }
        for (label, format) in formats {
            let mut response = ui.add_enabled(can_export, egui::Button::new(label));
            if !can_export && !disabled_tip.is_empty() {
                response = response.on_disabled_hover_text(&disabled_tip);
            }
            remember(ui, &format!("gallery.export.{format}"), &response);
            if response.clicked() {
                run(
                    app,
                    "komaphone.gallery.exportManaged",
                    json!({
                        "format": format,
                        "name": app.ui.gallery.export_name,
                        "chain": app.ui.gallery.chain,
                        "frame": if creature { None } else { Some(app.ui.gallery.frame) },
                        "fps": app.ui.gallery.fps,
                        "looping": app.ui.gallery.looping
                    }),
                );
            }
        }
        if app.session.gallery.exporting() {
            let cancel = ui.button("Cancel export");
            remember(ui, "gallery.export.cancel", &cancel);
            if cancel.clicked() {
                run(app, "komaphone.gallery.cancelExport", json!({}));
            }
        }
    });
    if let Some(last) = app.session.gallery.last_export.clone() {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("Saved to {}", last.path)).color(tokens.text_dim));
            let button_label = if last.is_directory { "Open folder" } else { "Show in folder" };
            let reveal = ui.button(button_label);
            remember(ui, "gallery.export.reveal", &reveal);
            if reveal.clicked() {
                match app.services.reveal_in_folder.as_ref() {
                    Some(service) => {
                        if let Err(err) = service(&last.path, last.is_directory) {
                            app.session.gallery.error = Some(err);
                        }
                    }
                    None => {
                        app.session.gallery.error = Some("Opening folders is unavailable in this environment.".into());
                    }
                }
            }
        });
    }
    if is_animation {
        ui.small(photocraft_engine::gallery_media::GIF_NOTICE);
    }
}

fn storage(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    ui.group(|ui| {
        ui.label("Archive file or new backup folder");
        let response = ui.text_edit_singleline(&mut app.ui.gallery.path);
        remember(ui, "gallery.storagePath", &response);
        ui.horizontal_wrapped(|ui| {
            button(app, ui, "Import editable archive", "komaphone.gallery.import", json!({"path":app.ui.gallery.path}));
            button(app, ui, "Back up all local originals", "komaphone.gallery.backup", json!({"path":app.ui.gallery.path}));
        });
        ui.label("Restore into a new data folder");
        let response = ui.text_edit_singleline(&mut app.ui.gallery.restore_destination);
        remember(ui, "gallery.restoreDestination", &response);
        button(
            app,
            ui,
            "Restore backup to new folder",
            "komaphone.gallery.restore",
            json!({"path":app.ui.gallery.path,"destination":app.ui.gallery.restore_destination}),
        );
        ui.small("Includes every local draft, retained replay, gallery original and trash entry. Room credentials are excluded; reconnect privately after restoration.");
    });
}

fn tick(app: &mut PhotocraftApp, ui: &egui::Ui) {
    let now = ui.input(|input| input.time);
    let elapsed = now - app.ui.gallery.last_time;
    let delta = if elapsed.is_finite() { elapsed.clamp(0.0, 604_800.0) } else { 0.0 };
    app.ui.gallery.last_time = now;
    let Some(manifest) = &app.session.gallery.active else {
        return;
    };
    if app.ui.gallery.pending_playback && !app.session.gallery.busy() {
        run(app, "komaphone.gallery.playback", json!({"fps":app.ui.gallery.fps,"looping":app.ui.gallery.looping}));
        app.ui.gallery.pending_playback = false;
        app.ui.gallery.requested = None;
        return;
    }
    let (frames, creature, project) = match &manifest.content {
        GalleryContent::Game { room } => {
            (usize::from(room.mode.rounds(room.members.len()).unwrap_or(1)), room.mode == photocraft_engine::party::Mode::ExquisiteCorpse, false)
        }
        GalleryContent::Project { .. } => (app.session.gallery.frame_count.max(1), false, true),
    };
    // Rendering is asynchronous. Advance the clocks during worker time and keep the
    // fractional remainder so decode latency never permanently slows playback.
    if app.ui.gallery.playing_animation {
        app.ui.gallery.accumulator += delta * f64::from(app.ui.gallery.fps.clamp(1, 24));
        let steps = app.ui.gallery.accumulator.floor() as u64;
        app.ui.gallery.accumulator -= steps as f64;
        let next = u64::from(app.ui.gallery.frame).saturating_add(steps);
        if app.ui.gallery.looping {
            app.ui.gallery.frame = (next % frames.max(1) as u64) as u8;
        } else {
            app.ui.gallery.frame = next.min(frames.saturating_sub(1) as u64) as u8;
            if next >= frames.saturating_sub(1) as u64 {
                app.ui.gallery.playing_animation = false;
            }
        }
    }
    if app.ui.gallery.playing_replay
        && let Some(replay) = &app.session.gallery.replay
    {
        app.ui.gallery.accumulator += delta * f64::from(app.ui.gallery.replay_speed.clamp(0.25, 8.0)) * 1000.0;
        let current = replay.moments.get(app.ui.gallery.replay_step).map_or(0, |step| step.elapsed_ms);
        let target = current as f64 + app.ui.gallery.accumulator;
        let index = replay.moments.partition_point(|step| step.elapsed_ms as f64 <= target).saturating_sub(1);
        app.ui.gallery.replay_step = index;
        app.ui.gallery.accumulator = target - replay.moments.get(index).map_or(0, |step| step.elapsed_ms) as f64;
        if index.saturating_add(1) >= replay.moments.len() {
            app.ui.gallery.playing_replay = false;
        }
    }
    let step = (app.ui.gallery.replay_mode && !selected_missing(manifest, app.ui.gallery.chain, app.ui.gallery.frame)).then_some(app.ui.gallery.replay_step);
    let frame = if project && step.is_some() { 0 } else { app.ui.gallery.frame };
    let key = (
        app.ui.gallery.chain,
        frame,
        step,
        creature && !app.ui.gallery.replay_mode,
        app.ui.gallery.gif_preview && !app.ui.gallery.replay_mode && !creature && frames > 1,
    );
    if !app.session.gallery.busy() && app.ui.gallery.requested != Some(key) {
        if key.4 {
            run(app, "komaphone.gallery.previewGif", json!({"chain":key.0,"frame":key.1,"fps":app.ui.gallery.fps,"looping":app.ui.gallery.looping}));
        } else {
            run(app, "komaphone.gallery.render", json!({"chain":key.0,"frame":key.1,"step":key.2,"creature":key.3}));
        }
        app.ui.gallery.requested = Some(key);
    }
    if app.ui.gallery.playing_animation || app.ui.gallery.playing_replay {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
    }
}

fn selected_missing(manifest: &photocraft_engine::party::gallery::GalleryManifest, chain: u8, frame: u8) -> bool {
    matches!(&manifest.content,GalleryContent::Game {room} if room.reveal.iter().any(|entry|matches!(entry,photocraft_engine::party::room::RevealedEntry::Missing {assignment,..} if assignment.id.chain==chain&&assignment.id.round==frame)))
}

#[cfg(test)]
#[path = "komaphone_gallery_tests.rs"]
mod tests;
