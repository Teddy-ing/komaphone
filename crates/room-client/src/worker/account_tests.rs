//! Synthetic HTTP fixtures: no Google login, cloud service or real credentials.
use super::*;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    sync::Mutex,
};
#[derive(Clone, Default)]
struct Vault(Arc<Mutex<BTreeMap<(String, String), String>>>);
impl CredentialStore for Vault {
    fn get(&self, n: &str, k: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(&(n.into(), k.into())).cloned())
    }
    fn set(&self, n: &str, k: &str, v: &str) -> Result<()> {
        self.0.lock().unwrap().insert((n.into(), k.into()), v.into());
        Ok(())
    }
    fn remove(&self, n: &str, k: &str) -> Result<()> {
        self.0.lock().unwrap().remove(&(n.into(), k.into()));
        Ok(())
    }
}
fn revoke_server() -> (String, String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let secret = "a".repeat(64);
    let expected = secret.clone();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
                Err(error) => panic!("revocation fixture did not receive request: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut request = Vec::new();
        let mut chunk = [0; 1024];
        loop {
            let n = stream.read(&mut chunk).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&chunk[..n]);
            assert!(request.len() <= 8192);
            if let Some(split) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..split]);
                let length = headers
                    .lines()
                    .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|value| value.trim().parse::<usize>().ok()))
                    .unwrap();
                if request.len() >= split + 4 + length {
                    assert!(headers.starts_with("POST /v1/auth HTTP/1.1"));
                    assert!(
                        headers.lines().any(|line| line.to_ascii_lowercase() == format!("authorization: bearer {expected}")),
                        "only the staged destination session may authorize this revoke"
                    );
                    let body: serde_json::Value = serde_json::from_slice(&request[split + 4..]).unwrap();
                    assert!(body.get("action").and_then(serde_json::Value::as_str) == Some("revoke"));
                    break;
                }
            }
        }
        let body = br#"{"kind":"done"}"#;
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        stream.write_all(body).unwrap();
    });
    (endpoint, secret, server)
}

fn staged(path: &std::path::Path, endpoint: &str, secret: String) -> Outbox {
    let mut store = Outbox::open(path, Box::<Vault>::default()).unwrap();
    store.configure("http://127.0.0.1:43127", "Original").unwrap();
    let pair = store.next().unwrap().unwrap();
    let profile = store.status().profile.unwrap();
    store.acknowledge(&pair, &RpcResponse::Paired { participant: profile.clone() }).unwrap();
    let options = store.prepare_hosted_relocation(endpoint, "Original").unwrap();
    store
        .stage_hosted_relocation(&crate::hosted_auth::Session {
            profile: profile.id,
            device: profile.device,
            label: profile.display_name,
            token: secret,
            device_credential: options.device_proof,
            expires_at_ms: u64::try_from(chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now()).timestamp_millis()).unwrap() + 60_000,
        })
        .unwrap();
    store
}

#[test]
fn superseded_signout_revokes_staged_destination_before_failed_connect() {
    let (endpoint, secret, server) = revoke_server();
    let temp = tempfile::tempdir().unwrap();
    let mut store = staged(&temp.path().join("room.db"), &endpoint, secret);
    let plan = store.status().relocation.unwrap();
    let (tx, _commands) = mpsc::sync_channel(1);
    let (_events, rx) = mpsc::sync_channel(1);
    let worker = Worker {
        alive: Arc::new(AtomicBool::new(true)),
        tx,
        rx,
        sequence: AtomicU64::new(1),
        login_cancel: Arc::new(AtomicBool::new(false)),
        canceled_through: Arc::new(AtomicU64::new(0)),
        auth_fence: Arc::new(AuthFence::default()),
        priority: Arc::new(Priority::default()),
        completion: Arc::new(Priority::default()),
        signout_requested: Arc::new(AtomicBool::new(false)),
        status: Arc::new(Priority::default()),
    };
    worker.send(ClientCommand::Retry).unwrap();
    worker.send(ClientCommand::SignOut).unwrap();
    worker.send(ClientCommand::Connect { endpoint: "invalid".into(), display_name: "Replacement".into() }).unwrap();
    let replacement = worker.priority.take().unwrap();
    consume_signout(&mut store, &ClientOptions::default(), &worker.signout_requested).unwrap();
    assert!(handle(&mut store, replacement.command).is_err());
    server.join().unwrap();
    assert!(store.session_for_revocation().unwrap().is_none());
    assert!(store.relocation_access().unwrap().is_none());
    assert!(store.complete_hosted_relocation(plan.id).is_err());
    assert!(!store.active());
    assert!(!store.status().signed_in);
    assert!(worker.auth_fence.paused());
}

