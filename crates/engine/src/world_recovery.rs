//! Explicit recovery controls. Quarantined originals are never silently rekeyed.
use crate::{CommandSpec, EngineError, Session};
use serde::Deserialize;
use serde_json::json;
fn available(session: &Session) -> std::result::Result<(), String> {
    if session.worlds.connected() { Ok(()) } else { Err("Connect the original account to inspect its saved edits".into()) }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    after: Option<photocraft_scene::shared::OpId>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resume {
    canvas: photocraft_scene::CanvasId,
}
pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "komaphone.world.recovery",
            label: "Inspect saved recovery edits",
            menu: &[],
            shortcut: None,
            params: r#"{"after":operation_id?}"#,
            enabled: available,
            journal: false,
            run: |session, params| {
                let page: Page = serde_json::from_value(params.clone()).map_err(|_| EngineError::Other("Invalid recovery page".into()))?;
                session.world_recovery_page(page.after)?;
                Ok(json!({"pending":true}))
            },
        },
        CommandSpec {
            id: "komaphone.world.recovery.resume",
            label: "Keep conflicting originals and resume new edits",
            menu: &[],
            shortcut: None,
            params: r#"{"canvas":canvas_id}"#,
            enabled: available,
            journal: false,
            run: |session, params| {
                let resume: Resume = serde_json::from_value(params.clone()).map_err(|_| EngineError::Other("Invalid recovery canvas".into()))?;
                session.world_resume_recovery(resume.canvas)?;
                Ok(json!({"pending":true,"originals_retained":true}))
            },
        },
    ]
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_recovery_controls_fail_without_mutation() {
        for spec in specs() {
            for params in [json!(null), json!([]), json!({"canvas":42}), json!({"after":"bad"})] {
                let mut session = Session::new();
                assert!((spec.run)(&mut session, &params).is_err());
                assert!(session.worlds.recovery.is_empty());
            }
        }
    }
}
