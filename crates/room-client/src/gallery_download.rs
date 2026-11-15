//! Read-only, bounded historical downloads. Each chunk is authorized by the service.
use crate::{ClientEvent, Error, Outbox, Result, Transport};
use photocraft_party::gallery::{GalleryBundle, GalleryManifest, MAX_GALLERY_BYTES};
use photocraft_party::protocol::{CHUNK_BYTES, Operation, PROTOCOL_VERSION, RpcRequest, RpcResponse};
use photocraft_party::{AssetRef, GameId, ProjectId, RequestId, RoomId};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

struct Job {
    endpoint: String,
    room: RoomId,
    game: GameId,
    timestamp: String,
    bundle: Option<GalleryBundle>,
    needed: Vec<AssetRef>,
    partial: Vec<u8>,
    received: u64,
    total: u64,
}

#[derive(Default)]
pub(crate) struct Downloads {
    job: Option<Job>,
    retry_at: Option<Instant>,
}

fn call(store: &Outbox, transport: &Transport, operation: Operation) -> Result<RpcResponse> {
    let status = store.status();
    let endpoint = status.endpoint.as_deref().ok_or(Error::Unavailable)?;
    let request = RpcRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(&crate::store::random_hex(16)?).map_err(|e| Error::Invalid(e.to_string()))?,
        operation,
    };
    transport.call(endpoint, &store.credential()?, &request)
}

pub(crate) fn history(store: &Outbox, transport: &Transport, room: RoomId, after_revision: u64) -> Result<ClientEvent> {
    match call(store, transport, Operation::History { room, after_revision })? {
        RpcResponse::History { games, next_revision } if games.len() <= 16 => {
            let mut previous = after_revision;
            let mut ids = std::collections::BTreeSet::new();
            for game in &games {
                if game.revision <= previous || !ids.insert(game.game) || !(2..=3).contains(&game.authors.len()) {
                    return Err(Error::Invalid("Invalid archived game list".into()));
                }
                for author in &game.authors {
                    author.validate().map_err(|e| Error::Invalid(e.to_string()))?;
                }
                game.mode.validate().map_err(|e| Error::Invalid(e.to_string()))?;
                previous = game.revision;
            }
            if next_revision.is_some_and(|next| games.is_empty() || next != previous) {
                return Err(Error::Invalid("Invalid archive page cursor".into()));
            }
            Ok(ClientEvent::History { room, games, next_revision })
        }
        _ => Err(Error::Invalid("Expected an authorized game history".into())),
    }
}

pub(crate) fn notes(store: &Outbox, transport: &Transport, room: RoomId, game: GameId) -> Result<ClientEvent> {
    match call(store, transport, Operation::GameNotes { room, game })? {
        RpcResponse::Notes { notes } if notes.len() <= 36 => {
            let mut seen = std::collections::BTreeSet::new();
            for (assignment, note) in &notes {
                assignment.validate().map_err(|e| Error::Invalid(e.to_string()))?;
                photocraft_party::validate_label(note, photocraft_party::MAX_NOTE_BYTES, true).map_err(|e| Error::Invalid(e.to_string()))?;
                if assignment.game != game || !seen.insert(*assignment) {
                    return Err(Error::Invalid("Invalid historical notes".into()));
                }
            }
            Ok(ClientEvent::GalleryNotes { game, notes })
        }
        _ => Err(Error::Invalid("Expected authorized historical notes".into())),
    }
}

impl Downloads {
    pub(crate) fn start(&mut self, store: &Outbox, room: RoomId, game: GameId, timestamp: String) -> Result<()> {
        if self.job.is_some() {
            return Err(Error::Busy);
        }
        photocraft_party::validate_label(&timestamp, 64, false).map_err(|e| Error::Invalid(e.to_string()))?;
        self.job = Some(Job {
            endpoint: store.status().endpoint.ok_or(Error::Unavailable)?,
            room,
            game,
            timestamp,
            bundle: None,
            needed: Vec::new(),
            partial: Vec::new(),
            received: 0,
            total: 0,
        });
        self.retry_at = None;
        Ok(())
    }

