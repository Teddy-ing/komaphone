#![cfg_attr(not(test), windows_subsystem = "windows")]
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(all(target_os = "windows", not(debug_assertions), not(target_feature = "crt-static")))]
compile_error!("The distributable Windows launcher requires RUSTFLAGS='-C target-feature=+crt-static'");

use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

type Result<T> = std::result::Result<T, String>;
const MAX_FILE_BYTES: u64 = 200 * 1024 * 1024;
static COPY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Pins are compiled into this package's launcher, never read from ambient runtime
/// environment variables or an editable settings file alongside an executable.
#[derive(Clone, Copy)]
struct Package<'a> {
    manifest_sha: &'a str,
    app_sha: &'a str,
    service_sha: &'a str,
    cert_sha: &'a str,
    endpoint: &'a str,
    profile: &'a str,
}

impl<'a> Package<'a> {
    fn validate(&self) -> Result<()> {
        for hash in [self.manifest_sha, self.app_sha, self.service_sha, self.cert_sha] {
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_lowercase()) {
                return Err("Package hashes must be uppercase SHA256".into());
            }
        }
        if !self.profile.starts_with("k4-lan-")
            || self.profile.len() > 64
            || !self.profile.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err("Invalid isolated package profile".into());
        }
        let authority = self.endpoint.strip_prefix("https://").ok_or("Private service must use HTTPS")?;
        let (host, port) = authority.split_once(':').ok_or("Private service needs an explicit port")?;
        let ip: std::net::Ipv4Addr = host.parse().map_err(|_| "Private service must use a numeric IPv4 address")?;
        let port: u16 = port.parse().map_err(|_| "Invalid private service port")?;
        if !ip.is_private() || port == 0 || self.endpoint != format!("https://{ip}:{port}") {
            return Err("Service endpoint must be canonical private IPv4 HTTPS".into());
        }
        Ok(())
    }
}

fn compiled_package() -> Result<Package<'static>> {
    let package = Package {
        manifest_sha: option_env!("KOMAPHONE_PACKAGE_MANIFEST_SHA").ok_or("Launcher missing compiled manifest pin")?,
        app_sha: option_env!("KOMAPHONE_PACKAGE_APP_SHA").ok_or("Launcher missing compiled app pin")?,
        service_sha: option_env!("KOMAPHONE_PACKAGE_SERVICE_SHA").ok_or("Launcher missing compiled service pin")?,
        cert_sha: option_env!("KOMAPHONE_PACKAGE_CERT_SHA").ok_or("Launcher missing compiled certificate pin")?,
        endpoint: option_env!("KOMAPHONE_PACKAGE_ENDPOINT").ok_or("Launcher missing compiled endpoint")?,
        profile: option_env!("KOMAPHONE_PACKAGE_PROFILE").ok_or("Launcher missing compiled profile")?,
    };
    package.validate()?;
    Ok(package)
}
const FILES: &[&str] = &[
    "photocraft.exe",
    "photocraft-sync.exe",
    "LICENSE-MIT",
    "LICENSE-APACHE",
    "NOTICE",
    "ATTRIBUTION.md",
    "assets/fonts/OFL-Inter.txt",
    "assets/fonts/OFL-JetBrainsMono.txt",
    "assets/icons/LICENSE-lucide.txt",
    "assets/icons/LICENSE-noun-magnetic-lasso.txt",
    "assets/icons/LICENSE-symmetry-axes.txt",
    "assets/dict/LICENSE-SCOWL.txt",
    "assets/app-icon/LICENSE.txt",
    "crates/algo/data/LICENSE-libfacedetection.txt",
    "crates/ui-egui/src/LICENSE-solarized.txt",
    "crates/ui-egui/src/i18n/LICENSE-translations.txt",
    "docs/komaphone/k4-service.md",
    "docs/komaphone/k4-party-release.md",
    "QUICKSTART.md",
    "package-info.json",
    "docs/komaphone/status.md",
    "SHA256SUMS",
];

