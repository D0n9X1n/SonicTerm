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
    // Ordering: served fetch_max Release precedes the unpark; the worker's Acquire re-check
    // after each park return sees the generation.
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
            // When: a test installed an unpark hook, it stands in for the real unpark.
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

/// Why a token ended without a frame; logged at `debug`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum YieldLoss {
    /// The window was suppressed: stopped, occluded, parked or hidden.
    Suppressed,
    /// The worker's deadline passed, or the floor changed, before admission.
    Expired,
    /// The pane left the window's visible set, or is another pane now.
    Moved,
    /// The software path took over.
    Software,
    /// Admission deferred by a surface timeout or a synchronized-output hold.
    Deferred,
    /// The retry found a visible parser busy.
    Parser,
    /// The retry found a visible image store busy.
    Images,
    /// The guarded synchronized-output recheck abandoned the retry.
    Sync,
    /// The retry could not collect a frame: no renderer, no layout or invalid topology.
    Invalid,
    /// The window was removed.
    Removed,
}

/// How far an accepted grant has got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum TokenStage {
    /// Accepted; the next admission may bypass the floor and the streaming carry once.
    Granted,
    /// Admitted; the attempt in flight must resolve it before it returns.
    Admitted,
}

/// A window's outstanding request to one pane's worker.
#[derive(Clone, Debug)]
pub(in crate::app) struct YieldAsk {
    /// The pane's handshake, compared by identity.
    pub(in crate::app) handshake: std::sync::Arc<ParserYield>,
    /// The pane the request names.
    pub(in crate::app) pane_id: u64,
    /// The request's generation.
    pub(in crate::app) generation: u64,
    /// The contention floor armed by the miss that published the request.
    pub(in crate::app) floor: Instant,
}

/// An accepted grant: one admission may bypass the floor and the streaming carry before
/// `park_deadline`.
#[derive(Clone, Debug)]
pub(in crate::app) struct YieldToken {
    /// The pane's handshake, compared by identity.
    pub(in crate::app) handshake: std::sync::Arc<ParserYield>,
    /// The pane the grant names.
    pub(in crate::app) pane_id: u64,
    /// The granted generation.
    pub(in crate::app) generation: u64,
    /// The floor the grant was accepted under; it must still be armed.
    pub(in crate::app) floor: Instant,
    /// The deadline the worker fixed before it sent the grant.
    pub(in crate::app) park_deadline: Instant,
    /// Granted, or admitted by the attempt in flight.
    pub(in crate::app) stage: TokenStage,
}

/// How a window answered a grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum YieldAnswer {
    /// Accepted: request a redraw.
    Accepted,
    /// Rejected or declined; the worker has been served.
    Rejected,
    /// The window does not hold the pane; serve it wherever it lives.
    NotHeld,
}

impl super::redraw::WindowRedrawState {
    /// End the token: count it as a frame, or as lost for `loss`, serve its generation, and
    /// clear it, which ends the carry and the bypass. A frame was already served at collection.
    pub(in crate::app) fn resolve_yield_token(&mut self, loss: Option<YieldLoss>) {
        let Some(token) = self.yield_token.take() else {
            // When: no token is outstanding, there is nothing to resolve or count.
            return;
        };
        token.handshake.serve_outstanding(token.generation);
        if let Some(reason) = loss {
            tracing::debug!(
                target: "sonicterm_app::parser_yield",
                pane_id = token.pane_id,
                generation = token.generation,
                ?reason,
                "yield token lost"
            );
        }
        if let Some(counters) = self.frame_counters.as_deref_mut() {
            // the App's gate is on, the token's end counts as a frame or a loss.
            if loss.is_some() {
                counters.parser_yield_lost += 1;
            } else {
                // When: `loss` is None, the coherent collection completed the granted retry.
                counters.parser_yield_frames += 1;
            }
        }
    }

    /// Resolve any token as lost for `loss`: an eager suppression or a removal.
    pub(in crate::app) fn suppress_yield(&mut self, loss: YieldLoss) {
        if self.yield_token.is_some() {
            self.resolve_yield_token(Some(loss));
        }
    }

    /// Resolve an admitted token as lost for `loss`, at an adapter exit before collection.
    pub(in crate::app) fn finish_admitted_yield(&mut self, loss: YieldLoss) {
        if self.yield_token.as_ref().is_some_and(|token| token.stage == TokenStage::Admitted) {
            self.resolve_yield_token(Some(loss));
        }
    }

    /// Clear any outstanding request, serving its generation.
    pub(in crate::app) fn resolve_yield_ask(&mut self) {
        if let Some(ask) = self.yield_ask.take() {
            ask.handshake.serve_outstanding(ask.generation);
        }
    }

