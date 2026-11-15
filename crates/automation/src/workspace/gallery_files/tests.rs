use super::*;
use std::io::Cursor;

fn fixture() -> (tempfile::TempDir, AuthorizedWorkspace) {
    let root = tempfile::tempdir().unwrap();
    for directory in ["read", "write", "outside"] {
        std::fs::create_dir(root.path().join(directory)).unwrap();
    }
    let workspace = AuthorizedWorkspace::new(Some(&root.path().join("read")), Some(&root.path().join("write"))).unwrap();
    (root, workspace)
}

fn export(path: &str, format: &str) -> GalleryAction {
    GalleryAction::Export { format: format.into(), path: path.into(), chain: 0, frame: None, fps: 6, looping: true }
}

fn action_output(prepared: &dyn PreparedGalleryFiles) -> PathBuf {
    match prepared.action() {
        GalleryAction::Export { path, .. } | GalleryAction::Backup { path } => PathBuf::from(path),
        GalleryAction::Restore { destination, .. } => PathBuf::from(destination),
        _ => panic!("expected an output action"),
    }
}

fn action_input(prepared: &dyn PreparedGalleryFiles) -> PathBuf {
    match prepared.action() {
        GalleryAction::Import { path } | GalleryAction::Restore { path, .. } => PathBuf::from(path),
        _ => panic!("expected an input action"),
    }
}

#[test]
fn only_four_file_actions_accept_capabilities() {
    let (_root, workspace) = fixture();
    for action in [GalleryAction::Refresh, GalleryAction::Remix { chain: 0, frame: None }, export("new", "invalid")] {
        assert!(workspace.gallery_files().prepare(&action).is_err());
    }
}

#[test]
fn missing_or_wrong_authority_fails_closed() {
    let (root, _) = fixture();
    let read_only = AuthorizedWorkspace::new(Some(&root.path().join("read")), None).unwrap();
    let write_only = AuthorizedWorkspace::new(None, Some(&root.path().join("write"))).unwrap();
    std::fs::write(root.path().join("read/input.komarchive"), b"archive").unwrap();
    assert!(AuthorizedWorkspace::default().gallery_files().prepare(&export("out.png", "png")).is_err());
    assert!(read_only.gallery_files().prepare(&export("out.png", "png")).is_err());
    assert!(write_only.gallery_files().prepare(&GalleryAction::Import { path: "input.komarchive".into() }).is_err());
    assert!(read_only.gallery_files().prepare(&GalleryAction::Backup { path: "backup".into() }).is_err());
    assert!(write_only.gallery_files().prepare(&GalleryAction::Restore { path: "backup".into(), destination: "restored".into() }).is_err());
    assert_eq!(std::fs::read_dir(root.path().join("write")).unwrap().count(), 0);
}

#[test]
fn traversal_devices_streams_and_outside_absolute_paths_are_rejected() {
    let (root, workspace) = fixture();
    for path in ["../outside/no", "nested/../../no", "./out", "a//b", "a\\b", "out:stream", "NUL", "COM1.png", "out.", "out ", "/absolute", ""] {
        assert!(workspace.gallery_files().prepare(&export(path, "png")).is_err(), "{path}");
        assert!(workspace.gallery_files().prepare(&GalleryAction::Import { path: path.into() }).is_err(), "{path}");
    }
    let outside = root.path().join("outside/no.png");
    assert!(workspace.gallery_files().prepare(&export(&outside.to_string_lossy(), "png")).is_err());
    assert!(!outside.exists());
    let too_deep = format!("{}out.png", "nested/".repeat(MAX_PATH_COMPONENTS));
    let error = workspace.gallery_files().prepare(&export(&too_deep, "png")).err().unwrap().to_string();
    assert!(error.contains("128-component depth limit"), "depth must fail before opening nonexistent parents: {error}");
}

#[test]
fn export_is_private_until_success_and_never_reuses_a_transfer() {
    let (root, workspace) = fixture();
    let mut prepared = workspace.gallery_files().prepare(&export("out.png", "png")).unwrap();
    let output = action_output(prepared.as_ref());
    assert!(!output.starts_with(root.path()));
    assert!(!root.path().join("write/out.png").exists());
    std::fs::write(&output, b"synthetic png").unwrap();
    prepared.finish().unwrap();
    assert_eq!(std::fs::read(root.path().join("write/out.png")).unwrap(), b"synthetic png");
    assert!(prepared.finish().is_err());
    let stage = output.parent().unwrap().to_owned();
    drop(prepared);
    assert!(!stage.exists());
}

