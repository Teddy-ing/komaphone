use photocraft_sync::{Store, StoreOptions};
use std::process::Command;

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_photocraft-sync"))
}

#[test]
fn backup_and_fresh_restore_cli_preserve_live_source_and_refuse_overwrite() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let live = Store::open(source.join("service.sqlite3"), StoreOptions::default(), 0).unwrap();
    let original = std::fs::read(source.join("service.sqlite3")).unwrap();
    let backup = directory.path().join("backup.sqlite3");
    let output = command().arg("--data-dir").arg(&source).arg("--backup-to").arg(&backup).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("backup_verified"));
    assert_eq!(std::fs::read(source.join("service.sqlite3")).unwrap(), original);
    let restored = directory.path().join("restored");
    let output = command().arg("--data-dir").arg(&restored).arg("--restore-from").arg(&backup).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("restore_verified"));
    let saved = std::fs::read(restored.join("service.sqlite3")).unwrap();
    let output = command().arg("--data-dir").arg(&restored).arg("--restore-from").arg(&backup).output().unwrap();
    assert!(!output.status.success());
    assert_eq!(std::fs::read(restored.join("service.sqlite3")).unwrap(), saved);
    let output = command().arg("--data-dir").arg(&source).arg("--backup-to").arg(&backup).output().unwrap();
    assert!(!output.status.success());
    assert!(Store::open(restored.join("service.sqlite3"), StoreOptions::default(), 0).is_ok());
    let stale = directory.path().join("stale-journal");
    std::fs::create_dir(&stale).unwrap();
    std::fs::write(stale.join("service.sqlite3-journal"), b"existing recovery material").unwrap();
    let output = command().arg("--data-dir").arg(&stale).arg("--restore-from").arg(&backup).output().unwrap();
    assert!(!output.status.success());
    assert!(!stale.join("service.sqlite3").exists());
    assert_eq!(std::fs::read(stale.join("service.sqlite3-journal")).unwrap(), b"existing recovery material");
    drop(live);
}

#[test]
fn corrupt_or_future_restore_never_replaces_source_or_creates_final_file() {
    for future in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.sqlite3");
        if future {
            drop(Store::open(&source, StoreOptions::default(), 0).unwrap());
            let db = rusqlite::Connection::open(&source).unwrap();
            db.pragma_update(None, "user_version", 999).unwrap();
        } else {
            std::fs::write(&source, b"truncated backup").unwrap();
        }
        let original = std::fs::read(&source).unwrap();
        let restored = directory.path().join("restore");
        let output = command().arg("--data-dir").arg(&restored).arg("--restore-from").arg(&source).output().unwrap();
        assert!(!output.status.success());
        assert!(!restored.join("service.sqlite3").exists());
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert!(std::fs::read_dir(restored).unwrap().next().is_none());
    }
}

#[test]
fn tls_configuration_and_nonloopback_plaintext_fail_before_data_creation() {
    let directory = tempfile::tempdir().unwrap();
    for arguments in [vec!["--bind", "0.0.0.0:0"], vec!["--tls-cert", "missing-cert.pem"], vec!["--tls-key", "missing-key.pem"]] {
        let destination = directory.path().join(format!("attempt-{}", arguments[0].trim_start_matches('-')));
        let output = command().arg("--data-dir").arg(&destination).args(arguments).output().unwrap();
        assert!(!output.status.success());
        assert!(!destination.exists());
    }
}