#[test]
fn pause_after_relocation_commit_revokes_destination_before_worker_activation() {
    let (endpoint, secret, server) = revoke_server();
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("room.db");
    let mut store = staged(&path, &endpoint, secret);
    let plan = store.status().relocation.unwrap();
    let fence = AuthFence::default();
    let ticket = fence.pause();
    // The real SQLite transaction has committed; cancellation now wins the
    // precise boundary before the same production continuation resumes HTTP.
    store.complete_hosted_relocation(plan.id).unwrap();
    assert!(store.active());
    assert!(store.status().signed_in);
    assert!(store.status().relocation.is_none());
    fence.pause();
    assert!(activate_completed_relocation(&mut store, &ClientOptions::default(), &fence, ticket).is_err());
    server.join().unwrap();
    assert!(fence.paused());
    assert!(!fence.allowed(ticket));
    assert!(!store.active());
    assert!(!store.status().signed_in);
    assert!(store.session_for_revocation().unwrap().is_none());
    assert!(store.credential().is_err());
    drop(store);
    let reopened = Outbox::open(path, Box::<Vault>::default()).unwrap();
    assert_eq!(reopened.status().endpoint.as_deref(), Some(endpoint.as_str()));
    assert!(!reopened.active());
    assert!(!reopened.status().signed_in);
}

fn signed_in_store(path: &std::path::Path, endpoint: &str, vault: &Vault) -> Outbox {
    let mut store = Outbox::open(path, Box::new(vault.clone())).unwrap();
    let login = store.prepare_hosted_login(endpoint, "Shutdown fixture", None).unwrap();
    store
        .install_hosted_session(
            endpoint,
            &crate::hosted_auth::Session {
                profile: photocraft_party::ParticipantId::new(&"b".repeat(32)).unwrap(),
                device: login.device,
                label: login.label,
                token: "a".repeat(64),
                device_credential: login.device_proof,
                expires_at_ms: u64::try_from(chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now()).timestamp_millis()).unwrap() + 60_000,
            },
        )
        .unwrap();
    store
}

#[test]
fn shutdown_during_http_preserves_session_and_queue_but_keeps_explicit_disconnect_paused() {
    for disconnect in [false, true] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("shutdown fixture request missing: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            loop {
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
                assert!(request.len() <= 8192);
                if let Some(split) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..split]);
                    let length = headers
                        .lines()
                        .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|n| n.trim().parse::<usize>().ok()))
                        .unwrap();
                    if request.len() >= split + 4 + length {
                        break;
                    }
                }
            }
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            // The stopped fence must reject this response before decoding it.
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").unwrap();
        });
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("room.db");
        let vault = Vault::default();
        let mut store = signed_in_store(&path, &endpoint, &vault);
        store.enqueue(Operation::Create { mode: Mode::Animation { rotations: 1 }, timer_ms: None }).unwrap();
        let queued = serde_json::to_vec(&store.next().unwrap().unwrap()).unwrap();
        let access = crate::world_worker::WorldAccess::from_room(&store).unwrap().unwrap();
        let profile = store.status().profile;
        let slot = store.status().account_slot;
        drop(store);
        let worker = Worker::start_with_credentials(path.clone(), ClientOptions::default(), Box::new(vault.clone())).unwrap();
        let alive = Arc::clone(&worker.alive);
        let old_access = access.with_auth_fence(Arc::clone(&worker.auth_fence), worker.auth_fence.ticket(), false).copy_for_worker();
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        if disconnect {
            worker.send(ClientCommand::Disconnect).unwrap();
        }
        drop(worker);
        assert!(matches!(old_access.verify_identity(None), Err(Error::Rejected { code, .. }) if code == "account_changed"));
        release_tx.send(()).unwrap();
        server.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!alive.load(Ordering::Acquire));
        let mut reopened = Outbox::open(path, Box::new(vault)).unwrap();
        assert_eq!(reopened.active(), !disconnect);
        assert_eq!(reopened.status().polling, !disconnect);
        assert!(reopened.status().signed_in);
        assert_eq!(reopened.status().profile, profile);
        assert_eq!(reopened.status().account_slot, slot);
        assert_eq!(reopened.credential().unwrap(), "a".repeat(64));
        assert!(reopened.status().error.is_none(), "runtime shutdown is not a durable account error");
        assert_eq!(reopened.status().pending, 1);
        if disconnect {
            assert!(crate::world_worker::WorldAccess::from_room(&reopened).unwrap().is_none());
            assert!(network_step(&mut reopened, &Transport::new(None).unwrap(), &mut Instant::now(), &AuthFence::default()).unwrap().is_none());
            reopened.retry().unwrap();
        }
        assert_eq!(serde_json::to_vec(&reopened.next().unwrap().unwrap()).unwrap(), queued);
        assert!(crate::world_worker::WorldAccess::from_room(&reopened).unwrap().is_some());
    }
}

