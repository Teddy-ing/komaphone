#![cfg(not(target_arch = "wasm32"))]
use photocraft_engine::{Session, worlds::Account};
use photocraft_party::protocol::RpcResponse;
use photocraft_room_client::{CredentialStore, Outbox, Result, world_worker::WorldAccess};
use photocraft_scene::{
    OwnerId,
    shared::DeviceId,
    world_protocol::{self as wire, Action, Response},
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
struct Credentials(Arc<Mutex<BTreeMap<(String, String), String>>>);
impl CredentialStore for Credentials {
    fn get(&self, n: &str, k: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(&(n.into(), k.into())).cloned())
    }
    fn set(&self, n: &str, k: &str, s: &str) -> Result<()> {
        self.0.lock().unwrap().insert((n.into(), k.into()), s.into());
        Ok(())
    }
    fn remove(&self, n: &str, k: &str) -> Result<()> {
        self.0.lock().unwrap().remove(&(n.into(), k.into()));
        Ok(())
    }
}
struct GateCredentials {
    inner: Credentials,
    entered: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
}
impl CredentialStore for GateCredentials {
    fn get(&self, n: &str, k: &str) -> Result<Option<String>> {
        self.inner.get(n, k)
    }
    fn set(&self, n: &str, k: &str, s: &str) -> Result<()> {
        self.inner.set(n, k, s)?;
        if n == "Komaphone.canvas-installation.v1" {
            self.entered.store(true, Ordering::Release);
            let start = Instant::now();
            while !self.release.load(Ordering::Acquire) {
                assert!(start.elapsed() < Duration::from_secs(5));
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        Ok(())
    }
    fn remove(&self, n: &str, k: &str) -> Result<()> {
        self.inner.remove(n, k)
    }
}
fn read_request(socket: &mut TcpStream) -> wire::Request {
    socket.set_nonblocking(false).unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let n = socket.read(&mut buffer).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buffer[..n]);
        assert!(bytes.len() < 16384);
        if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(&bytes[..end]);
            assert!(header.lines().any(|line| line.to_ascii_lowercase().starts_with("authorization: bearer ")));
            let length = header
                .lines()
                .find_map(|line| {
                    line.split_once(':').filter(|(key, _)| key.eq_ignore_ascii_case("content-length")).map(|(_, value)| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return wire::Request::decode(&bytes[end + 4..end + 4 + length]).unwrap();
            }
        }
    }
}
struct Server {
    endpoint: String,
    identity: Arc<Mutex<Option<(OwnerId, DeviceId)>>>,
    hold: Arc<AtomicBool>,
    entered: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let identity = Arc::new(Mutex::new(None));
        let hold = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let shared = (identity.clone(), hold.clone(), entered.clone(), stop.clone());
        let join = std::thread::spawn(move || {
            while !shared.3.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                assert!(matches!(read_request(&mut stream).action, Action::Identity {}));
                shared.2.store(true, Ordering::Release);
                while shared.1.load(Ordering::Acquire) && !shared.3.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                let (profile, device) = shared.0.lock().unwrap().unwrap();
                let bytes = serde_json::to_vec(&Response::Identity { profile, device }).unwrap();
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len());
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&bytes);
            }
        });
        Self { endpoint, identity, hold, entered, stop, join: Some(join) }
    }
    fn accept(&self, access: &WorldAccess) {
        *self.identity.lock().unwrap() = Some((access.scope().profile, access.scope().device));
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.hold.store(false, Ordering::Release);
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }
}
fn access(path: &Path, endpoint: &str, credentials: Credentials) -> WorldAccess {
    let mut room = Outbox::open(path, Box::new(credentials)).unwrap();
    room.configure(endpoint, "Link test").unwrap();
    let pair = room.next().unwrap().unwrap();
    let person = room.status().profile.unwrap();
    room.acknowledge(&pair, &RpcResponse::Paired { participant: person }).unwrap();
    WorldAccess::from_room(&room).unwrap().unwrap()
}
fn make_session(path: &Path, credentials: Credentials) -> Session {
    let mut session = Session::new();
    session.enable_canvases_with_link_credentials(path, Default::default(), Box::new(credentials)).unwrap();
    session.flush_canvases(Duration::from_secs(5)).unwrap();
    session
}
fn connect(session: &mut Session, access: &WorldAccess) {
    let scope = access.scope();
    session.worlds.account = Some(Account { endpoint: scope.endpoint.clone(), profile: scope.profile, device: scope.device });
    session.canvas_link_access(Some(access));
}
fn wait_link(session: &mut Session) {
    let start = Instant::now();
    while session.canvases.link.busy {
        session.poll_canvases();
        assert!(start.elapsed() < Duration::from_secs(8));
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn draw(session: &mut Session) {
    let active = session.canvases.active.as_ref().unwrap();
    session
        .execute(
            "komaphone.canvas.stroke",
            json!({"canvas":active.id,"anchor":active.camera.region,"points":[[0.,0.],[10.,10.]],"width":2.,"color":{"rgba":[0.,0.,0.,1.]}}),
        )
        .unwrap();
    session.flush_canvases(Duration::from_secs(1)).unwrap();
}
fn art(path: &Path) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    let db = rusqlite::Connection::open(path).unwrap();
    ["identity", "canvases", "strokes", "cells"]
        .into_iter()
        .map(|table| {
            let mut statement = db.prepare(&format!("SELECT * FROM {table} ORDER BY rowid")).unwrap();
            let count = statement.column_count();
            statement.query_map([], |row| (0..count).map(|index| row.get(index)).collect()).unwrap().collect::<std::result::Result<Vec<_>, _>>().unwrap()
        })
        .collect()
}

#[test]
fn real_identity_readback_rejects_wrong_profile_and_mixed_device_claims() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    let access = access(&root.path().join("room.db"), &server.endpoint, Credentials::default());
    server.accept(&access);
    assert!(access.verify_identity(None).unwrap().fresh());
    *server.identity.lock().unwrap() = Some((OwnerId::new(&format!("{:032x}", 999)).unwrap(), access.scope().device));
    assert!(access.verify_identity(None).is_err());
    *server.identity.lock().unwrap() = Some((access.scope().profile, DeviceId::new(&format!("{:032x}", 999)).unwrap()));
    assert!(access.verify_identity(None).is_err());
}

