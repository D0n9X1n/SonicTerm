//! Holding a window's frames while a visible pane's synchronized update (DEC 2026) is open.
//!
//! A pane holds its window while it is visible, its published update is open, its deadline has
//! not passed, and every reset it has published has reached a successful frame. The window holds
//! for at most `SYNC_OUTPUT_TIMEOUT` from the first `Sync` deferral of a stretch, whatever pane
//! caused it. A frame that must run (first frame, visibility, device recovery, resize, surface
//! recovery) ignores the hold and presents the live grid.

use std::time::Instant;

use sonicterm_vt::vt::SyncState;
use winit::window::WindowId;

use super::frame_counters::DeferRule;
use super::redraw::RedrawCause;
use super::spawn_pane::{
    published_resets, read_published_sync, SYNC_EPOCH_MASK, SYNC_OUTPUT_TIMEOUT,
};
use super::{App, PaneState, WindowState};

impl PaneState {
    /// Until when this pane's published update holds its window at `now`, read without the lock.
    ///
    /// A deadline the word's epoch has not published yet holds until the window cap; the
    /// recheck under the parser guard then decides.
    pub(super) fn sync_hold_until(&self, now: Instant) -> Option<Instant> {
        let published = read_published_sync(&self.sync_word, &self.sync_deadline_word);
        if !published.set || published.resets != published_resets(self.presented_sync_resets) {
            // When: the update is closed, or one of its resets has not reached a frame, the pane releases.
            return None;
        }
        match published.deadline {
            Some(deadline) => (now < deadline).then_some(deadline),
            None => Some(now + SYNC_OUTPUT_TIMEOUT),
        }
    }
}

impl WindowState {
    /// The panes this window shows: the active tab's leaves, or its zoomed pane.
    fn visible_pane_ids(&self) -> Vec<u64> {
        self.tab_states.get(self.tabs.active_index()).map_or_else(Vec::new, |tab| {
            tab.tree.zoomed_pane_id().map_or_else(|| tab.tree.leaves(), |id| vec![id])
        })
    }

    /// Whether this frame must run whatever the hold: first frame, visibility, device recovery,
    /// resize or surface recovery. A cleared retained key alone does not force a frame.
    pub(super) fn sync_frame_forced(&self) -> bool {
        self.sync_forced_cause().is_some()
    }

    /// The first reason this frame must run whatever the hold, in the order `sync_frame_forced`
    /// lists them; `None` when nothing forces it.
    pub(super) fn sync_forced_cause(&self) -> Option<ForcedCause> {
        [
            (self.redraw.last_present.is_none(), ForcedCause::First),
            (self.redraw.cause_pending(RedrawCause::Visibility), ForcedCause::Visibility),
            (self.redraw.cause_pending(RedrawCause::DeviceRecovered), ForcedCause::Device),
            (self.redraw.resize_pending, ForcedCause::Resize),
            (self.redraw.surface_recovery_pending, ForcedCause::Surface),
        ]
        .into_iter()
        .find_map(|(pending, cause)| pending.then_some(cause))
    }

    /// Why an attempt that proceeded past the guarded recheck at `now` was admitted for pane
    /// `pane_id`, whose guarded state is `state`: `Closed` when its update is closed, otherwise the
    /// reason an open update did not hold it. `None` when the window holds no such pane.
    pub(super) fn sync_admission(
        &self,
        pane_id: u64,
        state: SyncState,
        now: Instant,
    ) -> Option<SyncAdmission> {
        let pane = self.panes.get(&pane_id)?;
        if !state.set {
            // When: `state.set` is false, the guarded update is closed and the frame shows a finished one.
            return Some(SyncAdmission::Closed);
        }
        if let Some(cause) = self.sync_forced_cause() {
            // When: `sync_forced_cause` names a `cause`, that must-run cause showed the open update.
            return Some(SyncAdmission::Forced(cause));
        }
        if !self.sync_cap_open(now) {
            // When: `sync_cap_open` is false at `now`, the stretch reached its cap and the window stopped holding.
            return Some(SyncAdmission::Timeout);
        }
        if published_resets(state.resets) != published_resets(pane.presented_sync_resets) {
            // When: `published_resets` of `state` and `presented_sync_resets` differ, an unpresented reset's credit admits it.
            return Some(SyncAdmission::Credit);
        }
        // An open update that neither a forced cause, the cap nor a credit released has passed its deadline.
        Some(SyncAdmission::Timeout)
    }

    /// The earliest instant a visible pane stops holding this window, or `None` when none holds.
    fn sync_hold_until(&self, now: Instant) -> Option<Instant> {
        self.visible_pane_ids()
            .iter()
            .filter_map(|id| self.panes.get(id))
            .filter_map(|pane| pane.sync_hold_until(now))
            .min()
    }

    /// Whether the window's hold may still run at `now`: within the cap of the current stretch.
    fn sync_cap_open(&self, now: Instant) -> bool {
        self.redraw.sync_stretch_start.is_none_or(|start| now < start + SYNC_OUTPUT_TIMEOUT)
    }

    /// The Sync deferral predicate at admission: not forced, within the cap, and a visible pane holds.
    pub(super) fn sync_defers(&self, now: Instant) -> bool {
        !self.sync_frame_forced() && self.sync_cap_open(now) && self.sync_hold_until(now).is_some()
    }

