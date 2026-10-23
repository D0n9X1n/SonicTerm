//! Busy slots for SonicTerm's slow background work, and the `path_check` segment's hold rule.
//!
//! Each owner stamps its slot immediately before its blocking call and clears it when the call
//! ends, through a guard, so a panic or early return also clears it. Slots are always stamped;
//! only the status line reads them, and only while it lists `path_check`. Queue wait is never
//! counted: a slot covers execution only.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use sonicterm_cfg::status_line::PathCheckSettings;
use sonicterm_ui::status_line::PathCheckInput;

/// One worker's busy slot: microseconds since `origin` plus one when busy, 0 when idle.
#[derive(Debug)]
pub(crate) struct BusySlot {
    origin: Instant,
    started: AtomicU64,
}

impl BusySlot {
    /// An idle slot measuring from `origin`.
    pub(crate) fn new(origin: Instant) -> Self {
        Self { origin, started: AtomicU64::new(0) }
    }

    /// Mark the slot busy from now until the returned guard drops.
    // Ordering: started is stored Release, pairing with started_at's Acquire load.
    pub(crate) fn enter(&self) -> BusyGuard<'_> {
        let micros = Instant::now().saturating_duration_since(self.origin).as_micros() as u64;
        self.started.store(micros.saturating_add(1), Ordering::Release);
        BusyGuard { slot: self }
    }

    /// When the running call started, or `None` when the slot is idle.
    // Ordering: started is loaded Acquire, pairing with enter's Release store.
    pub(crate) fn started_at(&self) -> Option<Instant> {
        match self.started.load(Ordering::Acquire) {
            0 => None,
            stamp => Some(self.origin + Duration::from_micros(stamp - 1)),
        }
    }
}

/// Clears its slot when the guarded call ends, however it ends.
pub(crate) struct BusyGuard<'slot> {
    slot: &'slot BusySlot,
}

// Lifecycle: dropping `BusyGuard` clears `slot`, on return, early exit or unwind.
impl Drop for BusyGuard<'_> {
    // Ordering: started is stored Release, so a reader never sees a stale start after the end.
    fn drop(&mut self) {
        self.slot.started.store(0, Ordering::Release);
    }
}

/// The path workers' busy slots: the openability probe and the target open.
#[derive(Debug)]
pub(crate) struct PathBusy {
    /// The `sonicterm-path-probe` worker, around one request's candidate probes.
    pub(crate) probe: BusySlot,
    /// The `sonicterm-path-open` worker, around one re-probe and native open or reveal.
    pub(crate) open: BusySlot,
}

impl PathBusy {
    /// Two idle slots sharing one clock origin.
    pub(crate) fn new() -> Self {
        let origin = Instant::now();
        Self { probe: BusySlot::new(origin), open: BusySlot::new(origin) }
    }

    /// The start of the oldest running call, if any.
    pub(crate) fn oldest(&self) -> Option<Instant> {
        match (self.probe.started_at(), self.open.started_at()) {
            (Some(probe), Some(open)) => Some(probe.min(open)),
            (probe, open) => probe.or(open),
        }
    }
}

/// The `path_check` display's one hold rule: it appears when the oldest running call is older
/// than `show_after_ms`, and after every call ends it stays for `hold_ms`; a call that starts
/// during the hold continues the same display.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PathCheckHold {
    /// The display last shown, kept through the hold.
    shown: Option<PathCheckInput>,
    /// When every call was first seen idle while shown.
    idle_since: Option<Instant>,
    /// A submit's recheck, `submitted + show_after_ms`, fixed until it fires.
    recheck: Option<Instant>,
}

impl PathCheckHold {
    /// Note a submitted probe or open: its call may start later, so recheck at `show_after_ms`.
    pub(crate) fn note_submit(&mut self, now: Instant, settings: PathCheckSettings) {
        if self.recheck.is_none() {
            self.recheck = Some(now + Duration::from_millis(settings.show_after_ms));
        }
    }

    /// Advance to `now` with `oldest`, the oldest running call's start, and `waiting`, the checks
    /// queued behind it, and return what to show and the next wake, which is always after `now`.
    /// While checks wait, the recheck repeats every `show_after_ms` until the queue drains.
    pub(crate) fn step(
        &mut self,
        oldest: Option<Instant>,
        waiting: usize,
        now: Instant,
        settings: PathCheckSettings,
    ) -> (Option<PathCheckInput>, Option<Instant>) {
        let show_after = Duration::from_millis(settings.show_after_ms);
        let queued = waiting as u64;
        if self.recheck.is_none_or(|recheck| now >= recheck) {
            self.recheck = (waiting > 0).then_some(now + show_after);
        }
        if let Some(started) = oldest {
            // When: oldest is Some, a call is running; its age decides the display.
            let age = now.saturating_duration_since(started);
            if self.shown.is_none() && age < show_after {
                // When: shown is None and age is under show_after, the call may still end unseen.
                return (None, Some(started + show_after));
            }
            self.idle_since = None;
            let secs = age.as_secs();
            self.shown =
                Some(PathCheckInput { secs, warn: secs >= settings.warn_after_secs, queued });
            return (self.shown, Some(started + Duration::from_secs(secs + 1)));
        }
        if let Some(shown) = self.shown.as_mut().filter(|_| waiting > 0) {
            // When: shown is Some and checks still wait, the display stays with no hold running.
            shown.queued = queued;
            self.idle_since = None;
            return (self.shown, self.recheck);
        }
        if self.shown.is_some() {
            // When: shown is Some and nothing runs, the hold runs from the first idle sight.
            let idle_since = *self.idle_since.get_or_insert(now);
            let hold_end = idle_since + Duration::from_millis(settings.hold_ms);
            if now < hold_end {
                // When: now is before hold_end, the last display stays.
                return (self.shown, Some(hold_end));
            }
            self.shown = None;
            self.idle_since = None;
        }
        (None, self.recheck)
    }
}

#[cfg(test)]
#[path = "jobs_tests.rs"]
mod jobs_tests;
