//! Local party presentation commands share authorization with the native UI.
use crate::commands::{CommandSpec, always};
use crate::{EngineError, Result, Session};
use serde_json::{Value, json};

fn fields(params: &Value, allowed: &[&str]) -> Result<()> {
    if params.is_null() && allowed.is_empty() {
        return Ok(());
    }
    let object = params.as_object().ok_or_else(|| EngineError::Other("Expected game parameters as an object".into()))?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(EngineError::Other("Unknown game parameter".into()));
    }
    Ok(())
}
fn member(session: &Session) -> std::result::Result<(), String> {
    let room = session.rooms.status.room.as_ref().ok_or("Join a private room first")?;
    if !room.members.iter().any(|m| Some(m.participant.id) == session.rooms.status.profile.as_ref().map(|p| p.id)) {
        return Err("Waiting participants cannot open this game's artwork".into());
    }
    Ok(())
}
fn revealed(session: &Session) -> std::result::Result<(), String> {
    member(session)?;
    if session
        .rooms
        .status
        .room
        .as_ref()
        .is_none_or(|r| r.reveal.is_empty() || !matches!(r.phase, photocraft_party::room::Phase::Reveal | photocraft_party::room::Phase::Archived))
    {
        return Err("Finish the game before opening its reveal".into());
    }
    Ok(())
}
fn copy(session: &mut Session, params: &Value) -> Result<Value> {
    fields(params, &[])?;
    let room = session.rooms.status.room.as_ref().ok_or_else(|| EngineError::Other("No room".into()))?;
    let actor = session.rooms.status.profile.as_ref().ok_or_else(|| EngineError::Other("No profile".into()))?.id;
    let (asset, _) = crate::rooms::animation::previous_reference(room, actor)?.ok_or_else(|| EngineError::Other("No previous frame".into()))?;
    let bytes = session.rooms.media.png(asset.hash).ok_or_else(|| EngineError::Other("The previous frame is still loading; retry when visible".into()))?;
    let layer = crate::rooms::animation::copy_previous_frame(session, &bytes)?;
    Ok(json!({"layer":layer.0}))
}
fn save_note(session: &mut Session, params: &Value) -> Result<Value> {
    fields(params, &["note"])?;
    let note = params.get("note").and_then(Value::as_str).ok_or_else(|| EngineError::Other("note must be text".into()))?;
    photocraft_party::validate_label(note, photocraft_party::MAX_NOTE_BYTES, true).map_err(|e| EngineError::Other(e.to_string()))?;
    let assignment = session.rooms.status.room.as_ref().and_then(|r| r.assignment.as_ref()).ok_or_else(|| EngineError::Other("No active assignment".into()))?;
    if assignment.submitted
        || Some(assignment.assignment.player) != session.rooms.status.profile.as_ref().map(|profile| profile.id)
        || session.rooms.status.room.as_ref().is_none_or(|r| r.phase != photocraft_party::room::Phase::Drawing)
        || session.rooms.submission_project() == Some(assignment.assignment.project)
        || session.local_projects.preparing_room
    {
        return Err(EngineError::Other("This assignment's handoff note is frozen for submission".into()));
    }
    let draft = photocraft_room_client::NoteDraft { assignment: assignment.assignment.id, note: note.into() };
    #[cfg(not(target_arch = "wasm32"))]
    {
        session.room_request(photocraft_room_client::ClientCommand::SaveNote(draft))?;
        Ok(json!({"queued":true,"saved":false}))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = draft;
        Err(EngineError::Other("Private rooms require the native app".into()))
    }
}
fn save_reveal(session: &mut Session, params: &Value) -> Result<Value> {
    fields(params, &["game", "chain", "frame", "fps", "looping", "visible_parts"])?;
    let position: photocraft_room_client::RevealPosition =
        serde_json::from_value(params.clone()).map_err(|_| EngineError::Other("Invalid reveal position".into()))?;
    let room = session.rooms.status.room.as_ref().ok_or_else(|| EngineError::Other("No revealed game".into()))?;
    let valid_game = room.reveal.iter().any(|entry| match entry {
        photocraft_party::room::RevealedEntry::Artwork { assignment, .. } | photocraft_party::room::RevealedEntry::Missing { assignment, .. } => {
            assignment.id.game == position.game
        }
    });
    if !valid_game
        || usize::from(position.chain) >= room.members.len()
        || position.frame >= room.mode.rounds(room.members.len()).map_err(|e| EngineError::Other(e.to_string()))?
        || !(1..=24).contains(&position.fps)
        || !(1..=3).contains(&position.visible_parts)
    {
        return Err(EngineError::Other("Reveal position is outside this revealed game".into()));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        session.room_request(photocraft_room_client::ClientCommand::SaveReveal(position))?;
        Ok(json!({"queued":true,"saved":false}))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (session, position);
        Err(EngineError::Other("Private rooms require the native app".into()))
    }
}
fn notes(session: &mut Session, params: &Value) -> Result<Value> {
    fields(params, &[])?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        session.room_request(photocraft_room_client::ClientCommand::Notes)?;
        Ok(json!({"queued":true}))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = session;
        Err(EngineError::Other("Private rooms require the native app".into()))
    }
}
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "komaphone.animation.copyPrevious",
            label: "Copy previous frame",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: crate::rooms::animation::can_copy_previous,
            run: copy,
            journal: true,
        },
        CommandSpec {
            id: "komaphone.room.setNote",
            label: "Save handoff note locally",
            menu: &[],
            shortcut: None,
            params: r#"{"note":string}"#,
            enabled: member,
            run: save_note,
            journal: false,
        },
        CommandSpec {
            id: "komaphone.reveal.position",
            label: "Remember reveal position",
            menu: &[],
            shortcut: None,
            params: r#"{"game":id,"chain":integer,"frame":integer,"fps":integer,"looping":bool,"visible_parts":integer}"#,
            enabled: revealed,
            run: save_reveal,
            journal: false,
        },
        CommandSpec {
            id: "komaphone.reveal.notes",
            label: "Open handoff notes",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: revealed,
            run: notes,
            journal: false,
        },
        CommandSpec {
            id: "komaphone.room.mediaStatus",
            label: "Authorized room image cache",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: always,
            journal: false,
            run: |session, params| {
                fields(params, &[])?;
                session.poll_rooms();
                session.rooms.media.reconcile(&session.rooms.status);
                let allowed = session
                    .rooms
                    .status
                    .room
                    .as_ref()
                    .zip(session.rooms.status.profile.as_ref())
                    .map(|(room, actor)| photocraft_room_client::media::images(room, actor.id))
                    .unwrap_or_default();
                Ok(
                    json!({"allowed":allowed,"cached":session.rooms.media.images.keys().collect::<Vec<_>>(),"error":session.rooms.media.error,"notes":session.rooms.media.notes}),
                )
            },
        },
    ]
}
