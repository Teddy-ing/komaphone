//! Verified canonical canvas replay and separate optimistic local edits.
//! No network acknowledgment is inferred from a matching image or local save.
use std::sync::Arc;

use photocraft_scene::{
    CanvasId, OwnerId,
    shared::{AcceptedOp, WorldOp, WorldState},
    world_protocol::{CanvasSummary, Response},
};

use crate::{Error, Result};

#[derive(Clone, Debug)]
pub struct WorldReplica {
    info: CanvasSummary,
    accepted: Arc<WorldState>,
}
fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
fn scene(error: impl std::fmt::Display) -> Error {
    Error::Invalid(error.to_string())
}
fn validate_info(info: &CanvasSummary) -> Result<()> {
    photocraft_scene::validate_name(&info.name).map_err(scene)?;
    photocraft_scene::validate_name(&info.initial_name).map_err(scene)?;
    if let Some(origin) = &info.import_origin {
        origin.validate(info.canvas, info.owner).map_err(scene)?;
        if info.revision < origin.source_revision {
            return Err(invalid("Canvas summary omits the committed archive prefix"));
        }
    }
    if info.epoch == 0 || info.epoch > i64::MAX as u64 || info.revision > i64::MAX as u64 {
        return Err(invalid("Invalid canvas grant or revision"));
    }
    Ok(())
}

impl WorldReplica {
    pub fn open(info: CanvasSummary, cached: Option<WorldState>) -> Result<Self> {
        validate_info(&info)?;
        let accepted = match cached {
            Some(cached) => {
                cached.validate().map_err(scene)?;
                if cached.canvas != info.canvas
                    || cached.owner != info.owner
                    || cached.initial_name() != info.initial_name
                    || cached.import_origin != info.import_origin
                {
                    return Err(invalid("Cached canvas identity does not match its authorized service grant"));
                }
                if cached.revision > info.revision {
                    return Err(invalid("The service is older than the saved canvas; preserve local work and reconcile the service restore"));
                }
                cached
            }
            None => WorldState::with_import_origin(info.canvas, info.owner, info.initial_name.clone(), info.import_origin.clone()).map_err(scene)?,
        };
        let replica = Self { info, accepted: Arc::new(accepted) };
        if replica.accepted.import_origin.as_ref().is_some_and(|origin| origin.source_revision == 0) {
            replica.accepted.validate().map_err(scene)?;
        }
        replica.validate_current()?;
        Ok(replica)
    }
    pub fn info(&self) -> &CanvasSummary {
        &self.info
    }
    pub fn accepted(&self) -> &WorldState {
        &self.accepted
    }
    pub(crate) fn accepted_arc(&self) -> Arc<WorldState> {
        Arc::clone(&self.accepted)
    }
    pub fn complete(&self) -> bool {
        self.accepted.revision == self.info.revision
    }
    fn validate_current(&self) -> Result<()> {
        if self.complete() && self.accepted.name != self.info.name {
            return Err(invalid("Canvas title does not match its canonical event history"));
        }
        Ok(())
    }
    /// Refreshing a grant never rewrites local operations or review watermarks.
    pub fn update_info(&mut self, info: CanvasSummary) -> Result<()> {
        validate_info(&info)?;
        if info.canvas != self.info.canvas
            || info.owner != self.info.owner
            || info.initial_name != self.info.initial_name
            || info.import_origin != self.info.import_origin
            || info.epoch < self.info.epoch
            || info.revision < self.info.revision
        {
            return Err(invalid("Stale or foreign canvas grant; preserve the current snapshot"));
        }
        let prior = std::mem::replace(&mut self.info, info);
        if let Err(error) = self.validate_current() {
            self.info = prior;
            return Err(error);
        }
        Ok(())
    }
    /// Atomically install a verified pinned checkpoint state whose revision lies within
    /// `[self.accepted.revision, self.info.revision]`.
    pub fn install_checkpoint(&mut self, state: WorldState) -> Result<()> {
        state.validate().map_err(scene)?;
        self.install_prevalidated_checkpoint(state)
    }

