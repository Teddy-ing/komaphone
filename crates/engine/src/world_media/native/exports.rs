use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use photocraft_scene::{
    Viewport,
    shared::{
        WorldState,
        media::{AssetHash, MediaFrame},
        tours::FrozenTour,
    },
};
use serde::{Deserialize, Serialize};

use super::{WorldRenderer, decode_frame, encode_png, error, other, rgba8};
use crate::Result;

const MAX_EXPORT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ARCHIVE_MANIFEST: u64 = 32 * 1024 * 1024;

fn parent(path: &Path) -> Result<&Path> {
    if path.try_exists().map_err(other)? {
        return Err(error("Choose a new output path; existing files are preserved"));
    }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(error("Choose an existing parent output folder"));
    }
    Ok(parent)
}
fn check_progress(progress: &mut dyn FnMut(u32, u32) -> bool, index: u32, total: u32) -> Result<()> {
    if !progress(index, total) {
        return Err(error("Export cancelled; saved artwork is unchanged"));
    }
    Ok(())
}
fn publish_directory(staging: &Path, destination: &Path) -> Result<()> {
    // The Windows product path uses MoveFileEx without replacement. On other
    // systems fail closed rather than racing std::fs::rename's replacement rule.
    #[cfg(windows)]
    {
        crate::gallery_media::publish_new_directory(staging, destination)
    }
    #[cfg(not(windows))]
    {
        let _ = (staging, destination);
        Err(error("Atomic directory publication is currently supported on Windows; choose a single-file export on this platform"))
    }
}

pub fn export_view_png(
    world: &WorldState,
    view: &Viewport,
    elapsed_ms: u64,
    path: &Path,
    resolve: &mut dyn FnMut(&MediaFrame) -> Result<Vec<u8>>,
) -> Result<()> {
    parent(path)?;
    let image = super::render_view(world, view, elapsed_ms, resolve)?;
    crate::gallery_media::write_new_file(path, &encode_png(&image)?)
}

pub fn export_tour_sequence(
    frozen: &FrozenTour,
    path: &Path,
    resolve: &mut dyn FnMut(&MediaFrame) -> Result<Vec<u8>>,
    mut progress: impl FnMut(u32, u32) -> bool,
) -> Result<Vec<PathBuf>> {
    let staging = tempfile::tempdir_in(parent(path)?).map_err(other)?;
    let mut renderer = WorldRenderer::new(frozen.world())?;
    let count = frozen.frame_count();
    let mut names = Vec::with_capacity(count as usize);
    let mut bytes = 0_u64;
    for index in 0..count {
        check_progress(&mut progress, index, count)?;
        let frame = frozen.frame(index).map_err(other)?;
        let view = Viewport { camera: frame.camera, width: frozen.settings().width, height: frozen.settings().height };
        let time = u64::from(index) * 1000 / u64::from(frozen.settings().fps);
        let image = renderer.render_with(&view, time, resolve, &mut || progress(index, count))?;
        let encoded = encode_png(&image)?;
        bytes = bytes.saturating_add(encoded.len() as u64);
        if bytes > MAX_EXPORT_BYTES {
            return Err(error("PNG sequence exceeds the 512 MiB output budget; lower resolution or duration"));
        }
        let name = format!("frame-{index:04}.png");
        crate::gallery_media::write_new_file(&staging.path().join(&name), &encoded)?;
        names.push(name);
    }
    let metadata = serde_json::json!({"schema":1,"canvas":frozen.world().canvas,"world_revision":frozen.world().revision,"world_hash":frozen.scene_hash(),
        "tour":frozen.tour(),"fps":frozen.settings().fps,"frames":count,"width":frozen.settings().width,"height":frozen.settings().height});
    crate::gallery_media::write_new_file(&staging.path().join("tour.json"), &serde_json::to_vec_pretty(&metadata).map_err(other)?)?;
    check_progress(&mut progress, count, count)?;
    publish_directory(staging.path(), path)?;
    Ok(names.into_iter().map(|name| path.join(name)).collect())
}

