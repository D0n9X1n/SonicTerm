//! Display-link pacing for deferred hardware streaming.
//!
//! A window whose streaming frame is deferred on the hardware path is admitted on a display-link
//! tick, with a two-period fallback ceiling, instead of one period after its last render. The link
//! runs only while such an admission is pending. Every other deferral keeps its timer. The source is
//! a trait so the rules run against a fake on every platform; a window with no source installed
//! keeps the timer path unchanged.

use std::{
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};

use winit::window::WindowId;

use super::{redraw::WindowRedrawState, App, WindowState};

/// A per-window frame-timing source the App starts and stops.
pub(crate) trait DisplayLinkSource {
    /// Start or pause the source's ticks.
    fn set_running(&mut self, running: bool);
}

/// How a pending admission is paced, fixed at the deferral that stored it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PacingMode {
    /// One period from the pacing clock, a surface timeout or a contention floor.
    Timer,
    /// A display-link tick, or the two-period fallback ceiling when no tick comes.
    Link,
}

/// A window's installed display-link source and the generation its native link was started for.
#[derive(Default)]
pub(crate) struct WindowDisplayLink {
    /// The installed source; `None` keeps the window on the timer path.
    pub(crate) source: Option<Box<dyn DisplayLinkSource>>,
    /// The generation the native link was started for, or `None` while it is paused.
    pub(crate) native_generation: Option<u64>,
}

impl WindowRedrawState {
    /// Store `mode` for the pending admission unless a mode is already stored; a stored mode is kept.
    pub(super) fn store_pacing(&mut self, mode: PacingMode) {
        if self.pacing.is_none() {
            self.pacing = Some(mode);
        }
    }

    /// Cancel link pacing on a pause, suppression or restart: a newer generation rejects queued
    /// ticks, readiness is dropped, and a stored `Link` is cleared; a stored `Timer` is not the link's.
    // Ordering: link_generation Release publishes the bump before the native target's Acquire read posts a tick.
    pub(super) fn invalidate_link_pacing(&mut self) {
        self.link_generation.fetch_add(1, Ordering::Release);
        self.link_live = None;
        self.link_permit = None;
        if self.pacing == Some(PacingMode::Link) {
            self.pacing = None;
        }
    }

    /// Whether an accepted tick of the running interval is still unspent.
    pub(super) fn link_permit_valid(&self) -> bool {
        self.link_permit.is_some() && self.link_permit == self.link_live
    }
}

impl WindowState {
    /// The fallback ceiling of a `Link` admission: two periods after the pacing clock.
    pub(super) fn link_ceiling(&self, period: Duration, software: bool) -> Instant {
        self.pacing_clock(software) + period * 2
    }

    /// Whether this window may run its link and accept ticks now, apart from its pending mode.
    pub(super) fn link_eligible(&self, software: bool) -> bool {
        !self.redraw.sync_hold && !software && self.frame_deadlines_allowed()
    }

    /// The Frame deadline of a deferred window: the timer rule's first admissible instant, raised to
    /// the fallback ceiling while a `Link` admission waits for a tick.
    pub(super) fn frame_deadline(&self, period: Duration, software: bool) -> Instant {
        let floor = self.redraw_not_before(period, software);
        if self.redraw.pacing == Some(PacingMode::Link)
            && !self.redraw.sync_hold
            && !self.redraw.link_permit_valid()
        {
            floor.max(self.link_ceiling(period, software))
        } else {
            // When: `pacing` is not `Link`, `sync_hold` holds, or `link_permit_valid` is true, the floor decides.
            floor
        }
    }

