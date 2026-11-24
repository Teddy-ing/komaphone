use super::{AnimationOptions, ReplayInfo, ReplayMoment};
use crate::rooms::media::RoomImage;
use crate::{EngineError, Result};
use image::{ImageDecoder, ImageEncoder, ImageReader};
use photocraft_doc::{ColorMode, Document, Layer, LayerContent, SampleType, Size, Timeline, VideoData};
use photocraft_party::gallery::{GalleryBundle, GalleryContent, RemixProvenance};
use photocraft_party::replay_artifact::{ReplayArtifact, unpack_checkpoints};
use photocraft_party::room::{RevealedEntry, RoomView};
use photocraft_party::{AssetRef, Mode, Submission, SubmissionId};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

const MAX_PIXELS: u64 = 16_777_216;
const MAX_SEQUENCE_PIXELS: u64 = 32 * 1024 * 1024;
const MAX_FRAMES: usize = 36;

fn error(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}
fn other(message: impl std::fmt::Display) -> EngineError {
    error(message.to_string())
}

fn load_options(preserve_ids: bool) -> photocraft_format::LoadOptions {
    photocraft_format::LoadOptions {
        max_manifest_bytes: 8 * 1024 * 1024,
        max_blob_bytes: 64 * 1024 * 1024,
        max_total_bytes: 256 * 1024 * 1024,
        preserve_ids,
        max_canvas_side: 8192,
        max_canvas_pixels: MAX_PIXELS,
    }
}

fn load(bytes: &[u8], preserve_ids: bool) -> Result<Document> {
    if bytes.len() > photocraft_local_store::MAX_CHECKPOINT_BYTES {
        return Err(error("Gallery checkpoint exceeds 64 MiB"));
    }
    photocraft_format::load_from_bytes_with(bytes, &load_options(preserve_ids)).map_err(other)
}

fn load_copy(bytes: &[u8]) -> Result<Document> {
    // A freshly launched process may have a lower ID counter than this foreign
    // archive. Reserve its bounded, valid source IDs before minting copy IDs.
    drop(load(bytes, true)?);
    load(bytes, false)
}

fn valid_image(width: u32, height: u32, len: usize) -> Result<()> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .filter(|n| *n > 0 && *n <= MAX_PIXELS)
        .ok_or_else(|| error("Gallery image exceeds the 16 megapixel limit"))?;
    if pixels.checked_mul(4) != Some(len as u64) {
        return Err(error("Gallery image has an invalid sample count"));
    }
    Ok(())
}

/// Color-general CPU rendering through the canonical party color conversion.
pub fn render_document(document: &Document) -> Result<RoomImage> {
    let width = document.size.width;
    let height = document.size.height;
    let len = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|n| n.checked_mul(4))
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| error("Gallery canvas size overflow"))?;
    valid_image(width, height, len)?;
    let mut composite = photocraft_compose::flatten(document);
    photocraft_party::canonical_color::to_srgb(document, &mut composite.px).map_err(other)?;
    Ok(RoomImage { width, height, rgba: composite.to_rgba8().pixels })
}

pub fn encode_png(image: &RoomImage) -> Result<Vec<u8>> {
    valid_image(image.width, image.height, image.rgba.len())?;
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes).write_image(&image.rgba, image.width, image.height, image::ExtendedColorType::Rgba8).map_err(other)?;
    Ok(bytes)
}

fn decode_png(bytes: &[u8], expected: (u32, u32)) -> Result<RoomImage> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(expected.0);
    limits.max_image_height = Some(expected.1);
    limits.max_alloc = Some(MAX_PIXELS * 8);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(other)?;
    if decoder.dimensions() != expected || decoder.color_type() != image::ColorType::Rgba8 {
        return Err(error("Gallery image does not match its assigned canvas"));
    }
    let image = image::DynamicImage::from_decoder(decoder).map_err(other)?.into_rgba8();
    Ok(RoomImage { width: expected.0, height: expected.1, rgba: image.into_raw() })
}

