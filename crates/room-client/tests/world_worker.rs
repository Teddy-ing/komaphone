#![cfg(not(target_arch = "wasm32"))]
use photocraft_party::protocol::RpcResponse;
use photocraft_room_client::{
    CredentialStore, Outbox, Result,
    world_worker::{Command, Event, WorldAccess, WorldWorker},
};
use photocraft_scene::{
    CanvasId, OwnerId, RegionKey, Srgba, Stroke, StrokeId,
    shared::{AcceptedOp, Edit, WorldState},
    world_protocol::{self as wire, Action, CanvasSummary, Response},
};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Default, Clone)]
struct Credentials(Arc<Mutex<BTreeMap<(String, String), String>>>);

#[test]
fn unreadable_queue_reports_worker_stopped_without_request_identity() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("corrupt.db");
    std::fs::write(&database, b"not a SQLite database").unwrap();
    let worker = WorldWorker::start(database, None).unwrap();
    wait(&worker, Duration::from_secs(5), |event| matches!(event, Event::Stopped { message } if !message.is_empty()));
}

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
fn read_request(socket: &mut TcpStream) -> wire::Request {
    socket.set_nonblocking(false).unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let n = socket.read(&mut buffer).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(buffer.get(..n).unwrap());
        assert!(bytes.len() < wire::MAX_REQUEST_BYTES + 8192);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let header = String::from_utf8_lossy(bytes.get(..end).unwrap());
            let length = header
                .lines()
                .find_map(|line| {
                    line.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("content-length")).map(|(_, v)| v.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return wire::Request::decode(bytes.get(end + 4..end + 4 + length).unwrap()).unwrap();
            }
        }
    }
}
fn response(socket: &mut TcpStream, response: Response) {
    let body = serde_json::to_vec(&response).unwrap();
    let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    let _ = socket.write_all(head.as_bytes());
    let _ = socket.write_all(&body);
}
fn wait(worker: &WorldWorker, timeout: Duration, mut condition: impl FnMut(Event) -> bool) {
    let start = Instant::now();
    loop {
        while let Ok(event) = worker.poll() {
            assert!(!matches!(&event,Event::Error {message,..} if {eprintln!("worker error: {message}");true}));
            if condition(event) {
                return;
            }
        }
        assert!(start.elapsed() < timeout, "canvas worker timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn slow_http_upload_does_not_delay_second_local_durable_save() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let mut room = Outbox::open(root.path().join("room.db"), Box::new(Credentials::default())).unwrap();
    room.configure(&endpoint, "Worker fixture").unwrap();
    let pair = room.next().unwrap().unwrap();
    let profile = room.status().profile.unwrap();
    room.acknowledge(&pair, &RpcResponse::Paired { participant: profile.clone() }).unwrap();
    let owner = OwnerId::new(profile.id.as_str()).unwrap();
    let canvas = CanvasId::new(&"1".repeat(32)).unwrap();
    let blocked = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let flags = (blocked.clone(), release.clone(), stop.clone());
    let server = std::thread::spawn(move || {
        let mut world = WorldState::new(canvas, owner, "Fixture".into()).unwrap();
        while !flags.2.load(Ordering::SeqCst) {
            let (mut socket, _) = match listener.accept() {
                Ok(value) => value,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(e) => panic!("{e}"),
            };
            let request = read_request(&mut socket);
            let result = match request.action {
                Action::List { .. } => Response::Canvases { items: vec![], next: None },
                Action::Info { .. } => Response::Canvas {
                    info: CanvasSummary {
                        canvas,
                        owner,
                        name: world.name.clone(),
                        import_origin: None,
                        initial_name: "Fixture".into(),
                        revision: world.revision,
                        epoch: 1,
                    },
                },
                Action::Events { after, through, limit, .. } => Response::Events {
                    events: world.events_after(after, limit as usize).unwrap().to_vec(),
                    through: through.unwrap_or(world.revision),
                    next: None,
                },
                Action::Append { operation } => {
                    if world.revision == 0 {
                        flags.0.store(true, Ordering::SeqCst);
                        let start = Instant::now();
                        while !flags.1.load(Ordering::SeqCst) && !flags.2.load(Ordering::SeqCst) {
                            assert!(start.elapsed() < Duration::from_secs(4));
                            std::thread::sleep(Duration::from_millis(5));
                        }
                    }
                    let event = AcceptedOp { revision: world.revision + 1, op: operation };
                    world.apply(&event).unwrap();
                    Response::Accepted { event: Box::new(event) }
                }
                _ => panic!("unexpected synthetic action"),
            };
            response(&mut socket, result);
        }
    });
    let worker = WorldWorker::start(root.path().join("world.db"), None).unwrap();
    worker.send(Command::Access(WorldAccess::from_room(&room).unwrap())).unwrap();
    wait(&worker, Duration::from_secs(5), |event| matches!(event, Event::Activated { scope: Some(_), .. }));
    worker.send(Command::Open(canvas)).unwrap();
    wait(&worker, Duration::from_secs(5), |event| matches!(event, Event::State { .. }));
    let stroke = |n| Edit::AddStroke {
        stroke: Stroke {
            id: StrokeId::new(&format!("{n:032x}")).unwrap(),
            canvas,
            owner,
            revision: 0,
            anchor: RegionKey::default(),
            points: vec![[1., 1.], [2., 2.]],
            width: 3.,
            color: Srgba { rgba: [0., 0., 0., 1.] },
            erase: false,
            brush: None,
            samples: None,
        },
    };
    worker.send(Command::Edit { canvas, action: stroke(100) }).unwrap();
    wait(&worker, Duration::from_secs(5), |event| matches!(event, Event::Saved { .. }));
    let start = Instant::now();
    while !blocked.load(Ordering::SeqCst) {
        assert!(start.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(5));
    }
    let start = Instant::now();
    let sequence = worker.send(Command::Edit { canvas, action: stroke(101) }).unwrap();
    wait(&worker, Duration::from_secs(1), |event| matches!(event,Event::Saved {sequence:s,..} if s==sequence));
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(!release.load(Ordering::SeqCst));
    release.store(true, Ordering::SeqCst);
    wait(&worker, Duration::from_secs(5), |event| matches!(event,Event::State {canonical,pending,..} if canonical.revision==2 && pending==0));
    stop.store(true, Ordering::SeqCst);
    drop(worker);
    server.join().unwrap();
}

#[test]
fn three_held_presence_http_requests_leave_local_saves_responsive() {
    use photocraft_scene::shared::{DeviceId, OpId};
    use std::sync::atomic::AtomicUsize;
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let mut room = Outbox::open(root.path().join("room.db"), Box::new(Credentials::default())).unwrap();
    room.configure(&endpoint, "Pipeline fixture").unwrap();
    let pair = room.next().unwrap().unwrap();
    let profile = room.status().profile.unwrap();
    room.acknowledge(&pair, &RpcResponse::Paired { participant: profile.clone() }).unwrap();
    let owner = OwnerId::new(profile.id.as_str()).unwrap();
    let canvas = CanvasId::new(&"1".repeat(32)).unwrap();
    let stream = OpId::new(&"2".repeat(32)).unwrap();
    let held = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let world = Arc::new(Mutex::new(WorldState::new(canvas, owner, "Pipeline".into()).unwrap()));
    let flags = (held.clone(), release.clone(), stop.clone());
    let server = std::thread::spawn(move || {
        let mut threads = Vec::new();
        while !flags.2.load(Ordering::SeqCst) {
            let (mut socket, _) = match listener.accept() {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(e) => panic!("{e}"),
            };
            let request = read_request(&mut socket);
            let world = world.clone();
            let held = flags.0.clone();
            let release = flags.1.clone();
            threads.push(std::thread::spawn(move || {
                let result = match request.action {
                    Action::List { .. } => Response::Canvases { items: vec![], next: None },
                    Action::Info { .. } => {
                        let world = world.lock().unwrap();
                        Response::Canvas {
                            info: CanvasSummary {
                                canvas,
                                owner,
                                name: world.name.clone(),
                                import_origin: None,
                                initial_name: "Pipeline".into(),
                                revision: world.revision,
                                epoch: 1,
                            },
                        }
                    }
                    Action::Events { after, through, limit, .. } => {
                        let world = world.lock().unwrap();
                        Response::Events {
                            events: world.events_after(after, limit as usize).unwrap().to_vec(),
                            through: through.unwrap_or(world.revision),
                            next: None,
                        }
                    }
                    Action::PresenceOpen { canvas, epoch, nonce } => Response::PresenceReady { canvas, epoch, nonce, stream },
                    Action::PresenceUpdate { canvas, stream, serial, .. } => {
                        held.fetch_add(1, Ordering::SeqCst);
                        let start = Instant::now();
                        while !release.load(Ordering::SeqCst) {
                            assert!(start.elapsed() < Duration::from_secs(4));
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Response::PresenceSnapshot { canvas, stream, observation: serial, peers: vec![] }
                    }
                    Action::Append { operation } => {
                        let mut world = world.lock().unwrap();
                        let event = AcceptedOp { revision: world.revision + 1, op: operation };
                        world.apply(&event).unwrap();
                        Response::Accepted { event: Box::new(event) }
                    }
                    _ => panic!("unexpected pipeline action"),
                };
                response(&mut socket, result);
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
    });
    let worker = WorldWorker::start(root.path().join("world.db"), None).unwrap();
    worker.send(Command::Access(WorldAccess::from_room(&room).unwrap())).unwrap();
    wait(&worker, Duration::from_secs(3), |event| matches!(event, Event::Activated { .. }));
    worker.send(Command::Open(canvas)).unwrap();
    wait(&worker, Duration::from_secs(3), |event| matches!(event, Event::State { .. }));
    for n in 1..=3 {
        worker
            .send(Command::Request(Action::Presence {
                canvas,
                epoch: 1,
                camera: photocraft_scene::coordinates::home(canvas),
                preview: vec![],
                cursor: None,
                stroke: None,
            }))
            .unwrap();
        let start = Instant::now();
        while held.load(Ordering::SeqCst) < n {
            assert!(start.elapsed() < Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    let stroke = Stroke {
        id: StrokeId::new(&"3".repeat(32)).unwrap(),
        canvas,
        owner,
        revision: 0,
        anchor: RegionKey::default(),
        points: vec![[1., 1.], [2., 2.]],
        width: 3.,
        color: Srgba { rgba: [0., 0., 0., 1.] },
        erase: false,
        brush: None,
        samples: None,
    };
    let start = Instant::now();
    let sequence = worker.send(Command::Edit { canvas, action: Edit::AddStroke { stroke } }).unwrap();
    wait(&worker, Duration::from_secs(1), |event| matches!(event,Event::Saved {sequence:s,..} if s==sequence));
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(held.load(Ordering::SeqCst), 3);
    assert!(!release.load(Ordering::SeqCst));
    release.store(true, Ordering::SeqCst);
    wait(&worker, Duration::from_secs(3), |event| matches!(event,Event::State {canonical,pending:0,..} if canonical.revision==1));
    drop(worker);
    stop.store(true, Ordering::SeqCst);
    server.join().unwrap();
    let _ = DeviceId::new(&"4".repeat(32)).unwrap();
}
