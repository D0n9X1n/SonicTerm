//! One window's cursor clock: its blink, and whether its cursor may glide.
//!
//! The blink runs from the window's last activity: a key, paste or IME event in the window, focus
//! gained, or a frame that found the active pane's cursor somewhere new (output that moved it). It
//! rests solid [`BLINK_REST_AFTER`] later, so an idle window schedules no blink frames. The clock
//! lives here, per window, because only the app sees input; the renderer receives the sampled
//! phase through `GpuRenderer::set_cursor_blink_off`.

use std::time::{Duration, Instant};

use winit::window::WindowId;

#[cfg(doc)]
use sonicterm_ui::cursor::BLINK_REST_AFTER;
use sonicterm_ui::cursor::{blink_on, next_blink_deadline};

/// Output is *sustained* when it advanced in at least this many of the last
/// [`SUSTAINED_WINDOW`] frames' collections.
const SUSTAINED_FRAMES: u32 = 4;
/// How many recent collections the sustained-output rule looks at.
const SUSTAINED_WINDOW: u32 = 6;
/// Output right after input is its echo, never sustained: a typed character or an arrow key's
/// cursor move still glides.
const INPUT_ECHO: Duration = Duration::from_millis(250);

/// One window's blink clock and what its last frame showed.
#[derive(Debug, Clone)]
pub(crate) struct CursorBlink {
    /// The last activity; the blink's first, visible phase starts here.
    epoch: Instant,
    /// The active pane and cursor cell the last frame found, so a move restarts the blink.
    last_cursor: Option<(u64, u16, u16)>,
    /// Whether the last frame was sampled in an off phase, so activity can show the cursor at once.
    last_frame_off: bool,
    /// The last key, paste or IME event in the window.
    last_input: Option<Instant>,
    /// The active pane and its output generation at the last collection.
    last_output: Option<(u64, u64)>,
    /// One bit per recent collection, newest lowest: whether output advanced since the one before.
    output_bits: u32,
    /// The glide frame deadline armed for this window, kept until a frame is collected or it
    /// passes, so unrelated events never push it later and a deferred frame never makes it due
    /// again at once. Interior so the deadline fold, which only reads the App, can arm it.
    glide_deadline: std::cell::Cell<Option<Instant>>,
}

impl CursorBlink {
    /// A clock whose blink starts at `now`, visible.
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            epoch: now,
            last_cursor: None,
            last_frame_off: false,
            last_input: None,
            last_output: None,
            output_bits: 0,
            glide_deadline: std::cell::Cell::new(None),
        }
    }

    /// Restart the blink at `now`. Returns whether the last frame hid the cursor, in which case the
    /// caller requests a frame now so the cursor shows without waiting for the next deadline.
    pub(crate) fn activity(&mut self, now: Instant) -> bool {
        self.epoch = now;
        self.last_input = Some(now);
        // The restarted blink shows the cursor, so only the first activity after a hidden frame
        // owes a frame.
        std::mem::take(&mut self.last_frame_off)
    }

    /// Record a collection at `now` that found pane `pane_id`'s output at `generation`, and return
    /// whether that output is sustained: it advanced in at least 4 of the last 6 collections, with
    /// no input in the last 250 ms. A cursor never glides through sustained output.
    pub(crate) fn note_output(&mut self, now: Instant, pane_id: u64, generation: u64) -> bool {
        let advanced = self
            .last_output
            .is_some_and(|(pane, previous)| pane == pane_id && generation != previous);
        self.last_output = Some((pane_id, generation));
        let mask = (1 << SUSTAINED_WINDOW) - 1;
        self.output_bits = ((self.output_bits << 1) | u32::from(advanced)) & mask;
        let echo =
            self.last_input.is_some_and(|input| now.saturating_duration_since(input) < INPUT_ECHO);
        !echo && self.output_bits.count_ones() >= SUSTAINED_FRAMES
    }

    /// The phase for a frame collected at `now` that found the active pane `pane_id`'s cursor at
    /// `(row, col)`: `true` hides the cursor. A cursor somewhere new counts as activity first.
    /// `blinking` is whether the cursor blinks and is drawn at all; when it is not, the frame is
    /// always on.
    pub(crate) fn sample(&mut self, now: Instant, cursor: (u64, u16, u16), blinking: bool) -> bool {
        // A collected frame services the armed glide deadline; the next one is armed afresh.
        self.glide_deadline.set(None);
        if self.last_cursor != Some(cursor) {
            // The cursor is in a new pane or cell: output or a switch moved it, so restart.
            self.last_cursor = Some(cursor);
            self.epoch = now;
        }
        let off = blinking && !blink_on(self.epoch, now);
        self.last_frame_off = off;
        off
    }

    /// The next glide frame's deadline: the armed one while it is still ahead, otherwise one frame
    /// `period` after the later of `last_render` and `now`, armed until a frame is collected. Unrelated
    /// events therefore never slide it, and a frame deferred past it waits a whole period.
    pub(crate) fn glide_deadline(
        &self,
        now: Instant,
        last_render: Instant,
        period: Duration,
    ) -> Instant {
        if let Some(armed) = self.glide_deadline.get().filter(|armed| *armed > now) {
            // When: an `armed` deadline is still ahead, it stands.
            return armed;
        }
        let deadline = last_render.max(now) + period;
        self.glide_deadline.set(Some(deadline));
        deadline
    }

    /// When this window's next blink frame is due, or `None` once the blink rests.
    pub(crate) fn deadline(&self, now: Instant) -> Option<Instant> {
        next_blink_deadline(self.epoch, now)
    }
}

