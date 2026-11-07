//! A pane's change signal: a sequence the VT worker bumps after each published batch and the
//! event loop bumps after it records the process's exit, with a condition variable so an MCP
//! `wait_for` sleeps until one of them happens instead of polling.

use std::time::Instant;

use parking_lot::{Condvar, Mutex};

/// A sequence of pane changes and the waiters sleeping on it. Shared by the pane, its VT worker
/// and every MCP entry that names the pane.
#[derive(Debug, Default)]
pub(crate) struct PaneSignal {
    /// How many changes have been announced; only ever grows.
    sequence: Mutex<u64>,
    /// Woken, all at once, after each announcement.
    changed: Condvar,
}

impl PaneSignal {
    /// The current sequence. A waiter reads it *before* it inspects the pane, so a change that
    /// lands during the inspection still moves the sequence past what it waits on.
    pub(crate) fn sequence(&self) -> u64 {
        *self.sequence.lock()
    }

    /// Announce a change and wake every waiter. Callers announce only after the change is
    /// visible (the batch's generation published, the exit cell set) and with no parser or
    /// media guard held, so a woken waiter never blocks on the announcer.
    pub(crate) fn notify(&self) {
        *self.sequence.lock() += 1;
        self.changed.notify_all();
    }

    /// Sleep until the sequence moves past `seen` or `deadline` passes, and return the sequence
    /// then. The check and the sleep happen under one lock, so a change announced after `seen`
    /// was read is never slept through.
    pub(crate) fn wait_past(&self, seen: u64, deadline: Instant) -> u64 {
        let mut sequence = self.sequence.lock();
        while *sequence == seen {
            // Reported with the mutex held and the sequence confirmed unchanged, so a publisher that
            // acts on it can only bump the sequence after this waiter is on the condition variable.
            #[cfg(test)]
            PARK_HOOK.with_borrow(|hook| hook.as_ref().map(|hook| hook()));
            if self.changed.wait_until(&mut sequence, deadline).timed_out() {
                // When: the `deadline` passed with no announcement, the caller rechecks its state.
                break;
            }
        }
        *sequence
    }
}

#[cfg(test)]
thread_local! {
    /// Called on this thread each time a wait is about to block on the condition variable, with
    /// the signal's mutex held, so a test synchronizes on real registration rather than sleeping.
    pub(crate) static PARK_HOOK: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
#[path = "pane_signal_tests.rs"]
mod pane_signal_tests;