#[test]
fn shutdown_continuation_consumes_queued_signout_and_revokes_before_reopen() {
    let (endpoint, _secret, server) = revoke_server();
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("room.db");
    let vault = Vault::default();
    let mut store = signed_in_store(&path, &endpoint, &vault);
    let fence = AuthFence::default();
    // This same continuation handles a disconnected command receiver: a
    // coalesced priority sign-out may have arrived while recv_timeout waited.
    let requested = AtomicBool::new(true);
    fence.pause();
    fence.stop();
    finish_shutdown(&mut store, &ClientOptions::default(), &fence, &requested);
    server.join().unwrap();
    assert!(!requested.load(Ordering::Acquire));
    drop(store);
    let reopened = Outbox::open(path, Box::new(vault)).unwrap();
    assert!(!reopened.active());
    assert!(!reopened.status().signed_in);
    assert!(reopened.credential().is_err());
    assert!(crate::world_worker::WorldAccess::from_room(&reopened).unwrap().is_none());
}

#[test]
fn disconnect_ack_preserves_newest_local_polling_intent_and_remaining_artwork() {
    for (retry, pause_again) in [(false, false), (true, false), (true, true)] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("room.db");
        let vault = Vault::default();
        let mut store = signed_in_store(&path, "http://127.0.0.1:43127", &vault);
        let room = RoomId::new(&"c".repeat(32)).unwrap();
        store.enqueue(Operation::Disconnect { room }).unwrap();
        let disconnect = store.next().unwrap().unwrap();
        let art = queued_artwork(room);
        let art_id = store.enqueue(art.clone()).unwrap();
        store.stop_polling().unwrap();
        drop(store);
        let mut store = Outbox::open(&path, Box::new(vault.clone())).unwrap();
        if retry {
            handle(&mut store, ClientCommand::Retry).unwrap();
        }
        if pause_again {
            store.stop_polling().unwrap();
        }
        store.acknowledge(&disconnect, &RpcResponse::Disconnected).unwrap();
        drop(store);
        let store = Outbox::open(&path, Box::new(vault)).unwrap();
        let active = retry && !pause_again;
        assert_eq!(store.active(), active);
        assert_eq!(store.status().polling, active);
        assert!(store.status().signed_in);
        assert_eq!(store.status().pending, 1);
        let pending = store.next().unwrap().unwrap();
        assert_eq!(pending.request_id, art_id);
        assert!(pending.operation == art);
        assert_eq!(crate::world_worker::WorldAccess::from_room(&store).unwrap().is_some(), active);
    }
}

fn queued_artwork(room: RoomId) -> Operation {
    let bytes = vec![1, 3, 5, 7];
    Operation::Upload { room, hash: AssetHash::new(&format!("{:x}", Sha256::digest(&bytes))).unwrap(), total: 4, offset: 0, bytes, finish: true }
}

fn read_fixture_rpc(stream: &mut std::net::TcpStream) -> RpcRequest {
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut bytes = Vec::new();
    let mut chunk = [0; 1024];
    loop {
        let count = stream.read(&mut chunk).unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&chunk[..count]);
        assert!(bytes.len() <= 8192);
        if let Some(split) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..split]);
            let length = headers
                .lines()
                .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|value| value.trim().parse::<usize>().ok()))
                .unwrap();
            if bytes.len() >= split + 4 + length {
                assert!(headers.starts_with("POST /v1/rpc HTTP/1.1"));
                return serde_json::from_slice(&bytes[split + 4..]).unwrap();
            }
        }
    }
}

#[test]
fn one_retry_drains_saved_disconnect_and_artwork_resumes_polls_unless_new_pause_wins() {
    for pause_during_ack in [false, true] {
        reconnect_over_http(pause_during_ack);
    }
}