/// Sample `window`'s cursor for a frame collected at `now` and hand it to its renderer: the blink
/// phase, and whether the cursor may glide. The active pane `pane_id` shows its cursor at
/// `(row, col)` with its output at `generation`; `cursor_shown` is whether the frame draws a
/// terminal cursor at all (visible, at the live view, not read-only). Only a focused window with
/// `cursor_blink` on and a shown cursor blinks; a cursor glides only with `smooth`, no synchronized
/// update open (the caller folds it into `smooth`) and no sustained output.
pub(crate) fn sample_window_blink(
    window: &mut super::WindowState,
    now: Instant,
    pane_id: u64,
    (row, col): (u16, u16),
    generation: u64,
    cursor_shown: bool,
    smooth: bool,
) {
    let Some(renderer) = window.renderer.as_mut() else {
        // When: the window has no renderer (a headless test window), there is nothing to draw.
        return;
    };
    let blinking = renderer.cursor_blink() && renderer.window_focused() && cursor_shown;
    let off = window.cursor_blink.sample(now, (pane_id, row, col), blinking);
    renderer.set_cursor_blink_off(off);
    let sustained = window.cursor_blink.note_output(now, pane_id, generation);
    renderer.set_cursor_glide_allowed(smooth && !sustained);
}

impl super::App {
    /// Restart the blink of the window holding `pane_id`, as input that arrives without a key event
    /// (a menu paste, a file drop) does; a cursor hidden in an off phase is shown by a redraw now.
    /// Returns the window it asked to redraw, so a caller or test can tell which owner was woken.
    pub(in crate::app) fn note_cursor_activity(&mut self, pane_id: u64) -> Option<WindowId> {
        let id = self.registry.window_of_pane(pane_id)?;
        let window = self.registry.windows.get_mut(&id)?;
        if !window.cursor_blink.activity(Instant::now()) {
            // When: `activity` reports the cursor was shown, the next ordinary frame keeps it shown.
            return None;
        }
        // The last frame hid the cursor: that window, main or torn out, redraws now to show it.
        window.mark_redraw(super::redraw::RedrawCause::Cursor);
        if let Some(native) = window.window.as_ref() {
            super::frame_counters::request_native_redraw(native);
        }
        Some(id)
    }
}

/// Sample `window`'s cursor for a frame that holds the active pane `pane_id`'s `grid`: its blink
/// phase and whether it may glide, as [`sample_window_blink`]. `live_view` is whether the pane
/// shows its live screen rather than a scrolled-back view; `smooth` is `cursor_animation`.
// Ordering: output_generation Acquire pairs with the VT worker's Release publish of each batch.
pub(crate) fn sample_frame_cursor(
    window: &mut super::WindowState,
    parser: &sonicterm_vt::vt::Parser,
    pane_id: u64,
    live_view: bool,
    smooth: bool,
) {
    let grid = parser.grid();
    let cursor = (grid.cursor.row, grid.cursor.col);
    // The pane's own synchronized update, read under the frame's parser guard: a frame admitted by
    // its timeout or a forced redraw still sees it open.
    let sync_open = parser.synchronized_output().set;
    let shown = window_blinks(window) && live_view;
    let generation = window
        .panes
        .get(&pane_id)
        .map_or(0, |pane| pane.output_generation.load(std::sync::atomic::Ordering::Acquire));
    sample_window_blink(
        window,
        Instant::now(),
        pane_id,
        cursor,
        generation,
        shown,
        smooth && !sync_open,
    );
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