fn game(bundle: &GalleryBundle) -> Result<&RoomView> {
    bundle.manifest.validate().map_err(other)?;
    match &bundle.manifest.content {
        GalleryContent::Game { room } => Ok(room),
        _ => Err(error("Select a revealed game")),
    }
}
fn entry(bundle: &GalleryBundle, chain: u8, frame: u8) -> Result<&RevealedEntry> {
    game(bundle)?
        .reveal
        .iter()
        .find(|entry| {
            let assignment = match entry {
                RevealedEntry::Artwork { assignment, .. } | RevealedEntry::Missing { assignment, .. } => assignment,
            };
            assignment.id.chain == chain && assignment.id.round == frame
        })
        .ok_or_else(|| error("Choose an existing revealed frame or creature part"))
}
fn dimensions(mode: Mode) -> (u32, u32) {
    match mode {
        Mode::Animation { .. } => (1536, 1024),
        Mode::ExquisiteCorpse => (1024, 512),
    }
}

fn object(bundle: &GalleryBundle, asset: AssetRef) -> Result<&[u8]> {
    let bytes = bundle.object(asset).map_err(other)?;
    if format!("{:x}", Sha256::digest(bytes)) != asset.hash.as_str() {
        return Err(error("Gallery object checksum failed; the original was not changed"));
    }
    Ok(bytes)
}

fn artifact(bundle: &GalleryBundle, chain: u8, frame: u8) -> Result<Option<ReplayArtifact>> {
    bundle.manifest.validate().map_err(other)?;
    if let GalleryContent::Project { replay, .. } = &bundle.manifest.content {
        if chain != 0 || frame != 0 {
            return Err(error("A local drawing has one drawing history"));
        }
        return Ok(replay.as_deref().cloned());
    }
    let RevealedEntry::Artwork { assignment, revision, image, checkpoint, replay, assets, extent } = entry(bundle, chain, frame)? else {
        return Err(error("This part is explicitly missing; there is no drawing history to replay"));
    };
    let submission = Submission {
        id: SubmissionId::new(assignment.project.as_str()).map_err(other)?,
        assignment: assignment.id,
        author: assignment.player,
        project: assignment.project,
        revision: *revision,
        image: *image,
        checkpoint: *checkpoint,
        replay: **replay,
        assets: assets.clone(),
        note: String::new(),
        seam: None,
        extent: *extent,
    };
    let Some(manifest) = replay.manifest else {
        return Ok(None);
    };
    Ok(Some(ReplayArtifact::decode(object(bundle, manifest)?, &submission).map_err(other)?))
}

fn checkpoint<'a>(bundle: &'a GalleryBundle, artifact: &ReplayArtifact, asset: AssetRef) -> Result<&'a [u8]> {
    if let Some(bytes) = bundle.objects.get(&asset.hash) {
        if bytes.len() as u64 == asset.bytes {
            return object(bundle, asset);
        }
        return Err(error("Replay checkpoint length does not match its reference"));
    }
    for pack in &artifact.packs {
        let bytes = object(bundle, *pack)?;
        if let Some((_, data)) = unpack_checkpoints(bytes).map_err(other)?.into_iter().find(|(candidate, _)| *candidate == asset) {
            return Ok(data);
        }
    }
    Err(error("The retained replay checkpoint is missing; preserve this archive and restore its original"))
}

pub fn replay_info(bundle: &GalleryBundle, chain: u8, frame: u8) -> Result<ReplayInfo> {
    if matches!(&bundle.manifest.content, GalleryContent::Game { .. }) && matches!(entry(bundle, chain, frame)?, RevealedEntry::Missing { .. }) {
        return Ok(ReplayInfo { duration_ms: 0, moments: Vec::new(), baseline_only: true, missing: true });
    }
    let Some(artifact) = artifact(bundle, chain, frame)? else {
        return Ok(ReplayInfo { duration_ms: 0, moments: Vec::new(), baseline_only: true, missing: false });
    };
    Ok(ReplayInfo {
        duration_ms: artifact.steps.last().map_or(0, |step| step.elapsed_ms),
        baseline_only: artifact.steps.len() <= 1,
        missing: false,
        moments: artifact
            .steps
            .iter()
            .map(|step| ReplayMoment { revision: step.revision, elapsed_ms: step.elapsed_ms, command: step.command.clone(), kind: step.kind.clone() })
            .collect(),
    })
}

