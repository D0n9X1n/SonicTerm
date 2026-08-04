//! Pane teardown when a pane's child process ends.
//!
//! A shell that exits leaves its pane holding a dead grid. Closing that pane
//! is what a user typing `exit` means, but it also destroys their scrollback,
//! so it happens only on evidence that the exit was clean. An unclean exit —
//! or one whose status could not be read — leaves the pane on screen with
//! whatever killed it still legible.

use winit::window::WindowId;

use super::App;

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
            if let Some(w) = self.main_window() {
                w.request_redraw();
            }
        } else {
            // When: `site.window` is a child, close through its child-local tab/window reaper.
            // Reaps its own window when the last tab goes.
            self.close_tab_at_in_child(site.window, site.tab_index);
        }
    }

    /// Find the window and tab holding `pane_id`.
    fn locate_exited_pane(&self, pane_id: u64) -> Option<ExitedPaneSite> {
        self.windows.iter().find_map(|(window_id, ws)| {
            ws.tab_states.iter().enumerate().find_map(|(tab_index, st)| {
                let leaves = st.tree.leaves();
                leaves.contains(&pane_id).then_some(ExitedPaneSite {
                    window: *window_id,
                    tab_index,
                    sole_leaf: leaves.len() == 1,
                })
            })
        })
    }
}

#[cfg(test)]
#[path = "pane_exit_tests.rs"]
mod pane_exit_tests;
