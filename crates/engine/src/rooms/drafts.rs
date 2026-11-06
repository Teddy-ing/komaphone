//! Assigned drafts and exact durable submission packages, prepared on the local I/O worker.
use crate::{EngineError, Result, Session};
use image::ImageEncoder;
use photocraft_doc::{Color, ColorMode, Document, SampleType, Size};
use photocraft_local_store::{LoadedDraft, ReplayKind, ReplayRecord, SaveRequest, Store};
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
pub(crate) struct Prepared {
    pub submission: Submission,
    pub assets: Vec<Vec<u8>>,
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
                self.rooms.status.error = Some(error.to_string());
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
            self.rooms.status.error = Some(error.to_string());
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
fn checkpoint(objects: &mut BTreeMap<AssetHash, Vec<u8>>, doc: &Document) -> std::result::Result<CheckpointRef, String> {
    let bytes = photocraft_format::save_to_bytes(doc, &Default::default()).map_err(|e| e.to_string())?;
    let id = CheckpointId::new(&short_hash(&bytes)?).map_err(|e| e.to_string())?;
    Ok(CheckpointRef { id, asset: add_bounded(objects, bytes, false)? })
}
fn pack_history(objects: &mut BTreeMap<AssetHash, Vec<u8>>, keep: [AssetHash; 2]) -> std::result::Result<Vec<AssetRef>, String> {
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
pub(crate) fn prepare(store: &Store, request: &PrepareRequest) -> std::result::Result<Prepared, String> {
    if request.revision == 0 || request.revision > photocraft_party::MAX_REPLAY_EVENTS {
        return Err("Replay revision exceeds the submission limit".into());
    }
    let mut objects = BTreeMap::new();
    let replay_id = ReplayId::new(&short_hash(format!("{}:{}", request.project.as_str(), request.revision).as_bytes())?).map_err(|e| e.to_string())?;
    let mut artifact = ReplayArtifact { schema_version: 1, replay: replay_id, project: request.project, packs: Vec::new(), steps: Vec::new() };
    let mut metadata_bytes = 4096u64;
    for revision in 1..=request.revision {
        let (doc, _) = store.load_revision(request.project, revision).map_err(|e| e.to_string())?;
        if (doc.size.width, doc.size.height) != dimensions(request.mode) {
            return Err("A retained replay step has different canvas dimensions; preserve this local draft for recovery".into());
        }
        let checkpoint = checkpoint(&mut objects, &doc)?;
        let records = store.replay_page(request.project, revision - 1, 1).map_err(|e| e.to_string())?;
        let record = records.into_iter().next().ok_or("Missing retained replay record")?;
        let event = EventId::new(&short_hash(format!("{}:{revision}:event", request.project.as_str()).as_bytes())?).map_err(|e| e.to_string())?;
        let kind = match record.kind {
            ReplayKind::Baseline => "baseline",
            ReplayKind::Resolved => "resolved",
            ReplayKind::Checkpoint => "checkpoint",
            ReplayKind::Undo => "undo",
            ReplayKind::Redo => "redo",
        };
        let step = ReplayArtifactStep {
            event,
            revision,
            elapsed_ms: record.elapsed_ms,
            command: record.command,
            params: record.params,
            kind: kind.into(),
            checkpoint,
        };
        // Bound serialized metadata throughout construction, before retaining a large package.
        metadata_bytes = metadata_bytes.saturating_add(serde_json::to_vec(&step).map_err(|e| e.to_string())?.len() as u64 + 1);
        if metadata_bytes > photocraft_party::MAX_ASSET_BYTES {
            return Err("Replay package exceeds 64 MiB; local history is retained".into());
        }
        artifact.steps.push(step);
    }
    let first = artifact.steps.first().ok_or("Missing baseline")?;
    let last = artifact.steps.last().ok_or("Missing final replay step")?;
    let baseline = first.checkpoint;
    let final_checkpoint = last.checkpoint;
    let end_event = (request.revision > 1).then_some(last.event);
    let duration_ms = last.elapsed_ms;
    artifact.packs = pack_history(&mut objects, [baseline.asset.hash, final_checkpoint.asset.hash])?;
    let manifest = add(&mut objects, serde_json::to_vec(&artifact).map_err(|e| e.to_string())?)?;
    let (doc, _) = store.load_revision(request.project, request.revision).map_err(|e| e.to_string())?;
    let (width, height) = dimensions(request.mode);
    if (doc.size.width, doc.size.height) != (width, height) {
        return Err("The assigned drawing's dimensions changed; preserve this draft and restore its required canvas size".into());
    }
    let mut composite = photocraft_compose::flatten(&doc);
    photocraft_party::canonical_color::to_srgb(&doc, &mut composite.px).map_err(|e| e.to_string())?;
    let rendered = composite.to_rgba8();
    let image = add(&mut objects, png(width, height, &rendered.pixels)?)?;
    let seam = if request.mode == Mode::ExquisiteCorpse {
        let offset = usize::try_from(u64::from(height - 24) * u64::from(width) * 4).map_err(|_| "Seam bounds")?;
        Some(add(&mut objects, png(width, 24, rendered.pixels.get(offset..).ok_or("Seam bounds")?)?)?)
    } else {
        None
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
        revision: request.revision,
        image,
        checkpoint: final_checkpoint,
        replay: ReplayRef { id: replay_id, manifest: Some(manifest), baseline, end_event, event_count: request.revision - 1, duration_ms },
        assets: objects.iter().map(|(hash, bytes)| AssetRef { hash: *hash, bytes: bytes.len() as u64 }).collect(),
        note: request.note.clone(),
        seam,
    };
    submission.validate().map_err(|e| e.to_string())?;
    artifact.validate(&submission).map_err(|e| e.to_string())?;
    Ok(Prepared { submission, assets: objects.into_values().collect(), freeze: request.freeze })
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
        assert!(prepared.assets.len() < 10);
        assert_eq!(prepared.submission.replay.event_count, 269);
        let manifest = prepared.submission.replay.manifest.unwrap();
        let bytes = prepared.assets.iter().find(|bytes| hash(bytes) == manifest.hash.as_str()).unwrap();
        let artifact = ReplayArtifact::decode(bytes, &prepared.submission).unwrap();
        let packed = artifact
            .packs
            .iter()
            .map(|pack| {
                let bytes = prepared.assets.iter().find(|bytes| hash(bytes) == pack.hash.as_str()).unwrap();
                photocraft_party::replay_artifact::unpack_checkpoints(bytes).unwrap().len()
            })
            .sum::<usize>();
        assert_eq!(packed, 268);
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
