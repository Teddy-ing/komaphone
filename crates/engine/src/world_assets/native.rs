use super::{ExportStatus, PreparedWorldFiles, WorldExport, WorldFileAccess, error};
use crate::{Result, Session, world_media};
use photocraft_scene::{
    Camera, CanvasId, Viewport,
    shared::{
        WorldState,
        media::{AssetHash, MediaFrame, MediaPlacement},
        tours::{ExportSettings, FrozenTour},
    },
    world_protocol as wire,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
};

const MEMORY_BYTES: usize = 32 * 1024 * 1024;
const MAX_DOWNLOADS: usize = 8;
const MAX_CACHE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_CACHE_FILES: usize = 4096;

mod restore;
type ImportReply = SyncSender<std::result::Result<wire::Response, String>>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Scope {
    account: crate::worlds::Account,
    canvas: CanvasId,
    generation: u64,
}
impl Scope {
    fn key(&self) -> Result<String> {
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&(&self.account, self.canvas)).map_err(error)?)))
    }
}
#[derive(Clone)]
struct Ticket {
    scope: Scope,
    frame: MediaFrame,
    offset: u64,
}
struct Cached {
    frame: MediaFrame,
    bytes: Arc<Vec<u8>>,
}
#[derive(Default)]
pub(super) struct Assets {
    worker: Option<Worker>,
    scope: Option<Scope>,
    generation: u64,
    cache: BTreeMap<AssetHash, Cached>,
    used: usize,
    waiting: BTreeSet<AssetHash>,
    requests: BTreeMap<u64, Ticket>,
    export_cancel: Option<Arc<AtomicBool>>,
    restore_request: Option<u64>,
    import_reply: Option<(u64, ImportReply)>,
}
impl Assets {
    pub(super) fn start(&mut self, path: PathBuf) -> Result<()> {
        if self.worker.is_some() {
            return Err(error("World media storage is already configured"));
        }
        let worker = Worker::start(path)?;
        worker.generation.store(self.generation, Ordering::Release);
        self.worker = Some(worker);
        Ok(())
    }
    pub(super) fn reset(&mut self) {
        self.cancel_export();
        self.generation = self.generation.saturating_add(1);
        if let Some(worker) = &self.worker {
            worker.generation.store(self.generation, Ordering::Release);
        }
        self.scope = None;
        self.cache.clear();
        self.used = 0;
        self.waiting.clear();
        self.requests.clear();
        self.restore_request = None;
        self.import_reply = None;
    }
    pub(super) fn cancel_export(&mut self) {
        if let Some(cancel) = self.export_cancel.take() {
            cancel.store(true, Ordering::Release);
            // Wake a job waiting on a network chunk; a full queue already wakes it.
            let _ = self.send(Command::Cancel);
        }
    }
    pub(super) fn ensure_cancellable(&self) -> Result<()> {
        if self.restore_request.is_some() {
            return Err(error("Restore has reached the service; wait for its result before retrying the same destination"));
        }
        Ok(())
    }
    pub(super) fn bytes(&self, frame: &MediaFrame) -> Result<Option<Arc<Vec<u8>>>> {
        if let Some(cached) = self.cache.get(&frame.hash) {
            if !same_frame(&cached.frame, frame) {
                return Err(error("Conflicting immutable media descriptors"));
            }
            return Ok(Some(Arc::clone(&cached.bytes)));
        }
        Ok(None)
    }
    fn send(&self, command: Command) -> Result<()> {
        self.worker.as_ref().ok_or_else(|| error("World media worker unavailable"))?.send(command)
    }
    fn cache(&mut self, frame: MediaFrame, bytes: Arc<Vec<u8>>) -> Result<()> {
        if bytes.len() > MEMORY_BYTES {
            return Err(error("Media frame exceeds memory cache limit"));
        }
        self.waiting.remove(&frame.hash);
        if self.cache.contains_key(&frame.hash) {
            return Ok(());
        }
        while self.used.saturating_add(bytes.len()) > MEMORY_BYTES {
            if let Some((_, old)) = self.cache.pop_first() {
                self.used = self.used.saturating_sub(old.bytes.len());
            } else {
                break;
            }
        }
        self.used = self.used.saturating_add(bytes.len());
        self.cache.insert(frame.hash, Cached { frame, bytes });
        Ok(())
    }
}
fn same_frame(a: &MediaFrame, b: &MediaFrame) -> bool {
    a.hash == b.hash && a.byte_len == b.byte_len && a.width == b.width && a.height == b.height
}
fn scope(session: &mut Session) -> Result<Scope> {
    let account = session.worlds.account.as_ref().ok_or_else(|| error("Connect to the account that owns this cached canvas"))?.clone();
    let canvas = session.worlds.active.as_ref().ok_or_else(|| error("Open a shared canvas first"))?.info.canvas;
    let assets = &mut session.worlds.assets;
    if assets.inner.scope.as_ref().is_some_and(|old| old.account != account || old.canvas != canvas) {
        assets.reset();
    }
    if assets.inner.scope.is_none() {
        assets.inner.scope = Some(Scope { account, canvas, generation: assets.inner.generation });
    }
    assets.inner.scope.clone().ok_or_else(|| error("Missing media scope"))
}
pub(super) fn request_frame(session: &mut Session, frame: &MediaFrame) -> Result<Option<Arc<Vec<u8>>>> {
    frame.validate().map_err(error)?;
    let scope = scope(session)?;
    // Cached and in-flight frames already passed the scoped-reference check.
    // Keep the per-frame draw path independent of the size of world history.
    if let Some(bytes) = session.worlds.assets.inner.bytes(frame)? {
        return Ok(Some(bytes));
    }
    if session.worlds.assets.inner.waiting.contains(&frame.hash) || session.worlds.assets.inner.waiting.len() >= MAX_DOWNLOADS {
        return Ok(None);
    }
    let active = session.worlds.active.as_ref().ok_or_else(|| error("Open a shared canvas first"))?;
    let allowed = active.display.media.values().any(|media| media.media.frames.iter().any(|accepted| same_frame(accepted, frame)))
        || session.worlds.assets.prepared.as_ref().is_some_and(|media| media.frames.iter().any(|accepted| same_frame(accepted, frame)));
    if !allowed {
        return Err(error("This frame is not part of the current authorized canvas"));
    }
    let assets = &mut session.worlds.assets.inner;
    assets.send(Command::Load(Ticket { scope, frame: frame.clone(), offset: 0 }))?;
    assets.waiting.insert(frame.hash);
    Ok(None)
}
pub(super) fn export(session: &mut Session, action: WorldExport, files: Option<Arc<dyn WorldFileAccess>>) -> Result<()> {
    if matches!(action, WorldExport::Restore { .. }) {
        return restore::start(session, action, files);
    }
    let scope = scope(session)?;
    if session.worlds.assets.export.as_ref().is_some_and(|status| status.busy) {
        return Err(error("A world export is already running"));
    }
    let active = session.worlds.active.as_ref().ok_or_else(|| error("Open a shared canvas first"))?;
    if active.pending != 0 || session.worlds.busy() {
        return Err(error("Wait for all canvas edits to sync before exporting the accepted revision"));
    }
    let view_revision = if matches!(action, WorldExport::View { .. }) {
        if let Some(review) = &active.viewer.review {
            if review.canvas != scope.canvas || review.viewer != scope.account.profile {
                return Err(error("Review belongs to another canvas or account"));
            }
            review.validate(active.canonical.revision).map_err(error)?;
            review.interval.as_ref().map(|interval| interval.position)
        } else {
            None
        }
    } else {
        None
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let job =
        Job { scope, world: Arc::clone(&active.canonical), camera: active.viewer.camera.clone(), view_revision, action, files, cancel: Arc::clone(&cancel) };
    session.worlds.assets.inner.send(Command::Export(job))?;
    session.worlds.assets.inner.export_cancel = Some(cancel);
    session.worlds.assets.error = None;
    session.worlds.assets.export = Some(ExportStatus { busy: true, completed: 0, total: 0, message: "Preparing accepted canvas revision…".into() });
    Ok(())
}
pub(super) fn response(session: &mut Session, sequence: u64, request: Option<&wire::Action>, response: &wire::Response) -> Result<()> {
    if session.worlds.assets.inner.import_reply.as_ref().is_some_and(|(pending, _)| *pending == sequence) {
        if let Some((_, reply)) = session.worlds.assets.inner.import_reply.take() {
            let _ = reply.send(Ok(response.clone()));
        }
        session.worlds.assets.inner.restore_request = None;
        return Ok(());
    }
    if let Some(ticket) = session.worlds.assets.inner.requests.remove(&sequence) {
        if session.worlds.assets.inner.scope.as_ref() != Some(&ticket.scope) {
            return Ok(());
        }
        if let wire::Response::MediaChunk { bytes, total } = response {
            if bytes.is_empty() || bytes.len() > photocraft_party::protocol::CHUNK_BYTES || *total != ticket.frame.byte_len {
                session.worlds.assets.reset();
                return Err(error("Media response exceeds the requested immutable chunk limits"));
            }
            let result = session.worlds.assets.inner.send(Command::Chunk { ticket, bytes: bytes.clone(), total: *total });
            if result.is_err() {
                session.worlds.assets.reset();
            }
            return result;
        }
        session.worlds.assets.reset();
        return Err(error("Unexpected response to a media download"));
    }
    if let (Some(wire::Action::PrepareGameMedia { canvas, .. }), wire::Response::MediaPrepared { media }) = (request, response) {
        if session.worlds.active.as_ref().is_none_or(|active| active.info.canvas != *canvas) {
            return Ok(());
        }
        let scope = scope(session)?;
        let placement = MediaPlacement {
            id: photocraft_scene::shared::MediaId::new("11111111111111111111111111111111").map_err(error)?,
            canvas: *canvas,
            owner: scope.account.profile,
            anchor: photocraft_scene::coordinates::home(*canvas).region,
            offset: [0.0, 0.0],
            width: 1.0,
            height: 1.0,
            frames: media.frames.clone(),
            looping: media.looping,
            provenance: media.provenance.clone(),
        };
        placement.validate().map_err(error)?;
        session.worlds.assets.prepared = Some((**media).clone());
    }
    Ok(())
}
pub(super) fn failure(session: &mut Session, sequence: u64, message: &str) {
    if session.worlds.assets.inner.import_reply.as_ref().is_some_and(|(pending, _)| *pending == sequence)
        && let Some((_, reply)) = session.worlds.assets.inner.import_reply.take()
    {
        let _ = reply.send(Err(message.to_owned()));
    }
    if session.worlds.assets.inner.restore_request == Some(sequence) {
        session.worlds.assets.inner.restore_request = None;
        session.worlds.assets.export = Some(ExportStatus { busy: false, completed: 0, total: 1, message: message.into() });
        session.worlds.assets.error = Some(message.into());
    }
    if let Some(ticket) = session.worlds.assets.inner.requests.remove(&sequence) {
        session.worlds.assets.inner.waiting.remove(&ticket.frame.hash);
        if session.worlds.assets.inner.send(Command::Failure { ticket, message: message.into() }).is_err() {
            session.worlds.assets.reset();
        }
        session.worlds.assets.error = Some(message.into());
    }
}
pub(super) fn poll(session: &mut Session) {
    loop {
        let event = session.worlds.assets.inner.worker.as_ref().and_then(|worker| worker.events.try_recv().ok());
        let Some(event) = event else {
            break;
        };
        if session.worlds.assets.inner.scope.as_ref() != Some(event.scope()) {
            continue;
        }
        match event {
            Event::Import { action, reply, cancel, .. } => {
                if cancel.load(Ordering::Acquire) {
                    let _ = reply.send(Err("Restore cancelled before service request".into()));
                    continue;
                }
                let commit = matches!(*action, wire::Action::ImportCommit { .. });
                match session.world_request(*action) {
                    Ok(sequence) => {
                        session.worlds.assets.inner.import_reply = Some((sequence, reply));
                        if commit {
                            session.worlds.assets.inner.restore_request = Some(sequence);
                        }
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error.to_string()));
                    }
                }
            }
            Event::Need(ticket) => {
                match session.world_request(wire::Action::FetchMedia { canvas: ticket.scope.canvas, hash: ticket.frame.hash, offset: ticket.offset }) {
                    Ok(sequence) => {
                        session.worlds.assets.inner.requests.insert(sequence, ticket);
                    }
                    Err(error) => {
                        let message = error.to_string();
                        let _ = session.worlds.assets.inner.send(Command::Failure { ticket, message: message.clone() });
                        session.worlds.assets.error = Some(message);
                    }
                }
            }
            Event::Ready { frame, bytes, .. } => {
                if let Err(error) = session.worlds.assets.inner.cache(frame, bytes) {
                    session.worlds.assets.error = Some(error.to_string());
                }
            }
            Event::Failed { hash, message, .. } => {
                if let Some(hash) = hash {
                    session.worlds.assets.inner.waiting.remove(&hash);
                } else {
                    session.worlds.assets.export = Some(ExportStatus { busy: false, completed: 0, total: 0, message: message.clone() });
                }
                session.worlds.assets.error = Some(message);
            }
            Event::Progress { status, .. } => {
                session.worlds.assets.export = Some(status);
            }
        }
    }
}

