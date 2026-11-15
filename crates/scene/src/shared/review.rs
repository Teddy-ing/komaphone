//! Per-viewer catch-up bookmarks. Receiving events never marks them reviewed.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use super::{DeviceId, Error, MAX_EVENT_PAGE, Result, WorldState};
use crate::{CanvasId, OwnerId, RegionKey};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub revision: u64,
    pub author: OwnerId,
    pub device: DeviceId,
    pub region: Option<RegionKey>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interval {
    pub start_exclusive: u64,
    pub end_inclusive: u64,
    pub position: u64,
    pub playing: bool,
    pub speed: f64,
    pub fractional_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewState {
    pub canvas: CanvasId,
    pub viewer: OwnerId,
    /// The first-visit current-state baseline is retained for an honest UI label.
    pub baseline_revision: u64,
    pub reviewed_revision: u64,
    pub interval: Option<Interval>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangePage {
    pub changes: Vec<Change>,
    /// Last scanned revision, including events hidden by the author filter.
    pub scanned_through: u64,
    pub end_inclusive: u64,
}

impl ReviewState {
    pub fn first_visit(canvas: CanvasId, viewer: OwnerId, current_revision: u64) -> Result<Self> {
        super::valid_revision(current_revision)?;
        Ok(Self { canvas, viewer, baseline_revision: current_revision, reviewed_revision: current_revision, interval: None })
    }
    pub fn validate(&self, available_revision: u64) -> Result<()> {
        super::valid_revision(available_revision)?;
        if self.baseline_revision > self.reviewed_revision || self.reviewed_revision > available_revision {
            return Err(Error::Invalid("review watermark"));
        }
        if let Some(interval) = &self.interval
            && (interval.start_exclusive != self.reviewed_revision
                || interval.end_inclusive > available_revision
                || interval.start_exclusive > interval.position
                || interval.position > interval.end_inclusive
                || !interval.speed.is_finite()
                || !(0.25..=8.0).contains(&interval.speed)
                || !interval.fractional_ms.is_finite()
                || !(0.0..250.0).contains(&interval.fractional_ms))
        {
            return Err(Error::Invalid("review interval"));
        }
        Ok(())
    }
    pub fn unseen_revisions(&self, available_revision: u64) -> Result<u64> {
        self.validate(available_revision)?;
        Ok(available_revision - self.reviewed_revision)
    }
    pub fn begin(&mut self, available_revision: u64) -> Result<()> {
        self.validate(available_revision)?;
        if self.interval.is_some() {
            return Err(Error::Invalid("review already active; resume or cancel it"));
        }
        self.interval = Some(Interval {
            start_exclusive: self.reviewed_revision,
            end_inclusive: available_revision,
            position: self.reviewed_revision,
            playing: false,
            speed: 1.0,
            fractional_ms: 0.0,
        });
        Ok(())
    }
    pub fn scrub(&mut self, position: u64) -> Result<()> {
        let interval = self.interval.as_mut().ok_or(Error::Invalid("no active review"))?;
        if position < interval.start_exclusive || position > interval.end_inclusive {
            return Err(Error::Invalid("review scrub position"));
        }
        interval.position = position;
        interval.fractional_ms = 0.0;
        Ok(())
    }
    pub fn advance(&mut self, delta_ms: u64, available_revision: u64) -> Result<()> {
        self.validate(available_revision)?;
        let interval = self.interval.as_mut().ok_or(Error::Invalid("no active review"))?;
        if interval.playing {
            // Clamp a stalled frame; elapsed wall time does not allocate work or
            // jump past the fixed end. The renderer reconstructs the final cursor.
            let elapsed = (delta_ms.min(60_000) as f64) * interval.speed + interval.fractional_ms;
            let steps = (elapsed / 250.0).floor() as u64;
            interval.fractional_ms = elapsed % 250.0;
            interval.position = interval.position.saturating_add(steps).min(interval.end_inclusive);
            if interval.position == interval.end_inclusive {
                interval.playing = false;
                interval.fractional_ms = 0.0;
            }
        }
        Ok(())
    }
    /// Call only for the explicit Reviewed/Mark seen action. Events accepted
    /// after begin() remain unseen even if downloaded while playback was active.
    pub fn mark_seen(&mut self, available_revision: u64) -> Result<()> {
        self.validate(available_revision)?;
        let interval = self.interval.take().ok_or(Error::Invalid("no active review"))?;
        self.reviewed_revision = interval.end_inclusive;
        Ok(())
    }
    pub fn cancel(&mut self) {
        self.interval = None;
    }
    pub fn changes(&self, world: &WorldState, after: u64, limit: usize, authors: Option<&BTreeSet<OwnerId>>, include_own: bool) -> Result<ChangePage> {
        self.validate(world.revision)?;
        if self.canvas != world.canvas {
            return Err(Error::Invalid("review/world mismatch"));
        }
        let interval = self.interval.as_ref().ok_or(Error::Invalid("no active review"))?;
        if after < interval.start_exclusive || after > interval.end_inclusive || limit == 0 || limit > MAX_EVENT_PAGE {
            return Err(Error::Invalid("review page"));
        }
        let remaining = usize::try_from(interval.end_inclusive - after).unwrap_or(usize::MAX).min(limit);
        if remaining == 0 {
            return Ok(ChangePage { changes: Vec::new(), scanned_through: after, end_inclusive: interval.end_inclusive });
        }
        let events = world.events_after(after, remaining)?;
        let changes = events
            .iter()
            .filter(|event| (include_own || event.op.author != self.viewer) && authors.is_none_or(|filter| filter.contains(&event.op.author)))
            .map(|event| world.change(event))
            .collect::<Result<Vec<_>>>()?;
        Ok(ChangePage { changes, scanned_through: events.last().map_or(after, |event| event.revision), end_inclusive: interval.end_inclusive })
    }
    /// Deliberately has no author-filter parameter: filtered highlights must not
    /// remove causal operations from the canonical replay scene.
    pub fn scene_at_position(&self, world: &WorldState) -> Result<WorldState> {
        self.validate(world.revision)?;
        if self.canvas != world.canvas {
            return Err(Error::Invalid("review/world mismatch"));
        }
        let interval = self.interval.as_ref().ok_or(Error::Invalid("no active review"))?;
        world.replay_to(interval.position)
    }
}
