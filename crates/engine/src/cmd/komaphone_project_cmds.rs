//! Stable local-project command seam, shared by the workspace and automation.
use crate::commands::{CommandSpec, always};
use crate::{EngineError, Result, Session};
use photocraft_party::contracts::ProjectId;
use serde_json::{Value, json};

fn fields(params: &Value, allowed: &[&str]) -> Result<()> {
    if params.is_null() {
        return Ok(());
    }
    let object = params.as_object().ok_or_else(|| EngineError::Other("Expected an object".into()))?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(EngineError::Other("Unknown local-project parameter".into()));
    }
    Ok(())
}

fn available(session: &Session) -> std::result::Result<(), String> {
    if session.local_projects.available() { Ok(()) } else { Err("Permanent local drafts require the desktop app's local storage".into()) }
}

fn new_project(session: &mut Session, params: &Value) -> Result<Value> {
    fields(params, &["width", "height", "name"])?;
    if session.local_projects.failure_budget_exhausted() {
        return Err(EngineError::Other(
            "Retained failed drafts reached the memory limit; resolve their save errors before creating more local drawings".into(),
        ));
    }
    session.check_new_local_replay_capacity()?;
    let dimension = |name: &str, default: u64| -> Result<u64> {
        match params.get(name) {
            None => Ok(default),
            Some(value) => value.as_u64().filter(|v| (1..=8192).contains(v)).ok_or_else(|| EngineError::Other(format!("{name} must be 1..8192"))),
        }
    };
    let width = dimension("width", 1536)?;
    let height = dimension("height", 1024)?;
    if width.saturating_mul(height) > 16_777_216 {
        return Err(EngineError::Other("Local drawing exceeds the 16 megapixel project limit".into()));
    }
    let name = match params.get("name") {
        None => "Untitled drawing",
        Some(value) => {
            value.as_str().filter(|s| !s.trim().is_empty() && s.len() <= 96).ok_or_else(|| EngineError::Other("Name must be 1..96 UTF-8 bytes".into()))?
        }
    };
    session.execute("file.new", json!({"width":width,"height":height,"name":name,"background":"white"}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "komaphone.project.new",
            label: "New drawing",
            menu: &[],
            shortcut: None,
            params: r#"{"width":px=1536,"height":px=1024,"name":string?}"#,
            enabled: available,
            run: new_project,
            journal: false,
        },
        CommandSpec {
            id: "komaphone.project.list",
            label: "Local drafts",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: available,
            run: |s, p| {
                fields(p, &[])?;
                s.list_local_projects()?;
                Ok(json!({"pending":true}))
            },
            journal: false,
        },
        CommandSpec {
            id: "komaphone.project.open",
            label: "Reopen local draft",
            menu: &[],
            shortcut: None,
            params: r#"{"project":string}"#,
            enabled: available,
            run: |s, p| {
                fields(p, &["project"])?;
                let id = p.get("project").and_then(Value::as_str).ok_or_else(|| EngineError::Other("A project ID is required".into()))?;
                let project = ProjectId::new(id).map_err(|e| EngineError::Other(e.to_string()))?;
                s.open_local_project(project)?;
                Ok(json!({"pending":true}))
            },
            journal: false,
        },
        CommandSpec {
            id: "komaphone.project.retry",
            label: "Retry local save",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: available,
            run: |s, p| {
                fields(p, &[])?;
                s.retry_local_projects()?;
                Ok(json!({"pending":true}))
            },
            journal: false,
        },
        CommandSpec {
            id: "komaphone.project.status",
            label: "Local save status",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: always,
            run: |s, p| {
                fields(p, &[])?;
                s.poll_local_projects();
                Ok(json!({"available":s.local_projects.available(),"loading":s.local_projects.loading(),
                "drafts":s.local_projects.drafts,"error":s.local_projects.error,"notice":s.local_projects.notice,
                "usage":s.local_projects.usage.map(|usage|json!({"storedBytes":usage.stored_bytes,"quotaBytes":usage.quota_bytes})),
                "externalReason":s.active().and_then(|d|s.local_replay.excluded_reason(d.doc.id)),
                "active":s.active().map(|d|s.local_projects.status(d.doc.id))}))
            },
            journal: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_commands_reject_bad_params_and_unconfigured_storage() {
        let mut session = Session::new();
        for command in specs() {
            for params in [json!([]), json!(false), json!({"unexpected":true})] {
                assert!(session.execute(command.id, params).is_err());
            }
        }
        assert!(session.execute("komaphone.project.new", json!({})).is_err());
        assert_eq!(session.execute("komaphone.project.status", json!({})).unwrap()["available"], false);
    }
}