/// Exact retained edit boundaries; no command with external effects is executed.
/// At equal timestamps the latest completed edit wins. Use `replay_step` to inspect
/// each same-time boundary explicitly, including undo and redo.
pub fn replay_image(bundle: &GalleryBundle, chain: u8, frame: u8, elapsed_ms: u64) -> Result<RoomImage> {
    let Some(artifact) = artifact(bundle, chain, frame)? else {
        return render_frame(bundle, chain, frame);
    };
    let index = artifact.steps.iter().rposition(|step| step.elapsed_ms <= elapsed_ms).unwrap_or(0);
    replay_step_with(bundle, &artifact, index)
}
pub fn replay_step(bundle: &GalleryBundle, chain: u8, frame: u8, index: usize) -> Result<RoomImage> {
    let Some(artifact) = artifact(bundle, chain, frame)? else {
        return if index == 0 { render_frame(bundle, chain, frame) } else { Err(error("This import has a baseline only")) };
    };
    replay_step_with(bundle, &artifact, index)
}
fn replay_step_with(bundle: &GalleryBundle, artifact: &ReplayArtifact, index: usize) -> Result<RoomImage> {
    let step = artifact.steps.get(index).ok_or_else(|| error("Replay step is outside the retained history"))?;
    let document = load(checkpoint(bundle, artifact, step.checkpoint.asset)?, true)?;
    if let GalleryContent::Game { room } = &bundle.manifest.content
        && room.mode.validate_extent(photocraft_party::ImageExtent { width: document.size.width, height: document.size.height }).is_err()
    {
        return Err(error("A replay checkpoint has unsupported assignment dimensions"));
    }
    render_document(&document)
}

/// A final animation frame or one creature band. Missing submissions are transparent,
/// with the explicit missing label kept in metadata/UI rather than painted into pixels.
pub fn render_frame(bundle: &GalleryBundle, chain: u8, frame: u8) -> Result<RoomImage> {
    bundle.manifest.validate().map_err(other)?;
    if let GalleryContent::Project { checkpoint, .. } = &bundle.manifest.content {
        if chain != 0 {
            return Err(error("A local project has only one animation"));
        }
        let mut document = load(object(bundle, *checkpoint)?, true)?;
        if let Some(timeline) = &mut document.timeline {
            if usize::from(frame) >= timeline.duration {
                return Err(error("Frame is outside the local animation"));
            }
            timeline.current = usize::from(frame);
            crate::video_cmds::sync(&mut document);
        } else if frame != 0 {
            return Err(error("This local drawing has only one frame"));
        }
        return render_document(&document);
    }
    let mode = game(bundle)?.mode;
    let item = entry(bundle, chain, frame)?;
    match item {
        RevealedEntry::Artwork { image, .. } => {
            let ext = item.resolved_extent(mode).map_err(other)?;
            decode_png(object(bundle, *image)?, (ext.width, ext.height))
        }
        RevealedEntry::Missing { .. } => {
            let expected = dimensions(mode);
            Ok(RoomImage { width: expected.0, height: expected.1, rgba: vec![0; expected.0 as usize * expected.1 as usize * 4] })
        }
    }
}

pub fn render_creature(bundle: &GalleryBundle, chain: u8) -> Result<RoomImage> {
    if game(bundle)?.mode != Mode::ExquisiteCorpse {
        return Err(error("Choose an Exquisite Corpse game"));
    }
    let head = render_frame(bundle, chain, 0)?;
    let body = render_frame(bundle, chain, 1)?;
    let legs = render_frame(bundle, chain, 2)?;
    crate::rooms::corpse::assemble([Some(&head), Some(&body), Some(&legs)])
}

/// Verify lossless native endpoints against the accepted canonical PNGs before a
/// newly acquired game is acknowledged as a complete editable gallery original.
pub fn validate_originals(bundle: &GalleryBundle) -> Result<()> {
    bundle.validate().map_err(other)?;
    match &bundle.manifest.content {
        GalleryContent::Game { room } => {
            for item in &room.reveal {
                if let RevealedEntry::Artwork { assignment, checkpoint, .. } = item {
                    let expected = item.resolved_extent(room.mode).map_err(other)?;
                    let doc = load(object(bundle, checkpoint.asset)?, true)?;
                    if (doc.size.width, doc.size.height) != (expected.width, expected.height) {
                        return Err(error("An accepted checkpoint has different assignment dimensions"));
                    }
                    let canonical = render_document(&doc)?;
                    if canonical != render_frame(bundle, assignment.id.chain, assignment.id.round)? {
                        return Err(error("The final native replay pixels differ from the accepted image; preserve the original and retry its download"));
                    }
                    let info = replay_info(bundle, assignment.id.chain, assignment.id.round)?;
                    if canonical != replay_image(bundle, assignment.id.chain, assignment.id.round, info.duration_ms)? {
                        return Err(error("The retained replay endpoint differs from its accepted image"));
                    }
                }
            }
        }
        GalleryContent::Project { checkpoint, .. } => {
            drop(load(object(bundle, *checkpoint)?, true)?);
        }
    }
    Ok(())
}

