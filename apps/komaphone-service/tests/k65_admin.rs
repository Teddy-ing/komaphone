//! Actual administrator binary boundaries, using synthetic isolated data only.
use photocraft_party::{
    DeviceId, Participant, ParticipantId, RequestId,
    protocol::{Operation, RpcRequest},
};
use photocraft_sync::{Store, StoreOptions};
use std::process::Command;

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_photocraft-sync"))
}

#[test]
fn admission_cli_stores_only_digest_and_refuses_a_running_authority_lock() {
    let directory = tempfile::tempdir().unwrap();
    let output = command().arg("--data-dir").arg(directory.path()).args(["--create-admission", "--admission-hours", "1"]).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let admission = response["admission"].as_str().unwrap();
    photocraft_party::protocol::validate_secret(admission).unwrap();
    let path = directory.path().join("service.sqlite3");
    let original = std::fs::read(&path).unwrap();
    assert!(!original.windows(admission.len()).any(|bytes| bytes == admission.as_bytes()));
    let lock = std::fs::OpenOptions::new().read(true).write(true).open(directory.path().join("service.lock")).unwrap();
    lock.try_lock().unwrap();
    let output = command().arg("--data-dir").arg(directory.path()).arg("--create-admission").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("already running"));
    assert!(output.stdout.is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    drop(lock);
    let output = command().arg("--data-dir").arg(directory.path()).args(["--create-admission", "--admission-hours", "169"]).output().unwrap();
    assert!(!output.status.success());
}

#[test]
fn restore_cli_automatically_quarantines_profiles_preserving_original() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.sqlite3");
    let mut store = Store::open(&source, StoreOptions::default(), 0).unwrap();
    let participant = Participant {
        id: ParticipantId::new(&format!("{:032x}", 1)).unwrap(),
        device: DeviceId::new(&format!("{:032x}", 2)).unwrap(),
        display_name: "Original author".into(),
    };
    store
        .execute(
            None,
            &RpcRequest {
                protocol_version: photocraft_party::protocol::PROTOCOL_VERSION,
                request_id: RequestId::new(&format!("{:032x}", 3)).unwrap(),
                operation: Operation::Pair { participant: participant.clone(), credential: "1".repeat(64) },
            },
            0,
        )
        .unwrap();
    drop(store);
    let original = std::fs::read(&source).unwrap();
    let restored = directory.path().join("restored");
    let output = command().arg("--data-dir").arg(&restored).arg("--restore-from").arg(&source).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["all_profiles_quarantined"], true);
    assert_eq!(std::fs::read(&source).unwrap(), original);
    let db = rusqlite::Connection::open(restored.join("service.sqlite3")).unwrap();
    let (label, revoked): (String, bool) =
        db.query_row("SELECT label,revoked FROM hosted_profiles WHERE profile=?1", [participant.id.as_str()], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(label, "Original author");
    assert!(revoked);
    drop(db);
    let output = command().arg("--data-dir").arg(&restored).arg("--admit-profile").arg(participant.id.as_str()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let db = rusqlite::Connection::open(restored.join("service.sqlite3")).unwrap();
    let revoked: bool = db.query_row("SELECT revoked FROM hosted_profiles WHERE profile=?1", [participant.id.as_str()], |row| row.get(0)).unwrap();
    assert!(!revoked);
}
