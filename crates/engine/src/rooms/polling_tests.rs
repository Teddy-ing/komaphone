//! Synthetic native handoff regression; no Google, platform vault or public service.
use super::*;
use photocraft_room_client::{ClientCommand, CredentialStore, Outbox, Worker, hosted_auth};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
struct Vault(Arc<Mutex<BTreeMap<(String, String), String>>>);
impl CredentialStore for Vault {
    fn get(&self, namespace: &str, key: &str) -> photocraft_room_client::Result<Option<String>> {
        Ok(self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&(namespace.into(), key.into())).cloned())
    }
    fn set(&self, namespace: &str, key: &str, value: &str) -> photocraft_room_client::Result<()> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert((namespace.into(), key.into()), value.into());
        Ok(())
    }
    fn remove(&self, namespace: &str, key: &str) -> photocraft_room_client::Result<()> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&(namespace.into(), key.into()));
        Ok(())
    }
}

fn until(session: &mut Session, condition: impl Fn(&Session) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        session.poll_rooms();
        session.poll_worlds();
        if condition(session) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(condition(session), "native room-to-canvas handoff did not reach the expected state");
}

#[test]
fn explicit_retry_without_room_or_outbox_restores_world_access_and_new_pause_still_wins() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let stopping = Arc::new(AtomicBool::new(false));
    let stop = Arc::clone(&stopping);
    let lists = Arc::new(AtomicUsize::new(0));
    let received_lists = Arc::clone(&lists);
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !stop.load(Ordering::Acquire) && Instant::now() < deadline {
            let mut stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("synthetic canvas listener failed: {error}"),
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            let body = loop {
                let n = stream.read(&mut buffer).unwrap();
                assert!(n > 0);
                request.extend_from_slice(&buffer[..n]);
                assert!(request.len() <= 8192);
                if let Some(split) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..split]);
                    let size = headers
                        .lines()
                        .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|n| n.trim().parse::<usize>().ok()))
                        .unwrap();
                    if request.len() >= split + 4 + size {
                        assert!(headers.starts_with("POST /v1/canvas HTTP/1.1"), "no room RPC is needed to restore canvas access");
                        break serde_json::from_slice::<photocraft_scene::world_protocol::Request>(&request[split + 4..]).unwrap();
                    }
                }
            };
            assert!(matches!(body.action, photocraft_scene::world_protocol::Action::List { .. }));
            received_lists.fetch_add(1, Ordering::Release);
            let response = br#"{"kind":"canvases","items":[],"next":null}"#;
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()).unwrap();
            stream.write_all(response).unwrap();
        }
    });
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("rooms.sqlite3");
    let vault = Vault::default();
    let mut store = Outbox::open(&database, Box::new(vault.clone())).unwrap();
    let login = store.prepare_hosted_login(&endpoint, "Synthetic paused account", None).unwrap();
    let now: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
    store
        .install_hosted_session(
            &endpoint,
            &hosted_auth::Session {
                profile: photocraft_party::ParticipantId::new(&"b".repeat(32)).unwrap(),
                device: login.device,
                label: login.label,
                token: "a".repeat(64),
                device_credential: login.device_proof,
                expires_at_ms: u64::try_from(now.timestamp_millis()).unwrap() + 60_000,
            },
        )
        .unwrap();
    store.stop_polling().unwrap();
    let saved = store.status();
    assert!(saved.room.is_none() && saved.pending == 0 && saved.paired && saved.signed_in && !saved.polling && !saved.connected);
    drop(store);
    let mut session = Session::new();
    session.rooms = Rooms {
        status: saved.clone(),
        configured: true,
        worker: Some(Worker::start_with_credentials(database, ClientOptions::default(), Box::new(vault)).unwrap()),
        ..Rooms::default()
    };
    session.enable_worlds(root.path().join("world.sqlite3"), ClientOptions::default()).unwrap();
    // Acknowledge initial paused handoff before the only changed status field:
    // Retry has no room/pending work with which to set `connected` later.
    session.room_request(ClientCommand::WorldAccess).unwrap();
    until(&mut session, |s| !s.rooms.busy());
    assert!(session.worlds.account.is_none());
    assert_eq!(lists.load(Ordering::Acquire), 0);
    session.room_request(ClientCommand::Retry).unwrap();
    until(&mut session, |s| s.rooms.status.polling && s.worlds.account.is_some() && lists.load(Ordering::Acquire) > 0);
    let account = session.worlds.account.as_ref().unwrap();
    let profile = saved.profile.as_ref().unwrap();
    assert_eq!(account.profile.as_str(), profile.id.as_str());
    assert_eq!(account.device.as_str(), profile.device.as_str());
    assert_eq!(account.endpoint, endpoint);
    assert_eq!(session.rooms.status.profile, saved.profile);
    assert!(!session.rooms.status.connected && session.rooms.status.room.is_none() && session.rooms.status.pending == 0);
    // A newer explicit pause still immediately quarantines the restored view.
    session.room_request(ClientCommand::Disconnect).unwrap();
    assert!(session.worlds.account.is_none());
    until(&mut session, |s| !s.rooms.busy() && !s.rooms.status.polling);
    session.room_request(ClientCommand::Retry).unwrap();
    until(&mut session, |s| !s.rooms.busy() && s.rooms.status.error.is_some());
    assert!(!session.rooms.status.polling && session.worlds.account.is_none(), "a current runtime account fence cannot be bypassed by Retry");
    stopping.store(true, Ordering::Release);
    server.join().unwrap();
}