pub fn frame_count(bundle: &GalleryBundle, chain: u8) -> Result<usize> {
    bundle.manifest.validate().map_err(other)?;
    match &bundle.manifest.content {
        GalleryContent::Game { room } => {
            if usize::from(chain) >= room.members.len() {
                return Err(error("Choose an existing chain"));
            }
            if room.mode == Mode::ExquisiteCorpse {
                return Ok(1);
            }
            Ok(usize::from(room.mode.rounds(room.members.len()).map_err(other)?))
        }
        GalleryContent::Project { checkpoint, .. } => {
            if chain != 0 {
                return Err(error("A local project has only one animation"));
            }
            let doc = load(object(bundle, *checkpoint)?, true)?;
            let count = doc.timeline.as_ref().map_or(1, |t| t.duration);
            if count == 0 || count > MAX_FRAMES {
                return Err(error("Gallery animation export supports 1–36 frames"));
            }
            Ok(count)
        }
    }
}
fn output_frame(bundle: &GalleryBundle, chain: u8, frame: usize) -> Result<RoomImage> {
    if matches!(&bundle.manifest.content,GalleryContent::Game {room} if room.mode == Mode::ExquisiteCorpse) {
        if frame != 0 {
            return Err(error("A creature exports as one assembled image"));
        }
        render_creature(bundle, chain)
    } else {
        render_frame(bundle, chain, u8::try_from(frame).map_err(|_| error("Too many output frames"))?)
    }
}

/// A bounded thumbnail, regenerated from the original rather than treated as one.
pub fn thumbnail(bundle: &GalleryBundle, chain: u8, max_side: u32) -> Result<RoomImage> {
    if !(1..=512).contains(&max_side) {
        return Err(error("Thumbnail side must be 1–512 pixels"));
    }
    let image = output_frame(bundle, chain, 0)?;
    let source = image::RgbaImage::from_raw(image.width, image.height, image.rgba).ok_or_else(|| error("Invalid thumbnail pixels"))?;
    let scaled = image::DynamicImage::ImageRgba8(source).thumbnail(max_side, max_side).into_rgba8();
    Ok(RoomImage { width: scaled.width(), height: scaled.height(), rgba: scaled.into_raw() })
}

fn image_surface(image: &RoomImage) -> Result<photocraft_raster::Surface> {
    valid_image(image.width, image.height, image.rgba.len())?;
    let mut surface = photocraft_raster::Surface::new(photocraft_color::PixelFormat::RGBA8);
    let width = usize::try_from(image.width).map_err(|_| error("Image width overflow"))?;
    let mut row = Vec::with_capacity(width.saturating_mul(4));
    for (y, pixels) in image.rgba.chunks_exact(width.saturating_mul(4)).enumerate() {
        row.clear();
        row.extend(pixels.iter().map(|v| f32::from(*v) / 255.0));
        let y = i32::try_from(y).map_err(|_| error("Image row overflow"))?;
        surface.try_write_region(photocraft_geom::Rect::new(0, y, image.width as i32, y + 1), &row).map_err(other)?;
    }
    Ok(surface)
}

fn fresh_contents(document: &mut Document) {
    let mut contents = HashMap::new();
    let ids: Vec<_> = document.walk().into_iter().map(|(_, _, layer)| layer.id).collect();
    for id in ids {
        if let Some(layer) = document.layer_mut(id) {
            layer.locks = Default::default();
            layer.psd_id = None;
            if let LayerContent::Smart(smart) = &mut layer.content {
                smart.contents_id = *contents.entry(smart.contents_id).or_insert_with(photocraft_doc::SmartContentsId::fresh);
            }
        }
    }
}

