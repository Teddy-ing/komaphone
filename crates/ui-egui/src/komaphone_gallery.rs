//! Gallery view over the engine command seam. Drawing replay and animation are separate clocks.
use crate::{PhotocraftApp, theme::Tokens};
use egui::{RichText, TextureHandle};
use photocraft_engine::party::ProjectId;
use photocraft_engine::party::gallery::GalleryContent;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GalleryUi {
    pub open: bool,
    pub filter: String,
    pub favorites: bool,
    pub trash: bool,
    pub kind: String,
    pub chain: u8,
    pub frame: u8,
    pub replay_step: usize,
    pub replay_mode: bool,
    pub fps: u8,
    pub looping: bool,
    pub replay_speed: f32,
    pub name: String,
    pub path: String,
    pub restore_destination: String,
    pub storage_open: bool,
    pub gif_preview: bool,
    pub show_notes: bool,
    #[serde(skip)]
    pub playing_animation: bool,
    #[serde(skip)]
    pub playing_replay: bool,
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
            favorites: false,
            trash: false,
            kind: "all".into(),
            chain: 0,
            frame: 0,
            replay_step: 0,
            replay_mode: false,
            fps: 6,
            looping: true,
            replay_speed: 1.0,
            name: String::new(),
            path: String::new(),
            restore_destination: String::new(),
            storage_open: false,
            gif_preview: false,
            show_notes: false,
            playing_animation: false,
            playing_replay: false,
            last_time: 0.0,
            accumulator: 0.0,
            selected: None,
            requested: None,
            pending_playback: false,
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
        app.ui.rooms.view_reveal = false;
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.heading("Gallery");
        button(app, ui, "Refresh", "komaphone.gallery.list", json!({}));
        let response = ui.button("Draw");
        remember(ui, "gallery.draw", &response);
        if response.clicked() {
            app.ui.gallery.open = false;
        }
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
        let ratio =
            if app.session.gallery.download_bytes == 0 { 0.0 } else { app.session.gallery.downloaded_bytes as f32 / app.session.gallery.download_bytes as f32 };
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
    let height = ui.available_height();
    let mut thumbnail = None;
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(278.0, height), egui::Layout::top_down(egui::Align::Min), |ui| thumbnail = list(app, ui));
        ui.separator();
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), height), egui::Layout::top_down(egui::Align::Min), |ui| detail(app, ui));
    });
    tick(app, ui);
    if let Some((entry, draft)) = thumbnail
        && !app.session.gallery.busy()
    {
        run(app, "komaphone.gallery.thumbnail", json!({"entry":entry,"draft":draft}));
    }
}

