//! Per-pane scrollbar auto-hide + fade animation.
//!
//! Pure helpers + a small `ScrollbarVisState` struct that the
//! window-event + render plumbing in `app::` mutates. Kept as
//! standalone functions so the test suite can exercise them without a
//! winit window or wgpu surface.
//!
//! Semantics (Auto mode):
//! - Scrollbar is hidden by default (alpha 0).
//! - Interaction targets alpha 1 while edge hover, drag, or recent activity holds.
//! - [`ScrollbarMotion::Animated`] uses 150 ms fade-in and 300 ms fade-out frames.
//! - [`ScrollbarMotion::Snap`] assigns targets immediately and uses one idle deadline.
//!
//! Always / Never short-circuit to alpha 1.0 / 0.0 with no animation.

use sonicterm_cfg::config::ScrollbarMode;
use std::time::{Duration, Instant};

pub use sonicterm_ui::scrollbar::ALPHA_EMIT_FLOOR;

/// Logical-pixel distance from the pane's right edge that counts as
/// "hovering the scrollbar gutter" and shows the bar.
pub const EDGE_PROXIMITY_PX: f32 = 20.0;

/// Idle duration after the last scroll / drag / hover before the bar
/// begins fading out.
pub const IDLE_HIDE_MS: u64 = 600;

/// Fade-in duration (faster — affordance must appear promptly).
pub const FADE_IN_MS: u64 = 150;

/// Fade-out duration (slower — gentle dismissal).
pub const FADE_OUT_MS: u64 = 300;

/// Whether scrollbar opacity advances through fade frames or reaches its target immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollbarMotion {
    /// Advance opacity over the configured fade duration.
    Animated,
    /// Assign target opacity immediately and rely on one idle-hide deadline.
    Snap,
}

/// Resolve a window's scrollbar motion from renderer state, falling back only before attachment.
pub(crate) fn window_scrollbar_motion(
    renderer_degraded: Option<bool>,
    app_degraded: bool,
) -> ScrollbarMotion {
    if renderer_degraded.unwrap_or(app_degraded) {
        ScrollbarMotion::Snap
    } else {
        // When: renderer_degraded or app_degraded resolves false, preserve accelerated animation.
        ScrollbarMotion::Animated
    }
}

/// Per-pane visibility state. Constructed lazily on first use; lives
/// inside `WindowState.scrollbar_vis` keyed by `pane_id`.
#[derive(Debug, Clone, Copy)]
pub struct ScrollbarVisState {
    /// Current rendered alpha in `[0.0, 1.0]`. Lerped toward the target
    /// each frame by [`tick`].
    pub alpha: f32,
    /// Instant of the most recent "I'm relevant" event (scroll, drag,
    /// edge-hover entry, view-change), or `None` when the pane has never
    /// been active. `None` reads as "infinitely idle" → hidden. Modeled as
    /// an `Option` rather than a far-past `Instant` because
    /// `Instant::checked_sub(3600s)` returns `None` on a freshly-booted
    /// machine (monotonic clock younger than the offset), which silently
    /// made the bar start VISIBLE — a real defect caught by CI on fresh
    /// Windows runners.
    pub last_active: Option<Instant>,
    /// Sticky bit: cursor is currently inside the right-edge proximity
    /// strip. When `true` we override the idle-hide timer.
    pub mouse_near_right_edge: bool,
    /// Last [`tick`] instant. Drives animated steps and records the latest snap.
    pub last_tick: Instant,
}

impl ScrollbarVisState {
    /// Construct an initially-hidden state. `last_active` is `None` so the
    /// pane reads as fully idle (bar hidden) until the first real activity.
    pub fn new(now: Instant) -> Self {
        Self { alpha: 0.0, last_active: None, mouse_near_right_edge: false, last_tick: now }
    }

    /// Record a "user is interacting with this pane's scroll" event
    /// (scrollwheel, drag, view_top jump). Resets the idle-hide window.
    pub fn mark_active(&mut self, now: Instant) {
        self.last_active = Some(now);
    }
}

