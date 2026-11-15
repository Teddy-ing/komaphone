//! One bounded native CPU worker per window. Replacing the latest request cannot
//! spawn extra workers or publish a stale account/canvas/review projection.
use photocraft_engine::{
    scene::{CanvasId, shared::WorldState},
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
        // A different scope cannot reuse an exhausted serial. No new request
        // can be issued after that practically unreachable limit.
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
        let result = prepare(&request);
        let mut mailbox = lock(&shared);
        if mailbox.closed {
            return;
        }
        if mailbox.serial == serial || (result.is_ok() && mailbox.request.as_ref().is_some_and(|latest| request.key.may_show_before(&latest.key))) {
            let previous = mailbox.result.replace(Completed { key: request.key, result });
            drop(mailbox);
            drop(previous);
        }
    }
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
