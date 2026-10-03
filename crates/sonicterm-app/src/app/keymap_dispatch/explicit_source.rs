use sonicterm_cfg::keymap::{Action, Direction, ScrollAction};
use winit::window::WindowId;

use super::{read_only_allows_action, split_dir};
use crate::app::{App, FrontmostKind};

impl App {
    /// Route actions to their explicit live source window without falling back to cached focus.
    pub fn run_action_for_window(&mut self, action: &Action, source_window_id: WindowId) -> bool {
        if !self.windows.contains_key(&source_window_id) {
            // When: windows lacks source_window_id, refuse its action instead of falling through to another terminal.
            return false;
        }
        self.mark_window_redraw(source_window_id, super::redraw::RedrawCause::Input);
        let _ = self.clear_stale_frontmost();
        let source_kind = self.kind_for(source_window_id);
        if let FrontmostKind::Child(id) = source_kind {
            // When: source_kind is Child(id), renderer readiness must not redirect the action to main.
            if self
                .windows
                .get(&id)
                .is_some_and(|window| window.renderer.is_none() && window.window.is_some())
            {
                // When: a live child has not acquired rendering state, refuse its action without using the main window.
                return false;
            }
        }
        if self.run_field_clipboard_action(action, source_window_id) {
            // When: source_window_id's palette or search field owns copy or paste, consume it before READONLY or terminal routing.
            return true;
        }
        if self.read_only_active_for_kind(source_kind) && !read_only_allows_action(action) {
            // When: source_kind is READONLY and action is not allowed, consume it without dispatch.
            return true;
        }
        match action {
            Action::CopyToClipboard => self.copy_selection_for_kind(source_kind),
            Action::EnterCopyMode => self.enter_copy_mode_for_kind(source_kind),
            Action::EnterQuickSelect => self.enter_quick_select_for_kind(source_kind),
            Action::PasteFromClipboard => self.paste_clipboard_for_kind(source_kind),
            Action::ReloadConfig => self.force_reload_config(),
            Action::SaveCurrentSettings => self.save_current_settings_for_kind(source_kind),
            Action::NewTab => {
                // When: action is Action::NewTab, create a tab in the routed terminal window.
                self.observe_intent(sonicterm_app_core::AppIntent::NewTab {
                    window: sonicterm_types::WindowKey::new(0),
                    cwd: None,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    // When: source_kind is Child(id), a refused tab creation must not mutate main.
                    self.spawn_tab_in_child(id);
                    return true;
                }
                let tab_number = self.main_tabs().map(|tabs| tabs.len() + 1).unwrap_or(1);
                self.new_tab(format!("shell {tab_number}"));
            }
            Action::CloseTab => {
                // When: action is Action::CloseTab, close the routed window's active tab.
                let active_idx = self.main_tabs().map(|tabs| tabs.active_index()).unwrap_or(0);
                self.observe_intent(sonicterm_app_core::AppIntent::CloseTab {
                    window: sonicterm_types::WindowKey::new(0),
                    idx: active_idx,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    // When: `source_kind` names a child, a refused close must not remove a main-window tab.
                    self.close_active_tab_in_child(id);
                    return true;
                }
                let tab_index = self.main_tabs().map(|tabs| tabs.active_index()).unwrap_or(0);
                self.close_tab_at(tab_index);
                self.reap_empty_main_window_after_close();
            }
            Action::NextTab => {
                // When: action is Action::NextTab, activate the routed window's next tab.
                self.observe_intent(sonicterm_app_core::AppIntent::NextTab {
                    window: sonicterm_types::WindowKey::new(0),
                });
                if let FrontmostKind::Child(id) = source_kind {
                    // When: source_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.next_tab_in_child(id) {
                        // When: next_tab_in_child succeeds for id, the child fully consumed NextTab.
                        return true;
                    }
                }
                self.next_main_tab();
            }
            Action::PrevTab => {
                // When: action is Action::PrevTab, activate the routed window's previous tab.
                self.observe_intent(sonicterm_app_core::AppIntent::PrevTab {
                    window: sonicterm_types::WindowKey::new(0),
                });
                if let FrontmostKind::Child(id) = source_kind {
                    // When: source_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.prev_tab_in_child(id) {
                        // When: prev_tab_in_child succeeds for id, the child fully consumed PrevTab.
                        return true;
                    }
                }
                self.prev_main_tab();
            }
            Action::ActivateTab(tab_index) => {
                // When: action is Action::ActivateTab(tab_index), activate tab_index in the routed window.
                self.observe_intent(sonicterm_app_core::AppIntent::GoToTab {
                    window: sonicterm_types::WindowKey::new(0),
                    idx: *tab_index,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    // When: source_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.activate_tab_in_child(id, *tab_index) {
                        // When: activate_tab_in_child succeeds for id and tab_index, the child consumed ActivateTab.
                        return true;
                    }
                }
                self.activate_main_tab(*tab_index);
            }
            Action::ActivateLastTab => {
                // When: action is Action::ActivateLastTab, activate the routed window's final tab.
                if let FrontmostKind::Child(id) = source_kind {
                    // When: source_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.activate_last_tab_in_child(id) {
                        // When: activate_last_tab_in_child succeeds for id, the child consumed ActivateLastTab.
                        return true;
                    }
                }
                self.activate_last_main_tab();
            }
            Action::SplitRight => {
                // When: action is Action::SplitRight, split the routed active pane to the right.
                self.observe_intent(sonicterm_app_core::AppIntent::SplitPane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Right,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    // When: `source_kind` resolves a live child, even a refused split must not reach main.
                    self.split_active_pane_in_child(id, Direction::Right);
                    return true;
                }
                self.split_active(Direction::Right);
            }
            Action::SplitDown => {
                // When: action is Action::SplitDown, split the routed active pane downward.
                self.observe_intent(sonicterm_app_core::AppIntent::SplitPane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Down,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    // When: `source_kind` resolves a live child, even a refused split must not reach main.
                    self.split_active_pane_in_child(id, Direction::Down);
                    return true;
                }
                self.split_active(Direction::Down);
            }
            Action::ClosePane => {
                // When: action is Action::ClosePane, close the routed active pane.
                self.observe_intent(sonicterm_app_core::AppIntent::ClosePane {
                    window: sonicterm_types::WindowKey::new(0),
                });
                if let FrontmostKind::Child(id) = source_kind {
                    // When: `source_kind` names a child, a refused pane close must stay local to it.
                    self.close_active_pane_in_child(id);
                    return true;
                }
                self.close_active_pane();
            }
            Action::CloseActivePaneOrTab => {
                // When: action is Action::CloseActivePaneOrTab, close a split pane or its single-pane tab.
                if let FrontmostKind::Child(id) = source_kind {
                    // When: `source_kind` names a child, failed close validation cannot choose a peer target.
                    self.close_active_pane_or_tab_in_child(id);
                    return true;
                }
                let (tab_index, pane_count) = {
                    let main = self.main();
                    let tab_index = main.map(|window| window.tabs.active_index()).unwrap_or(0);
                    let pane_count = main
                        .and_then(|window| window.tab_states.get(tab_index))
                        .map(|tab| tab.tree.leaves().len())
                        .unwrap_or(0);
                    (tab_index, pane_count)
                };
                if pane_count > 1 {
                    self.close_active_pane();
                } else {
                    // When: pane_count is at most one, close the single-pane tab at tab_index.
                    self.close_tab_at(tab_index);
                }
                self.reap_empty_main_window_after_close();
            }
            Action::TogglePaneZoom => {
                // When: action is Action::TogglePaneZoom, toggle zoom in the routed active pane.
                if let FrontmostKind::Child(id) = source_kind {
                    // When: source_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.toggle_active_pane_zoom_in_child(id) {
                        // When: toggle_active_pane_zoom_in_child succeeds, the child consumed TogglePaneZoom.
                        return true;
                    }
                }
                self.toggle_active_pane_zoom();
            }
            Action::ToggleBroadcast { scope } => self.toggle_broadcast_for(source_kind, *scope),
            Action::FocusPane(direction) => {
                // When: action is Action::FocusPane(direction), move focus toward direction.
                let dir = match direction {
                    Direction::Left => sonicterm_app_core::SplitDir::Left,
                    Direction::Right => sonicterm_app_core::SplitDir::Right,
                    Direction::Up => sonicterm_app_core::SplitDir::Up,
                    Direction::Down => sonicterm_app_core::SplitDir::Down,
                };
                let wkey = sonicterm_types::WindowKey::new(0);
                let intent = match dir {
                    sonicterm_app_core::SplitDir::Left => {
                        sonicterm_app_core::AppIntent::FocusPaneLeft { window: wkey }
                    }
                    sonicterm_app_core::SplitDir::Right => {
                        sonicterm_app_core::AppIntent::FocusPaneRight { window: wkey }
                    }
                    sonicterm_app_core::SplitDir::Up => {
                        sonicterm_app_core::AppIntent::FocusPaneUp { window: wkey }
                    }
                    sonicterm_app_core::SplitDir::Down => {
                        sonicterm_app_core::AppIntent::FocusPaneDown { window: wkey }
                    }
                };
                self.observe_intent(intent);
                if let FrontmostKind::Child(id) = source_kind {
                    // When: source_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.focus_pane_dir_in_child(id, *direction) {
                        // When: focus_pane_dir_in_child succeeds for id and direction, the child consumed FocusPane.
                        return true;
                    }
                }
                self.focus_pane_dir(*direction);
            }
            Action::ResizePaneLeft => {
                // When: action is Action::ResizePaneLeft, grow the routed pane leftward.
                self.observe_intent(sonicterm_app_core::AppIntent::ResizePane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Left,
                    cells: 1,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    // When: source_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.resize_active_split_in_child(id, Direction::Left) {
                        // When: resize_active_split_in_child succeeds Left, the child consumed ResizePaneLeft.
                        return true;
                    }
                }
                self.resize_active_split(Direction::Left);
            }
            Action::ResizePaneRight => {
                // When: action is Action::ResizePaneRight, grow the routed pane rightward.
                self.observe_intent(sonicterm_app_core::AppIntent::ResizePane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Right,
                    cells: 1,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    // When: source_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.resize_active_split_in_child(id, Direction::Right) {
                        // When: resize_active_split_in_child succeeds Right, the child consumed ResizePaneRight.
                        return true;
                    }
                }
                self.resize_active_split(Direction::Right);
            }
            Action::ResizePaneUp => {
                // When: action is Action::ResizePaneUp, grow the routed pane upward.
                self.observe_intent(sonicterm_app_core::AppIntent::ResizePane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Up,
                    cells: 1,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    // When: source_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.resize_active_split_in_child(id, Direction::Up) {
                        // When: resize_active_split_in_child succeeds Up, the child consumed ResizePaneUp.
                        return true;
                    }
                }
                self.resize_active_split(Direction::Up);
            }
            Action::ResizePaneDown => {
                // When: action is Action::ResizePaneDown, grow the routed pane downward.
                self.observe_intent(sonicterm_app_core::AppIntent::ResizePane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Down,
                    cells: 1,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    // When: source_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.resize_active_split_in_child(id, Direction::Down) {
                        // When: resize_active_split_in_child succeeds Down, the child consumed ResizePaneDown.
                        return true;
                    }
                }
                self.resize_active_split(Direction::Down);
            }
            Action::ResizePane { dir, amount } => {
                // When: action is ResizePane with dir and amount, apply amount increments in dir.
                // ResizePane applies amount increments in dir.
                if *amount == 0 {
                    // When: amount is zero, consume ResizePane without changing the layout.
                    return true;
                }
                self.observe_intent(sonicterm_app_core::AppIntent::ResizePane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: split_dir(*dir),
                    cells: *amount,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    // Child sources receive every resize increment.
                    for _ in 0..*amount {
                        self.resize_active_split_in_child(id, *dir);
                    }
                } else {
                    // When: source_kind is not Child, resize the main active split.
                    for _ in 0..*amount {
                        self.resize_active_split(*dir);
                    }
                }
            }
            Action::MoveTabToNewWindow => {
                // MoveTabToNewWindow routes tear-out from source_window_id.
                // MoveTabToNewWindow queues the routed active tab for tear-out.
                if self.windows.contains_key(&source_window_id) {
                    // A registered source window can queue its active tab for tear-out.
                    self.queue_active_tab_tear_out(source_window_id);
                }
            }
            Action::Scroll(kind) => {
                // When: action is Scroll, resolve the live source pane instead of the observational reducer topology.
                let Some(pane) = self.active_pane_id_for_kind(source_kind) else {
                    // When: the explicit source has no active pane, scrolling cannot be redirected to another window.
                    return true;
                };
                let rows = self
                    .pane_by_id(pane)
                    .map(|pane| crate::app::frame_counters::lock_parser(&pane.parser).grid().rows)
                    .unwrap_or(1);
                let delta = match kind {
                    ScrollAction::LineUp => -1,
                    ScrollAction::LineDown => 1,
                    ScrollAction::PageUp => -i32::from(rows),
                    ScrollAction::PageDown => i32::from(rows),
                    ScrollAction::ToTop => i32::MIN,
                    ScrollAction::ToBottom => i32::MAX,
                };
                if let FrontmostKind::Child(id) = source_kind {
                    if let Some(window) = self.windows.get_mut(&id) {
                        super::child_window::scroll_child_pane(window, pane, delta);
                    }
                } else {
                    // When: source_kind is not Child, the resolved pane belongs to the main scrolling route.
                    self.scroll_pane(pane, delta);
                }
            }
            Action::ToggleFullscreen => self.toggle_fullscreen_for(source_kind),
            // Non-routed arms delegate to the cached-frontmost dispatcher.
            // Clipboard, theme, and config avoid window-local state; NewWindow
            // creates its own top level; search and palette use the main overlay.
            _ => {
                // When: a shared action consults focus, lend it the explicit source and restore the actual focus record afterwards.
                let previous = self.frontmost_window.replace(source_window_id);
                let handled = self.run_action(action);
                self.frontmost_window = previous.filter(|id| self.windows.contains_key(id));
                return handled;
            }
        }
        true
    }
}
