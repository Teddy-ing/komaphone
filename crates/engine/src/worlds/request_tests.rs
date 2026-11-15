use super::*;
use photocraft_room_client::world_worker::WorldWorker;

#[test]
fn newer_world_presence_is_forwarded_while_older_publication_is_pending() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::new();
    session.worlds.worker = Some(WorldWorker::start(root.path().join("world.db"), None).unwrap());
    let canvas = CanvasId::new("00000000000000000000000000000001").unwrap();
    let presence = |x| wire::Action::Presence {
        canvas,
        epoch: 1,
        camera: photocraft_scene::coordinates::home(canvas),
        preview: vec![[x, 0.0]],
        cursor: None,
        stroke: None,
    };
    let old = session.world_request(presence(0.0)).unwrap();
    let latest = session.world_request(presence(20.0)).unwrap();
    assert_ne!(old, latest, "A newer gesture must reach the worker before the old response arrives");
    assert!(matches!(session.worlds.requests.get(&latest), Some(wire::Action::Presence { preview, .. }) if preview == &vec![[20.0, 0.0]]));
    assert_eq!(session.worlds.presence_requests_pending(), 2);
    assert_eq!(session.worlds.requests_pending(), 0);
    // Read-only polling still coalesces, without swallowing later publications.
    let peers = session.world_request(wire::Action::Peers { canvas }).unwrap();
    assert_eq!(session.world_request(wire::Action::Peers { canvas }).unwrap(), peers);
    assert_eq!(session.worlds.requests.len(), 3);
}
