//! Bounded HTTP front end and serial database worker.
use crate::{Error, Result, Store, StoreOptions};
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use photocraft_party::protocol::{Fault, MAX_RESPONSE_BYTES, Operation, RpcRequest, RpcResponse};
use photocraft_party::{Error as DomainError, MAX_MESSAGE_BYTES};
use std::{
    collections::BTreeMap,
    future::Future,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};
use tokio::sync::{Semaphore, oneshot};

const QUEUE: usize = 32;
struct Job {
    credential: Option<String>,
    request: ServiceRequest,
    reply: oneshot::Sender<Result<ServiceResponse>>,
}
enum ServiceRequest {
    Room(RpcRequest),
    World(Box<photocraft_scene::world_protocol::Request>),
    Identity(crate::identity::wire::VerifiedRequest),
}
enum ServiceResponse {
    Room(RpcResponse),
    World(photocraft_scene::world_protocol::Response),
    Identity(crate::identity::wire::Response),
}
#[derive(Clone)]
struct Shared {
    sender: mpsc::SyncSender<Job>,
    semaphore: Arc<Semaphore>,
    clock: Clock,
    rates: Arc<Mutex<Rates>>,
    google: Option<crate::identity::google::GoogleVerifier>,
}
#[derive(Clone)]
struct Clock {
    start: Instant,
    origin: u64,
}
impl Clock {
    fn new() -> Result<Self> {
        Ok(Self { start: Instant::now(), origin: u64::try_from(chrono::Utc::now().timestamp_millis()).map_err(|_| Error::Invalid("service clock"))? })
    }
    fn now(&self) -> u64 {
        self.origin.saturating_add(u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX))
    }
}
#[derive(Default)]
struct Rates {
    entries: BTreeMap<(String, u8), (u64, u32)>,
}
impl Rates {
    fn allow(&mut self, principal: &str, op: &Operation, now: u64) -> bool {
        let (kind, limit) = match op {
            Operation::Pair { .. } => (0, 32),
            Operation::Invite { .. } | Operation::Join { .. } => (1, 128),
            Operation::Upload { .. } | Operation::Fetch { .. } | Operation::GameFetch { .. } => (2, 20000),
            Operation::Poll { .. } | Operation::History { .. } | Operation::Game { .. } | Operation::GameNotes { .. } => (3, 20000),
            _ => (4, 2048),
        };
        let minute = now / 60000;
        self.entries.retain(|_, (old, _)| *old == minute);
        let key = (crate::store::digest(principal.as_bytes()), kind);
        if !self.entries.contains_key(&key) && self.entries.len() >= 1024 {
            return false;
        }
        let counter = self.entries.entry(key).or_insert((minute, 0));
        counter.1 = counter.1.saturating_add(1);
        counter.1 <= limit
    }
}

/// A real TCP listener and database worker, usable by isolated integration clients.
pub struct RunningService {
    pub address: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<Result<()>>,
}
impl RunningService {
    pub async fn shutdown(mut self) -> Result<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.task.await.map_err(|_| Error::Storage)?
    }
}

pub async fn spawn(bind: SocketAddr, data_file: impl AsRef<Path>, options: StoreOptions) -> Result<RunningService> {
    if !bind.ip().is_loopback() {
        return Err(Error::Invalid("unencrypted listener must be loopback; TLS is required for private-network access"));
    }
    let clock = Clock::new()?;
    let store = Store::open(data_file, options, clock.now())?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let address = listener.local_addr()?;
    let (stop, shutdown) = oneshot::channel();
    if options.hosted {
        return Err(Error::Invalid("hosted mode requires Google verifier"));
    }
    let task = tokio::spawn(serve(listener, store, clock, None, async {
        let _ = shutdown.await;
    }));
    Ok(RunningService { address, stop: Some(stop), task })
}

/// Public hosted deployment binds only behind the trusted HTTPS reverse proxy.
pub async fn spawn_hosted(
    bind: SocketAddr,
    data_file: impl AsRef<Path>,
    mut options: StoreOptions,
    google: crate::identity::google::GoogleVerifier,
) -> Result<RunningService> {
    if !bind.ip().is_loopback() {
        return Err(Error::Invalid("hosted listener must be loopback behind the HTTPS proxy"));
    }
    options.hosted = true;
    let clock = Clock::new()?;
    let store = Store::open(data_file, options, clock.now())?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let address = listener.local_addr()?;
    let (stop, shutdown) = oneshot::channel();
    let task = tokio::spawn(serve(listener, store, clock, Some(google), async {
        let _ = shutdown.await;
    }));
    Ok(RunningService { address, stop: Some(stop), task })
}

