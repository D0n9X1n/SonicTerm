//! Main-window collection, renderer handoff and frame settlement.

use super::*;

#[cfg(test)]
#[path = "window_event_redraw_tests.rs"]
mod window_event_redraw_tests;

impl App {
    /// The pointer owners main and torn-out windows share, ahead of either window's own handler:
    /// a query-field gesture, the open palette, then the right button, which only the status line
    /// strip handles. Returns `None` when the event goes on to the window's handler, else whether
    /// the consuming owner was the palette taking a left release (pending windows then open).
    /// Ask the key's window for a frame once the key is handled, unless a step of the key
    /// already asked during this dispatch.
    pub(in crate::app) fn finish_window_key(&mut self, win_id: WindowId) {
        self.request_redraw_once(win_id);
    }
}
