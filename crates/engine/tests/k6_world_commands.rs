//! Command surface checks independent from the durable service/queue tests.
use photocraft_engine::{
    Session,
    scene::{
        self, CanvasId, OwnerId,
        shared::{DeviceId, WorldState},
        world_protocol::CanvasSummary,
    },
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::Arc;

#[test]
fn restore_parser_requires_only_a_path_and_new_namespace_not_claimed_authority() {
    let canvas = CanvasId::new(&format!("{:032x}", 987)).unwrap();
    let action = photocraft_engine::world_cmds::parse_restore(&json!({"path":"archive", "canvas":canvas})).unwrap();
    assert!(matches!(action, photocraft_engine::world_assets::WorldExport::Restore { canvas: got, .. } if got == canvas));
    for bad in [json!({}), json!({"path":""}), json!({"path":false}), json!({"path":"archive", "owner":canvas}), json!({"path":"archive", "canvas":"zero"})] {
        assert!(photocraft_engine::world_cmds::parse_restore(&bad).is_err());
    }
}

fn fixture() -> Session {
    let mut s = Session::new();
    let canvas = CanvasId::new(&format!("{:032x}", 1)).unwrap();
    let owner = OwnerId::new(&format!("{:032x}", 10)).unwrap();
    let world = Arc::new(WorldState::new(canvas, owner, "Command synthetic".into()).unwrap());
    s.worlds.account = Some(photocraft_engine::worlds::Account {
        endpoint: "https://localhost:43195".into(),
        profile: owner,
        device: DeviceId::new(&format!("{:032x}", 20)).unwrap(),
    });
    s.worlds.active = Some(photocraft_engine::worlds::Canvas {
        info: CanvasSummary { canvas, owner, name: world.name.clone(), import_origin: None, initial_name: world.name.clone(), revision: 0, epoch: 1 },
        canonical: world.clone(),
        display: world,
        pending: 0,
        viewer: photocraft_engine::worlds::Viewer {
            camera: scene::coordinates::home(canvas),
            bookmarks: vec![],
            review: Some(scene::shared::ReviewState::first_visit(canvas, owner, 0).unwrap()),
        },
        notice: None,
    });
    s
}
fn run(s: &mut Session, id: &str, p: Value) -> photocraft_engine::Result<Value> {
    let specs = photocraft_engine::world_cmds::specs();
    let spec = specs.iter().find(|spec| spec.id == id).unwrap();
    (spec.run)(s, &p)
}
#[test]
fn all_world_commands_are_unique_registered_and_keep_sensitive_work_out_of_journals() {
    let specs = photocraft_engine::world_cmds::specs();
    assert!(specs.len() >= 35);
    let mut ids = BTreeSet::new();
    for spec in specs {
        assert!(ids.insert(spec.id));
        assert!(!spec.journal, "{} must not journal private data", spec.id);
        assert!(photocraft_engine::commands::command_specs().iter().any(|registered| registered.id == spec.id), "{} missing registry", spec.id);
        for params in [json!({"unexpected":true}), json!([]), json!({"points":[]}), json!({"name":"\0"})] {
            let mut s = Session::new();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (spec.run)(&mut s, &params)));
            assert!(result.is_ok(), "{} panicked", spec.id);
            assert!(result.unwrap().is_err(), "{} accepted malformed parameters", spec.id);
        }
    }
}
#[test]
fn failed_view_save_and_hostile_scope_leave_canonical_and_personal_state_unchanged() {
    let mut s = fixture();
    let before = serde_json::to_value(&s.worlds.active.as_ref().unwrap().viewer).unwrap();
    let hash = s.worlds.active.as_ref().unwrap().canonical.canonical_hash().unwrap();
    assert!(run(&mut s, "komaphone.world.bookmark", json!({"name":"Must not falsely save"})).is_err());
    assert!(run(&mut s, "komaphone.world.review.begin", json!({})).is_err());
    assert_eq!(serde_json::to_value(&s.worlds.active.as_ref().unwrap().viewer).unwrap(), before);
    let mut foreign = s.worlds.active.as_ref().unwrap().viewer.camera.clone();
    foreign.canvas = CanvasId::new(&format!("{:032x}", 2)).unwrap();
    assert!(run(&mut s, "komaphone.world.camera", json!({"camera":foreign})).is_err());
    assert!(
        run(&mut s, "komaphone.world.stroke", json!({"canvas":foreign.canvas,"anchor":foreign.region,"points":[[0,0]],"width":3,"color":{"rgba":[0,0,0,1]}}))
            .is_err()
    );
    assert_eq!(s.worlds.active.as_ref().unwrap().canonical.canonical_hash().unwrap(), hash);
    assert!(s.documents().is_empty());
}
#[test]
fn export_parser_is_pure_and_rejects_unbounded_or_mismatched_options() {
    use photocraft_engine::world_cmds::parse_export;
    assert!(parse_export(&json!({"format":"png","path":"synthetic.png","width":64,"height":64})).is_ok());
    for p in [
        json!({"format":"png","path":""}),
        json!({"format":"archive","path":"x","width":64}),
        json!({"format":"gif","path":"x"}),
        json!({"format":"png","path":"x","width":u64::MAX}),
        json!({"format":"png","path":"x","fps":0}),
        json!({"format":"png","path":"x\0y"}),
    ] {
        assert!(parse_export(&p).is_err(), "{p}");
    }
}