/// Native encrypted route. Callers choose a private interface/firewall; binding it is
/// deliberately separate from the loopback-only plaintext development API.
pub async fn spawn_tls(bind: SocketAddr, data_file: impl AsRef<Path>, options: StoreOptions, tls: crate::tls::TlsConfig) -> Result<RunningService> {
    let clock = Clock::new()?;
    let store = Store::open(data_file, options, clock.now())?;
    let tcp = tokio::net::TcpListener::bind(bind).await?;
    let address = tcp.local_addr()?;
    let listener = crate::tls::TlsListener::new(tcp, tls);
    let (stop, shutdown) = oneshot::channel();
    if options.hosted {
        return Err(Error::Invalid("hosted mode requires Google verifier"));
    }
    let task = tokio::spawn(serve(listener, store, clock, None, async {
        let _ = shutdown.await;
    }));
    Ok(RunningService { address, stop: Some(stop), task })
}

#[derive(Default)]
struct CheckpointQueueState {
    pending: BTreeMap<photocraft_scene::CanvasId, crate::worlds::PendingCheckpointJob>,
    closed: bool,
}

impl CheckpointQueueState {
    fn push(&mut self, job: crate::worlds::PendingCheckpointJob) {
        if let Some(existing) = self.pending.get_mut(&job.canvas) {
            if job.target_revision >= existing.target_revision {
                *existing = job;
            }
            return;
        }
        if self.pending.len() >= crate::worlds::MAX_CHECKPOINT_BACKLOG
            && let Some(oldest) = self.pending.iter().min_by_key(|(_, j)| j.target_revision).map(|(c, _)| *c)
        {
            self.pending.remove(&oldest);
        }
        self.pending.insert(job.canvas, job);
    }
}

fn spawn_checkpoint_worker(
    db_path: PathBuf,
    queue: Arc<(Mutex<CheckpointQueueState>, Condvar)>,
    prepared_tx: mpsc::SyncSender<crate::worlds::PreparedCheckpoint>,
) -> std::io::Result<thread::JoinHandle<()>> {
    thread::Builder::new().name("komaphone-checkpoint".into()).spawn(move || {
        loop {
            let job = {
                let mut guard = queue.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                while guard.pending.is_empty() && !guard.closed {
                    guard = queue.1.wait(guard).unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                if guard.closed {
                    return;
                }
                let next_canvas = guard.pending.iter().min_by_key(|(_, j)| j.target_revision).map(|(c, _)| *c);
                let Some(canvas) = next_canvas else { continue };
                guard.pending.remove(&canvas)
            };
            let Some(job) = job else { continue };
            let prepared = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Store::prepare_checkpoint_from_path(&db_path, &job)))
                .ok()
                .and_then(Result::ok)
                .flatten();
            if let Some(prepared) = prepared
                && prepared_tx.send(prepared).is_err()
            {
                return;
            }
        }
    })
}