    /// Reconcile this window's native link with its logical pacing state. A pause stops only the
    /// generation it was started for and never writes `pacing`; a start always bumps the generation.
    // Ordering: link_generation Release bumps are published before the native target's Acquire read.
    pub(super) fn sync_display_link(&mut self, software: bool) {
        let should_run = self.display_link.source.is_some()
            && self.redraw.pacing == Some(PacingMode::Link)
            && self.link_eligible(software);
        if let Some(native) = self.display_link.native_generation {
            if !should_run || self.redraw.link_live != Some(native) {
                // Nothing link-paced is pending, or `native` belongs to an invalidated interval: pause it.
                if let Some(source) = self.display_link.source.as_mut() {
                    source.set_running(false);
                }
                self.display_link.native_generation = None;
                if self.redraw.link_live == Some(native) {
                    // The paused interval is the live one, so its readiness ends with it.
                    self.redraw.link_generation.fetch_add(1, Ordering::Release);
                    self.redraw.link_live = None;
                    self.redraw.link_permit = None;
                }
            }
        }
        if should_run && self.redraw.link_live.is_none() {
            // The generation is bumped before the link is unpaused, so every tick carries the new one.
            let generation = self.redraw.link_generation.fetch_add(1, Ordering::Release) + 1;
            self.redraw.link_live = Some(generation);
            self.display_link.native_generation = Some(generation);
            if let Some(source) = self.display_link.source.as_mut() {
                source.set_running(true);
            }
        }
    }
}

impl App {
    /// Start or pause every window's display link from its pending admission's mode.
    pub(super) fn sync_display_links(&mut self) {
        let software = self.software_render_degrade;
        for window in self.windows.values_mut() {
            window.sync_display_link(software);
        }
    }

    /// Accept a display-link tick only for the window's live generation and a pending `Link` admission:
    /// it stores a permit, counts the tick and requests one frame. Any other tick is dropped.
    pub(super) fn handle_display_link_tick(
        &mut self,
        id: WindowId,
        generation: u64,
        target: Instant,
    ) {
        let software = self.software_render_degrade;
        let now = self.dispatch_now();
        let Some(window) = self.windows.get_mut(&id) else {
            // When: `id` closed after its tick was queued, no other window may take the tick.
            tracing::trace!(
                ?id,
                generation,
                ?target,
                ?now,
                "display-link tick for a closed window"
            );
            return;
        };
        if window.redraw.link_live != Some(generation)
            || window.redraw.pacing != Some(PacingMode::Link)
            || !window.link_eligible(software)
        {
            // When: the tick's interval is stale, nothing link-paced is pending, or the window is held back.
            tracing::trace!(?id, generation, ?target, ?now, "display-link tick rejected");
            return;
        }
        window.redraw.link_permit = Some(generation);
        if let Some(counters) = window.redraw.frame_counters.as_deref_mut() {
            // the App's gate is on, each accepted tick is counted.
            counters.display_link_ticks += 1;
        }
        if !window.redraw.request_in_flight {
            window.redraw.request_in_flight = true;
            window.request_window_redraw();
        }
    }

    /// Set the software-render degrade flag; turning it on invalidates every window's link pacing,
    /// because the software path never runs a link.
    pub(super) fn set_software_render_degrade(&mut self, degrade: bool) {
        let turning_on = degrade && !self.software_render_degrade;
        self.software_render_degrade = degrade;
        if turning_on {
            for window in self.windows.values_mut() {
                window.redraw.invalidate_link_pacing();
            }
        }
    }
}

/// The shared generation a fresh window's link starts from.
pub(super) fn new_link_generation() -> Arc<std::sync::atomic::AtomicU64> {
    Arc::new(std::sync::atomic::AtomicU64::new(0))
}

/// A recording fake for tests: what it was told, in order.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct FakeLinkLog {
    /// Whether the fake is running now.
    pub(crate) running: bool,
    /// Every `set_running` argument, in order.
    pub(crate) calls: Vec<bool>,
}

/// A fake display-link source that records into a shared log.
#[cfg(test)]
pub(crate) struct FakeLink(pub(crate) std::rc::Rc<std::cell::RefCell<FakeLinkLog>>);

#[cfg(test)]
impl DisplayLinkSource for FakeLink {
    fn set_running(&mut self, running: bool) {
        let mut log = self.0.borrow_mut();
        log.running = running;
        log.calls.push(running);
    }
}

#[cfg(test)]
#[path = "display_link_tests.rs"]
mod display_link_tests;
