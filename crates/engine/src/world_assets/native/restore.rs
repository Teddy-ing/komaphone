//! One bounded archive upload at a time through the existing authenticated worker.
use super::{Command, Event, ExportStatus, Scope, WorldExport, WorldFileAccess, current, error};
use crate::{Result, Session, world_media};
use photocraft_scene::{
    shared::{OpId, WorldState, archive_import::MAX_CHUNK_BYTES},
    world_protocol as wire,
};
use std::{
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, RecvTimeoutError, SyncSender},
    },
    time::{Duration, Instant},
};

pub(super) struct Job {
    pub scope: Scope,
    action: WorldExport,
    files: Option<Arc<dyn WorldFileAccess>>,
    cancel: Arc<AtomicBool>,
    commit_started: AtomicBool,
}

pub(super) fn start(session: &mut Session, action: WorldExport, files: Option<Arc<dyn WorldFileAccess>>) -> Result<()> {
    let WorldExport::Restore { canvas, .. } = &action else { return Err(error("Expected archive restore")) };
    if !session.worlds.connected() {
        return Err(error("Sign in to restore an archive as a private canvas"));
    }
    if session.worlds.assets.export.as_ref().is_some_and(|status| status.busy) {
        return Err(error("A world export or restore is already running"));
    }
    let account = session.worlds.account.clone().ok_or_else(|| error("Sign in before restoring an archive"))?;
    // A restored destination has no live canvas yet. When a canvas is open,
    // retain its cache scope so ordinary repaint requests cannot cancel import.
    let scope_canvas = session.worlds.active.as_ref().map_or(*canvas, |active| active.info.canvas);
    let assets = &mut session.worlds.assets;
    if assets.inner.scope.as_ref().is_some_and(|scope| scope.account != account || scope.canvas != scope_canvas) {
        assets.reset();
    }
    let scope = Scope { account, canvas: scope_canvas, generation: assets.inner.generation };
    assets.inner.scope = Some(scope.clone());
    let cancel = Arc::new(AtomicBool::new(false));
    assets.inner.send(Command::Import(Job { scope, action, files, cancel: Arc::clone(&cancel), commit_started: AtomicBool::new(false) }))?;
    assets.inner.export_cancel = Some(cancel);
    assets.error = None;
    assets.export = Some(ExportStatus { busy: true, completed: 0, total: 0, message: "Validating portable archive…".into() });
    Ok(())
}
impl Job {
    fn alive(&self, generation: &AtomicU64) -> bool {
        current(&self.scope, generation) && !self.cancel.load(Ordering::Acquire)
    }
    fn check(&self, generation: &AtomicU64) -> Result<()> {
        if self.alive(generation) { Ok(()) } else { Err(error("Archive restore cancelled; original files are unchanged")) }
    }
    fn request(&self, action: wire::Action, events: &SyncSender<Event>, generation: &AtomicU64) -> Result<wire::Response> {
        self.check(generation)?;
        let committing = matches!(action, wire::Action::ImportCommit { .. });
        if committing {
            self.commit_started.store(true, Ordering::Release);
        }
        let (reply, receive) = mpsc::sync_channel(1);
        events.send(Event::Import { scope: self.scope.clone(), action: Box::new(action), reply, cancel: Arc::clone(&self.cancel) }).map_err(error)?;
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            self.check(generation)?;
            match receive.recv_timeout(Duration::from_millis(100)) {
                Ok(Ok(response)) => return Ok(response),
                Ok(Err(message)) => {
                    return Err(error(if committing {
                        format!("Restore confirmation failed: {message}. Check the destination canvas before retrying with the same destination ID")
                    } else {
                        message
                    }));
                }
                Err(RecvTimeoutError::Disconnected) => return Err(error("Restore account changed or request ended; original archive is unchanged")),
                Err(RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                Err(RecvTimeoutError::Timeout) => return Err(error("Restore confirmation timed out; inspect the destination before retrying its same ID")),
            }
        }
    }
    fn progress(&self, events: &SyncSender<Event>, completed: u64, total: u64, message: String, busy: bool) -> Result<()> {
        events
            .send(Event::Progress {
                scope: self.scope.clone(),
                status: ExportStatus { busy, completed: u32::try_from(completed).map_err(error)?, total: u32::try_from(total).map_err(error)?, message },
            })
            .map_err(error)
    }
}

