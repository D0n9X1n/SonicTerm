//! The renderer-waiting handshake between a window and a pane's VT worker.
//!
//! On hardware, a window whose frame missed a pane's parser asks that pane's worker for its next
//! gap between batches. The worker sends `ParserYielded` with a park deadline it fixed before the
//! send, then parks between batches, holding no lock, until a window serves its generation or the
//! deadline passes. Serving stores the generation before it unparks, and unparks only on an
//! effective advance, so no wake is lost and none is spent on an already-served generation.

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        OnceLock,
    },
    thread::Thread,
    time::{Duration, Instant},
};

/// How long a worker that granted a request waits for a window to serve it: an experimental
/// requested budget, capped at an open synchronized update's stored deadline.
pub(in crate::app) const PARSER_YIELD_PARK: Duration = Duration::from_millis(2);

/// One pane's handshake state, shared by its worker, its pane and each frame collection.
#[derive(Debug, Default)]
pub(in crate::app) struct ParserYield {
    /// The newest generation a window requested; only the event loop advances it.
    pub(in crate::app) requested: AtomicU64,
    /// The newest generation a window served; it never decreases.
    pub(in crate::app) served: AtomicU64,
    /// The pane's VT worker thread, registered once before its first receive.
    worker: OnceLock<Thread>,
}

impl ParserYield {
    /// A handshake with nothing requested or served and no worker registered.
    pub(in crate::app) fn new() -> Self {
        Self::default()
    }

    /// Publish a new request and return its generation.
    // Ordering: requested fetch_add Release; the worker's Acquire load sees it, and no other data
    // is published through it.
    pub(in crate::app) fn request(&self) -> u64 {
        self.requested.fetch_add(1, Ordering::Release) + 1
    }

    /// Register the calling thread as this pane's worker; a second registration is ignored.
    pub(in crate::app) fn register_worker(&self) {
        let _ = self.worker.set(std::thread::current());
    }

    /// Serve every request up to `generation`, waking the worker only on an effective advance.
    ///
    /// Returns whether `served` advanced. An already-served or older generation does nothing.
    // Ordering: served fetch_max Release before the unpark, pairing with the worker's Acquire
    // re-check after each park return, so a woken worker always sees the generation it waits for.
    pub(in crate::app) fn serve_outstanding(&self, generation: u64) -> bool {
        let previous = self.served.fetch_max(generation, Ordering::Release);
        if previous >= generation {
            // When: `previous` already covers `generation`, nothing advanced, so nothing wakes.
            return false;
        }
        let unparked = self.wake();
        #[cfg(test)]
        SERVE_LOG.with(|log| {
            log.borrow_mut().push(ServeEntry {
                handshake: std::ptr::from_ref(self) as usize,
                old: previous,
                new: generation,
                unparked,
            });
        });
        let _ = unparked;
        true
    }

    /// Unpark the registered worker; a test's hook replaces the real unpark when installed.
    fn wake(&self) -> bool {
        #[cfg(test)]
        if let Some(unparked) = UNPARK_HOOK.with(|hook| {
            hook.borrow_mut().as_mut().map(|hook| {
                hook(self);
                true
            })
        }) {
            return unparked;
        }
        self.worker.get().map(Thread::unpark).is_some()
    }
}

/// The clock and park the worker waits on: the real clock and thread park in production, one
/// fake shared with the window side in tests.
pub(in crate::app) trait YieldClock {
    /// The current instant.
    fn now(&mut self) -> Instant;
    /// Park until `deadline` or an unpark, whichever is first; it may also return spuriously.
    fn park_until(&mut self, deadline: Instant);
}

/// The production clock: `Instant::now` and `thread::park_timeout` on the worker's park token.
pub(in crate::app) struct ThreadYieldClock;

impl YieldClock for ThreadYieldClock {
    fn now(&mut self) -> Instant {
        Instant::now()
    }

    fn park_until(&mut self, deadline: Instant) {
        std::thread::park_timeout(deadline.saturating_duration_since(Instant::now()));
    }
}

/// One successful send's wait: measured, not bounded by the code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) struct YieldWait {
    /// From the clock reading before the send to the end of the wait predicate.
    pub(in crate::app) wait: Duration,
    /// Park calls made; a stale token or a spurious return adds one.
    pub(in crate::app) parks: u32,
    /// Whether the wait ended with the generation still unserved.
    pub(in crate::app) timed_out: bool,
    /// How far past its deadline the wait ended, when it did.
    pub(in crate::app) overshoot: Option<Duration>,
}