/// Logical-px proximity check. Returns `true` when `cursor` is inside
/// the pane vertically AND within `EDGE_PROXIMITY_PX` of the right
/// edge horizontally.
pub fn is_mouse_near_right_edge(
    pane_x: f32,
    pane_y: f32,
    pane_w: f32,
    pane_h: f32,
    cursor_x: f32,
    cursor_y: f32,
) -> bool {
    if cursor_y < pane_y || cursor_y > pane_y + pane_h {
        // When: cursor_y sits outside the pane band the horizontal edge test is
        // skipped, so a neighbouring pane's gutter cannot claim the hover.
        return false;
    }
    let right = pane_x + pane_w;
    cursor_x >= right - EDGE_PROXIMITY_PX && cursor_x <= right + EDGE_PROXIMITY_PX.min(8.0)
}

fn auto_target(state: &ScrollbarVisState, drag_active: bool, now: Instant) -> f32 {
    let recently_active = state.last_active.is_some_and(|active| {
        now.saturating_duration_since(active).as_millis() < u128::from(IDLE_HIDE_MS)
    });
    if drag_active || state.mouse_near_right_edge || recently_active {
        1.0
    } else {
        // When: drag_active, mouse_near_right_edge, and recently_active are false, Auto targets hidden.
        0.0
    }
}

/// Step the scrollbar opacity according to the selected motion policy.
pub fn tick(
    state: &mut ScrollbarVisState,
    mode: ScrollbarMode,
    drag_active: bool,
    motion: ScrollbarMotion,
    now: Instant,
) -> f32 {
    let target = match mode {
        ScrollbarMode::Always => 1.0,
        ScrollbarMode::Never => 0.0,
        ScrollbarMode::Auto => auto_target(state, drag_active, now),
    };
    if !matches!(mode, ScrollbarMode::Auto) || matches!(motion, ScrollbarMotion::Snap) {
        // When: `matches` selects fixed mode or Snap motion, assign the target without a fade frame.
        state.alpha = target;
        state.last_tick = now;
        return target;
    }

    let dt_ms = now.saturating_duration_since(state.last_tick).as_millis().max(1) as f32;
    let duration_ms = if target > state.alpha {
        FADE_IN_MS as f32
    } else {
        // When: `target` is at or below `state.alpha`, use the gentler fade-out duration.
        FADE_OUT_MS as f32
    };
    let step = dt_ms / duration_ms;
    let delta = target - state.alpha;
    if delta.abs() <= step {
        state.alpha = target;
    } else {
        // When: delta remains larger than step, advance once so a later frame can finish the fade.
        state.alpha += step.copysign(delta);
    }
    state.alpha = state.alpha.clamp(0.0, 1.0);
    state.last_tick = now;
    state.alpha
}

/// Whether another opacity frame is required at the supplied instant.
pub fn is_animating(
    state: &ScrollbarVisState,
    mode: ScrollbarMode,
    drag_active: bool,
    motion: ScrollbarMotion,
    now: Instant,
) -> bool {
    if !matches!(mode, ScrollbarMode::Auto) || matches!(motion, ScrollbarMotion::Snap) {
        // When: `matches` selects fixed mode or Snap motion, no intermediate opacity remains.
        return false;
    }
    let target = auto_target(state, drag_active, now);
    (state.alpha - target).abs() > f32::EPSILON
        || state.last_active.is_some_and(|active| {
            now.saturating_duration_since(active).as_millis() < u128::from(IDLE_HIDE_MS)
        })
}

/// Earliest idle-hide deadline held by a visible snapped scrollbar.
pub fn next_snap_deadline(
    vis: &std::collections::HashMap<u64, ScrollbarVisState>,
    mode: ScrollbarMode,
    drag_active_on_pane: Option<u64>,
) -> Option<Instant> {
    if !matches!(mode, ScrollbarMode::Auto) {
        // When: `matches` rejects Auto mode, no idle transition or hide wake exists.
        return None;
    }
    vis.iter()
        .filter(|(id, state)| {
            state.alpha > ALPHA_EMIT_FLOOR
                && !state.mouse_near_right_edge
                && drag_active_on_pane != Some(**id)
        })
        .filter_map(|(_, state)| {
            state.last_active.map(|active| active + Duration::from_millis(IDLE_HIDE_MS))
        })
        .min()
}