fn uploaded(response: wire::Response, id: OpId, object: &wire::ImportObject, minimum: u64, total: u64) -> Result<u64> {
    match response {
        wire::Response::ImportProgress { id: returned, object: returned_object, received, total: returned_total }
            if returned == id && &returned_object == object && received >= minimum && received <= total && returned_total == total =>
        {
            Ok(received)
        }
        _ => Err(error("Service returned an invalid archive upload offset or object")),
    }
}
fn completed(info: &wire::CanvasSummary, expected: &WorldState) -> Result<()> {
    if info.canvas != expected.canvas
        || info.owner != expected.owner
        || info.revision < expected.revision
        || info.import_origin != expected.import_origin
        || info.initial_name != expected.replay_to(0).map_err(error)?.name
        || info.epoch == 0
    {
        return Err(error("Service restored a different canvas or archive origin"));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn upload_file(
    job: &Job,
    path: &Path,
    object: wire::ImportObject,
    id: OpId,
    total: u64,
    mut offset: u64,
    events: &SyncSender<Event>,
    generation: &AtomicU64,
) -> Result<()> {
    let mut file = world_media::archive_regular_file(path)?;
    if file.metadata().map_err(error)?.len() != total {
        return Err(error("Archive object changed before upload"));
    }
    let mut bytes = vec![0_u8; MAX_CHUNK_BYTES];
    while offset < total {
        job.check(generation)?;
        let length = usize::try_from((total - offset).min(MAX_CHUNK_BYTES as u64)).map_err(error)?;
        file.seek(SeekFrom::Start(offset)).map_err(error)?;
        let chunk = bytes.get_mut(..length).ok_or_else(|| error("Invalid upload chunk length"))?;
        file.read_exact(chunk).map_err(error)?;
        let response = job.request(wire::Action::ImportChunk { id, object: object.clone(), offset, bytes: chunk.to_vec() }, events, generation)?;
        offset = uploaded(response, id, &object, offset.saturating_add(length as u64), total)?;
    }
    Ok(())
}

pub(super) fn run(job: Job, events: &SyncSender<Event>, generation: &AtomicU64) -> Result<()> {
    let result = run_inner(&job, events, generation);
    if result.is_err()
        && current(&job.scope, generation)
        && !job.commit_started.load(Ordering::Acquire)
        && let WorldExport::Restore { canvas, .. } = &job.action
        && let Ok(id) = OpId::new(canvas.as_str())
    {
        // Best effort only. A switched account must never send cleanup using its
        // new credentials; server expiry also bounds abandoned staging.
        let (reply, receive) = mpsc::sync_channel(1);
        if events
            .send(Event::Import {
                scope: job.scope.clone(),
                action: Box::new(wire::Action::ImportCancel { id }),
                reply,
                cancel: Arc::new(AtomicBool::new(false)),
            })
            .is_ok()
        {
            let _ = receive.recv_timeout(Duration::from_secs(2));
        }
    }
    result
}

fn run_inner(job: &Job, events: &SyncSender<Event>, generation: &AtomicU64) -> Result<()> {
    job.check(generation)?;
    let mut prepared = job.files.as_ref().map(|files| files.prepare(&job.action)).transpose()?;
    let action = prepared.as_ref().map_or_else(|| job.action.clone(), |files| files.action());
    if action.clone().with_path(PathBuf::new()) != job.action.clone().with_path(PathBuf::new()) {
        return Err(error("File adapter changed the requested restore destination"));
    }
    let WorldExport::Restore { path, canvas } = action else { return Err(error("Expected archive restore")) };
    let upload = world_media::inspect_archive_for_upload(&path, &mut || job.alive(generation))?;
    let expected = upload.world.import_to(canvas, job.scope.account.profile).map_err(error)?;
    let id = OpId::new(canvas.as_str()).map_err(error)?;
    job.check(generation)?;
    let begin = job.request(
        wire::Action::ImportBegin {
            id,
            new_canvas: canvas,
            manifest_sha256: upload.manifest_hash,
            manifest_bytes: upload.manifest_bytes,
            total_bytes: upload.total_bytes,
        },
        events,
        generation,
    )?;
    if let wire::Response::Canvas { info } = &begin {
        completed(info, &expected)?;
    } else {
        let offset = uploaded(begin, id, &wire::ImportObject::Manifest {}, 0, upload.manifest_bytes)?;
        upload_file(job, &path.join("world.json"), wire::ImportObject::Manifest {}, id, upload.manifest_bytes, offset, events, generation)?;
        let mut sent = upload.manifest_bytes;
        for frame in &upload.assets {
            upload_file(
                job,
                &path.join(format!("{}.png", frame.hash.as_str())),
                wire::ImportObject::Asset { hash: frame.hash },
                id,
                frame.byte_len,
                0,
                events,
                generation,
            )?;
            sent = sent.saturating_add(frame.byte_len);
            job.progress(events, sent, upload.total_bytes, "Uploading verified archive objects…".into(), true)?;
        }
        // After this request is queued the UI disallows cancellation: a network
        // failure cannot prove whether the atomic service commit took place.
        match job.request(wire::Action::ImportCommit { id }, events, generation)? {
            wire::Response::Canvas { info } => completed(&info, &expected)?,
            _ => return Err(error("Invalid final archive restore response; inspect the destination before retrying")),
        }
    }
    if let Some(files) = &mut prepared {
        files.finish()?;
    }
    job.progress(events, upload.total_bytes, upload.total_bytes, format!("Restored private canvas {}; original archive is unchanged", canvas.as_str()), false)
}

#[cfg(test)]
mod tests;
