//! One exact completed GPU output. Polls and cursor labels do not regenerate
//! unchanged retained geometry; camera movement and transient drawing still do.
#[cfg(test)]
use super::{Key, Viewport};
#[cfg(test)]
use crate::komaphone_world_gpu::Stats;
#[cfg(test)]
use photocraft_engine::scene::shared::media::MediaFrame;

#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(super) struct OutputKey {
    ready: Key,
    view: Viewport,
    density: u32,
    frames: Vec<MediaFrame>,
}
#[cfg(test)]
impl OutputKey {
    pub fn new(ready: Key, view: Viewport, density: f32, frames: Vec<MediaFrame>) -> Self {
        Self { ready, view, density: density.to_bits(), frames }
    }
}
#[cfg(test)]
struct Completed {
    key: OutputKey,
    texture: egui::TextureId,
    stats: Stats,
}
#[derive(Default)]
pub(super) struct OutputCache {
    // Keep the camera/account key off the app's stack. Only one texture is held
    // by WorldGpu; this cache owns neither another image nor any source samples.
    #[cfg(test)]
    completed: Option<Box<Completed>>,
}
impl OutputCache {
    pub fn clear(&mut self) {
        #[cfg(test)]
        {
            self.completed = None;
        }
    }
    #[cfg(test)]
    pub fn render(
        &mut self,
        key: OutputKey,
        reusable: bool,
        render: impl FnOnce() -> Result<(egui::TextureId, Stats), String>,
    ) -> Result<(egui::TextureId, Stats), String> {
        if reusable
            && let Some(completed) = &self.completed
            && completed.key == key
        {
            return Ok((completed.texture, completed.stats.clone()));
        }
        // The single GPU target may be overwritten by any draw, including a
        // preview or a failed render. The previous key is no longer valid.
        self.clear();
        let (texture, stats) = render()?;
        if reusable && stats.pending_assets == 0 {
            self.completed = Some(Box::new(Completed { key, texture, stats: stats.clone() }));
        }
        Ok((texture, stats))
    }
}

#[cfg(test)]
mod tests;
