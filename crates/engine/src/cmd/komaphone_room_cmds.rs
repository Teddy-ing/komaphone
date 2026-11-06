//! Stable room commands shared by native UI and control automation. None enters Actions,
//! the drawing journal or replay: invitation capabilities must never be recorded there.
use crate::commands::{CommandSpec, always};
use crate::{EngineError, Result, Session};
use photocraft_party::room::{Action, Phase, RoomView};
use photocraft_party::{Mode, ParticipantId, RequestId, RoomId};
use serde_json::{Value, json};

fn fields(params: &Value, allowed: &[&str]) -> Result<()> {
    if params.is_null() && allowed.is_empty() {
        return Ok(());
    }
    let object = params.as_object().ok_or_else(|| EngineError::Other("Expected room parameters as an object".into()))?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(EngineError::Other("Unknown room parameter".into()));
    }
    Ok(())
}
fn text<'a>(params: &'a Value, key: &str, limit: usize) -> Result<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= limit)
        .ok_or_else(|| EngineError::Other(format!("{key} must be a nonempty string of at most {limit} bytes")))
}
fn available(session: &Session) -> std::result::Result<(), String> {
    if session.rooms.available() { Ok(()) } else { Err("Private rooms require Windows room storage".into()) }
}
fn idle(session: &Session) -> std::result::Result<(), String> {
    available(session)?;
    if session.rooms.busy() {
        return Err("Wait for pending room work or resolve its error".into());
    }
    if !session.rooms.status.paired {
        return Err("Connect your profile to the service first".into());
    }
    Ok(())
}
fn room(session: &Session) -> std::result::Result<&RoomView, String> {
    idle(session)?;
    session.rooms.status.room.as_ref().ok_or_else(|| "Create or join a room first".into())
}
fn member(session: &Session) -> std::result::Result<&RoomView, String> {
    let room = room(session)?;
    let actor = session.rooms.status.profile.as_ref().map(|p| p.id);
    if !room.members.iter().any(|m| Some(m.participant.id) == actor) {
        return Err("Waiting for the next game".into());
    }
    Ok(room)
}
fn moderator(session: &Session) -> std::result::Result<&RoomView, String> {
    let room = member(session)?;
    if session.rooms.status.profile.as_ref().map(|p| p.id) != Some(room.moderator) {
        return Err("Only the moderator can do this".into());
    }
    Ok(room)
}
fn lobby_member(session: &Session) -> std::result::Result<(), String> {
    if member(session)?.phase != Phase::Lobby {
        return Err("Available in the lobby".into());
    }
    Ok(())
}
fn lobby_host(session: &Session) -> std::result::Result<(), String> {
    if moderator(session)?.phase != Phase::Lobby {
        return Err("Available in the lobby".into());
    }
    Ok(())
}
fn active_host(session: &Session) -> std::result::Result<(), String> {
    if !matches!(moderator(session)?.phase, Phase::Drawing | Phase::Transfer) {
        return Err("Available during an active game".into());
    }
    Ok(())
}
fn assignment(session: &Session) -> std::result::Result<&RoomView, String> {
    let room = member(session)?;
    if !matches!(room.phase, Phase::Drawing | Phase::Transfer) || room.assignment.as_ref().is_none_or(|a| a.submitted) {
        return Err("No editable assignment is available".into());
    }
    Ok(room)
}
fn can_open(session: &Session) -> std::result::Result<(), String> {
    if assignment(session)?.phase != Phase::Drawing {
        return Err("The timer has frozen this drawing".into());
    }
    if !session.local_projects.available() || session.local_projects.loading() {
        return Err("Wait for local drawing storage".into());
    }
    Ok(())
}
fn can_submit(session: &Session) -> std::result::Result<(), String> {
    let room = assignment(session)?;
    let doc = session.active().ok_or_else(|| "Open the assigned drawing".to_string())?;
    let project = session.local_projects.status(doc.doc.id).project;
    if !session.local_document_saved(doc.doc.id) || project != room.assignment.as_ref().map(|a| a.assignment.project) {
        return Err("Open the assigned drawing and wait for its local save".into());
    }
    Ok(())
}
fn mode(params: &Value) -> Result<Mode> {
    let mode: Mode = serde_json::from_value(params.get("mode").cloned().unwrap_or_else(|| json!({"kind":"animation","rotations":2})))
        .map_err(|_| EngineError::Other("Mode must be animation with 1..4 rotations or exquisite_corpse".into()))?;
    mode.validate().map_err(|e| EngineError::Other(e.to_string()))?;
    Ok(mode)
}
fn timer(params: &Value) -> Result<Option<u64>> {
    match params.get("timer_ms") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|n| (1..=photocraft_party::MAX_TIMER_MS).contains(n))
            .map(Some)
            .ok_or_else(|| EngineError::Other("Timer must be null (untimed) or 1..3600000 milliseconds".into())),
    }
}
fn action(session: &mut Session, value: Action) -> Result<Value> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        session.room_request(photocraft_room_client::ClientCommand::Action(Box::new(value)))?;
        Ok(json!({"queued":true,"accepted":false}))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (session, value);
        Err(EngineError::Other("Private rooms require Windows".into()))
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn request(session: &mut Session, command: photocraft_room_client::ClientCommand) -> Result<Value> {
    session.room_request(command)?;
    Ok(json!({"queued":true,"accepted":false}))
}

