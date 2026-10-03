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
//! - The target is stored. Every write to one of its inputs calls [`retarget`],
//!   which dates a transition only when the target changes.
//! - A bar that reached its target requests no frames. Its one wake is the idle
//!   deadline (`last_active + IDLE_HIDE_MS`), consumed when it fires.
//! - [`ScrollbarMotion::Animated`] uses 150 ms fade-in and 300 ms fade-out frames;
//!   the first step after a transition is capped at one frame period.
//! - [`ScrollbarMotion::Snap`] assigns targets immediately.
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
#[derive(Debug, Clone, Copy, PartialEq)]
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
    /// Opacity the bar is moving toward, 0 or 1. [`retarget`] writes it, and
    /// only when the effective target changes.
    pub target: f32,
    /// When `target` last changed, or `None` before its first change. A fade
    /// measures its first step from here rather than from a stale `last_tick`.
    pub transition_start: Option<Instant>,
    /// Whether the idle deadline armed by `last_active` has fired. Activity
    /// clears it, so each activity arms exactly one deadline.
    pub idle_consumed: bool,
    /// Whether no tick has run since `target` last changed. The tick that
    /// clears it caps its step at one frame period. `transition_start` alone
    /// cannot say this when a tick and a retarget share one instant.
    pub first_step_pending: bool,
}

impl ScrollbarVisState {
    /// Construct an initially-hidden state. `last_active` is `None` so the
    /// pane reads as fully idle (bar hidden) until the first real activity.
    pub fn new(now: Instant) -> Self {
        Self {
            alpha: 0.0,
            last_active: None,
            mouse_near_right_edge: false,
            last_tick: now,
            target: 0.0,
            transition_start: None,
            idle_consumed: false,
            first_step_pending: false,
        }
    }
}

/// The longest first step a fade takes after its target changes, in ms: one
/// 60 Hz frame, the period the fade durations assume. A transition serviced
/// late, or a first frame delayed by a slow present, still fades over at least
/// two frames instead of jumping to its target.
pub const FADE_FIRST_STEP_MAX_MS: f32 = 1000.0 / 60.0;

/// Record scroll, drag, view-jump or edge-hover-entry activity at `now`. It
/// restarts the idle window and re-arms its one deadline. The caller then
/// calls [`retarget`], because the effective target may have changed.
pub fn note_activity(state: &mut ScrollbarVisState, now: Instant) {
    state.last_active = Some(now);
    state.idle_consumed = false;
}

/// The opacity `mode` asks for at `now`: fixed for Always and Never; for Auto,
/// shown while dragged, edge-hovered or active within `IDLE_HIDE_MS`.
fn effective_target(
    state: &ScrollbarVisState,
    mode: ScrollbarMode,
    drag_active: bool,
    now: Instant,
) -> f32 {
    match mode {
        ScrollbarMode::Always => 1.0,
        ScrollbarMode::Never => 0.0,
        ScrollbarMode::Auto => auto_target(state, drag_active, now),
    }
}

/// Recompute the target from its inputs. Only when it changes, store it and
/// date the transition at `now`. Returns whether it changed. An event that
/// keeps the target leaves the transition alone, so a stream of activity never
/// restarts a fade.
pub fn retarget(
    state: &mut ScrollbarVisState,
    mode: ScrollbarMode,
    drag_active: bool,
    now: Instant,
) -> bool {
    let target = effective_target(state, mode, drag_active, now);
    if (target - state.target).abs() <= f32::EPSILON {
        // When: `target` matches the stored target, nothing transitions.
        return false;
    }
    state.target = target;
    state.transition_start = Some(now);
    state.first_step_pending = true;
    true
}

/// Record activity on `pane_id` and retarget it, creating its state when the
/// pane has not rendered yet.
pub fn note_pane_activity(
    vis: &mut std::collections::HashMap<u64, ScrollbarVisState>,
    pane_id: u64,
    mode: ScrollbarMode,
    drag_active_on_pane: Option<u64>,
    now: Instant,
) {
    let state = vis.entry(pane_id).or_insert_with(|| ScrollbarVisState::new(now));
    note_activity(state, now);
    retarget(state, mode, drag_active_on_pane == Some(pane_id), now);
}

