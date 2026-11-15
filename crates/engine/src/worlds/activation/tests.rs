#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use photocraft_party::protocol::RpcResponse;
use photocraft_room_client::{CredentialStore, Outbox, world_worker::WorldWorker};
use photocraft_scene::world_protocol::CanvasSummary;
use std::{collections::BTreeMap, path::Path, sync::Mutex};

#[derive(Default)]
struct Credentials(Mutex<BTreeMap<(String, String), String>>);
impl CredentialStore for Credentials {
    fn get(&self, namespace: &str, key: &str) -> photocraft_room_client::Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(&(namespace.into(), key.into())).cloned())
    }
    fn set(&self, namespace: &str, key: &str, value: &str) -> photocraft_room_client::Result<()> {
        self.0.lock().unwrap().insert((namespace.into(), key.into()), value.into());
        Ok(())
    }
    fn remove(&self, namespace: &str, key: &str) -> photocraft_room_client::Result<()> {
        self.0.lock().unwrap().remove(&(namespace.into(), key.into()));
        Ok(())
    }
}
fn access(root: &Path, name: &str) -> WorldAccess {
    let mut room = Outbox::open(root.join(format!("{name}.db")), Box::new(Credentials::default())).unwrap();
    room.configure("http://127.0.0.1:1", name).unwrap();
    let pair = room.next().unwrap().unwrap();
    let profile = room.status().profile.unwrap();
    room.acknowledge(&pair, &RpcResponse::Paired { participant: profile }).unwrap();
    WorldAccess::from_room(&room).unwrap().unwrap()
}
fn info(access: &WorldAccess, number: u64) -> CanvasSummary {
    CanvasSummary {
        canvas: photocraft_scene::CanvasId::new(&format!("{number:032x}")).unwrap(),
        owner: access.scope().profile,
        name: format!("Private {number}"),
        import_origin: None,
        initial_name: format!("Private {number}"),
        revision: 0,
        epoch: 1,
    }
}
fn ack(sequence: u64, access: Option<&WorldAccess>) -> Event {
    Event::Activated { sequence, scope: access.map(|access| access.scope().clone()) }
}

#[test]
fn buffered_prior_account_events_never_repopulate_catalog_or_thumbnails_before_latest_activation() {
    let root = tempfile::tempdir().unwrap();
    let first = access(root.path(), "First");
    let second = access(root.path(), "Second");
    let private = info(&first, 1);
    let desired = info(&second, 2);
    let mut session = Session::new();
    session.begin_world_activation(10, Some(first.copy_for_worker()));
    // These events have already left the coordinator; clearing its outbound
    // queue on the next Access cannot retract this buffered channel prefix.
    let buffered = vec![ack(10, Some(&first)), Event::CachedCanvases { items: vec![private.clone()] }];
    session.begin_world_activation(11, Some(second.copy_for_worker()));
    for event in buffered {
        session.world_event(event);
    }
    assert!(session.worlds.account.is_none() && session.worlds.items.is_empty());
    assert!(!session.worlds.thumbnails.contains(private.canvas));
    session.world_event(ack(11, Some(&second)));
    session.world_event(Event::CachedCanvases { items: vec![desired.clone()] });
    assert_eq!(session.worlds.account.as_ref().unwrap().profile, second.scope().profile);
    assert_eq!(session.worlds.items.as_slice(), std::slice::from_ref(&desired));
    assert!(!session.worlds.thumbnails.contains(private.canvas));
    assert!(session.worlds.thumbnails.contains(desired.canvas));

    // Matching scope alone is insufficient for a rapid A→B→A switch.
    session.begin_world_activation(12, Some(first.copy_for_worker()));
    session.world_event(ack(10, Some(&first)));
    session.world_event(Event::CachedCanvases { items: vec![private.clone()] });
    session.world_event(ack(11, Some(&second)));
    session.world_event(Event::CachedCanvases { items: vec![desired] });
    assert!(session.worlds.account.is_none() && session.worlds.items.is_empty());
    session.world_event(ack(12, Some(&first)));
    session.world_event(Event::CachedCanvases { items: vec![private.clone()] });
    assert_eq!(session.worlds.items, [private]);
    session.begin_world_activation(13, None);
    session.world_event(ack(12, Some(&first)));
    assert!(session.worlds.account.is_none());
    session.world_event(ack(13, None));
    assert!(session.worlds.account.is_none() && session.worlds.items.is_empty());
}

