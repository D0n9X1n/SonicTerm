//! Pane teardown when a pane's child process ends.
//!
//! A shell that exits leaves its pane holding a dead grid. Closing that pane
//! is what a user typing `exit` means, but it also destroys their scrollback,
//! so it happens only on evidence that the exit was clean. An unclean exit —
//! or one whose status could not be read — leaves the pane on screen with
//! whatever killed it still legible.

use winit::window::WindowId;

use super::{child_window, App};

/// Where an exited pane sits in the window/tab topology.
struct ExitedPaneSite {
    /// The window holding the pane.
    window: WindowId,
    /// Index of the pane's tab within that window.
    tab_index: usize,
    /// Whether the pane is its tab's only leaf, so closing it empties the tab.
    sole_leaf: bool,
}

impl App {
    /// Act on a pane whose child process ended.
    ///
    /// `was_clean` is the classification made by that pane's VT worker before
    /// it exited. `None` means the status could not be read, which holds the
    /// pane open exactly as an unclean exit does: closing on our own
    /// uncertainty would discard a user's scrollback to no purpose.
    pub(super) fn handle_pane_process_exited(&mut self, pane_id: u64, was_clean: Option<bool>) {
        if was_clean != Some(true) {
            // When: `was_clean` is false or unknown, preserve the pane and its scrollback for diagnosis.
            tracing::debug!(
                pane = pane_id,
                ?was_clean,
                "pane child exited without a clean status; holding the pane open"
            );
            return;
        }
        let Some(site) = self.locate_exited_pane(pane_id) else {
            // When: `locate_exited_pane(pane_id)` returns no `site`, teardown raced and nothing remains to close.
            return;
        };
        // The intent describes what happened regardless of how much topology
        // follows from it, and its `PtyClose` effect owns the multi-leaf case
        // — closing the tree node and resizing the survivor.
        self.dispatch_intent(sonicterm_app_core::AppIntent::PtyExit {
            pane: sonicterm_app_core::PaneId(pane_id),
            status: 0,
        });
        if !site.sole_leaf {
            // When: `site.sole_leaf` is false, reducer cleanup resized the surviving sibling and the tab remains usable.
            return;
        }
        // The tab's only pane is gone, so the tab has nothing left to show.
        // The reducer does not reach this: its pane close deliberately leaves
        // a sole leaf in place, which is why the tab used to survive its own
        // shell.
        tracing::info!(
            pane = pane_id,
            tab = site.tab_index,
            "pane child exited cleanly and emptied its tab; closing the tab"
        );
        if Some(site.window) == self.main_window_id {
            self.close_tab_at(site.tab_index);
            // A window with no tabs is not a state the app should be able to
            // reach, and this is the same reaper the keymap's tab close uses.
            self.reap_empty_main_window_after_close();
            if let Some(window) = self.main_window() {
                crate::app::frame_counters::request_native_redraw(window);
            }
        } else {
            // When: `site.window` is a child, close through its child-local tab/window reaper.
            // Reaps its own window when the last tab goes.
            self.close_tab_at_in_child(site.window, site.tab_index);
        }
    }

    /// Find the window and tab holding `pane_id`.
    fn locate_exited_pane(&self, pane_id: u64) -> Option<ExitedPaneSite> {
        self.windows.iter().find_map(|(window_id, window)| {
            window.tab_states.iter().enumerate().find_map(|(tab_index, tab_state)| {
                let leaves = tab_state.tree.leaves();
                leaves.contains(&pane_id).then_some(ExitedPaneSite {
                    window: *window_id,
                    tab_index,
                    sole_leaf: leaves.len() == 1,
                })
            })
        })
    }
}

impl App {
    pub(super) fn close_pty_pane(&mut self, pane_id: u64) -> bool {
        let mut retired = None;
        let mut resize_main = false;
        let mut redraw_main = false;

        if let Some(main) = self.main_mut() {
            // When: `main_mut` resolves a window, so its tabs are searched for
            // the pane before any child window is considered.
            let active_tab = main.tabs.active_index();
            for (tab_idx, tab_state) in main.tab_states.iter_mut().enumerate() {
                let leaves = tab_state.tree.leaves();
                if !leaves.contains(&pane_id) {
                    // When: this tab's `leaves` exclude `pane_id`, so its split
                    // tree does not hold the pane being closed.
                    continue;
                }
                if leaves.len() > 1 && tab_state.tree.close(pane_id) {
                    if tab_state.active_pane == pane_id {
                        tab_state.active_pane = leaves
                            .into_iter()
                            .find(|id| *id != pane_id)
                            .unwrap_or(tab_state.active_pane);
                        // The search was scanning the grid that just went
                        // away. Its matches, their coordinates, and the
                        // revision it recorded all describe that grid.
                        if let Some(search) = tab_state.search.as_mut() {
                            search.invalidate_for_new_grid();
                        }
                    }
                    if tab_idx == active_tab {
                        resize_main = true;
                        redraw_main = true;
                    }
                }
                break;
            }
            retired = main.remove_pane(pane_id);
        }

        if resize_main {
            self.resize_visible_panes();
        }
        if redraw_main {
            if let Some(window) = self.main_window() {
                crate::app::frame_counters::request_native_redraw(window);
            }
        }
        if let Some(pane) = retired {
            // When: retired holds the main pane, transfer its PTY before returning without scanning child windows.
            self.retire_pane(pane);
            return true;
        }

        for child in self.windows.values_mut() {
            let mut resize_child = false;
            let mut redraw_child = false;
            let active_tab = child.tabs.active_index();
            for (tab_idx, tab_state) in child.tab_states.iter_mut().enumerate() {
                let leaves = tab_state.tree.leaves();
                if !leaves.contains(&pane_id) {
                    // When: this tab's `leaves` exclude `pane_id`, so this child's
                    // split tree does not hold the pane being closed.
                    continue;
                }
                if leaves.len() > 1 && tab_state.tree.close(pane_id) {
                    if tab_state.active_pane == pane_id {
                        tab_state.active_pane = leaves
                            .into_iter()
                            .find(|id| *id != pane_id)
                            .unwrap_or(tab_state.active_pane);
                        // The search was scanning the grid that just went
                        // away. Its matches, their coordinates, and the
                        // revision it recorded all describe that grid.
                        if let Some(search) = tab_state.search.as_mut() {
                            search.invalidate_for_new_grid();
                        }
                    }
                    if tab_idx == active_tab {
                        resize_child = true;
                        redraw_child = true;
                    }
                }
                break;
            }
            if let Some(pane) = child.remove_pane(pane_id) {
                // When: remove_pane returns custody, finish child layout before ending the window borrow and retiring its PTY.
                if resize_child {
                    child_window::resize_visible_panes_in_child(child);
                }
                if redraw_child {
                    child.request_window_redraw();
                }
                retired = Some(pane);
                break;
            }
        }

        if let Some(pane) = retired {
            self.retire_pane(pane);
            true
        } else {
            // When: retired is empty, no window owned the requested pane and no native teardown was submitted.
            false
        }
    }
}

#[cfg(test)]
#[path = "pane_exit_tests.rs"]
mod pane_exit_tests;
