//! Scrollback viewport anchoring across history eviction.
//!
//! A scrolled-back pane names its top row by absolute scrollback index. Once
//! history is full, every new output line evicts the oldest row, so the same
//! index names the next row and a reader's view would creep forward one line
//! per output line. [`ViewportAnchor`] records the eviction count each pinned
//! row was measured against and subtracts later evictions, so the same text
//! stays at the top of the view while a command keeps printing.
//!
//! `PaneState::viewport_top_abs` stays the public compatibility projection.
//! Frames, hit-testing, scrollbar navigation, copy mode, and search resolve
//! through the anchor; an external assignment that differs from the last
//! projection is adopted as a new pin when the anchor next resolves it, at the
//! eviction baseline seen then.

use std::collections::HashMap;

use sonicterm_gpu::core::GpuRenderer;
use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::Parser;

use super::PaneState;

/// Which screen a viewport row was measured on, and that screen's incarnation.
///
/// `epoch` is the grid's screen epoch, which advances on every primary and
/// alternate transition. Primary history survives an alternate round trip, so
/// only the alternate flag and a backwards counter change row identity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ScreenIdentity {
    /// Whether the alternate screen was active.
    is_alt: bool,
    /// The grid's screen epoch at measurement.
    epoch: u64,
}

/// History identity captured under one parser lock.
///
/// A writer that computes a new viewport from a grid snapshot passes the
/// baseline taken from that same snapshot, so rows the VT thread evicts after
/// the lock is released are subtracted on the next reconciliation instead of
/// being folded into the new pin.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ViewportBaseline {
    /// Screen the snapshot was taken on.
    screen: ScreenIdentity,
    /// Rows dropped from scrollback over the grid's lifetime.
    evicted: u64,
    /// Scrollback rows retained, which is also the live tail's absolute top.
    live_top: u64,
}

impl ViewportBaseline {
    /// Capture `grid`'s screen and eviction identity.
    pub(crate) fn of(grid: &Grid) -> Self {
        Self {
            screen: ScreenIdentity { is_alt: grid.is_alt(), epoch: grid.screen_epoch() },
            evicted: grid.scrollback_evicted(),
            live_top: grid.scrollback_len() as u64,
        }
    }
}

/// A pinned absolute row and the eviction count it is valid against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pin {
    /// Absolute scrollback row shown at the top of the viewport.
    row: u64,
    /// `Grid::scrollback_evicted` when `row` was last resolved.
    evicted: u64,
}

/// How a pane's viewport chooses its top row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum AnchorMode {
    /// Show the live screen and keep following new output.
    #[default]
    FollowTail,
    /// Keep one history row at the top of the view.
    Pinned(Pin),
}

impl AnchorMode {
    /// Pin `top` against `evicted`, or follow the tail when `top` is `None`.
    fn at(top: Option<u64>, evicted: u64) -> Self {
        top.map_or(Self::FollowTail, |row| Self::Pinned(Pin { row, evicted }))
    }

    /// The pinned row, if any.
    fn pin(self) -> Option<Pin> {
        match self {
            Self::FollowTail => None,
            Self::Pinned(pin) => Some(pin),
        }
    }

    /// The compatibility projection written to `viewport_top_abs`.
    fn projection(self) -> Option<u64> {
        self.pin().map(|pin| pin.row)
    }
}

/// Private pane-local viewport anchor behind `PaneState::viewport_top_abs`.
///
/// Holds the active screen's mode, the screen it was last resolved on, a
/// primary-screen pin suspended while the alternate screen is active, and the
/// last projection written to the public field so an external assignment can
/// be told apart from the anchor's own rebase.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ViewportAnchor {
    /// The active screen's viewport policy.
    mode: AnchorMode,
    /// Screen `mode` was last resolved against; `None` before first use.
    screen: Option<ScreenIdentity>,
    /// Primary-screen pin saved while the alternate screen is active.
    suspended_primary: Option<Pin>,
    /// The value this anchor last wrote to `viewport_top_abs`.
    projection: Option<u64>,
}

