#![cfg_attr(not(test), windows_subsystem = "windows")]
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
//! Hosted candidate launcher, reusing K4's verified copy/lock/export primitives.

#[cfg(all(target_os = "windows", not(debug_assertions), not(target_feature = "crt-static")))]
compile_error!("The Windows launcher requires RUSTFLAGS='-C target-feature=+crt-static'");

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

#[derive(Clone, Copy)]
struct Package<'a> {
    manifest_sha: &'a str,
    app_sha: &'a str,
    profile: &'a str,
}
impl Package<'_> {
    fn validate(&self) -> Result<()> {
        for hash in [self.manifest_sha, self.app_sha] {
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_lowercase()) {
                return Err("Package hashes must be uppercase SHA256".into());
            }
        }
        if !self.profile.starts_with("k65-hosted")
            || self.profile.len() > 64
            || !self.profile.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err("Invalid isolated hosted profile".into());
        }
        Ok(())
    }
}
fn compiled_package() -> Result<Package<'static>> {
    let package = Package {
        manifest_sha: option_env!("KOMAPHONE_PACKAGE_MANIFEST_SHA").ok_or("Launcher missing manifest pin")?,
        app_sha: option_env!("KOMAPHONE_PACKAGE_APP_SHA").ok_or("Launcher missing app pin")?,
        profile: option_env!("KOMAPHONE_PACKAGE_PROFILE").ok_or("Launcher missing profile")?,
    };
    package.validate()?;
    Ok(package)
}
const FILES: &[&str] = &[
    "photocraft.exe",
    "google-desktop-client.json",
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
    "docs/komaphone/k65-friends.md",
    "package-info.json",
    "SHA256SUMS",
];

// These bounded filesystem and process-lock helpers are retained from the K4
// launcher, with only its now-unused service binary pin removed.
include!("verified_files.rs");

fn program_directory(destination: &Path, package: Package<'_>) -> PathBuf {
    destination.join("programs").join(package.manifest_sha)
}
fn prepare(source: &Path, destination: &Path, package: Package<'_>) -> Result<()> {
    package.validate()?;
    let hashes = verify_candidate(&source.join("candidate"), package)?;
    let program = program_directory(destination, package);
    for (path, hash) in hashes {
        copy_verified(&source.join("candidate").join(&path), &program.join(path), &destination.join("copy-staging"), &hash, MAX_FILE_BYTES)?;
    }
    verify_candidate(&program, package)?;
    ensure_directory(&destination.join("data"))?;
    ensure_directory(&destination.join("config"))?;
    Ok(())
}
fn app_command(app: &Path, destination: &Path, safe_gpu: bool) -> Result<Command> {
    let google_configuration = app.parent().ok_or("Missing installed program folder")?.join("google-desktop-client.json");
    if !google_configuration.is_absolute() {
        return Err("Installed Google Desktop configuration path must be absolute".into());
    }
    let exports = destination.join("exports");
    ensure_directory(&exports)?;
    let mut command = Command::new(app);
    command
        .current_dir(&exports)
        .env("KOMAPHONE_DATA_DIR", destination.join("data"))
        .env("KOMAPHONE_EXPORTS_DIR", exports)
        .env("PHOTOCRAFT_CONFIG_DIR", destination.join("config"))
        // Only the verified sidecar path enters the child environment, never its value.
        .env("KOMAPHONE_GOOGLE_CLIENT_CONFIG", google_configuration)
        // Use the application's compiled hosted endpoint and normal public trust.
        .env_remove("KOMAPHONE_TRUSTED_CERT")
        .env_remove("KOMAPHONE_SERVICE_ENDPOINT")
        .env_remove("PHOTOCRAFT_CONTROL_TOKEN");
    if safe_gpu {
        command.arg("--safe-gpu");
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    Ok(command)
}
fn launch(source: &Path, destination: &Path, safe_gpu: bool, package: Package<'_>) -> Result<()> {
    let _profile_lock = lock_profile(destination)?;
    append_event(destination, serde_json::json!({"event":"prepare","safe_gpu":safe_gpu}))?;
    prepare(source, destination, package)?;
    let app = program_directory(destination, package).join("photocraft.exe");
    require_hash(&app, package.app_sha, MAX_FILE_BYTES)?;
    append_event(
        destination,
        serde_json::json!({"event":"verified","manifest_sha256":package.manifest_sha,"app_sha256":package.app_sha,"profile":package.profile}),
    )?;
    let mut child = app_command(&app, destination, safe_gpu)?
        .spawn()
        .map_err(|e| format!("Cannot start Komaphone: {e}. Check the Microsoft Visual C++ x64 runtime and graphics driver."))?;
    let launched = append_event(destination, serde_json::json!({"event":"launched","pid":child.id(),"safe_gpu":safe_gpu}));
    // Keep the exclusive profile lock even if log writing fails after spawning.
    let status = child.wait().map_err(|e| format!("Wait for Komaphone: {e}"))?;
    let exited = append_event(destination, serde_json::json!({"event":"process_exit","success":status.success(),"code":status.code()}));
    launched?;
    exited?;
    if !status.success() {
        return Err(format!("Komaphone exited with {status}. Try Start-Komaphone-Safe-GPU.exe after closing the app."));
    }
    Ok(())
}
fn run() -> Result<()> {
    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is unavailable")?);
    if !local.is_absolute() {
        return Err("LOCALAPPDATA must be absolute".into());
    }
    let package = compiled_package()?;
    let destination = local.join("Komaphone").join(package.profile);
    let result = (|| {
        if std::env::args_os().count() != 1 {
            return Err("This helper accepts no command-line arguments".into());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let safe_gpu = match exe.file_name().and_then(|s| s.to_str()).map(str::to_ascii_lowercase).as_deref() {
            Some("start-komaphone.exe") => false,
            Some("start-komaphone-safe-gpu.exe") => true,
            _ => return Err("Unexpected launcher filename".into()),
        };
        launch(exe.parent().ok_or("Missing launcher folder")?, &destination, safe_gpu, package)
    })();
    if let Err(error) = &result {
        let _ = append_event(&destination, serde_json::json!({"event":"error","message":error}));
        // Show the concrete diagnostic instead of a silent no-window failure.
        #[cfg(windows)]
        if let Some(windows) = std::env::var_os("SystemRoot") {
            let notepad = PathBuf::from(windows).join("System32/notepad.exe");
            if notepad.is_absolute() && destination.join("launcher-log.txt").is_file() {
                let _ = Command::new(notepad).arg(destination.join("launcher-log.txt")).spawn();
            }
        }
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
mod tests;