    pub(crate) fn cancel(&mut self) {
        self.job = None;
        self.retry_at = None;
    }

    pub(crate) fn step(&mut self, store: &Outbox, transport: &Transport, events: &SyncSender<ClientEvent>) {
        if self.job.is_none() || self.retry_at.is_some_and(|retry| Instant::now() < retry) {
            return;
        }
        match self.advance(store, transport, events) {
            Ok(()) => self.retry_at = None,
            Err(Error::Unavailable) => {
                self.retry_at = Instant::now().checked_add(Duration::from_secs(1));
                let _ = events.try_send(ClientEvent::GalleryProgress {
                    received: self.job.as_ref().map_or(0, |job| job.received),
                    total: self.job.as_ref().map_or(0, |job| job.total),
                    waiting: true,
                });
            }
            Err(error) => {
                // Keep errors until delivered; do not report a partial download as saved.
                match events.try_send(ClientEvent::GalleryError(error.to_string())) {
                    Ok(()) => self.cancel(),
                    Err(_) => self.retry_at = Instant::now().checked_add(Duration::from_secs(1)),
                }
            }
        }
    }

    fn advance(&mut self, store: &Outbox, transport: &Transport, events: &SyncSender<ClientEvent>) -> Result<()> {
        let Some(job) = &mut self.job else { return Ok(()) };
        let status = store.status();
        if status.endpoint.as_ref() != Some(&job.endpoint) {
            return Err(Error::Invalid("Service changed during gallery download; retry from its original room".into()));
        }
        if job.bundle.is_none() {
            let actor = status.profile.as_ref().ok_or(Error::Unavailable)?.id;
            let RpcResponse::Game { view } = call(store, transport, Operation::Game { room: job.room, game: job.game })? else {
                return Err(Error::Invalid("Expected a revealed game".into()));
            };
            if view.id != job.room {
                return Err(Error::Invalid("The service returned a different room".into()));
            }
            let manifest = GalleryManifest::from_reveal(
                ProjectId::new(job.game.as_str()).map_err(|e| Error::Invalid(e.to_string()))?,
                &view,
                actor,
                job.timestamp.clone(),
            )
            .map_err(|e| Error::Invalid(e.to_string()))?;
            if manifest.game() != Some(job.game) {
                return Err(Error::Invalid("The service returned a different game".into()));
            }
            job.needed = manifest.required_assets().map_err(|e| Error::Invalid(e.to_string()))?;
            job.total = job.needed.iter().try_fold(0u64, |sum, asset| {
                sum.checked_add(asset.bytes)
                    .filter(|n| *n <= MAX_GALLERY_BYTES)
                    .ok_or_else(|| Error::Invalid("Gallery game exceeds the 512 MiB download limit; existing originals are preserved".into()))
            })?;
            job.bundle = Some(GalleryBundle { manifest, objects: BTreeMap::new() });
        }
        if let Some(asset) = job.needed.last().copied() {
            let revision = job
                .bundle
                .as_ref()
                .and_then(|bundle| match &bundle.manifest.content {
                    photocraft_party::gallery::GalleryContent::Game { room } => Some(room.revision),
                    _ => None,
                })
                .ok_or_else(|| Error::Invalid("Missing archived room revision".into()))?;
            let RpcResponse::AssetChunk { bytes, total } =
                call(store, transport, Operation::GameFetch { room: job.room, game: job.game, revision, hash: asset.hash, offset: job.partial.len() as u64 })?
            else {
                return Err(Error::Invalid("Expected an authorized game asset".into()));
            };
            let count = bytes.len() as u64;
            let complete = append_chunk(asset, &mut job.partial, &bytes, total)?;
            job.received = job.received.saturating_add(count);
            if complete {
                if let Some(bundle) = &mut job.bundle {
                    bundle.objects.insert(asset.hash, std::mem::take(&mut job.partial));
                }
                job.needed.pop();
            }
            let _ = events.try_send(ClientEvent::GalleryProgress { received: job.received, total: job.total, waiting: false });
        }
        if job.needed.is_empty()
            && let Some(bundle) = job.bundle.take()
        {
            bundle.validate().map_err(|e| Error::Invalid(e.to_string()))?;
            match events.try_send(ClientEvent::Gallery(Box::new(bundle))) {
                Ok(()) => self.job = None,
                Err(std::sync::mpsc::TrySendError::Full(ClientEvent::Gallery(bundle))) => job.bundle = Some(*bundle),
                Err(_) => self.job = None,
            }
        }
        Ok(())
    }
}

