//! Pane split, retirement, focus, zoom and resize commands.

use super::*;

impl App {
    pub(in crate::app) fn split_active(&mut self, dir: Direction) {
        let Some(window) = self.main() else {
            // When: `main` is absent, no destination exists for a new pane or PTY.
            return;
        };
        let Some(tab) = window.tab_states.get(window.tabs.active_index()) else {
            // When: `tab_states` has no active tab, refuse before creating a speculative PTY.
            return;
        };
        if !tab.tree.leaves().contains(&tab.active_pane)
            || !window.panes.contains_key(&tab.active_pane)
        {
            // When: `active_pane` is not a live leaf, preserve topology without spawning another shell.
            return;
        }
        let launch =
            super::super::pane_launch::PaneLaunch::from_window(Some(window), &self.local_hostname);
        let new_id = next_pane_id();
        let mut new_pane = Some(self.spawn_pane(new_id, &launch));
        let did_split = 'install: {
            let Some(window) = self.main_mut() else {
                // When: main_mut has no destination, leave the spawned pane outside the window for retirement.
                break 'install false;
            };
            let tab_index = window.tabs.active_index();
            let split_ok = {
                let Some(tab_state) = window.tab_states.get_mut(tab_index) else {
                    // When: tab_states lacks tab_index, retain the uninstalled pane for the common retirement path.
                    break 'install false;
                };
                let focus = tab_state.active_pane;
                if tab_state.tree.split(focus, dir, new_id) {
                    tab_state.active_pane = new_id;
                    true
                } else {
                    // When: tree.split returns false, keep the existing pane layout and ownership.
                    false
                }
            };
            if split_ok {
                window.panes.insert(new_id, new_pane.take().expect("uninstalled split pane"));
            }
            split_ok
        };
        if let Some(pane) = new_pane {
            self.retire_pane(pane);
        }
        if did_split {
            // Own the new pane now rather than on the next 30-second sample:
            // until it has an owner its memory is attributed to nothing, and
            // anything reserving against it has no owner to reserve against.
            self.reconcile_pane_owners();
            self.resize_visible_panes();
            if let Some(renderer) = self.main_renderer_mut() {
                renderer.flash_pane_focus(new_id);
            }
            if let Some(main_window) = self.main_window() {
                crate::app::frame_counters::request_native_redraw(main_window);
            }
        }
    }

    pub(in crate::app) fn close_active_pane(&mut self) {
        let mut retired = None;
        let outcome = {
            let Some(window) = self.main_mut() else {
                // When: main_mut returns None, there is no active window pane to close.
                return;
            };
            let tab_index = window.tabs.active_index();
            let inner = {
                let Some(tab_state) = window.tab_states.get_mut(tab_index) else {
                    // When: tab_states.get_mut cannot find tab_index, there is no active pane tree to close.
                    return;
                };
                let focus = tab_state.active_pane;
                if matches!(tab_state.tree, PaneTree::Leaf { id, .. } if id == focus) {
                    (Some(tab_index), None)
                } else {
                    // When: matches finds no focused PaneTree::Leaf, close only the split pane.
                    let new_focus = tab_state
                        .tree
                        .leaves()
                        .into_iter()
                        .find(|id| *id != focus)
                        .unwrap_or(focus);
                    if tab_state.tree.close(focus) {
                        // A successful tree close activates its surviving sibling.
                        tab_state.active_pane = new_focus;
                        // Same reason as the exit-driven path: the search was
                        // scanning the grid that just went away.
                        if let Some(search) = tab_state.search.as_mut() {
                            search.invalidate_for_new_grid();
                        }
                        (None, Some(focus))
                    } else {
                        // When: tree.close returns false, preserve the pane and tab unchanged.
                        (None, None)
                    }
                }
            };
            if let (_, Some(focus)) = inner {
                retired = window.remove_pane(focus);
            }
            inner
        };
        if let Some(pane) = retired {
            self.retire_pane(pane);
        }
        match outcome {
            (Some(tab_index), _) => self.close_tab_at(tab_index),
            (_, Some(_focus)) => {
                // the surviving sibling's PaneRect just grew to cover
                // the closed pane's area. Push the new layout into its Grid
                // + PtyHandle (matches split / zoom / resize-split paths and
                // mirrors `close_active_pane_in_child`). Without this the
                // survivor keeps its narrow split-time column count and
                // shell output wraps at the old width until the OS window
                // is resized. The actual resize is delegated to
                // `resize_visible_panes` which routes through the pure
                // helper `resize_panes_to_rects` — the path tested by
                // `close_sibling_pane_resizes_survivor_to_full_width` in
                // `crates/sonicterm-app/tests/per_pane_resize.rs`.
                self.resize_visible_panes();
                if let Some(active_id) = self.active_pane_id() {
                    if let Some(renderer) = self.main_renderer_mut() {
                        renderer.flash_pane_focus(active_id);
                    }
                }
                if let Some(main_window) = self.main_window() {
                    crate::app::frame_counters::request_native_redraw(main_window);
                }
            }
            _ => {
                // When: outcome closes neither a tab nor a pane, no layout update is required.
            }
        }
    }

    pub(in crate::app) fn focus_pane_dir(&mut self, dir: Direction) {
        let Some(window) = self.main_mut() else {
            // When: main_mut returns None, there is no pane focus to move.
            return;
        };
        let tab_idx = window.tabs.active_index();
        let Some(next) = window
            .tab_states
            .get(tab_idx)
            .and_then(|tab| tab.tree.focus_neighbor(tab.active_pane, dir))
        else {
            // When: the active tab has no pane in `dir`, focus stays unchanged.
            return;
        };
        if let Some(change) = window.begin_pane_focus_change(next) {
            window.finish_pane_focus_change(change);
        }
    }

    pub(in crate::app) fn toggle_active_pane_zoom(&mut self) {
        let toggled = {
            let Some(window) = self.main_mut() else {
                // When: main_mut returns None, there is no pane zoom state to toggle.
                return;
            };
            let tab_index = window.tabs.active_index();
            let Some(tab_state) = window.tab_states.get_mut(tab_index) else {
                // When: tab_states.get_mut cannot find tab_index, there is no active pane tree to zoom.
                return;
            };
            tab_state.tree.toggle_zoom(tab_state.active_pane)
        };
        if toggled {
            self.resize_visible_panes();
            if let Some(main_window) = self.main_window() {
                crate::app::frame_counters::request_native_redraw(main_window);
            }
        }
    }

    pub(in crate::app) fn toggle_broadcast(
        &mut self,
        scope: sonicterm_cfg::keymap::BroadcastScope,
    ) {
        self.toggle_broadcast_for(self.frontmost_kind(), scope);
    }

    pub(in crate::app) fn toggle_broadcast_for(
        &mut self,
        kind: FrontmostKind,
        scope: sonicterm_cfg::keymap::BroadcastScope,
    ) {
        let Some(source_pane) = self.active_pane_id_for_kind(kind) else {
            // When: active_pane_id_for_kind returns None, no pane can source the broadcast.
            return;
        };
        self.broadcast = self.broadcast.toggled(scope, source_pane);
        self.request_redraw_all_terminal_windows();
    }

    pub(in crate::app) fn resize_active_split(&mut self, dir: Direction) {
        let resized = {
            let Some(window) = self.main_mut() else {
                // When: main_mut returns None, there is no active split to resize.
                return;
            };
            let tab_index = window.tabs.active_index();
            let Some(tab_state) = window.tab_states.get_mut(tab_index) else {
                // When: tab_states.get_mut cannot find tab_index, there is no active split tree.
                return;
            };
            tab_state.tree.resize_split(tab_state.active_pane, dir, 0.05)
        };
        if resized {
            self.resize_visible_panes();
            if let Some(main_window) = self.main_window() {
                crate::app::frame_counters::request_native_redraw(main_window);
            }
        }
    }

    pub(in crate::app) fn resize_visible_panes(&mut self) {
        let viewport = self.test_viewport_override;
        if let Some(window) = self.main_mut() {
            window.complete_topology_change(
                super::super::TopologyChange {
                    resize_visible: true,
                    focus_feedback: None,
                    dirt: super::super::window_state::TopologyDirt::Window,
                },
                viewport,
            );
        }
    }
}

#[cfg(test)]
#[path = "spawn_pane_commands_tests.rs"]
mod spawn_pane_commands_tests;
