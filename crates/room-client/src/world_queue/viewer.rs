//! Personal navigation/review state; snapshot downloads never alter this table.
use super::{Activation, WorldOutbox, active, checksum, invalid, read_u64, sql};
use crate::Result;
use photocraft_scene::{Bookmark, Camera, coordinates, shared::ReviewState};
use photocraft_scene::{CanvasId, OwnerId};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
pub(super) const MAX_VIEWER_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewerState {
    pub camera: Camera,
    pub bookmarks: Vec<Bookmark>,
    pub review: Option<ReviewState>,
}
impl ViewerState {
    pub(super) fn validate(&self, canvas: CanvasId, viewer: OwnerId, available: u64) -> Result<()> {
        if self.camera.canvas != canvas || self.bookmarks.len() > photocraft_scene::MAX_BOOKMARKS {
            return Err(invalid("Canvas viewer identity or bookmark limit"));
        }
        coordinates::center(&self.camera).map_err(|e| invalid(&e.to_string()))?;
        for bookmark in &self.bookmarks {
            if bookmark.camera.canvas != canvas {
                return Err(invalid("Bookmark belongs to another canvas"));
            }
            photocraft_scene::validate_name(&bookmark.name).map_err(|e| invalid(&e.to_string()))?;
            coordinates::center(&bookmark.camera).map_err(|e| invalid(&e.to_string()))?;
        }
        if let Some(review) = &self.review {
            if review.canvas != canvas || review.viewer != viewer {
                return Err(invalid("Review belongs to another canvas or account"));
            }
            review.validate(available).map_err(|e| invalid(&e.to_string()))?;
        }
        Ok(())
    }
}
impl WorldOutbox {
    pub fn save_view(&mut self, lease: &Activation, canvas: CanvasId, viewer: &ViewerState) -> Result<()> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        active(&tx, lease)?;
        let revision: Option<u64> = tx
            .query_row("SELECT revision FROM canvas_heads WHERE scope=?1 AND canvas=?2", params![lease.key, canvas.as_str()], |r| read_u64(r, 0))
            .optional()
            .map_err(sql)?;
        viewer.validate(canvas, lease.scope.profile, revision.unwrap_or(0))?;
        let bytes = serde_json::to_vec(viewer).map_err(|_| invalid("Viewer state encoding"))?;
        if bytes.len() > MAX_VIEWER_BYTES {
            return Err(invalid("Viewer state exceeds its byte limit"));
        }
        tx.execute(
            "INSERT INTO viewers VALUES(?1,?2,?3,?4) ON CONFLICT(scope,canvas) DO UPDATE SET body=excluded.body,hash=excluded.hash",
            params![lease.key, canvas.as_str(), bytes, checksum(&lease.key, canvas, &bytes)],
        )
        .map_err(sql)?;
        tx.commit().map_err(sql)
    }
    pub fn load_view(&self, lease: &Activation, canvas: CanvasId) -> Result<Option<ViewerState>> {
        active(&self.db, lease)?;
        let row: Option<(Vec<u8>, String)> = self
            .db
            .query_row(
                "SELECT substr(body,1,?3),hash FROM viewers WHERE scope=?1 AND canvas=?2",
                params![lease.key, canvas.as_str(), (MAX_VIEWER_BYTES + 1) as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(sql)?;
        let Some((bytes, hash)) = row else { return Ok(None) };
        if bytes.len() > MAX_VIEWER_BYTES || checksum(&lease.key, canvas, &bytes) != hash {
            return Err(invalid("Viewer state checksum failed; preserve recovery data"));
        }
        let viewer: ViewerState = serde_json::from_slice(&bytes).map_err(|_| invalid("Saved viewer state is invalid"))?;
        let revision: Option<u64> = self
            .db
            .query_row("SELECT revision FROM canvas_heads WHERE scope=?1 AND canvas=?2", params![lease.key, canvas.as_str()], |r| read_u64(r, 0))
            .optional()
            .map_err(sql)?;
        viewer.validate(canvas, lease.scope.profile, revision.unwrap_or(0))?;
        Ok(Some(viewer))
    }
}