async fn serve<L: axum::serve::Listener<Addr = SocketAddr>>(
    listener: L,
    mut store: Store,
    clock: Clock,
    google: Option<crate::identity::google::GoogleVerifier>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let (sender, receiver) = mpsc::sync_channel::<Job>(QUEUE);
    let ckpt_queue = Arc::new((Mutex::new(CheckpointQueueState::default()), Condvar::new()));
    let (prepared_tx, prepared_rx) = mpsc::sync_channel::<crate::worlds::PreparedCheckpoint>(crate::worlds::MAX_CHECKPOINT_BACKLOG);
    let ckpt_worker = spawn_checkpoint_worker(store.db_path().to_path_buf(), Arc::clone(&ckpt_queue), prepared_tx)?;
    // Dispatch any catch-up checkpoints detected on Store::open immediately.
    {
        let initial = store.take_pending_checkpoints();
        if !initial.is_empty() {
            let mut guard = ckpt_queue.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            for job in initial {
                guard.push(job);
            }
            ckpt_queue.1.notify_one();
        }
    }
    let authority_ckpt_queue = Arc::clone(&ckpt_queue);
    let worker_clock = clock.clone();
    let client_id = google.as_ref().map(|g| g.client_id().to_owned());
    let worker = thread::Builder::new().name("komaphone-authority".into()).spawn(move || {
        let mut last_tick = Instant::now();
        loop {
            match receiver.recv_timeout(Duration::from_millis(20)) {
                Ok(job) => {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match job.request {
                        ServiceRequest::Room(request) => store.execute(job.credential.as_deref(), &request, worker_clock.now()).map(ServiceResponse::Room),
                        ServiceRequest::World(request) => store
                            .world_execute(job.credential.as_deref().ok_or(DomainError::Unauthorized)?, &request, worker_clock.now())
                            .map(ServiceResponse::World),
                        ServiceRequest::Identity(request) => store
                            .identity_execute(
                                request,
                                job.credential.as_deref(),
                                client_id.as_deref().ok_or(Error::Invalid("hosted login disabled"))?,
                                worker_clock.now(),
                            )
                            .map(ServiceResponse::Identity),
                    }))
                    .unwrap_or(Err(Error::Storage));
                    let _ = job.reply.send(result);
                    let new_jobs = store.take_pending_checkpoints();
                    if !new_jobs.is_empty() {
                        let mut guard = authority_ckpt_queue.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        for ckpt_job in new_jobs {
                            guard.push(ckpt_job);
                        }
                        authority_ckpt_queue.1.notify_one();
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            // Publish at most one pre-compressed checkpoint per turn so incoming RPCs stay responsive.
            if let Ok(prepared) = prepared_rx.try_recv() {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| store.publish_prepared_checkpoint(&prepared)));
            }
            if last_tick.elapsed() >= Duration::from_millis(250) {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| store.tick(worker_clock.now()))).unwrap_or(Err(Error::Storage));
                if result.is_err() {
                    eprintln!("Komaphone service timer/storage update failed; local drafts remain authoritative until a successful commit.");
                }
                last_tick = Instant::now();
            }
        }
    })?;
    let hosted = google.is_some();
    let shared = Shared { sender, semaphore: Arc::new(Semaphore::new(QUEUE)), clock, rates: Arc::new(Mutex::new(Rates::default())), google };
    let router = Router::new()
        .route("/v1/rpc", post(rpc))
        .route("/v1/canvas", post(world_rpc))
        .route("/v1/auth", post(auth_rpc))
        .route("/v1/auth/config", get(auth_config))
        .route("/health", get(move || async move { Json(serde_json::json!({"protocol_version":1,"service":"Komaphone","hosted":hosted})) }))
        .with_state(shared);
    let result = axum::serve(listener, router).with_graceful_shutdown(shutdown).await;
    // Router has been dropped by serve; wake checkpoint worker, stop authority worker first
    // (dropping prepared_rx so checkpoint worker cannot block on send), then join checkpoint worker.
    {
        let mut guard = ckpt_queue.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.closed = true;
        ckpt_queue.1.notify_all();
    }
    worker.join().map_err(|_| Error::Storage)?;
    let _ = ckpt_worker.join();
    result.map_err(Error::Io)
}

async fn rpc(State(shared): State<Shared>, request: Request) -> Response {
    let Ok(_permit) = shared.semaphore.clone().try_acquire_owned() else {
        return fault(Error::Busy);
    };
    if request.headers().contains_key(header::ORIGIN) {
        return fault(DomainError::Unauthorized.into());
    }
    if request.headers().contains_key(header::CONTENT_ENCODING) {
        return fault(Error::Invalid("compressed request bodies are unsupported"));
    }
    let credential = match request.headers().get(header::AUTHORIZATION) {
        None => None,
        Some(value) => match value.to_str().ok().and_then(|v| v.strip_prefix("Bearer ")) {
            Some(value) if value.len() == 64 => Some(value.to_owned()),
            _ => return fault(DomainError::Unauthorized.into()),
        },
    };
    let bytes = match tokio::time::timeout(Duration::from_secs(10), to_bytes(request.into_body(), MAX_MESSAGE_BYTES)).await {
        Ok(Ok(bytes)) => bytes,
        _ => return fault(DomainError::Limit("request bytes or transfer time").into()),
    };
    let request = match RpcRequest::decode(&bytes) {
        Ok(request) => request,
        Err(error) => return fault(error.into()),
    };
    let now = shared.clock.now();
    let allowed =
        shared.rates.lock().unwrap_or_else(std::sync::PoisonError::into_inner).allow(credential.as_deref().unwrap_or("unpaired"), &request.operation, now);
    if !allowed {
        return fault(Error::Busy);
    }
    let (reply, receive) = oneshot::channel();
    if shared.sender.try_send(Job { credential, request: ServiceRequest::Room(request), reply }).is_err() {
        return fault(Error::Busy);
    }
    // Timeout does not cancel a committing operation; the exact request ID is safe to retry.
    match tokio::time::timeout(Duration::from_secs(30), receive).await {
        Ok(Ok(Ok(ServiceResponse::Room(response)))) => match serde_json::to_vec(&response) {
            Ok(bytes) if bytes.len() <= MAX_RESPONSE_BYTES => ([(header::CONTENT_TYPE, "application/json")], bytes).into_response(),
            _ => fault(DomainError::Limit("response; request a bounded asset chunk").into()),
        },
        Ok(Ok(Err(error))) => fault(error),
        _ => fault(Error::Busy),
    }
}