/// Expire due snapped scrollbars and report whether visible opacity changed.
pub fn expire_due_snaps(
    vis: &mut std::collections::HashMap<u64, ScrollbarVisState>,
    mode: ScrollbarMode,
    drag_active_on_pane: Option<u64>,
    now: Instant,
) -> bool {
    if !matches!(mode, ScrollbarMode::Auto) {
        // When: `matches` rejects Auto mode, expiration must preserve fixed visibility.
        return false;
    }
    let mut changed = false;
    for (id, state) in vis {
        if state.mouse_near_right_edge || drag_active_on_pane == Some(*id) {
            // When: mouse_near_right_edge is true or drag_active_on_pane matches id, visibility still holds.
            continue;
        }
        let due = state
            .last_active
            .is_some_and(|active| active + Duration::from_millis(IDLE_HIDE_MS) <= now);
        if due && state.alpha > ALPHA_EMIT_FLOOR {
            state.alpha = 0.0;
            state.last_active = None;
            state.last_tick = now;
            changed = true;
        }
    }
    changed
}

/// One-shot helper used at the top of the render path: for the given
/// pane list (id + logical rect), update each pane's
/// `mouse_near_right_edge` from the current cursor, tick the alpha,
/// and return a map of `(pane_id -> alpha)` for `PaneRender`. Closed
/// panes are pruned from `vis` in-place.
#[allow(clippy::too_many_arguments)]
pub fn update_and_collect(
    vis: &mut std::collections::HashMap<u64, ScrollbarVisState>,
    panes: &[(u64, f32, f32, f32, f32)],
    cursor: (f32, f32),
    active_id: u64,
    drag_active_on_pane: Option<u64>,
    mode: ScrollbarMode,
    motion: ScrollbarMotion,
    now: Instant,
) -> std::collections::HashMap<u64, f32> {
    let live_ids: std::collections::HashSet<u64> = panes.iter().map(|(id, ..)| *id).collect();
    vis.retain(|id, _| live_ids.contains(id));

    let mut out = std::collections::HashMap::with_capacity(panes.len());
    for &(id, px, py, pw, ph) in panes {
        let state = vis.entry(id).or_insert_with(|| ScrollbarVisState::new(now));
        let near = is_mouse_near_right_edge(px, py, pw, ph, cursor.0, cursor.1);
        if near && !state.mouse_near_right_edge {
            state.last_active = Some(now);
        }
        state.mouse_near_right_edge = near;
        let drag = drag_active_on_pane == Some(id) && id == active_id;
        let alpha = tick(state, mode, drag, motion, now);
        out.insert(id, alpha);
    }
    out
}

/// Update only the right-edge hover flags from a cursor move. Returns
/// `true` if any pane crossed the proximity threshold and therefore needs
/// a redraw to start the Auto-mode fade immediately.
pub fn update_hover_states(
    vis: &mut std::collections::HashMap<u64, ScrollbarVisState>,
    panes: &[(u64, f32, f32, f32, f32)],
    cursor: (f32, f32),
    now: Instant,
) -> bool {
    let live_ids: std::collections::HashSet<u64> = panes.iter().map(|(id, ..)| *id).collect();
    vis.retain(|id, _| live_ids.contains(id));

    let mut changed = false;
    for &(id, px, py, pw, ph) in panes {
        let state = vis.entry(id).or_insert_with(|| ScrollbarVisState::new(now));
        let near = is_mouse_near_right_edge(px, py, pw, ph, cursor.0, cursor.1);
        if state.mouse_near_right_edge != near {
            state.mouse_near_right_edge = near;
            if near {
                state.last_active = Some(now);
            }
            changed = true;
        }
    }
    changed
}

/// Clear all right-edge hover flags, e.g. when the pointer leaves a window.
/// Returns `true` when a pane crossed from near-edge to away, which should
/// schedule a redraw so Auto mode can begin the normal fade-out timing.
pub fn clear_hover_states(vis: &mut std::collections::HashMap<u64, ScrollbarVisState>) -> bool {
    let mut changed = false;
    for state in vis.values_mut() {
        if state.mouse_near_right_edge {
            state.mouse_near_right_edge = false;
            changed = true;
        }
    }
    changed
}

use super::App;

impl App {
    // Ordering: redraw_request_count uses Relaxed as a plain tally of redraw
    // requests; it guards no other data, so no happens-before edge is required.
    fn request_scrollbar_redraw(&self) {
        self.redraw_request_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(w) = self.main_window() {
            w.request_redraw();
        }
    }