/// Creates an independent editable project. None retains the complete animation or
/// creature; Some is explicit still/part extraction. The caller records this result
/// as a new baseline and keeps the source history in its original gallery entry.
pub fn remix(bundle: &GalleryBundle, chain: u8, frame: Option<u8>) -> Result<(Document, RemixProvenance)> {
    bundle.validate().map_err(other)?;
    let source_revision = match &bundle.manifest.content {
        GalleryContent::Game { room } => room.revision,
        GalleryContent::Project { replay, .. } => replay.as_ref().and_then(|r| r.steps.last()).map_or(1, |step| step.revision),
    };
    let mut authors = if bundle.manifest.authors.is_empty() {
        bundle.manifest.provenance.as_ref().map(|source| source.authors.clone()).unwrap_or_default()
    } else {
        bundle.manifest.authors.clone()
    };
    if matches!(bundle.manifest.content, GalleryContent::Game { .. })
        && let Some(frame) = frame
    {
        let assignment = match entry(bundle, chain, frame)? {
            RevealedEntry::Artwork { assignment, .. } | RevealedEntry::Missing { assignment, .. } => assignment,
        };
        authors.retain(|author| author.id == assignment.player);
    }
    let provenance = RemixProvenance { source_entry: bundle.manifest.id, source_game: bundle.manifest.game(), source_revision, chain, frame, authors };
    let mut document = match &bundle.manifest.content {
        GalleryContent::Project { checkpoint, .. } => remix_project(bundle, *checkpoint, chain, frame)?,
        GalleryContent::Game { room } => remix_game(bundle, room, chain, frame)?,
    };
    document.name = format!("Remix of {}", bundle.manifest.title).chars().take(96).collect();
    if let Some(timeline) = &mut document.timeline {
        timeline.fps = f32::from(bundle.manifest.fps);
        let ids: Vec<_> = document.walk().into_iter().map(|(_, _, layer)| layer.id).collect();
        for id in ids {
            if let Some(video) = document.layer_mut(id).and_then(|layer| layer.video.as_mut()) {
                video.fps = f32::from(bundle.manifest.fps);
            }
        }
    }
    document.selection = None;
    document.quick_mask = None;
    document.notes.clear();
    fresh_contents(&mut document);
    let json = serde_json::to_string(&provenance).map_err(other)?;
    document.metadata.text.retain(|(key, _)| key != "Komaphone remix source");
    document.metadata.text.push(("Komaphone remix source".into(), json.clone()));
    document.metadata.text.retain(|(key, _)| key != "Komaphone animation looping");
    document.metadata.text.push(("Komaphone animation looping".into(), bundle.manifest.looping.to_string()));
    if let GalleryContent::Game { room } = &bundle.manifest.content {
        let missing: Vec<_> = room
            .reveal
            .iter()
            .filter_map(|entry| match entry {
                RevealedEntry::Missing { assignment, reason } if assignment.id.chain == chain => Some((assignment.id, reason)),
                _ => None,
            })
            .collect();
        if !missing.is_empty() {
            document.metadata.text.push(("Komaphone missing submissions".into(), serde_json::to_string(&missing).map_err(other)?));
        }
    }
    // Escape the entire JSON as XML text; names cannot inject XMP markup.
    let escaped = json.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    if let Some(original_xmp) = document.metadata.xmp.take() {
        document.metadata.text.push(("Komaphone original XMP".into(), original_xmp.to_string()));
    }
    document.metadata.xmp=Some(format!("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:komaphone=\"urn:komaphone:remix:1\"><komaphone:source>{escaped}</komaphone:source></rdf:Description></rdf:RDF></x:xmpmeta>").into());
    Ok((document, provenance))
}

fn remix_project(bundle: &GalleryBundle, checkpoint: AssetRef, chain: u8, frame: Option<u8>) -> Result<Document> {
    if chain != 0 {
        return Err(error("A local project has only one animation"));
    }
    let mut doc = load_copy(object(bundle, checkpoint)?)?;
    let Some(frame) = frame else {
        return Ok(doc);
    };
    if let Some(timeline) = &mut doc.timeline {
        if usize::from(frame) >= timeline.duration {
            return Err(error("Choose an existing animation frame"));
        }
        timeline.current = usize::from(frame);
        crate::video_cmds::sync(&mut doc);
        let ids: Vec<_> = doc.walk().into_iter().map(|(_, _, layer)| layer.id).collect();
        for id in ids {
            if let Some(layer) = doc.layer_mut(id) {
                layer.video = None;
            }
        }
        doc.timeline = None;
    } else if frame != 0 {
        return Err(error("A still drawing has only one frame"));
    }
    Ok(doc)
}

