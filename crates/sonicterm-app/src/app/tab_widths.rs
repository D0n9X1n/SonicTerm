//! The hold rule for measured tab widths.
//!
//! The renderer measures each tab's title with the tab font right before it
//! draws the bar, and stores the width on the tab (`measure_tab_widths`).
//! Hit-testing, drag and tear-out slots, native drag snapshots and the overflow
//! selector read those stored widths, so they always match the last drawn bar.
//! A title, badge or privilege marker that changes while a tab is pressed or
//! dragged, or while the pointer rests on the bar, is measured but held, so a
//! click never lands on a neighbour that moved; it is laid out once the bar is
//! released.

use winit::window::WindowId;

use super::App;

/// Whether a bar keeps its laid-out tab widths this frame: while any window
/// has a pressed or dragged tab, or while the pointer rests on this bar.
pub(super) fn tab_widths_held(tab_gesture_active: bool, pointer_over_bar: bool) -> bool {
    tab_gesture_active || pointer_over_bar
}

impl App {
    /// Whether any window has a pressed or dragged tab. A drag can end on any
    /// window's bar, so every bar holds its tab widths until it is released.
    pub(super) fn tab_gesture_active(&self) -> bool {
        self.windows
            .values()
            .any(|window| window.pressed_tab.is_some() || window.drag_session.is_some())
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
                window.request_redraw();
            }
        }
    }
}

#[cfg(test)]
#[path = "tab_widths_tests.rs"]
mod tab_widths_tests;