struct BoundedWriter<'a> {
    file: &'a mut File,
    written: u64,
    failed: Arc<AtomicBool>,
}
impl Write for BoundedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.written.saturating_add(bytes.len() as u64) > MAX_EXPORT_BYTES {
            self.failed.store(true, Ordering::Relaxed);
            return Err(std::io::Error::other("GIF output exceeds 512 MiB"));
        }
        let count = match self.file.write(bytes) {
            Ok(count) => count,
            Err(error) => {
                self.failed.store(true, Ordering::Relaxed);
                return Err(error);
            }
        };
        if count == 0 && !bytes.is_empty() {
            self.failed.store(true, Ordering::Relaxed);
        }
        self.written += count as u64;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush().inspect_err(|_| {
            self.failed.store(true, Ordering::Relaxed);
        })
    }
}

/// GIF is encoded incrementally: only the current scene/image/frame is resident.
/// Palette and 10ms timing limits are the same documented GIF limitations as K4.
pub fn export_tour_gif(
    frozen: &FrozenTour,
    path: &Path,
    looping: bool,
    resolve: &mut dyn FnMut(&MediaFrame) -> Result<Vec<u8>>,
    mut progress: impl FnMut(u32, u32) -> bool,
) -> Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(parent(path)?).map_err(other)?;
    let mut renderer = WorldRenderer::new(frozen.world())?;
    let count = frozen.frame_count();
    // The underlying GIF writer emits its final trailer in Drop. Preserve an I/O
    // failure from that path even if the dependency cannot return it to us.
    let failed = Arc::new(AtomicBool::new(false));
    {
        let writer = BoundedWriter { file: temporary.as_file_mut(), written: 0, failed: Arc::clone(&failed) };
        let mut encoder = image::codecs::gif::GifEncoder::new_with_speed(writer, 10);
        if looping {
            encoder.set_repeat(image::codecs::gif::Repeat::Infinite).map_err(other)?;
        }
        for index in 0..count {
            check_progress(&mut progress, index, count)?;
            let frame = frozen.frame(index).map_err(other)?;
            let view = Viewport { camera: frame.camera, width: frozen.settings().width, height: frozen.settings().height };
            let time = u64::from(index) * 1000 / u64::from(frozen.settings().fps);
            let image = renderer.render_with(&view, time, resolve, &mut || progress(index, count))?;
            let rgba = image::RgbaImage::from_raw(image.width, image.height, rgba8(&image)?).ok_or_else(|| error("Invalid tour output image"))?;
            let delay = image::Delay::from_numer_denom_ms(1000, frozen.settings().fps);
            encoder.encode_frame(image::Frame::from_parts(rgba, 0, 0, delay)).map_err(other)?;
        }
        check_progress(&mut progress, count, count)?;
    }
    if failed.load(Ordering::Relaxed) {
        return Err(error("GIF finalization failed; the incomplete temporary output was not published"));
    }
    temporary.as_file().sync_all().map_err(other)?;
    temporary.persist_noclobber(path).map_err(|e| other(e.error))?;
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArchiveManifest {
    schema: u32,
    world: WorldState,
    assets: Vec<MediaFrame>,
}

