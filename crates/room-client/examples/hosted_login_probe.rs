//! Interactive, isolated Windows proof of actual Google sign-in and service auth.
//! No simulated identity provider; launch only when the human is ready to sign in.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

const HELP: &str = "Komaphone hosted login probe\n\
Usage: hosted_login_probe --endpoint ORIGIN --data-dir ABSOLUTE_FRESH_DIRECTORY --google-client-file ABSOLUTE_PRIVATE_JSON --admission-file ABSOLUTE_PRIVATE_JSON [--second-admission-file ABSOLUTE_PRIVATE_JSON]\n\
The Google client file is the downloaded Desktop client JSON matching Komaphone's pinned client ID.\n\
The admission file is bounded JSON from photocraft-sync --create-admission.\n\
This opens Google in your normal browser, tests authenticated canvas identity and\n\
creation of an empty Animation room and canvas, then revokes the session and signs out.\n\
The optional second invitation tests a different Google account and private-data isolation.\n\
Only synthetic isolated service/client data should be used. It does not test gameplay.\n\
Credentials are stored through Windows Credential Manager; no tokens or login URLs are printed.";

#[cfg(windows)]
fn main() {
    if let Err(error) = native::run() {
        eprintln!("Komaphone hosted login probe failed: {error}");
        std::process::exit(1);
    }
}
#[cfg(not(windows))]
fn main() {
    println!("{HELP}\nActual sign-in requires Windows Credential Manager.");
}

#[cfg(windows)]
mod native {
    use photocraft_party::{
        Mode, RequestId,
        protocol::{Operation, PROTOCOL_VERSION, RpcRequest, RpcResponse},
    };
    use photocraft_room_client::{Error, Outbox, Result, Transport, WindowsCredentials, hosted_auth, normalize_endpoint};
    use photocraft_scene::world_protocol as world;
    use serde::Deserialize;
    use std::{
        io::Read,
        path::{Path, PathBuf},
        sync::atomic::AtomicBool,
    };

    struct Args {
        endpoint: String,
        data_dir: PathBuf,
        admission_file: PathBuf,
        second_admission_file: Option<PathBuf>,
        google_client_file: PathBuf,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Admission {
        admission: String,
        expires_at_ms: u64,
    }

    pub(super) fn run() -> Result<()> {
        let Some(args) = arguments()? else {
            return Ok(());
        };
        let admission = read_admission(&args.admission_file)?;
        let second_admission = args.second_admission_file.as_deref().map(read_admission).transpose()?;
        let client = hosted_auth::Client::with_installed_client_json(None, &read_client_configuration(&args.google_client_file)?)?;
        let transport = Transport::new(None)?;
        client.configuration(&args.endpoint)?;
        ensure_fresh_directory(&args.data_dir)?;
        let mut outbox = Outbox::open(args.data_dir.join("room.sqlite3"), Box::new(WindowsCredentials))?;
        let first = probe_account(&mut outbox, &client, &transport, &args.endpoint, admission, None)?;
        let first_slot = outbox.status().account_slot.ok_or_else(|| invalid("Saved account slot is missing"))?;
        let second = if let Some(admission) = second_admission {
            outbox.select_account(None)?;
            println!("Select the second alternate Google test account for the isolation check.");
            let second = probe_account(&mut outbox, &client, &transport, &args.endpoint, admission, Some(&first))?;
            outbox.select_account(Some(&first_slot))?;
            if outbox.status().profile.as_ref().map(|p| p.id) != Some(first.session.profile) || outbox.credential().is_ok() {
                return Err(invalid("Returning to the first account did not preserve its identity and signed-out boundary"));
            }
            Some(second)
        } else {
            None
        };
        println!(
            "{}",
            serde_json::json!({
                "actual_google_sign_in":true,"protected_credentials":true,"canvas_identity":true,
                "party_create":true,"canvas_create":true,"session_revoked":true,"local_signed_out":true,
                "profile":first.session.profile,"device":first.session.device,"room":first.room,"canvas":first.canvas,
                "second_account_isolation":second.is_some(),"second_profile":second.as_ref().map(|s| s.session.profile),
                "gameplay_tested":false,"hosted_internet_acceptance":false
            })
        );
        Ok(())
    }
    struct CheckedAccount {
        session: hosted_auth::Session,
        room: photocraft_party::RoomId,
        canvas: photocraft_scene::CanvasId,
    }
    fn probe_account(
        outbox: &mut Outbox,
        client: &hosted_auth::Client,
        transport: &Transport,
        endpoint: &str,
        admission: String,
        previous: Option<&CheckedAccount>,
    ) -> Result<CheckedAccount> {
        let login = outbox.prepare_hosted_login(endpoint, "Hosted login probe", Some(admission))?;
        println!("Awaiting Google sign-in in your normal browser. No login URL or credentials will be printed.");
        let session = match client.sign_in(login, &AtomicBool::new(false), open_browser) {
            Ok(session) => session,
            Err(error) => {
                let _ = outbox.cancel_hosted_login();
                return Err(error);
            }
        };
        if let Err(error) = outbox.install_hosted_session(endpoint, &session) {
            let _ = client.revoke(endpoint, &session.token);
            let _ = outbox.sign_out_local();
            return Err(error);
        }
        // Read back the protected credential for requests, proving durable install.
        let checks = check_authenticated(outbox, transport, endpoint, &session).and_then(|created| {
            if let Some(previous) = previous {
                check_isolation(outbox, transport, endpoint, &session, previous)?;
            }
            Ok(created)
        });
        let revoked = client.revoke(endpoint, &session.token);
        let signed_out = outbox.sign_out_local();
        let (room, canvas) = checks?;
        revoked?;
        signed_out?;
        let rejected = transport.call_world(endpoint, &session.token, &world::Request { version: world::VERSION, action: world::Action::Identity {} });
        if !matches!(rejected, Err(Error::Rejected { ref code, .. }) if code == "session") {
            return Err(invalid("Revoked session was not rejected with the expected sign-in-required response"));
        }
        if outbox.status().signed_in || outbox.credential().is_ok() {
            return Err(invalid("Local sign-out did not disable credential access"));
        }
        Ok(CheckedAccount { session, room, canvas })
    }

