//! Read-only party derivatives, separate from editable documents and replay history.
use crate::{EngineError, Result};
use photocraft_party::{AssetHash, AssetRef, AssignmentId, GameId, RoomId};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[cfg(test)]
#[path = "media_tests.rs"]
mod tests;

#[derive(Default)]
pub struct RoomMedia {
    room: Option<RoomId>,
    pub images: BTreeMap<AssetHash, Arc<RoomImage>>,
    pngs: BTreeMap<AssetHash, Arc<Vec<u8>>>,
    pub notes: Vec<(AssignmentId, String)>,
    notes_epoch: Option<u64>,
    notes_game: Option<GameId>,
    pub error: Option<String>,
}
impl RoomMedia {
    pub fn notes_loaded(&self, room: RoomId, epoch: u64) -> bool {
        self.room == Some(room) && self.notes_epoch == Some(epoch)
    }
    pub fn reconcile(&mut self, status: &photocraft_room_client::ClientStatus) {
        let Some((room, actor)) = status.room.as_ref().zip(status.profile.as_ref()) else {
            *self = Self::default();
            return;
        };
        if self.room != Some(room.id) {
            *self = Self { room: Some(room.id), ..Self::default() };
        }
        let allowed = photocraft_room_client::media::images(room, actor.id);
        if allowed.is_empty() {
            self.error = None;
        }
        self.images.retain(|hash, _| allowed.iter().any(|asset| asset.hash == *hash));
        self.pngs.retain(|hash, _| self.images.contains_key(hash));
        if self.notes_epoch != Some(room.epoch)
            || self.notes_game
                != room.reveal.first().map(|entry| match entry {
                    photocraft_party::room::RevealedEntry::Artwork { assignment, .. } | photocraft_party::room::RevealedEntry::Missing { assignment, .. } => {
                        assignment.id.game
                    }
                })
            || room.reveal.is_empty()
            || !room.members.iter().any(|member| member.participant.id == actor.id)
            || !matches!(room.phase, photocraft_party::room::Phase::Reveal | photocraft_party::room::Phase::Archived)
        {
            self.notes.clear();
            self.notes_epoch = None;
            self.notes_game = None;
        }
    }
    pub fn png(&self, hash: AssetHash) -> Option<Arc<Vec<u8>>> {
        self.pngs.get(&hash).cloned()
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn accept(&mut self, status: &photocraft_room_client::ClientStatus, room: RoomId, asset: AssetRef, bytes: Arc<Vec<u8>>) -> Result<()> {
        use image::ImageDecoder;
        self.reconcile(status);
        let (view, actor) = status.room.as_ref().zip(status.profile.as_ref()).ok_or_else(|| EngineError::Other("No authorized image context".into()))?;
        if room != view.id || !photocraft_room_client::media::images(view, actor.id).contains(&asset) {
            return Ok(()); // A completed download may race a new assignment.
        }
        if self.pngs.get(&asset.hash).is_some_and(|png| png.len() as u64 == asset.bytes) && self.images.contains_key(&asset.hash) {
            return Ok(());
        }
        if bytes.len() as u64 != asset.bytes || asset.bytes > photocraft_room_client::media::MAX_IMAGE_BYTES {
            return Err(EngineError::Other("Invalid party image size".into()));
        }
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(1536);
        limits.max_image_height = Some(1024);
        limits.max_alloc = Some(16 * 1024 * 1024);
        let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes.as_slice()), image::ImageFormat::Png);
        reader.limits(limits);
        let decoder = reader.into_decoder().map_err(|e| EngineError::Other(e.to_string()))?;
        let (width, height) = decoder.dimensions();
        let expected = match view.mode {
            photocraft_party::Mode::Animation { .. } => (1536, 1024),
            photocraft_party::Mode::ExquisiteCorpse => {
                if matches!(view.phase, photocraft_party::room::Phase::Drawing | photocraft_party::room::Phase::Transfer) { (1024, 24) } else { (1024, 512) }
            }
        };
        if (width, height) != expected || decoder.total_bytes() > 8 * 1024 * 1024 {
            return Err(EngineError::Other("Party image dimensions do not match the authorized assignment".into()));
        }
        let rgba = image::DynamicImage::from_decoder(decoder).map_err(|e| EngineError::Other(e.to_string()))?.to_rgba8().into_raw();
        let used: usize = self.images.iter().filter(|(hash, _)| **hash != asset.hash).map(|(_, image)| image.rgba.len()).sum();
        if used.saturating_add(rgba.len()) > photocraft_room_client::media::MAX_CACHE_BYTES {
            return Err(EngineError::Other("Party image cache is full".into()));
        }
        self.images.insert(asset.hash, Arc::new(RoomImage { width, height, rgba }));
        self.pngs.insert(asset.hash, bytes);
        self.error = None;
        Ok(())
    }
    pub(crate) fn accept_notes(
        &mut self,
        status: &photocraft_room_client::ClientStatus,
        room: RoomId,
        game: GameId,
        epoch: u64,
        notes: Vec<(AssignmentId, String)>,
    ) {
        self.reconcile(status);
        if status.room.as_ref().is_some_and(|view| {
            view.id == room
                && view.epoch == epoch
                && !view.reveal.is_empty()
                && view.members.iter().any(|member| Some(member.participant.id) == status.profile.as_ref().map(|profile| profile.id))
                && view.reveal.iter().all(|entry| match entry {
                    photocraft_party::room::RevealedEntry::Artwork { assignment, .. } | photocraft_party::room::RevealedEntry::Missing { assignment, .. } => {
                        assignment.id.game == game
                    }
                })
                && notes.len() <= 36
                && notes.iter().map(|(assignment, _)| assignment).collect::<std::collections::BTreeSet<_>>().len() == notes.len()
                && notes.iter().all(|(id, note)| {
                    photocraft_party::validate_label(note, photocraft_party::MAX_NOTE_BYTES, true).is_ok()
                        && view.reveal.iter().any(|entry| matches!(entry,photocraft_party::room::RevealedEntry::Artwork{assignment,..} if assignment.id==*id))
                })
                && matches!(view.phase, photocraft_party::room::Phase::Reveal | photocraft_party::room::Phase::Archived)
        }) {
            self.notes = notes;
            self.notes_epoch = Some(epoch);
            self.notes_game = Some(game);
        }
    }
}