fn append_chunk(asset: AssetRef, partial: &mut Vec<u8>, bytes: &[u8], total: u64) -> Result<bool> {
    asset.validate().map_err(|e| Error::Invalid(e.to_string()))?;
    if total != asset.bytes || bytes.is_empty() || bytes.len() > CHUNK_BYTES || (partial.len() as u64).saturating_add(bytes.len() as u64) > total {
        return Err(Error::Invalid("Invalid archived asset length".into()));
    }
    partial.try_reserve(bytes.len()).map_err(|_| Error::Invalid("Insufficient memory for gallery download".into()))?;
    partial.extend_from_slice(bytes);
    let complete = partial.len() as u64 == total;
    if complete && format!("{:x}", Sha256::digest(&*partial)) != asset.hash.as_str() {
        return Err(Error::Invalid("Archived asset checksum failed".into()));
    }
    Ok(complete)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn asset(bytes: &[u8]) -> AssetRef {
        AssetRef { hash: photocraft_party::AssetHash::new(&format!("{:x}", Sha256::digest(bytes))).unwrap(), bytes: bytes.len() as u64 }
    }
    #[test]
    fn gallery_chunks_never_complete_before_exact_hash_and_length() {
        let reference = asset(b"abcdef");
        let mut partial = Vec::new();
        assert!(!append_chunk(reference, &mut partial, b"abc", 6).unwrap());
        let before = partial.clone();
        for (chunk, total) in [(b"".as_slice(), 6), (b"def", 7), (b"defg", 6)] {
            assert!(append_chunk(reference, &mut partial, chunk, total).is_err());
            assert_eq!(partial, before);
        }
        assert!(append_chunk(reference, &mut partial, b"def", 6).unwrap());
        assert_eq!(partial, b"abcdef");
        assert!(append_chunk(reference, &mut partial, b"def", 6).is_err());
        let mut reordered = Vec::new();
        assert!(!append_chunk(reference, &mut reordered, b"def", 6).unwrap());
        assert!(append_chunk(reference, &mut reordered, b"abc", 6).is_err());
    }
    #[test]
    fn oversized_chunks_are_rejected_before_allocation_and_cancel_drops_pending_work() {
        let bytes = vec![5; CHUNK_BYTES + 1];
        let mut partial = Vec::new();
        assert!(append_chunk(asset(&bytes), &mut partial, &bytes, bytes.len() as u64).is_err());
        assert!(partial.is_empty());
        let mut downloads = Downloads {
            job: Some(Job {
                endpoint: "http://127.0.0.1:1".into(),
                room: RoomId::new(&"1".repeat(32)).unwrap(),
                game: GameId::new(&"2".repeat(32)).unwrap(),
                timestamp: "2026-11-06T00:00:00Z".into(),
                bundle: None,
                needed: Vec::new(),
                partial: vec![1, 2, 3],
                received: 3,
                total: 100,
            }),
            retry_at: Some(Instant::now()),
        };
        downloads.cancel();
        assert!(downloads.job.is_none());
        assert!(downloads.retry_at.is_none());
    }
}