    /// Install a `WorldState` already validated off-thread by `verify_and_decode_checkpoint`.
    pub(crate) fn install_prevalidated_checkpoint(&mut self, state: WorldState) -> Result<()> {
        if state.canvas != self.info.canvas
            || state.owner != self.info.owner
            || state.initial_name() != self.info.initial_name
            || state.import_origin != self.info.import_origin
        {
            return Err(invalid("Checkpoint identity disagrees with authorized canvas summary"));
        }
        if state.revision < self.accepted.revision || state.revision > self.info.revision {
            return Err(invalid("Checkpoint revision is out of bounds for this replica"));
        }
        if state.revision == self.info.revision && state.name != self.info.name {
            return Err(invalid("Checkpoint canvas title disagrees with authorized summary"));
        }
        self.accepted = Arc::new(state);
        Ok(())
    }
    /// Accept a contiguous bounded page atomically. Reordered/malformed payloads
    /// cannot replace a newer local scene with an old whole-world upload.
    pub fn apply_page(&mut self, response: &Response) -> Result<()> {
        let Response::Events { events, through, next } = response else {
            return Err(invalid("Expected a canvas event page"));
        };
        if *through > self.info.revision || *through < self.accepted.revision || events.len() > photocraft_scene::world_protocol::MAX_PAGE as usize {
            return Err(invalid("Canvas event page interval"));
        }
        let last = events.last().map_or(self.accepted.revision, |e| e.revision);
        if last > *through || *next != (last < *through).then_some(last) || (events.is_empty() && last < *through) {
            return Err(invalid("Canvas event page is incomplete or cannot make progress"));
        }
        let mut candidate = (*self.accepted).clone();
        for event in events {
            if event.revision != candidate.revision.saturating_add(1) {
                return Err(invalid("Missing or reordered canvas events; request the last acknowledged revision"));
            }
            candidate.apply(event).map_err(scene)?;
        }
        if candidate.revision == self.info.revision && candidate.name != self.info.name {
            return Err(invalid("Canvas title differs from the accepted history"));
        }
        if candidate
            .import_origin
            .as_ref()
            .is_some_and(|origin| self.accepted.revision < origin.source_revision && candidate.revision >= origin.source_revision)
        {
            candidate.validate().map_err(scene)?;
        }
        self.accepted = Arc::new(candidate);
        Ok(())
    }
    /// A returned upload receipt is validated against the exact queued operation
    /// by WorldOutbox. Missing intervening events must still be downloaded.
    pub fn append_contiguous(&mut self, event: &AcceptedOp) -> Result<bool> {
        if event.op.canvas != self.info.canvas {
            return Err(invalid("Acknowledgment belongs to another canvas"));
        }
        if let Some(existing) = self.accepted.receipt(event.op.id) {
            return if existing == event { Ok(false) } else { Err(invalid("Conflicting canvas receipt")) };
        }
        if event.revision != self.accepted.revision.saturating_add(1) {
            return Ok(false);
        }
        if self
            .accepted
            .import_origin
            .as_ref()
            .is_some_and(|origin| self.accepted.revision < origin.source_revision && event.revision >= origin.source_revision)
        {
            let mut candidate = (*self.accepted).clone();
            candidate.apply(event).map_err(scene)?;
            candidate.validate().map_err(scene)?;
            self.accepted = Arc::new(candidate);
        } else {
            Arc::make_mut(&mut self.accepted).apply(event).map_err(scene)?;
        }
        if event.revision >= self.info.revision {
            self.info.revision = event.revision;
            self.info.name = self.accepted.name.clone();
        }
        Ok(true)
    }
    /// Optimistic presentation is separate from canonical accepted state and is
    /// rebuilt after network events. Only this account's retained queue is used.
    pub fn with_pending(&self, profile: OwnerId, pending: &[WorldOp]) -> Result<WorldState> {
        if pending.len() > 8192 {
            return Err(invalid("Canvas local preview operation limit"));
        }
        let mut view = (*self.accepted).clone();
        for op in pending {
            if op.canvas != self.info.canvas || op.author != profile || op.authority_epoch != self.info.epoch {
                return Err(invalid("Pending artwork belongs to another canvas, account or revoked grant"));
            }
            if let Some(existing) = view.receipt(op.id) {
                if existing.op != *op {
                    return Err(invalid("Pending artwork conflicts with an immutable receipt"));
                }
                continue;
            }
            if view.base_revision() > 0 && view.device_author(op.device) == Some(op.author) && op.sequence < view.next_sequence(op.device).map_err(scene)? {
                continue;
            }
            view.apply(&AcceptedOp { revision: view.revision.checked_add(1).ok_or_else(|| invalid("Canvas revision exhausted"))?, op: op.clone() })
                .map_err(scene)?;
        }
        Ok(view)
    }
    pub fn canvas(&self) -> CanvasId {
        self.info.canvas
    }
}
