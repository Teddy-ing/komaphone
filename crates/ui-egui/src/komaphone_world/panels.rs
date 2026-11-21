use super::{PhotocraftApp, Tokens, TourStop, Value, button, json, remember, reviewing, run, scene};

/// Only materialize controls for visible rows; collection limits are not paint budgets.
pub(super) fn list_rows(ui: &mut egui::Ui, id: &'static str, lines: usize, count: usize, mut row: impl FnMut(&mut egui::Ui, usize)) {
    if count == 0 {
        return;
    }
    let line = ui.spacing().interact_size.y.max(ui.text_style_height(&egui::TextStyle::Body));
    let height = line * lines as f32 + ui.spacing().item_spacing.y * lines.saturating_sub(1) as f32;
    egui::ScrollArea::vertical().id_salt(id).max_height(240.0).show_rows(ui, height, count, |ui, range| {
        for index in range {
            ui.push_id(index, |ui| {
                ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
                    ui.set_min_height(height);
                    row(ui, index);
                });
            });
        }
    });
}

pub(super) fn sidebar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let tokens = Tokens::get(ui.ctx());
    egui::Panel::left("world-sidebar").exact_size(260.0).frame(egui::Frame::NONE.fill(tokens.card).inner_margin(10)).show(ui, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            let idle = !app.ui.world.has_stranded_draft();
            ui.heading("Your shared canvases");
            if !app.session.worlds.connected() {
                if ui.button("Open Private room").clicked() {
                    app.ui.world.open = false;
                    app.ui.rooms.open = true;
                }
                return;
            }
            let label = ui.label("New canvas name");
            let response = ui.text_edit_singleline(&mut app.ui.world.name).labelled_by(label.id);
            remember(ui, "world.name", &response);
            button(app, ui, "Create canvas", "komaphone.world.create", json!({"name":app.ui.world.name}), idle);
            button(app, ui, "Refresh", "komaphone.world.list", json!({}), idle);
            let section = ui.collapsing("Restore an archive", |ui| restore_archive(app, ui, idle));
            remember(ui, "world.section.restore", &section.header_response);
            list_rows(ui, "world-canvas-list", 2, app.session.worlds.items.len(), |ui, index| {
                let Some(item) = app.session.worlds.items.get(index) else { return };
                let (canvas, name, owner, revision) = (item.canvas, item.name.clone(), item.owner, item.revision);
                ui.horizontal(|ui| {
                    super::thumbnails::show(app, ui, canvas);
                    ui.vertical(|ui| {
                        let response =
                            ui.add_enabled(idle, egui::Button::new(&name).selected(app.ui.world.selected == Some(canvas)).truncate()).on_hover_text(&name);
                        remember(ui, &format!("world.open.{}", canvas.as_str()), &response);
                        if response.clicked() && !app.ui.world.unfinished() {
                            run(app, "komaphone.world.open", json!({"canvas":canvas}));
                        }
                        let owner =
                            if app.session.worlds.account.as_ref().is_some_and(|account| account.profile == owner) { "You" } else { display_name(app, owner) };
                        let pending = app.session.worlds.active.as_ref().filter(|active| active.info.canvas == canvas).map_or(0, |active| active.pending);
                        ui.small(if pending > 0 { format!("{owner} · {pending} waiting to sync") } else { format!("{owner} · saved revision {revision}") });
                    });
                });
            });
            let section = ui.collapsing("Trash", |ui| {
                button(app, ui, "Refresh Trash", "komaphone.world.trash", json!({}), idle);
                ui.small("Only owners can restore deleted canvases. Restoration brings back their artwork and previous members.");
                list_rows(ui, "world-trash-list", 2, app.session.worlds.trash.len(), |ui, index| {
                    let Some(item) = app.session.worlds.trash.get(index) else { return };
                    let (canvas, epoch) = (item.canvas, item.owner_epoch);
                    ui.label(&item.name);
                    let response = ui.add_enabled(idle, egui::Button::new("Restore canvas and members"));
                    remember(ui, &format!("world.undelete.{}", canvas.as_str()), &response);
                    if response.clicked() && !app.ui.world.unfinished() {
                        run(app, "komaphone.world.undelete", json!({"canvas":canvas,"expected_epoch":epoch}));
                    }
                });
            });
            remember(ui, "world.section.trash", &section.header_response);
            let section = ui.collapsing("Accept an invitation", |ui| {
                ui.label("Paste the invitation shared by the canvas owner.");
                let response = ui.add(egui::TextEdit::multiline(&mut app.ui.world.accept_code).desired_rows(2).char_limit(3072));
                remember(ui, "world.invitation", &response);
                let response = ui.add_enabled(idle, egui::Button::new("Accept"));
                remember(ui, "world.invitation.accept", &response);
                if response.clicked() && !app.ui.world.unfinished() {
                    let parsed = if app.ui.world.accept_code.len() <= 3072 {
                        serde_json::from_str::<Value>(&app.ui.world.accept_code).map_err(|e| e.to_string())
                    } else {
                        Err("Invitation is too long".into())
                    };
                    match parsed {
                        Ok(value) => {
                            run(app, "komaphone.world.accept", value);
                        }
                        Err(_) => app.ui.world.message = Some("Paste the complete invitation code.".into()),
                    }
                }
            });
            remember(ui, "world.section.accept", &section.header_response);
            let section = ui.collapsing("Saved edits needing review", |ui| {
                button(app, ui, "Load saved edits", "komaphone.world.recovery", json!({}), idle);
                ui.small("Conflicting or revoked edits remain saved on this account. Continuing new edits keeps these originals unchanged.");
                for item in &app.session.worlds.recovery {
                    ui.label(format!("{} · canvas {}", item.reason, item.operation.canvas.as_str().get(..8).unwrap_or_default()));
                    if ui.button("Copy this retained edit").clicked()
                        && let Ok(value) = serde_json::to_string(item)
                    {
                        ui.ctx().copy_text(value);
                    }
                }
                if let Some(after) = app.session.worlds.recovery_next {
                    button(app, ui, "Next saved edits", "komaphone.world.recovery", json!({"after":after}), idle);
                }
                if let Some(canvas) = app.session.worlds.active.as_ref().map(|a| a.info.canvas)
                    && app.session.worlds.recovery.iter().any(|r| r.operation.canvas == canvas && r.reason == "Conflict")
                {
                    button(app, ui, "Keep originals and continue new edits", "komaphone.world.recovery.resume", json!({"canvas":canvas}), idle);
                }
            });
            remember(ui, "world.section.recovery", &section.header_response);
            let Some(active) = app.session.worlds.active.as_ref() else { return };
            let canvas = active.info.canvas;
            let owner = app.session.worlds.account.as_ref().is_some_and(|a| a.profile == active.info.owner);
            let epoch = active.info.epoch;
            let bookmarks = active.viewer.bookmarks.clone();
            ui.separator();
            ui.heading("Canvas");
            if app.session.worlds.active.as_ref().is_some_and(|active| active.canonical.import_origin.is_some()) {
                ui.small("Restored from an archive. Historical credits are recorded from that file.");
            }
            let label = ui.label("Canvas name");
            ui.text_edit_singleline(&mut app.ui.world.rename).labelled_by(label.id);
            button(app, ui, "Rename", "komaphone.world.rename", json!({"canvas":canvas,"name":app.ui.world.rename}), idle && owner);
            if owner {
                // `komaphone.world.delete` refuses while requests/edits are pending;
                // keeping the button visually enabled avoids blinking on every stroke ACK.
                button(app, ui, "Move canvas to Trash", "komaphone.world.delete", json!({"canvas":canvas,"expected_epoch":epoch}), idle);
            }
            let section = ui.collapsing("People and invitations", |ui| members(app, ui, idle, owner));
            remember(ui, "world.section.people", &section.header_response);
            let section = ui.collapsing("Bookmarks", |ui| {
                let label = ui.label("Bookmark name");
                ui.text_edit_singleline(&mut app.ui.world.bookmark).labelled_by(label.id);
                button(app, ui, "Save this view", "komaphone.world.bookmark", json!({"name":app.ui.world.bookmark}), idle);
                for bookmark in bookmarks {
                    ui.horizontal(|ui| {
                        button(app, ui, &bookmark.name, "komaphone.world.go", json!({"name":bookmark.name}), idle);
                        button(app, ui, "Auto zoom", "komaphone.world.go", json!({"name":bookmark.name,"animate":true}), idle);
                        button(app, ui, "Remove", "komaphone.world.bookmark.remove", json!({"name":bookmark.name}), idle);
                    });
                }
                if let Some(flight) = app.session.worlds.flight.clone() {
                    ui.horizontal(|ui| {
                        button(
                            app,
                            ui,
                            if flight.paused { "Resume" } else { "Pause" },
                            "komaphone.world.flight",
                            json!({"state":if flight.paused{"resume"}else{"pause"}}),
                            idle,
                        );
                        button(app, ui, "Cancel", "komaphone.world.flight", json!({"state":"cancel"}), idle);
                    });
                }
            });
            remember(ui, "world.section.bookmarks", &section.header_response);
            let section = ui.collapsing("Since your last review", |ui| review(app, ui, idle));
            remember(ui, "world.section.review", &section.header_response);
            let section = ui.collapsing("Tours", |ui| tours(app, ui, idle));
            remember(ui, "world.section.tours", &section.header_response);
            let section = ui.collapsing("Export and remix", |ui| media(app, ui, idle));
            remember(ui, "world.section.media", &section.header_response);
        });
    });
}
fn person_index(people: &[scene::world_protocol::Person], profile: scene::OwnerId) -> Option<usize> {
    // The engine validates strictly ordered profile pages before adding them.
    people.binary_search_by_key(&profile, |person| person.profile).ok()
}
pub(super) fn display_name(app: &PhotocraftApp, profile: scene::OwnerId) -> &str {
    person_index(&app.session.worlds.people, profile)
        .and_then(|index| app.session.worlds.people.get(index))
        .map_or("Member", |person| person.display_name.as_str())
}
pub(super) fn credit_name(app: &PhotocraftApp, profile: scene::OwnerId) -> String {
    if let Some(origin) = app.session.worlds.active.as_ref().and_then(|active| active.canonical.import_origin.as_ref())
        && let Some(original) = origin.original_author(profile)
    {
        if origin.actors.iter().filter(|actor| actor.original == original).count() > 1 {
            return format!("Archive author {} · {}", original.as_str().get(..8).unwrap_or_default(), profile.as_str().get(..8).unwrap_or_default());
        }
        return format!("Archive author {}", original.as_str().get(..8).unwrap_or_default());
    }
    display_name(app, profile).to_owned()
}
fn directory_index(row: usize, excluded: Option<usize>) -> usize {
    row.saturating_add(usize::from(excluded.is_some_and(|index| row >= index)))
}
fn members(app: &mut PhotocraftApp, ui: &mut egui::Ui, idle: bool, owner: bool) {
    let refresh = ui.button("Refresh people and members");
    remember(ui, "world.people.refresh", &refresh);
    if refresh.clicked() {
        run(app, "komaphone.world.people", json!({}));
        run(app, "komaphone.world.members", json!({}));
    }
    list_rows(ui, "world-member-list", 1, app.session.worlds.members.len(), |ui, index| {
        let Some(member) = app.session.worlds.members.get(index) else { return };
        let (profile, is_owner) = (member.profile, member.owner);
        ui.horizontal(|ui| {
            let name = display_name(app, profile);
            let label = if is_owner { format!("{name} · owner") } else { name.to_owned() };
            let width = if owner && !is_owner { (ui.available_width() - 70.0).max(1.0) } else { ui.available_width() };
            ui.add_sized([width, ui.spacing().interact_size.y], egui::Label::new(&label).truncate()).on_hover_text(&label);
            if owner && !is_owner {
                button(app, ui, "Remove", "komaphone.world.remove", json!({"member":profile}), idle);
            }
        });
    });
    if owner {
        let selected = app.ui.world.invite_to.map_or("Choose a person", |id| display_name(app, id)).to_owned();
        craft_ui::ComboBox::from_id_salt("world-invite-person").selected_text(selected).show_ui(ui, |ui| {
            let excluded = app.session.worlds.account.as_ref().and_then(|account| person_index(&app.session.worlds.people, account.profile));
            let count = app.session.worlds.people.len().saturating_sub(usize::from(excluded.is_some()));
            list_rows(ui, "world-invite-people", 1, count, |ui, row| {
                if let Some(person) = app.session.worlds.people.get(directory_index(row, excluded)) {
                    ui.selectable_value(&mut app.ui.world.invite_to, Some(person.profile), &person.display_name);
                }
            });
        });
        if let Some(recipient) = app.ui.world.invite_to {
            button(app, ui, "Create invitation", "komaphone.world.invite", json!({"recipient":recipient}), idle);
        }
        if let Some(invitation) = &app.session.worlds.invitation {
            ui.label("This invitation is for the selected person. Share it with them to accept.");
            let response = ui.button("Copy invitation");
            remember(ui, "world.invitation.copy", &response);
            if response.clicked() {
                ui.ctx().copy_text(invitation.to_string());
            }
        }
    } else {
        button(app, ui, "Leave canvas", "komaphone.world.leave", json!({}), idle);
    }
}
fn review(app: &mut PhotocraftApp, ui: &mut egui::Ui, idle: bool) {
    let Some(active) = app.session.worlds.active.as_ref() else { return };
    let Some(review) = active.viewer.review.clone() else { return };
    let available = active.canonical.revision;
    ui.label(format!("First-visit baseline: revision {}", review.baseline_revision));
    ui.label(format!("{} changes since your last review", available.saturating_sub(review.reviewed_revision)));
    if let Some(interval) = review.interval {
        let mut speed = interval.speed;
        let response = ui.add(egui::Slider::new(&mut speed, 0.25..=8.0).logarithmic(true).text("Playback speed"));
        remember(ui, "world.review.speed", &response);
        if response.changed() {
            run(app, "komaphone.world.review.control", json!({"state":if interval.playing{"play"}else{"pause"},"speed":speed}));
        }
        let mut position = interval.position;
        let response = ui.add(egui::Slider::new(&mut position, interval.start_exclusive..=interval.end_inclusive).text("Revision"));
        remember(ui, "world.review.scrub", &response);
        if response.changed() {
            run(app, "komaphone.world.review.scrub", json!({"revision":position}));
        }
        ui.horizontal(|ui| {
            button(
                app,
                ui,
                if interval.playing { "Pause" } else { "Play" },
                "komaphone.world.review.control",
                json!({"state":if interval.playing{"pause"}else{"play"}}),
                idle,
            );
            button(app, ui, "Exit review", "komaphone.world.review.control", json!({"state":"cancel"}), idle);
        });
        button(app, ui, "Mark these changes seen", "komaphone.world.review.markSeen", json!({}), idle);
        ui.small("New changes arriving during playback stay unseen. Drawing resumes after you exit review.");
        super::review::show(app, ui, &interval);
    } else {
        button(app, ui, "Review changes", "komaphone.world.review.begin", json!({}), idle);
    }
}
fn tours(app: &mut PhotocraftApp, ui: &mut egui::Ui, idle: bool) {
    let Some(active) = app.session.worlds.active.as_ref() else { return };
    let camera = active.viewer.camera.clone();
    let world = active.display.clone();
    let tours = world.tours.values().collect::<Vec<_>>();
    let label = ui.label("Tour name");
    ui.text_edit_singleline(&mut app.ui.world.tour_name).labelled_by(label.id);
    if ui.add_enabled(idle && app.ui.world.tour_stops.len() < 128, egui::Button::new("Add this view as a stop")).clicked() && !app.ui.world.unfinished() {
        let travel_ms = if app.ui.world.tour_stops.is_empty() { 0 } else { 1500 };
        app.ui.world.tour_stops.push(TourStop { camera, travel_ms, pause_ms: 1000, target: None });
    }
    ui.label(format!("{} draft stops", app.ui.world.tour_stops.len()));
    let mut remove = None;
    let mut reorder = None;
    for (index, stop) in app.ui.world.tour_stops.iter_mut().enumerate() {
        ui.push_id(index, |ui| {
            ui.label(format!("Stop {}", index + 1));
            ui.horizontal(|ui| {
                ui.label("Travel ms");
                ui.add_enabled(index > 0, egui::DragValue::new(&mut stop.travel_ms).range(0..=120_000));
                ui.label("Pause ms");
                ui.add(egui::DragValue::new(&mut stop.pause_ms).range(0..=120_000));
                if ui.small_button("Remove").clicked() {
                    remove = Some(index);
                }
                if index > 0 && ui.small_button("Move up").clicked() {
                    reorder = Some((index, index - 1));
                }
            });
        });
    }
    if let Some((from, to)) = reorder
        && from < app.ui.world.tour_stops.len()
        && to < app.ui.world.tour_stops.len()
    {
        app.ui.world.tour_stops.swap(from, to);
        if let Some(first) = app.ui.world.tour_stops.first_mut() {
            first.travel_ms = 0;
        }
    }
    if let Some(index) = remove {
        app.ui.world.tour_stops.remove(index);
        if let Some(first) = app.ui.world.tour_stops.first_mut() {
            first.travel_ms = 0;
        }
    }
    let response =
        ui.add_enabled(idle && !app.ui.world.tour_stops.is_empty() && app.session.is_enabled("komaphone.world.tour.put"), egui::Button::new("Save tour"));
    remember(ui, "komaphone.world.tour.put", &response);
    if response.clicked() && !app.ui.world.unfinished() {
        let mut params = json!({"name":app.ui.world.tour_name,"stops":app.ui.world.tour_stops});
        if let Some((id, revision)) = app.ui.world.tour_edit {
            params["id"] = json!(id);
            params["expected_revision"] = json!(revision);
        }
        run(app, "komaphone.world.tour.put", params);
    }
    if ui.button("Start a new tour").clicked() {
        app.ui.world.tour_edit = None;
        app.ui.world.tour_stops.clear();
    }
    list_rows(ui, "world-tour-list", 3, tours.len(), |ui, index| {
        let Some(tour) = tours.get(index) else { return };
        ui.push_id(tour.id.as_str(), |ui| {
            ui.add(egui::Label::new(&tour.name).truncate()).on_hover_text(&tour.name);
            ui.horizontal(|ui| {
                button(app, ui, "Play", "komaphone.world.tour.play", json!({"id":tour.id}), idle);
                if ui.button("Edit stops").clicked() {
                    app.ui.world.tour_name = tour.name.clone();
                    app.ui.world.tour_stops = tour.stops.clone();
                    app.ui.world.tour_edit = Some((tour.id, tour.revision));
                }
                button(app, ui, "Remove", "komaphone.world.tour.remove", json!({"id":tour.id}), idle);
            });
            if tour.missing_stops(&world).is_ok_and(|missing| !missing.is_empty()) {
                ui.horizontal(|ui| {
                    ui.small("Missing stop").on_hover_text("A tour destination is missing. Keep the route, or explicitly remove missing stops.");
                    button(app, ui, "Skip missing stops", "komaphone.world.tour.skipMissing", json!({"id":tour.id,"expected_revision":tour.revision}), idle);
                });
            }
        });
    });
    if let Some(playback) = app.session.worlds.tour_playback.clone() {
        if let Some(duration) = world.tours.get(&playback.tour).and_then(|tour| tour.duration_ms().ok()) {
            let mut elapsed = playback.elapsed_ms;
            let response = ui.add(egui::Slider::new(&mut elapsed, 0..=duration).text("Tour ms"));
            remember(ui, "world.tour.scrub", &response);
            if response.changed() {
                run(app, "komaphone.world.tour.control", json!({"state":"pause","elapsed_ms":elapsed}));
            }
        }
        ui.horizontal(|ui| {
            button(
                app,
                ui,
                if playback.playing { "Pause" } else { "Resume" },
                "komaphone.world.tour.control",
                json!({"state":if playback.playing{"pause"}else{"resume"}}),
                idle,
            );
            button(app, ui, "Cancel", "komaphone.world.tour.control", json!({"state":"cancel"}), idle);
        });
    }
}
fn restore_archive(app: &mut PhotocraftApp, ui: &mut egui::Ui, idle: bool) {
    ui.label("Create a private canvas from a saved archive folder.");
    let label = ui.label("Archive folder");
    let response = ui.text_edit_singleline(&mut app.ui.world.export_path).labelled_by(label.id);
    remember(ui, "world.restore.path", &response);
    let busy = app.session.worlds.assets.export.as_ref().is_some_and(|status| status.busy);
    let ready = idle && !busy && !app.ui.world.export_path.trim().is_empty();
    button(app, ui, "Restore as a new private canvas", "komaphone.world.restore", json!({"path":app.ui.world.export_path}), ready);
    ui.small("The original stays unchanged. Historical credits come from the archive; invitations and account access are not transferred.");
    if let Some(status) = &app.session.worlds.assets.export {
        ui.label(&status.message);
        if status.total > 0 {
            ui.add(egui::ProgressBar::new(status.completed as f32 / status.total as f32));
        }
    }
    if busy {
        button(app, ui, "Cancel", "komaphone.world.export.cancel", json!({}), true);
    }
    if let Some(error) = &app.session.worlds.assets.error {
        ui.colored_label(Tokens::get(ui.ctx()).danger, error);
    }
}
fn media(app: &mut PhotocraftApp, ui: &mut egui::Ui, idle: bool) {
    ui.label("PNG captures this view. Tours and archives use the latest synced revision.");
    let label = ui.label("Output path");
    let response = ui.text_edit_singleline(&mut app.ui.world.export_path).labelled_by(label.id);
    remember(ui, "world.export.path", &response);
    ui.horizontal(|ui| {
        ui.label("Size");
        ui.add(egui::DragValue::new(&mut app.ui.world.export_width).range(1..=2048));
        ui.add(egui::DragValue::new(&mut app.ui.world.export_height).range(1..=2048));
    });
    ui.horizontal(|ui| {
        ui.label("Frames / sec");
        ui.add(egui::DragValue::new(&mut app.ui.world.export_fps).range(1..=24));
    });
    let world = app.session.worlds.active.as_ref().map(|active| active.canonical.clone());
    let routes = world.as_ref().map(|world| world.tours.values().collect::<Vec<_>>()).unwrap_or_default();
    let selected = routes.iter().find(|t| Some(t.id) == app.ui.world.export_tour).map_or("Choose a saved tour", |t| t.name.as_str());
    craft_ui::ComboBox::from_id_salt("world-export-tour").selected_text(selected).show_ui(ui, |ui| {
        list_rows(ui, "world-export-tour-list", 1, routes.len(), |ui, index| {
            if let Some(route) = routes.get(index) {
                ui.selectable_value(&mut app.ui.world.export_tour, Some(route.id), &route.name);
            }
        });
    });
    let busy = app.session.worlds.assets.export.as_ref().is_some_and(|status| status.busy);
    let ready = idle && !busy && !app.ui.world.export_path.trim().is_empty();
    ui.horizontal_wrapped(|ui| {
        for (label, format) in [("PNG view", "png"), ("Tour GIF", "gif"), ("Tour PNG frames", "sequence"), ("Editable archive", "archive")] {
            let mut params = json!({"format":format,"path":app.ui.world.export_path});
            if format != "archive" {
                params["width"] = json!(app.ui.world.export_width);
                params["height"] = json!(app.ui.world.export_height);
            }
            if format == "png" {
                params["elapsed_ms"] = json!(ui.input(|input| (input.time.max(0.0) * 1000.0) as u64));
            }
            if matches!(format, "gif" | "sequence") {
                params["tour"] = json!(app.ui.world.export_tour);
                params["fps"] = json!(app.ui.world.export_fps);
            }
            let response = ui.add_enabled(ready && (!matches!(format, "gif" | "sequence") || app.ui.world.export_tour.is_some()), egui::Button::new(label));
            remember(ui, &format!("world.export.{format}"), &response);
            if response.clicked() && !app.ui.world.unfinished() {
                run(app, "komaphone.world.export", params);
            }
        }
    });
    if let Some(status) = &app.session.worlds.assets.export {
        ui.label(&status.message);
        if status.total > 0 {
            ui.add(egui::ProgressBar::new(status.completed as f32 / status.total as f32));
        }
    }
    if busy {
        button(app, ui, "Cancel export", "komaphone.world.export.cancel", json!({}), true);
    }
    if let Some(error) = &app.session.worlds.assets.error {
        ui.colored_label(Tokens::get(ui.ctx()).danger, error);
    }
    ui.separator();
    ui.label("Place game artwork without changing its original or credits.");
    if let Some(source) = app.ui.world.source.clone() {
        let kind = if source.frame.is_some() { "Selected still" } else { "Complete game result" };
        ui.label(kind);
        button(
            app,
            ui,
            "Prepare this source",
            "komaphone.world.remix.prepare",
            json!({"room":source.room,"game":source.game,"chain":source.chain,"frame":source.frame,"fps":source.fps,"looping":source.looping}),
            idle,
        );
    } else {
        ui.small("Open a result in Gallery and choose Place in a canvas.");
    }
    let prepared = app.session.worlds.assets.prepared().is_some();
    if prepared {
        place_button(app, ui, "Place prepared result", "komaphone.world.remix.place", idle && !reviewing(app));
    }
    if app.session.worlds.copy_source.is_some() {
        place_button(app, ui, "Paste copied media here", "komaphone.world.remix.paste", idle && !reviewing(app));
    }
    let media = world.as_ref().map(|world| world.media.values().filter(|media| media.visible).collect::<Vec<_>>()).unwrap_or_default();
    list_rows(ui, "world-media-list", 2, media.len(), |ui, index| {
        let Some(item) = media.get(index) else { return };
        ui.push_id(item.media.id.as_str(), |ui| {
            let label = format!(
                "{} · {}",
                if item.media.frames.len() > 1 { "Animation" } else { "Drawing" },
                item.media.provenance.authors.iter().map(|c| c.label.as_str()).collect::<Vec<_>>().join(", ")
            );
            ui.add(egui::Label::new(&label).truncate()).on_hover_text(&label);
            button(app, ui, "Copy to another canvas", "komaphone.world.remix.copy", json!({"media":item.media.id}), idle);
        });
    });
    let strokes = world.as_ref().map(|world| world.strokes.values().filter(|stroke| stroke.visible).collect::<Vec<_>>()).unwrap_or_default();
    ui.label("Copy a stroke into your own editable layer");
    list_rows(ui, "world-stroke-copy-list", 2, strokes.len(), |ui, index| {
        let Some(source) = strokes.get(index) else { return };
        let id = source.stroke.id;
        ui.label(format!("{} · stroke {}", credit_name(app, source.stroke.owner), id.as_str().get(..8).unwrap_or_default()));
        let response = ui.add_enabled(idle, egui::Button::new("Copy stroke"));
        remember(ui, &format!("world.stroke.copy.{}", id.as_str()), &response);
        if response.clicked() && !app.ui.world.unfinished() {
            run(app, "komaphone.world.stroke.copy", json!({"stroke":id}));
        }
    });
    if app.session.worlds.stroke_copy_source.is_some() {
        paste_stroke(app, ui, idle && !reviewing(app));
    }
}
fn paste_stroke(app: &mut PhotocraftApp, ui: &mut egui::Ui, idle: bool) {
    let Some((source_world, source_id, credit)) = app.session.worlds.stroke_copy_source.as_ref() else { return };
    let Some(source) = source_world.strokes.get(source_id) else { return };
    let Some(active) = app.session.worlds.active.as_ref() else { return };
    ui.small(format!("Copied artwork by {}. Open the destination canvas and paste here.", credit.label));
    let Some(first) = source.stroke.points.first().copied() else { return };
    let (mut low, mut high) = (first, first);
    for point in &source.stroke.points {
        low[0] = low[0].min(point[0]);
        low[1] = low[1].min(point[1]);
        high[0] = high[0].max(point[0]);
        high[1] = high[1].max(point[1]);
    }
    let camera = &active.viewer.camera;
    let span = (high[0] - low[0]).max(high[1] - low[1]).max(source.stroke.width);
    let scale = (300.0 / (span * camera.zoom)).min(1_000_000.0);
    let Ok(center) = scene::coordinates::center(camera).and_then(|point| scene::coordinates::local_at(&camera.region, point)) else { return };
    let offset = [center[0] - (low[0] + high[0]) * 0.5 * scale, center[1] - (low[1] + high[1]) * 0.5 * scale];
    button(app, ui, "Paste copied stroke here", "komaphone.world.stroke.paste", json!({"anchor":camera.region,"offset":offset,"scale":scale}), idle);
}
fn place_button(app: &mut PhotocraftApp, ui: &mut egui::Ui, label: &str, command: &str, idle: bool) {
    let Some(active) = app.session.worlds.active.as_ref() else { return };
    let camera = &active.viewer.camera;
    let size = 300.0 / camera.zoom;
    let frame = if command.ends_with(".paste") {
        app.session.worlds.copy_source.as_ref().and_then(|(_, m)| m.media.frames.first())
    } else {
        app.session.worlds.assets.prepared().and_then(|m| m.frames.first())
    };
    let ratio = frame.map_or(1.0, |frame| f64::from(frame.height) / f64::from(frame.width.max(1)));
    let height = size * ratio;
    let center = scene::coordinates::center(camera).and_then(|point| scene::coordinates::local_at(&camera.region, point));
    let params = match center {
        Ok([x, y]) => json!({"anchor":camera.region,"offset":[x-size/2.0,y-height/2.0],"width":size,"height":height}),
        Err(_) => return,
    };
    button(app, ui, label, command, params, idle);
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Rect;

    #[test]
    fn archive_restore_is_visible_without_an_existing_canvas() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.session.worlds.account = Some(photocraft_engine::worlds::Account {
            endpoint: "http://127.0.0.1:41234".into(),
            profile: scene::OwnerId::new(&"1".repeat(32)).unwrap(),
            device: scene::shared::DeviceId::new(&"2".repeat(32)).unwrap(),
        });
        let ctx = egui::Context::default();
        for _ in 0..2 {
            ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))), ..Default::default() }, |ui| {
                sidebar(&mut app, ui);
            })
            .textures_delta
            .clear();
        }
        assert!(app.session.worlds.active.is_none());
        assert!(crate::komaphone_workspace::control_rect(&ctx, "world.section.restore").is_some());
    }

    #[test]
    fn archive_credits_do_not_impersonate_live_directory_names() {
        use photocraft_engine::worlds::{Canvas, Viewer};
        use std::sync::Arc;
        let owner = scene::OwnerId::new(&"1".repeat(32)).unwrap();
        let source = scene::shared::WorldState::new(scene::CanvasId::new(&"2".repeat(32)).unwrap(), owner, "Source".into()).unwrap();
        let canvas = scene::CanvasId::new(&"3".repeat(32)).unwrap();
        let world = Arc::new(source.import_to(canvas, owner).unwrap());
        let alias = world.import_origin.as_ref().unwrap().actors[0].imported;
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.session.worlds.people = vec![
            scene::world_protocol::Person { profile: owner, display_name: "Live importer".into() },
            scene::world_protocol::Person { profile: alias, display_name: "A live account with the same ID".into() },
        ];
        app.session.worlds.people.sort_by_key(|person| person.profile);
        app.session.worlds.active = Some(Canvas {
            info: scene::world_protocol::CanvasSummary {
                canvas,
                owner,
                name: world.name.clone(),
                initial_name: world.name.clone(),
                revision: 0,
                epoch: 1,
                import_origin: world.import_origin.clone(),
            },
            canonical: Arc::clone(&world),
            display: world,
            pending: 0,
            viewer: Viewer { camera: scene::coordinates::home(canvas), bookmarks: vec![], review: None },
            notice: None,
        });
        assert_eq!(credit_name(&app, alias), "Archive author 11111111");
        assert_eq!(credit_name(&app, owner), "Live importer");
    }

    #[test]
    fn large_world_directory_only_builds_visible_controls() {
        let ctx = egui::Context::default();
        let mut visited = Vec::new();
        ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 600.0))), ..Default::default() }, |ui| {
            visited.clear();
            list_rows(ui, "world-large-directory-test", 1, 65_536, |ui, index| {
                visited.push(index);
                ui.label(format!("Canvas {index}"));
            });
        })
        .textures_delta
        .clear();
        assert!(!visited.is_empty());
        assert!(visited.len() < 32, "Only a bounded viewport should construct controls: {}", visited.len());
        assert_eq!(visited.first(), Some(&0));
        assert!(visited.windows(2).all(|pair| pair[1] == pair[0] + 1));
    }

    #[test]
    fn ordered_people_lookup_and_invitation_rows_keep_every_other_person() {
        let mut people = (1..=1000)
            .map(|number| scene::world_protocol::Person {
                profile: scene::OwnerId::new(&format!("{number:032x}")).unwrap(),
                display_name: format!("Person {number}"),
            })
            .collect::<Vec<_>>();
        for excluded in [0, 500, 999] {
            let profile = people[excluded].profile;
            assert_eq!(person_index(&people, profile), Some(excluded));
            let rows = (0..people.len() - 1).map(|row| directory_index(row, Some(excluded))).collect::<Vec<_>>();
            assert_eq!(rows, (0..people.len()).filter(|index| *index != excluded).collect::<Vec<_>>());
        }
        let profile = people[500].profile;
        people[500].display_name = "Updated name".into();
        assert_eq!(people[person_index(&people, profile).unwrap()].display_name, "Updated name");
        assert_eq!(person_index(&people, scene::OwnerId::new(&format!("{:032x}", 1001)).unwrap()), None);
        assert_eq!(directory_index(999, None), 999);
    }
}