impl ViewportAnchor {
    /// Rebase against `now`, write the projection into `field`, and return it.
    ///
    /// A `field` value that differs from the last projection was written by
    /// legacy code: it is adopted against `now` before any rebase, `None` as
    /// follow-the-tail and `Some(row)` as a pin at the current eviction count.
    /// Reconciling twice against the same baseline changes nothing.
    pub(crate) fn reconcile(
        &mut self,
        field: &mut Option<u64>,
        now: ViewportBaseline,
    ) -> Option<u64> {
        self.follow_screen(now);
        if *field != self.projection {
            // An external writer assigned `field`; pin that request at the current eviction
            // count instead of rebasing it.
            self.mode = AnchorMode::at(*field, now.evicted);
        }
        self.rebase(now);
        self.projection = self.mode.projection();
        *field = self.projection;
        self.projection
    }

    /// What [`Self::reconcile`] would return, without committing it.
    pub(crate) fn resolve(&self, mut field: Option<u64>, now: ViewportBaseline) -> Option<u64> {
        let mut preview = *self;
        preview.reconcile(&mut field, now)
    }

    /// Repin to `top`, or follow the tail for `None`, measured against `at`.
    ///
    /// Always repins, even when `top` equals the current projection, which a
    /// plain field assignment cannot express. A pin on the primary screen
    /// replaces any suspended primary pin; one on the alternate screen leaves
    /// the suspended pin for the return to primary.
    pub(crate) fn set(&mut self, field: &mut Option<u64>, top: Option<u64>, at: ViewportBaseline) {
        self.follow_screen(at);
        self.mode = AnchorMode::at(top, at.evicted);
        if !at.screen.is_alt {
            self.suspended_primary = None;
        }
        self.projection = self.mode.projection();
        *field = self.projection;
    }

    /// Follow the live tail on every screen; report whether the view moved.
    ///
    /// Submitting input returns the whole pane to its live output, so a
    /// primary pin suspended behind the alternate screen is dropped as well.
    pub(crate) fn release(&mut self, field: &mut Option<u64>) -> bool {
        let moved = field.is_some();
        self.mode = AnchorMode::FollowTail;
        self.suspended_primary = None;
        self.projection = None;
        *field = None;
        moved
    }

    /// Carry the anchor across any screen change since it was last resolved.
    fn follow_screen(&mut self, now: ViewportBaseline) {
        let Some(previous) = self.screen.replace(now.screen) else {
            // When: `previous` is absent, the anchor was never resolved and belongs to `now`.
            return;
        };
        if self.identity_regressed(previous, now) {
            // When: `identity_regressed` finds a counter moved backwards, the grid was
            // replaced and no recorded row can be rebased onto it.
            self.mode = AnchorMode::FollowTail;
            self.suspended_primary = None;
            return;
        }
        let (was_alt, is_alt) = (previous.is_alt, now.screen.is_alt);
        if !was_alt && is_alt {
            // The alternate screen opened: save the primary pin, and follow the alternate
            // screen's own tail, since it keeps no history.
            self.suspended_primary = self.mode.pin();
            self.mode = AnchorMode::FollowTail;
        } else if was_alt && !is_alt {
            // When: `was_alt` ended on the primary screen, resume the suspended pin, which
            // `rebase` then shifts by the rows evicted meanwhile.
            let resumed = self.suspended_primary.take();
            self.mode = resumed.map_or(AnchorMode::FollowTail, AnchorMode::Pinned);
        } else if is_alt && previous.epoch != now.screen.epoch {
            // When: `is_alt` holds across a changed `epoch`, a different alternate screen
            // replaced the one this pin was measured on.
            self.mode = AnchorMode::FollowTail;
        }
    }

    /// Whether a counter moved backwards, which a continuous grid never does.
    fn identity_regressed(&self, previous: ScreenIdentity, now: ViewportBaseline) -> bool {
        now.screen.epoch < previous.epoch
            || [self.mode.pin(), self.suspended_primary]
                .into_iter()
                .flatten()
                .any(|pin| pin.evicted > now.evicted)
    }

