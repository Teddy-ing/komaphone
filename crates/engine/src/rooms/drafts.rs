//! Assigned drafts and exact durable submission packages, prepared on the local I/O worker.
use crate::{EngineError, Result, Session};
use image::ImageEncoder;
use photocraft_doc::{Color, ColorMode, Document, SampleType, Size};
use photocraft_local_store::{LoadedDraft, ReplayKind, ReplayRecord, RevisionSummary, SaveRequest, Store};
use photocraft_party::replay_artifact::{ReplayArtifact, ReplayArtifactStep};
use photocraft_party::room::Phase;
use photocraft_party::{
    AssetHash, AssetRef, AssignmentId, CheckpointId, CheckpointRef, EventId, Mode, ParticipantId, ProjectId, ReplayId, ReplayRef, Submission, SubmissionId,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(crate) struct PrepareRequest {
    pub assignment: AssignmentId,
    pub author: ParticipantId,
    pub project: ProjectId,
    pub revision: u64,
    pub mode: Mode,
    pub note: String,
    pub freeze: bool,
}
pub(crate) const MAX_PARTY_REPLAY_STEPS: usize = 36;
pub(crate) const MAX_PARTY_REPLAY_PACK_BYTES: u64 = 4 * 1024 * 1024;
const _: () = assert!(MAX_PARTY_REPLAY_PACK_BYTES <= photocraft_party::MAX_ASSET_BYTES);
const MAX_SUBMISSION_OPERATIONS: usize = 8190;
const MAX_OUTBOX_PENDING_BYTES: u64 = 384 * 1024 * 1024;
const RPC_UPLOAD_HEADER_OVERHEAD: u64 = 320;
const RPC_APPLY_ENVELOPE_OVERHEAD: usize = 512;

pub(crate) struct Prepared {
    pub submission: Submission,
    pub assets: Vec<Vec<u8>>,
    pub local_revision: u64,
    pub freeze: bool,
}

impl Session {
    pub(crate) fn freeze_room_deadline(&mut self) {
        let Some(room) = self.rooms.status.room.as_ref() else { return };
        if room.phase != Phase::Transfer || self.rooms.busy() || self.local_projects.preparing_room {
            return;
        }
        let Some(assignment) = room.assignment.as_ref().filter(|a| !a.submitted) else { return };
        if self.local_projects.frozen_assignment == Some(assignment.assignment.id) {
            return;
        }
        let Some(author) = self.rooms.status.profile.as_ref().map(|p| p.id) else { return };
        let project = assignment.assignment.project;
        let id = assignment.assignment.id;
        let epoch = room.epoch;
        let room_id = room.id;
        let mode = room.mode;
        let pin = self
            .rooms
            .status
            .freeze_pin
            .as_ref()
            .filter(|pin| pin.room == room_id && pin.epoch == epoch && pin.assignment == id && pin.project == project)
            .cloned();
        if let Some(pin) = pin {
            let note = self.rooms.status.note_draft.as_ref().filter(|draft| draft.assignment == id).map(|draft| draft.note.clone()).unwrap_or_default();
            let request = PrepareRequest { assignment: id, author, project, revision: pin.revision, mode, note, freeze: true };
            self.local_projects.frozen_assignment = Some(id);
            if let Err(error) = self.prepare_assigned_submission(request) {
                self.rooms.set_submission_error(error.to_string());
            }
            return;
        }
        let revision = self
            .documents()
            .iter()
            .map(|d| self.local_projects.status(d.doc.id))
            .find(|s| s.project == Some(project))
            .map(|s| s.local_revision)
            .or_else(|| self.local_projects.drafts.iter().find(|d| d.project == project).map(|d| d.revision));
        let Some(revision) = revision.filter(|r| *r > 0) else { return };
        let pin = crate::rooms::FreezePin { room: room_id, epoch, assignment: id, project, revision };
        if let Err(error) = self.pin_room_freeze(pin) {
            self.rooms.set_submission_error(error.to_string());
        }
    }
    pub fn open_room_draft(&mut self) -> Result<()> {
        let room = self.rooms.status.room.as_ref().ok_or_else(|| EngineError::Other("Join a room first".into()))?;
        if room.phase != Phase::Drawing {
            return Err(EngineError::Other("The room is not accepting drawing edits".into()));
        }
        let assignment = room.assignment.as_ref().filter(|a| !a.submitted).ok_or_else(|| EngineError::Other("No editable assignment".into()))?;
        self.open_assigned_local_project(assignment.assignment.project, room.mode)
    }
    pub fn submit_room_draft(&mut self, note: String) -> Result<()> {
        photocraft_party::validate_label(&note, photocraft_party::MAX_NOTE_BYTES, true).map_err(|e| EngineError::Other(e.to_string()))?;
        let room = self.rooms.status.room.as_ref().ok_or_else(|| EngineError::Other("Join a room first".into()))?;
        if !matches!(room.phase, Phase::Drawing | Phase::Transfer) {
            return Err(EngineError::Other("Submissions are not open".into()));
        }
        let assignment = room.assignment.as_ref().filter(|a| !a.submitted).ok_or_else(|| EngineError::Other("No pending assignment".into()))?;
        let author = self.rooms.status.profile.as_ref().ok_or_else(|| EngineError::Other("Connect your profile first".into()))?.id;
        if room.phase == Phase::Transfer {
            if self.rooms.busy() || self.local_projects.preparing_room {
                return Err(EngineError::Other("Submission work is already pending".into()));
            }
            // A retry always uses the first committed deadline pin and deterministic IDs.
            self.local_projects.frozen_assignment = None;
            self.freeze_room_deadline();
            return if self.rooms.busy() || self.local_projects.preparing_room {
                Ok(())
            } else {
                Err(EngineError::Other("No saved assigned revision is available to freeze; preserve the local draft".into()))
            };
        }
        let document = self.active().ok_or_else(|| EngineError::Other("Open your assigned drawing".into()))?;
        let local = self.local_projects.status(document.doc.id);
        if local.project != Some(assignment.assignment.project) || !self.local_document_saved(document.doc.id) {
            return Err(EngineError::Other("Open the assigned drawing and wait for Saved locally before submitting".into()));
        }
        self.prepare_assigned_submission(PrepareRequest {
            assignment: assignment.assignment.id,
            author,
            project: assignment.assignment.project,
            revision: local.local_revision,
            mode: room.mode,
            note,
            freeze: room.phase == Phase::Transfer,
        })
    }
}

fn dimensions(mode: Mode) -> (u32, u32) {
    match mode {
        Mode::Animation { .. } => (1536, 1024),
        Mode::ExquisiteCorpse => (1024, 512),
    }
}
pub(crate) fn open(store: &mut Store, project: ProjectId, mode: Mode) -> std::result::Result<LoadedDraft, String> {
    match store.load(project) {
        Ok(loaded) => return Ok(loaded),
        Err(photocraft_local_store::Error::NotFound) => {}
        Err(error) => return Err(error.to_string()),
    }
    let (width, height) = dimensions(mode);
    let document = Document::with_background("Room drawing", Size { width, height }, ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let request = SaveRequest {
        project,
        expected_revision: 0,
        request_id: photocraft_local_store::fresh_event_id().map_err(|e| e.to_string())?,
        title: "Room drawing".into(),
        document,
        event: ReplayRecord {
            schema_version: 1,
            command: "file.new".into(),
            params: json!({"width":width,"height":height,"background":"white"}),
            elapsed_ms: 0,
            kind: ReplayKind::Baseline,
        },
    };
    store.save(&request).map_err(|e| e.to_string())?;
    store.load(project).map_err(|e| e.to_string())
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn short_hash(bytes: &[u8]) -> std::result::Result<String, String> {
    hash(bytes).get(..32).map(str::to_owned).ok_or_else(|| "Invalid artifact identity".into())
}
fn add(objects: &mut BTreeMap<AssetHash, Vec<u8>>, bytes: Vec<u8>) -> std::result::Result<AssetRef, String> {
    add_bounded(objects, bytes, true)
}
fn add_bounded(objects: &mut BTreeMap<AssetHash, Vec<u8>>, bytes: Vec<u8>, manifest_limit: bool) -> std::result::Result<AssetRef, String> {
    let asset = AssetRef { hash: AssetHash::new(&hash(&bytes)).map_err(|e| e.to_string())?, bytes: bytes.len() as u64 };
    asset.validate().map_err(|e| e.to_string())?;
    if !objects.contains_key(&asset.hash) {
        let total = objects.values().map(|b| b.len() as u64).fold(asset.bytes, u64::saturating_add);
        if (manifest_limit && objects.len() >= photocraft_party::MAX_ASSETS) || total > photocraft_party::MAX_PROJECT_BYTES {
            return Err("Submission exceeds the retained asset budget. All local drawing and replay remain saved.".into());
        }
        objects.insert(asset.hash, bytes);
    }
    Ok(asset)
}
pub(crate) fn checkpoint(objects: &mut BTreeMap<AssetHash, Vec<u8>>, doc: &Document) -> std::result::Result<CheckpointRef, String> {
    let bytes = photocraft_format::save_to_bytes(doc, &Default::default()).map_err(|e| e.to_string())?;
    let id = CheckpointId::new(&short_hash(&bytes)?).map_err(|e| e.to_string())?;
    Ok(CheckpointRef { id, asset: add_bounded(objects, bytes, false)? })
}
pub(crate) fn pack_history(objects: &mut BTreeMap<AssetHash, Vec<u8>>, keep: [AssetHash; 2]) -> std::result::Result<Vec<AssetRef>, String> {
    let history = std::mem::take(objects);
    let mut group = Vec::new();
    let mut size = 10u64;
    let mut packs = Vec::new();
    for (hash, bytes) in history {
        if keep.contains(&hash) {
            add(objects, bytes)?;
            continue;
        }
        let entry = AssetRef { hash, bytes: bytes.len() as u64 };
        if size.saturating_add(72).saturating_add(entry.bytes) > photocraft_party::MAX_ASSET_BYTES && !group.is_empty() {
            let pack = photocraft_party::replay_artifact::pack_checkpoints(&group).map_err(|e| e.to_string())?;
            packs.push(add(objects, pack)?);
            group.clear();
            size = 10;
        }
        // A checkpoint close to the object limit cannot fit a framing header; retain it directly.
        if entry.bytes > photocraft_party::MAX_ASSET_BYTES - 82 {
            add(objects, bytes)?;
            continue;
        }
        size = size.saturating_add(72).saturating_add(entry.bytes);
        group.push((entry, bytes));
    }
    if !group.is_empty() {
        packs.push(add(objects, photocraft_party::replay_artifact::pack_checkpoints(&group).map_err(|e| e.to_string())?)?);
    }
    Ok(packs)
}
fn png(width: u32, height: u32, pixels: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes).write_image(pixels, width, height, image::ExtendedColorType::Rgba8).map_err(|e| e.to_string())?;
    Ok(bytes)
}

struct SampledKeyframe {
    summary: RevisionSummary,
    checkpoint: CheckpointRef,
    bytes: Vec<u8>,
    json_transfer_bytes: u64,
}

fn checkpoint_ref_from_verified_bytes(asset: AssetRef, bytes: &[u8]) -> std::result::Result<CheckpointRef, String> {
    if bytes.len() as u64 != asset.bytes {
        return Err("Stored checkpoint length mismatch; preserve this local draft for recovery".into());
    }
    asset.validate().map_err(|_| {
        "This drawing's editable checkpoint exceeds the 64 MiB single-object upload limit; merge or simplify heavy imported layers in your saved local draft and try again."
            .to_string()
    })?;
    let id = CheckpointId::new(&short_hash(bytes)?).map_err(|e| e.to_string())?;
    Ok(CheckpointRef { id, asset })
}

fn encode_checkpoint_bytes(doc: &Document) -> std::result::Result<(CheckpointRef, Vec<u8>), String> {
    let bytes = photocraft_format::save_to_bytes(doc, &Default::default()).map_err(|e| e.to_string())?;
    let asset = AssetRef { hash: AssetHash::new(&hash(&bytes)).map_err(|e| e.to_string())?, bytes: bytes.len() as u64 };
    let checkpoint = checkpoint_ref_from_verified_bytes(asset, &bytes)?;
    Ok((checkpoint, bytes))
}

/// Upper-bound the JSON outbox size produced when `worker::submit` chunks `bytes` into
/// `Operation::Upload` requests (`serde_json` encodes each `u8` as `1..=3` ASCII digits + `,`).
pub(crate) fn upload_transfer_json_bytes(bytes: &[u8]) -> u64 {
    if bytes.is_empty() {
        return 0;
    }
    let chunks = bytes.len().div_ceil(photocraft_party::protocol::CHUNK_BYTES) as u64;
    let body_digits: u64 = bytes
        .iter()
        .map(|&b| {
            if b < 10 {
                2u64
            } else if b < 100 {
                3u64
            } else {
                4u64
            }
        })
        .sum();
    chunks.saturating_mul(RPC_UPLOAD_HEADER_OVERHEAD).saturating_add(body_digits)
}

fn upload_chunk_ops(bytes_len: usize) -> usize {
    bytes_len.div_ceil(photocraft_party::protocol::CHUNK_BYTES)
}

fn check_submission_limits(objects: &BTreeMap<AssetHash, Vec<u8>>, submission_json_bytes: usize, freeze: bool) -> std::result::Result<(), String> {
    if objects.is_empty() || objects.len() > photocraft_party::MAX_ASSETS {
        return Err("Submission exceeds the 256-object limit; your saved local draft is preserved.".into());
    }
    if submission_json_bytes.saturating_add(RPC_APPLY_ENVELOPE_OVERHEAD) > photocraft_party::MAX_MESSAGE_BYTES {
        return Err("Submission metadata exceeds the 256 KiB room request limit; your saved local draft is preserved.".into());
    }
    let mut raw_total = 0u64;
    let mut ops_total = usize::from(freeze).saturating_add(1);
    let apply_count = u64::from(freeze).saturating_add(1);
    let mut outbox_json_total = apply_count.saturating_mul((submission_json_bytes.saturating_add(RPC_APPLY_ENVELOPE_OVERHEAD)) as u64);
    for bytes in objects.values() {
        let len = bytes.len() as u64;
        if len == 0 || len > photocraft_party::MAX_ASSET_BYTES {
            return Err(
                "This drawing exceeds the 64 MiB single-object upload limit; merge or simplify heavy imported layers in your saved local draft and try again."
                    .into(),
            );
        }
        raw_total = raw_total.saturating_add(len);
        ops_total = ops_total.saturating_add(upload_chunk_ops(bytes.len()));
        outbox_json_total = outbox_json_total.saturating_add(upload_transfer_json_bytes(bytes));
    }
    if raw_total > photocraft_party::MAX_PROJECT_BYTES {
        return Err("Submission exceeds the 512 MiB room storage budget; your saved local draft is preserved.".into());
    }
    if ops_total > MAX_SUBMISSION_OPERATIONS {
        return Err(
            "This drawing exceeds the room upload chunk limit (8,190 operations); merge or simplify heavy imported layers in your saved local draft and try again."
                .into(),
        );
    }
    if outbox_json_total > MAX_OUTBOX_PENDING_BYTES {
        return Err(
            "This drawing exceeds the 384 MiB pending transfer queue budget; merge or simplify heavy imported layers in your saved local draft and try again."
                .into(),
        );
    }
    Ok(())
}

fn replay_kind_label(kind: ReplayKind) -> &'static str {
    match kind {
        ReplayKind::Baseline => "baseline",
        ReplayKind::Resolved => "resolved",
        ReplayKind::Checkpoint => "checkpoint",
        ReplayKind::Undo => "undo",
        ReplayKind::Redo => "redo",
    }
}

pub(crate) fn sample_intermediate_revisions(final_revision: u64, max_intermediates: usize) -> Vec<u64> {
    if final_revision <= 2 || max_intermediates == 0 {
        return Vec::new();
    }
    let available = final_revision - 2;
    if available <= max_intermediates as u64 {
        return (2..final_revision).collect();
    }
    let slots = (max_intermediates as u128).saturating_add(1);
    let span = (final_revision - 1) as u128;
    let mut selected = Vec::with_capacity(max_intermediates);
    for step in 1..=max_intermediates {
        let numerator = (step as u128).saturating_mul(span).saturating_add(slots / 2);
        let revision = 1u64.saturating_add((numerator / slots) as u64).clamp(2, final_revision - 1);
        if selected.last().copied() != Some(revision) {
            selected.push(revision);
        }
    }
    selected
}

fn sample_indices(len: usize, target: usize) -> Vec<usize> {
    if target == 0 || len == 0 {
        return Vec::new();
    }
    if target >= len {
        return (0..len).collect();
    }
    let slots = target.saturating_add(1);
    let span = len.saturating_add(1);
    let mut indices = Vec::with_capacity(target);
    for step in 1..=target {
        let index = step.saturating_mul(span).saturating_add(slots / 2) / slots;
        let clamped = index.saturating_sub(1).min(len - 1);
        if indices.last().copied() != Some(clamped) {
            indices.push(clamped);
        }
    }
    indices
}

fn flatten_intermediate_keyframe(doc: &Document) -> std::result::Result<Document, String> {
    let width = doc.size.width;
    let row_bytes = usize::try_from(width).ok().and_then(|w| w.checked_mul(4)).ok_or("Keyframe canvas width overflow")?;
    let mut composite = photocraft_compose::flatten(doc);
    photocraft_party::canonical_color::to_srgb(doc, &mut composite.px).map_err(|e| e.to_string())?;
    let rendered = composite.to_rgba8();
    let mut flat = Document::new("Replay keyframe", doc.size, ColorMode::Rgb, SampleType::U8);
    flat.id = doc.id;
    flat.icc_profile = None;
    let mut surface = photocraft_raster::Surface::new(photocraft_color::PixelFormat::RGBA8);
    let strip_rows = 64usize;
    let strip_bytes = row_bytes.checked_mul(strip_rows).ok_or("Keyframe strip overflow")?;
    let mut samples = Vec::with_capacity(strip_bytes);
    let width_i32 = i32::try_from(width).map_err(|_| "Keyframe canvas width overflow")?;
    for (strip_index, pixels) in rendered.pixels.chunks(strip_bytes).enumerate() {
        if pixels.iter().all(|&sample| sample == 0) {
            continue;
        }
        let rows = pixels.len() / row_bytes;
        let y0 = i32::try_from(strip_index.saturating_mul(strip_rows)).map_err(|_| "Keyframe canvas height overflow")?;
        let y1 = y0.checked_add(i32::try_from(rows).map_err(|_| "Keyframe canvas height overflow")?).ok_or("Keyframe canvas height overflow")?;
        samples.clear();
        samples.extend(pixels.iter().map(|&sample| f32::from(sample) / 255.0));
        surface.try_write_region(photocraft_geom::Rect::new(0, y0, width_i32, y1), &samples).map_err(|e| e.to_string())?;
    }
    let mut layer = photocraft_doc::Layer::raster("Keyframe", flat.pixel_format());
    layer.id = photocraft_doc::LayerId(1);
    layer.content = photocraft_doc::LayerContent::Raster(surface);
    flat.layers.push(layer);
    Ok(flat)
}

struct MandatoryBudget {
    raw_bytes: u64,
    chunk_ops: usize,
    outbox_json_bytes: u64,
    asset_count: usize,
}

fn select_bounded_intermediates(candidates: Vec<SampledKeyframe>, keep: [AssetHash; 2], mandatory: &MandatoryBudget) -> Vec<SampledKeyframe> {
    if candidates.is_empty() {
        return Vec::new();
    }
    let mut target = candidates.len();
    loop {
        let indices = sample_indices(candidates.len(), target);
        let mut unique = BTreeMap::new();
        for &idx in &indices {
            if let Some(item) = candidates.get(idx) {
                let hash = item.checkpoint.asset.hash;
                if !keep.contains(&hash) {
                    unique.entry(hash).or_insert((item.checkpoint.asset.bytes, item.json_transfer_bytes));
                }
            }
        }
        let pack_bytes = if unique.is_empty() { 0 } else { unique.values().fold(10u64, |sum, &(bytes, _)| sum.saturating_add(72).saturating_add(bytes)) };
        let pack_json_bytes: u64 = unique.values().fold(if unique.is_empty() { 0 } else { 1024 }, |sum, &(bytes, json_bytes)| {
            let _ = bytes;
            sum.saturating_add(360).saturating_add(json_bytes)
        });
        let pack_chunks = if pack_bytes == 0 { 0 } else { upload_chunk_ops(pack_bytes as usize) };
        let manifest_est = ((indices.len().saturating_add(2) as u64).saturating_mul(320)).saturating_add(2048);
        let manifest_chunks = upload_chunk_ops(manifest_est as usize);
        let manifest_json = manifest_est.saturating_mul(4).saturating_add((manifest_chunks as u64).saturating_mul(RPC_UPLOAD_HEADER_OVERHEAD));
        let extra_assets = usize::from(pack_bytes > 0).saturating_add(1);

        let fits = pack_bytes <= MAX_PARTY_REPLAY_PACK_BYTES
            && mandatory.asset_count.saturating_add(extra_assets) <= photocraft_party::MAX_ASSETS
            && mandatory.raw_bytes.saturating_add(pack_bytes).saturating_add(manifest_est) <= photocraft_party::MAX_PROJECT_BYTES
            && mandatory.chunk_ops.saturating_add(pack_chunks).saturating_add(manifest_chunks) <= MAX_SUBMISSION_OPERATIONS
            && mandatory.outbox_json_bytes.saturating_add(pack_json_bytes).saturating_add(manifest_json) <= MAX_OUTBOX_PENDING_BYTES;
        if fits {
            let keep_set: std::collections::BTreeSet<usize> = indices.into_iter().collect();
            return candidates.into_iter().enumerate().filter(|(idx, _)| keep_set.contains(idx)).map(|(_, item)| item).collect();
        }
        if target <= 1 {
            return Vec::new();
        }
        target /= 2;
    }
}

fn make_replay_step(
    project: ProjectId,
    step_revision: u64,
    summary: &RevisionSummary,
    checkpoint: CheckpointRef,
) -> std::result::Result<ReplayArtifactStep, String> {
    let event = EventId::new(&short_hash(format!("{}:{}:event", project.as_str(), summary.revision).as_bytes())?).map_err(|e| e.to_string())?;
    Ok(ReplayArtifactStep {
        event,
        revision: step_revision,
        elapsed_ms: summary.elapsed_ms,
        command: summary.command.clone(),
        params: json!({}),
        kind: replay_kind_label(summary.kind).into(),
        checkpoint,
    })
}

pub(crate) fn prepare(store: &Store, request: &PrepareRequest) -> std::result::Result<Prepared, String> {
    if request.revision == 0 || request.revision > photocraft_party::MAX_REPLAY_EVENTS {
        return Err("Replay revision exceeds the submission limit".into());
    }
    // 1. Inspect endpoint revision metadata first before reading or decoding `.pcraft` blobs.
    let baseline_meta = store.revision_summary(request.project, 1).map_err(|e| e.to_string())?;
    baseline_meta
        .checkpoint
        .validate()
        .map_err(|_| "This drawing's baseline checkpoint exceeds the 64 MiB single-object upload limit; your saved local draft is preserved.".to_string())?;
    if request.revision > 1 {
        let final_meta = store.revision_summary(request.project, request.revision).map_err(|e| e.to_string())?;
        final_meta.checkpoint.validate().map_err(|_| {
            "This drawing's editable checkpoint exceeds the 64 MiB single-object upload limit; merge or simplify heavy imported layers in your saved local draft and try again."
                .to_string()
        })?;
    }

    let (baseline_doc, baseline_bytes, baseline_summary) = store.load_revision_bytes(request.project, 1).map_err(|e| e.to_string())?;
    request
        .mode
        .validate_extent(photocraft_party::ImageExtent { width: baseline_doc.size.width, height: baseline_doc.size.height })
        .map_err(|_| "A retained replay step has unsupported canvas dimensions; preserve this local draft for recovery".to_string())?;
    let baseline = checkpoint_ref_from_verified_bytes(baseline_summary.checkpoint, &baseline_bytes)?;

    let (final_doc, final_summary, final_checkpoint, final_bytes) = if request.revision == 1 {
        let bytes = baseline_bytes.clone();
        (baseline_doc, None, baseline, bytes)
    } else {
        let (doc, bytes, summary) = store.load_revision_bytes(request.project, request.revision).map_err(|e| e.to_string())?;
        request.mode.validate_extent(photocraft_party::ImageExtent { width: doc.size.width, height: doc.size.height }).map_err(|_| {
            "The assigned drawing's dimensions changed outside the supported assignment bounds; preserve this draft and restore a valid canvas size".to_string()
        })?;
        let checkpoint_ref = checkpoint_ref_from_verified_bytes(summary.checkpoint, &bytes)?;
        (doc, Some(summary), checkpoint_ref, bytes)
    };
    let (width, height) = (final_doc.size.width, final_doc.size.height);

    let mut composite = photocraft_compose::flatten(&final_doc);
    photocraft_party::canonical_color::to_srgb(&final_doc, &mut composite.px).map_err(|e| e.to_string())?;
    let rendered = composite.to_rgba8();
    let image_bytes = png(width, height, &rendered.pixels)?;
    let seam_bytes = if request.mode == Mode::ExquisiteCorpse {
        let seam_rows = photocraft_party::CORPSE_SEAM_HEIGHT;
        let top = height.checked_sub(seam_rows).ok_or("Seam bounds")?;
        let offset = usize::try_from(u64::from(top) * u64::from(width) * 4).map_err(|_| "Seam bounds")?;
        Some(png(width, seam_rows, rendered.pixels.get(offset..).ok_or("Seam bounds")?)?)
    } else {
        None
    };

    // 2. Verify the mandatory final drawing payload (endpoints + rendered PNG/seam + transfer overhead)
    // fits within all limits BEFORE loading or flattening any intermediate replay checkpoints.
    let mut mandatory_objects = BTreeMap::new();
    let _ = add(&mut mandatory_objects, baseline_bytes.clone())?;
    let _ = add(&mut mandatory_objects, final_bytes.clone())?;
    let _ = add(&mut mandatory_objects, image_bytes.clone())?;
    if let Some(seam) = &seam_bytes {
        let _ = add(&mut mandatory_objects, seam.clone())?;
    }
    let apply_count = u64::from(request.freeze).saturating_add(1);
    let est_submission_json = 4096usize.saturating_add(request.note.len());
    check_submission_limits(&mandatory_objects, est_submission_json, request.freeze)?;
    let mandatory_budget = MandatoryBudget {
        raw_bytes: mandatory_objects.values().map(|b| b.len() as u64).fold(0u64, u64::saturating_add),
        chunk_ops: mandatory_objects.values().map(|b| upload_chunk_ops(b.len())).fold(usize::from(request.freeze).saturating_add(1), usize::saturating_add),
        outbox_json_bytes: mandatory_objects
            .values()
            .map(|b| upload_transfer_json_bytes(b))
            .fold(apply_count.saturating_mul((est_submission_json.saturating_add(RPC_APPLY_ENVELOPE_OVERHEAD)) as u64), u64::saturating_add),
        asset_count: mandatory_objects.len(),
    };
    drop(mandatory_objects);

    // 3. Sample intermediate revision metadata first; deduplicate identical stored checkpoint hashes
    // so unchanged/undone steps avoid redundant `.pcraft` blob loads and flattening.
    let max_intermediates = MAX_PARTY_REPLAY_STEPS.saturating_sub(2);
    let sampled_revisions = sample_intermediate_revisions(request.revision, max_intermediates);
    let mut sampled_summaries = Vec::with_capacity(sampled_revisions.len());
    for source_revision in sampled_revisions {
        sampled_summaries.push(store.revision_summary(request.project, source_revision).map_err(|e| e.to_string())?);
    }
    let mut flattened_cache: BTreeMap<AssetHash, (CheckpointRef, Vec<u8>, u64)> = BTreeMap::new();
    let mut candidates = Vec::with_capacity(sampled_summaries.len());
    for summary in sampled_summaries {
        let (checkpoint_ref, bytes, json_transfer_bytes) = if summary.checkpoint.hash == baseline.asset.hash {
            (baseline, baseline_bytes.clone(), 0)
        } else if summary.checkpoint.hash == final_checkpoint.asset.hash {
            (final_checkpoint, final_bytes.clone(), 0)
        } else if let Some((cached_ref, cached_bytes, cached_json)) = flattened_cache.get(&summary.checkpoint.hash) {
            (*cached_ref, cached_bytes.clone(), *cached_json)
        } else {
            let (doc, _) = store.load_revision(request.project, summary.revision).map_err(|e| e.to_string())?;
            if request.mode.validate_extent(photocraft_party::ImageExtent { width: doc.size.width, height: doc.size.height }).is_err() {
                // Skip intermediate steps with transient non-matching dimensions once the final canvas size is restored.
                continue;
            }
            let flattened = flatten_intermediate_keyframe(&doc)?;
            let (checkpoint_ref, bytes) = encode_checkpoint_bytes(&flattened)?;
            let json_bytes = upload_transfer_json_bytes(&bytes);
            flattened_cache.insert(summary.checkpoint.hash, (checkpoint_ref, bytes.clone(), json_bytes));
            (checkpoint_ref, bytes, json_bytes)
        };
        candidates.push(SampledKeyframe { summary, checkpoint: checkpoint_ref, bytes, json_transfer_bytes });
    }
    drop(flattened_cache);

    let retained_intermediates = select_bounded_intermediates(candidates, [baseline.asset.hash, final_checkpoint.asset.hash], &mandatory_budget);

    let mut objects = BTreeMap::new();
    add_bounded(&mut objects, baseline_bytes, false)?;
    if final_checkpoint.asset.hash != baseline.asset.hash {
        add_bounded(&mut objects, final_bytes, false)?;
    }
    for item in &retained_intermediates {
        if !objects.contains_key(&item.checkpoint.asset.hash) {
            add_bounded(&mut objects, item.bytes.clone(), false)?;
        }
    }

    let replay_id = ReplayId::new(&short_hash(format!("{}:{}", request.project.as_str(), request.revision).as_bytes())?).map_err(|e| e.to_string())?;
    let mut artifact = ReplayArtifact { schema_version: 1, replay: replay_id, project: request.project, packs: Vec::new(), steps: Vec::new() };
    let mut metadata_bytes = 4096u64;
    let push_step = |steps: &mut Vec<ReplayArtifactStep>, metadata_bytes: &mut u64, step: ReplayArtifactStep| -> std::result::Result<(), String> {
        *metadata_bytes = metadata_bytes.saturating_add(serde_json::to_vec(&step).map_err(|e| e.to_string())?.len() as u64 + 1);
        if *metadata_bytes > photocraft_party::MAX_ASSET_BYTES {
            return Err("Replay package exceeds 64 MiB; local history is retained".into());
        }
        steps.push(step);
        Ok(())
    };
    push_step(&mut artifact.steps, &mut metadata_bytes, make_replay_step(request.project, 1, &baseline_summary, baseline)?)?;
    for item in retained_intermediates {
        let next_revision = (artifact.steps.len() as u64).saturating_add(1);
        push_step(&mut artifact.steps, &mut metadata_bytes, make_replay_step(request.project, next_revision, &item.summary, item.checkpoint)?)?;
    }
    if let Some(summary) = final_summary.as_ref() {
        let next_revision = (artifact.steps.len() as u64).saturating_add(1);
        push_step(&mut artifact.steps, &mut metadata_bytes, make_replay_step(request.project, next_revision, summary, final_checkpoint)?)?;
    }

    let last = artifact.steps.last().ok_or("Missing final replay step")?;
    let step_count = artifact.steps.len() as u64;
    let end_event = (step_count > 1).then_some(last.event);
    let duration_ms = last.elapsed_ms;
    artifact.packs = pack_history(&mut objects, [baseline.asset.hash, final_checkpoint.asset.hash])?;
    let manifest = add(&mut objects, serde_json::to_vec(&artifact).map_err(|e| e.to_string())?)?;
    let image = add(&mut objects, image_bytes)?;
    let seam = match seam_bytes {
        Some(bytes) => Some(add(&mut objects, bytes)?),
        None => None,
    };

    let submission = Submission {
        id: SubmissionId::new(&short_hash(
            format!(
                "{}:{}:{}:{}:{}:submission",
                request.assignment.game.as_str(),
                request.assignment.chain,
                request.assignment.round,
                request.project.as_str(),
                request.revision
            )
            .as_bytes(),
        )?)
        .map_err(|e| e.to_string())?,
        assignment: request.assignment,
        author: request.author,
        project: request.project,
        revision: step_count,
        image,
        checkpoint: final_checkpoint,
        replay: ReplayRef { id: replay_id, manifest: Some(manifest), baseline, end_event, event_count: step_count - 1, duration_ms },
        assets: objects.iter().map(|(hash, bytes)| AssetRef { hash: *hash, bytes: bytes.len() as u64 }).collect(),
        note: request.note.clone(),
        seam,
        extent: Some(photocraft_party::ImageExtent { width, height }),
    };
    submission.validate().map_err(|e| e.to_string())?;
    artifact.validate(&submission).map_err(|e| e.to_string())?;
    let submission_json_bytes = serde_json::to_vec(&submission).map_err(|e| e.to_string())?.len();
    check_submission_limits(&objects, submission_json_bytes, request.freeze)?;
    Ok(Prepared { submission, assets: objects.into_values().collect(), local_revision: request.revision, freeze: request.freeze })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn package_converts_linear_color_before_png_but_retains_native_profile_and_depth() {
        for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
            let dir = tempfile::tempdir().unwrap();
            let mut store = Store::open(dir.path().join("drafts.sqlite3"), Default::default()).unwrap();
            let project = photocraft_local_store::fresh_project_id().unwrap();
            let mut doc = Document::with_background("Linear gray", Size { width: 1024, height: 512 }, ColorMode::Rgb, depth, Color::rgb(0.5, 0.5, 0.5));
            let profile = photocraft_cms::Builtin::LinearSrgb.profile().to_bytes();
            doc.icc_profile = Some(profile.clone());
            store
                .save(&SaveRequest {
                    project,
                    expected_revision: 0,
                    request_id: photocraft_local_store::fresh_event_id().unwrap(),
                    title: "Linear gray".into(),
                    document: doc,
                    event: ReplayRecord { schema_version: 1, command: "file.new".into(), params: json!({}), elapsed_ms: 0, kind: ReplayKind::Baseline },
                })
                .unwrap();
            let request = PrepareRequest {
                assignment: AssignmentId { game: photocraft_party::GameId::new("11111111111111111111111111111111").unwrap(), chain: 0, round: 0 },
                author: ParticipantId::new("22222222222222222222222222222222").unwrap(),
                project,
                revision: 1,
                mode: Mode::ExquisiteCorpse,
                note: String::new(),
                freeze: false,
            };
            let prepared = prepare(&store, &request).unwrap();
            for asset in [prepared.submission.image, prepared.submission.seam.unwrap()] {
                let bytes = prepared.assets.iter().find(|b| hash(b) == asset.hash.as_str()).unwrap();
                let png = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).unwrap().into_rgba8();
                assert_eq!(png.get_pixel(0, 0).0, [188, 188, 188, 255], "{depth:?}");
            }
            let checkpoint = prepared.assets.iter().find(|b| hash(b) == prepared.submission.checkpoint.asset.hash.as_str()).unwrap();
            let reopened = photocraft_format::load_from_bytes(checkpoint).unwrap();
            assert_eq!(reopened.depth, depth);
            assert_eq!(reopened.icc_profile, Some(profile));
            assert!((photocraft_compose::flatten(&reopened).px[0][0] - 0.5).abs() < 0.003);
        }
    }
    #[test]
    fn hundreds_of_retained_edits_fit_bounded_replay_packs() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("drafts.sqlite3"), Default::default()).unwrap();
        let project = photocraft_local_store::fresh_project_id().unwrap();
        let mut document = open(&mut store, project, Mode::ExquisiteCorpse).unwrap().document;
        for revision in 2..=270 {
            document.layers.get_mut(0).unwrap().name = format!("Edit {revision}");
            store
                .save(&SaveRequest {
                    project,
                    expected_revision: revision - 1,
                    request_id: photocraft_local_store::fresh_event_id().unwrap(),
                    title: "Room drawing".into(),
                    document: document.clone(),
                    event: ReplayRecord {
                        schema_version: 1,
                        command: "layer.rename".into(),
                        params: json!({"name":format!("Edit {revision}")}),
                        elapsed_ms: revision,
                        kind: ReplayKind::Resolved,
                    },
                })
                .unwrap();
        }
        let req = PrepareRequest {
            assignment: AssignmentId { game: photocraft_party::GameId::new("11111111111111111111111111111111").unwrap(), chain: 0, round: 0 },
            author: ParticipantId::new("22222222222222222222222222222222").unwrap(),
            project,
            revision: 270,
            mode: Mode::ExquisiteCorpse,
            note: String::new(),
            freeze: false,
        };
        let prepared = prepare(&store, &req).unwrap();
        assert_eq!(prepared.local_revision, 270);
        assert!(prepared.assets.len() < 10);
        assert_eq!(prepared.submission.revision, MAX_PARTY_REPLAY_STEPS as u64);
        assert_eq!(prepared.submission.replay.event_count, (MAX_PARTY_REPLAY_STEPS - 1) as u64);
        assert_eq!(prepared.submission.replay.duration_ms, 270);
        let manifest = prepared.submission.replay.manifest.unwrap();
        let bytes = prepared.assets.iter().find(|bytes| hash(bytes) == manifest.hash.as_str()).unwrap();
        let artifact = ReplayArtifact::decode(bytes, &prepared.submission).unwrap();
        assert_eq!(artifact.steps.len(), MAX_PARTY_REPLAY_STEPS);
        assert!(artifact.steps.iter().all(|step| step.params == json!({})));
        let packed = artifact
            .packs
            .iter()
            .map(|pack| {
                let bytes = prepared.assets.iter().find(|bytes| hash(bytes) == pack.hash.as_str()).unwrap();
                photocraft_party::replay_artifact::unpack_checkpoints(bytes).unwrap().len()
            })
            .sum::<usize>();
        assert!((1..=MAX_PARTY_REPLAY_STEPS - 2).contains(&packed));
        let final_bytes = prepared.assets.iter().find(|bytes| hash(bytes) == prepared.submission.checkpoint.asset.hash.as_str()).unwrap();
        let final_doc = photocraft_format::load_from_bytes(final_bytes).unwrap();
        assert_eq!(final_doc.layers[0].name, "Edit 270");
        assert_eq!(store.load(project).unwrap().info.revision, 270, "local store retains full history");
    }
    #[test]
    fn assigned_draft_reopens_and_package_retains_exact_history_and_seam() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("drafts.sqlite3"), Default::default()).unwrap();
        let project = photocraft_local_store::fresh_project_id().unwrap();
        let baseline = open(&mut store, project, Mode::ExquisiteCorpse).unwrap();
        let mut doc = baseline.document.clone();
        doc.layers.get_mut(0).unwrap().name = "Renamed, then undone".into();
        let change = SaveRequest {
            project,
            expected_revision: 1,
            request_id: photocraft_local_store::fresh_event_id().unwrap(),
            title: "Room drawing".into(),
            document: doc.clone(),
            event: ReplayRecord {
                schema_version: 1,
                command: "layer.rename".into(),
                params: json!({"name":"Renamed, then undone"}),
                elapsed_ms: 100,
                kind: ReplayKind::Resolved,
            },
        };
        store.save(&change).unwrap();
        let mut undo = change;
        undo.expected_revision = 2;
        undo.request_id = photocraft_local_store::fresh_event_id().unwrap();
        undo.document = baseline.document;
        undo.event = ReplayRecord { schema_version: 1, command: "edit.undo".into(), params: json!({}), elapsed_ms: 200, kind: ReplayKind::Undo };
        store.save(&undo).unwrap();
        assert_eq!(open(&mut store, project, Mode::ExquisiteCorpse).unwrap().info.revision, 3);
        let req = PrepareRequest {
            assignment: AssignmentId { game: photocraft_party::GameId::new("11111111111111111111111111111111").unwrap(), chain: 0, round: 0 },
            author: ParticipantId::new("22222222222222222222222222222222").unwrap(),
            project,
            revision: 3,
            mode: Mode::ExquisiteCorpse,
            note: "metadata only".into(),
            freeze: false,
        };
        let package = prepare(&store, &req).unwrap();
        let manifest = package.submission.replay.manifest.unwrap();
        let bytes = package.assets.iter().find(|bytes| hash(bytes) == manifest.hash.as_str()).unwrap();
        let artifact = ReplayArtifact::decode(bytes, &package.submission).unwrap();
        assert_eq!(artifact.steps.len(), 3);
        assert_eq!(artifact.steps[1].command, "layer.rename");
        assert_eq!(artifact.steps[2].kind, "undo");
        assert_eq!(artifact.steps[0].checkpoint.asset, artifact.steps[2].checkpoint.asset);
        assert_ne!(package.submission.seam.unwrap().hash, package.submission.image.hash);
        let mut bad = package.submission.clone();
        bad.replay.event_count = 1;
        assert!(artifact.validate(&bad).is_err());
        drop(store);
        let reopened = Store::open(dir.path().join("drafts.sqlite3"), Default::default()).unwrap();
        assert_eq!(reopened.load(project).unwrap().info.revision, 3);
    }
}
