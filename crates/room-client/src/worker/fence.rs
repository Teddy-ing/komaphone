//! An account pause cannot wait behind a full work or presentation channel.
use crate::{Error, Result};
use std::sync::{
    Mutex, PoisonError,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

#[derive(Default)]
pub(crate) struct AuthFence {
    state: AtomicU64,
    stopped: AtomicBool,
}
impl AuthFence {
    pub(crate) fn ticket(&self) -> u64 {
        self.state.load(Ordering::Acquire)
    }
    pub(crate) fn pause(&self) -> u64 {
        let previous = self.state.fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| Some(value.saturating_add(2) | 1)).unwrap_or(u64::MAX);
        previous.saturating_add(2) | 1
    }
    pub(crate) fn current(&self, ticket: u64) -> bool {
        !self.stopped() && self.ticket() >> 1 == ticket >> 1
    }
    pub(crate) fn allowed(&self, ticket: u64) -> bool {
        let now = self.ticket();
        !self.stopped() && now & 1 == 0 && now >> 1 == ticket >> 1
    }
    pub(crate) fn paused(&self) -> bool {
        self.stopped() || self.pause_requested()
    }
    pub(crate) fn pause_requested(&self) -> bool {
        self.ticket() & 1 != 0
    }
    /// Stop this runtime without turning a normal close into a saved account pause.
    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
    }
    pub(crate) fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
    pub(crate) fn resume(&self, ticket: u64) -> bool {
        if self.stopped() {
            return false;
        }
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| (current != u64::MAX && current >> 1 == ticket >> 1).then_some(current & !1))
            .is_ok()
    }
    pub(crate) fn check(&self, ticket: u64) -> Result<()> {
        if self.allowed(ticket) {
            Ok(())
        } else {
            Err(Error::Rejected { code: "account_changed".into(), message: "Account changed or paused; queued artwork remains local".into() })
        }
    }
}

pub(crate) struct Stamped<T> {
    pub(crate) ticket: u64,
    pub(crate) value: T,
}
pub(crate) struct Priority<T> {
    slot: Mutex<Option<T>>,
}
impl<T> Default for Priority<T> {
    fn default() -> Self {
        Self { slot: Mutex::new(None) }
    }
}
impl<T> Priority<T> {
    pub(crate) fn put(&self, value: T) {
        *self.slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(value);
    }
    pub(crate) fn take(&self) -> Option<T> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_resume_cannot_undo_a_new_pause() {
        let fence = AuthFence::default();
        let first = fence.pause();
        let newer = fence.pause();
        assert!(!fence.resume(first));
        assert!(fence.paused());
        assert!(fence.resume(newer));
        assert!(fence.allowed(newer));
        assert!(!fence.allowed(first));
    }
    #[test]
    fn runtime_stop_blocks_old_access_without_inventing_an_account_pause() {
        for paused in [false, true] {
            let fence = AuthFence::default();
            if paused {
                fence.pause();
            }
            let ticket = fence.ticket();
            fence.stop();
            assert!(fence.paused());
            assert_eq!(fence.pause_requested(), paused);
            assert!(!fence.current(ticket));
            assert!(!fence.allowed(ticket));
            assert!(!fence.resume(ticket));
        }
    }
}
