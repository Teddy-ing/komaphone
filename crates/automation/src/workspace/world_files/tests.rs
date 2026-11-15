use super::*;

fn view(path: &str) -> WorldExport {
    WorldExport::View { path: path.into(), width: 64, height: 64, elapsed_ms: 0 }
}
fn fixture() -> (tempfile::TempDir, AuthorizedWorkspace) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("write")).unwrap();
    let workspace = AuthorizedWorkspace::new(None, Some(&root.path().join("write"))).unwrap();
    (root, workspace)
}
#[test]
fn no_ambient_bypass_and_missing_or_wrong_write_authority_fails() {
    assert!(super::super::authorize_engine_command("komaphone.world.export", &serde_json::json!({"format":"png","path":"out.png"})).is_err());
    assert!(AuthorizedWorkspace::default().world_files().prepare(&view("out.png")).is_err());
    let (root, workspace) = fixture();
    for path in ["../out.png", "sub/../../out.png", "NUL", "name:stream", "a\\b", "", "a//b"] {
        assert!(workspace.world_files().prepare(&view(path)).is_err(), "{path}");
    }
    assert!(workspace.world_files().prepare(&view(&root.path().join("outside.png").to_string_lossy())).is_err());
    assert_eq!(std::fs::read_dir(root.path().join("write")).unwrap().count(), 0);
}

#[test]
fn restore_reads_only_granted_flat_bounded_snapshot_and_keeps_input_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("archive");
    std::fs::create_dir(&archive).unwrap();
    std::fs::write(archive.join("world.json"), b"synthetic manifest").unwrap();
    let canvas = photocraft_engine::scene::CanvasId::new(&format!("{:032x}", 500)).unwrap();
    let action = WorldExport::Restore { path: "archive".into(), canvas };
    assert!(super::super::authorize_engine_command("komaphone.world.restore", &serde_json::json!({"path":"archive"})).is_err());
    assert!(AuthorizedWorkspace::default().world_files().prepare(&action).is_err());
    let write_only = AuthorizedWorkspace::new(None, Some(root.path())).unwrap();
    assert!(write_only.world_files().prepare(&action).is_err());
    let reader = AuthorizedWorkspace::new(Some(root.path()), None).unwrap();
    let mut files = reader.world_files().prepare(&action).unwrap();
    let stage = files.action().path().to_owned();
    assert!(!stage.starts_with(root.path()));
    assert_eq!(std::fs::read(stage.join("world.json")).unwrap(), b"synthetic manifest");
    std::fs::write(archive.join("world.json"), b"newer source").unwrap();
    assert_eq!(std::fs::read(stage.join("world.json")).unwrap(), b"synthetic manifest");
    files.finish().unwrap();
    assert!(files.finish().is_err());
    drop(files);
    assert!(!stage.exists());
    assert_eq!(std::fs::read(archive.join("world.json")).unwrap(), b"newer source");
    std::fs::write(archive.join("credentials.txt"), b"unrelated private file").unwrap();
    assert!(reader.world_files().prepare(&action).is_err());
    assert!(reader.world_files().prepare(&action.clone().with_path("../archive".into())).is_err());
}
#[test]
fn private_staging_publishes_once_and_concurrent_collision_preserves_original() {
    let (root, workspace) = fixture();
    let mut prepared = workspace.world_files().prepare(&view("out.png")).unwrap();
    let stage = prepared.action().path().to_owned();
    assert!(!stage.starts_with(root.path()));
    std::fs::write(&stage, b"synthetic bytes").unwrap();
    assert!(!root.path().join("write/out.png").exists());
    prepared.finish().unwrap();
    assert_eq!(std::fs::read(root.path().join("write/out.png")).unwrap(), b"synthetic bytes");
    assert!(prepared.finish().is_err());
    drop(prepared);
    assert!(!stage.exists());
    let mut collision = workspace.world_files().prepare(&view("race.png")).unwrap();
    std::fs::write(collision.action().path(), b"new").unwrap();
    std::fs::write(root.path().join("write/race.png"), b"old").unwrap();
    assert!(collision.finish().is_err());
    assert_eq!(std::fs::read(root.path().join("write/race.png")).unwrap(), b"old");
}
#[cfg(windows)]
#[test]
fn bounded_world_directories_accept_their_manifests_and_reject_foreign_files() {
    let (root, workspace) = fixture();
    let mut archive = workspace.world_files().prepare(&WorldExport::Archive { path: "archive".into() }).unwrap();
    let stage = archive.action().path().to_owned();
    std::fs::create_dir(&stage).unwrap();
    std::fs::write(stage.join("world.json"), b"{}").unwrap();
    std::fs::write(stage.join(format!("{:064x}.png", 1)), b"png").unwrap();
    archive.finish().unwrap();
    assert!(root.path().join("write/archive/world.json").exists());
    let mut invalid = workspace.world_files().prepare(&WorldExport::Archive { path: "invalid".into() }).unwrap();
    let stage = invalid.action().path().to_owned();
    std::fs::create_dir(&stage).unwrap();
    std::fs::write(stage.join("world.json"), b"{}").unwrap();
    std::fs::write(stage.join("credentials.txt"), b"must not publish").unwrap();
    assert!(invalid.finish().is_err());
    assert!(!root.path().join("write/invalid").exists());
    let tour = photocraft_engine::scene::shared::TourId::new(&format!("{:032x}", 1)).unwrap();
    let mut sequence = workspace.world_files().prepare(&WorldExport::TourSequence { path: "sequence".into(), tour, width: 2, height: 2, fps: 6 }).unwrap();
    let stage = sequence.action().path().to_owned();
    std::fs::create_dir(&stage).unwrap();
    std::fs::write(stage.join("tour.json"), b"{}").unwrap();
    std::fs::write(stage.join("frame-0000.png"), b"png").unwrap();
    sequence.finish().unwrap();
    assert!(root.path().join("write/sequence/tour.json").exists());
}
