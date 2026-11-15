//! Stable gallery/replay/export actions, shared by UI and authorized automation.
use crate::commands::{CommandSpec, always};
use crate::gallery::GalleryAction;
use crate::{EngineError, Result, Session};
use serde_json::{Value, json};

fn available(session: &Session) -> std::result::Result<(), String> {
    if session.gallery.available() { Ok(()) } else { Err("Gallery requires the native app's local storage".into()) }
}
fn idle(session: &Session) -> std::result::Result<(), String> {
    available(session)?;
    if session.gallery.busy() { Err("Gallery is working".into()) } else { Ok(()) }
}
fn fields(params: &Value, allowed: &[&str]) -> Result<()> {
    if params.is_null() && allowed.is_empty() {
        return Ok(());
    }
    let object = params.as_object().ok_or_else(|| EngineError::Other("Gallery parameters must be an object".into()))?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(EngineError::Other("Unknown gallery parameter".into()));
    }
    Ok(())
}
fn request(session: &mut Session, params: &Value, action: &str) -> Result<Value> {
    session.gallery_action(GalleryAction::from_params(params, action)?)?;
    Ok(json!({"pending":true,"saved":false}))
}

pub fn specs() -> Vec<CommandSpec> {
    let mut specs = vec![
        CommandSpec {
            id: "komaphone.gallery.status",
            label: "Gallery status",
            menu: &[],
            shortcut: None,
            params: r#"{"replay_after":integer=0,"replay_limit":integer=128(max512)}"#,
            enabled: always,
            journal: false,
            run: |s, p| {
                fields(p, &["replay_after", "replay_limit"])?;
                let index = |name: &str, default: usize| -> Result<usize> {
                    p.get(name).map_or(Ok(default), |value| {
                        value.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| EngineError::Other(format!("Invalid {name}")))
                    })
                };
                let after = index("replay_after", 0)?;
                let limit = index("replay_limit", 128)?;
                if !(1..=512).contains(&limit) {
                    return Err(EngineError::Other("Replay page size must be 1–512".into()));
                }
                s.poll_rooms();
                s.poll_gallery();
                let items: Vec<_> = s.gallery.items.iter().filter(|item| s.gallery_item_allowed(item)).collect();
                let active = s.gallery.active.as_ref().map(|active| {
                    let content = match &active.content {
                        photocraft_party::gallery::GalleryContent::Game { .. } => json!(active.content),
                        photocraft_party::gallery::GalleryContent::Project { checkpoint, replay, .. } => json!({"kind":"project","checkpoint":checkpoint,"replay_steps":replay.as_ref().map_or(0, |replay| replay.steps.len())}),
                    };
                    json!({"schema_version":active.schema_version,"id":active.id,"title":active.title,"created_at":active.created_at,"updated_at":active.updated_at,"authors":active.authors,"content":content,"provenance":active.provenance,"fps":active.fps,"looping":active.looping,"notes":active.notes})
                });
                let replay = s.gallery.replay.as_ref().map(|replay| {
                    let moments: Vec<_> = replay.moments.iter().skip(after).take(limit).collect();
                    let next = after.saturating_add(moments.len());
                    json!({"duration_ms":replay.duration_ms,"baseline_only":replay.baseline_only,"missing":replay.missing,"moments":moments,"moment_count":replay.moments.len(),"next":(next<replay.moments.len()).then_some(next)})
                });
                Ok(json!({"available":s.gallery.available(),"busy":s.gallery.busy(),"items":items,"active":active,
                "activeDraft":s.gallery.active_draft,"replay":replay,"imageRevision":s.gallery.image_revision,
                "image":s.gallery.image.as_ref().map(|image|json!({"width":image.width,"height":image.height})),
                "remote":s.gallery.remote,"remoteCursor":s.gallery.remote_cursor,"downloading":s.gallery.downloading,
                "downloadedBytes":s.gallery.downloaded_bytes,"downloadBytes":s.gallery.download_bytes,"waiting":s.gallery.waiting,
                "usageBytes":s.gallery.usage_bytes,"quotaBytes":s.gallery.quota_bytes,"error":s.gallery.error,"notice":s.gallery.notice}))
            },
        },
        CommandSpec {
            id: "komaphone.gallery.download",
            label: "Save revealed game to gallery",
            menu: &[],
            shortcut: None,
            params: r#"{"game":id?}"#,
            enabled: available,
            journal: false,
            run: |s, p| {
                fields(p, &["game"])?;
                let game =
                    p.get("game").map(|value| serde_json::from_value(value.clone()).map_err(|_| EngineError::Other("Invalid game ID".into()))).transpose()?;
                s.download_gallery_game(game)?;
                Ok(json!({"pending":true,"saved":false}))
            },
        },
        CommandSpec {
            id: "komaphone.gallery.history",
            label: "Previous room games",
            menu: &[],
            shortcut: None,
            params: r#"{"after_revision":integer?}"#,
            enabled: available,
            journal: false,
            run: |s, p| {
                fields(p, &["after_revision"])?;
                let after_revision = match p.get("after_revision") {
                    Some(value) => value.as_u64().ok_or_else(|| EngineError::Other("Invalid archive page revision".into()))?,
                    None => 0,
                };
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let room = s.rooms.status.room.as_ref().ok_or_else(|| EngineError::Other("Connect to a private room first".into()))?.id;
                    if after_revision == 0 {
                        s.gallery.remote.clear();
                    }
                    s.room_request(photocraft_room_client::ClientCommand::History { room, after_revision })?;
                    Ok(json!({"pending":true}))
                }
                #[cfg(target_arch = "wasm32")]
                {
                    let _ = (s, after_revision);
                    Err(EngineError::Other("Private room history requires the native app".into()))
                }
            },
        },
        CommandSpec {
            id: "komaphone.gallery.cancelDownload",
            label: "Cancel gallery download",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: available,
            journal: false,
            run: |s, p| {
                fields(p, &[])?;
                #[cfg(not(target_arch = "wasm32"))]
                {
                    s.room_request(photocraft_room_client::ClientCommand::CancelDownload)?;
                    s.gallery.downloading = false;
                    Ok(json!({"cancelled":true}))
                }
                #[cfg(target_arch = "wasm32")]
                {
                    let _ = s;
                    Err(EngineError::Other("Private downloads require the native app".into()))
                }
            },
        },
    ];
    macro_rules! action {
        ($id:literal,$label:literal,$params:literal,$action:literal) => {
            specs.push(CommandSpec {
                id: $id,
                label: $label,
                menu: &[],
                shortcut: None,
                params: $params,
                enabled: idle,
                journal: false,
                run: |s, p| request(s, p, $action),
            });
        };
    }
    action!("komaphone.gallery.list", "Open gallery", "{}", "refresh");
    action!("komaphone.gallery.open", "Open gallery original", r#"{"entry":id,"draft":bool=false}"#, "open");
    action!(
        "komaphone.gallery.update",
        "Rename, favorite or trash",
        r#"{"entry":id,"draft":bool=false,"title":string?,"favorite":bool?,"trashed":bool?}"#,
        "update"
    );
    action!("komaphone.gallery.thumbnail", "Load gallery thumbnail", r#"{"entry":id,"draft":bool=false}"#, "thumbnail");
    action!(
        "komaphone.gallery.render",
        "View frame or drawing replay",
        r#"{"chain":integer,"frame":integer,"step":integer?,"elapsed_ms":integer?,"creature":bool=false}"#,
        "render"
    );
    action!("komaphone.gallery.remix", "Remix as a new local project", r#"{"chain":integer,"frame":integer?}"#, "remix");
    action!("komaphone.gallery.playback", "Save animation playback settings", r#"{"fps":integer,"looping":bool}"#, "playback");
    action!("komaphone.gallery.previewGif", "Preview encoded GIF", r#"{"chain":integer,"frame":integer,"fps":integer,"looping":bool}"#, "preview_gif");
    specs.push(CommandSpec {
        id: "komaphone.gallery.notes",
        label: "Open archived handoff notes",
        menu: &[],
        shortcut: None,
        params: "{}",
        enabled: available,
        journal: false,
        run: |s, p| {
            fields(p, &[])?;
            #[cfg(not(target_arch = "wasm32"))]
            {
                let active = s.gallery.active.as_ref().ok_or_else(|| EngineError::Other("Open a saved game first".into()))?;
                let photocraft_party::gallery::GalleryContent::Game { room } = &active.content else {
                    return Err(EngineError::Other("Local drawings have no handoff notes".into()));
                };
                let game = active.game().ok_or_else(|| EngineError::Other("No revealed game".into()))?;
                s.room_request(photocraft_room_client::ClientCommand::GameNotes { room: room.id, game })?;
                Ok(json!({"pending":true}))
            }
            #[cfg(target_arch = "wasm32")]
            {
                let _ = s;
                Err(EngineError::Other("Historical notes require the native app".into()))
            }
        },
    });
    action!(
        "komaphone.gallery.export",
        "Export gallery artwork",
        r#"{"format":"archive|png|gif|sequence","path":string,"chain":integer,"frame":integer?,"fps":integer,"looping":bool}"#,
        "export"
    );
    action!("komaphone.gallery.import", "Import portable archive", r#"{"path":string}"#, "import");
    action!("komaphone.gallery.backup", "Back up all local originals", r#"{"path":string}"#, "backup");
    action!("komaphone.gallery.restore", "Restore into a new data directory", r#"{"path":string,"destination":string}"#, "restore");
    specs
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_or_unconfigured_commands_fail_without_mutation() {
        let mut session = Session::new();
        for spec in specs() {
            for params in [json!([]), json!(false), json!({"unexpected":true})] {
                assert!(session.execute(spec.id, params).is_err(), "{}", spec.id);
            }
        }
        assert!(session.execute("komaphone.gallery.remix", json!({"chain":0})).is_err());
        assert!(session.documents().is_empty());
    }
}