fn metadata(path: &Path) -> Result<fs::Metadata> {
    let value = fs::symlink_metadata(path).map_err(|e| format!("Inspect {path:?}: {e}"))?;
    if value.file_type().is_symlink() {
        return Err(format!("Symbolic links are forbidden: {path:?}"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if value.file_attributes() & 0x400 != 0 {
            return Err(format!("Reparse points are forbidden: {path:?}"));
        }
    }
    Ok(value)
}

fn check_ancestors(path: &Path) -> Result<()> {
    if path.ancestors().count() > 64 {
        return Err("Path ancestor depth exceeds limit".into());
    }
    for parent in path.ancestors() {
        match fs::symlink_metadata(parent) {
            Ok(_) => {
                metadata(parent)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("Inspect ancestor {parent:?}: {e}")),
        }
    }
    Ok(())
}

fn ensure_directory(path: &Path) -> Result<()> {
    check_ancestors(path)?;
    fs::create_dir_all(path).map_err(|e| format!("Create directory {path:?}: {e}"))?;
    if !metadata(path)?.is_dir() {
        return Err(format!("Not a directory: {path:?}"));
    }
    Ok(())
}

fn digest(path: &Path, limit: u64) -> Result<String> {
    let info = metadata(path)?;
    if !info.is_file() || info.len() > limit {
        return Err(format!("Not a bounded regular file: {path:?}"));
    }
    let mut file = File::open(path).map_err(|e| format!("Open {path:?}: {e}"))?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 65536];
    let mut length = 0_u64;
    loop {
        let count = file.read(&mut buffer).map_err(|e| format!("Read {path:?}: {e}"))?;
        if count == 0 {
            break;
        }
        length = length.saturating_add(count as u64);
        if length > limit {
            return Err(format!("File grew beyond limit: {path:?}"));
        }
        hash.update(buffer.get(..count).ok_or("Invalid read count")?);
    }
    Ok(format!("{:X}", hash.finalize()))
}

fn require_hash(path: &Path, expected: &str, limit: u64) -> Result<()> {
    if digest(path, limit)? != expected {
        return Err(format!("SHA256 mismatch: {path:?}"));
    }
    Ok(())
}

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let info = metadata(path)?;
    if !info.is_file() || info.len() > limit as u64 {
        return Err("Manifest is not a bounded regular file".into());
    }
    let mut bytes = Vec::new();
    File::open(path).map_err(|e| e.to_string())?.take((limit as u64).saturating_add(1)).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("Manifest exceeds read limit".into());
    }
    Ok(bytes)
}

fn parse_manifest(text: &str, package: Package<'_>) -> Result<BTreeMap<String, String>> {
    let mut hashes = BTreeMap::new();
    for line in text.lines() {
        let (hash, path) = line.split_once("  ").ok_or("Invalid manifest line")?;
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_lowercase()) || !FILES.contains(&path) || path == "SHA256SUMS" {
            return Err("Invalid manifest path/hash".into());
        }
        if hashes.insert(path.to_owned(), hash.to_owned()).is_some() {
            return Err("Duplicate manifest path".into());
        }
    }
    if hashes.len() != FILES.len().saturating_sub(1) {
        return Err("Incomplete manifest".into());
    }
    if hashes.get("photocraft.exe").map(String::as_str) != Some(package.app_sha)
        || hashes.get("photocraft-sync.exe").map(String::as_str) != Some(package.service_sha)
    {
        return Err("Unexpected candidate binaries".into());
    }
    hashes.insert("SHA256SUMS".into(), package.manifest_sha.into());
    Ok(hashes)
}

fn inventory(root: &Path, directory: &Path, depth: u8, files: &mut BTreeSet<String>) -> Result<()> {
    if depth > 16 {
        return Err("Directory nesting exceeds limit".into());
    }
    if !metadata(directory)?.is_dir() {
        return Err("Inventory root is not a directory".into());
    }
    for entry in fs::read_dir(directory).map_err(|e| format!("List {directory:?}: {e}"))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let info = metadata(&path)?;
        if info.is_dir() {
            inventory(root, &path, depth + 1, files)?;
        } else if info.is_file() {
            let relative = path.strip_prefix(root).map_err(|e| e.to_string())?.to_str().ok_or("Non-Unicode candidate path")?.replace('\\', "/");
            if !FILES.contains(&relative.as_str()) || !files.insert(relative) {
                return Err("Unexpected candidate file".into());
            }
        } else {
            return Err("Non-regular candidate object".into());
        }
    }
    Ok(())
}