fn remix_game(bundle: &GalleryBundle, room: &RoomView, chain: u8, frame: Option<u8>) -> Result<Document> {
    if usize::from(chain) >= room.members.len() {
        return Err(error("Choose an existing revealed chain"));
    }
    if let Some(frame) = frame {
        let RevealedEntry::Artwork { checkpoint, .. } = entry(bundle, chain, frame)? else {
            return Err(error("A missing submission cannot be remixed"));
        };
        return load_copy(object(bundle, checkpoint.asset)?);
    }
    if room.mode == Mode::ExquisiteCorpse {
        return remix_creature(bundle, chain);
    }
    let count = frame_count(bundle, chain)?;
    let mut doc = Document::new("Remixed animation", Size { width: 1536, height: 1024 }, ColorMode::Rgb, SampleType::U8);
    doc.icc_profile = Some(photocraft_cms::Builtin::Srgb.profile().to_bytes());
    let mut frames = Vec::with_capacity(count);
    for index in 0..count {
        frames.push(image_surface(&render_frame(bundle, chain, index as u8)?)?);
    }
    let first = frames.first().cloned().ok_or_else(|| error("This animation has no frames"))?;
    let mut layer = Layer::raster("Remixed frames", doc.pixel_format());
    layer.content = LayerContent::Raster(first);
    layer.video = Some(VideoData::new(frames, f32::from(bundle.manifest.fps)));
    doc.layers.push(layer);
    doc.timeline = Some(Timeline::new(count, f32::from(bundle.manifest.fps)));
    Ok(doc)
}

fn remix_creature(bundle: &GalleryBundle, chain: u8) -> Result<Document> {
    let parts = [render_frame(bundle, chain, 0)?, render_frame(bundle, chain, 1)?, render_frame(bundle, chain, 2)?];
    let mut total_height = 0u32;
    for image in &parts {
        if image.width != photocraft_party::CORPSE_WIDTH
            || !(photocraft_party::CORPSE_MIN_BAND_HEIGHT..=photocraft_party::CORPSE_MAX_BAND_HEIGHT).contains(&image.height)
        {
            return Err(error("A creature part has unsupported dimensions"));
        }
        total_height = total_height.checked_add(image.height).ok_or_else(|| error("Creature height overflow"))?;
    }
    if total_height > photocraft_party::CORPSE_MAX_TOTAL_HEIGHT {
        return Err(error("Assembled creature exceeds the maximum supported height"));
    }
    let mut doc = Document::new("Remixed creature", Size { width: photocraft_party::CORPSE_WIDTH, height: total_height }, ColorMode::Rgb, SampleType::U8);
    doc.icc_profile = Some(photocraft_cms::Builtin::Srgb.profile().to_bytes());
    let mut y_offset = 0i32;
    for (part, image) in parts.iter().enumerate() {
        let mut layer = Layer::raster(crate::rooms::corpse::part_name(part as u8), doc.pixel_format());
        let surface = image_surface(image)?;
        let w = i32::try_from(image.width).map_err(|_| error("Creature part width overflow"))?;
        let h = i32::try_from(image.height).map_err(|_| error("Creature part height overflow"))?;
        layer.content = LayerContent::Raster(surface.translated(0, y_offset, photocraft_geom::Rect::new(0, 0, w, h)));
        doc.layers.push(layer);
        y_offset = y_offset.checked_add(h).ok_or_else(|| error("Creature height overflow"))?;
    }
    Ok(doc)
}

pub fn gif_bytes(bundle: &GalleryBundle, chain: u8, options: AnimationOptions) -> Result<Vec<u8>> {
    gif_bytes_with(bundle, chain, options, |_, _| true)
}

