//! One bounded native CPU worker per window. Replacing the latest request cannot
//! spawn extra workers or publish a stale account/canvas/review projection.
//!
//! Uses incremental `WorldRenderer::apply_accepted` and `step_review_to` within
//! the same scope so edits and catch-up playback update in O(1) and return exact
//! `dirty_rects` (preserving unaffected tiles across the pyramid).
use photocraft_engine::{
    scene::{CanvasId, shared::WorldState, spatial::WorldRect},
    world_media::WorldRenderer,
    worlds::Account,
};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Scope {
    pub activation: u64,
    pub account: Account,
    pub canvas: CanvasId,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Revision {
    Live { canonical: u64, display: u64, pending: usize },
    Review { end: u64, position: u64 },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Key {
    pub scope: Scope,
    pub revision: Revision,
}
impl Key {
    /// A clearly labelled older live projection may make progress while newer
    /// edits arrive. Historical review frames and other accounts never qualify.
    pub fn may_show_before(&self, wanted: &Self) -> bool {
        self.scope == wanted.scope
            && matches!((&self.revision, &wanted.revision),
            (Revision::Live { canonical:a, display:b, .. }, Revision::Live {canonical:c, display:d, ..}) if a <= c && b <= d)
    }
}
struct Request {
    key: Key,
    source: Arc<WorldState>,
}
pub(super) struct Completed {
    pub key: Key,
    pub result: Result<WorldRenderer, String>,
    pub dirty_rects: Option<Vec<WorldRect>>,
}
#[derive(Default)]
struct Mailbox {
    serial: u64,
    request: Option<Request>,
    result: Option<Completed>,
    closed: bool,
}
#[derive(Default)]
struct Shared {
    mailbox: Mutex<Mailbox>,
    wake: Condvar,
}
fn lock(shared: &Shared) -> MutexGuard<'_, Mailbox> {
    shared.mailbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
pub(super) struct Worker {
    shared: Arc<Shared>,
}
impl Worker {
    pub fn start() -> Result<Self, String> {
        Self::start_with(|_| {})
    }
    fn start_with(before: impl FnMut(&Key) + Send + 'static) -> Result<Self, String> {
        let shared = Arc::new(Shared::default());
        let thread = Arc::clone(&shared);
        std::thread::Builder::new().name("komaphone-world-view".into()).spawn(move || run(thread, before)).map_err(|e| e.to_string())?;
        Ok(Self { shared })
    }
    pub fn request(&self, key: Key, source: Arc<WorldState>) -> Result<(), String> {
        let mut mailbox = lock(&self.shared);
        mailbox.serial = mailbox.serial.checked_add(1).ok_or_else(|| "Shared view request limit reached".to_owned())?;
        mailbox.request = Some(Request { key, source });
        self.shared.wake.notify_one();
        Ok(())
    }
    pub fn invalidate(&self) {
        let mut mailbox = lock(&self.shared);
        mailbox.serial = mailbox.serial.saturating_add(1);
        mailbox.request = None;
    }
    pub fn take(&self) -> Option<Completed> {
        lock(&self.shared).result.take()
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        let mut mailbox = lock(&self.shared);
        mailbox.closed = true;
        mailbox.request = None;
        self.shared.wake.notify_one();
    }
}
fn run(shared: Arc<Shared>, mut before: impl FnMut(&Key)) {
    let mut cached: Option<(Key, WorldRenderer)> = None;
    loop {
        let (serial, request) = {
            let mut mailbox = lock(&shared);
            while mailbox.request.is_none() && !mailbox.closed {
                mailbox = shared.wake.wait(mailbox).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if mailbox.closed {
                return;
            }
            let Some(request) = mailbox.request.take() else { continue };
            (mailbox.serial, request)
        };
        before(&request.key);
        let (result, dirty_rects) = prepare_incremental(&request, &mut cached);
        let mut mailbox = lock(&shared);
        if mailbox.closed {
            return;
        }
        if mailbox.serial == serial || (result.is_ok() && mailbox.request.as_ref().is_some_and(|latest| request.key.may_show_before(&latest.key))) {
            let previous = mailbox.result.replace(Completed { key: request.key, result, dirty_rects });
            drop(mailbox);
            drop(previous);
        }
    }
}

fn prepare_incremental(request: &Request, cached: &mut Option<(Key, WorldRenderer)>) -> (Result<WorldRenderer, String>, Option<Vec<WorldRect>>) {
    if request.source.canvas != request.key.scope.canvas {
        *cached = None;
        return (Err("Shared view source belongs to another canvas".into()), None);
    }
    // Check if we can incrementally advance/step an existing cached renderer in the exact same scope.
    if let Some((prev_key, prev_renderer)) = cached.as_mut()
        && prev_key.scope == request.key.scope
    {
        match (&prev_key.revision, &request.key.revision) {
            (Revision::Live { display: old_disp, pending: old_pending, .. }, Revision::Live { display: new_disp, pending: new_pending, .. })
                if request.source.revision == *new_disp
                    && *new_disp > *old_disp
                    && *new_disp - *old_disp <= 64
                    && (*old_pending == 0 || *new_pending >= *old_pending)
                    && same_history_tip(prev_renderer.world(), &request.source, *old_disp) =>
            {
                let delta = (*new_disp - *old_disp) as usize;
                if let Ok(events) = request.source.events_after(*old_disp, delta) {
                    let mut dirty = Vec::new();
                    let mut ok = true;
                    for ev in events {
                        match prev_renderer.apply_accepted(ev) {
                            Ok(Some(r)) => dirty.push(r),
                            Ok(None) => {}
                            Err(_) => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok && prev_renderer.world().revision == *new_disp {
                        *prev_key = request.key.clone();
                        return (Ok(prev_renderer.clone()), Some(dirty));
                    }
                }
            }
            (Revision::Review { end: old_end, .. }, Revision::Review { end: new_end, position: new_pos })
                if old_end == new_end && *new_pos <= *new_end && *new_end <= request.source.revision =>
            {
                if let Ok(dirty) = prev_renderer.step_review_to(&request.source, *new_pos) {
                    *prev_key = request.key.clone();
                    return (Ok(prev_renderer.clone()), Some(dirty));
                }
            }
            _ => {}
        }
    }
    let result = prepare(request);
    match result {
        Ok(renderer) => {
            *cached = Some((request.key.clone(), renderer.clone()));
            (Ok(renderer), None)
        }
        Err(e) => {
            *cached = None;
            (Err(e), None)
        }
    }
}

fn same_history_tip(old: &WorldState, new: &WorldState, rev: u64) -> bool {
    if rev == 0 {
        return true;
    }
    let old_tip = old.events_after(rev - 1, 1).ok().and_then(|s| s.first()).map(|e| e.op.id);
    let new_tip = new.events_after(rev - 1, 1).ok().and_then(|s| s.first()).map(|e| e.op.id);
    old_tip.is_some() && old_tip == new_tip
}

fn prepare(request: &Request) -> Result<WorldRenderer, String> {
    if request.source.canvas != request.key.scope.canvas {
        return Err("Shared view source belongs to another canvas".into());
    }
    match request.key.revision {
        Revision::Live { display, .. } => {
            if request.source.revision != display {
                return Err("Shared view revision mismatch".into());
            }
            WorldRenderer::new(&request.source).map_err(|e| e.to_string())
        }
        Revision::Review { end, position } => {
            if position > end || end > request.source.revision {
                return Err("Invalid fixed review projection".into());
            }
            let scene = request.source.replay_to(position).map_err(|e| e.to_string())?;
            WorldRenderer::new(&scene).map_err(|e| e.to_string())
        }
    }
}

#[cfg(test)]
mod tests;
