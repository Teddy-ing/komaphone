use super::*;

struct Fixture {
    root: PathBuf,
    manifest_sha: String,
    app_sha: String,
}
fn temp_root() -> PathBuf {
    let sequence = COPY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("komaphone-k65-launcher-{}-{sequence}", std::process::id()));
    fs::create_dir(&path).unwrap();
    path
}
impl Fixture {
    fn new(marker: &[u8]) -> Self {
        let root = temp_root();
        let mut rows = Vec::new();
        for path in FILES.iter().filter(|path| **path != "SHA256SUMS") {
            let full = root.join("candidate").join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(&full, if *path == "photocraft.exe" { marker } else { path.as_bytes() }).unwrap();
            rows.push(format!("{}  {path}", digest(&full, MAX_FILE_BYTES).unwrap()));
        }
        fs::write(root.join("candidate/SHA256SUMS"), rows.join("\n") + "\n").unwrap();
        Self {
            manifest_sha: digest(&root.join("candidate/SHA256SUMS"), 16384).unwrap(),
            app_sha: digest(&root.join("candidate/photocraft.exe"), MAX_FILE_BYTES).unwrap(),
            root,
        }
    }
    fn package(&self) -> Package<'_> {
        Package { manifest_sha: &self.manifest_sha, app_sha: &self.app_sha, profile: "k65-hosted-test" }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn manifest_rejects_paths_duplicate_credentials_and_unexpected_files() {
    let fixture = Fixture::new(b"v1");
    let text = fs::read_to_string(fixture.root.join("candidate/SHA256SUMS")).unwrap();
    assert_eq!(parse_manifest(&text, fixture.package()).unwrap().len(), FILES.len());
    assert!(parse_manifest(&text.replace("NOTICE", "../NOTICE"), fixture.package()).is_err());
    assert!(parse_manifest(&text.replace("NOTICE", "credentials.json"), fixture.package()).is_err());
    assert!(parse_manifest(&format!("{text}{}  NOTICE\n", "A".repeat(64)), fixture.package()).is_err());
    fs::write(fixture.root.join("candidate/service.pem"), b"unexpected certificate").unwrap();
    assert!(verify_candidate(&fixture.root.join("candidate"), fixture.package()).is_err());
}

#[test]
fn update_changes_only_versioned_program_directory_and_preserves_data() {
    let first = Fixture::new(b"v1");
    let second = Fixture::new(b"v2");
    let destination = temp_root();
    prepare(&first.root, &destination, first.package()).unwrap();
    fs::write(destination.join("data/original-drawing"), b"accepted artwork").unwrap();
    fs::write(destination.join("config/preferences"), b"same profile").unwrap();
    prepare(&second.root, &destination, second.package()).unwrap();
    prepare(&first.root, &destination, first.package()).unwrap();
    assert_eq!(fs::read(destination.join("data/original-drawing")).unwrap(), b"accepted artwork");
    assert_eq!(fs::read(destination.join("config/preferences")).unwrap(), b"same profile");
    assert_ne!(program_directory(&destination, first.package()), program_directory(&destination, second.package()));
    verify_candidate(&program_directory(&destination, first.package()), first.package()).unwrap();
    verify_candidate(&program_directory(&destination, second.package()), second.package()).unwrap();
    fs::remove_dir_all(destination).unwrap();
}

#[test]
fn process_environment_removes_legacy_trust_and_keeps_local_storage() {
    let fixture = Fixture::new(b"v1");
    let destination = temp_root();
    let command = app_command(&fixture.root.join("candidate/photocraft.exe"), &destination, true).unwrap();
    let environment = command.get_envs().collect::<BTreeMap<_, _>>();
    for name in ["KOMAPHONE_TRUSTED_CERT", "KOMAPHONE_SERVICE_ENDPOINT", "PHOTOCRAFT_CONTROL_TOKEN"] {
        assert_eq!(environment.get(std::ffi::OsStr::new(name)), Some(&None));
    }
    assert_eq!(command.get_current_dir(), Some(destination.join("exports").as_path()));
    assert_eq!(environment.get(std::ffi::OsStr::new("KOMAPHONE_EXPORTS_DIR")), Some(&Some(destination.join("exports").as_os_str())));
    let config_path = fixture.root.join("candidate").join("google-desktop-client.json");
    assert_eq!(environment.get(std::ffi::OsStr::new("KOMAPHONE_GOOGLE_CLIENT_CONFIG")), Some(&Some(config_path.as_os_str())));
    assert_eq!(command.get_args().collect::<Vec<_>>(), [std::ffi::OsStr::new("--safe-gpu")]);
    fs::remove_dir_all(destination).unwrap();
}

#[test]
fn interrupted_update_preserves_previous_program_and_retries_without_data_loss() {
    let first = Fixture::new(b"old supported version");
    let second = Fixture::new(b"new supported version");
    let destination = temp_root();
    prepare(&first.root, &destination, first.package()).unwrap();
    let saved = destination.join("data/old-format-draft");
    fs::write(&saved, b"original schema bytes: unsynced drawing").unwrap();
    // A partially published new program contains a conflicting file. Preparing
    // it must fail without replacing either that file or the working version.
    let partial = program_directory(&destination, second.package());
    fs::create_dir_all(&partial).unwrap();
    fs::write(partial.join("NOTICE"), b"interrupted/conflicting file").unwrap();
    assert!(prepare(&second.root, &destination, second.package()).is_err());
    assert_eq!(fs::read(&saved).unwrap(), b"original schema bytes: unsynced drawing");
    verify_candidate(&program_directory(&destination, first.package()), first.package()).unwrap();
    assert_eq!(fs::read(partial.join("NOTICE")).unwrap(), b"interrupted/conflicting file");
    // Only the test-created conflicting file is removed; already verified
    // published files can be reused on retry.
    fs::remove_file(partial.join("NOTICE")).unwrap();
    prepare(&second.root, &destination, second.package()).unwrap();
    prepare(&first.root, &destination, first.package()).unwrap();
    assert_eq!(fs::read(&saved).unwrap(), b"original schema bytes: unsynced drawing");
    verify_candidate(&partial, second.package()).unwrap();
    fs::remove_dir_all(destination).unwrap();
}

#[test]
fn google_configuration_is_required_and_hash_verified_before_launch() {
    let fixture = Fixture::new(b"v1");
    let destination = temp_root();
    prepare(&fixture.root, &destination, fixture.package()).unwrap();
    let installed = program_directory(&destination, fixture.package());
    let config = installed.join("google-desktop-client.json");
    fs::write(&config, b"tampered public client config").unwrap();
    assert!(verify_candidate(&installed, fixture.package()).is_err());
    assert!(prepare(&fixture.root, &destination, fixture.package()).is_err());
    fs::remove_file(fixture.root.join("candidate/google-desktop-client.json")).unwrap();
    assert!(verify_candidate(&fixture.root.join("candidate"), fixture.package()).is_err());
    fs::remove_dir_all(destination).unwrap();
}

#[test]
fn relative_export_survives_restart_outside_verified_programs() {
    const CHILD: &str = "KOMAPHONE_K65_TEST_EXPORT";
    if std::env::var_os(CHILD).is_some() {
        fs::write("export.png", b"relative export fixture").unwrap();
        return;
    }
    let fixture = Fixture::new(b"v1");
    let destination = temp_root();
    prepare(&fixture.root, &destination, fixture.package()).unwrap();
    let output = app_command(&std::env::current_exe().unwrap(), &destination, false)
        .unwrap()
        .args(["--exact", "tests::relative_export_survives_restart_outside_verified_programs"])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    prepare(&fixture.root, &destination, fixture.package()).unwrap();
    assert_eq!(fs::read(destination.join("exports/export.png")).unwrap(), b"relative export fixture");
    assert!(!program_directory(&destination, fixture.package()).join("export.png").exists());
    fs::remove_dir_all(destination).unwrap();
}

#[test]
fn conflicting_destination_file_remains_untouched() {
    let root = temp_root();
    let source = root.join("source");
    let target = root.join("target");
    fs::write(&source, b"wanted").unwrap();
    fs::write(&target, b"user artwork").unwrap();
    let hash = digest(&source, 100).unwrap();
    assert!(copy_verified(&source, &target, &root.join("staging"), &hash, 100).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"user artwork");
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(windows)]
fn exclusive_profile_lock_releases_after_handle_close() {
    let root = temp_root();
    let lock = lock_profile(&root).unwrap();
    assert!(lock_profile(&root).is_err());
    drop(lock);
    drop(lock_profile(&root).unwrap());
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(windows)]
fn export_junction_and_ancestor_are_rejected_without_touching_target() {
    let root = temp_root();
    let outside = temp_root();
    fs::write(outside.join("keep"), b"preserved").unwrap();
    let exports = root.join("exports");
    let result = Command::new("cmd.exe").args(["/D", "/C", "mklink", "/J"]).arg(&exports).arg(&outside).output().unwrap();
    assert!(result.status.success());
    assert!(app_command(Path::new("unused.exe"), &root, false).is_err());
    assert!(app_command(Path::new("unused.exe"), &exports.join("nested"), false).is_err());
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"preserved");
    fs::remove_dir(exports).unwrap();
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(outside).unwrap();
}
