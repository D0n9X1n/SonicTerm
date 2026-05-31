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

use sonic_core::config::ScrollbarMode;
use std::time::{Duration, Instant};

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
    /// edge-hover entry, view-change). Drives the idle-hide window.
    pub last_active: Instant,
    /// Sticky bit: cursor is currently inside the right-edge proximity
    /// strip. When `true` we override the idle-hide timer.
    pub mouse_near_right_edge: bool,
    /// Last frame's `tick` instant. Drives the per-frame lerp step
    /// independent of monitor refresh.
    pub last_tick: Instant,
}

impl ScrollbarVisState {
    /// Construct an initially-hidden state. `now` seeds `last_active`
    /// far enough in the past that the bar starts hidden.
    pub fn new(now: Instant) -> Self {
        let past = now.checked_sub(Duration::from_secs(3600)).unwrap_or(now);
        Self { alpha: 0.0, last_active: past, mouse_near_right_edge: false, last_tick: now }
    }

    /// Record a "user is interacting with this pane's scroll" event
    /// (scrollwheel, drag, view_top jump). Resets the idle-hide window.
    pub fn mark_active(&mut self, now: Instant) {
        self.last_active = now;
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
            let idle_ms = now.saturating_duration_since(state.last_active).as_millis() as u64;
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
    let idle_ms = state.last_active.elapsed().as_millis() as u64;
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
            state.last_active = now;
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
                state.last_active = now;
            }
            changed = true;
        }
    }
    changed
}

use super::{to_logical_pos, App};

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
        let sf = self.main().map(|ws| ws.scale_factor as f32).unwrap_or(1.0);
        let (cx_phys, cy_phys) = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
        let cursor = to_logical_pos(cx_phys, cy_phys, sf);
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