/// Display the actual quantized GIF output, including its binary-alpha limit.
pub fn gif_preview(bytes: &[u8], frame: usize) -> Result<RoomImage> {
    if bytes.len() as u64 > photocraft_party::MAX_ASSET_BYTES || frame >= MAX_FRAMES {
        return Err(error("GIF preview is outside its bounded frame range"));
    }
    let options = photocraft_codecs::DecodeOptions {
        limits: photocraft_codecs::Limits { max_width: 8192, max_height: 8192, max_alloc: MAX_SEQUENCE_PIXELS * 4, ..Default::default() },
        ..Default::default()
    };
    let frames = photocraft_codecs::decode_gif_frames(bytes, &options).map_err(other)?;
    if frames.len() > MAX_FRAMES {
        return Err(error("GIF preview supports up to 36 frames"));
    }
    let selected = frames.get(frame).ok_or_else(|| error("GIF frame is outside the export"))?;
    if frames.iter().any(|frame| !frame.image.warnings.is_empty()) {
        return Err(error("The GIF preview is incomplete"));
    }
    Ok(RoomImage { width: selected.image.width(), height: selected.image.height(), rgba: selected.image.data().to_vec() })
}
pub fn gif_bytes_with(bundle: &GalleryBundle, chain: u8, options: AnimationOptions, mut progress: impl FnMut(usize, usize) -> bool) -> Result<Vec<u8>> {
    if !(1..=24).contains(&options.fps) {
        return Err(error("Animation speed must be 1–24 frames per second"));
    }
    let count = frame_count(bundle, chain)?;
    if !progress(0, count) {
        return Err(error("Export cancelled; the original is unchanged"));
    }
    let mut frames = Vec::with_capacity(count);
    let mut dimensions = None;
    let mut pixels = 0u64;
    for index in 0..count {
        let image = output_frame(bundle, chain, index)?;
        pixels = pixels.saturating_add(u64::from(image.width) * u64::from(image.height));
        if pixels > MAX_SEQUENCE_PIXELS {
            return Err(error("GIF export exceeds the 32 megapixel animation budget; use PNG frames or an archive"));
        }
        if dimensions.is_some_and(|size| size != (image.width, image.height)) {
            return Err(error("Animation frames have different dimensions"));
        }
        dimensions = Some((image.width, image.height));
        // Quantize each source frame separately; preserve the original lossless bytes.
        frames.push(gif_frame(&image, (100.0 / f64::from(options.fps)).round() as u16)?);
        if !progress(index + 1, count) {
            return Err(error("Export cancelled; the original is unchanged"));
        }
    }
    let (width, height) = dimensions.ok_or_else(|| error("Animation contains no frames"))?;
    photocraft_codecs::web::encode_gif_animated(width, height, &frames, options.looping).map_err(other)
}

fn gif_frame(image: &RoomImage, delay_cs: u16) -> Result<photocraft_codecs::web::GifFrame> {
    use photocraft_algo::quantize::{self, Dither, Forced, PaletteKind};
    valid_image(image.width, image.height, image.rgba.len())?;
    let mut pixels: Vec<[f32; 4]> =
        image.rgba.chunks_exact(4).map(|p| [f32::from(p[0]) / 255.0, f32::from(p[1]) / 255.0, f32::from(p[2]) / 255.0, f32::from(p[3]) / 255.0]).collect();
    let visible: Vec<[f32; 4]> = pixels.iter().filter(|p| p[3] >= 0.5).map(|p| [p[0], p[1], p[2], 1.0]).collect();
    let input = if visible.is_empty() { &pixels } else { &visible };
    // Reserve a transparent index even for opaque frames. The shared encoder uses
    // disposal 2 for those frames, clearing the preceding canvas before the next
    // full frame; otherwise an opaque predecessor can leak through later holes.
    let mut palette = quantize::build_palette(input, PaletteKind::Adaptive, 255, Forced::None).map_err(error)?;
    palette.truncate(255);
    palette.push(palette.first().copied().unwrap_or([0, 0, 0]));
    let transparent = Some((palette.len() - 1) as u8);
    let indices = quantize::quantize(&mut pixels, image.width as usize, &palette, Dither::None, 1.0, transparent.map(usize::from));
    Ok(photocraft_codecs::web::GifFrame { indices, palette, transparent, delay_cs })
}

