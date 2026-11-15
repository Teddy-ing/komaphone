//! Hosted account actions. Capabilities never enter journals or drawing replay.
use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};
use serde_json::Value;
#[cfg(any(not(target_arch = "wasm32"), test))]
use serde_json::json;

fn fields(value: &Value, allowed: &[&str]) -> Result<()> {
    let object = value.as_object().ok_or_else(|| error("Expected account parameters"))?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(error("Unknown account parameter"));
    }
    Ok(())
}
fn error(message: &str) -> EngineError {
    EngineError::Other(message.into())
}
fn available(session: &Session) -> std::result::Result<(), String> {
    if session.rooms.available() { Ok(()) } else { Err("Sign-in requires native room storage".into()) }
}
fn idle(session: &Session) -> std::result::Result<(), String> {
    available(session)?;
    if session.rooms.status.signing_in { Err("Finish or cancel the current sign-in".into()) } else { Ok(()) }
}
fn sign_in(session: &mut Session, params: &Value) -> Result<Value> {
    fields(params, &["endpoint", "display_name", "admission"])?;
    let endpoint = match params.get("endpoint") {
        None => "https://komaphone.tedsoftware.org",
        Some(Value::String(endpoint)) if !endpoint.is_empty() && endpoint.len() <= 2048 => endpoint,
        _ => return Err(error("Use a valid HTTPS service address")),
    };
    let label = params.get("display_name").and_then(Value::as_str).ok_or_else(|| error("Enter your display name"))?;
    photocraft_party::validate_label(label, 96, false).map_err(|_| error("Enter a display name of at most 96 bytes"))?;
    let admission = match params.get("admission") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) if text.is_empty() => None,
        Some(Value::String(text)) => {
            photocraft_party::protocol::validate_secret(text).map_err(|_| error("The service invitation is invalid"))?;
            Some(text.clone())
        }
        _ => return Err(error("The service invitation is invalid")),
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let endpoint = photocraft_room_client::normalize_endpoint(endpoint).map_err(|_| error("Use a valid HTTPS service address"))?;
        session.room_request(photocraft_room_client::ClientCommand::SignIn { endpoint, display_name: label.into(), admission })?;
        Ok(json!({"queued":true}))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (session, endpoint, label, admission);
        Err(error("Google sign-in requires the native Windows app"))
    }
}
fn select(session: &mut Session, params: &Value) -> Result<Value> {
    fields(params, &["slot"])?;
    let slot = match params.get("slot") {
        None | Some(Value::Null) => None,
        Some(Value::String(slot)) if slot.len() == 32 && slot.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) => Some(slot.clone()),
        _ => return Err(error("Choose a saved account or a new account")),
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        session.room_request(photocraft_room_client::ClientCommand::SelectAccount { slot })?;
        Ok(json!({"queued":true}))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (session, slot);
        Err(error("Account switching requires the native Windows app"))
    }
}
fn relocate(session: &mut Session, params: &Value) -> Result<Value> {
    fields(params, &["endpoint", "display_name"])?;
    let endpoint = match params.get("endpoint") {
        None => "https://komaphone.tedsoftware.org",
        Some(Value::String(endpoint)) if !endpoint.is_empty() && endpoint.len() <= 2048 => endpoint,
        _ => return Err(error("Use a valid HTTPS service address")),
    };
    let label = params.get("display_name").and_then(Value::as_str).ok_or_else(|| error("Enter your display name"))?;
    photocraft_party::validate_label(label, 96, false).map_err(|_| error("Enter a display name of at most 96 bytes"))?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        let endpoint = photocraft_room_client::normalize_endpoint(endpoint).map_err(|_| error("Use a valid HTTPS service address"))?;
        session.room_request(photocraft_room_client::ClientCommand::Relocate { endpoint, display_name: label.into() })?;
        Ok(json!({"queued":true}))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (session, endpoint, label);
        Err(error("Account migration requires the native Windows app"))
    }
}
fn simple(session: &mut Session, params: &Value, cancel: bool) -> Result<Value> {
    fields(params, &[])?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        let command = if cancel { photocraft_room_client::ClientCommand::CancelSignIn } else { photocraft_room_client::ClientCommand::SignOut };
        session.room_request(command)?;
        Ok(json!({"queued":true}))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (session, cancel);
        Err(error("Account actions require the native Windows app"))
    }
}
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "komaphone.account.relocate",
            label: "Move saved account to hosted service",
            menu: &[],
            shortcut: None,
            params: r#"{"endpoint":string?,"display_name":string}"#,
            enabled: idle,
            run: relocate,
            journal: false,
        },
        CommandSpec {
            id: "komaphone.account.signIn",
            label: "Sign in with Google",
            menu: &[],
            shortcut: None,
            params: r#"{"endpoint":string?,"display_name":string,"admission":secret?}"#,
            enabled: idle,
            run: sign_in,
            journal: false,
        },
        CommandSpec {
            id: "komaphone.account.cancel",
            label: "Cancel sign-in",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: available,
            run: |s, p| simple(s, p, true),
            journal: false,
        },
        CommandSpec {
            id: "komaphone.account.signOut",
            label: "Sign out",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: available,
            run: |s, p| simple(s, p, false),
            journal: false,
        },
        CommandSpec {
            id: "komaphone.account.select",
            label: "Choose account",
            menu: &[],
            shortcut: None,
            params: r#"{"slot":id|null}"#,
            enabled: idle,
            run: select,
            journal: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_account_parameters_fail_without_recording_secrets() {
        let mut session = Session::new();
        for command in specs() {
            assert!(!command.journal);
            for bad in [json!(false), json!([]), json!({"unexpected":"secret"})] {
                assert!((command.run)(&mut session, &bad).is_err());
            }
        }
        assert!(sign_in(&mut session, &json!({"display_name":"Test", "admission":"private-value"})).is_err());
        assert!(select(&mut session, &json!({"slot":"../other-account"})).is_err());
        assert!(session.journal.is_empty());
    }
}