fn connect(session: &mut Session, params: &Value) -> Result<Value> {
    fields(params, &["endpoint", "display_name"])?;
    let endpoint = text(params, "endpoint", 2048)?;
    let display_name = text(params, "display_name", 96)?;
    photocraft_party::contracts::validate_label(display_name, 96, false).map_err(|e| EngineError::Other(e.to_string()))?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        let endpoint = photocraft_room_client::normalize_endpoint(endpoint).map_err(|e| EngineError::Other(e.to_string()))?;
        request(session, photocraft_room_client::ClientCommand::Connect { endpoint, display_name: display_name.into() })
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (session, endpoint);
        Err(EngineError::Other("Private rooms require Windows".into()))
    }
}
fn create(session: &mut Session, params: &Value) -> Result<Value> {
    fields(params, &["mode", "timer_ms"])?;
    let (mode, timer_ms) = (mode(params)?, timer(params)?);
    #[cfg(not(target_arch = "wasm32"))]
    {
        request(session, photocraft_room_client::ClientCommand::Create { mode, timer_ms })
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (session, mode, timer_ms);
        Err(EngineError::Other("Private rooms require Windows".into()))
    }
}
fn join(session: &mut Session, params: &Value) -> Result<Value> {
    fields(params, &["room", "invitation"])?;
    let room = RoomId::new(text(params, "room", 32)?).map_err(|e| EngineError::Other(e.to_string()))?;
    let invitation = text(params, "invitation", 64)?;
    photocraft_party::protocol::validate_secret(invitation).map_err(|_| EngineError::Other("A valid private invitation is required".into()))?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        request(session, photocraft_room_client::ClientCommand::Join { room, invitation: invitation.into() })
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (session, room);
        Err(EngineError::Other("Private rooms require Windows".into()))
    }
}
fn invite(session: &mut Session, params: &Value) -> Result<Value> {
    fields(params, &["expires_in_ms", "uses"])?;
    let expires_in_ms = match params.get("expires_in_ms") {
        None => 3_600_000,
        Some(value) => value
            .as_u64()
            .filter(|n| (1_000..=86_400_000).contains(n))
            .ok_or_else(|| EngineError::Other("Invitation lifetime must be 1 second through 24 hours".into()))?,
    };
    let uses = match params.get("uses") {
        None => 2,
        Some(value) => {
            value.as_u64().filter(|n| (1..=2).contains(n)).ok_or_else(|| EngineError::Other("Invitation must allow one or two friends".into()))? as u8
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        request(session, photocraft_room_client::ClientCommand::Invite { expires_in_ms, uses })
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (session, expires_in_ms, uses);
        Err(EngineError::Other("Private rooms require Windows".into()))
    }
}
fn revoke(session: &mut Session, params: &Value) -> Result<Value> {
    fields(params, &["invitation_id"])?;
    let invitation_id = RequestId::new(text(params, "invitation_id", 32)?).map_err(|e| EngineError::Other(e.to_string()))?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        request(session, photocraft_room_client::ClientCommand::Revoke { invitation_id })
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (session, invitation_id);
        Err(EngineError::Other("Private rooms require Windows".into()))
    }
}
fn invitation(session: &mut Session, params: &Value) -> Result<Value> {
    fields(params, &[])?;
    session.poll_rooms();
    let room = session.rooms.status.room.as_ref().ok_or_else(|| EngineError::Other("No current room".into()))?;
    if session.rooms.status.profile.as_ref().map(|p| p.id) != Some(room.moderator) {
        return Err(EngineError::Other("Only the moderator can share this invitation".into()));
    }
    let invite =
        session.rooms.invitation().filter(|invite| invite.room == room.id).ok_or_else(|| EngineError::Other("Create a private invitation first".into()))?;
    Ok(json!({"room":invite.room,"invitation_id":invite.id,"invitation":invite.secret,"expires_at_ms":invite.expires_at_ms}))
}
fn start(session: &mut Session, params: &Value) -> Result<Value> {
    fields(params, &[])?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        request(session, photocraft_room_client::ClientCommand::Start)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = session;
        Err(EngineError::Other("Private rooms require Windows".into()))
    }
}
fn open_draft(session: &mut Session, params: &Value) -> Result<Value> {
    fields(params, &[])?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        session.open_room_draft()?;
        Ok(json!({"queued":true,"accepted":false}))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = session;
        Err(EngineError::Other("Private rooms require Windows".into()))
    }
}
fn submit_draft(session: &mut Session, params: &Value) -> Result<Value> {
    fields(params, &["note"])?;
    let note = match params.get("note") {
        None => String::new(),
        Some(value) => value
            .as_str()
            .filter(|s| s.len() <= photocraft_party::MAX_NOTE_BYTES)
            .ok_or_else(|| EngineError::Other("Note must be at most 1024 UTF-8 bytes".into()))?
            .to_string(),
    };
    photocraft_party::contracts::validate_label(&note, photocraft_party::MAX_NOTE_BYTES, true).map_err(|e| EngineError::Other(e.to_string()))?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        session.submit_room_draft(note)?;
        Ok(json!({"queued":true,"accepted":false}))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (session, note);
        Err(EngineError::Other("Private rooms require Windows".into()))
    }
}
fn native_simple(session: &mut Session, params: &Value, kind: &str) -> Result<Value> {
    fields(params, &[])?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        let command = match kind {
            "disconnect" => photocraft_room_client::ClientCommand::Disconnect,
            "retry" => photocraft_room_client::ClientCommand::Retry,
            "retain" => photocraft_room_client::ClientCommand::RetainRejected,
            _ => return Err(EngineError::Other("Unknown room action".into())),
        };
        request(session, command)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (session, kind);
        Err(EngineError::Other("Private rooms require Windows".into()))
    }
}