/// Retarget every pane of one window, after its drag started or ended.
pub fn retarget_panes(
    vis: &mut std::collections::HashMap<u64, ScrollbarVisState>,
    mode: ScrollbarMode,
    drag_active_on_pane: Option<u64>,
    now: Instant,
) {
    for (id, state) in vis.iter_mut() {
        retarget(state, mode, drag_active_on_pane == Some(*id), now);
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
///
/// The target is retargeted first, so a frame after the idle window has
/// passed starts the fade even before the idle deadline is serviced. An
/// animated step measures time from `max(last_tick, transition_start)`, and
/// the first step after a transition is capped at [`FADE_FIRST_STEP_MAX_MS`].
pub fn tick(
    state: &mut ScrollbarVisState,
    mode: ScrollbarMode,
    drag_active: bool,
    motion: ScrollbarMotion,
    now: Instant,
) -> f32 {
    retarget(state, mode, drag_active, now);
    if !matches!(mode, ScrollbarMode::Auto) || matches!(motion, ScrollbarMotion::Snap) {
        // When: `matches` selects fixed mode or Snap motion, assign the target without a fade frame.
        state.alpha = state.target;
        state.last_tick = now;
        state.first_step_pending = false;
        return state.alpha;
    }

    let since = state.transition_start.map_or(state.last_tick, |start| start.max(state.last_tick));
    let mut dt_ms = now.saturating_duration_since(since).as_secs_f32() * 1000.0;
    if state.first_step_pending {
        // The first tick since the target changed is capped, so a late wake
        // or a slow first present cannot finish the fade in one frame.
        dt_ms = dt_ms.min(FADE_FIRST_STEP_MAX_MS);
        state.first_step_pending = false;
    }
    let dt_ms = dt_ms.max(1.0);
    let target = state.target;
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

/// Whether another opacity frame is required: only while an animated Auto
/// bar's alpha has not reached its stored target. A settled bar asks for
/// nothing; the idle deadline starts its fade.
pub fn is_animating(
    state: &ScrollbarVisState,
    mode: ScrollbarMode,
    motion: ScrollbarMotion,
) -> bool {
    if !matches!(mode, ScrollbarMode::Auto) || matches!(motion, ScrollbarMotion::Snap) {
        // When: `matches` selects fixed mode or Snap motion, no intermediate opacity remains.
        return false;
    }
    (state.alpha - state.target).abs() > f32::EPSILON
}

/// Earliest idle deadline in one window, for Snap and Fade alike. A pane
/// contributes `last_active + IDLE_HIDE_MS` only while it is shown or fading
/// in, not held by edge hover or a drag, and its deadline has not fired.
pub fn next_idle_deadline(
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
            (state.alpha > ALPHA_EMIT_FLOOR || state.target > 0.0)
                && !state.mouse_near_right_edge
                && drag_active_on_pane != Some(**id)
                && !state.idle_consumed
        })
        .filter_map(|(_, state)| {
            state.last_active.map(|active| active + Duration::from_millis(IDLE_HIDE_MS))
        })
        .min()
}

/// Service due idle deadlines: each due pane consumes its deadline and is
/// retargeted at the deadline instant. Snap hides at once; Fade keeps its
/// alpha and fades on the frames that follow. Returns whether the window
/// needs a frame: Snap when alpha changed, Fade while a due pane is still
/// short of its target. A pane whose activity, hover or drag came after the
/// deadline was collected is not due, so its expiry is a no-op.
pub fn expire_due_idle(
    vis: &mut std::collections::HashMap<u64, ScrollbarVisState>,
    mode: ScrollbarMode,
    drag_active_on_pane: Option<u64>,
    motion: ScrollbarMotion,
    now: Instant,
) -> bool {
    if !matches!(mode, ScrollbarMode::Auto) {
        // When: `matches` rejects Auto mode, expiration must preserve fixed visibility.
        return false;
    }
    let mut changed = false;
    for (id, state) in vis {
        if state.mouse_near_right_edge || drag_active_on_pane == Some(*id) || state.idle_consumed {
            // When: `mouse_near_right_edge` or `drag_active_on_pane` holds the bar, or `idle_consumed`.
            continue;
        }
        let Some(deadline) =
            state.last_active.map(|active| active + Duration::from_millis(IDLE_HIDE_MS))
        else {
            // When: `last_active` is None, the pane was never active and arms no deadline.
            continue;
        };
        if deadline > now {
            // When: `deadline` is later than `now`, fresh activity moved it; it stays armed.
            continue;
        }
        state.idle_consumed = true;
        retarget(state, mode, false, deadline);
        match motion {
            ScrollbarMotion::Snap => {
                let before = state.alpha;
                state.alpha = state.target;
                state.last_tick = now;
                state.first_step_pending = false;
                changed |= (before - state.alpha).abs() > f32::EPSILON;
            }
            ScrollbarMotion::Animated => {
                // A pane still short of its target needs a frame, whether this
                // expiry or an earlier release or frame changed the target.
                changed |= (state.alpha - state.target).abs() > f32::EPSILON;
            }
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
    for &(id, pane_x, pane_y, pane_w, pane_h) in panes {
        let state = vis.entry(id).or_insert_with(|| ScrollbarVisState::new(now));
        let near = is_mouse_near_right_edge(pane_x, pane_y, pane_w, pane_h, cursor.0, cursor.1);
        if near && !state.mouse_near_right_edge {
            note_activity(state, now);
        }
        state.mouse_near_right_edge = near;
        let drag = drag_active_on_pane == Some(id) && id == active_id;
        // The hover write above is a target input, so the target follows it now.
        retarget(state, mode, drag, now);
        let alpha = tick(state, mode, drag, motion, now);
        out.insert(id, alpha);
    }
    out
}

/// Update only the right-edge hover flags from a cursor move, retargeting
/// each pane whose flag changed. Returns `true` if any pane crossed the
/// proximity threshold and therefore needs a redraw to start its fade.
pub fn update_hover_states(
    vis: &mut std::collections::HashMap<u64, ScrollbarVisState>,
    panes: &[(u64, f32, f32, f32, f32)],
    cursor: (f32, f32),
    mode: ScrollbarMode,
    drag_active_on_pane: Option<u64>,
    now: Instant,
) -> bool {
    let live_ids: std::collections::HashSet<u64> = panes.iter().map(|(id, ..)| *id).collect();
    vis.retain(|id, _| live_ids.contains(id));

    let mut changed = false;
    for &(id, pane_x, pane_y, pane_w, pane_h) in panes {
        let state = vis.entry(id).or_insert_with(|| ScrollbarVisState::new(now));
        let near = is_mouse_near_right_edge(pane_x, pane_y, pane_w, pane_h, cursor.0, cursor.1);
        if state.mouse_near_right_edge != near {
            state.mouse_near_right_edge = near;
            if near {
                note_activity(state, now);
            }
            retarget(state, mode, drag_active_on_pane == Some(id), now);
            changed = true;
        }
    }
    changed
}

/// Clear all right-edge hover flags, e.g. when the pointer leaves a window,
/// retargeting each pane that was hovered. Returns `true` when a pane crossed
/// from near-edge to away, which should schedule a redraw for its fade.
pub fn clear_hover_states(
    vis: &mut std::collections::HashMap<u64, ScrollbarVisState>,
    mode: ScrollbarMode,
    drag_active_on_pane: Option<u64>,
    now: Instant,
) -> bool {
    let mut changed = false;
    for (id, state) in vis.iter_mut() {
        if state.mouse_near_right_edge {
            state.mouse_near_right_edge = false;
            retarget(state, mode, drag_active_on_pane == Some(*id), now);
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
        if let Some(window) = self.main_window() {
            crate::app::frame_counters::request_native_redraw(window);
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
        let (cursor_x, cursor_y) = self.main().map(|main| main.cursor_pos).unwrap_or((0.0, 0.0));
        let cursor = (cursor_x as f32, cursor_y as f32);
        let rects: Vec<(u64, f32, f32, f32, f32)> =
            pane_rects.iter().map(|(id, rect)| (*id, rect.x, rect.y, rect.w, rect.h)).collect();
        let mode = self.config.appearance.scrollbar;
        let changed = self
            .main_mut()
            .map(|main| main.update_scrollbar_hover(&rects, cursor, mode, Instant::now()))
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
            pane_rects.iter().map(|(id, rect)| (*id, rect.x, rect.y, rect.w, rect.h)).collect();
        let mode = self.config.appearance.scrollbar;
        let changed = self
            .windows
            .get_mut(&win_id)
            .map(|child| child.update_scrollbar_hover(&rects, cursor, mode, Instant::now()))
            .unwrap_or(false);
        if changed {
            if let Some(child) = self.windows.get(&win_id) {
                child.request_window_redraw();
            }
        }
        changed
    }

    pub(crate) fn clear_scrollbar_hover(&mut self) -> bool {
        let mode = self.config.appearance.scrollbar;
        let changed = self
            .main_mut()
            .map(|main| main.clear_scrollbar_hover_states(mode, Instant::now()))
            .unwrap_or(false);
        if changed {
            self.request_scrollbar_redraw();
        }
        changed
    }

    pub(crate) fn clear_scrollbar_hover_in_child(
        &mut self,
        win_id: winit::window::WindowId,
    ) -> bool {
        let mode = self.config.appearance.scrollbar;
        let changed = self
            .windows
            .get_mut(&win_id)
            .map(|child| child.clear_scrollbar_hover_states(mode, Instant::now()))
            .unwrap_or(false);
        if changed {
            if let Some(child) = self.windows.get(&win_id) {
                child.request_window_redraw();
            }
        }
        changed
    }

    /// Mark a pane's scrollbar as "actively in use" so its alpha
    /// resets to fully-visible and the idle hide timer restarts.
    /// Called from the update points (scroll, drag, view_top jump).
    pub(crate) fn mark_scrollbar_active(&mut self, pane_id: u64) {
        let now = Instant::now();
        let mode = self.config.appearance.scrollbar;
        let marked = self
            .main_mut()
            .map(|main| {
                main.note_scrollbar_activity(pane_id, mode, now);
                true
            })
            .unwrap_or(false);
        if marked {
            self.request_scrollbar_redraw();
        }
    }
}

impl super::WindowState {
    /// The pane whose scrollbar thumb this window is dragging, if any.
    fn scrollbar_drag_pane(&self) -> Option<u64> {
        self.scrollbar_drag.as_ref().map(|drag| drag.pane_id)
    }

    /// Request this window's frame when a pane's alpha has not reached its
    /// target. A settled window has no frame coming, so a retarget that starts
    /// a fade would otherwise leave the bar frozen. Every window method that
    /// retargets ends here; a request already in flight is reused.
    fn wake_scrollbar_if_animating(&mut self) {
        let animating = self
            .scrollbar_vis
            .values()
            .any(|state| (state.alpha - state.target).abs() > f32::EPSILON);
        if !animating {
            // When: no pane is `animating`, every bar shows its target and needs no frame.
            return;
        }
        self.mark_redraw(super::redraw::RedrawCause::Scrollbar);
        if self.frame_deadlines_allowed() && !self.redraw.request_in_flight {
            self.redraw.request_in_flight = true;
            self.request_window_redraw();
        }
    }

    /// Record scrollbar activity on `pane_id`, creating its state when the
    /// pane has not rendered yet, so a scroll before the first frame still shows the bar.
    pub(crate) fn note_scrollbar_activity(
        &mut self,
        pane_id: u64,
        mode: ScrollbarMode,
        now: Instant,
    ) {
        let drag_pane = self.scrollbar_drag_pane();
        note_pane_activity(&mut self.scrollbar_vis, pane_id, mode, drag_pane, now);
        self.wake_scrollbar_if_animating();
    }

    /// Recompute every pane's scrollbar target after this window's drag
    /// started or ended, so a release starts its fade at once.
    fn retarget_scrollbars(&mut self, mode: ScrollbarMode, now: Instant) {
        let drag_pane = self.scrollbar_drag_pane();
        retarget_panes(&mut self.scrollbar_vis, mode, drag_pane, now);
        self.wake_scrollbar_if_animating();
    }

    /// Start a scrollbar thumb drag, which holds its bar shown from `now`.
    pub(crate) fn begin_scrollbar_drag(
        &mut self,
        drag: super::scrollbar_input::ScrollbarDragState,
        mode: ScrollbarMode,
        now: Instant,
    ) {
        self.scrollbar_drag = Some(drag);
        self.retarget_scrollbars(mode, now);
    }

    /// End any scrollbar thumb drag (release, focus loss or drag cancel), so a
    /// bar past its idle window starts fading at `now`. Returns whether a drag ended.
    pub(crate) fn end_scrollbar_drag(&mut self, mode: ScrollbarMode, now: Instant) -> bool {
        let ended = self.scrollbar_drag.take().is_some();
        self.retarget_scrollbars(mode, now);
        ended
    }

    /// Update this window's right-edge hover flags from `cursor` over the
    /// laid-out pane `rects`; true when a pane crossed the threshold.
    pub(crate) fn update_scrollbar_hover(
        &mut self,
        rects: &[(u64, f32, f32, f32, f32)],
        cursor: (f32, f32),
        mode: ScrollbarMode,
        now: Instant,
    ) -> bool {
        let drag_pane = self.scrollbar_drag_pane();
        let changed =
            update_hover_states(&mut self.scrollbar_vis, rects, cursor, mode, drag_pane, now);
        self.wake_scrollbar_if_animating();
        changed
    }

    /// Clear this window's right-edge hover flags; true when one was set.
    pub(crate) fn clear_scrollbar_hover_states(
        &mut self,
        mode: ScrollbarMode,
        now: Instant,
    ) -> bool {
        let drag_pane = self.scrollbar_drag_pane();
        let changed = clear_hover_states(&mut self.scrollbar_vis, mode, drag_pane, now);
        self.wake_scrollbar_if_animating();
        changed
    }
}

#[cfg(test)]
#[path = "scrollbar_visibility_tests.rs"]
mod scrollbar_visibility_tests;