#[test]
fn file_collision_before_prepare_or_after_prepare_preserves_original() {
    let (root, workspace) = fixture();
    let destination = root.path().join("write/out.png");
    std::fs::write(&destination, b"original").unwrap();
    assert!(workspace.gallery_files().prepare(&export("out.png", "png")).is_err());
    std::fs::remove_file(&destination).unwrap();
    let mut prepared = workspace.gallery_files().prepare(&export("out.png", "png")).unwrap();
    std::fs::write(action_output(prepared.as_ref()), b"new export").unwrap();
    std::fs::write(&destination, b"concurrent original").unwrap();
    assert!(prepared.finish().is_err());
    assert_eq!(std::fs::read(&destination).unwrap(), b"concurrent original");
    assert_eq!(std::fs::read_dir(root.path().join("write")).unwrap().count(), 1);
}

#[test]
fn missing_output_reports_failure_without_publishing() {
    let (root, workspace) = fixture();
    let mut prepared = workspace.gallery_files().prepare(&export("out.png", "png")).unwrap();
    assert!(prepared.finish().is_err());
    assert!(!root.path().join("write/out.png").exists());
    assert_eq!(std::fs::read_dir(root.path().join("write")).unwrap().count(), 0);
}

#[test]
fn import_uses_a_stable_bounded_snapshot_and_preserves_original() {
    let (root, workspace) = fixture();
    let source = root.path().join("read/input.komarchive");
    std::fs::write(&source, b"original archive").unwrap();
    let mut prepared = workspace.gallery_files().prepare(&GalleryAction::Import { path: "input.komarchive".into() }).unwrap();
    let input = action_input(prepared.as_ref());
    assert_ne!(input, source);
    std::fs::write(&source, b"later bytes").unwrap();
    assert_eq!(std::fs::read(&input).unwrap(), b"original archive");
    prepared.finish().unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), b"later bytes");
    assert_eq!(std::fs::read_dir(root.path().join("write")).unwrap().count(), 0);
}

#[test]
fn nested_existing_parent_and_granted_absolute_path_work() {
    let (root, workspace) = fixture();
    std::fs::create_dir(root.path().join("write/nested")).unwrap();
    let destination = root.path().join("write/nested/output.gif");
    let mut prepared = workspace.gallery_files().prepare(&export(&destination.to_string_lossy(), "gif")).unwrap();
    std::fs::write(action_output(prepared.as_ref()), b"gif").unwrap();
    prepared.finish().unwrap();
    assert_eq!(std::fs::read(destination).unwrap(), b"gif");
    assert!(workspace.gallery_files().prepare(&export("missing/child.png", "png")).is_err());
    assert!(!root.path().join("write/missing").exists());
}

#[test]
fn streaming_rejects_growth_shrink_and_aggregate_overflow() {
    for (bytes, observed, allowance) in [(b"growth".as_slice(), 5, 100), (b"short".as_slice(), 9, 100), (b"large".as_slice(), 5, 4)] {
        let mut remaining = allowance;
        let mut output = Vec::new();
        assert!(copy_checked(&mut Cursor::new(bytes), &mut output, observed, &mut remaining).is_err());
        assert_eq!(remaining, allowance);
    }
    let mut remaining = 7;
    let mut output = Vec::new();
    copy_checked(&mut Cursor::new(b"good"), &mut output, 4, &mut remaining).unwrap();
    assert_eq!(remaining, 3);
    assert_eq!(output, b"good");
}

