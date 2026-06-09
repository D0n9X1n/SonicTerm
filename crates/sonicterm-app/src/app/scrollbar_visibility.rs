//! Per-pane scrollbar auto-hide + fade animation (#386 PR-D).
//!
//! Pure helpers + a small `ScrollbarVisState` struct that the
//! window-event + render plumbing in `app::` mutates. Kept as
//! standalone functions so the test suite can exercise them without a
//! winit window or wgpu surface.
//!
//! Semantics (Auto mode):
//! - Scrollbar is hidden by default (alpha 0).
//! - Becomes visible (alpha lerps to 1 over ~150 ms) when any of:
//!   - mouse is within `EDGE_PROXIMITY_PX` of the pane's right edge, OR
//!   - the pane saw scroll/drag activity within the last `IDLE_HIDE_MS`.
//! - Fades back to 0 over ~300 ms once the conditions stop holding and
//!   the idle delay elapses.
//!
//! Always / Never short-circuit to alpha 1.0 / 0.0 with no animation.

use sonicterm_cfg::config::ScrollbarMode;
use std::time::Instant;

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

/// Below this alpha the renderer skips emitting the scrollbar quads
/// entirely (saves two `QuadInstance` writes per pane per frame).
pub const ALPHA_EMIT_FLOOR: f32 = 0.01;

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
    /// Last frame's `tick` instant. Drives the per-frame lerp step
    /// independent of monitor refresh.
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
        return false;
    }
    let right = pane_x + pane_w;
    cursor_x >= right - EDGE_PROXIMITY_PX && cursor_x <= right + EDGE_PROXIMITY_PX.min(8.0)
}

/// Step the alpha animation one frame. Returns the new alpha.
///
/// `drag_active` should be `true` while a scrollbar-thumb drag is in
/// progress on this pane (per PR-C `ScrollbarDragState`).
///
/// For `Always` the returned alpha is always 1.0; for `Never` always 0.0
/// (state struct is bypassed but kept in lock-step so a live mode swap
/// snaps without a stale stored alpha).
pub fn tick(
    state: &mut ScrollbarVisState,
    mode: ScrollbarMode,
    drag_active: bool,
    now: Instant,
) -> f32 {
    match mode {
        ScrollbarMode::Always => {
            state.alpha = 1.0;
            state.last_tick = now;
            1.0
        }
        ScrollbarMode::Never => {
            state.alpha = 0.0;
            state.last_tick = now;
            0.0
        }
        ScrollbarMode::Auto => {
            let idle_ms = match state.last_active {
                Some(t) => now.saturating_duration_since(t).as_millis() as u64,
                None => u64::MAX,
            };
            let visible_now = drag_active || state.mouse_near_right_edge || idle_ms < IDLE_HIDE_MS;
            let target = if visible_now { 1.0 } else { 0.0 };
            let dt_ms = now.saturating_duration_since(state.last_tick).as_millis().max(1) as f32;
            let duration_ms =
                if target > state.alpha { FADE_IN_MS as f32 } else { FADE_OUT_MS as f32 };
            let step = dt_ms / duration_ms;
            let delta = target - state.alpha;
            if delta.abs() <= step {
                state.alpha = target;
            } else {
                state.alpha += step.copysign(delta);
            }
            state.alpha = state.alpha.clamp(0.0, 1.0);
            state.last_tick = now;
            state.alpha
        }
    }
}

/// `true` when the per-frame `tick` will still produce visible change
/// — used to decide whether to schedule another redraw next frame.
pub fn is_animating(state: &ScrollbarVisState, mode: ScrollbarMode, drag_active: bool) -> bool {
    if !matches!(mode, ScrollbarMode::Auto) {
        return false;
    }
    let idle_ms = match state.last_active {
        Some(t) => t.elapsed().as_millis() as u64,
        None => u64::MAX,
    };
    let visible_now = drag_active || state.mouse_near_right_edge || idle_ms < IDLE_HIDE_MS;
    let target = if visible_now { 1.0 } else { 0.0 };
    (state.alpha - target).abs() > f32::EPSILON
        // Even when alpha is currently parked at 1.0, the idle window may
        // close shortly and trigger a fade-out. Keep scheduling frames until
        // that boundary passes so Auto mode does not freeze fully visible
        // until the next unrelated terminal event.
        || idle_ms < IDLE_HIDE_MS
        || (visible_now && state.alpha < 1.0)
        || (!visible_now && state.alpha > 0.0)
}