fn reconnect_over_http(pause_during_ack: bool) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("room.db");
    let vault = Vault::default();
    let mut store = signed_in_store(&path, &endpoint, &vault);
    let profile = store.status().profile.unwrap();
    let slot = store.status().account_slot;
    let room = photocraft_party::room::Room::new(RoomId::new(&"c".repeat(32)).unwrap(), profile.clone(), Mode::Animation { rotations: 1 }, None).unwrap();
    let view = room.view(profile.id).unwrap();
    store.update_poll(view.clone()).unwrap();
    handle(&mut store, ClientCommand::Disconnect).unwrap();
    let disconnect = store.next().unwrap().unwrap();
    let art = queued_artwork(room.id());
    let art_id = store.enqueue(art.clone()).unwrap();
    assert!(!store.active());
    drop(store);
    let (received_tx, received_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = Arc::clone(&stop);
    let server_view = view.clone();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !server_stop.load(Ordering::Acquire) && Instant::now() < deadline {
            let mut stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("reconnect fixture accept failed: {error}"),
            };
            let request = read_fixture_rpc(&mut stream);
            received_tx.send(request.clone()).unwrap();
            let response = match request.operation {
                Operation::Poll { .. } => RpcResponse::Room { view: Box::new(server_view.clone()), outcome: None },
                Operation::Disconnect { .. } => {
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    RpcResponse::Disconnected
                }
                Operation::Upload { hash, total, .. } => {
                    RpcResponse::Uploaded { asset: Some(photocraft_party::AssetRef { hash, bytes: total }), next_offset: total }
                }
                _ => panic!("unexpected reconnect fixture operation"),
            };
            let body = serde_json::to_vec(&response).unwrap();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            stream.write_all(&body).unwrap();
        }
    });
    let worker = Worker::start_with_credentials(path.clone(), ClientOptions::default(), Box::new(vault.clone())).unwrap();
    worker.send(ClientCommand::Retry).unwrap();
    assert!(matches!(received_rx.recv_timeout(Duration::from_secs(5)).unwrap().operation, Operation::Poll { .. }));
    assert!(received_rx.recv_timeout(Duration::from_secs(5)).unwrap() == disconnect);
    if pause_during_ack {
        worker.send(ClientCommand::Disconnect).unwrap();
    }
    release_tx.send(()).unwrap();
    if pause_during_ack {
        wait_worker_status(&worker, |status| !status.polling && status.pending == 3);
        assert!(received_rx.recv_timeout(Duration::from_millis(350)).is_err(), "a newer explicit pause fences the old reply and all following HTTP");
    } else {
        let uploaded = received_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(uploaded.request_id, art_id);
        assert!(uploaded.operation == art);
        assert!(
            matches!(received_rx.recv_timeout(Duration::from_secs(5)).unwrap().operation, Operation::Poll { .. }),
            "polling continues after the old disconnect acknowledgement"
        );
        wait_worker_status(&worker, |status| status.polling && status.connected && status.pending == 0);
    }
    worker.send(ClientCommand::WorldAccess).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut access_received = false;
    while Instant::now() < deadline {
        if let Ok(ClientEvent::WorldAccess { access, .. }) = worker.poll() {
            assert_eq!(access.is_some(), !pause_during_ack);
            access_received = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(access_received);
    let alive = Arc::clone(&worker.alive);
    drop(worker);
    while alive.load(Ordering::Acquire) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!alive.load(Ordering::Acquire));
    stop.store(true, Ordering::Release);
    server.join().unwrap();
    let reopened = Outbox::open(path, Box::new(vault)).unwrap();
    assert_eq!(reopened.active(), !pause_during_ack);
    assert_eq!(reopened.status().profile, Some(profile));
    assert_eq!(reopened.status().account_slot, slot);
    assert_eq!(reopened.status().room, Some(view));
    assert!(reopened.status().signed_in);
    assert_eq!(reopened.credential().unwrap(), "a".repeat(64));
    assert_eq!(reopened.status().pending, if pause_during_ack { 3 } else { 0 });
    if pause_during_ack {
        assert!(reopened.next().unwrap().unwrap() == disconnect);
    }
}

fn wait_worker_status(worker: &Worker, accepts: impl Fn(&ClientStatus) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(ClientEvent::Status { status, .. }) = worker.poll()
            && accepts(&status)
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("worker did not publish expected reconnect state");
}