fn list(app: &mut PhotocraftApp, ui: &mut egui::Ui) -> Option<(ProjectId, bool)> {
    let response = ui.add(egui::TextEdit::singleline(&mut app.ui.gallery.filter).hint_text("Search titles or authors"));
    remember(ui, "gallery.filter", &response);
    ui.horizontal(|ui| {
        let response = ui.checkbox(&mut app.ui.gallery.favorites, "Favorites");
        remember(ui, "gallery.favorites", &response);
        let response = ui.checkbox(&mut app.ui.gallery.trash, "Trash");
        remember(ui, "gallery.trashFilter", &response);
    });
    let combo = craft_ui::ComboBox::from_id_salt("gallery-kind").selected_text(&app.ui.gallery.kind).show_ui(ui, |ui| {
        for kind in ["all", "draft", "animation", "creature", "project"] {
            ui.selectable_value(&mut app.ui.gallery.kind, kind.into(), kind);
        }
    });
    remember(ui, "gallery.kind", &combo.response);
    let filter = app.ui.gallery.filter.to_lowercase();
    let items: Vec<_> = app
        .session
        .gallery
        .items
        .iter()
        .filter(|item| {
            item.trashed == app.ui.gallery.trash
                && (!app.ui.gallery.favorites || item.favorite)
                && (app.ui.gallery.kind == "all" || app.ui.gallery.kind == item.kind)
                && (filter.is_empty() || item.title.to_lowercase().contains(&filter) || item.authors.iter().any(|a| a.to_lowercase().contains(&filter)))
                && app.session.gallery_item_allowed(item)
        })
        .cloned()
        .collect();
    let mut thumbnail = None;
    egui::ScrollArea::vertical().id_salt("gallery-list").max_height((ui.available_height() - 100.0).max(80.0)).show(ui, |ui| {
        if items.is_empty() {
            ui.label("No saved items match this view.");
        }
        for item in items {
            let row = ui.group(|ui| {
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
                            run(app, "komaphone.gallery.open", json!({"entry":item.id,"draft":item.draft}));
                        }
                        ui.small(format!("{}{}", item.kind, if item.complete { "" } else { " · incomplete" }));
                        ui.small(item.authors.join(", "));
                        ui.small(&item.updated_at);
                        ui.small(&item.state);
                    });
                });
            });
            if thumbnail.is_none() && ui.is_rect_visible(row.response.rect) && !app.session.gallery.thumbnails.contains_key(&(item.id, item.draft)) {
                thumbnail = Some((item.id, item.draft));
            }
        }
    });
    if app.session.rooms.status.room.is_some() {
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
    let Some(manifest) = app.session.gallery.active.clone() else {
        ui.heading("Keep what you make");
        ui.label("Open a drawing or saved game to replay, export, or make an independent remix.");
        ui.label("At reveal, use Save to gallery to retain the complete game and drawing histories offline.");
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
        app.ui.gallery.show_notes = false;
        app.ui.gallery.gif_preview = false;
        app.ui.gallery.pending_playback = false;
        app.ui.gallery.fps = manifest.fps;
        app.ui.gallery.looping = manifest.looping;
        app.ui.gallery.name = manifest.title.clone();
        app.ui.gallery.requested = None;
    }
    let (chains, frames, creature) = match &manifest.content {
        GalleryContent::Game { room } => {
            (room.members.len(), room.mode.rounds(room.members.len()).unwrap_or(1), room.mode == photocraft_engine::party::Mode::ExquisiteCorpse)
        }
        GalleryContent::Project { .. } => (1, app.session.gallery.frame_count.clamp(1, 36) as u8, false),
    };
    egui::ScrollArea::vertical().id_salt("gallery-detail").show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.heading(&manifest.title);
            let item = app.session.gallery.items.iter().find(|item| item.id == manifest.id && item.draft == app.session.gallery.active_draft).cloned();
            if let Some(item) = item {
                button(
                    app,
                    ui,
                    if item.favorite { "Unfavorite" } else { "Favorite" },
                    "komaphone.gallery.update",
                    json!({"entry":item.id,"draft":item.draft,"favorite":!item.favorite}),
                );
                let response = ui.button(if item.trashed { "Restore from trash" } else { "Move to trash" });
                remember(ui, "gallery.trash", &response);
                if response.clicked() {
                    run(app, "komaphone.gallery.update", json!({"entry":item.id,"draft":item.draft,"trashed":!item.trashed}));
                }
            }
        });
        ui.horizontal(|ui| {
            let response = ui.add(egui::TextEdit::singleline(&mut app.ui.gallery.name).desired_width(210.0));
            remember(ui, "gallery.name", &response);
            let response = ui.button("Rename");
            remember(ui, "gallery.rename", &response);
            if response.clicked() {
                run(app, "komaphone.gallery.update", json!({"entry":manifest.id,"draft":app.session.gallery.active_draft,"title":app.ui.gallery.name}));
            }
        });
        ui.label(manifest.authors.iter().map(|a| a.display_name.as_str()).collect::<Vec<_>>().join(" · "));
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
        ui.horizontal_wrapped(|ui| {
            for chain in 0..chains {
                let response =
                    ui.selectable_value(&mut app.ui.gallery.chain, chain as u8, format!("{} {}", if creature { "Creature" } else { "Animation" }, chain + 1));
                remember(ui, &format!("gallery.chain.{chain}"), &response);
                if response.changed() {
                    app.ui.gallery.replay_step = 0;
                    app.ui.gallery.playing_animation = false;
                    app.ui.gallery.playing_replay = false;
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
            }
        });
        if app.ui.gallery.replay_mode {
            ui.horizontal_wrapped(|ui| {
                ui.label(if creature { "Part" } else { "Frame" });
                let response = ui.add(egui::DragValue::new(&mut app.ui.gallery.frame).range(0..=frames.saturating_sub(1)));
                remember(ui, "gallery.frame", &response);
                if response.changed() {
                    app.ui.gallery.replay_step = 0;
                    app.ui.gallery.playing_replay = false;
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
                }
                if let Some(moment) = replay.moments.get(app.ui.gallery.replay_step) {
                    ui.small(format!("{:.2}s · {} · {}", moment.elapsed_ms as f64 / 1000.0, moment.kind, moment.command));
                }
            }
        } else if !creature {
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
            ui.add(egui::Image::new(&handle).max_width(ui.available_width()).max_height(340.0).maintain_aspect_ratio(true));
        }
        ui.horizontal_wrapped(|ui| {
            button(
                app,
                ui,
                if creature {
                    "Remix creature"
                } else if frames > 1 {
                    "Remix full animation"
                } else {
                    "Remix drawing"
                },
                "komaphone.gallery.remix",
                json!({"chain":app.ui.gallery.chain}),
            );
            let response = ui.add_enabled(!app.session.gallery.busy(), egui::Button::new("Remix selected still"));
            remember(ui, "gallery.remixStill", &response);
            if response.clicked() {
                run(app, "komaphone.gallery.remix", json!({"chain":app.ui.gallery.chain,"frame":app.ui.gallery.frame}));
            }
            if app.session.gallery.active_draft {
                let response = ui.button("Continue original draft");
                remember(ui, "gallery.continue", &response);
                if response.clicked() {
                    run(app, "komaphone.project.open", json!({"project":manifest.id}));
                    app.ui.gallery.open = false;
                    app.ui.rooms.view_reveal = false;
                }
            }
        });
        ui.small("Remix creates a new saved project. Source artwork, credits and original drawing history stay intact.");
        if matches!(manifest.content, GalleryContent::Game { .. }) {
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
        }
        if matches!(manifest.content, GalleryContent::Game { .. }) {
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
        exports(app, ui, creature);
    });
}