#[cfg(windows)]
fn directory_link(target: &Path, path: &Path) {
    // cmd's mklink parses forward slashes as switches even within joined paths.
    let target = target.to_string_lossy().replace('/', "\\");
    let path = path.to_string_lossy().replace('/', "\\");
    let output = std::process::Command::new("cmd.exe").args(["/D", "/C", "mklink", "/J"]).arg(path).arg(target).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

#[cfg(windows)]
#[test]
fn request_junctions_cannot_read_write_or_restore_outside_the_grant() {
    let (root, workspace) = fixture();
    let outside = root.path().join("outside");
    std::fs::write(outside.join("secret.komarchive"), b"private").unwrap();
    let read_link = root.path().join("read/escape");
    let write_link = root.path().join("write/escape");
    directory_link(&outside, &read_link);
    directory_link(&outside, &write_link);
    assert!(workspace.gallery_files().prepare(&GalleryAction::Import { path: "escape/secret.komarchive".into() }).is_err());
    assert!(workspace.gallery_files().prepare(&export("escape/output.png", "png")).is_err());
    assert!(workspace.gallery_files().prepare(&GalleryAction::Backup { path: "escape/backup".into() }).is_err());
    assert!(workspace.gallery_files().prepare(&GalleryAction::Restore { path: "escape".into(), destination: "restored".into() }).is_err());
    assert_eq!(std::fs::read(outside.join("secret.komarchive")).unwrap(), b"private");
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    std::fs::remove_dir(read_link).unwrap();
    std::fs::remove_dir(write_link).unwrap();
}

#[cfg(windows)]
#[test]
fn held_output_parent_cannot_be_retargeted_after_preparation() {
    let (root, workspace) = fixture();
    let ancestor = root.path().join("write/nested");
    let parent = ancestor.join("deeper");
    std::fs::create_dir_all(&parent).unwrap();
    let mut prepared = workspace.gallery_files().prepare(&export("nested/deeper/output.png", "png")).unwrap();
    // Every traversed directory remains pinned without FILE_SHARE_DELETE.
    assert!(std::fs::rename(&parent, ancestor.join("moved-parent")).is_err());
    assert!(std::fs::rename(&ancestor, root.path().join("write/moved-ancestor")).is_err());
    std::fs::write(action_output(prepared.as_ref()), b"rendered").unwrap();
    prepared.finish().unwrap();
    assert_eq!(std::fs::read(parent.join("output.png")).unwrap(), b"rendered");
}

#[cfg(windows)]
#[test]
fn retargeted_launch_alias_does_not_grant_a_new_directory() {
    let (root, _) = fixture();
    let alias = root.path().join("launch-alias");
    let original = root.path().join("write");
    let outside = root.path().join("outside");
    directory_link(&original, &alias);
    let workspace = AuthorizedWorkspace::new(Some(&alias), Some(&alias)).unwrap();
    std::fs::remove_dir(&alias).unwrap();
    directory_link(&outside, &alias);
    let mut prepared = workspace.gallery_files().prepare(&export(&alias.join("output.png").to_string_lossy(), "png")).unwrap();
    std::fs::write(action_output(prepared.as_ref()), b"original capability").unwrap();
    prepared.finish().unwrap();
    assert_eq!(std::fs::read(original.join("output.png")).unwrap(), b"original capability");
    assert!(!outside.join("output.png").exists());
    std::fs::remove_dir(alias).unwrap();
}

#[cfg(windows)]
#[test]
fn sequence_backup_and_restored_directories_publish_complete_allowlists() {
    let (root, workspace) = fixture();
    let cases = [
        (export("frames", "sequence"), "frames", vec!["frame-0000.png", "frame-0001.png"]),
        (GalleryAction::Backup { path: "backup".into() }, "backup", vec!["backup.json", "gallery.komabackup", "123456789abcdef0123456789abcdef0.komarchive"]),
    ];
    for (action, destination, names) in cases {
        let mut prepared = workspace.gallery_files().prepare(&action).unwrap();
        let output = action_output(prepared.as_ref());
        std::fs::create_dir(&output).unwrap();
        for name in &names {
            std::fs::write(output.join(name), name.as_bytes()).unwrap();
        }
        assert!(!root.path().join("write").join(destination).exists());
        prepared.finish().unwrap();
        for name in names {
            assert_eq!(std::fs::read(root.path().join("write").join(destination).join(name)).unwrap(), name.as_bytes());
        }
    }
}

#[cfg(windows)]
#[test]
fn restore_stages_only_declared_backup_files_then_publishes_closed_databases() {
    let (root, workspace) = fixture();
    let backup = root.path().join("read/backup");
    std::fs::create_dir(&backup).unwrap();
    for (name, bytes) in [("backup.json", b"index".as_slice()), ("gallery.komabackup", b"gallery".as_slice())] {
        std::fs::write(backup.join(name), bytes).unwrap();
    }
    let mut prepared = workspace.gallery_files().prepare(&GalleryAction::Restore { path: "backup".into(), destination: "restored".into() }).unwrap();
    assert_eq!(std::fs::read(action_input(prepared.as_ref()).join("gallery.komabackup")).unwrap(), b"gallery");
    let output = action_output(prepared.as_ref());
    std::fs::create_dir(&output).unwrap();
    std::fs::write(output.join("drafts.sqlite3"), b"draft db").unwrap();
    std::fs::write(output.join("gallery.sqlite3"), b"gallery db").unwrap();
    prepared.finish().unwrap();
    assert_eq!(std::fs::read(root.path().join("write/restored/drafts.sqlite3")).unwrap(), b"draft db");
    assert_eq!(std::fs::read(backup.join("gallery.komabackup")).unwrap(), b"gallery");
}

#[cfg(windows)]
#[test]
fn invalid_or_partial_directory_never_publishes_and_cleans_owned_staging() {
    let (root, workspace) = fixture();
    for files in [vec!["backup.json"], vec!["backup.json", "gallery.komabackup", "credentials.json"], vec!["backup.json", "gallery.komabackup", "nested/file"]]
    {
        let mut prepared = workspace.gallery_files().prepare(&GalleryAction::Backup { path: "backup".into() }).unwrap();
        let output = action_output(prepared.as_ref());
        std::fs::create_dir(&output).unwrap();
        for name in files {
            let path = output.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"data").unwrap();
        }
        assert!(prepared.finish().is_err());
        assert!(!root.path().join("write/backup").exists());
        assert_eq!(std::fs::read_dir(root.path().join("write")).unwrap().count(), 0);
    }
}