    /// Count one grant that was not accepted.
    fn count_rejected(&mut self) {
        if let Some(counters) = self.frame_counters.as_deref_mut() {
            // the App's gate is on, each grant not accepted is counted.
            counters.parser_yield_rejected += 1;
        }
    }
}

impl super::WindowState {
    /// The handshake of `pane_id` if this window shows it now.
    fn visible_handshake(&self, pane_id: u64) -> Option<&std::sync::Arc<ParserYield>> {
        self.visible_pane_ids()
            .contains(&pane_id)
            .then(|| self.panes.get(&pane_id).map(|pane| &pane.parser_yield))
            .flatten()
    }

    /// Resolve an outstanding request that can no longer be granted: its floor passed, it was
    /// served, or its pane left the visible set or is another pane now.
    // Ordering: served loads Acquire, pairing with serve_outstanding's Release.
    fn resolve_stale_ask(&mut self, now: Instant) {
        let Some(ask) = self.redraw.yield_ask.as_ref() else {
            // When: `yield_ask` is None, no request is outstanding, so nothing can be stale.
            return;
        };
        let stale = now >= ask.floor
            || ask.handshake.served.load(Ordering::Acquire) >= ask.generation
            || !self
                .visible_handshake(ask.pane_id)
                .is_some_and(|handshake| std::sync::Arc::ptr_eq(handshake, &ask.handshake));
        if stale {
            self.redraw.resolve_yield_ask();
        }
    }

    /// Revalidate a granted token at admission; a token no longer live resolves as lost, and
    /// the ordinary rules then run against the still-armed floor. Returns whether it is live.
    pub(in crate::app) fn revalidate_yield_token(&mut self, now: Instant, software: bool) -> bool {
        let Some(token) = self.redraw.yield_token.as_ref() else {
            // When: no token is outstanding, admission runs its ordinary rules.
            return false;
        };
        if token.stage != TokenStage::Granted {
            // When: an admitted token reaches admission, the backstop has already resolved it.
            return false;
        }
        let loss = if software {
            Some(YieldLoss::Software)
        } else if now >= token.park_deadline || self.retry_not_before != Some(token.floor) {
            // When: the worker's deadline passed or the floor changed, the grant is spent.
            Some(YieldLoss::Expired)
        } else if !self
            .visible_handshake(token.pane_id)
            .is_some_and(|handshake| std::sync::Arc::ptr_eq(handshake, &token.handshake))
        {
            // When: `visible_handshake` of `token.pane_id` is absent or not `ptr_eq`, the grant moved.
            Some(YieldLoss::Moved)
        } else {
            // When: `software`, `park_deadline`, `retry_not_before` and `visible_handshake` all hold, it is live.
            None
        };
        match loss {
            Some(loss) => {
                self.redraw.suppress_yield(loss);
                false
            }
            None => true,
        }
    }

    /// A coherent collection supersedes any request, resolves any token as a frame, and opens
    /// a new episode. The collection already served every visible pane under its guards.
    pub(in crate::app) fn yield_collected(&mut self) {
        self.redraw.resolve_yield_ask();
        self.redraw.resolve_yield_token(None);
        self.redraw.yield_episode_spent = false;
    }

    /// The window is being removed: resolve its token `lost(removed)` and its request, before
    /// its counters retire, so the closed totals carry the loss and no occupancy.
    pub(in crate::app) fn resolve_window_yield(&mut self) {
        self.redraw.suppress_yield(YieldLoss::Removed);
        self.redraw.resolve_yield_ask();
    }

    /// Answer a worker's grant for `pane_id`'s `generation`, sent with `park_deadline`.
    // Ordering: served loads Acquire, pairing with serve_outstanding's Release.
    fn answer_parser_yield(
        &mut self,
        pane_id: u64,
        generation: u64,
        park_deadline: Instant,
        now: Instant,
        software: bool,
    ) -> YieldAnswer {
        let Some(held) =
            self.panes.get(&pane_id).map(|pane| std::sync::Arc::clone(&pane.parser_yield))
        else {
            // When: this window does not hold the pane, any request for it here is moved.
            if self.redraw.yield_ask.as_ref().is_some_and(|ask| ask.pane_id == pane_id) {
                self.redraw.resolve_yield_ask();
            }
            self.redraw.count_rejected();
            return YieldAnswer::NotHeld;
        };
        let matched = self.redraw.yield_ask.as_ref().is_some_and(|ask| {
            ask.pane_id == pane_id
                && ask.generation == generation
                && std::sync::Arc::ptr_eq(&ask.handshake, &held)
        });
        if !matched {
            // When: `matched` is false, the grant is stale; the worker is served and
            // the current request, if any, stays.
            held.serve_outstanding(generation);
            self.redraw.count_rejected();
            return YieldAnswer::Rejected;
        }
        let floor = self.redraw.yield_ask.as_ref().map(|ask| ask.floor).expect("matched");
        let valid = self.retry_not_before == Some(floor)
            && now < floor
            && now < park_deadline
            && held.served.load(Ordering::Acquire) < generation;
        let eligible = self.visible_handshake(pane_id).is_some()
            && self.frame_deadlines_allowed()
            && !self.redraw.timeout_pending
            && !self.sync_defers(now)
            && !software;
        if !(valid && eligible) {
            // When: `valid` and `eligible` do not both hold, the request resolves and
            // the floor stays.
            self.redraw.resolve_yield_ask();
            self.redraw.count_rejected();
            return YieldAnswer::Rejected;
        }
        let ask = self.redraw.yield_ask.take().expect("matched");
        self.redraw.yield_episode_spent = true;
        self.redraw.yield_token = Some(YieldToken {
            handshake: ask.handshake,
            pane_id,
            generation,
            floor,
            park_deadline,
            stage: TokenStage::Granted,
        });
        if let Some(counters) = self.redraw.frame_counters.as_deref_mut() {
            // the App's gate is on, each accepted grant is counted.
            counters.parser_yield_wakes += 1;
        }
        YieldAnswer::Accepted
    }
}