#[test]
fn native_link_rollback_restart_and_wrong_account_preserve_all_existing_art_ids() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    let credentials = Credentials::default();
    let db = root.path().join("canvas.db");
    let access_a = access(&root.path().join("room-a.db"), &server.endpoint, credentials.clone());
    server.accept(&access_a);
    let mut session = make_session(&db, credentials.clone());
    session.execute("komaphone.canvas.create", json!({"name":"Existing art"})).unwrap();
    session.flush_canvases(Duration::from_secs(5)).unwrap();
    draw(&mut session);
    let original = art(&db);
    let owner = session.canvases.owner;
    connect(&mut session, &access_a);
    assert_eq!(session.execute("komaphone.canvas.link", json!({})).unwrap()["pending"], true);
    wait_link(&mut session);
    assert!(session.canvases.link.error.is_none(), "{:?}", session.canvases.link.error);
    assert_eq!(session.canvases.link.binding.as_ref().unwrap().profile, access_a.scope().profile);
    assert_ne!(owner, Some(access_a.scope().profile));
    assert!(session.canvases.link.backup.is_some());
    assert_eq!(art(&db), original);
    assert!(session.execute("komaphone.canvas.link.status", json!({})).unwrap()["binding"]["active"].as_bool().unwrap());
    session.execute("komaphone.canvas.link.rollback", json!({})).unwrap();
    wait_link(&mut session);
    assert!(!session.canvases.link.binding.as_ref().unwrap().active);
    assert_eq!(art(&db), original);
    drop(session);
    let mut reopened = make_session(&db, credentials.clone());
    assert!(!reopened.canvases.link.binding.as_ref().unwrap().active);
    connect(&mut reopened, &access_a);
    reopened.canvas_link_identity(false).unwrap();
    wait_link(&mut reopened);
    assert!(reopened.canvases.link.binding.as_ref().unwrap().active);
    let access_b = access(&root.path().join("room-b.db"), &server.endpoint, credentials);
    server.accept(&access_b);
    connect(&mut reopened, &access_b);
    reopened.canvas_link_identity(false).unwrap();
    wait_link(&mut reopened);
    assert!(reopened.canvases.link.error.is_some());
    assert!(reopened.canvases.error.is_none());
    assert_eq!(reopened.canvases.link.binding.as_ref().unwrap().profile, access_a.scope().profile);
    assert_eq!(art(&db), original);
    draw(&mut reopened);
    assert_eq!(reopened.canvases.active.as_ref().unwrap().stroke_count, 2);
    assert!(reopened.execute("komaphone.canvas.link", json!({"profile":access_b.scope().profile})).is_err());
}