#[test]
fn redundant_handoffs_and_duplicate_activation_acks_preserve_pending_durability_receipts() {
    let root = tempfile::tempdir().unwrap();
    let first = access(root.path(), "First");
    let mut session = Session::new();
    session.worlds.worker = Some(WorldWorker::start(root.path().join("world.db"), None).unwrap());
    session.begin_world_activation(10, Some(first.copy_for_worker()));
    session.world_event(ack(10, Some(&first)));
    session.worlds.pending.insert(80);
    let activation = session.worlds.activation;
    session.world_access(Some(first.copy_for_worker())).unwrap();
    session.world_event(ack(10, Some(&first)));
    assert_eq!(session.worlds.activation, activation);
    assert!(session.worlds.pending.contains(&80));
    session.world_event(Event::ViewSaved { sequence: 80, canvas: info(&first, 1).canvas });
    assert!(!session.worlds.pending.contains(&80));
    assert_eq!(session.worlds.account.as_ref().unwrap().profile, first.scope().profile);
}

#[test]
fn activation_failure_retains_privacy_gate_and_allows_exact_session_retry() {
    let root = tempfile::tempdir().unwrap();
    let first = access(root.path(), "First");
    let mut session = Session::new();
    session.begin_world_activation(10, Some(first.copy_for_worker()));
    session.world_event(Event::Error { sequence: 10, message: "Synthetic local storage failure".into() });
    assert!(session.worlds.access_request.as_ref().unwrap().failed);
    session.world_event(Event::CachedCanvases { items: vec![info(&first, 1)] });
    assert!(session.worlds.items.is_empty() && session.worlds.account.is_none());
    session.begin_world_activation(11, Some(first.copy_for_worker()));
    session.world_event(ack(10, Some(&first)));
    assert!(session.worlds.account.is_none());
    session.world_event(ack(11, Some(&first)));
    assert_eq!(session.worlds.account.as_ref().unwrap().profile, first.scope().profile);
}

#[test]
fn stopped_worker_surfaces_during_handoff_without_accepting_unscoped_old_errors() {
    let root = tempfile::tempdir().unwrap();
    let first = access(root.path(), "First");
    let mut session = Session::new();
    session.begin_world_activation(10, Some(first.copy_for_worker()));
    session.world_event(Event::Error { sequence: 0, message: "An old account request failed".into() });
    assert!(!session.worlds.access_request.as_ref().unwrap().failed);
    assert!(session.worlds.error.is_none());
    session.world_event(Event::Stopped { message: "Cannot open the local database".into() });
    assert!(session.worlds.access_request.as_ref().unwrap().failed);
    assert!(session.worlds.error.as_ref().unwrap().contains("Cannot open the local database"));
    assert!(session.worlds.error.as_ref().unwrap().contains("restart Komaphone"));
    session.world_event(Event::CachedCanvases { items: vec![info(&first, 1)] });
    assert!(session.worlds.items.is_empty() && session.worlds.account.is_none());
}

#[test]
fn account_pause_invalidates_relocation_ack_and_buffered_world_data() {
    let root = tempfile::tempdir().unwrap();
    let first = access(root.path(), "First");
    let mut session = Session::new();
    let id = photocraft_party::RequestId::new("00000000000000000000000000000001").unwrap();
    session.begin_world_activation(10, Some(first.copy_for_worker()));
    session.world_event(ack(10, Some(&first)));
    session.worlds.relocation_pending = Some((12, id));
    session.suspend_world_access();
    session.world_event(Event::Relocated { sequence: 12, id });
    session.world_event(ack(10, Some(&first)));
    session.world_event(Event::CachedCanvases { items: vec![info(&first, 1)] });
    assert!(session.worlds.relocation_pending.is_none());
    assert!(session.worlds.account.is_none());
    assert!(session.worlds.items.is_empty());
    assert!(session.worlds.access_request.as_ref().unwrap().failed);
    assert!(session.rooms.status.error.is_none(), "stale relocation must not dispatch a room completion");
}

#[test]
fn prior_account_durable_receipt_does_not_clear_current_error() {
    let root = tempfile::tempdir().unwrap();
    let first = access(root.path(), "First");
    let mut session = Session::new();
    session.begin_world_activation(10, None);
    session.world_event(ack(10, None));
    session.worlds.error = Some("Current account needs attention".into());
    let operation = photocraft_scene::shared::WorldOp {
        id: photocraft_scene::shared::OpId::new("00000000000000000000000000000001").unwrap(),
        canvas: info(&first, 1).canvas,
        author: first.scope().profile,
        device: first.scope().device,
        sequence: 1,
        authority_epoch: 1,
        action: photocraft_scene::shared::Edit::Rename { name: "Private old name".into(), expected_revision: 0 },
    };
    session.world_event(Event::Saved { sequence: 8, operation });
    assert_eq!(session.worlds.error.as_deref(), Some("Current account needs attention"));
    assert!(session.worlds.items.is_empty());
}
