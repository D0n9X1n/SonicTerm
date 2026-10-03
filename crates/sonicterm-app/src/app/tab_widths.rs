//! The hold rule for measured tab widths.
//!
//! The renderer measures each tab's title with the tab font right before it
//! draws the bar, and stores the width on the tab (`measure_tab_widths`).
//! Hit-testing, drag and tear-out slots, native drag snapshots and the overflow
//! selector read those stored widths, so they always match the bar on screen:
//! a redraw whose frame does not present restores the widths and width limits it
//! was drawn with (`settle_tab_widths`).
//!
//! A title, badge or privilege marker that changes while a tab is pressed or
//! dragged in any window, or while the pointer rests on the bar, is measured
//! but held, so a click never lands on a neighbour that moved; it is laid out
//! once no tab is pressed or dragged and the pointer leaves the bar. Whether the
//! pointer rests on the bar comes from the window's own pointer, which the
//! dispatcher records on every move and leave before an overlay, a modal or a
//! handler can consume the event (`record_window_pointer`).
//!
//! A font, DPI, `tab_min_width` or `tab_max_width` reload is not held: it lays
//! the bar out again at once, even while a tab is pressed or the pointer rests
//! on the bar, because the next frame draws every tab at the new size. Like a
//! title change, it reaches hit-testing once a frame showing it presents.

use sonicterm_gpu::core::PresentOutcome;
use sonicterm_ui::tabs::{LaidOutWidths, TabBar};
use winit::event::WindowEvent;
use winit::window::WindowId;

use super::App;

/// Whether a bar keeps its laid-out tab widths this frame: while any window
/// has a pressed or dragged tab, or while the pointer rests on this bar.
pub(super) fn tab_widths_held(tab_gesture_active: bool, pointer_over_bar: bool) -> bool {
    tab_gesture_active || pointer_over_bar
}

/// Whether a window pointer at `pointer` rests on a bar drawn in the vertical
/// `band`. A hidden bar has no band, and a pointer that left the window is
/// recorded as `(-1, -1)`, which rests on nothing.
pub(super) fn pointer_rests_on_bar(pointer: (f64, f64), band: Option<(f32, f32)>) -> bool {
    let Some((top, bottom)) = band else {
        // When: `band` is None, the bar is hidden, so no pointer rests on it.
        return false;
    };
    let (pointer_x, pointer_y) = (pointer.0 as f32, pointer.1 as f32);
    pointer_x >= 0.0 && pointer_y >= top && pointer_y <= bottom
}

/// Whether a pointer recorded at `pointer` wakes a bar so its held widths lay
/// out: nothing still holds the bar (no tab gesture, pointer not on it) and the
/// bar holds a measured width it has not drawn. Tab hover asks for a frame only
/// when the hovered tab changes, so this wake is what lays held widths out after
/// the pointer leaves from empty bar space.
pub(super) fn held_bar_release_wakes(
    tab_gesture_active: bool,
    holds_widths: bool,
    pointer: (f64, f64),
    band: Option<(f32, f32)>,
) -> bool {
    !tab_gesture_active && holds_widths && !pointer_rests_on_bar(pointer, band)
}

/// Keep the widths and width limits a redraw laid out only when its frame
/// reached the screen. Otherwise restore `drawn`, the geometry still on screen,
/// so clicks and drops resolve against the bar the user sees; the newer
/// measurement stays stored for the next redraw to lay out.
pub(super) fn settle_tab_widths(tabs: &mut TabBar, drawn: LaidOutWidths, outcome: &PresentOutcome) {
    if !matches!(outcome, PresentOutcome::Presented) {
        // The screen still shows `drawn`, so hit-testing keeps it.
        tabs.restore_laid_out_widths(drawn);
    }
}

impl App {
    /// Whether any window has a pressed or dragged tab. A drag can end on any
    /// window's bar, so every bar holds its tab widths until it is released.
    pub(super) fn tab_gesture_active(&self) -> bool {
        self.windows
            .values()
            .any(|window| window.pressed_tab.is_some() || window.drag_session.is_some())
    }

    /// Whether `window`'s bar, drawn in the vertical `band`, keeps its laid-out
    /// widths this frame: while any window has a pressed or dragged tab, or
    /// while `window`'s own recorded pointer rests on the bar.
    pub(super) fn tab_widths_held_in(&self, window: WindowId, band: Option<(f32, f32)>) -> bool {
        let pointer = self.windows.get(&window).map_or((-1.0, -1.0), |state| state.cursor_pos);
        tab_widths_held(self.tab_gesture_active(), pointer_rests_on_bar(pointer, band))
    }

    /// Record `window`'s pointer from a `CursorMoved` or `CursorLeft` event.
    /// The dispatcher calls this before an overlay, a modal or a handler can
    /// consume the event, so the hold never reads a stale pointer. A pointer
    /// that leaves a bar holding widths redraws it, so those widths lay out.
    pub(super) fn record_window_pointer(&mut self, window: WindowId, event: &WindowEvent) {
        let pointer = match event {
            WindowEvent::CursorMoved { position, .. } => (position.x, position.y),
            WindowEvent::CursorLeft { .. } => (-1.0, -1.0),
            _ => {
                // When: `event` is neither a pointer move nor a leave, so the recorded pointer stands.
                return;
            }
        };
        let tab_gesture_active = self.tab_gesture_active();
        let Some(state) = self.windows.get_mut(&window) else {
            // When: `windows` no longer holds `window`, so it has no pointer to record.
            return;
        };
        state.cursor_pos = pointer;
        let band = state.renderer.as_ref().and_then(|renderer| renderer.tab_bar_band());
        if held_bar_release_wakes(
            tab_gesture_active,
            state.tabs.has_held_content_widths(),
            pointer,
            band,
        ) {
            // Nothing holds this bar any more, so a redraw lays its held widths out.
            state.request_window_redraw();
        }
    }

    /// Windows whose bar holds a measured tab width it has not laid out yet.
    pub(super) fn windows_with_held_tab_widths(&self) -> Vec<WindowId> {
        self.windows
            .iter()
            .filter(|(_, window)| window.tabs.has_held_content_widths())
            .map(|(id, _)| *id)
            .collect()
    }

    /// Redraw each bar whose held tab widths can apply now that no tab is
    /// pressed or dragged, so a deferred title change appears without waiting
    /// for an unrelated frame.
    pub(super) fn redraw_held_tab_widths(&self) {
        if self.tab_gesture_active() {
            // When: `tab_gesture_active` still holds every bar, a redraw would keep its widths.
            return;
        }
        for id in self.windows_with_held_tab_widths() {
            if let Some(window) = self.windows.get(&id) {
                window.request_window_redraw();
            }
        }
    }
}

#[cfg(test)]
#[path = "tab_widths_tests.rs"]
mod tab_widths_tests;