impl super::App {
    /// Answer a worker's grant. An accepted grant keeps the floor, serves nothing and asks for
    /// one redraw; every other answer serves the worker so it resumes before its deadline.
    pub(super) fn handle_parser_yielded(
        &mut self,
        window_id: winit::window::WindowId,
        pane_id: u64,
        generation: u64,
        park_deadline: Instant,
    ) {
        let now = self.dispatch_now();
        let software = self.software_render_degrade;
        let answer = self.windows.get_mut(&window_id).map_or(YieldAnswer::NotHeld, |window| {
            window.answer_parser_yield(pane_id, generation, park_deadline, now, software)
        });
        match answer {
            YieldAnswer::Accepted => {
                if let Some(window) = self.windows.get(&window_id) {
                    window.request_window_redraw();
                }
            }
            YieldAnswer::Rejected => {
                // When: `YieldAnswer::Rejected`, the window rejected or declined and already served the worker.
            }
            YieldAnswer::NotHeld => {
                // The window is gone or never held the pane: serve the pane where it lives.
                let pane = self.windows.values().find_map(|window| window.panes.get(&pane_id));
                if let Some(pane) = pane {
                    pane.parser_yield.serve_outstanding(generation);
                }
            }
        }
    }

    /// Publish one request after a hardware parser miss on `pane_id`, when the episode is open
    /// and no request is outstanding. The floor is already armed by the miss's deferral.
    pub(super) fn publish_parser_yield(
        &mut self,
        id: winit::window::WindowId,
        pane_id: u64,
        images: bool,
        now: Instant,
    ) {
        if images || self.software_render_degrade {
            // When: `images` or `software_render_degrade` is set, no worker gap would help.
            return;
        }
        let Some(window) = self.windows.get_mut(&id) else {
            // When: `id` closed, there is no window to retry.
            return;
        };
        window.resolve_stale_ask(now);
        if window.redraw.yield_episode_spent || window.redraw.yield_ask.is_some() {
            // When: `yield_episode_spent` or `yield_ask` is set, the episode used its retry or a request waits.
            return;
        }
        let (Some(floor), Some(pane)) = (window.retry_not_before, window.panes.get(&pane_id))
        else {
            // When: no floor is armed or the pane is gone, there is nothing to request.
            return;
        };
        let handshake = std::sync::Arc::clone(&pane.parser_yield);
        let generation = handshake.request();
        window.redraw.yield_ask = Some(YieldAsk { handshake, pane_id, generation, floor });
        if let Some(counters) = window.redraw.frame_counters.as_deref_mut() {
            // the App's gate is on, each published request is counted.
            counters.parser_yield_requests += 1;
        }
    }

    /// Resolve every window's token `lost(removed)` and its request before shutdown reports the
    /// final counters or retires panes, gate on or off. A resolved window holds neither, so a
    /// repeat serves and counts nothing.
    pub(super) fn resolve_all_window_yields(&mut self) {
        for window in self.windows.values_mut() {
            window.resolve_window_yield();
        }
    }

    /// Resolve `id`'s admitted token as lost for `loss` at an adapter exit before collection.
    pub(super) fn finish_yield_attempt(&mut self, id: winit::window::WindowId, loss: YieldLoss) {
        if let Some(window) = self.windows.get_mut(&id) {
            window.redraw.finish_admitted_yield(loss);
        }
    }
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

// `parser_yield` is private to `app`, so this public declaration reaches no further than `app`;
// the worker-loop tests in `spawn_pane_tests` share its fake park.
#[cfg(test)]
#[path = "parser_yield_tests.rs"]
pub mod parser_yield_tests;