fn verify_candidate(root: &Path, package: Package<'_>) -> Result<BTreeMap<String, String>> {
    check_ancestors(root)?;
    let manifest_path = root.join("SHA256SUMS");
    let bytes = read_bounded(&manifest_path, 16384)?;
    if format!("{:X}", Sha256::digest(&bytes)) != package.manifest_sha {
        return Err("Candidate manifest SHA256 mismatch".into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
    let hashes = parse_manifest(text, package)?;
    let mut files = BTreeSet::new();
    inventory(root, root, 0, &mut files)?;
    if files.len() != FILES.len() {
        return Err("Candidate file set is incomplete".into());
    }
    for (path, hash) in &hashes {
        require_hash(&root.join(path), hash, MAX_FILE_BYTES)?;
    }
    Ok(hashes)
}

fn copy_verified(source: &Path, destination: &Path, staging_root: &Path, hash: &str, limit: u64) -> Result<()> {
    require_hash(source, hash, limit)?;
    let parent = destination.parent().ok_or("Missing destination parent")?;
    ensure_directory(parent)?;
    match fs::symlink_metadata(destination) {
        Ok(_) => return require_hash(destination, hash, limit),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("Inspect copy destination: {e}")),
    }
    ensure_directory(staging_root)?;
    let (staging, mut output) = create_staging(staging_root)?;
    let input = File::open(source).map_err(|e| e.to_string())?;
    let copied = std::io::copy(&mut input.take(limit.saturating_add(1)), &mut output).map_err(|e| format!("Copy to staging {staging:?}: {e}"))?;
    if copied > limit {
        return Err(format!("Copy exceeded size limit; staging retained: {staging:?}"));
    }
    output.sync_all().map_err(|e| e.to_string())?;
    drop(output);
    require_hash(&staging, hash, limit)?;
    // An NTFS hard link publishes verified bytes atomically and never replaces a file.
    // Staging lives outside the exact candidate inventory so interrupted copies remain retryable.
    match fs::hard_link(&staging, destination) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => require_hash(destination, hash, limit)?,
        Err(e) => return Err(format!("Publish copy on local NTFS destination: {e}; staging retained: {staging:?}")),
    }
    fs::remove_file(&staging).map_err(|e| format!("Remove owned copy staging: {e}"))?;
    require_hash(destination, hash, limit)?;
    Ok(())
}

fn create_staging(parent: &Path) -> Result<(PathBuf, File)> {
    for _ in 0..32 {
        let sequence = COPY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".komaphone-copy-{}-{sequence}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("Create owned staging: {e}")),
        }
    }
    Err("Could not reserve a fresh copy staging file".into())
}

fn prepare(source: &Path, destination: &Path, package: Package<'_>) -> Result<()> {
    package.validate()?;
    let candidate = source.join("candidate");
    let hashes = verify_candidate(&candidate, package)?;
    let public_cert = source.join("publiccert/service.pem");
    check_ancestors(&public_cert)?;
    require_hash(&public_cert, package.cert_sha, 16384)?;
    for (path, hash) in hashes {
        copy_verified(&candidate.join(&path), &destination.join("candidate").join(path), &destination.join("copy-staging"), &hash, MAX_FILE_BYTES)?;
    }
    copy_verified(&public_cert, &destination.join("publiccert/service.pem"), &destination.join("copy-staging"), package.cert_sha, 16384)?;
    verify_candidate(&destination.join("candidate"), package)?;
    ensure_directory(&destination.join("data"))?;
    ensure_directory(&destination.join("config"))?;
    Ok(())
}

fn append_event(destination: &Path, value: serde_json::Value) -> Result<()> {
    ensure_directory(destination)?;
    let path = destination.join("launcher-log.txt");
    check_ancestors(&path)?;
    if path.exists() && metadata(&path)?.len() > 1024 * 1024 {
        return Err("Launcher log exceeds limit".into());
    }
    let mut file = OpenOptions::new().append(true).create(true).open(&path).map_err(|e| e.to_string())?;
    writeln!(file, "{value}").map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}

