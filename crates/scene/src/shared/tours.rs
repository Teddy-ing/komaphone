//! Stable camera routes shared by preview and frozen-revision export.
use serde::{Deserialize, Serialize};

use super::{Error, Result, TourId, WorldState};
use crate::{Camera, CanvasId, StrokeId, coordinates};

pub const MAX_TOUR_STOPS: usize = 128;
pub const MAX_TOUR_DURATION_MS: u64 = 600_000;
pub const MAX_STOP_DURATION_MS: u32 = 120_000;
pub const MAX_EXPORT_FRAMES: u32 = 1800;
pub const MAX_EXPORT_SIDE: u32 = 2048;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TourStop {
    pub camera: Camera,
    /// Travel from the preceding stop. The first stop must have zero travel.
    pub travel_ms: u32,
    pub pause_ms: u32,
    /// Optional authored content anchor. Coordinates alone remain valid even
    /// where there is currently no drawing; content anchors can become missing.
    #[serde(default)]
    pub target: Option<StrokeId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tour {
    pub id: TourId,
    pub canvas: CanvasId,
    pub name: String,
    pub revision: u64,
    pub stops: Vec<TourStop>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TourFrame {
    pub camera: Camera,
    pub stop: usize,
    pub travelling: bool,
    pub finished: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportSettings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}
impl ExportSettings {
    pub fn frame_count(&self, tour: &Tour) -> Result<u32> {
        if self.width == 0 || self.height == 0 || self.width > MAX_EXPORT_SIDE || self.height > MAX_EXPORT_SIDE || !(1..=24).contains(&self.fps) {
            return Err(Error::Invalid("tour export dimensions or frame rate"));
        }
        let duration = tour.duration_ms()?;
        let frames = duration
            .checked_mul(u64::from(self.fps))
            .and_then(|n| n.checked_add(999))
            .map(|n| n / 1000)
            .filter(|n| *n > 0 && *n <= u64::from(MAX_EXPORT_FRAMES))
            .ok_or(Error::Limit("tour export frame count"))?;
        u32::try_from(frames).map_err(|_| Error::Limit("tour export frame count"))
    }
}

/// Pin both scene and route, including their hashes. A later edit cannot alter
/// this object. Export adapters render one frame at a time and must publish output
/// atomically after successful completion; cancellation never mutates the world.
#[derive(Debug, Clone)]
pub struct FrozenTour {
    world: WorldState,
    tour: Tour,
    scene_hash: String,
    settings: ExportSettings,
    frame_count: u32,
}
impl FrozenTour {
    pub fn world(&self) -> &WorldState {
        &self.world
    }
    pub fn tour(&self) -> &Tour {
        &self.tour
    }
    pub fn scene_hash(&self) -> &str {
        &self.scene_hash
    }
    pub fn settings(&self) -> &ExportSettings {
        &self.settings
    }
    pub fn frame_count(&self) -> u32 {
        self.frame_count
    }
    pub fn new(world: &WorldState, id: TourId, settings: ExportSettings) -> Result<Self> {
        let tour = world.tours.get(&id).ok_or(Error::Invalid("missing tour"))?.clone();
        if !tour.missing_stops(world)?.is_empty() {
            return Err(Error::Invalid("tour has missing destinations; repair or explicitly skip them"));
        }
        let frame_count = settings.frame_count(&tour)?;
        Ok(Self { world: world.clone(), tour, scene_hash: world.canonical_hash()?, settings, frame_count })
    }
    pub fn frame(&self, index: u32) -> Result<TourFrame> {
        if self.settings.frame_count(&self.tour)? != self.frame_count {
            return Err(Error::Invalid("modified tour export settings"));
        }
        if index >= self.frame_count {
            return Err(Error::Invalid("tour frame index"));
        }
        // Render the exact last endpoint, not an interpolation that stops just
        // before it. Duration rounding remains bounded to one output frame.
        let elapsed = if index + 1 == self.frame_count { self.tour.duration_ms()? } else { u64::from(index) * 1000 / u64::from(self.settings.fps) };
        self.tour.frame_at(elapsed)
    }
}

impl Tour {
    pub fn validate(&self) -> Result<()> {
        crate::validate_name(&self.name)?;
        super::valid_revision(self.revision)?;
        if self.stops.is_empty() || self.stops.len() > MAX_TOUR_STOPS {
            return Err(Error::Limit("tour stops"));
        }
        if self.stops.first().is_none_or(|stop| stop.travel_ms != 0) {
            return Err(Error::Invalid("first tour stop travel duration"));
        }
        let mut duration = 0_u64;
        for stop in &self.stops {
            coordinates::validate_camera(&stop.camera)?;
            if stop.camera.canvas != self.canvas {
                return Err(Error::Invalid("foreign-canvas tour destination"));
            }
            if stop.travel_ms > MAX_STOP_DURATION_MS || stop.pause_ms > MAX_STOP_DURATION_MS {
                return Err(Error::Limit("tour stop duration"));
            }
            duration = duration.checked_add(u64::from(stop.travel_ms) + u64::from(stop.pause_ms)).ok_or(Error::Limit("tour duration"))?;
        }
        if duration == 0 || duration > MAX_TOUR_DURATION_MS {
            return Err(Error::Limit("tour duration"));
        }
        Ok(())
    }
    pub fn duration_ms(&self) -> Result<u64> {
        self.validate()?;
        Ok(self.stops.iter().map(|stop| u64::from(stop.travel_ms) + u64::from(stop.pause_ms)).sum())
    }
    pub fn missing_stops(&self, world: &WorldState) -> Result<Vec<usize>> {
        self.validate()?;
        if self.canvas != world.canvas {
            return Err(Error::Invalid("tour/world mismatch"));
        }
        Ok(self
            .stops
            .iter()
            .enumerate()
            .filter_map(|(index, stop)| stop.target.filter(|id| world.strokes.get(id).is_none_or(|stroke| !stroke.visible)).map(|_| index))
            .collect())
    }
    /// Explicit, nonmutating repair proposal. The caller must save it through a
    /// revision-checked PutTour; preview never silently discards destinations.
    pub fn skipping_missing(&self, world: &WorldState) -> Result<Self> {
        let missing = self.missing_stops(world)?;
        let mut result = self.clone();
        result.stops = self.stops.iter().enumerate().filter(|(index, _)| !missing.contains(index)).map(|(_, stop)| stop.clone()).collect();
        if let Some(first) = result.stops.first_mut() {
            first.travel_ms = 0;
        }
        result.revision = 0;
        result.validate()?;
        Ok(result)
    }
    pub fn frame_at(&self, elapsed_ms: u64) -> Result<TourFrame> {
        let duration = self.duration_ms()?;
        if elapsed_ms >= duration {
            let index = self.stops.len().saturating_sub(1);
            let stop = self.stops.get(index).ok_or(Error::Invalid("empty tour"))?;
            return Ok(TourFrame { camera: stop.camera.clone(), stop: index, travelling: false, finished: true });
        }
        let mut elapsed = elapsed_ms;
        for (index, stop) in self.stops.iter().enumerate() {
            if elapsed < u64::from(stop.travel_ms) {
                let previous = index.checked_sub(1).and_then(|n| self.stops.get(n)).ok_or(Error::Invalid("tour predecessor"))?;
                let progress = elapsed as f64 / f64::from(stop.travel_ms);
                return Ok(TourFrame {
                    camera: coordinates::flight(&previous.camera, &stop.camera, progress)?,
                    stop: index,
                    travelling: true,
                    finished: false,
                });
            }
            elapsed = elapsed.saturating_sub(u64::from(stop.travel_ms));
            if elapsed < u64::from(stop.pause_ms) {
                return Ok(TourFrame { camera: stop.camera.clone(), stop: index, travelling: false, finished: false });
            }
            elapsed = elapsed.saturating_sub(u64::from(stop.pause_ms));
        }
        Err(Error::Invalid("tour timeline"))
    }
}

/// Local viewer state. Serialization supports close/reopen without syncing one
/// person's playhead to another person's view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TourPlayback {
    pub tour: TourId,
    pub tour_revision: u64,
    pub elapsed_ms: u64,
    pub playing: bool,
}
impl TourPlayback {
    pub fn new(tour: &Tour) -> Result<Self> {
        tour.validate()?;
        Ok(Self { tour: tour.id, tour_revision: tour.revision, elapsed_ms: 0, playing: true })
    }
    pub fn validate(&self, tour: &Tour) -> Result<()> {
        if self.tour != tour.id || self.tour_revision != tour.revision || self.elapsed_ms > tour.duration_ms()? {
            return Err(Error::Invalid("tour playback or changed route"));
        }
        Ok(())
    }
    pub fn advance(&mut self, tour: &Tour, delta_ms: u64) -> Result<TourFrame> {
        self.validate(tour)?;
        if self.playing {
            self.elapsed_ms = self.elapsed_ms.saturating_add(delta_ms).min(tour.duration_ms()?);
        }
        let frame = tour.frame_at(self.elapsed_ms)?;
        if frame.finished {
            self.playing = false;
        }
        Ok(frame)
    }
    pub fn scrub(&mut self, tour: &Tour, elapsed_ms: u64) -> Result<TourFrame> {
        self.validate(tour)?;
        if elapsed_ms > tour.duration_ms()? {
            return Err(Error::Invalid("tour scrub position"));
        }
        self.elapsed_ms = elapsed_ms;
        tour.frame_at(elapsed_ms)
    }
    pub fn cancel(&mut self) {
        self.playing = false;
        self.elapsed_ms = 0;
    }
}