/// Portable originals, accepted history, tours, credits, and immutable assets.
/// No live membership, invitations, account credentials, or local review state.
pub fn export_archive(
    world: &WorldState,
    path: &Path,
    resolve: &mut dyn FnMut(&MediaFrame) -> Result<Vec<u8>>,
    mut progress: impl FnMut(u32, u32) -> bool,
) -> Result<()> {
    world.validate().map_err(other)?;
    world.validate_complete_import().map_err(other)?;
    let mut assets = BTreeMap::<AssetHash, MediaFrame>::new();
    for media in world.media.values() {
        for frame in &media.media.frames {
            if let Some(previous) = assets.get(&frame.hash) {
                if previous.byte_len != frame.byte_len || previous.width != frame.width || previous.height != frame.height {
                    return Err(error("Conflicting references to one immutable asset"));
                }
            } else {
                assets.insert(frame.hash, frame.clone());
            }
        }
    }
    let staging = tempfile::tempdir_in(parent(path)?).map_err(other)?;
    let total = u32::try_from(assets.len()).map_err(other)?;
    let mut bytes = 0_u64;
    for (index, frame) in assets.values().enumerate() {
        check_progress(&mut progress, index as u32, total)?;
        let data = resolve(frame)?;
        decode_frame(frame, &data)?;
        bytes = bytes.saturating_add(data.len() as u64);
        if bytes > MAX_EXPORT_BYTES {
            return Err(error("World archive exceeds 512 MiB; saved world is unchanged"));
        }
        crate::gallery_media::write_new_file(&staging.path().join(format!("{}.png", frame.hash.as_str())), &data)?;
    }
    let schema = if world.schema >= photocraft_scene::shared::SHARED_SCHEMA_BRUSH { 2 } else { 1 };
    let manifest = serde_json::to_vec(&ArchiveManifest { schema, world: world.clone(), assets: assets.into_values().collect() }).map_err(other)?;
    if manifest.len() as u64 > MAX_ARCHIVE_MANIFEST || bytes.saturating_add(manifest.len() as u64) > MAX_EXPORT_BYTES {
        return Err(error("World archive manifest or total exceeds its output budget"));
    }
    crate::gallery_media::write_new_file(&staging.path().join("world.json"), &manifest)?;
    check_progress(&mut progress, total, total)?;
    publish_directory(staging.path(), path)
}

/// Validate a portable archive for read-only viewing or an explicit owner-only
/// copy operation. Returned historical IDs confer no live service authority.
pub fn read_archive(path: &Path) -> Result<(WorldState, BTreeMap<AssetHash, Vec<u8>>)> {
    read_archive_impl(path, true)
}
/// Validate every image while retaining only the accepted scene. Restore does
/// not need a second in-memory copy of the archive's entire image inventory.
pub fn inspect_archive(path: &Path) -> Result<WorldState> {
    Ok(read_archive_impl(path, false)?.0)
}

/// Validated portable input for a streamed import. Only one encoded image is
/// retained at a time; historical identifiers never grant service authority.
pub struct ArchiveUpload {
    pub world: WorldState,
    pub assets: Vec<MediaFrame>,
    pub manifest_hash: AssetHash,
    pub manifest_bytes: u64,
    pub total_bytes: u64,
}

/// Reject links before an archive can upload files outside its selected folder.
pub fn archive_regular_file(path: &Path) -> Result<File> {
    let metadata = std::fs::symlink_metadata(path).map_err(other)?;
    if !metadata.is_file() || archive_link(&metadata) {
        return Err(error("Archive objects must be regular files, not links"));
    }
    File::open(path).map_err(other)
}
fn archive_link(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}
fn archive_read(path: &Path, limit: u64, proceed: &mut dyn FnMut() -> bool) -> Result<Vec<u8>> {
    let mut file = archive_regular_file(path)?;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        if !proceed() {
            return Err(error("Archive restore cancelled"));
        }
        let read = file.read(&mut chunk).map_err(other)?;
        if read == 0 {
            return Ok(bytes);
        }
        if (bytes.len() as u64).saturating_add(read as u64) > limit {
            return Err(error("Archive object exceeds its declared limit"));
        }
        bytes.extend_from_slice(chunk.get(..read).ok_or_else(|| error("Invalid archive read size"))?);
    }
}

