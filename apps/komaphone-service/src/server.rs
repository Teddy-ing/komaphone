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
    path::Path,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};
use tokio::sync::{Semaphore, oneshot};

const QUEUE: usize = 32;
struct Job {
    credential: Option<String>,
    request: RpcRequest,
    reply: oneshot::Sender<Result<RpcResponse>>,
}
#[derive(Clone)]
struct Shared {
    sender: mpsc::SyncSender<Job>,
    semaphore: Arc<Semaphore>,
    clock: Clock,
    rates: Arc<Mutex<Rates>>,
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
            Operation::Upload { .. } | Operation::Fetch { .. } => (2, 20000),
            Operation::Poll { .. } => (3, 20000),
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
    let task = tokio::spawn(serve(listener, store, clock, async {
        let _ = shutdown.await;
    }));
    Ok(RunningService { address, stop: Some(stop), task })
}

async fn serve(listener: tokio::net::TcpListener, mut store: Store, clock: Clock, shutdown: impl Future<Output = ()> + Send + 'static) -> Result<()> {
    let (sender, receiver) = mpsc::sync_channel::<Job>(QUEUE);
    let worker_clock = clock.clone();
    let worker = thread::Builder::new().name("komaphone-authority".into()).spawn(move || {
        let mut last_tick = Instant::now();
        loop {
            match receiver.recv_timeout(Duration::from_millis(250)) {
                Ok(job) => {
                    let result =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| store.execute(job.credential.as_deref(), &job.request, worker_clock.now())))
                            .unwrap_or(Err(Error::Storage));
                    let _ = job.reply.send(result);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
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
    let shared = Shared { sender, semaphore: Arc::new(Semaphore::new(QUEUE)), clock, rates: Arc::new(Mutex::new(Rates::default())) };
    let router = Router::new()
        .route("/v1/rpc", post(rpc))
        .route("/health", get(|| async { Json(serde_json::json!({"protocol_version":1,"service":"Komaphone"})) }))
        .with_state(shared);
    let result = axum::serve(listener, router).with_graceful_shutdown(shutdown).await;
    // Router has been dropped by serve; no senders remain and the isolated worker can terminate.
    worker.join().map_err(|_| Error::Storage)?;
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
    if shared.sender.try_send(Job { credential, request, reply }).is_err() {
        return fault(Error::Busy);
    }
    // Timeout does not cancel a committing operation; the exact request ID is safe to retry.
    match tokio::time::timeout(Duration::from_secs(30), receive).await {
        Ok(Ok(Ok(response))) => match serde_json::to_vec(&response) {
            Ok(bytes) if bytes.len() <= MAX_RESPONSE_BYTES => ([(header::CONTENT_TYPE, "application/json")], bytes).into_response(),
            _ => fault(DomainError::Limit("response; request a bounded asset chunk").into()),
        },
        Ok(Ok(Err(error))) => fault(error),
        _ => fault(Error::Busy),
    }
}
fn fault(error: Error) -> Response {
    let (status, code) = match &error {
        Error::Domain(DomainError::Unauthorized) => (StatusCode::FORBIDDEN, "unauthorized"),
        Error::Domain(DomainError::Conflict) => (StatusCode::CONFLICT, "conflict"),
        Error::Domain(DomainError::Phase) => (StatusCode::CONFLICT, "phase"),
        Error::Domain(DomainError::Version(_)) | Error::Schema => (StatusCode::UPGRADE_REQUIRED, "version"),
        Error::Domain(DomainError::Limit(_)) | Error::Full => (StatusCode::PAYLOAD_TOO_LARGE, "limit"),
        Error::Storage | Error::Io(_) => (StatusCode::SERVICE_UNAVAILABLE, "storage"),
        Error::Busy => (StatusCode::TOO_MANY_REQUESTS, "busy"),
        _ => (StatusCode::BAD_REQUEST, "invalid"),
    };
    (status, Json(Fault { code: code.into(), message: error.to_string() })).into_response()
}
