//! A pane's change signal: a sequence the VT worker bumps after each published batch, the event
//! loop bumps after it records the process's exit, and pane retirement bumps once as it closes the
//! signal, with a condition variable so an MCP wait sleeps until one of them happens instead of
//! polling. The MCP server also wakes every registered waiter when it revokes calls.

use std::time::Instant;

use parking_lot::{Condvar, Mutex};

/// A sequence of pane changes and the waiters sleeping on it. Shared by the pane, its VT worker
/// and every MCP entry that names the pane.
#[derive(Debug, Default)]
pub(crate) struct PaneSignal {
    /// How many changes have been announced, and whether the pane closed.
    state: Mutex<SignalState>,
    /// Woken, all at once, after each announcement.
    changed: Condvar,
    /// Told each time any thread is about to sleep on this signal, with its mutex held, so a test
    /// synchronizes with a wait on a thread it does not own (a connection's worker).
    #[cfg(test)]
    pub(crate) park_notifier: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

/// The guarded part of a [`PaneSignal`].
#[derive(Debug, Default)]
struct SignalState {
    /// How many changes have been announced; only ever grows.
    sequence: u64,
    /// The pane was retired; set once, never cleared.
    closed: bool,
}

impl PaneSignal {
    /// The current sequence. A waiter reads it *before* it inspects the pane, so a change that
    /// lands during the inspection still moves the sequence past what it waits on.
    pub(crate) fn sequence(&self) -> u64 {
        self.state.lock().sequence
    }

    /// Whether the pane was retired. A waiter that saw this still scans output published before
    /// the close, which the close announcement follows.
    pub(crate) fn is_closed(&self) -> bool {
        self.state.lock().closed
    }

    /// Mark the pane closed and wake every waiter. Only pane retirement calls this; a pane moved
    /// between tabs or windows keeps its signal open.
    pub(crate) fn close(&self) {
        let mut state = self.state.lock();
        state.closed = true;
        state.sequence += 1;
        drop(state);
        self.changed.notify_all();
    }

    /// Announce a change and wake every waiter. Callers announce only after the change is
    /// visible (the batch's generation published, the exit cell set) and with no parser or
    /// media guard held, so a woken waiter never blocks on the announcer.
    pub(crate) fn notify(&self) {
        self.state.lock().sequence += 1;
        self.changed.notify_all();
    }

    /// Sleep until the sequence moves past `seen`, the signal is closed, or `deadline` passes, and
    /// return the sequence then. The check and the sleep happen under one lock, so a change
    /// announced after `seen` was read is never slept through, and a closed signal never sleeps.
    // Lock order: state, then the test-only park_notifier; nothing takes park_notifier first.
    pub(crate) fn wait_past(&self, seen: u64, deadline: Instant) -> u64 {
        let mut state = self.state.lock();
        while state.sequence == seen && !state.closed {
            // Reported with the mutex held and the sequence confirmed unchanged, so a publisher that
            // acts on it can only bump the sequence after this waiter is on the condition variable.
            #[cfg(test)]
            PARK_HOOK.with_borrow(|hook| hook.as_ref().map(|hook| hook()));
            #[cfg(test)]
            if let Some(notifier) = self.park_notifier.lock().as_ref() {
                // When: a test installed a `notifier`; one that stopped listening is ignored.
                let _ = notifier.send(());
            }
            if self.changed.wait_until(&mut state, deadline).timed_out() {
                // When: the `deadline` passed with no announcement, the caller rechecks its state.
                break;
            }
        }
        state.sequence
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