/// Keep one launcher/app pair per profile. The OS releases this exclusive handle
/// on a crash; an existing empty file is not a stale PID lock and can be reused.
fn lock_profile(destination: &Path) -> Result<File> {
    ensure_directory(destination)?;
    let path = destination.join("launcher.lock");
    check_ancestors(&path)?;
    if path.exists() && (!metadata(&path)?.is_file() || metadata(&path)?.len() != 0) {
        return Err("Unexpected profile lock file; no file was replaced".into());
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    options.open(&path).map_err(|e| format!("Cannot exclusively open this profile; close its existing Komaphone window before relaunching: {e}"))
}

fn launch(source: &Path, destination: &Path, safe_gpu: bool, package: Package<'_>) -> Result<()> {
    let _profile_lock = lock_profile(destination)?;
    append_event(destination, serde_json::json!({"event":"prepare","safe_gpu":safe_gpu}))?;
    prepare(source, destination, package)?;
    let candidate = destination.join("candidate");
    let app = candidate.join("photocraft.exe");
    let cert = destination.join("publiccert/service.pem");
    require_hash(&app, package.app_sha, MAX_FILE_BYTES)?;
    require_hash(&cert, package.cert_sha, 16384)?;
    append_event(
        destination,
        serde_json::json!({"event":"verified","manifest_sha256":package.manifest_sha,"app_sha256":package.app_sha,"service_sha256":package.service_sha,"public_cert_sha256":package.cert_sha,"endpoint":package.endpoint,"profile":package.profile}),
    )?;
    let mut command = app_command(&app, destination, safe_gpu, package)?;
    let child = command.spawn().map_err(|e| format!("Launch native candidate: {e}"))?;
    // Once the child exists, keep the exclusive profile handle until it exits,
    // even when recording its PID fails (for example on a full disk).
    let launched_event = append_event(destination, serde_json::json!({"event":"launched","pid":child.id(),"safe_gpu":safe_gpu,"executable":app}));
    wait_after_spawn(child, destination, launched_event)
}

fn app_command(app: &Path, destination: &Path, safe_gpu: bool, package: Package<'_>) -> Result<Command> {
    // Relative Gallery exports must never add files to the pinned candidate.
    // Revalidate this user-data directory before every child launch.
    let exports = destination.join("exports");
    ensure_directory(&exports)?;
    let mut command = Command::new(app);
    command
        .current_dir(exports)
        .env("KOMAPHONE_DATA_DIR", destination.join("data"))
        .env("PHOTOCRAFT_CONFIG_DIR", destination.join("config"))
        .env("KOMAPHONE_TRUSTED_CERT", destination.join("publiccert/service.pem"))
        .env("KOMAPHONE_SERVICE_ENDPOINT", package.endpoint);
    if safe_gpu {
        command.arg("--safe-gpu");
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW hides only the dev-build console; native UI remains.
    }
    Ok(command)
}

fn wait_after_spawn(mut child: std::process::Child, destination: &Path, launched_event: Result<()>) -> Result<()> {
    let status = child.wait().map_err(|e| format!("Wait for native candidate: {e}"))?;
    let exit_event = append_event(destination, serde_json::json!({"event":"process_exit","success":status.success(),"code":status.code()}));
    launched_event?;
    exit_event?;
    if !status.success() {
        return Err(format!("Native app exited with {status}; check the app runtime and graphics prerequisites"));
    }
    Ok(())
}

fn run() -> Result<()> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is unavailable")?;
    let local = PathBuf::from(local);
    if !local.is_absolute() {
        return Err("LOCALAPPDATA must be absolute".into());
    }
    let package = compiled_package()?;
    let destination = local.join("Komaphone").join(package.profile);
    let result = (|| {
        if std::env::args_os().count() != 1 {
            return Err("This GUI helper does not accept command-line arguments".into());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let name = exe.file_name().and_then(|s| s.to_str()).ok_or("Invalid launcher filename")?.to_ascii_lowercase();
        let safe_gpu = match name.as_str() {
            "start-komaphone.exe" => false,
            "start-komaphone-safe-gpu.exe" => true,
            _ => return Err("Unexpected launcher filename".into()),
        };
        launch(exe.parent().ok_or("Missing launcher directory")?, &destination, safe_gpu, package)
    })();
    if let Err(error) = &result {
        let _ = append_event(&destination, serde_json::json!({"event":"error","message":error}));
    }
    result
}

fn main() {
    if run().is_err() {
        std::process::exit(1);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    struct Fixture {
        root: PathBuf,
        manifest_sha: String,
        app_sha: String,
        service_sha: String,
        cert_sha: String,
    }
    impl Fixture {
        fn new() -> Self {
            let root = temp_root();
            let candidate = root.join("candidate");
            let mut rows = Vec::new();
            for path in FILES.iter().filter(|path| **path != "SHA256SUMS") {
                let full = candidate.join(path);
                fs::create_dir_all(full.parent().unwrap()).unwrap();
                fs::write(&full, path.as_bytes()).unwrap();
                rows.push(format!("{}  {path}", digest(&full, MAX_FILE_BYTES).unwrap()));
            }
            let manifest = candidate.join("SHA256SUMS");
            fs::write(&manifest, rows.join("\n") + "\n").unwrap();
            let cert = root.join("publiccert/service.pem");
            fs::create_dir(cert.parent().unwrap()).unwrap();
            fs::write(&cert, b"synthetic public certificate fixture").unwrap();
            Self {
                manifest_sha: digest(&manifest, 16384).unwrap(),
                app_sha: digest(&candidate.join("photocraft.exe"), MAX_FILE_BYTES).unwrap(),
                service_sha: digest(&candidate.join("photocraft-sync.exe"), MAX_FILE_BYTES).unwrap(),
                cert_sha: digest(&cert, 16384).unwrap(),
                root,
            }
        }
        fn package(&self) -> Package<'_> {
            Package {
                manifest_sha: &self.manifest_sha,
                app_sha: &self.app_sha,
                service_sha: &self.service_sha,
                cert_sha: &self.cert_sha,
                endpoint: "https://10.0.0.40:43127",
                profile: "k4-lan-test",
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    fn temp_root() -> PathBuf {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let sequence = COPY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("komaphone-launcher-{}-{stamp}-{sequence}", std::process::id()));
        fs::create_dir(&path).unwrap();
        path
    }
    #[test]
    fn exact_manifest_rejects_traversal_duplicate_and_unexpected_files() {
        let fixture = Fixture::new();
        let text = fs::read_to_string(fixture.root.join("candidate/SHA256SUMS")).unwrap();
        assert_eq!(parse_manifest(&text, fixture.package()).unwrap().len(), 22);
        assert!(parse_manifest(&text.replace("LICENSE-MIT", "../LICENSE-MIT"), fixture.package()).is_err());
        assert!(parse_manifest(&format!("{text}\n{}  NOTICE", "A".repeat(64)), fixture.package()).is_err());
        assert!(parse_manifest(&text.replace("NOTICE", "credentials.json"), fixture.package()).is_err());
    }
    #[test]
    fn configuration_rejects_public_plaintext_ambiguous_and_unbounded_routes() {
        let fixture = Fixture::new();
        fixture.package().validate().unwrap();
        for endpoint in [
            "http://10.0.0.40:43127",
            "https://8.8.8.8:43127",
            "https://127.0.0.1:43127",
            "https://example.com:43127",
            "https://user@10.0.0.40:43127",
            "https://10.0.0.40:0",
            "https://10.0.0.40:43127/",
            "https://10.0.0.40:43127?token=x",
            "https://010.0.0.40:43127",
            "https://10.0.0.40:043127",
        ] {
            let mut package = fixture.package();
            package.endpoint = endpoint;
            assert!(package.validate().is_err(), "accepted {endpoint}");
        }
        for profile in ["../k4-lan-test", "k4-lan-test/child", "k4-lan-test:stream", "k4-lan-test ", "K4-lan-test", ""] {
            let mut package = fixture.package();
            package.profile = profile;
            assert!(package.validate().is_err(), "accepted {profile}");
        }
        let mut package = fixture.package();
        package.cert_sha = "not a hash";
        assert!(package.validate().is_err());
    }
    #[test]
    fn copy_reuses_identical_and_preserves_conflicting_file() {
        let root = temp_root();
        let source = root.join("source");
        let target = root.join("target");
        fs::write(&source, b"verified bytes").unwrap();
        let hash = digest(&source, 100).unwrap();
        copy_verified(&source, &target, &root.join("staging"), &hash, 100).unwrap();
        copy_verified(&source, &target, &root.join("staging"), &hash, 100).unwrap();
        fs::write(&target, b"existing different bytes").unwrap();
        assert!(copy_verified(&source, &target, &root.join("staging"), &hash, 100).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"existing different bytes");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[cfg(windows)]
    fn profile_lock_refuses_second_launcher_and_releases_without_stale_pid() {
        let root = temp_root();
        let first = lock_profile(&root).unwrap();
        assert!(lock_profile(&root).is_err());
        drop(first);
        let later = lock_profile(&root).unwrap();
        drop(later);
        fs::write(root.join("launcher.lock"), b"unrelated file").unwrap();
        assert!(lock_profile(&root).is_err());
        assert_eq!(fs::read(root.join("launcher.lock")).unwrap(), b"unrelated file");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[cfg(windows)]
    fn launched_log_failure_still_waits_with_profile_locked() {
        const CHILD_ROOT: &str = "KOMAPHONE_LAUNCHER_TEST_CHILD_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = PathBuf::from(root);
            fs::write(root.join("child-started"), b"ready").unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !root.join("child-release").exists() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(root.join("child-release").exists(), "child was not released");
            return;
        }
        let root = temp_root();
        let lock = lock_profile(&root).unwrap();
        fs::write(root.join("launcher-log.txt"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::launched_log_failure_still_waits_with_profile_locked", "--nocapture"])
            .env(CHILD_ROOT, &root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let log_result = append_event(&root, serde_json::json!({"event":"launched"}));
        assert!(log_result.is_err());
        let worker_root = root.clone();
        let waiter = std::thread::spawn(move || {
            let _lock = lock;
            wait_after_spawn(child, &worker_root, log_result)
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !root.join("child-started").exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let started = root.join("child-started").exists();
        let premature_exit = waiter.is_finished();
        let refused = lock_profile(&root).is_err();
        fs::write(root.join("child-release"), b"finish").unwrap();
        let result = waiter.join().unwrap();
        assert!(started && !premature_exit && refused, "logging failure released a running child's profile");
        assert!(result.is_err(), "logging failure should still be reported after exit");
        drop(lock_profile(&root).unwrap());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn incorrect_source_hash_does_not_create_destination() {
        let root = temp_root();
        let source = root.join("source");
        let target = root.join("target");
        fs::write(&source, b"wrong").unwrap();
        assert!(copy_verified(&source, &target, &root.join("staging"), &"0".repeat(64), 100).is_err());
        assert!(!target.exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn bounded_manifest_and_path_depth_reject_unbounded_input() {
        let root = temp_root();
        let file = root.join("large");
        fs::write(&file, vec![b'a'; 16385]).unwrap();
        assert!(read_bounded(&file, 16384).is_err());
        let mut deep = root.clone();
        for _ in 0..70 {
            deep.push("nested");
        }
        assert!(check_ancestors(&deep).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn leftover_partial_staging_does_not_prevent_retry() {
        let root = temp_root();
        let source = root.join("source");
        let target = root.join("target");
        fs::write(&source, b"complete source").unwrap();
        let hash = digest(&source, 100).unwrap();
        let (staging, mut file) = create_staging(&root).unwrap();
        file.write_all(b"interrupted").unwrap();
        drop(file);
        copy_verified(&source, &target, &root.join("staging"), &hash, 100).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"complete source");
        assert_eq!(fs::read(&staging).unwrap(), b"interrupted");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn candidate_rejects_changed_missing_extra_and_wrong_certificate_before_copy() {
        let fixture = Fixture::new();
        verify_candidate(&fixture.root.join("candidate"), fixture.package()).unwrap();
        let destination = temp_root();
        fs::write(fixture.root.join("publiccert/service.pem"), b"wrong certificate").unwrap();
        assert!(prepare(&fixture.root, &destination, fixture.package()).is_err());
        assert!(!destination.join("candidate").exists());
        fs::write(fixture.root.join("candidate/extra.txt"), b"unexpected").unwrap();
        assert!(verify_candidate(&fixture.root.join("candidate"), fixture.package()).is_err());
        fs::remove_file(fixture.root.join("candidate/extra.txt")).unwrap();
        fs::write(fixture.root.join("candidate/NOTICE"), b"changed").unwrap();
        assert!(verify_candidate(&fixture.root.join("candidate"), fixture.package()).is_err());
        fs::remove_file(fixture.root.join("candidate/NOTICE")).unwrap();
        assert!(verify_candidate(&fixture.root.join("candidate"), fixture.package()).is_err());
        fs::remove_dir_all(destination).unwrap();
    }
    #[test]
    fn preparation_reuses_stable_profile_and_preserves_conflicts_and_orphan_staging() {
        let fixture = Fixture::new();
        let destination = temp_root();
        prepare(&fixture.root, &destination, fixture.package()).unwrap();
        let sentinel = destination.join("data/restart-sentinel.txt");
        fs::write(&sentinel, b"keep this local profile").unwrap();
        let staging = destination.join("copy-staging/interrupted-copy.tmp");
        fs::write(&staging, b"incomplete interrupted copy").unwrap();
        prepare(&fixture.root, &destination, fixture.package()).unwrap();
        assert_eq!(fs::read(&sentinel).unwrap(), b"keep this local profile");
        assert_eq!(fs::read(&staging).unwrap(), b"incomplete interrupted copy");
        verify_candidate(&destination.join("candidate"), fixture.package()).unwrap();
        fs::write(destination.join("publiccert/service.pem"), b"existing different certificate").unwrap();
        assert!(prepare(&fixture.root, &destination, fixture.package()).is_err());
        assert_eq!(fs::read(destination.join("publiccert/service.pem")).unwrap(), b"existing different certificate");
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn relative_export_preserves_candidate_inventory_and_next_preparation() {
        const EXPORT_CHILD: &str = "KOMAPHONE_LAUNCHER_TEST_EXPORT_CHILD";
        if std::env::var_os(EXPORT_CHILD).is_some() {
            fs::write("ss", b"relative Gallery export").unwrap();
            return;
        }
        let fixture = Fixture::new();
        let destination = temp_root();
        prepare(&fixture.root, &destination, fixture.package()).unwrap();
        let mut command = app_command(&std::env::current_exe().unwrap(), &destination, false, fixture.package()).unwrap();
        assert_eq!(command.get_current_dir(), Some(destination.join("exports").as_path()));
        let output =
            command.args(["--exact", "tests::relative_export_preserves_candidate_inventory_and_next_preparation"]).env(EXPORT_CHILD, "1").output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(fs::read(destination.join("exports/ss")).unwrap(), b"relative Gallery export");
        assert!(!destination.join("candidate/ss").exists());
        prepare(&fixture.root, &destination, fixture.package()).unwrap();
        verify_candidate(&destination.join("candidate"), fixture.package()).unwrap();
        assert_eq!(fs::read(destination.join("exports/ss")).unwrap(), b"relative Gallery export");

        fs::write(destination.join("candidate/ss"), b"unexpected installed file").unwrap();
        assert_eq!(prepare(&fixture.root, &destination, fixture.package()).unwrap_err(), "Unexpected candidate file");
        assert_eq!(fs::read(destination.join("candidate/ss")).unwrap(), b"unexpected installed file");
        fs::write(fixture.root.join("candidate/ss"), b"unexpected source file").unwrap();
        assert_eq!(verify_candidate(&fixture.root.join("candidate"), fixture.package()).unwrap_err(), "Unexpected candidate file");
        assert_eq!(fs::read(fixture.root.join("candidate/ss")).unwrap(), b"unexpected source file");
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn export_working_directory_refuses_existing_file_without_replacing_it() {
        let fixture = Fixture::new();
        let destination = temp_root();
        let exports = destination.join("exports");
        fs::write(&exports, b"existing user file").unwrap();
        assert!(app_command(&fixture.root.join("candidate/photocraft.exe"), &destination, false, fixture.package()).is_err());
        assert_eq!(fs::read(exports).unwrap(), b"existing user file");
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    #[cfg(windows)]
    fn export_working_directory_refuses_junction_and_reparse_ancestor() {
        let fixture = Fixture::new();
        let destination = temp_root();
        let outside = temp_root();
        let sentinel = outside.join("keep.txt");
        fs::write(&sentinel, b"preserved outside junction").unwrap();
        let exports = destination.join("exports");
        let output = Command::new("cmd.exe").args(["/D", "/C", "mklink", "/J"]).arg(&exports).arg(&outside).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(app_command(&fixture.root.join("candidate/photocraft.exe"), &destination, false, fixture.package()).is_err());
        assert!(app_command(&fixture.root.join("candidate/photocraft.exe"), &exports.join("nested"), false, fixture.package()).is_err());
        assert!(!outside.join("nested").exists());
        assert_eq!(fs::read(sentinel).unwrap(), b"preserved outside junction");
        fs::remove_dir(exports).unwrap();
        fs::remove_dir_all(destination).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
}