pub fn inspect_archive_for_upload(path: &Path, proceed: &mut dyn FnMut() -> bool) -> Result<ArchiveUpload> {
    use photocraft_scene::shared::archive_import::{ArchiveManifest, MAX_MANIFEST_BYTES, MAX_TOTAL_BYTES};
    use sha2::{Digest, Sha256};
    let metadata = std::fs::symlink_metadata(path).map_err(other)?;
    if !metadata.is_dir() || archive_link(&metadata) {
        return Err(error("Choose a regular archive directory, not a link"));
    }
    let bytes = archive_read(&path.join("world.json"), MAX_MANIFEST_BYTES, proceed)?;
    let manifest = ArchiveManifest::decode(&bytes).map_err(other)?;
    if !proceed() {
        return Err(error("Archive restore cancelled"));
    }
    let manifest_bytes = bytes.len() as u64;
    let manifest_hash = AssetHash::new(&format!("{:x}", Sha256::digest(&bytes))).map_err(other)?;
    let mut total_bytes = manifest_bytes;
    let mut expected = std::collections::BTreeSet::from(["world.json".to_owned()]);
    for frame in &manifest.assets {
        let name = format!("{}.png", frame.hash.as_str());
        expected.insert(name.clone());
        let bytes = archive_read(&path.join(name), frame.byte_len, proceed)?;
        decode_frame(frame, &bytes)?;
        total_bytes = total_bytes.saturating_add(bytes.len() as u64);
        if total_bytes > MAX_TOTAL_BYTES {
            return Err(error("World archive exceeds its import budget"));
        }
    }
    for entry in std::fs::read_dir(path).map_err(other)? {
        if !proceed() {
            return Err(error("Archive restore cancelled"));
        }
        let entry = entry.map_err(other)?;
        let name = entry.file_name().into_string().map_err(|_| error("Invalid archive filename"))?;
        if !expected.remove(&name) {
            return Err(error("Unexpected archive object; keep only the exported manifest and images"));
        }
    }
    if !expected.is_empty() {
        return Err(error("Archive objects changed while reading"));
    }
    Ok(ArchiveUpload { world: manifest.world, assets: manifest.assets, manifest_hash, manifest_bytes, total_bytes })
}
fn read_archive_impl(path: &Path, retain_assets: bool) -> Result<(WorldState, BTreeMap<AssetHash, Vec<u8>>)> {
    let mut bytes = Vec::new();
    File::open(path.join("world.json")).map_err(other)?.take(MAX_ARCHIVE_MANIFEST + 1).read_to_end(&mut bytes).map_err(other)?;
    if bytes.len() as u64 > MAX_ARCHIVE_MANIFEST {
        return Err(error("World archive manifest exceeds 32 MiB"));
    }
    let manifest: ArchiveManifest = serde_json::from_slice(&bytes).map_err(other)?;
    let expected_world = match manifest.schema {
        1 => photocraft_scene::shared::SHARED_SCHEMA,
        2 => photocraft_scene::shared::SHARED_SCHEMA_BRUSH,
        _ => return Err(error("Unsupported world archive schema")),
    };
    manifest.world.validate().map_err(other)?;
    if manifest.world.schema != expected_world {
        return Err(error("Unsupported world archive schema"));
    }
    if manifest.assets.len() > manifest.world.limits().max_media_frames {
        return Err(error("Too many world archive assets"));
    }
    let mut expected = BTreeMap::new();
    for media in manifest.world.media.values() {
        for frame in &media.media.frames {
            let descriptor = (frame.byte_len, frame.width, frame.height);
            if expected.insert(frame.hash, descriptor).is_some_and(|prior| prior != descriptor) {
                return Err(error("Retained content has conflicting immutable asset descriptors"));
            }
        }
    }
    if expected.len() != manifest.assets.len() {
        return Err(error("World archive asset inventory differs from retained content"));
    }
    let mut assets = BTreeMap::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut total = bytes.len() as u64;
    for frame in manifest.assets {
        if expected.get(&frame.hash) != Some(&(frame.byte_len, frame.width, frame.height)) || !seen.insert(frame.hash) {
            return Err(error("World archive asset reference mismatch"));
        }
        frame.validate().map_err(other)?;
        let mut data = Vec::new();
        File::open(path.join(format!("{}.png", frame.hash.as_str()))).map_err(other)?.take(frame.byte_len + 1).read_to_end(&mut data).map_err(other)?;
        total = total.saturating_add(data.len() as u64);
        if total > MAX_EXPORT_BYTES {
            return Err(error("World archive exceeds 512 MiB"));
        }
        decode_frame(&frame, &data)?;
        if retain_assets {
            assets.insert(frame.hash, data);
        }
    }
    Ok((manifest.world, assets))
}