fn spec(id: &'static str, label: &'static str, params: &'static str, run: fn(&mut Session, &Value) -> Result<Value>) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: available, run, journal: false }
}
pub fn specs() -> Vec<CommandSpec> {
    let mut specs = vec![
        spec("komaphone.room.connect", "Connect to private service", r#"{"endpoint":string,"display_name":string}"#, connect),
        spec("komaphone.room.create", "Create private room", r#"{"mode":{"kind":"animation","rotations":2},"timer_ms":null}"#, create),
        spec("komaphone.room.join", "Join private room", r#"{"room":id,"invitation":secret}"#, join),
        spec("komaphone.room.invite", "Create invitation", r#"{"expires_in_ms":3600000,"uses":2}"#, invite),
        spec("komaphone.room.invitation", "Copy private invitation", "{}", invitation),
        spec("komaphone.room.revoke", "Revoke invitation", r#"{"invitation_id":id}"#, revoke),
        spec("komaphone.room.start", "Start game", "{}", start),
        spec("komaphone.room.openDraft", "Open assigned drawing", "{}", open_draft),
        spec("komaphone.room.submit", "Submit saved assignment", r#"{"note":string?}"#, submit_draft),
        spec("komaphone.room.ready", "Ready", r#"{"ready":bool}"#, |s, p| {
            fields(p, &["ready"])?;
            let ready = p.get("ready").and_then(Value::as_bool).ok_or_else(|| EngineError::Other("ready must be true or false".into()))?;
            action(s, Action::Ready { ready })
        }),
        spec("komaphone.room.options", "Room options", r#"{"mode":object,"timer_ms":null|milliseconds}"#, |s, p| {
            fields(p, &["mode", "timer_ms"])?;
            action(s, Action::SetOptions { mode: mode(p)?, timer_ms: timer(p)? })
        }),
        spec("komaphone.room.pause", "Pause game", "{}", |s, p| {
            fields(p, &[])?;
            action(s, Action::Pause)
        }),
        spec("komaphone.room.resume", "Resume game", "{}", |s, p| {
            fields(p, &[])?;
            action(s, Action::Resume)
        }),
        spec("komaphone.room.claim", "Claim moderation", "{}", |s, p| {
            fields(p, &[])?;
            action(s, Action::ClaimModerator)
        }),
        spec("komaphone.room.archive", "Archive game", "{}", |s, p| {
            fields(p, &[])?;
            action(s, Action::Archive)
        }),
        spec("komaphone.room.leave", "Leave room", "{}", |s, p| {
            fields(p, &[])?;
            action(s, Action::Leave)
        }),
        spec("komaphone.room.next", "Next game", "{}", |s, p| {
            fields(p, &[])?;
            action(s, Action::NextGame)
        }),
        spec("komaphone.room.skip", "Skip missing player", r#"{"participant":id,"reason":string}"#, |s, p| {
            fields(p, &["participant", "reason"])?;
            let participant = ParticipantId::new(text(p, "participant", 32)?).map_err(|e| EngineError::Other(e.to_string()))?;
            action(s, Action::Skip { participant, reason: text(p, "reason", 256)?.into() })
        }),
        spec("komaphone.room.end", "End incomplete game", r#"{"reason":string}"#, |s, p| {
            fields(p, &["reason"])?;
            action(s, Action::End { reason: text(p, "reason", 256)?.into() })
        }),
        spec("komaphone.room.disconnect", "Disconnect", "{}", |s, p| native_simple(s, p, "disconnect")),
        spec("komaphone.room.retry", "Retry pending room work", "{}", |s, p| native_simple(s, p, "retry")),
        spec("komaphone.room.retainRejected", "Keep rejected work and refresh", "{}", |s, p| native_simple(s, p, "retain")),
    ];
    specs.push(CommandSpec {
        id: "komaphone.room.status",
        label: "Private room status",
        menu: &[],
        shortcut: None,
        params: "{}",
        enabled: always,
        run: |s, p| {
            fields(p, &[])?;
            s.poll_rooms();
            Ok(json!({"available":s.rooms.available(),"busy":s.rooms.busy(),"status":s.rooms.status}))
        },
        journal: false,
    });
    for command in &mut specs {
        command.enabled = match command.id {
            "komaphone.room.status" => always,
            "komaphone.room.create" | "komaphone.room.join" => idle,
            "komaphone.room.ready" => lobby_member,
            "komaphone.room.options" => lobby_host,
            "komaphone.room.start" => |s| {
                lobby_host(s)?;
                let room = moderator(s)?;
                if room.members.len() < 2 || room.members.iter().any(|m| !m.ready || !m.connected) {
                    return Err("Two or three connected players must be ready".into());
                }
                Ok(())
            },
            "komaphone.room.invite" | "komaphone.room.revoke" => |s| moderator(s).map(|_| ()),
            "komaphone.room.invitation" => |s| {
                available(s)?;
                let room = s.rooms.status.room.as_ref().ok_or_else(|| "No current room".to_string())?;
                if s.rooms.status.profile.as_ref().map(|p| p.id) != Some(room.moderator) || s.rooms.invitation().is_none_or(|invite| invite.room != room.id) {
                    return Err("No current moderator invitation".into());
                }
                Ok(())
            },
            "komaphone.room.openDraft" => can_open,
            "komaphone.room.submit" => can_submit,
            "komaphone.room.pause" => |s| {
                active_host(s)?;
                if moderator(s)?.paused.is_some() {
                    return Err("The game is already paused".into());
                }
                Ok(())
            },
            "komaphone.room.resume" => |s| {
                active_host(s)?;
                let room = moderator(s)?;
                if room.paused.is_none() {
                    return Err("The game is not paused".into());
                }
                // A disconnected player's current turn may already be submitted or
                // explicitly skipped. Only the authority sees those other outcomes.
                Ok(())
            },
            "komaphone.room.claim" => |s| {
                let room = member(s)?;
                if room.members.iter().any(|m| m.participant.id == room.moderator && m.connected) {
                    return Err("The moderator is still connected".into());
                }
                Ok(())
            },
            "komaphone.room.skip" | "komaphone.room.end" => active_host,
            "komaphone.room.archive" => |s| {
                if moderator(s)?.phase != Phase::Reveal {
                    return Err("Available after reveal".into());
                }
                Ok(())
            },
            "komaphone.room.next" => |s| {
                if !matches!(moderator(s)?.phase, Phase::Reveal | Phase::Archived) {
                    return Err("Finish this game first".into());
                }
                Ok(())
            },
            "komaphone.room.leave" => |s| room(s).map(|_| ()),
            _ => available,
        };
    }
    specs
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn room_commands_reject_bad_params_without_journaling_secrets() {
        let mut session = Session::new();
        for command in specs() {
            assert!(!command.journal);
            for params in [json!([]), json!(false), json!({"unexpected":true})] {
                assert!(session.execute(command.id, params).is_err());
            }
        }
        assert!(session.execute("komaphone.room.join", json!({"room":"a".repeat(32),"invitation":"b".repeat(64)})).is_err());
        assert!(session.journal.is_empty());
        assert_eq!(session.execute("komaphone.room.status", json!({})).unwrap()["available"], false);
        assert!(!serde_json::to_string(&session.rooms.status).unwrap().contains(&"b".repeat(64)));
    }
}