/// One-shot helper used at the top of the render path: for the given
/// pane list (id + logical rect), update each pane's
/// `mouse_near_right_edge` from the current cursor, tick the alpha,
/// and return a map of `(pane_id -> alpha)` for `PaneRender`. Closed
/// panes are pruned from `vis` in-place.
pub fn update_and_collect(
    vis: &mut std::collections::HashMap<u64, ScrollbarVisState>,
    panes: &[(u64, f32, f32, f32, f32)],
    cursor: (f32, f32),
    active_id: u64,
    drag_active_on_pane: Option<u64>,
    mode: ScrollbarMode,
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
        let alpha = tick(state, mode, drag, now);
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

use super::App;

impl App {
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
            return false;
        }
        let pane_rects = self.compute_active_pane_rects();
        if pane_rects.is_empty() {
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
            return false;
        }
        let Some(child) = self.windows.get(&win_id) else { return false };
        let pane_rects = Self::compute_pane_rects_for(child);
        if pane_rects.is_empty() {
            return false;
        }
        let cursor = (child.cursor_pos.0 as f32, child.cursor_pos.1 as f32);
        let rects: Vec<(u64, f32, f32, f32, f32)> =
            pane_rects.iter().map(|(id, r)| (*id, r.x, r.y, r.w, r.h)).collect();
        let changed = self
            .windows
            .get_mut(&win_id)
            .map(|child| update_hover_states(&mut child.scrollbar_vis, &rects, cursor, Instant::now()))
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
    /// Called from PR-D update points (scroll, drag, view_top jump).
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
mod tests {
    //! Pure-helper coverage for the auto-hide/fade model. These functions
    //! back BOTH the main-window render path (`window_event.rs`) and the
    //! torn-out child render path (`child_window.rs`) verbatim, so a single
    //! correct spec here pins main/child scrollbar parity. The
    //! `child_window` integration suite exercises the same helpers through
    //! the child plumbing; this module nails the math directly.

    use super::*;
    use std::time::Duration;

    // A single pane id=1 occupying x∈[0,800), y∈[30,600).
    const PANE: (u64, f32, f32, f32, f32) = (1, 0.0, 30.0, 800.0, 570.0);

    fn at(secs_ago: u64, now: Instant) -> Instant {
        now.checked_sub(Duration::from_secs(secs_ago)).unwrap()
    }

    #[test]
    fn new_state_starts_hidden() {
        let now = Instant::now();
        let s = ScrollbarVisState::new(now);
        assert_eq!(s.alpha, 0.0);
        assert!(!s.mouse_near_right_edge);
        // `None` == never active == infinitely idle, so the bar starts
        // hidden. This must hold even on a freshly-booted machine whose
        // monotonic clock is younger than the old 3600s offset (the bug
        // CI caught on fresh Windows runners).
        assert_eq!(s.last_active, None);
        assert!(!is_animating(&s, ScrollbarMode::Auto, false), "fresh state must not animate");
    }

    #[test]
    fn idle_cursor_away_from_edge_stays_hidden() {
        // The user's bug: scrollbar visible without the cursor near the
        // right edge. With no recent activity and the cursor parked in the
        // middle of the pane, alpha must stay 0 and the bar must not animate.
        let now = Instant::now();
        let mut vis = std::collections::HashMap::new();
        let cursor = (400.0, 300.0); // dead-center, far from right edge
        let alphas = update_and_collect(
            &mut vis,
            &[PANE],
            cursor,
            PANE.0,
            None,
            ScrollbarMode::Auto,
            now,
        );
        assert_eq!(alphas.get(&1).copied(), Some(0.0), "center cursor must keep bar hidden");
        let st = vis.get(&1).unwrap();
        assert!(!is_animating(st, ScrollbarMode::Auto, false), "settled-hidden must not redraw-storm");
    }

    #[test]
    fn cursor_near_right_edge_shows_bar() {
        let now = Instant::now();
        let mut vis = std::collections::HashMap::new();
        // x just inside the right edge (800 - 5 = 795) within EDGE_PROXIMITY_PX.
        let cursor = (795.0, 300.0);
        // First frame enters the proximity strip → marks active, begins fade-in.
        update_and_collect(&mut vis, &[PANE], cursor, 1, None, ScrollbarMode::Auto, now);
        assert!(vis.get(&1).unwrap().mouse_near_right_edge);
        // Advance ~200ms (> FADE_IN_MS) and the bar reaches full alpha.
        let later = now.checked_add(Duration::from_millis(200)).unwrap();
        let alphas =
            update_and_collect(&mut vis, &[PANE], cursor, 1, None, ScrollbarMode::Auto, later);
        assert_eq!(alphas.get(&1).copied(), Some(1.0));
    }

    #[test]
    fn recent_scroll_activity_keeps_bar_visible_then_fades() {
        // Mirrors set_child_pane_view_top/mark_scrollbar_active: a scroll
        // marks the pane active, so the bar shows even with the cursor away
        // from the edge — but only for the idle window, then it fades.
        let now = Instant::now();
        let mut st = ScrollbarVisState::new(now);
        st.mark_active(now);
        // Immediately after activity: animating toward visible.
        assert!(is_animating(&st, ScrollbarMode::Auto, false));
        let v = tick(&mut st, ScrollbarMode::Auto, false, now.checked_add(Duration::from_millis(200)).unwrap());
        assert_eq!(v, 1.0, "recent activity makes the bar fully visible");
        // Long past the idle window with no further activity: fades to hidden.
        st.last_active = Some(at(10, now));
        let faded = tick(&mut st, ScrollbarMode::Auto, false, now.checked_add(Duration::from_secs(11)).unwrap());
        assert_eq!(faded, 0.0, "idle past IDLE_HIDE_MS fades the bar out");
        assert!(!is_animating(&st, ScrollbarMode::Auto, false), "fully hidden + idle must not keep redrawing");
    }

    #[test]
    fn always_and_never_short_circuit() {
        let now = Instant::now();
        let mut st = ScrollbarVisState::new(now);
        assert_eq!(tick(&mut st, ScrollbarMode::Always, false, now), 1.0);
        assert!(!is_animating(&st, ScrollbarMode::Always, false), "Always never animates");
        assert_eq!(tick(&mut st, ScrollbarMode::Never, false, now), 0.0);
        assert!(!is_animating(&st, ScrollbarMode::Never, false), "Never never animates");
    }

    #[test]
    fn drag_overrides_idle_and_edge() {
        // A thumb drag keeps the bar visible regardless of cursor position
        // or idle time — true on both windows (drag_active_on_pane).
        let now = Instant::now();
        let mut vis = std::collections::HashMap::new();
        let cursor = (10.0, 300.0); // far left, nowhere near the edge
        let later = now.checked_add(Duration::from_millis(300)).unwrap();
        update_and_collect(&mut vis, &[PANE], cursor, 1, Some(1), ScrollbarMode::Auto, now);
        let alphas =
            update_and_collect(&mut vis, &[PANE], cursor, 1, Some(1), ScrollbarMode::Auto, later);
        assert_eq!(alphas.get(&1).copied(), Some(1.0), "active drag forces visible");
    }

    #[test]
    fn near_edge_band_is_tight_to_the_right_gutter() {
        // Regression guard for the "scrollbar shows without edge hover"
        // report: the proximity test must be FALSE for a center cursor and
        // TRUE only within EDGE_PROXIMITY_PX of the right edge.
        let (_, px, py, pw, ph) = PANE;
        assert!(!is_mouse_near_right_edge(px, py, pw, ph, 400.0, 300.0), "center is not near edge");
        assert!(!is_mouse_near_right_edge(px, py, pw, ph, 770.0, 300.0), "30px in is outside the 20px band");
        assert!(is_mouse_near_right_edge(px, py, pw, ph, 795.0, 300.0), "5px from edge is inside the band");
        // Outside the pane vertically → never near the edge.
        assert!(!is_mouse_near_right_edge(px, py, pw, ph, 795.0, 5.0), "above the pane is not near edge");
    }
}