    fn check_authenticated(
        outbox: &Outbox,
        transport: &Transport,
        endpoint: &str,
        session: &hosted_auth::Session,
    ) -> Result<(photocraft_party::RoomId, photocraft_scene::CanvasId)> {
        let credential = outbox.credential()?;
        let identity = transport.call_world(endpoint, &credential, &world::Request { version: world::VERSION, action: world::Action::Identity {} })?;
        if !matches!(identity, world::Response::Identity { profile, device } if profile.as_str() == session.profile.as_str() && device.as_str() == session.device.as_str())
        {
            return Err(invalid("Canvas identity does not match the signed-in profile and device"));
        }
        let id = random_id()?;
        let canvas = photocraft_scene::CanvasId::new(&id).map_err(|_| invalid("Cannot allocate a probe canvas ID"))?;
        let created = transport.call_world(
            endpoint,
            &credential,
            &world::Request { version: world::VERSION, action: world::Action::Create { canvas, name: "Isolated login probe".into() } },
        )?;
        if !matches!(created, world::Response::Canvas { info } if info.canvas == canvas && info.owner.as_str() == session.profile.as_str()) {
            return Err(invalid("The private canvas was not owned by this account"));
        }
        let request_id = RequestId::new(&id).map_err(|_| invalid("Cannot allocate a valid probe request ID"))?;
        let response = transport.call(
            endpoint,
            &credential,
            &RpcRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                operation: Operation::Create { mode: Mode::Animation { rotations: 1 }, timer_ms: None },
            },
        )?;
        match response {
            RpcResponse::Room { view, .. }
                if view.members.iter().any(|member| member.participant.id == session.profile && member.participant.device == session.device) =>
            {
                Ok((view.id, canvas))
            }
            _ => Err(invalid("The authenticated party creation did not return this participant")),
        }
    }
    fn check_isolation(outbox: &Outbox, transport: &Transport, endpoint: &str, session: &hosted_auth::Session, previous: &CheckedAccount) -> Result<()> {
        if session.profile == previous.session.profile || session.device == previous.session.device {
            return Err(invalid("The second login must use a different alternate Google account and local device slot"));
        }
        let credential = outbox.credential()?;
        let room = transport.call(
            endpoint,
            &credential,
            &RpcRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: RequestId::new(&random_id()?).map_err(|_| invalid("Probe request ID"))?,
                operation: Operation::Poll { room: previous.room, after_revision: 0 },
            },
        );
        let canvas =
            transport.call_world(endpoint, &credential, &world::Request { version: world::VERSION, action: world::Action::Info { canvas: previous.canvas } });
        if !matches!(room, Err(Error::Rejected { ref code, .. }) if code == "unauthorized")
            || !matches!(canvas, Err(Error::Rejected { ref code, .. }) if code == "unauthorized")
        {
            return Err(invalid("The second account was not denied access to the first account's private room and canvas"));
        }
        Ok(())
    }
    fn random_id() -> Result<String> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|_| invalid("Cannot allocate a secure probe ID"))?;
        Ok(bytes.iter().map(|value| format!("{value:02x}")).collect())
    }
    fn arguments() -> Result<Option<Args>> {
        let mut args = std::env::args_os().skip(1);
        let mut endpoint = None;
        let mut data_dir = None;
        let mut admission_file = None;
        let mut second_admission_file = None;
        let mut google_client_file = None;
        while let Some(flag) = args.next() {
            if flag == "--help" || flag == "-h" {
                println!("{}", super::HELP);
                return Ok(None);
            }
            let value = args.next().ok_or_else(|| invalid("Every probe option requires a value; use --help"))?;
            if flag == "--endpoint" && endpoint.is_none() {
                endpoint = Some(normalize_endpoint(value.to_str().ok_or_else(|| invalid("Endpoint must be valid Unicode"))?)?);
            } else if flag == "--data-dir" && data_dir.is_none() {
                data_dir = Some(PathBuf::from(value));
            } else if flag == "--admission-file" && admission_file.is_none() {
                admission_file = Some(PathBuf::from(value));
            } else if flag == "--second-admission-file" && second_admission_file.is_none() {
                second_admission_file = Some(PathBuf::from(value));
            } else if flag == "--google-client-file" && google_client_file.is_none() {
                google_client_file = Some(PathBuf::from(value));
            } else {
                return Err(invalid("Unknown or duplicate probe option; use --help"));
            }
        }
        Ok(Some(Args {
            endpoint: endpoint.ok_or_else(|| invalid("--endpoint is required"))?,
            data_dir: data_dir.ok_or_else(|| invalid("--data-dir is required"))?,
            admission_file: admission_file.ok_or_else(|| invalid("--admission-file is required"))?,
            second_admission_file,
            google_client_file: google_client_file.ok_or_else(|| invalid("--google-client-file is required"))?,
        }))
    }
    fn read_client_configuration(path: &Path) -> Result<Vec<u8>> {
        if !path.is_absolute() {
            return Err(invalid("Google Desktop client JSON must be an absolute private file path"));
        }
        let file = std::fs::File::open(path).map_err(|_| invalid("Cannot open the downloaded Google Desktop client JSON"))?;
        if !file.metadata().map_err(|_| invalid("Cannot inspect the Google Desktop client file"))?.is_file() {
            return Err(invalid("Google Desktop client input must be a file"));
        }
        let mut bytes = Vec::new();
        file.take(32769).read_to_end(&mut bytes).map_err(|_| invalid("Cannot read the Google Desktop client file"))?;
        if bytes.len() > 32768 {
            return Err(invalid("Google Desktop client file exceeds its 32 KiB bound"));
        }
        Ok(bytes)
    }
    fn ensure_fresh_directory(path: &Path) -> Result<()> {
        if !path.is_absolute() {
            return Err(invalid("Probe data directory must be absolute"));
        }
        if path.try_exists().map_err(|_| invalid("Cannot inspect probe data directory"))?
            && std::fs::read_dir(path).map_err(|_| invalid("Cannot read probe data directory"))?.next().is_some()
        {
            return Err(invalid("Probe data directory must be fresh and empty; existing work is preserved"));
        }
        std::fs::create_dir_all(path).map_err(|_| invalid("Cannot create isolated probe data directory"))
    }
    fn read_admission(path: &Path) -> Result<String> {
        if !path.is_absolute() {
            return Err(invalid("Admission file must be an absolute path to private administrator output"));
        }
        let file = std::fs::File::open(path).map_err(|_| invalid("Cannot open the private admission file"))?;
        if !file.metadata().map_err(|_| invalid("Cannot inspect the private admission file"))?.is_file() {
            return Err(invalid("Admission input must be a file"));
        }
        let mut bytes = Vec::new();
        file.take(4097).read_to_end(&mut bytes).map_err(|_| invalid("Cannot read the private admission file"))?;
        if bytes.len() > 4096 {
            return Err(invalid("Admission file exceeds the 4 KiB bound"));
        }
        let admission: Admission = serde_json::from_slice(&bytes).map_err(|_| invalid("Admission file must contain the administrator's JSON output"))?;
        photocraft_party::protocol::validate_secret(&admission.admission).map_err(|_| invalid("Admission capability is invalid"))?;
        let instant: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
        let now = u64::try_from(instant.timestamp_millis()).map_err(|_| invalid("Correct the system clock before sign-in"))?;
        if admission.expires_at_ms <= now {
            return Err(invalid("Admission invitation has expired; create another"));
        }
        Ok(admission.admission)
    }
    fn open_browser(url: &str) -> Result<()> {
        let parsed = reqwest::Url::parse(url).map_err(|_| invalid("Cannot validate the Google browser URL"))?;
        if parsed.scheme() != "https"
            || parsed.host_str() != Some("accounts.google.com")
            || parsed.path() != "/o/oauth2/v2/auth"
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
        {
            return Err(invalid("Refusing an unexpected sign-in browser destination"));
        }
        // Use the Windows shell association directly; spawning Explorer can
        // succeed without handing the URL to the registered browser.
        open::that_detached(url).map_err(|_| invalid("Windows could not open the normal browser; check the default HTTPS application and retry sign-in"))
    }
    fn invalid(message: &str) -> Error {
        Error::Invalid(message.into())
    }
}