struct Worker {
    commands: SyncSender<Command>,
    events: Receiver<Event>,
    generation: Arc<AtomicU64>,
}
impl Worker {
    fn start(root: PathBuf) -> Result<Self> {
        let (commands, rx) = mpsc::sync_channel(32);
        let (tx, events) = mpsc::sync_channel(32);
        let generation = Arc::new(AtomicU64::new(0));
        let check = Arc::clone(&generation);
        std::thread::Builder::new().name("komaphone-world-assets".into()).spawn(move || run(root, rx, tx, check)).map_err(error)?;
        Ok(Self { commands, events, generation })
    }
    fn send(&self, command: Command) -> Result<()> {
        self.commands.try_send(command).map_err(|_| error("World media worker is busy; retry shortly"))
    }
}
struct Job {
    scope: Scope,
    world: Arc<WorldState>,
    camera: Camera,
    /// Cheap UI capture only; replay and renderer construction run on this worker.
    view_revision: Option<u64>,
    action: WorldExport,
    files: Option<Arc<dyn WorldFileAccess>>,
    cancel: Arc<AtomicBool>,
}
enum Command {
    Load(Ticket),
    Chunk { ticket: Ticket, bytes: Vec<u8>, total: u64 },
    Export(Job),
    Import(restore::Job),
    Failure { ticket: Ticket, message: String },
    Cancel,
}
enum Event {
    Import { scope: Scope, action: Box<wire::Action>, reply: ImportReply, cancel: Arc<AtomicBool> },
    Need(Ticket),
    Ready { scope: Scope, frame: MediaFrame, bytes: Arc<Vec<u8>> },
    Failed { scope: Scope, hash: Option<AssetHash>, message: String },
    Progress { scope: Scope, status: ExportStatus },
}
impl Event {
    fn scope(&self) -> &Scope {
        match self {
            Self::Need(ticket) => &ticket.scope,
            Self::Ready { scope, .. } | Self::Failed { scope, .. } | Self::Progress { scope, .. } | Self::Import { scope, .. } => scope,
        }
    }
}
struct Download {
    ticket: Ticket,
    bytes: Vec<u8>,
    display: bool,
}
struct PendingJob {
    job: Job,
    frames: Vec<MediaFrame>,
    next: usize,
}