    /// Note a Sync deferral at `now`; the first one of a stretch starts its cap.
    pub(super) fn note_sync_deferral(&mut self, now: Instant) {
        self.redraw.sync_stretch_start.get_or_insert(now);
    }

    /// When a Sync-deferred window wakes: the earliest held deadline or the cap, whichever is first.
    pub(super) fn sync_wake_at(&self, now: Instant) -> Instant {
        let cap = self.redraw.sync_stretch_start.map(|start| start + SYNC_OUTPUT_TIMEOUT);
        match (self.sync_hold_until(now), cap) {
            (Some(hold), Some(cap)) => hold.min(cap),
            (Some(hold), None) => hold,
            (None, Some(cap)) => cap.min(now),
            (None, None) => now,
        }
    }

    /// Record each visible pane's published reset count for the admitted attempt.
    // Ordering: sync_resets loads Relaxed; the credit only compares with a later word's count.
    pub(super) fn record_attempt_sync_resets(&mut self) {
        let resets = self
            .visible_pane_ids()
            .into_iter()
            .filter_map(|id| {
                let pane = self.panes.get(&id)?;
                Some((id, pane.sync_resets.load(std::sync::atomic::Ordering::Relaxed)))
            })
            .collect();
        self.redraw.attempt_sync_resets = resets;
    }

    /// Spend the attempt's reset credits on a successful outcome; a failure discards them.
    pub(super) fn credit_sync_resets(&mut self, success: bool) {
        let credits = std::mem::take(&mut self.redraw.attempt_sync_resets);
        if !success {
            // When: `success` is false, nothing reached the screen, so unpresented resets keep releasing.
            return;
        }
        for (id, resets) in credits {
            if let Some(pane) = self.panes.get_mut(&id) {
                pane.presented_sync_resets = resets;
            }
        }
    }
}

/// A must-run cause that admits a frame through a hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ForcedCause {
    /// The window has not presented yet.
    First,
    /// The window became visible.
    Visibility,
    /// The device recovered.
    Device,
    /// The surface changed size.
    Resize,
    /// The surface is being recovered.
    Surface,
}

/// Why a frame was admitted while a pane's guarded state was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SyncAdmission {
    /// The pane's update was closed.
    Closed,
    /// An open update was shown because a must-run cause forced the frame.
    Forced(ForcedCause),
    /// An open update was shown because its deadline or the window's cap passed.
    Timeout,
    /// An open update was shown because a reset of the pane had not reached a frame.
    Credit,
}

impl SyncAdmission {
    /// The admission's name on a `sonic::perf_present` line.
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Forced(ForcedCause::First) => "forced-first",
            Self::Forced(ForcedCause::Visibility) => "forced-visibility",
            Self::Forced(ForcedCause::Device) => "forced-device",
            Self::Forced(ForcedCause::Resize) => "forced-resize",
            Self::Forced(ForcedCause::Surface) => "forced-surface",
            Self::Timeout => "timeout",
            Self::Credit => "credit",
        }
    }
}

/// Whether one guarded pane still holds at `now`: open, every reset presented, deadline ahead.
fn guarded_sync_holds(pane: &PaneState, state: SyncState, now: Instant) -> bool {
    if !state.set || published_resets(state.resets) != published_resets(pane.presented_sync_resets)
    {
        // When: the guarded update is closed or has an unpresented reset, the pane releases.
        return false;
    }
    let published = read_published_sync(&pane.sync_word, &pane.sync_deadline_word);
    match published.deadline {
        Some(deadline) if published.epoch == state.epoch & SYNC_EPOCH_MASK => now < deadline,
        // The guard's epoch has no deadline published yet, so the window cap bounds it.
        _ => true,
    }
}

impl App {
    /// The recheck under the collected guards: abandon a frame a visible pane now holds.
    ///
    /// `states` is each guarded pane's `synchronized_output()`. An abandoned frame settles nothing:
    /// clocks, the retry floor, receipts and pending causes are untouched, and it counts `defer_sync`.
    /// A frame that proceeds records the guarded reset counts as its credits.
    pub(super) fn abandon_synchronized_frame(
        &mut self,
        id: WindowId,
        states: &[(u64, SyncState)],
        now: Instant,
    ) -> bool {
        let Some(window) = self.windows.get_mut(&id) else {
            // When: `id` closed during collection, there is no frame to hold.
            return false;
        };
        let held = !window.sync_frame_forced()
            && window.sync_cap_open(now)
            && states.iter().any(|(pane_id, state)| {
                window.panes.get(pane_id).is_some_and(|pane| guarded_sync_holds(pane, *state, now))
            });
        if !held {
            // When: `held` is false, the frame proceeds and credits the resets read under the guards.
            window.redraw.attempt_sync_resets =
                states.iter().map(|(pane_id, state)| (*pane_id, state.resets)).collect();
            return false;
        }
        window.redraw.attempt_causes = None;
        window.redraw.deferred = true;
        window.redraw.deferred_rule = Some(DeferRule::Sync);
        window.redraw.request_in_flight = false;
        window.note_sync_deferral(now);
        if let Some(counters) = window.redraw.frame_counters.as_deref_mut() {
            counters.note_defer(DeferRule::Sync);
        }
        if self.main_window_id == Some(id) {
            self.pending_redraw = true;
        } else {
            // When: `id` is a child, its deferral joins the child wake set.
            self.pending_redraw_windows.insert(id);
        }
        true
    }
}

#[cfg(test)]
#[path = "sync_frame_tests.rs"]
mod sync_frame_tests;
