use super::{Thumbnail, error};
use crate::{Result, world_media};
use photocraft_room_client::{world_queue::WorldScope, world_worker::WorldAccess};
use photocraft_scene::{CanvasId, Viewport, shared::WorldState, world_protocol::CanvasSummary};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};

const WIDTH: u32 = 160;
const HEIGHT: u32 = 100;
const CACHE_SCHEMA: u32 = 2;
const MEMORY_ITEMS: usize = 32;
const JOBS: usize = 8;
const DISK_ITEMS: usize = 256;
const DISK_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CACHE_FILE: u64 = 512 * 1024;
const MEDIA_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Key {
    scope: WorldScope,
    info: CanvasSummary,
}
impl Key {
    fn id(&self) -> Result<String> {
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(self).map_err(error)?)))
    }
}
struct Job {
    key: Key,
    generation: u64,
    access: WorldAccess,
    source: Option<Arc<WorldState>>,
}
struct Completed {
    key: Key,
    generation: u64,
    result: std::result::Result<Thumbnail, String>,
}
#[derive(Default)]
struct Mailbox {
    generation: u64,
    allowed: BTreeMap<CanvasId, Key>,
    requests: VecDeque<Job>,
    completed: VecDeque<Completed>,
    closed: bool,
}
#[derive(Default)]
struct Shared {
    mailbox: Mutex<Mailbox>,
    wake: Condvar,
}
fn lock(shared: &Shared) -> MutexGuard<'_, Mailbox> {
    shared.mailbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn current(shared: &Shared, key: &Key, generation: u64) -> bool {
    let state = lock(shared);
    !state.closed && state.generation == generation && state.allowed.get(&key.info.canvas) == Some(key)
}
struct Worker {
    shared: Arc<Shared>,
}
impl Worker {
    fn start(path: PathBuf, certificate: Option<Vec<u8>>) -> Result<Self> {
        let shared = Arc::new(Shared::default());
        let thread = Arc::clone(&shared);
        std::thread::Builder::new().name("komaphone-world-thumbnails".into()).spawn(move || work(thread, path, certificate)).map_err(error)?;
        Ok(Self { shared })
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let mut state = lock(&self.shared);
        state.closed = true;
        state.requests.clear();
        self.shared.wake.notify_one();
    }
}
#[derive(Default)]
pub(super) struct State {
    worker: Option<Worker>,
    access: Option<WorldAccess>,
    catalog: BTreeMap<CanvasId, Key>,
    memory: BTreeMap<CanvasId, (Key, Arc<Thumbnail>)>,
    lru: VecDeque<CanvasId>,
    pending: BTreeMap<CanvasId, Key>,
    failures: BTreeMap<CanvasId, (Instant, String)>,
}
impl State {
    pub fn start(&mut self, path: PathBuf, certificate: Option<Vec<u8>>) -> Result<()> {
        if self.worker.is_some() {
            return Err(error("Canvas thumbnails already configured"));
        }
        self.worker = Some(Worker::start(path, certificate)?);
        Ok(())
    }
    /// Clears all presentation and cancels work. The separately managed opaque
    /// credential handoff survives the Session event's presentation reset.
    pub fn reset(&mut self) {
        self.catalog.clear();
        self.memory.clear();
        self.lru.clear();
        self.pending.clear();
        self.failures.clear();
        if let Some(worker) = &self.worker {
            let mut state = lock(&worker.shared);
            state.generation = state.generation.saturating_add(1);
            state.allowed.clear();
            state.requests.clear();
            state.completed.clear();
        }
    }
    pub fn access(&mut self, access: Option<&WorldAccess>) {
        if self.access.as_ref().map(WorldAccess::scope) != access.map(WorldAccess::scope) {
            self.reset();
        }
        self.access = access.map(WorldAccess::copy_for_worker);
    }
    fn reconcile(&mut self) {
        self.memory.retain(|canvas, (key, _)| self.catalog.get(canvas) == Some(key));
        self.pending.retain(|canvas, key| self.catalog.get(canvas) == Some(key));
        self.failures.retain(|canvas, _| self.catalog.contains_key(canvas));
        self.lru.retain(|canvas| self.memory.contains_key(canvas));
        if let Some(worker) = &self.worker {
            let mut state = lock(&worker.shared);
            state.allowed.clone_from(&self.catalog);
            state.requests.retain(|job| self.catalog.get(&job.key.info.canvas) == Some(&job.key));
            state.completed.retain(|job| self.catalog.get(&job.key.info.canvas) == Some(&job.key));
            worker.shared.wake.notify_one();
        }
    }
    pub fn catalog(&mut self, items: &[CanvasSummary]) {
        let Some(access) = &self.access else {
            self.reset();
            return;
        };
        self.catalog = items.iter().take(16_384).map(|info| (info.canvas, Key { scope: access.scope().clone(), info: info.clone() })).collect();
        self.reconcile();
    }
    pub fn observe(&mut self, info: &CanvasSummary) {
        if let Some(key) = self.catalog.get_mut(&info.canvas)
            && key.info.epoch == info.epoch
            && key.info.revision < info.revision
            && key.info.owner == info.owner
        {
            key.info = info.clone();
            let key = key.clone();
            self.memory.remove(&info.canvas);
            self.pending.remove(&info.canvas);
            self.failures.remove(&info.canvas);
            self.lru.retain(|canvas| *canvas != info.canvas);
            if let Some(worker) = &self.worker {
                let mut state = lock(&worker.shared);
                state.allowed.insert(info.canvas, key.clone());
                state.requests.retain(|job| job.key.info.canvas != info.canvas || job.key == key);
                state.completed.retain(|job| job.key.info.canvas != info.canvas || job.key == key);
                worker.shared.wake.notify_one();
            }
        }
    }
    pub fn contains(&self, canvas: CanvasId) -> bool {
        self.catalog.contains_key(&canvas)
    }
    pub fn remove(&mut self, canvas: CanvasId) {
        self.catalog.remove(&canvas);
        self.reconcile();
    }
    pub fn error(&self, canvas: CanvasId) -> Option<&str> {
        self.failures.get(&canvas).map(|(_, message)| message.as_str())
    }
    pub fn poll(&mut self) {
        let Some(worker) = &self.worker else { return };
        let (generation, completed) = {
            let mut state = lock(&worker.shared);
            (state.generation, std::mem::take(&mut state.completed))
        };
        worker.shared.wake.notify_one();
        for item in completed {
            let canvas = item.key.info.canvas;
            if item.generation != generation || self.catalog.get(&canvas) != Some(&item.key) {
                continue;
            }
            if self.pending.get(&canvas) == Some(&item.key) {
                self.pending.remove(&canvas);
            }
            match item.result {
                Ok(image) => {
                    self.failures.remove(&canvas);
                    self.lru.retain(|id| *id != canvas);
                    self.lru.push_back(canvas);
                    self.memory.insert(canvas, (item.key, Arc::new(image)));
                    while self.memory.len() > MEMORY_ITEMS {
                        if let Some(old) = self.lru.pop_front() {
                            self.memory.remove(&old);
                        } else {
                            break;
                        }
                    }
                }
                Err(message) => {
                    self.failures.insert(canvas, (Instant::now(), message));
                }
            }
        }
    }
    pub fn get(&mut self, canvas: CanvasId, source: Option<Arc<WorldState>>) -> Result<Option<Arc<Thumbnail>>> {
        let Some(key) = self.catalog.get(&canvas).cloned() else { return Ok(None) };
        if let Some((stored, image)) = self.memory.get(&canvas)
            && *stored == key
        {
            self.lru.retain(|id| *id != canvas);
            self.lru.push_back(canvas);
            return Ok(Some(Arc::clone(image)));
        }
        if self.pending.get(&canvas) == Some(&key) || self.failures.get(&canvas).is_some_and(|(at, _)| at.elapsed() < Duration::from_secs(10)) {
            return Ok(None);
        }
        let (Some(worker), Some(access)) = (&self.worker, &self.access) else { return Ok(None) };
        let mut state = lock(&worker.shared);
        if state.requests.len() >= JOBS || state.completed.len() >= JOBS {
            return Ok(None);
        }
        let generation = state.generation;
        state.requests.push_back(Job { key: key.clone(), generation, access: access.copy_for_worker(), source });
        self.pending.insert(canvas, key);
        worker.shared.wake.notify_one();
        Ok(None)
    }
}
fn work(shared: Arc<Shared>, root: PathBuf, certificate: Option<Vec<u8>>) {
    loop {
        let job = {
            let mut state = lock(&shared);
            while (state.requests.is_empty() || state.completed.len() >= JOBS) && !state.closed {
                state = shared.wake.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if state.closed {
                return;
            }
            let Some(job) = state.requests.pop_front() else { continue };
            job
        };
        if !current(&shared, &job.key, job.generation) {
            continue;
        }
        let result = prepare(&root, &shared, &job, certificate.as_deref()).map_err(|e| e.to_string());
        let mut state = lock(&shared);
        if !state.closed && state.generation == job.generation && state.allowed.get(&job.key.info.canvas) == Some(&job.key) {
            // Completed outcomes are small decoded previews; the UI drains this
            // bounded mailbox, never a whole scene or a secret-bearing job.
            if state.completed.len() < JOBS {
                state.completed.push_back(Completed { key: job.key, generation: job.generation, result });
            }
        }
    }
}
fn prepare(root: &Path, shared: &Shared, job: &Job, certificate: Option<&[u8]>) -> Result<Thumbnail> {
    let mut alive = || current(shared, &job.key, job.generation);
    if !alive() {
        return Err(error("Thumbnail cancelled"));
    }
    std::fs::create_dir_all(root).map_err(error)?;
    let path = root.join(format!("{}.thumb", job.key.id()?));
    match read_cache(&path, &job.key) {
        Ok(Some(image)) => return Ok(image),
        Ok(None) => {}
        Err(_) => {
            std::fs::remove_file(&path).map_err(error)?;
        }
    }
    let reader = job.access.thumbnail_reader(certificate).map_err(error)?;
    let loaded;
    let source = match job
        .source
        .as_ref()
        .filter(|world| world.canvas == job.key.info.canvas && world.revision == job.key.info.revision && world.owner == job.key.info.owner)
    {
        Some(world) => world.as_ref(),
        None => {
            loaded = reader.scene(&job.key.info, &mut alive).map_err(error)?;
            &loaded
        }
    };
    if source.name != job.key.info.name || source.replay_to(0).map_err(error)?.name != job.key.info.initial_name {
        return Err(error("Thumbnail source identity differs from its catalog"));
    }
    let mut bytes = 0u64;
    let thumbnail = render_scene(
        source,
        &mut |frame| {
            bytes = bytes.saturating_add(frame.byte_len);
            if bytes > MEDIA_BYTES {
                return Err(error("Thumbnail media exceeds its 32 MiB preview budget; open the canvas"));
            }
            reader.frame(source.canvas, frame, &mut || current(shared, &job.key, job.generation)).map_err(error)
        },
        &mut alive,
    )?;
    write_cache(root, &path, &job.key, &thumbnail)?;
    if !alive() {
        return Err(error("Thumbnail scope changed before publication"));
    }
    Ok(thumbnail)
}
fn render_scene(
    source: &WorldState,
    resolve: &mut dyn FnMut(&photocraft_scene::shared::media::MediaFrame) -> Result<Vec<u8>>,
    alive: &mut dyn FnMut() -> bool,
) -> Result<Thumbnail> {
    let mut renderer = world_media::WorldRenderer::new(source)?;
    let mut camera = photocraft_scene::coordinates::home(source.canvas);
    camera.zoom = photocraft_scene::coordinates::MIN_ROOT_ZOOM;
    let mut view = Viewport { camera, width: 640, height: 400 };
    if !visible_at_thumbnail_scale(&mut renderer, &view)? {
        let target = source
            .strokes
            .values()
            .find(|stroke| stroke.visible && !stroke.stroke.erase && stroke.stroke.effective_opacity() > 0.0)
            .and_then(|stroke| stroke.stroke.points.first().map(|point| (stroke.stroke.anchor, *point)))
            .or_else(|| source.media.values().find(|media| media.visible).map(|media| (media.media.anchor, media.media.offset)));
        if let Some((region, local)) = target {
            let point = photocraft_scene::coordinates::point(&region, local).map_err(error)?;
            view.camera = photocraft_scene::coordinates::target(source.canvas, point, region.depth, 1.0).map_err(error)?;
        }
    }
    let image = renderer.render_with(&view, 0, resolve, alive)?;
    if !alive() {
        return Err(error("Thumbnail scope changed"));
    }
    let full = image::RgbaImage::from_raw(image.width, image.height, world_media::rgba8(&image)?).ok_or_else(|| error("Thumbnail pixel shape"))?;
    let rgba = image::imageops::resize(&full, WIDTH, HEIGHT, image::imageops::FilterType::Triangle).into_raw();
    Ok(Thumbnail { width: WIDTH, height: HEIGHT, rgba, revision: source.revision })
}

/// An index candidate is not necessarily visible in a small preview: deep art
/// can intersect Home while occupying much less than one output pixel.
fn visible_at_thumbnail_scale(renderer: &mut world_media::WorldRenderer, view: &Viewport) -> Result<bool> {
    for object in renderer.visible_content(view)? {
        match object {
            world_media::VisibleObject::Stroke(shared) => {
                if shared.stroke.erase || shared.stroke.effective_opacity() == 0.0 {
                    continue;
                }
                let Some(projected) = photocraft_scene::render::project(&shared.stroke, view).map_err(error)? else { continue };
                let reach = shared.stroke.brush.as_ref().map_or(1.0, |brush| brush.reach_factor());
                for (shapes, padding) in [(&projected.paths, f64::from(projected.width) * 0.5 * reach), (&projected.fills, 0.0)] {
                    let mut min = [f64::INFINITY; 2];
                    let mut max = [f64::NEG_INFINITY; 2];
                    for &[x, y] in shapes.iter().flatten() {
                        min[0] = min[0].min(f64::from(x));
                        min[1] = min[1].min(f64::from(y));
                        max[0] = max[0].max(f64::from(x));
                        max[1] = max[1].max(f64::from(y));
                    }
                    if thumbnail_footprint(min, max, padding, view) {
                        return Ok(true);
                    }
                }
            }
            world_media::VisibleObject::Media(shared) => {
                let media = &shared.media;
                let start =
                    photocraft_scene::coordinates::point_to_screen(view, photocraft_scene::coordinates::point(&media.anchor, media.offset).map_err(error)?)
                        .map_err(error)?;
                let end = photocraft_scene::coordinates::point_to_screen(
                    view,
                    photocraft_scene::coordinates::point(&media.anchor, [media.offset[0] + media.width, media.offset[1] + media.height]).map_err(error)?,
                )
                .map_err(error)?;
                if thumbnail_footprint(start, end, 0.0, view) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}
fn thumbnail_footprint(min: [f64; 2], max: [f64; 2], padding: f64, view: &Viewport) -> bool {
    if min.into_iter().chain(max).any(|value| !value.is_finite()) {
        return false;
    }
    let width = ((max[0] + padding).min(f64::from(view.width)) - (min[0] - padding).max(0.0)).max(0.0) * f64::from(WIDTH) / f64::from(view.width);
    let height = ((max[1] + padding).min(f64::from(view.height)) - (min[1] - padding).max(0.0)).max(0.0) * f64::from(HEIGHT) / f64::from(view.height);
    // A tiny indexed derivative should trigger native-depth framing, rather
    // than turning a canvas that contains drawing into an apparently blank card.
    width.max(height) >= 2.0 && width * height >= 2.0
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cache {
    schema: u32,
    key: Key,
    hash: String,
    png: Vec<u8>,
}
fn read_cache(path: &Path, key: &Key) -> Result<Option<Thumbnail>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(error(e)),
    };
    let mut bytes = Vec::new();
    file.take(MAX_CACHE_FILE + 1).read_to_end(&mut bytes).map_err(error)?;
    if bytes.len() as u64 > MAX_CACHE_FILE {
        return Err(error("Thumbnail cache exceeds its file limit"));
    }
    let saved: Cache = serde_json::from_slice(&bytes).map_err(error)?;
    if saved.schema != CACHE_SCHEMA || saved.key != *key || saved.png.len() > 128 * 1024 || saved.hash != format!("{:x}", Sha256::digest(&saved.png)) {
        return Err(error("Thumbnail cache identity/checksum mismatch"));
    }
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(&saved.png), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(WIDTH);
    limits.max_image_height = Some(HEIGHT);
    limits.max_alloc = Some(1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(error)?.into_rgba8();
    if image.width() != WIDTH || image.height() != HEIGHT {
        return Err(error("Thumbnail cache dimensions"));
    }
    Ok(Some(Thumbnail { width: WIDTH, height: HEIGHT, rgba: image.into_raw(), revision: key.info.revision }))
}
fn write_cache(root: &Path, path: &Path, key: &Key, image: &Thumbnail) -> Result<()> {
    use image::ImageEncoder;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png).write_image(&image.rgba, WIDTH, HEIGHT, image::ExtendedColorType::Rgba8).map_err(error)?;
    let hash = format!("{:x}", Sha256::digest(&png));
    let bytes = serde_json::to_vec(&Cache { schema: CACHE_SCHEMA, key: key.clone(), hash, png }).map_err(error)?;
    if bytes.len() as u64 > MAX_CACHE_FILE {
        return Err(error("Thumbnail encoded cache limit"));
    }
    make_room(root, bytes.len() as u64)?;
    let mut temporary = tempfile::NamedTempFile::new_in(root).map_err(error)?;
    temporary.write_all(&bytes).map_err(error)?;
    temporary.as_file().sync_all().map_err(error)?;
    match temporary.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(error(e.error)),
    }
}
fn make_room(root: &Path, incoming: u64) -> Result<()> {
    let mut files = Vec::new();
    let mut bytes = 0u64;
    for (count, entry) in std::fs::read_dir(root).map_err(error)?.enumerate() {
        if count >= 1024 {
            return Err(error("Thumbnail cache directory inventory limit"));
        }
        let entry = entry.map_err(error)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.strip_suffix(".thumb").is_some_and(|stem| stem.len() == 64 && stem.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))) {
            continue;
        }
        if !entry.file_type().map_err(error)?.is_file() {
            continue;
        }
        let metadata = entry.metadata().map_err(error)?;
        bytes = bytes.saturating_add(metadata.len());
        files.push((metadata.modified().map_err(error)?, entry.path(), metadata.len()));
    }
    files.sort_by_key(|(modified, _, _)| *modified);
    let mut count = files.len();
    for (_, path, length) in files {
        if count < DISK_ITEMS && bytes.saturating_add(incoming) <= DISK_BYTES {
            break;
        }
        std::fs::remove_file(path).map_err(error)?;
        bytes = bytes.saturating_sub(length);
        count = count.saturating_sub(1);
    }
    if bytes.saturating_add(incoming) > DISK_BYTES {
        return Err(error("Thumbnail cache capacity"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