async fn world_rpc(State(shared): State<Shared>, request: Request) -> Response {
    use photocraft_scene::world_protocol as wire;
    let Ok(_permit) = shared.semaphore.clone().try_acquire_owned() else { return fault(Error::Busy) };
    if request.headers().contains_key(header::ORIGIN) {
        return fault(DomainError::Unauthorized.into());
    }
    if request.headers().contains_key(header::CONTENT_ENCODING) {
        return fault(Error::Invalid("compressed request bodies are unsupported"));
    }
    let credential = match request.headers().get(header::AUTHORIZATION).and_then(|h| h.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")) {
        Some(value) if photocraft_party::protocol::validate_secret(value).is_ok() => value.to_owned(),
        _ => return fault(DomainError::Unauthorized.into()),
    };
    let bytes = match tokio::time::timeout(Duration::from_secs(10), to_bytes(request.into_body(), wire::MAX_REQUEST_BYTES)).await {
        Ok(Ok(bytes)) => bytes,
        _ => return fault(DomainError::Limit("canvas request size or transfer time").into()),
    };
    // The body is already bounded above. Keep malformed requests distinct from
    // a valid envelope using an unsupported version, so clients can offer the
    // existing upgrade/reconnect guidance instead of a generic invalid request.
    let request: wire::Request = match serde_json::from_slice(&bytes) {
        Ok(request) => request,
        Err(_) => return fault(Error::Invalid("canvas request")),
    };
    if request.version != wire::VERSION {
        return fault(DomainError::Version(request.version).into());
    }
    if request.validate_bounds().is_err() {
        return fault(DomainError::Limit("canvas request bounds").into());
    }
    // Presence has a distinct bucket so dropped previews cannot exhaust durable writes.
    let kind = if matches!(
        request.action,
        wire::Action::Presence { .. }
            | wire::Action::Peers { .. }
            | wire::Action::PresenceOpen { .. }
            | wire::Action::PresenceUpdate { .. }
            | wire::Action::PresenceRead { .. }
    ) {
        5
    } else {
        6
    };
    let allowed = {
        let mut rates = shared.rates.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let minute = shared.clock.now() / 60000;
        rates.entries.retain(|_, (old, _)| *old == minute);
        let key = (crate::store::digest(credential.as_bytes()), kind);
        if !rates.entries.contains_key(&key) && rates.entries.len() >= 1024 {
            false
        } else {
            let counter = rates.entries.entry(key).or_insert((minute, 0));
            counter.1 = counter.1.saturating_add(1);
            counter.1 <= if kind == 5 { 1200 } else { 2048 }
        }
    };
    if !allowed {
        return fault(Error::Busy);
    }
    let (reply, receive) = oneshot::channel();
    if shared.sender.try_send(Job { credential: Some(credential), request: ServiceRequest::World(Box::new(request)), reply }).is_err() {
        return fault(Error::Busy);
    }
    match tokio::time::timeout(Duration::from_secs(30), receive).await {
        Ok(Ok(Ok(ServiceResponse::World(response)))) => match serde_json::to_vec(&response) {
            Ok(bytes) if bytes.len() <= wire::MAX_RESPONSE_BYTES => ([(header::CONTENT_TYPE, "application/json")], bytes).into_response(),
            _ => fault(DomainError::Limit("canvas response").into()),
        },
        Ok(Ok(Err(error))) => fault(error),
        _ => fault(Error::Busy),
    }
}

async fn auth_config(State(shared): State<Shared>) -> Response {
    let Some(google) = shared.google else {
        return fault(Error::Invalid("hosted login is not configured"));
    };
    ([(header::CACHE_CONTROL, "no-store")], Json(serde_json::json!({"protocol_version":1,"client_id":google.client_id(),"hosted":true}))).into_response()
}