/// What one between-batch handshake step did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum YieldOutcome {
    /// No request is newer than the last grant.
    NoRequest,
    /// The pane has no redraw target to tell.
    NoTarget,
    /// An open update's hold is already due, so there is no time to wait.
    Expired,
    /// The event loop refused the event.
    SendFailed,
    /// The event was sent and the wait finished.
    Sent(YieldWait),
}

/// Offer the pane's next gap to the newest unanswered request, between batches.
///
/// The worker grants each generation once. `target` reads the redraw target and must release
/// its guard before returning, so nothing is held across the send or the park. The park deadline
/// is fixed before the send, capped at `held_deadline`, and never recomputed after a wake.
// Ordering: requested and served load Acquire, pairing with the event loop's Release stores.
pub(in crate::app) fn yield_step<Target>(
    handshake: &ParserYield,
    granted: &mut u64,
    held_deadline: Option<Instant>,
    target: impl FnOnce() -> Option<Target>,
    send: impl FnOnce(Target, u64, Instant) -> bool,
    clock: &mut impl YieldClock,
) -> YieldOutcome {
    let requested = handshake.requested.load(Ordering::Acquire);
    if requested <= *granted {
        // When: `requested` is not newer than `granted`, no window is waiting on this worker.
        return YieldOutcome::NoRequest;
    }
    *granted = requested;
    let Some(target) = target() else {
        // When: `target` is None, no window can receive the grant; the window's ask expires.
        return YieldOutcome::NoTarget;
    };
    let sent_at = clock.now();
    let requested_deadline = sent_at + PARSER_YIELD_PARK;
    let deadline = held_deadline.map_or(requested_deadline, |held| held.min(requested_deadline));
    if deadline <= sent_at {
        // When: `deadline` is already due, the open update's hold must be released, not waited on.
        return YieldOutcome::Expired;
    }
    if !send(target, requested, deadline) {
        // When: `send` failed, nobody will serve this generation, so the worker does not park.
        return YieldOutcome::SendFailed;
    }
    let mut parks = 0;
    while handshake.served.load(Ordering::Acquire) < requested && clock.now() < deadline {
        clock.park_until(deadline);
        parks += 1;
    }
    let end = clock.now();
    YieldOutcome::Sent(YieldWait {
        wait: end.saturating_duration_since(sent_at),
        parks,
        timed_out: handshake.served.load(Ordering::Acquire) < requested,
        overshoot: end.checked_duration_since(deadline).filter(|over| !over.is_zero()),
    })
}

/// Test-only: one effective advance of a handshake's `served`.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) struct ServeEntry {
    /// The handshake's address, identity only.
    pub(in crate::app) handshake: usize,
    /// `served` before the advance.
    pub(in crate::app) old: u64,
    /// `served` after it.
    pub(in crate::app) new: u64,
    /// Whether a worker (or the test's hook) was unparked.
    pub(in crate::app) unparked: bool,
}

/// Test-only replacement for the real unpark: called with the handshake being woken.
#[cfg(test)]
pub(in crate::app) type UnparkHook = Box<dyn FnMut(&ParserYield)>;

#[cfg(test)]
thread_local! {
    /// Test-only: this thread's effective serves, in order.
    static SERVE_LOG: std::cell::RefCell<Vec<ServeEntry>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Test-only: replaces the real unpark on this thread while installed.
    static UNPARK_HOOK: std::cell::RefCell<Option<UnparkHook>> = const { std::cell::RefCell::new(None) };
}

/// Test-only: take this thread's effective serves.
#[cfg(test)]
pub(in crate::app) fn take_serve_log() -> Vec<ServeEntry> {
    SERVE_LOG.with(|log| std::mem::take(&mut *log.borrow_mut()))
}

/// Test-only: install or remove this thread's unpark hook.
#[cfg(test)]
pub(in crate::app) fn set_unpark_hook(hook: Option<UnparkHook>) {
    UNPARK_HOOK.with(|slot| *slot.borrow_mut() = hook);
}

#[cfg(test)]
#[path = "parser_yield_tests.rs"]
pub(in crate::app) mod parser_yield_tests;