fn run(root: PathBuf, commands: Receiver<Command>, events: SyncSender<Event>, generation: Arc<AtomicU64>) {
    let mut downloads = BTreeMap::<AssetHash, Download>::new();
    let mut pending: Option<PendingJob> = None;
    while let Ok(command) = commands.recv() {
        downloads.retain(|_, download| current(&download.ticket.scope, &generation));
        if pending.as_ref().is_some_and(|job| !alive(&job.job, &generation))
            && let Some(job) = pending.take()
            && current(&job.job.scope, &generation)
            && events
                .send(Event::Progress {
                    scope: job.job.scope,
                    status: ExportStatus { busy: false, completed: 0, total: 0, message: "Export cancelled; saved artwork is unchanged".into() },
                })
                .is_err()
        {
            return;
        }
        let (scope, hash, result) = match command {
            Command::Import(job) => {
                let scope = job.scope.clone();
                let result = restore::run(job, &events, &generation);
                (scope, None, result)
            }
            Command::Load(ticket) => {
                if !current(&ticket.scope, &generation) {
                    continue;
                }
                let scope = ticket.scope.clone();
                let hash = ticket.frame.hash;
                let result = load(&root, ticket, true, &mut downloads, &events);
                (scope, Some(hash), result)
            }
            Command::Chunk { ticket, bytes, total } => {
                if !current(&ticket.scope, &generation) {
                    continue;
                }
                let scope = ticket.scope.clone();
                let hash = ticket.frame.hash;
                let result = chunk(&root, ticket, bytes, total, &mut downloads, &events);
                (scope, Some(hash), result)
            }
            Command::Failure { ticket, message } => {
                if !current(&ticket.scope, &generation) {
                    continue;
                }
                (ticket.scope, Some(ticket.frame.hash), Err(error(message)))
            }
            Command::Cancel => continue,
            Command::Export(job) => {
                if !alive(&job, &generation) {
                    continue;
                }
                let scope = job.scope.clone();
                let result = prepare_job(job).map(|job| {
                    pending = Some(job);
                });
                (scope, None, result)
            }
        };
        if let Err(error) = result {
            if let Some(hash) = hash {
                downloads.remove(&hash);
            }
            if events.send(Event::Failed { scope: scope.clone(), hash, message: error.to_string() }).is_err() {
                return;
            }
            if pending.as_ref().is_some_and(|job| job.job.scope == scope && hash.is_none_or(|failed| job.frames.iter().any(|frame| frame.hash == failed))) {
                pending = None;
                let _ = events
                    .send(Event::Progress { scope: scope.clone(), status: ExportStatus { busy: false, completed: 0, total: 0, message: error.to_string() } });
            }
        }
        if let Some(mut job) = pending.take() {
            match advance(&root, &mut job, &mut downloads, &events, &generation) {
                Ok(false) => pending = Some(job),
                Ok(true) => {}
                Err(error) => {
                    if events
                        .send(Event::Progress {
                            scope: job.job.scope,
                            status: ExportStatus { busy: false, completed: 0, total: 0, message: error.to_string() },
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            }
        }
    }
}
fn current(scope: &Scope, generation: &AtomicU64) -> bool {
    scope.generation == generation.load(Ordering::Acquire)
}
fn alive(job: &Job, generation: &AtomicU64) -> bool {
    current(&job.scope, generation) && !job.cancel.load(Ordering::Acquire)
}
fn cache_path(root: &Path, scope: &Scope, frame: &MediaFrame) -> Result<PathBuf> {
    Ok(root.join(scope.key()?).join(format!("{}.png", frame.hash.as_str())))
}
fn read(path: &Path, frame: &MediaFrame) -> Result<Option<Vec<u8>>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(error(e)),
    };
    let mut bytes = Vec::new();
    file.take(frame.byte_len.saturating_add(1)).read_to_end(&mut bytes).map_err(error)?;
    world_media::decode_frame(frame, &bytes)?;
    Ok(Some(bytes))
}
fn cache_size(root: &Path) -> Result<(usize, u64)> {
    let mut count = 0usize;
    let mut bytes = 0u64;
    let mut scopes = 0usize;
    if !root.try_exists().map_err(error)? {
        return Ok((0, 0));
    }
    for entry in std::fs::read_dir(root).map_err(error)? {
        let entry = entry.map_err(error)?;
        if !entry.file_type().map_err(error)?.is_dir() {
            continue;
        }
        scopes = scopes.saturating_add(1);
        if scopes > MAX_CACHE_FILES {
            return Err(error("World media cache directory limit reached"));
        }
        for file in std::fs::read_dir(entry.path()).map_err(error)? {
            let file = file.map_err(error)?;
            if !file.file_type().map_err(error)?.is_file() {
                continue;
            }
            count = count.saturating_add(1);
            bytes = bytes.saturating_add(file.metadata().map_err(error)?.len());
            if count > MAX_CACHE_FILES || bytes > MAX_CACHE_BYTES {
                return Err(error("World media cache is full; exported originals remain intact"));
            }
        }
    }
    Ok((count, bytes))
}
fn save(root: &Path, scope: &Scope, frame: &MediaFrame, bytes: &[u8]) -> Result<()> {
    let path = cache_path(root, scope, frame)?;
    if let Some(existing) = read(&path, frame)? {
        return if existing == bytes { Ok(()) } else { Err(error("Conflicting cached media")) };
    }
    let (count, used) = cache_size(root)?;
    if count >= MAX_CACHE_FILES || used.saturating_add(bytes.len() as u64) > MAX_CACHE_BYTES {
        return Err(error("World media cache exceeds 512 MiB; free cache space before downloading more"));
    }
    let parent = path.parent().ok_or_else(|| error("Invalid media cache path"))?;
    std::fs::create_dir_all(parent).map_err(|e| error(format!("Cannot create media cache directory: {e}")))?;
    // tempfile's Windows publication uses MoveFileExW directly. Unlike Rust's
    // std filesystem operations, it does not add the extended path prefix for
    // paths beyond MAX_PATH. Canonicalize the existing parent before creating
    // the temporary file so both paths use the same supported absolute form.
    let parent = parent.canonicalize().map_err(|e| error(format!("Cannot resolve media cache directory: {e}")))?;
    let path = parent.join(path.file_name().ok_or_else(|| error("Missing media cache filename"))?);
    let mut temporary = tempfile::NamedTempFile::new_in(&parent).map_err(|e| error(format!("Cannot create temporary media cache file: {e}")))?;
    temporary.write_all(bytes).map_err(|e| error(format!("Cannot write temporary media cache file: {e}")))?;
    temporary.as_file().sync_all().map_err(|e| error(format!("Cannot sync temporary media cache file: {e}")))?;
    match temporary.persist_noclobber(&path) {
        Ok(_) => Ok(()),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
            read(&path, frame)?.ok_or_else(|| error("Missing concurrent cache publication"))?;
            Ok(())
        }
        Err(e) => Err(error(format!("Cannot publish verified media cache file: {}", e.error))),
    }
}
fn load(root: &Path, ticket: Ticket, display: bool, downloads: &mut BTreeMap<AssetHash, Download>, events: &SyncSender<Event>) -> Result<()> {
    if let Some(download) = downloads.get_mut(&ticket.frame.hash) {
        if download.ticket.scope != ticket.scope || !same_frame(&download.ticket.frame, &ticket.frame) {
            return Err(error("Conflicting active media request"));
        }
        download.display |= display;
        return Ok(());
    }
    if let Some(bytes) = read(&cache_path(root, &ticket.scope, &ticket.frame)?, &ticket.frame)? {
        if display {
            events.send(Event::Ready { scope: ticket.scope, frame: ticket.frame, bytes: Arc::new(bytes) }).map_err(error)?;
        }
        return Ok(());
    }
    if downloads.len() >= MAX_DOWNLOADS {
        return Err(error("Too many concurrent media downloads; retry shortly"));
    }
    events.send(Event::Need(ticket.clone())).map_err(error)?;
    downloads.insert(ticket.frame.hash, Download { ticket, bytes: Vec::new(), display });
    Ok(())
}
fn chunk(root: &Path, ticket: Ticket, bytes: Vec<u8>, total: u64, downloads: &mut BTreeMap<AssetHash, Download>, events: &SyncSender<Event>) -> Result<()> {
    let download = downloads.get_mut(&ticket.frame.hash).ok_or_else(|| error("Unsolicited media chunk"))?;
    if download.ticket.scope != ticket.scope
        || !same_frame(&download.ticket.frame, &ticket.frame)
        || ticket.offset != download.bytes.len() as u64
        || total != ticket.frame.byte_len
        || bytes.is_empty()
        || bytes.len() > photocraft_party::protocol::CHUNK_BYTES
        || ticket.offset.saturating_add(bytes.len() as u64) > total
    {
        return Err(error("Media chunk has a wrong scope, offset, size or immutable total"));
    }
    download.bytes.extend_from_slice(&bytes);
    if download.bytes.len() as u64 == total {
        let download = downloads.remove(&ticket.frame.hash).ok_or_else(|| error("Missing completed media download"))?;
        world_media::decode_frame(&ticket.frame, &download.bytes)?;
        save(root, &ticket.scope, &ticket.frame, &download.bytes)?;
        if download.display {
            events.send(Event::Ready { scope: ticket.scope, frame: ticket.frame, bytes: Arc::new(download.bytes) }).map_err(error)?;
        }
    } else {
        let mut next = ticket;
        next.offset = download.bytes.len() as u64;
        download.ticket = next.clone();
        events.send(Event::Need(next)).map_err(error)?;
    }
    Ok(())
}
fn prepare_job(mut job: Job) -> Result<PendingJob> {
    job.world.validate().map_err(error)?;
    if job.world.canvas != job.scope.canvas || job.camera.canvas != job.scope.canvas {
        return Err(error("Export belongs to another canvas"));
    }
    if matches!(job.action, WorldExport::View { .. })
        && let Some(revision) = job.view_revision
    {
        job.world = Arc::new(job.world.replay_to(revision).map_err(error)?);
    }
    match &job.action {
        WorldExport::View { width, height, .. } => {
            photocraft_scene::coordinates::validate_view(&Viewport { camera: job.camera.clone(), width: *width, height: *height }).map_err(error)?;
            if *width > world_media::MAX_RENDER_SIDE || *height > world_media::MAX_RENDER_SIDE {
                return Err(error("World view export exceeds 2048 pixels"));
            }
        }
        WorldExport::TourGif { tour, width, height, fps, .. } | WorldExport::TourSequence { tour, width, height, fps, .. } => {
            FrozenTour::new(&job.world, *tour, ExportSettings { width: *width, height: *height, fps: *fps }).map_err(error)?;
        }
        WorldExport::Archive { .. } => {}
        WorldExport::Restore { .. } => return Err(error("Restore requires the independent archive upload worker")),
    }
    let mut frames = BTreeMap::<AssetHash, MediaFrame>::new();
    let mut total = 0u64;
    let selected = if let WorldExport::View { width, height, elapsed_ms, .. } = &job.action {
        let mut renderer = world_media::WorldRenderer::new(&job.world)?;
        let view = Viewport { camera: job.camera.clone(), width: *width, height: *height };
        let mut frames = Vec::new();
        for object in renderer.visible_content(&view)? {
            if let world_media::VisibleObject::Media(media) = object {
                frames.push(media.media.frame_at(*elapsed_ms).map_err(error)?.1.clone());
            }
        }
        Some(frames)
    } else {
        None
    };
    let candidates: Vec<_> = if let Some(selected) = selected {
        selected
    } else {
        job.world
            .media
            .values()
            .filter(|media| media.visible || matches!(job.action, WorldExport::Archive { .. }))
            .flat_map(|media| media.media.frames.iter().cloned())
            .collect()
    };
    for frame in &candidates {
        if let Some(old) = frames.get(&frame.hash) {
            if !same_frame(old, frame) {
                return Err(error("Conflicting accepted media descriptors"));
            }
        } else {
            total = total.saturating_add(frame.byte_len);
            if total > MAX_CACHE_BYTES || frames.len() >= MAX_CACHE_FILES {
                return Err(error("Export media exceeds the bounded cache capacity"));
            }
            frames.insert(frame.hash, frame.clone());
        }
    }
    Ok(PendingJob { job, frames: frames.into_values().collect(), next: 0 })
}
fn advance(
    root: &Path,
    pending: &mut PendingJob,
    downloads: &mut BTreeMap<AssetHash, Download>,
    events: &SyncSender<Event>,
    generation: &AtomicU64,
) -> Result<bool> {
    let job = &pending.job;
    if !alive(job, generation) {
        return Err(error("Export cancelled; saved artwork is unchanged"));
    }
    while let Some(frame) = pending.frames.get(pending.next) {
        if read(&cache_path(root, &job.scope, frame)?, frame)?.is_none() {
            load(root, Ticket { scope: job.scope.clone(), frame: frame.clone(), offset: 0 }, false, downloads, events)?;
            events
                .send(Event::Progress {
                    scope: job.scope.clone(),
                    status: ExportStatus {
                        busy: true,
                        completed: pending.next as u32,
                        total: pending.frames.len() as u32,
                        message: "Downloading verified export media…".into(),
                    },
                })
                .map_err(error)?;
            return Ok(false);
        }
        pending.next = pending.next.saturating_add(1);
    }
    let mut prepared: Option<Box<dyn PreparedWorldFiles>> = match &job.files {
        Some(files) => Some(files.prepare(&job.action)?),
        None => None,
    };
    let action = prepared.as_ref().map_or_else(|| job.action.clone(), |files| files.action());
    if action.clone().with_path(PathBuf::new()) != job.action.clone().with_path(PathBuf::new()) {
        return Err(error("File capability adapter changed the requested export"));
    }
    let mut resolve = |frame: &MediaFrame| {
        if !alive(job, generation) {
            return Err(error("Export cancelled"));
        }
        read(&cache_path(root, &job.scope, frame)?, frame)?.ok_or_else(|| error("Export media is not downloaded"))
    };
    let mut progress = |completed, total| {
        alive(job, generation)
            && events
                .send(Event::Progress {
                    scope: job.scope.clone(),
                    status: ExportStatus { busy: true, completed, total, message: format!("Exporting frozen revision {}…", job.world.revision) },
                })
                .is_ok()
    };
    if !alive(job, generation) {
        return Err(error("Export cancelled"));
    }
    match action {
        WorldExport::View { path, width, height, elapsed_ms } => {
            let view = Viewport { camera: job.camera.clone(), width, height };
            let mut renderer = world_media::WorldRenderer::new(&job.world)?;
            let image = renderer.render_with(&view, elapsed_ms, &mut resolve, &mut || alive(job, generation))?;
            let encoded = world_media::encode_png(&image)?;
            if !alive(job, generation) {
                return Err(error("Export cancelled"));
            }
            crate::gallery_media::write_new_file(&path, &encoded)?;
        }
        WorldExport::TourGif { path, tour, width, height, fps, looping } => {
            let frozen = FrozenTour::new(&job.world, tour, ExportSettings { width, height, fps }).map_err(error)?;
            world_media::export_tour_gif(&frozen, &path, looping, &mut resolve, &mut progress)?;
        }
        WorldExport::TourSequence { path, tour, width, height, fps } => {
            let frozen = FrozenTour::new(&job.world, tour, ExportSettings { width, height, fps }).map_err(error)?;
            world_media::export_tour_sequence(&frozen, &path, &mut resolve, &mut progress)?;
        }
        WorldExport::Archive { path } => world_media::export_archive(&job.world, &path, &mut resolve, &mut progress)?,
        WorldExport::Restore { .. } => return Err(error("Restore requires the independent archive upload worker")),
    }
    if let Some(files) = &mut prepared {
        if !alive(job, generation) {
            return Err(error("Export cancelled before publication"));
        }
        files.finish()?;
    }
    events
        .send(Event::Progress {
            scope: job.scope.clone(),
            status: ExportStatus { busy: false, completed: 1, total: 1, message: format!("Exported accepted revision {}", job.world.revision) },
        })
        .map_err(error)?;
    Ok(true)
}

#[cfg(test)]
mod tests;