/// New output paths only. A failed/cancelled export cannot overwrite any original.
pub fn write_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(error("Choose an existing export folder"));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(other)?;
    temporary.write_all(bytes).map_err(other)?;
    temporary.as_file().sync_all().map_err(other)?;
    temporary.persist_noclobber(path).map_err(|e| error(format!("Could not save export; choose a new filename: {}", e.error)))?;
    Ok(())
}
pub fn export_png(bundle: &GalleryBundle, chain: u8, frame: Option<u8>, path: &Path) -> Result<()> {
    let image = if let Some(frame) = frame { render_frame(bundle, chain, frame)? } else { output_frame(bundle, chain, 0)? };
    write_new_file(path, &encode_png(&image)?)
}
pub fn export_gif(bundle: &GalleryBundle, chain: u8, options: AnimationOptions, path: &Path) -> Result<()> {
    write_new_file(path, &gif_bytes(bundle, chain, options)?)
}

/// Publish a complete native directory. Windows' ordinary rename can replace an
/// empty directory created after a preflight check; explicitly omit replacement.
pub(crate) fn publish_new_directory(source: &Path, destination: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let source = source.to_str().ok_or_else(|| error("Gallery staging path is not valid text"))?;
        let destination = destination.to_str().ok_or_else(|| error("Gallery destination path is not valid text"))?;
        if source.contains('\0') || destination.contains('\0') {
            return Err(error("Gallery publication paths cannot contain NUL"));
        }
        winsafe::MoveFileEx(source, Some(destination), winsafe::co::MOVEFILE::WRITE_THROUGH).map_err(other)
    }
    #[cfg(not(windows))]
    {
        // Preserve the existing native behavior outside the Windows product target.
        std::fs::rename(source, destination).map_err(other)
    }
}
pub fn export_sequence(bundle: &GalleryBundle, chain: u8, path: &Path, mut progress: impl FnMut(usize, usize) -> bool) -> Result<Vec<PathBuf>> {
    let count = frame_count(bundle, chain)?;
    if path.exists() {
        return Err(error("Choose a new folder for the PNG sequence; existing files are preserved"));
    }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(error("Choose an existing parent export folder"));
    }
    let staging = tempfile::tempdir_in(parent).map_err(other)?;
    let mut names = Vec::with_capacity(count);
    for index in 0..count {
        if !progress(index, count) {
            return Err(error("Export cancelled; the original is unchanged"));
        }
        let name = format!("frame-{index:04}.png");
        write_new_file(&staging.path().join(&name), &encode_png(&output_frame(bundle, chain, index)?)?)?;
        names.push(name);
    }
    if !progress(count, count) {
        return Err(error("Export cancelled; the original is unchanged"));
    }
    if path.exists() {
        return Err(error("The selected export folder now exists; choose a new folder"));
    }
    publish_new_directory(staging.path(), path)?;
    // TempDir now points to the absent staging path; drop cannot touch the published output.
    Ok(names.into_iter().map(|name| path.join(name)).collect())
}

#[cfg(all(test, windows))]
mod directory_publication_tests {
    use super::*;

    #[test]
    fn directory_publication_never_replaces_a_concurrent_destination() {
        let root = tempfile::tempdir().unwrap();
        let staged = root.path().join("staged");
        let destination = root.path().join("destination");
        std::fs::create_dir(&staged).unwrap();
        std::fs::write(staged.join("original"), b"complete output").unwrap();
        let nul_destination = root.path().join("truncated\0suffix");
        assert!(publish_new_directory(&staged, &nul_destination).is_err());
        assert!(!root.path().join("truncated").exists());
        assert!(publish_new_directory(&root.path().join("staged\0suffix"), &destination).is_err());
        assert!(!destination.exists());
        std::fs::create_dir(&destination).unwrap();
        assert!(publish_new_directory(&staged, &destination).is_err());
        assert_eq!(std::fs::read_dir(&destination).unwrap().count(), 0);
        assert_eq!(std::fs::read(staged.join("original")).unwrap(), b"complete output");
        std::fs::write(destination.join("keep"), b"existing work").unwrap();
        assert!(publish_new_directory(&staged, &destination).is_err());
        assert_eq!(std::fs::read(destination.join("keep")).unwrap(), b"existing work");
        let fresh = root.path().join("fresh");
        publish_new_directory(&staged, &fresh).unwrap();
        assert_eq!(std::fs::read(fresh.join("original")).unwrap(), b"complete output");
        assert!(!staged.exists());
    }
}
