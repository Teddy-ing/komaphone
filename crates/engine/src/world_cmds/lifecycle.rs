use super::{CanvasId, CommandSpec, Deserialize, active, available, empty, error, opened, owner, parse, pending, same, spec, wire};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CanvasEpoch {
    canvas: CanvasId,
    expected_epoch: u64,
}

pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        spec("komaphone.world.trash", "Refresh deleted canvases", "{}", available, |s, p| {
            empty(p)?;
            pending(s, wire::Action::Trash { after: None, limit: wire::MAX_PAGE })
        }),
        spec("komaphone.world.delete", "Move my canvas to Trash", r#"{"canvas":id,"expected_epoch":integer}"#, opened, |s, p| {
            let p: CanvasEpoch = parse(p)?;
            same(s, p.canvas)?;
            owner(s)?;
            if p.expected_epoch != active(s)?.info.epoch || p.expected_epoch == 0 {
                return Err(error("Canvas access changed; refresh before moving it to Trash"));
            }
            if s.worlds.busy() || active(s)?.pending > 0 {
                return Err(error("Wait for saved edits to sync before moving this canvas to Trash"));
            }
            pending(s, wire::Action::Delete { canvas: p.canvas, expected_epoch: p.expected_epoch })
        }),
        spec("komaphone.world.undelete", "Restore my deleted canvas and its members", r#"{"canvas":id,"expected_epoch":integer}"#, available, |s, p| {
            let p: CanvasEpoch = parse(p)?;
            if p.expected_epoch == 0 || !s.worlds.trash.iter().any(|item| item.canvas == p.canvas && item.owner_epoch == p.expected_epoch) {
                return Err(error("Refresh Trash and select a deleted canvas owned by this account"));
            }
            pending(s, wire::Action::RestoreOriginal { canvas: p.canvas, expected_epoch: p.expected_epoch })
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;
    use serde_json::json;

    #[test]
    fn lifecycle_requests_cannot_claim_an_unlisted_trash_entry_or_unopened_canvas() {
        for id in ["komaphone.world.delete", "komaphone.world.undelete"] {
            let mut session = Session::new();
            let commands = specs();
            let command = commands.iter().find(|command| command.id == id).unwrap();
            for params in [json!({}), json!({"canvas":"11111111111111111111111111111111","expected_epoch":1}), json!({"expected_epoch":0})] {
                assert!((command.run)(&mut session, &params).is_err());
            }
        }
    }
}
