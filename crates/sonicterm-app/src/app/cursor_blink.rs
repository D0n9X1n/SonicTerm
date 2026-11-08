//! One window's cursor blink clock.
//!
//! The blink runs from the window's last activity: a key, paste or IME event in the window, focus
//! gained, or a frame that found the active pane's cursor somewhere new (output that moved it). It
//! rests solid [`BLINK_REST_AFTER`] later, so an idle window schedules no blink frames. The clock
//! lives here, per window, because only the app sees input; the renderer receives the sampled
//! phase through `GpuRenderer::set_cursor_blink_off`.

use std::time::Instant;

#[cfg(doc)]
use sonicterm_ui::cursor::BLINK_REST_AFTER;
use sonicterm_ui::cursor::{blink_on, next_blink_deadline};

/// One window's blink clock and what its last frame showed.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CursorBlink {
    /// The last activity; the blink's first, visible phase starts here.
    epoch: Instant,
    /// The active pane and cursor cell the last frame found, so a move restarts the blink.
    last_cursor: Option<(u64, u16, u16)>,
    /// Whether the last frame was sampled in an off phase, so activity can show the cursor at once.
    last_frame_off: bool,
}

impl CursorBlink {
    /// A clock whose blink starts at `now`, visible.
    pub(crate) fn new(now: Instant) -> Self {
        Self { epoch: now, last_cursor: None, last_frame_off: false }
    }

    /// Restart the blink at `now`. Returns whether the last frame hid the cursor, in which case the
    /// caller requests a frame now so the cursor shows without waiting for the next deadline.
    pub(crate) fn activity(&mut self, now: Instant) -> bool {
        self.epoch = now;
        self.last_frame_off
    }

    /// The phase for a frame collected at `now` that found the active pane `pane_id`'s cursor at
    /// `(row, col)`: `true` hides the cursor. A cursor somewhere new counts as activity first.
    /// `blinking` is whether the cursor blinks and is drawn at all; when it is not, the frame is
    /// always on.
    pub(crate) fn sample(&mut self, now: Instant, cursor: (u64, u16, u16), blinking: bool) -> bool {
        if self.last_cursor != Some(cursor) {
            // The cursor is in a new pane or cell: output or a switch moved it, so restart.
            self.last_cursor = Some(cursor);
            self.epoch = now;
        }
        let off = blinking && !blink_on(self.epoch, now);
        self.last_frame_off = off;
        off
    }

    /// When this window's next blink frame is due, or `None` once the blink rests.
    pub(crate) fn deadline(&self, now: Instant) -> Option<Instant> {
        next_blink_deadline(self.epoch, now)
    }
}

/// Sample `window`'s blink for a frame collected at `now` and hand the phase to its renderer. The
/// active pane `pane_id` shows its cursor at `(row, col)`; `cursor_shown` is whether the frame
/// draws a terminal cursor at all (visible, at the live view, not read-only). Only a focused window
/// with `cursor_blink` on and a shown cursor blinks.
pub(crate) fn sample_window_blink(
    window: &mut super::WindowState,
    now: Instant,
    pane_id: u64,
    (row, col): (u16, u16),
    cursor_shown: bool,
) {
    let Some(renderer) = window.renderer.as_mut() else {
        // When: the window has no renderer (a headless test window), there is nothing to hide.
        return;
    };
    let blinking = renderer.cursor_blink() && renderer.window_focused() && cursor_shown;
    let off = window.cursor_blink.sample(now, (pane_id, row, col), blinking);
    renderer.set_cursor_blink_off(off);
}

/// Whether `window`'s cursor blinks now: blinking on, focused, and its active pane's cursor drawn
/// (visible and at the live view). A window that does not blink schedules no blink frame.
pub(crate) fn window_blinks(window: &super::WindowState) -> bool {
    let Some(renderer) = window.renderer.as_ref() else {
        // When: the window has no renderer, it draws no cursor to blink.
        return false;
    };
    blinks(window, renderer.cursor_blink(), renderer.window_focused())
}

/// [`window_blinks`] with the renderer's `cursor_blink` setting and focus passed in, so a headless
/// test window, which has no renderer, can be checked too.
// Ordering: cursor_visible Relaxed reads only choose whether a blink deadline exists.
pub(crate) fn blinks(window: &super::WindowState, cursor_blink: bool, focused: bool) -> bool {
    let read_only = window.copy_mode.as_ref().is_some_and(|state| state.is_read_only());
    cursor_blink
        && focused
        && !read_only
        && window
            .tab_states
            .get(window.tabs.active_index())
            .and_then(|tab| window.panes.get(&tab.active_pane))
            .is_some_and(|pane| {
                pane.cursor_visible.load(std::sync::atomic::Ordering::Relaxed)
                    && pane.viewport_top_abs.is_none()
            })
}

#[cfg(test)]
#[path = "cursor_blink_tests.rs"]
mod cursor_blink_tests;