fn exports(app: &mut PhotocraftApp, ui: &mut egui::Ui, creature: bool) {
    ui.label("New output file or folder");
    let response = ui.add(egui::TextEdit::singleline(&mut app.ui.gallery.path).desired_width(f32::INFINITY).hint_text("C:\\Users\\…\\animation.gif"));
    remember(ui, "gallery.path", &response);
    ui.horizontal_wrapped(|ui| {
        for (label,format) in [("Editable archive","archive"),("PNG image","png"),("PNG frames","sequence"),("Animated GIF","gif")] {
            let response=ui.add_enabled(!app.session.gallery.busy()&&!app.ui.gallery.path.trim().is_empty()&&(!creature||format!="gif"),egui::Button::new(label));remember(ui,&format!("gallery.export.{format}"),&response);
            if response.clicked(){run(app,"komaphone.gallery.export",json!({"format":format,"path":app.ui.gallery.path,"chain":app.ui.gallery.chain,"frame":if creature{None}else{Some(app.ui.gallery.frame)},"fps":app.ui.gallery.fps,"looping":app.ui.gallery.looping}));}
        }
    });
    ui.small(photocraft_engine::gallery_media::GIF_NOTICE);
}

fn storage(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    ui.group(|ui| {
        ui.label("Archive file or new backup folder");let response=ui.text_edit_singleline(&mut app.ui.gallery.path);remember(ui,"gallery.storagePath",&response);
        ui.horizontal_wrapped(|ui| {
            button(app,ui,"Import editable archive","komaphone.gallery.import",json!({"path":app.ui.gallery.path}));
            button(app,ui,"Back up all local originals","komaphone.gallery.backup",json!({"path":app.ui.gallery.path}));
        });
        ui.label("Restore into a new data folder");let response=ui.text_edit_singleline(&mut app.ui.gallery.restore_destination);remember(ui,"gallery.restoreDestination",&response);
        button(app,ui,"Restore backup to new folder","komaphone.gallery.restore",json!({"path":app.ui.gallery.path,"destination":app.ui.gallery.restore_destination}));
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
    let key =
        (app.ui.gallery.chain, frame, step, creature && !app.ui.gallery.replay_mode, app.ui.gallery.gif_preview && !app.ui.gallery.replay_mode && !creature);
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