async fn auth_rpc(State(shared): State<Shared>, request: Request) -> Response {
    use crate::identity::wire::{Request as AuthRequest, VerifiedRequest};
    let Some(google) = &shared.google else {
        return fault(Error::Invalid("hosted login is not configured"));
    };
    let Ok(_permit) = shared.semaphore.clone().try_acquire_owned() else {
        return fault(Error::Busy);
    };
    if request.headers().contains_key(header::ORIGIN) || request.headers().contains_key(header::CONTENT_ENCODING) {
        return fault(DomainError::Unauthorized.into());
    }
    let allowed = {
        let mut rates = shared.rates.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let minute = shared.clock.now() / 60000;
        rates.entries.retain(|_, (old, _)| *old == minute);
        let counter = rates.entries.entry(("hosted-auth".into(), 7)).or_insert((minute, 0));
        counter.1 = counter.1.saturating_add(1);
        counter.1 <= 120
    };
    if !allowed {
        return fault(Error::Busy);
    }
    let credential = match request.headers().get(header::AUTHORIZATION) {
        None => None,
        Some(value) => match value.to_str().ok().and_then(|s| s.strip_prefix("Bearer ")) {
            Some(value) if photocraft_party::protocol::validate_secret(value).is_ok() => Some(value.to_owned()),
            _ => return fault(DomainError::Unauthorized.into()),
        },
    };
    let bytes = match tokio::time::timeout(Duration::from_secs(10), to_bytes(request.into_body(), crate::identity::wire::MAX_REQUEST_BYTES)).await {
        Ok(Ok(bytes)) => bytes,
        _ => return fault(DomainError::Limit("login request bytes or transfer time").into()),
    };
    let parsed: AuthRequest = match serde_json::from_slice(&bytes) {
        Ok(parsed) => parsed,
        Err(_) => return fault(Error::Invalid("login request")),
    };
    let verified = match parsed {
        AuthRequest::Begin { device } => VerifiedRequest::Begin { device },
        AuthRequest::Cancel { transaction, nonce } => VerifiedRequest::Cancel { transaction, nonce },
        AuthRequest::Revoke => VerifiedRequest::Revoke,
        AuthRequest::Finish { transaction, id_token, device_proof, admission, label } => {
            if photocraft_party::protocol::validate_secret(&device_proof).is_err()
                || admission.as_deref().is_some_and(|v| photocraft_party::protocol::validate_secret(v).is_err())
                || label.len() > 128
            {
                return fault(Error::Invalid("login credential or display label"));
            }
            match google.verify(transaction, &id_token, shared.clock.now()).await {
                Ok(verified) => VerifiedRequest::Finish { verified, device_proof, admission, label },
                Err(error) => return fault(error),
            }
        }
    };
    let (reply, receive) = oneshot::channel();
    if shared.sender.try_send(Job { credential, request: ServiceRequest::Identity(verified), reply }).is_err() {
        return fault(Error::Busy);
    }
    match tokio::time::timeout(Duration::from_secs(30), receive).await {
        Ok(Ok(Ok(ServiceResponse::Identity(response)))) => ([(header::CACHE_CONTROL, "no-store")], Json(response)).into_response(),
        Ok(Ok(Err(error))) => fault(error),
        _ => fault(Error::Busy),
    }
}
fn fault(error: Error) -> Response {
    let (status, code) = match &error {
        Error::Session => (StatusCode::UNAUTHORIZED, "session"),
        Error::PresenceStream => (StatusCode::CONFLICT, "presence_stream"),
        Error::Domain(DomainError::Unauthorized) => (StatusCode::FORBIDDEN, "unauthorized"),
        Error::Domain(DomainError::Conflict) => (StatusCode::CONFLICT, "conflict"),
        Error::Domain(DomainError::Phase) => (StatusCode::CONFLICT, "phase"),
        Error::Domain(DomainError::Version(_)) | Error::Schema => (StatusCode::UPGRADE_REQUIRED, "version"),
        Error::Domain(DomainError::Limit(_)) | Error::Full => (StatusCode::PAYLOAD_TOO_LARGE, "limit"),
        Error::Storage | Error::Io(_) => (StatusCode::SERVICE_UNAVAILABLE, "storage"),
        Error::Busy => (StatusCode::TOO_MANY_REQUESTS, "busy"),
        Error::IdentityProvider => (StatusCode::SERVICE_UNAVAILABLE, "identity_provider"),
        _ => (StatusCode::BAD_REQUEST, "invalid"),
    };
    (status, Json(Fault { code: code.into(), message: error.to_string() })).into_response()
}
