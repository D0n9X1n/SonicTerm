//! The App's window registry: which windows exist, which one is main, which is frontmost, the
//! stable key each window has, and the pool of prepared windows. Panes are owned through their
//! window here, and every lookup by pane id goes through it.

use super::*;

/// The windows that exist, the main and frontmost window, each window key, and prepared windows.
pub(in crate::app) struct WindowRegistry {
    /// Every terminal and report window, main included; each owns its tabs and panes, so a pane
    /// belongs to exactly one window.
    pub(in crate::app) windows: HashMap<WindowId, WindowState>,
    /// Id of the main window. Set in `do_resumed` once the main `Window` is
    /// created and its [`WindowState`] is inserted into [`Self::windows`].
    ///
    /// `None` before that point, which is why every `main_*()` accessor
    /// returns an `Option` rather than assuming a main window exists.
    pub(in crate::app) main_window_id: Option<WindowId>,
    /// Most-recently-OS-frontmost window id, INCLUDING the main window.
    /// Tracks *every* sonic-owned terminal window with a single
    /// non-`Option` discriminant once the first focus arrives:
    ///
    ///   * `Some(main_window_id)`  → main window is OS-frontmost
    ///   * `Some(child_window_id)` → that child window is OS-frontmost
    ///   * `None`                  → no sonic window has been focused yet,
    ///     OR focus has moved out of every sonic window to another app.
    ///
    /// Subsumes a separate "which child has focus" field: main-vs-child is
    /// discriminated by `frontmost_kind()`, so one id answers both questions
    /// and the two cannot disagree.
    ///
    /// Keyboard / menubar / accelerator actions (Cmd+T, Cmd+W, Cmd+\\, …)
    /// route through this id so a chord typed in window B never mutates
    /// window A's tab vec. Set in both the main and child `Focused(true)`
    /// arms; on `Focused(false)` we only clear when the dropped window was
    /// the current frontmost — focus moving to a *different* sonic window
    /// arrives as that other window's `Focused(true)` and overwrites
    /// frontmost in the right order.
    ///
    /// Without it, Cmd+T after a tear-out opened a tab in the wrong window,
    /// and Cmd+W in a new window closed the old window's tab.
    pub(in crate::app) frontmost_window: Option<WindowId>,
    pub(in crate::app) window_keys: crate::window_key_boundary::WindowKeyRegistry,
    /// Windows created ahead of need, so a new window or tear-out appears without the wait.
    pub(in crate::app) warm_window_pool: Vec<WarmWindow>,
}

impl WindowRegistry {
    /// The pane `pane_id` in whichever window owns it; a pane belongs to exactly one window.
    pub(in crate::app) fn pane(&self, pane_id: u64) -> Option<&PaneState> {
        self.windows.values().find_map(|window| window.panes.get(&pane_id))
    }

    /// [`Self::pane`], mutably.
    pub(in crate::app) fn pane_mut(&mut self, pane_id: u64) -> Option<&mut PaneState> {
        self.windows.values_mut().find_map(|window| window.panes.get_mut(&pane_id))
    }

    /// The window that owns `pane_id`, if any.
    pub(in crate::app) fn window_of_pane(&self, pane_id: u64) -> Option<WindowId> {
        self.windows
            .iter()
            .find_map(|(id, window)| window.panes.contains_key(&pane_id).then_some(*id))
    }
}

#[cfg(test)]
#[path = "window_registry_state_tests.rs"]
mod window_registry_state_tests;