#[cfg(windows)]
#[test]
fn empty_or_nonempty_directory_collision_after_prepare_is_not_replaced() {
    let (root, workspace) = fixture();
    for nonempty in [false, true] {
        let destination_name = if nonempty { "occupied" } else { "empty" };
        let mut prepared = workspace.gallery_files().prepare(&export(destination_name, "sequence")).unwrap();
        let output = action_output(prepared.as_ref());
        std::fs::create_dir(&output).unwrap();
        std::fs::write(output.join("frame-0000.png"), b"frame").unwrap();
        let destination = root.path().join("write").join(destination_name);
        std::fs::create_dir(&destination).unwrap();
        if nonempty {
            std::fs::write(destination.join("original.txt"), b"original").unwrap();
        }
        assert!(prepared.finish().is_err());
        assert!(!destination.join("frame-0000.png").exists());
        assert_eq!(std::fs::read_dir(&destination).unwrap().count(), usize::from(nonempty));
        if nonempty {
            assert_eq!(std::fs::read(destination.join("original.txt")).unwrap(), b"original");
        }
    }
}

#[cfg(windows)]
#[test]
fn atomic_publication_primitives_refuse_a_final_collision_without_preflight() {
    let (root, _) = fixture();
    let dir = Dir::open_ambient_dir(root.path().join("write"), cap_std::ambient_authority()).unwrap();
    dir.create_dir("staged").unwrap();
    dir.create_dir("occupied").unwrap();
    dir.write("staged/complete.png", b"new image").unwrap();
    for invalid in ["../escape", "nested/leaf", "name:stream", "occupied\0suffix"] {
        assert!(rename_directory_noclobber(&dir, Path::new("staged"), Path::new(invalid)).is_err());
    }
    // Exercise the OS primitive directly, bypassing require_absent: an empty
    // destination created after preflight must still be preserved on Windows.
    assert!(rename_directory_noclobber(&dir, Path::new("staged"), Path::new("occupied")).is_err());
    assert_eq!(dir.read("staged/complete.png").unwrap(), b"new image");
    assert_eq!(dir.read_dir("occupied").unwrap().count(), 0);
    rename_directory_noclobber(&dir, Path::new("staged"), Path::new("published")).unwrap();
    assert!(dir.symlink_metadata("staged").is_err());
    assert_eq!(dir.read("published/complete.png").unwrap(), b"new image");
    dir.write("temporary.png", b"new bytes").unwrap();
    dir.write("existing.png", b"original bytes").unwrap();
    assert!(dir.hard_link("temporary.png", &dir, "existing.png").is_err());
    assert_eq!(dir.read("existing.png").unwrap(), b"original bytes");
}

#[cfg(windows)]
#[test]
fn malformed_backup_member_and_oversized_index_are_rejected_before_engine() {
    let (root, workspace) = fixture();
    let backup = root.path().join("read/backup");
    std::fs::create_dir(&backup).unwrap();
    std::fs::write(backup.join("backup.json"), b"{}").unwrap();
    std::fs::write(backup.join("gallery.komabackup"), b"archive").unwrap();
    std::fs::write(backup.join("unrelated.txt"), b"private").unwrap();
    let action = GalleryAction::Restore { path: "backup".into(), destination: "restored".into() };
    assert!(workspace.gallery_files().prepare(&action).is_err());
    std::fs::remove_file(backup.join("unrelated.txt")).unwrap();
    let index = std::fs::OpenOptions::new().write(true).open(backup.join("backup.json")).unwrap();
    index.set_len(MAX_INDEX_BYTES + 1).unwrap();
    drop(index);
    assert!(workspace.gallery_files().prepare(&action).is_err());
    assert!(!root.path().join("write/restored").exists());
}
