//! Explicit acceptance probe; never invoked by the shipped application.
//! Reads one approved Windows session in memory and never changes app/account state.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

const HELP: &str = "Komaphone restore session probe\nUsage: restore_session_probe --database ABSOLUTE_ROOMS_SQLITE3 --receipt ABSOLUTE_PRIVATE_JSON --receipt-sha256 SHA256\nRequires an approved, pinned backup/tunnel receipt. Reads exactly one Windows credential.\nChecks the same session: live identity200 -> restored401/session -> live identity200.\nNo account mutations, token output, token files, redirects or proxies. --help performs no reads or requests.";

#[cfg(windows)]
fn main() {
    if let Err(error) = native::run() {
        eprintln!("Restore session probe failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    println!("{HELP}\nExecution requires Windows Credential Manager.");
}

#[cfg(windows)]
mod native {
    use photocraft_party::protocol::{Fault, validate_secret};
    use photocraft_room_client::{CredentialStore, WindowsCredentials, hosted_auth::SERVICE_ENDPOINT, normalize_endpoint};
    use photocraft_scene::world_protocol as world;
    use rusqlite::{Connection, OpenFlags};
    use serde::Deserialize;
    use sha2::{Digest, Sha256};
    use std::{fs::File, io::Read, os::windows::fs::MetadataExt, path::Path, time::Duration};

    type Checked<T> = Result<T, &'static str>;
    const MAX_RECEIPT: u64 = 16 * 1024;
    const MAX_STATE: usize = 5 * 1024 * 1024;

    // Receipt is produced independently from the exact backup and isolated restore.
    // It contains a verifier hash, never a bearer, and must remain a private file.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Receipt {
        version: u32,
        production_endpoint: String,
        restore_endpoint: String,
        tunnel_verified: bool,
        backup_sha256: String,
        backup_generation: String,
        profile: String,
        device: String,
        slot: String,
        session_verifier: String,
        session_expires_at_ms: u64,
        backup_session_revoked: bool,
        backup_device_revoked: bool,
        backup_profile_revoked: bool,
        restored_session_revoked: bool,
        restored_profile_revoked: bool,
        restored_session_present: bool,
        restore_backup_sha256: String,
        receipt_expires_at_ms: u64,
        #[serde(default)]
        nonmember_canvas: Option<String>,
    }

    // Only selected metadata is decoded; no account directory or outbox is read.
    #[derive(Deserialize)]
    struct State {
        schema: u32,
        namespace: String,
        slot: String,
        active: bool,
        hosted_device: Option<String>,
        session_expires_at_ms: u64,
        relocation: Option<serde_json::Value>,
        relocation_session: Option<serde_json::Value>,
        linking_legacy: bool,
        status: Status,
    }
    #[derive(Deserialize)]
    struct Status {
        endpoint: Option<String>,
        profile: Option<Profile>,
        hosted: bool,
        signed_in: bool,
        signing_in: bool,
        paired: bool,
    }
    #[derive(Deserialize)]
    struct Profile {
        id: String,
        device: String,
    }
    #[derive(PartialEq, Eq)]
    struct Scope {
        namespace: String,
        key: String,
    }

    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
    fn hex(value: &str, length: usize) -> bool {
        value.len() == length && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
    fn now_ms() -> Checked<u64> {
        let now: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
        u64::try_from(now.timestamp_millis()).map_err(|_| "Invalid system clock")
    }
    fn regular_file(path: &Path) -> Checked<()> {
        let metadata = std::fs::symlink_metadata(path).map_err(|_| "Required file is unavailable")?;
        if !path.is_absolute() || !metadata.is_file() || metadata.file_attributes() & 0x400 != 0 {
            return Err("Require an absolute regular file without a reparse point");
        }
        Ok(())
    }
    fn receipt_bytes(path: &Path) -> Checked<Vec<u8>> {
        regular_file(path)?;
        let mut bytes = Vec::new();
        File::open(path).map_err(|_| "Receipt unavailable")?.take(MAX_RECEIPT + 1).read_to_end(&mut bytes).map_err(|_| "Cannot read receipt")?;
        if bytes.len() as u64 > MAX_RECEIPT {
            return Err("Receipt exceeds its byte limit");
        }
        Ok(bytes)
    }
    fn decode_receipt(bytes: &[u8], expected_hash: &str, now: u64) -> Checked<Receipt> {
        if bytes.len() as u64 > MAX_RECEIPT || !hex(expected_hash, 64) || digest(bytes) != expected_hash {
            return Err("Receipt hash or size mismatch");
        }
        let receipt: Receipt = serde_json::from_slice(bytes).map_err(|_| "Invalid receipt schema")?;
        validate_receipt(&receipt, now)?;
        Ok(receipt)
    }
    fn validate_receipt(receipt: &Receipt, now: u64) -> Checked<()> {
        if receipt.version != 1
            || receipt.production_endpoint != SERVICE_ENDPOINT
            || !receipt.tunnel_verified
            || !hex(&receipt.backup_sha256, 64)
            || receipt.restore_backup_sha256 != receipt.backup_sha256
            || receipt.backup_generation.is_empty()
            || receipt.backup_generation.len() > 24
            || !receipt.backup_generation.bytes().all(|b| b.is_ascii_digit())
            || !hex(&receipt.profile, 32)
            || !hex(&receipt.device, 32)
            || !hex(&receipt.slot, 32)
            || !hex(&receipt.session_verifier, 64)
            || receipt.backup_session_revoked
            || receipt.backup_device_revoked
            || receipt.backup_profile_revoked
            || !receipt.restored_session_revoked
            || !receipt.restored_profile_revoked
            || !receipt.restored_session_present
            || receipt.nonmember_canvas.as_ref().is_some_and(|id| !hex(id, 32))
        {
            return Err("Receipt does not prove the required backup and quarantine scope");
        }
        if receipt.receipt_expires_at_ms <= now
            || receipt.receipt_expires_at_ms > now.saturating_add(15 * 60 * 1000)
            || receipt.session_expires_at_ms <= receipt.receipt_expires_at_ms
        {
            return Err("Receipt expired or session lifetime is insufficient");
        }
        let normalized = normalize_endpoint(&receipt.restore_endpoint).map_err(|_| "Invalid restore endpoint")?;
        let url = reqwest::Url::parse(&normalized).map_err(|_| "Invalid restore endpoint")?;
        let loopback = url.host_str().and_then(|host| host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok()).is_some_and(|ip| ip.is_loopback());
        if normalized != receipt.restore_endpoint || url.scheme() != "http" || !loopback || url.port().is_none_or(|port| port <= 1024) {
            return Err("Restore endpoint must be the approved numeric loopback tunnel port");
        }
        Ok(())
    }
    fn validate_state(body: &[u8], checksum: &str, receipt: &Receipt) -> Checked<Scope> {
        if body.len() > MAX_STATE || !hex(checksum, 64) || digest(body) != checksum {
            return Err("Client metadata checksum or size mismatch");
        }
        let state: State = serde_json::from_slice(body).map_err(|_| "Invalid client metadata")?;
        let profile = state.status.profile.as_ref().ok_or("Client profile missing")?;
        if state.schema != 2
            || !hex(&state.namespace, 32)
            || state.slot != receipt.slot
            || !state.active
            || !state.status.hosted
            || !state.status.signed_in
            || state.status.signing_in
            || !state.status.paired
            || state.status.endpoint.as_deref() != Some(SERVICE_ENDPOINT)
            || profile.id != receipt.profile
            || profile.device != receipt.device
            || state.hosted_device.as_deref() != Some(receipt.device.as_str())
            || state.session_expires_at_ms != receipt.session_expires_at_ms
            || state.relocation.is_some()
            || state.relocation_session.is_some()
            || state.linking_legacy
        {
            return Err("Active client account does not match the approved receipt");
        }
        Ok(Scope { namespace: state.namespace, key: format!("hosted/session/{}/{}", state.slot, digest(SERVICE_ENDPOINT.as_bytes())) })
    }
    fn read_scope(path: &Path, receipt: &Receipt) -> Checked<Scope> {
        regular_file(path)?;
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .map_err(|_| "Cannot open client database read-only")?;
        db.busy_timeout(Duration::from_secs(1)).map_err(|_| "Cannot configure bounded database read")?;
        let app: u32 = db.pragma_query_value(None, "application_id", |r| r.get(0)).map_err(|_| "Cannot verify database identity")?;
        let schema: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0)).map_err(|_| "Cannot verify database schema")?;
        if app != 0x4b524f4d || schema != 2 {
            return Err("Unexpected client database identity or schema");
        }
        let (body, checksum): (Vec<u8>, String) = db
            .query_row(
                "SELECT body,digest FROM state WHERE id=1 AND typeof(body)='blob' AND length(body)<=?1
                 AND typeof(digest)='text' AND length(CAST(digest AS BLOB))=64",
                [MAX_STATE as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| "Cannot read bounded active client metadata")?;
        validate_state(&body, &checksum, receipt)
    }
    fn verify_secret(token: &str, receipt: &Receipt) -> Checked<()> {
        validate_secret(token).map_err(|_| "Stored session has invalid format")?;
        if digest(token.as_bytes()) != receipt.session_verifier {
            return Err("Stored session is not the exact backed-up session");
        }
        Ok(())
    }
    fn request(client: &reqwest::blocking::Client, endpoint: &str, token: &str, action: world::Action) -> Checked<(u16, Vec<u8>)> {
        let response = client
            .post(format!("{endpoint}/v1/canvas"))
            .bearer_auth(token)
            .json(&world::Request { version: world::VERSION, action })
            .send()
            .map_err(|_| "Identity request transport failed")?;
        let status = response.status().as_u16();
        if response.content_length().is_some_and(|n| n > 8192) {
            return Err("Identity response exceeds its byte limit");
        }
        let mut bytes = Vec::new();
        response.take(8193).read_to_end(&mut bytes).map_err(|_| "Cannot read identity response")?;
        if bytes.len() > 8192 {
            return Err("Identity response exceeds its byte limit");
        }
        Ok((status, bytes))
    }
    fn accepted(status: u16, bytes: &[u8], receipt: &Receipt) -> Checked<()> {
        if status != 200 {
            return Err("Live service did not accept the known session");
        }
        match serde_json::from_slice::<world::Response>(bytes) {
            Ok(world::Response::Identity { profile, device }) if profile.as_str() == receipt.profile && device.as_str() == receipt.device => Ok(()),
            _ => Err("Live identity does not match the approved account"),
        }
    }
    fn denied(status: u16, bytes: &[u8], expected_status: u16, expected_code: &str) -> Checked<()> {
        if status == expected_status && serde_json::from_slice::<Fault>(bytes).is_ok_and(|fault| fault.code == expected_code) {
            Ok(())
        } else {
            Err("Service did not return the required authorization denial")
        }
    }
    pub(super) fn run() -> Checked<()> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.as_slice() == ["--help"] {
            println!("{}", super::HELP);
            return Ok(());
        }
        let [database_flag, database, receipt_flag, receipt_path, hash_flag, receipt_hash] = args.as_slice() else {
            return Err("Expected --database PATH --receipt PATH --receipt-sha256 HASH");
        };
        if database_flag != "--database" || receipt_flag != "--receipt" || hash_flag != "--receipt-sha256" {
            return Err("Unexpected probe arguments");
        }
        let receipt = decode_receipt(&receipt_bytes(Path::new(receipt_path))?, receipt_hash, now_ms()?)?;
        let scope = read_scope(Path::new(database), &receipt)?;
        // Exactly one named entry; no enumeration, proof access, writes or deletes.
        let token = WindowsCredentials
            .get(&scope.namespace, &scope.key)
            .map_err(|_| "Cannot read the approved Windows session")?
            .ok_or("Approved Windows session is missing")?;
        verify_secret(&token, &receipt)?;
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| "Cannot initialize identity transport")?;
        validate_receipt(&receipt, now_ms()?)?;
        let (status, bytes) = request(&client, SERVICE_ENDPOINT, &token, world::Action::Identity {})?;
        accepted(status, &bytes, &receipt)?;
        if let Some(canvas) = &receipt.nonmember_canvas {
            validate_receipt(&receipt, now_ms()?)?;
            let canvas = photocraft_scene::CanvasId::new(canvas).map_err(|_| "Invalid approved nonmember canvas")?;
            let (status, bytes) = request(&client, SERVICE_ENDPOINT, &token, world::Action::Info { canvas })?;
            denied(status, &bytes, 403, "unauthorized")?;
        }
        validate_receipt(&receipt, now_ms()?)?;
        let (status, bytes) = request(&client, &receipt.restore_endpoint, &token, world::Action::Identity {})?;
        denied(status, &bytes, 401, "session")?;
        validate_receipt(&receipt, now_ms()?)?;
        let (status, bytes) = request(&client, SERVICE_ENDPOINT, &token, world::Action::Identity {})?;
        accepted(status, &bytes, &receipt)?;
        if read_scope(Path::new(database), &receipt)? != scope {
            return Err("Active client scope changed during verification");
        }
        drop(token);
        let nonmember_denied = receipt.nonmember_canvas.is_some();
        println!(
            "{{\"known_session_hash_matched\":true,\"live_identity_before\":200,\"restored_identity\":401,\"restored_fault\":\"session\",\"live_identity_after\":200,\"client_scope_unchanged\":true,\"nonmember_canvas_denied\":{nonmember_denied},\"receipt_sha256\":\"{receipt_hash}\"}}"
        );
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        const NOW: u64 = 1_800_000_000_000;
        fn receipt_value() -> serde_json::Value {
            serde_json::json!({"version":1,"production_endpoint":SERVICE_ENDPOINT,"restore_endpoint":"http://127.0.0.1:49501","tunnel_verified":true,
                "backup_sha256":"a".repeat(64),"backup_generation":"123456789","profile":"b".repeat(32),"device":"c".repeat(32),"slot":"d".repeat(32),
                "session_verifier":digest("e".repeat(64).as_bytes()),"session_expires_at_ms":NOW+3_600_000,"backup_session_revoked":false,
                "backup_device_revoked":false,"backup_profile_revoked":false,"restored_session_revoked":true,"restored_profile_revoked":true,
                "restored_session_present":true,"restore_backup_sha256":"a".repeat(64),"receipt_expires_at_ms":NOW+600_000})
        }
        fn receipt() -> Receipt {
            serde_json::from_value(receipt_value()).unwrap()
        }
        fn state_value() -> serde_json::Value {
            serde_json::json!({"schema":2,"namespace":"f".repeat(32),"slot":"d".repeat(32),"active":true,"hosted_device":"c".repeat(32),
                "session_expires_at_ms":NOW+3_600_000,"relocation":null,"relocation_session":null,"linking_legacy":false,
                "status":{"endpoint":SERVICE_ENDPOINT,"profile":{"id":"b".repeat(32),"device":"c".repeat(32)},"hosted":true,"signed_in":true,"signing_in":false,"paired":true}})
        }
        fn state_bytes() -> Vec<u8> {
            serde_json::to_vec(&state_value()).unwrap()
        }
        #[test]
        fn valid_scope_selects_only_session_key() {
            validate_receipt(&receipt(), NOW).unwrap();
            let bytes = state_bytes();
            let scope = validate_state(&bytes, &digest(&bytes), &receipt()).unwrap();
            assert_eq!(scope.namespace, "f".repeat(32));
            assert_eq!(scope.key, format!("hosted/session/{}/{}", "d".repeat(32), digest(SERVICE_ENDPOINT.as_bytes())));
            verify_secret(&"e".repeat(64), &receipt()).unwrap();
            assert!(verify_secret(&"f".repeat(64), &receipt()).is_err());
        }
        #[test]
        fn receipt_pin_rejects_tampering_and_unknown_fields() {
            let bytes = serde_json::to_vec(&receipt_value()).unwrap();
            assert!(decode_receipt(&bytes, &digest(&bytes), NOW).is_ok());
            assert!(decode_receipt(&bytes, &"0".repeat(64), NOW).is_err());
            let mut value = receipt_value();
            value["unexpected"] = true.into();
            let bytes = serde_json::to_vec(&value).unwrap();
            assert!(decode_receipt(&bytes, &digest(&bytes), NOW).is_err());
        }
        #[test]
        fn receipt_rejects_wrong_backups_expiry_and_unproven_quarantine() {
            for (key, hostile) in [
                ("restore_backup_sha256", serde_json::json!("f".repeat(64))),
                ("receipt_expires_at_ms", NOW.into()),
                ("receipt_expires_at_ms", (NOW + 901_000).into()),
                ("session_expires_at_ms", (NOW + 1).into()),
                ("backup_session_revoked", true.into()),
                ("backup_device_revoked", true.into()),
                ("backup_profile_revoked", true.into()),
                ("restored_session_revoked", false.into()),
                ("restored_profile_revoked", false.into()),
                ("restored_session_present", false.into()),
                ("tunnel_verified", false.into()),
            ] {
                let mut candidate = receipt_value();
                candidate[key] = hostile;
                let candidate: Receipt = serde_json::from_value(candidate).unwrap();
                assert!(validate_receipt(&candidate, NOW).is_err());
            }
        }
        #[test]
        fn endpoints_must_be_pinned_live_and_numeric_loopback_restore() {
            for endpoint in [
                "http://localhost:49501",
                "https://127.0.0.1:49501",
                "http://192.0.2.1:49501",
                "http://127.0.0.1:80",
                "http://user@127.0.0.1:49501",
                "http://127.0.0.1:49501/path",
            ] {
                let mut receipt = receipt();
                receipt.restore_endpoint = endpoint.into();
                assert!(validate_receipt(&receipt, NOW).is_err());
            }
            let mut receipt = receipt();
            receipt.production_endpoint = "https://example.invalid".into();
            assert!(validate_receipt(&receipt, NOW).is_err());
        }
        #[test]
        fn metadata_rejects_wrong_scope_and_paused_or_relocating_state() {
            for (pointer, hostile) in [
                ("/schema", 1.into()),
                ("/slot", serde_json::json!("a".repeat(32))),
                ("/namespace", "bad".into()),
                ("/active", false.into()),
                ("/status/signed_in", false.into()),
                ("/status/hosted", false.into()),
                ("/status/signing_in", true.into()),
                ("/relocation", serde_json::json!({})),
                ("/relocation_session", serde_json::json!({})),
                ("/linking_legacy", true.into()),
                ("/session_expires_at_ms", NOW.into()),
                ("/status/profile/device", serde_json::json!("a".repeat(32))),
                ("/status/profile/id", serde_json::json!("a".repeat(32))),
                ("/status/endpoint", "https://example.invalid".into()),
            ] {
                let mut state = state_value();
                *state.pointer_mut(pointer).unwrap() = hostile;
                let body = serde_json::to_vec(&state).unwrap();
                assert!(validate_state(&body, &digest(&body), &receipt()).is_err());
            }
            let body = state_bytes();
            assert!(validate_state(&body, &"0".repeat(64), &receipt()).is_err());
        }
        #[test]
        fn rejection_must_be_exact_http_session_fault_without_reflection() {
            assert!(denied(401, br#"{"code":"session","message":"untrusted-secret"}"#, 401, "session").is_ok());
            assert!(denied(403, br#"{"code":"unauthorized","message":"untrusted-secret"}"#, 403, "unauthorized").is_ok());
            for (status, body) in [
                (403, br#"{"code":"session","message":"untrusted-secret"}"#.as_slice()),
                (401, br#"{"code":"unauthorized","message":"untrusted-secret"}"#),
                (200, b"untrusted-secret"),
            ] {
                let error = denied(status, body, 401, "session").unwrap_err();
                assert!(!error.contains("untrusted-secret"));
            }
        }
        #[test]
        fn readonly_database_probe_preserves_bytes_and_rejects_wrong_application() {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("rooms.sqlite3");
            let db = Connection::open(&path).unwrap();
            db.execute_batch("PRAGMA application_id=1263685453; PRAGMA user_version=2; CREATE TABLE state(id INTEGER PRIMARY KEY, body BLOB, digest TEXT);")
                .unwrap();
            let body = state_bytes();
            db.execute("INSERT INTO state VALUES(1,?1,?2)", rusqlite::params![body, digest(&body)]).unwrap();
            db.close().unwrap();
            let before = std::fs::read(&path).unwrap();
            assert!(read_scope(&path, &receipt()).is_ok());
            assert_eq!(std::fs::read(&path).unwrap(), before);
            let db = Connection::open(&path).unwrap();
            db.pragma_update(None, "application_id", 0).unwrap();
            db.close().unwrap();
            assert!(read_scope(&path, &receipt()).is_err());
        }
        #[test]
        fn sql_rejects_oversized_and_nontext_checksums_before_rust_decoding() {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("rooms.sqlite3");
            let db = Connection::open(&path).unwrap();
            db.execute_batch("PRAGMA application_id=1263685453; PRAGMA user_version=2; CREATE TABLE state(id INTEGER PRIMARY KEY, body BLOB, digest TEXT);")
                .unwrap();
            let body = state_bytes();
            db.execute("INSERT INTO state VALUES(1,?1,?2)", rusqlite::params![body, digest(&body)]).unwrap();
            assert!(read_scope(&path, &receipt()).is_ok());
            for checksum in [
                rusqlite::types::Value::Text("a".repeat(1024 * 1024)),
                rusqlite::types::Value::Blob(vec![b'a'; 64]),
                // Exactly64 Unicode characters still exceed the byte bound.
                rusqlite::types::Value::Text("é".repeat(64)),
                rusqlite::types::Value::Null,
            ] {
                db.execute("UPDATE state SET digest=?1 WHERE id=1", [checksum]).unwrap();
                assert!(matches!(read_scope(&path, &receipt()), Err("Cannot read bounded active client metadata")));
            }
        }
    }
}
