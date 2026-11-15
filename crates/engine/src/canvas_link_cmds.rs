//! Explicit native profile binding commands; no supplied identity is trusted.
use crate::{
    EngineError, Result,
    commands::{CommandSpec, always},
};
use serde_json::{Value, json};
fn empty(value: &Value) -> Result<()> {
    if value.as_object().is_none_or(|object| !object.is_empty()) {
        Err(EngineError::Other("This command accepts an empty object; account identity comes from the protected service connection".into()))
    } else {
        Ok(())
    }
}
pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "komaphone.canvas.link.status",
            label: "Local canvas account binding",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: always,
            journal: false,
            run: |session, params| {
                empty(params)?;
                session.poll_canvases();
                serde_json::to_value(&session.canvases.link).map_err(|e| EngineError::Other(e.to_string()))
            },
        },
        CommandSpec {
            id: "komaphone.canvas.link",
            label: "Link local canvases to current profile",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: always,
            journal: false,
            run: |session, params| {
                empty(params)?;
                session.canvas_link_identity(false)?;
                Ok(json!({"pending":true,"linked":false}))
            },
        },
        CommandSpec {
            id: "komaphone.canvas.link.rollback",
            label: "Roll back local profile binding",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: always,
            journal: false,
            run: |session, params| {
                empty(params)?;
                session.canvas_link_identity(true)?;
                Ok(json!({"pending":true}))
            },
        },
    ]
}
