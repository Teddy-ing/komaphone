//! Read-only authenticated thumbnail source. Never exposes the bearer credential.
use super::{WorldAccess, other};
use crate::{Error, Result, Transport, world_replica::WorldReplica};
use photocraft_scene::{
    CanvasId,
    shared::{WorldState, media::MediaFrame},
    world_protocol::{self as wire, Action, CanvasSummary, Response},
};
use sha2::{Digest, Sha256};

pub struct ThumbnailReader {
    access: WorldAccess,
    transport: Transport,
}
impl WorldAccess {
    pub fn thumbnail_reader(&self, trusted_certificate: Option<&[u8]>) -> Result<ThumbnailReader> {
        Ok(ThumbnailReader { access: self.copy_for_worker(), transport: self.transport(trusted_certificate)? })
    }
}
impl ThumbnailReader {
    fn call(&self, action: Action, current: &mut dyn FnMut() -> bool) -> Result<Response> {
        if !current() {
            return Err(other("Canvas thumbnail cancelled"));
        }
        let response = self.transport.call_world(&self.access.scope.endpoint, &self.access.credential, &wire::Request { version: wire::VERSION, action })?;
        if !current() {
            return Err(other("Canvas thumbnail scope changed"));
        }
        Ok(response)
    }
    /// The catalog's exact accepted revision is pinned. A live ownership/grant
    /// check precedes every uncached reconstruction and every media chunk.
    pub fn scene(&self, expected: &CanvasSummary, current: &mut dyn FnMut() -> bool) -> Result<WorldState> {
        let Response::Canvas { info } = self.call(Action::Info { canvas: expected.canvas }, current)? else {
            return Err(other("Unexpected thumbnail authorization response"));
        };
        if info.canvas != expected.canvas
            || info.owner != expected.owner
            || info.initial_name != expected.initial_name
            || info.epoch != expected.epoch
            || info.revision < expected.revision
        {
            return Err(other("Thumbnail catalog grant changed; refresh the canvas list"));
        }
        let mut replica = WorldReplica::open(expected.clone(), None)?;
        let mut bytes = 0usize;
        if expected.revision >= super::CHECKPOINT_CATCHUP_GAP
            && let Response::CheckpointMeta { checkpoint: Some(ckpt) } = self.call(Action::CheckpointMeta { canvas: expected.canvas }, current)?
            && ckpt.validate().is_ok()
            && ckpt.canvas == expected.canvas
            && ckpt.revision <= expected.revision
            && ckpt.compressed_bytes <= 32 * 1024 * 1024
        {
            let mut compressed = Vec::with_capacity(ckpt.compressed_bytes as usize);
            while (compressed.len() as u64) < ckpt.compressed_bytes {
                let offset = compressed.len() as u64;
                let Response::CheckpointChunk { revision, compressed_sha256, offset: got_off, bytes: chunk, total } = self.call(
                    Action::CheckpointChunk {
                        canvas: expected.canvas,
                        revision: ckpt.revision,
                        compressed_sha256: ckpt.compressed_sha256,
                        offset,
                        len: wire::CHECKPOINT_CHUNK_BYTES,
                    },
                    current,
                )?
                else {
                    return Err(other("Unexpected thumbnail checkpoint chunk response"));
                };
                if revision != ckpt.revision
                    || compressed_sha256 != ckpt.compressed_sha256
                    || got_off != offset
                    || total != ckpt.compressed_bytes
                    || chunk.is_empty()
                    || (compressed.len() as u64).saturating_add(chunk.len() as u64) > ckpt.compressed_bytes
                {
                    return Err(other("Thumbnail checkpoint chunk mismatch"));
                }
                bytes = bytes.saturating_add(chunk.len());
                if bytes > 32 * 1024 * 1024 {
                    return Err(other("Thumbnail history exceeds its 32 MiB preview budget; open the canvas"));
                }
                compressed.extend_from_slice(&chunk);
            }
            let decoded = photocraft_scene::shared::WorldCheckpoint::decode_zlib_bounded(
                &compressed,
                ckpt.compressed_sha256.as_str(),
                ckpt.uncompressed_bytes,
                ckpt.state_hash.as_str(),
                wire::MAX_CHECKPOINT_UNCOMPRESSED_BYTES,
            )
            .map_err(other)?;
            let state = WorldState::from_checkpoint(decoded).map_err(other)?;
            replica.install_checkpoint(state)?;
        }
        while !replica.complete() {
            let response = self.call(
                Action::Events { canvas: expected.canvas, after: replica.accepted().revision, through: Some(expected.revision), limit: wire::MAX_PAGE },
                current,
            )?;
            bytes = bytes.saturating_add(serde_json::to_vec(&response).map_err(other)?.len());
            if bytes > 32 * 1024 * 1024 {
                return Err(other("Thumbnail history exceeds its 32 MiB preview budget; open the canvas"));
            }
            replica.apply_page(&response)?;
        }
        Ok(replica.accepted().clone())
    }
    pub fn frame(&self, canvas: CanvasId, frame: &MediaFrame, current: &mut dyn FnMut() -> bool) -> Result<Vec<u8>> {
        frame.validate().map_err(other)?;
        let expected = usize::try_from(frame.byte_len).map_err(|_| other("Thumbnail media size"))?;
        let mut data = Vec::new();
        data.try_reserve_exact(expected).map_err(|_| other("Thumbnail media allocation"))?;
        while data.len() < expected {
            let Response::MediaChunk { bytes, total } = self.call(Action::FetchMedia { canvas, hash: frame.hash, offset: data.len() as u64 }, current)? else {
                return Err(other("Unexpected thumbnail media response"));
            };
            if total != frame.byte_len
                || bytes.is_empty()
                || bytes.len() > photocraft_party::protocol::CHUNK_BYTES
                || data.len().saturating_add(bytes.len()) > expected
            {
                return Err(other("Thumbnail media chunk differs from the accepted descriptor"));
            }
            data.extend_from_slice(&bytes);
        }
        if format!("{:x}", Sha256::digest(&data)) != frame.hash.as_str() {
            return Err(Error::Invalid("Thumbnail media checksum mismatch".into()));
        }
        Ok(data)
    }
}

#[cfg(test)]
mod tests;