    /// Shift a primary pin by the rows evicted since it was last resolved.
    ///
    /// A surviving row keeps its text; an evicted one clamps to the oldest
    /// retained row, or follows the tail when no history remains.
    fn rebase(&mut self, now: ViewportBaseline) {
        let AnchorMode::Pinned(pin) = self.mode else {
            // When: `mode` follows the tail, there is no pinned row to rebase.
            return;
        };
        if now.screen.is_alt {
            // When: `is_alt`, the eviction count only tracks primary history, so an alternate
            // screen pin keeps its row and readers clamp it to the live screen.
            return;
        }
        // `follow_screen` resets a pin whose count exceeds `now`, so this never saturates.
        let delta = now.evicted.saturating_sub(pin.evicted);
        self.mode = if pin.row >= delta {
            AnchorMode::Pinned(Pin { row: pin.row - delta, evicted: now.evicted })
        } else if now.live_top > 0 {
            // When: the pinned row was evicted but `live_top` shows history remains, clamp to
            // the oldest retained row.
            AnchorMode::Pinned(Pin { row: 0, evicted: now.evicted })
        } else {
            // When: the pinned row was evicted and no history remains, the view follows the
            // live tail again.
            AnchorMode::FollowTail
        };
    }
}

impl PaneState {
    /// Repin this pane's viewport so absolute history row `top` is at its top.
    ///
    /// `None` follows the live tail. Unlike assigning `viewport_top_abs`, this
    /// repins even when `top` equals the current value, measuring the row
    /// against the grid's current eviction count. Takes the parser lock, so a
    /// caller that already holds it must not call this.
    pub fn pin_viewport_top(&mut self, top: Option<u64>) {
        let at = ViewportBaseline::of(self.parser.lock().grid());
        self.set_viewport_top_at(at, top);
    }

    /// Repin to `top` against a baseline captured under the lock that chose it.
    pub(crate) fn set_viewport_top_at(&mut self, at: ViewportBaseline, top: Option<u64>) {
        self.viewport_anchor.set(&mut self.viewport_top_abs, top, at);
    }

    /// Rebase the anchor against `grid` and commit the projection.
    pub(crate) fn reconcile_viewport(&mut self, grid: &Grid) -> Option<u64> {
        self.viewport_anchor.reconcile(&mut self.viewport_top_abs, ViewportBaseline::of(grid))
    }

    /// The anchored viewport top against `grid`, without committing it.
    pub(crate) fn resolved_viewport(&self, grid: &Grid) -> Option<u64> {
        self.viewport_anchor.resolve(self.viewport_top_abs, ViewportBaseline::of(grid))
    }

    /// The absolute row at the top of the view, clamped to the live screen.
    pub(crate) fn resolved_view_top(&self, grid: &Grid) -> u64 {
        GpuRenderer::resolved_view_top_abs_legacy(grid, self.resolved_viewport(grid))
    }

    /// Follow the live tail on every screen; report whether the view moved.
    pub(crate) fn release_viewport(&mut self) -> bool {
        self.viewport_anchor.release(&mut self.viewport_top_abs)
    }
}

/// Viewport projections one frame presents, from a single reconcile pass.
///
/// Both render collectors read every pane's `PaneRender` viewport and the
/// active window's frame viewport from this value, so the two cannot diverge.
#[derive(Debug, Clone, Default)]
pub(crate) struct FrameViewports {
    /// Rebased `viewport_top_abs` of each held pane.
    pub(crate) per_pane: HashMap<u64, Option<u64>>,
    /// Rebased `viewport_top_abs` of the active pane, for the window's frame facts.
    pub(crate) active: Option<u64>,
}

impl FrameViewports {
    /// The projection for `pane_id`; a pane the frame did not hold follows the tail.
    pub(crate) fn of(&self, pane_id: u64) -> Option<u64> {
        self.per_pane.get(&pane_id).copied().flatten()
    }
}

/// Rebase every held pane's viewport against the parser snapshot a frame
/// presents, and return the projections that frame reads.
///
/// Both render collectors consume the returned value, so each pane's
/// `PaneRender` viewport and the active window's frame viewport come from one
/// reconcile pass.
pub(crate) fn reconcile_held_viewports<'a>(
    panes: &mut HashMap<u64, PaneState>,
    held: impl IntoIterator<Item = (u64, &'a Parser)>,
    active_pane: u64,
) -> FrameViewports {
    let mut frame = FrameViewports::default();
    for (pane_id, parser) in held {
        if let Some(pane) = panes.get_mut(&pane_id) {
            frame.per_pane.insert(pane_id, pane.reconcile_viewport(parser.grid()));
        }
    }
    frame.active = frame.of(active_pane);
    frame
}

#[cfg(test)]
#[path = "viewport_anchor_tests.rs"]
mod viewport_anchor_tests;
