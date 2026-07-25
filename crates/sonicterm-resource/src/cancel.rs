//! Level-triggered cancellation shared by supervised transports and workers.

use parking_lot::{Condvar, Mutex};
use sonicterm_types::CancelReason;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

struct CancelState {
    reason: Mutex<Option<CancelReason>>,
    signal: Condvar,
    cancelled: AtomicBool,
}

/// Publishes cancellation to every holder of a [`CancelToken`].
///
/// Cancellation is level-triggered: once published it stays observable, so a
/// worker that starts or wakes after the signal still sees it. This is what lets
/// a transport be cancelled without racing the moment it checks.
pub struct CancelSource {
    state: Arc<CancelState>,
}

impl CancelSource {
    /// Create a source and its first token.
    pub fn new() -> Self {
        Self {
            state: Arc::new(CancelState {
                reason: Mutex::new(None),
                signal: Condvar::new(),
                cancelled: AtomicBool::new(false),
            }),
        }
    }

    /// Hand out a token observing this source.
    pub fn token(&self) -> CancelToken {
        CancelToken { state: self.state.clone() }
    }

    /// Publish cancellation, waking every waiter.
    ///
    /// Repeat calls keep the first reason: the original cause of teardown is the
    /// useful one for diagnosis, and later cascading reasons would mask it.
    pub fn cancel(&self, reason: CancelReason) {
        let mut current = self.state.reason.lock();
        if current.is_none() {
            *current = Some(reason);
            // Release under the lock so no waiter can miss the transition.
            self.state.cancelled.store(true, Ordering::Release);
        }
        drop(current);
        self.state.signal.notify_all();
    }

    /// Return whether cancellation was already published.
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }
}

impl Default for CancelSource {
    fn default() -> Self {
        Self::new()
    }
}

/// Observes cancellation published by a [`CancelSource`].
#[derive(Clone)]
pub struct CancelToken {
    state: Arc<CancelState>,
}

impl CancelToken {
    /// Return whether cancellation has been published.
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    /// Return the recorded reason, if cancellation was published.
    pub fn reason(&self) -> Option<CancelReason> {
        *self.state.reason.lock()
    }

    /// Block until cancellation is published.
    pub fn wait(&self) {
        let mut reason = self.state.reason.lock();
        while reason.is_none() {
            self.state.signal.wait(&mut reason);
        }
    }

    /// Block until cancellation is published or the deadline elapses.
    ///
    /// Returns whether cancellation was observed. A false return is a timeout,
    /// not a release: the caller still owns whatever it was protecting.
    pub fn wait_until(&self, deadline: Instant) -> bool {
        let mut reason = self.state.reason.lock();
        while reason.is_none() {
            if self.state.signal.wait_until(&mut reason, deadline).timed_out() && reason.is_none() {
                return false;
            }
        }
        true
    }

    /// Block until cancellation is published or the timeout elapses.
    pub fn wait_for(&self, timeout: Duration) -> bool {
        match Instant::now().checked_add(timeout) {
            Some(deadline) => self.wait_until(deadline),
            None => {
                self.wait();
                true
            }
        }
    }
}

#[cfg(test)]
#[path = "cancel_tests.rs"]
mod cancel_tests;