#[test]
fn blocked_identity_network_does_not_delay_local_fsync_and_signout_cancels_pending_link() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    let credentials = Credentials::default();
    let db = root.path().join("canvas.db");
    let access = access(&root.path().join("room.db"), &server.endpoint, credentials.clone());
    server.accept(&access);
    let mut session = make_session(&db, credentials);
    session.execute("komaphone.canvas.create", json!({"name":"Offline usable"})).unwrap();
    session.flush_canvases(Duration::from_secs(5)).unwrap();
    connect(&mut session, &access);
    server.hold.store(true, Ordering::Release);
    session.canvas_link_identity(false).unwrap();
    let start = Instant::now();
    while !server.entered.load(Ordering::Acquire) {
        assert!(start.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(5));
    }
    session.canvas_link_access(Some(&access));
    assert!(session.canvases.link.busy);
    assert!(session.canvas_link_identity(false).is_err());
    let started = Instant::now();
    draw(&mut session);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(server.hold.load(Ordering::Acquire));
    session.canvas_link_access(None);
    session.worlds.account = None;
    server.hold.store(false, Ordering::Release);
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(150) {
        session.poll_canvases();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!session.canvases.link.busy);
    assert!(session.canvases.link.binding.is_none());
    assert!(session.canvases.error.is_none());
    let store = photocraft_local_store::canvas::CanvasStore::open(&db, Default::default()).unwrap();
    assert!(store.local_binding().unwrap().is_none());
    assert_eq!(store.active().unwrap().unwrap().stroke_count, 1);
}

#[test]
fn account_switch_during_protected_key_enrollment_rolls_back_the_binding_transaction() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new();
    let credentials = Credentials::default();
    let db = root.path().join("canvas.db");
    let access = access(&root.path().join("room.db"), &server.endpoint, credentials.clone());
    server.accept(&access);
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let mut session = Session::new();
    session
        .enable_canvases_with_link_credentials(
            &db,
            Default::default(),
            Box::new(GateCredentials { inner: credentials, entered: entered.clone(), release: release.clone() }),
        )
        .unwrap();
    session.flush_canvases(Duration::from_secs(5)).unwrap();
    session.execute("komaphone.canvas.create", json!({"name":"Retained"})).unwrap();
    session.flush_canvases(Duration::from_secs(5)).unwrap();
    draw(&mut session);
    let original = art(&db);
    connect(&mut session, &access);
    session.canvas_link_identity(false).unwrap();
    let start = Instant::now();
    while !entered.load(Ordering::Acquire) {
        session.poll_canvases();
        assert!(start.elapsed() < Duration::from_secs(4));
        std::thread::sleep(Duration::from_millis(5));
    }
    session.canvas_link_access(None);
    session.worlds.account = None;
    release.store(true, Ordering::Release);
    wait_link(&mut session);
    assert!(session.canvases.link.error.as_ref().is_some_and(|error| error.contains("Account changed")));
    assert!(session.canvases.link.binding.is_none());
    assert!(session.canvases.error.is_none());
    assert_eq!(art(&db), original);
    let store = photocraft_local_store::canvas::CanvasStore::open(&db, Default::default()).unwrap();
    assert!(store.local_binding().unwrap().is_none());
}
