//! Bounded, ephemeral presentation downloads. Reconcile authorization before every chunk.
use crate::{ClientEvent, Error, Outbox, Result, Transport, media};
use photocraft_party::protocol::{CHUNK_BYTES, Operation, PROTOCOL_VERSION, RpcRequest, RpcResponse};
use photocraft_party::{AssetHash, AssetRef, RequestId, RoomId};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{Arc, mpsc::SyncSender};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(crate) struct Downloads {
    room: Option<RoomId>,
    complete: BTreeMap<AssetHash, Complete>,
    partial: Option<(AssetRef, Vec<u8>)>,
    retry: Option<(AssetRef, Instant)>,
    failures: u32,
}
struct Complete {
    asset: AssetRef,
    bytes: Arc<Vec<u8>>,
    acknowledged: bool,
    next_publication: Option<Instant>,
}
impl Downloads {
    /// The consumer acknowledges only after accepting the image under its latest
    /// projection. A queued event is not proof that the renderer could authorize it.
    pub(crate) fn acknowledge(&mut self, room: RoomId, hash: AssetHash) {
        if self.room == Some(room)
            && let Some(complete) = self.complete.get_mut(&hash)
        {
            complete.acknowledged = true;
        }
    }
    pub fn step(&mut self, store: &Outbox, transport: &Transport, events: &SyncSender<ClientEvent>) -> Result<()> {
        let status = store.status();
        let Some((room, actor)) = status.room.as_ref().zip(status.profile.as_ref()) else {
            *self = Self::default();
            return Ok(());
        };
        if self.room != Some(room.id) {
            *self = Self { room: Some(room.id), ..Self::default() };
        }
        let allowed = media::images(room, actor.id);
        self.complete.retain(|_, complete| allowed.contains(&complete.asset));
        if self.partial.as_ref().is_some_and(|(asset, _)| !allowed.contains(asset)) {
            self.partial = None;
        }
        if self.retry.as_ref().is_some_and(|(asset, _)| !allowed.contains(asset)) {
            self.retry = None;
            self.failures = 0;
        }
        let now = Instant::now();
        for complete in self.complete.values_mut() {
            if !complete.acknowledged
                && complete.next_publication.is_none_or(|next| now >= next)
                && events.try_send(ClientEvent::Image { room: room.id, asset: complete.asset, bytes: complete.bytes.clone() }).is_ok()
            {
                complete.next_publication = now.checked_add(Duration::from_secs(1));
            }
        }
        if !store.active() || !status.connected || status.pending > 0 {
            return Ok(());
        }
        // A failed image must not monopolize the worker between authoritative polls
        // or outbox retries. Revocation above still takes effect during this backoff.
        if self.retry.is_some_and(|(_, retry)| Instant::now() < retry) {
            return Ok(());
        }
        if self.partial.is_none()
            && let Some(asset) = allowed.iter().find(|asset| !self.complete.contains_key(&asset.hash))
        {
            let used = self.complete.values().fold(0usize, |sum, complete| sum.saturating_add(complete.bytes.len()));
            if used.saturating_add(asset.bytes as usize) > media::MAX_CACHE_BYTES {
                self.defer(*asset);
                return Err(Error::Invalid("Party image cache limit reached; reopen the room to retry".into()));
            }
            self.partial = Some((*asset, Vec::new()));
        }
        let Some(asset) = self.partial.as_ref().map(|(asset, _)| *asset) else { return Ok(()) };
        match self.fetch_chunk(store, transport, &status, room.id) {
            Ok(()) => {
                self.retry = None;
                self.failures = 0;
                Ok(())
            }
            Err(error) => {
                self.defer(asset);
                Err(error)
            }
        }
    }

    fn defer(&mut self, asset: AssetRef) {
        self.failures = self.failures.saturating_add(1).min(5);
        self.retry = Instant::now().checked_add(Duration::from_millis(250 * (1u64 << self.failures))).map(|at| (asset, at));
    }

    fn fetch_chunk(&mut self, store: &Outbox, transport: &Transport, status: &crate::ClientStatus, room: RoomId) -> Result<()> {
        let Some((asset, bytes)) = self.partial.as_mut() else { return Ok(()) };
        let request = RpcRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(&crate::store::random_hex(16)?).map_err(|e| Error::Invalid(e.to_string()))?,
            operation: Operation::Fetch { room, hash: asset.hash, offset: bytes.len() as u64 },
        };
        let endpoint = status.endpoint.as_deref().ok_or_else(|| Error::Invalid("No service address".into()))?;
        let response = transport.call(endpoint, &store.credential()?, &request)?;
        let RpcResponse::AssetChunk { bytes: chunk, total } = response else {
            return Err(Error::Invalid("Expected an authorized image chunk".into()));
        };
        if total != asset.bytes || chunk.is_empty() || chunk.len() > CHUNK_BYTES || bytes.len().saturating_add(chunk.len()) as u64 > total {
            self.partial = None;
            return Err(Error::Invalid("Invalid image length; reference was not displayed".into()));
        }
        bytes.try_reserve(chunk.len()).map_err(|_| Error::Invalid("Not enough memory to load this room image".into()))?;
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 == total {
            let hash = format!("{:x}", Sha256::digest(&*bytes));
            if hash != asset.hash.as_str() {
                self.partial = None;
                return Err(Error::Invalid("Image checksum failed; reference was not displayed".into()));
            }
            let asset = *asset;
            let complete = Arc::new(std::mem::take(bytes));
            self.complete.insert(asset.hash, Complete { asset, bytes: complete, acknowledged: false, next_publication: None });
            self.partial = None;
        }
        Ok(())
    }
}

pub(crate) fn notes(store: &Outbox, transport: &Transport) -> Result<ClientEvent> {
    let status = store.status();
    let room = status.room.as_ref().ok_or_else(|| Error::Invalid("No revealed game".into()))?;
    if !matches!(room.phase, photocraft_party::room::Phase::Reveal | photocraft_party::room::Phase::Archived) || room.reveal.is_empty() {
        return Err(Error::Invalid("Notes are available only after your game reveals".into()));
    }
    let game = room
        .reveal
        .first()
        .map(|entry| match entry {
            photocraft_party::room::RevealedEntry::Artwork { assignment, .. } | photocraft_party::room::RevealedEntry::Missing { assignment, .. } => {
                assignment.id.game
            }
        })
        .ok_or_else(|| Error::Invalid("No revealed game is available".into()))?;
    let request = RpcRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(&crate::store::random_hex(16)?).map_err(|e| Error::Invalid(e.to_string()))?,
        operation: Operation::Notes { room: room.id },
    };
    match transport.call(status.endpoint.as_deref().ok_or(Error::Unavailable)?, &store.credential()?, &request)? {
        RpcResponse::Notes { notes } if notes.len() <= 36 => {
            for (assignment, note) in &notes {
                photocraft_party::validate_label(note, photocraft_party::MAX_NOTE_BYTES, true).map_err(|e| Error::Invalid(e.to_string()))?;
                if !room.reveal.iter().any(|entry| match entry {
                    photocraft_party::room::RevealedEntry::Artwork { assignment: a, .. } => a.id == *assignment,
                    _ => false,
                }) {
                    return Err(Error::Invalid("Notes do not belong to this reveal".into()));
                }
            }
            Ok(ClientEvent::Notes { room: room.id, game, epoch: room.epoch, notes })
        }
        _ => Err(Error::Invalid("Invalid revealed notes response".into())),
    }
}

#[cfg(test)]
#[path = "download_tests.rs"]
mod tests;