    /// Refresh Auto-mode right-edge hover state from the last cursor
    /// position. Returns `true` when any pane crosses the threshold.
    pub(crate) fn refresh_scrollbar_hover_from_cursor(&mut self) -> bool {
        if !matches!(self.config.appearance.scrollbar, ScrollbarMode::Auto) {
            // When: the configured scrollbar matches Always or Never there is no
            // hover threshold to cross, so no state is touched and no redraw runs.
            return false;
        }
        let pane_rects = self.compute_active_pane_rects();
        if pane_rects.is_empty() {
            // When: pane_rects is empty the active tab has no laid-out panes, so
            // there is nothing for the cursor to be near.
            return false;
        }
        let (cx, cy) = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
        let cursor = (cx as f32, cy as f32);
        let rects: Vec<(u64, f32, f32, f32, f32)> =
            pane_rects.iter().map(|(id, r)| (*id, r.x, r.y, r.w, r.h)).collect();
        let changed = self
            .main_mut()
            .map(|ws| update_hover_states(&mut ws.scrollbar_vis, &rects, cursor, Instant::now()))
            .unwrap_or(false);
        if changed {
            self.request_scrollbar_redraw();
        }
        changed
    }

    /// Test-only shim for the CursorMoved scrollbar-hover branch. Tests set
    /// `WindowState::cursor_pos`, provide `test_viewport_override`, then call
    /// this to exercise the same production state update + redraw request.
    #[doc(hidden)]
    pub fn __test_refresh_scrollbar_hover_from_cursor(&mut self) -> bool {
        self.refresh_scrollbar_hover_from_cursor()
    }

    /// Child-window mirror of [`Self::refresh_scrollbar_hover_from_cursor`].
    /// Torn-out windows own their own `WindowState`, cursor position, pane
    /// layout, and redraw target, but the Auto-mode hover math must be shared
    /// with the main window. Returns `true` when any pane crosses the right-edge
    /// proximity threshold.
    pub(crate) fn refresh_scrollbar_hover_from_cursor_in_child(
        &mut self,
        win_id: winit::window::WindowId,
    ) -> bool {
        if !matches!(self.config.appearance.scrollbar, ScrollbarMode::Auto) {
            // When: the configured scrollbar matches Always or Never the child
            // window has no fade to drive, so its hover flags stay untouched.
            return false;
        }
        let Some(child) = self.windows.get(&win_id) else {
            // When: windows no longer holds win_id the torn-out window closed
            // before this cursor move was handled.
            return false;
        };
        let pane_rects = Self::compute_pane_rects_for(child);
        if pane_rects.is_empty() {
            // When: pane_rects is empty the child window has no laid-out panes to
            // test against its cursor position.
            return false;
        }
        let cursor = (child.cursor_pos.0 as f32, child.cursor_pos.1 as f32);
        let rects: Vec<(u64, f32, f32, f32, f32)> =
            pane_rects.iter().map(|(id, r)| (*id, r.x, r.y, r.w, r.h)).collect();
        let changed = self
            .windows
            .get_mut(&win_id)
            .map(|child| {
                update_hover_states(&mut child.scrollbar_vis, &rects, cursor, Instant::now())
            })
            .unwrap_or(false);
        if changed {
            if let Some(child) = self.windows.get(&win_id) {
                child.request_redraw();
            }
        }
        changed
    }

    pub(crate) fn clear_scrollbar_hover(&mut self) -> bool {
        let changed =
            self.main_mut().map(|ws| clear_hover_states(&mut ws.scrollbar_vis)).unwrap_or(false);
        if changed {
            self.request_scrollbar_redraw();
        }
        changed
    }

    pub(crate) fn clear_scrollbar_hover_in_child(
        &mut self,
        win_id: winit::window::WindowId,
    ) -> bool {
        let changed = self
            .windows
            .get_mut(&win_id)
            .map(|child| clear_hover_states(&mut child.scrollbar_vis))
            .unwrap_or(false);
        if changed {
            if let Some(child) = self.windows.get(&win_id) {
                child.request_redraw();
            }
        }
        changed
    }

    /// Mark a pane's scrollbar as "actively in use" so its alpha
    /// resets to fully-visible and the idle hide timer restarts.
    /// Called from the update points (scroll, drag, view_top jump).
    pub(crate) fn mark_scrollbar_active(&mut self, pane_id: u64) {
        let now = Instant::now();
        let marked = self
            .main_mut()
            .map(|ws| {
                ws.scrollbar_vis
                    .entry(pane_id)
                    .or_insert_with(|| ScrollbarVisState::new(now))
                    .mark_active(now);
                true
            })
            .unwrap_or(false);
        if marked {
            self.request_scrollbar_redraw();
        }
    }
}

#[cfg(test)]
#[path = "scrollbar_visibility_tests.rs"]
mod scrollbar_visibility_tests;
