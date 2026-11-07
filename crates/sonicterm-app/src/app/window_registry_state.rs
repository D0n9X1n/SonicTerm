//! The App's window registry: which windows exist, which one is main, which is frontmost, the
//! stable key each window has, and the pool of prepared windows. Panes are owned through their
//! window here, and every lookup by pane id goes through it.
//!
//! The window map is a [`WindowMap`]: callers read it and change a window in place, but only
//! [`WindowRegistry::insert_window`] and [`WindowRegistry::remove_window`] add or drop a window,
//! and both check in debug builds that no pane belongs to two windows.

use super::*;

/// Every window by id. Reads and in-place changes are open to the app; adding or dropping a
/// window is not, so it goes through the registry's checked `insert_window`/`remove_window`.
#[derive(Default)]
pub(in crate::app) struct WindowMap(HashMap<WindowId, WindowState>);

impl WindowMap {
    /// The window `id`, if it exists.
    pub(in crate::app) fn get(&self, id: &WindowId) -> Option<&WindowState> {
        self.0.get(id)
    }

    /// [`Self::get`], mutably; the window stays in the map.
    pub(in crate::app) fn get_mut(&mut self, id: &WindowId) -> Option<&mut WindowState> {
        self.0.get_mut(id)
    }

    /// Whether window `id` exists.
    pub(in crate::app) fn contains_key(&self, id: &WindowId) -> bool {
        self.0.contains_key(id)
    }

    /// Every window id, in the map's arbitrary order.
    pub(in crate::app) fn keys(&self) -> impl Iterator<Item = &WindowId> {
        self.0.keys()
    }

    /// Every window, in the map's arbitrary order.
    pub(in crate::app) fn values(&self) -> impl Iterator<Item = &WindowState> {
        self.0.values()
    }

    /// Every window, mutably; none can be added or dropped through this.
    pub(in crate::app) fn values_mut(&mut self) -> impl Iterator<Item = &mut WindowState> {
        self.0.values_mut()
    }

    /// Every (id, window) pair.
    pub(in crate::app) fn iter(&self) -> impl Iterator<Item = (&WindowId, &WindowState)> {
        self.0.iter()
    }

    /// Every (id, window) pair, with the window mutable.
    pub(in crate::app) fn iter_mut(
        &mut self,
    ) -> impl Iterator<Item = (&WindowId, &mut WindowState)> {
        self.0.iter_mut()
    }

    /// How many windows exist, report windows included.
    pub(in crate::app) fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether no window exists; tests read it, production asks `main_window_id` instead.
    #[cfg(test)]
    pub(in crate::app) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Room for `additional` more windows, so a later insert cannot allocate.
    pub(in crate::app) fn reserve(&mut self, additional: usize) {
        self.0.reserve(additional);
    }

    /// Add or replace a window; only [`WindowRegistry::insert_window`] calls it.
    fn insert(&mut self, id: WindowId, window: WindowState) -> Option<WindowState> {
        self.0.insert(id, window)
    }

    /// Drop a window; only [`WindowRegistry::remove_window`] calls it.
    fn remove(&mut self, id: &WindowId) -> Option<WindowState> {
        self.0.remove(id)
    }
}

impl std::ops::Index<&WindowId> for WindowMap {
    type Output = WindowState;

    /// The window `id`; panics when it does not exist, as `HashMap` indexing does.
    fn index(&self, id: &WindowId) -> &WindowState {
        &self.0[id]
    }
}

impl<'map> IntoIterator for &'map WindowMap {
    type Item = (&'map WindowId, &'map WindowState);
    type IntoIter = std::collections::hash_map::Iter<'map, WindowId, WindowState>;

    /// [`WindowMap::iter`], so `for (id, window) in &windows` reads as it did on the map.
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'map> IntoIterator for &'map mut WindowMap {
    type Item = (&'map WindowId, &'map mut WindowState);
    type IntoIter = std::collections::hash_map::IterMut<'map, WindowId, WindowState>;

    /// [`WindowMap::iter_mut`], so `for (id, window) in &mut windows` reads as it did on the map.
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter_mut()
    }
}

/// Where a pane lives: its window, and the index of the tab whose tree holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) struct PaneSlot {
    /// The window whose `panes` own the pane.
    pub(in crate::app) window_id: WindowId,
    /// The tab in that window whose layout holds the pane.
    pub(in crate::app) tab_index: usize,
}

/// The windows that exist, the main and frontmost window, each window key, and prepared windows.
pub(in crate::app) struct WindowRegistry {
    /// Every terminal and report window, main included; each owns its tabs and panes, so a pane
    /// belongs to exactly one window.
    pub(in crate::app) windows: WindowMap,
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
    /// Add `window` as `id`; returns a window it replaced. Only the App's registration chokepoint
    /// calls it, so every window also gets its governor owner. Debug builds check that none of
    /// its panes already belongs to another window.
    pub(in crate::app) fn insert_window(
        &mut self,
        id: WindowId,
        window: WindowState,
    ) -> Option<WindowState> {
        let replaced = self.windows.insert(id, window);
        self.debug_assert_unique_panes();
        replaced
    }

    /// Drop window `id` and return it, so the caller can tear its panes down or move them.
    pub(in crate::app) fn remove_window(&mut self, id: WindowId) -> Option<WindowState> {
        let removed = self.windows.remove(&id);
        self.debug_assert_unique_panes();
        removed
    }

    /// A pane id two windows both own, if any; a pane must belong to exactly one window.
    pub(in crate::app) fn duplicate_pane(&self) -> Option<u64> {
        let mut seen = std::collections::HashSet::new();
        self.windows
            .values()
            .flat_map(|window| window.panes.keys())
            .find(|&&pane_id| !seen.insert(pane_id))
            .copied()
    }

    /// Panics in debug builds when [`Self::duplicate_pane`] finds a pane two windows own. The
    /// walk is linear in the panes, and windows are added and dropped rarely.
    pub(in crate::app) fn debug_assert_unique_panes(&self) {
        if cfg!(debug_assertions) {
            // When: `debug_assertions` are on, walk every pane; release builds skip the walk.
            if let Some(pane_id) = self.duplicate_pane() {
                panic!("pane {pane_id} belongs to two windows");
            }
        }
    }

    /// The window and tab that hold `pane_id`; `None` when no window owns it or no tab's layout
    /// holds it (a pane mid-move).
    pub(in crate::app) fn locate_pane(&self, pane_id: u64) -> Option<PaneSlot> {
        let window_id = self.window_of_pane(pane_id)?;
        let tab_index = self.windows[&window_id]
            .tab_states
            .iter()
            .position(|tab_state| tab_state.tree.contains_pane(pane_id))?;
        Some(PaneSlot { window_id, tab_index })
    }

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
