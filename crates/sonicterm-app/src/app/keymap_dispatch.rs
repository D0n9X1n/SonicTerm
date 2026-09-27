//! Extracted from `app/mod.rs` from the monolithic app module.
//! `App`'s referenced fields are `pub(super)`; this submodule lives in
//! the same `app` module tree, so direct field access works.

#![allow(unused_imports)]

use std::collections::HashMap;
use std::sync::{atomic::Ordering, Arc};
use std::time::{Duration, Instant};

use super::config_apply::{WEIGHT_SCALE_MAX, WEIGHT_SCALE_MIN};
use anyhow::Context;
use parking_lot::Mutex;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Action, Direction, Keymap, ScrollAction};
use sonicterm_cfg::theme::Theme;
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_grid::grid::Grid;
use sonicterm_io::pty::PtyHandle;
use sonicterm_ui::pane::PaneTree;
use sonicterm_ui::selection::Selection;
use sonicterm_ui::tabbar_view::{TabBarLayout, TabHit};
use sonicterm_ui::tabs::{Tab, TabBar};
use sonicterm_vt::vt::{Parser, VtEvent};
use winit::{
    event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowAttributes, WindowId},
};

use super::{child_window, redraw};
use super::{
    key_encoding::{encode_key, encode_logical, key_event_to_string, key_name},
    mark_all_panes_dirty, next_pane_id, pick_prompt_target, resize_all_panes, shell_quote_posix,
    with_integrated_titlebar, wrap_paste, App, FrontmostKind, PaneState, TabState, UserEvent,
    WindowState,
};

mod explicit_source;
mod notifications;

pub(super) fn read_only_allows_action(action: &Action) -> bool {
    sonicterm_ui::command_label::descriptor(action).read_only_allowed
}

pub(super) fn terminal_input_passthrough_binding(key_str: &str, action: &Action) -> bool {
    cfg!(target_os = "windows")
        && key_str == "alt+v"
        && matches!(action, Action::PasteFromClipboard)
}

/// One palette/keymap step of monochrome-text weight. Four steps span
/// 1.0 -> 2.0, so a useful weight is a few presses away while the full
/// 0.5..=5.0 range stays reachable.
const FONT_WEIGHT_STEP: f32 = 0.25;

const _: () = {
    // Keep the step meaningful relative to the range it moves through.
    assert!(FONT_WEIGHT_STEP > 0.0);
    assert!(FONT_WEIGHT_STEP < WEIGHT_SCALE_MAX - WEIGHT_SCALE_MIN);
};

impl App {
    /// Arm or confirm keyboard quit on the live source window without changing frontmost routing.
    pub(super) fn on_quit_chord_pressed(&mut self, win_id: WindowId, is_repeat: bool) -> bool {
        if !self.windows.contains_key(&win_id) {
            // When: win_id is stale, it cannot arm a quit prompt on another window.
            return false;
        }
        let now = Instant::now();
        match self.quit_hold.on_press(now, is_repeat) {
            super::quit_hold::QuitHoldAction::ShowPrompt { .. } => {
                self.show_notification_for_kind(
                    self.kind_for(win_id),
                    sonicterm_ui::overlays::NotificationLevel::Error,
                    super::quit_hold::QUIT_CONFIRM_PROMPT.to_string(),
                );
            }
            super::quit_hold::QuitHoldAction::None => {
                // When: on_press returns QuitHoldAction::None, leave the current quit guard unchanged.
            }
            super::quit_hold::QuitHoldAction::Quit => {
                self.pending_exit = true;
            }
        }
        true
    }

    /// Timer tick for the quit confirmation guard. This expires stale first
    /// presses; the central notification expiry clears the visible prompt.
    pub(super) fn expire_quit_confirmation(&mut self) {
        let _ = self.quit_hold.on_tick(Instant::now());
    }

    /// Save the current font settings to `path` and report the result on
    /// `kind`'s notification surface.
    pub(super) fn save_current_settings_to_for_kind(
        &mut self,
        path: &std::path::Path,
        kind: FrontmostKind,
    ) {
        match self.save_current_settings_to(path) {
            Ok(()) => {
                tracing::info!(path = %path.display(), "saved current font settings");
                self.show_notification_for_kind(
                    kind,
                    sonicterm_ui::overlays::NotificationLevel::Info,
                    "Current font settings saved".to_string(),
                );
            }
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %format_args!("{error:#}"),
                    "unable to save current font settings"
                );
                self.show_notification_for_kind(
                    kind,
                    sonicterm_ui::overlays::NotificationLevel::Error,
                    "Unable to save current font settings; existing config unchanged".to_string(),
                );
            }
        }
    }

    /// Resolve the production config path, save, and route its notification.
    pub(super) fn save_current_settings_for_kind(&mut self, kind: FrontmostKind) {
        let path = match self.current_settings_path() {
            Ok(path) => path,
            Err(error) => {
                // When: current_settings_path cannot resolve the user's config
                // location, report failure on the source without mutating state.
                tracing::warn!(
                    path = "<unresolved>",
                    error = %format_args!("{error:#}"),
                    "unable to save current font settings"
                );
                self.show_notification_for_kind(
                    kind,
                    sonicterm_ui::overlays::NotificationLevel::Error,
                    "Unable to save current font settings; existing config unchanged".to_string(),
                );
                return;
            }
        };
        self.save_current_settings_to_for_kind(&path, kind);
    }

    pub(super) fn start_update_check_for_kind(&mut self, kind: FrontmostKind) {
        self.show_notification_for_kind_until(
            kind,
            sonicterm_ui::overlays::NotificationLevel::Warning,
            "Checking for updates…".to_string(),
            None,
        );
        let Some(proxy) = self.event_loop_proxy.clone() else {
            // When: event_loop_proxy is None, replace the progress bubble with an error.
            self.show_notification_for_kind(
                kind,
                sonicterm_ui::overlays::NotificationLevel::Error,
                "Unable to check updates".to_string(),
            );
            return;
        };
        std::thread::spawn(move || {
            let result = crate::app::update_check::check_latest_release(env!("CARGO_PKG_VERSION"));
            let (level, message) = match result {
                crate::app::update_check::UpdateCheckResult::Newer { tag, .. } => (
                    sonicterm_ui::overlays::NotificationLevel::Warning,
                    format!("Update available: {tag}"),
                ),
                crate::app::update_check::UpdateCheckResult::UpToDate => (
                    sonicterm_ui::overlays::NotificationLevel::Info,
                    "SonicTerm is up to date".to_string(),
                ),
                crate::app::update_check::UpdateCheckResult::Unavailable => (
                    sonicterm_ui::overlays::NotificationLevel::Error,
                    "Unable to check updates".to_string(),
                ),
            };
            let _ = proxy.send_event(UserEvent::UpdateCheckFinished { level, message });
        });
    }

    fn read_only_active_for_kind(&self, kind: FrontmostKind) -> bool {
        match kind {
            FrontmostKind::Main => self
                .main()
                .and_then(|ws| ws.copy_mode.as_ref())
                .is_some_and(|mode| mode.is_read_only()),
            FrontmostKind::Child(id) => self
                .windows
                .get(&id)
                .and_then(|ws| ws.copy_mode.as_ref())
                .is_some_and(|mode| mode.is_read_only()),
            FrontmostKind::None | FrontmostKind::Other => false,
        }
    }

    /// Dispatch a menu action with window-local rename and search paste preceding READONLY refusal.
    pub fn run_action(&mut self, action: &Action) -> bool {
        // if `frontmost_window` was set to a stale id
        // (window closed between focus event + this dispatch), clear it
        // now so the routing arms below see `None` (safe main fallback)
        // AND the next action doesn't retry the dead window. This single
        // up-front check covers every routed arm.
        let _ = self.clear_stale_frontmost();
        if matches!(action, Action::PasteFromClipboard)
            && self.paste_window_name_for_kind(self.frontmost_kind())
        {
            // When: paste_window_name_for_kind consumes paste, neither search nor READONLY may redirect it.
            return true;
        }
        if matches!(action, Action::PasteFromClipboard)
            && self.search_paste_window_for_kind(self.frontmost_kind()).is_some()
        {
            // When: search_paste_window_for_kind resolves an editor for frontmost_kind, it owns paste even in READONLY.
            self.paste_clipboard_for_kind(self.frontmost_kind());
            return true;
        }
        if self.read_only_active_for_kind(self.frontmost_kind()) && !read_only_allows_action(action)
        {
            // When: READONLY is active and read_only_allows_action rejects action, consume it safely.
            return true;
        }
        match action {
            Action::CopyToClipboard => self.copy_selection_for_kind(self.frontmost_kind()),
            Action::EnterCopyMode => self.enter_copy_mode_for_kind(self.frontmost_kind()),
            Action::EnterQuickSelect => self.enter_quick_select(),
            Action::PasteFromClipboard => self.paste_clipboard_for_kind(self.frontmost_kind()),
            Action::ReloadConfig => self.force_reload_config(),
            Action::NewTab => {
                // When: action is Action::NewTab, create a tab in the routed terminal window.

                // Notify the reducer before creating the tab. It bumps tab_count,
                // sets active_tab_idx, and emits Render(TabAdded).
                // Boundary below remains source-of-truth for the
                // actual tab spawn (it owns the PtyHandle/Grid/Parser
                // tree that the renderer paints).
                self.observe_intent(sonicterm_app_core::AppIntent::NewTab {
                    window: sonicterm_types::WindowKey::new(0),
                    cwd: None,
                });
                // route through the unified
                // `frontmost_window` discriminator so a Cmd+T typed in a
                // torn-out child opens a tab in THAT child, not in the
                // main window. `frontmost_window` subsumed the `focused_child`
                // fallback — `frontmost_window` is set by the same focus
                // event so the back-compat path was redundant.
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    // When: frontmost_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.spawn_tab_in_child(id) {
                        // When: spawn_tab_in_child succeeds for id, the child fully consumed NewTab.
                        return true;
                    }
                    // Child vanished between focus and dispatch — clear
                    // tracker and fall through.
                    self.frontmost_window = None;
                }
                let n = self.main_tabs().map(|t| t.len() + 1).unwrap_or(1);
                self.new_tab(format!("shell {n}"));
            }
            Action::CloseTab => {
                // When: action is Action::CloseTab, close the routed window's active tab.

                // Notify the reducer first so tab_count and active_tab_idx stay in sync.
                let active_idx = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
                self.observe_intent(sonicterm_app_core::AppIntent::CloseTab {
                    window: sonicterm_types::WindowKey::new(0),
                    idx: active_idx,
                });
                // route to frontmost window.
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    // When: frontmost_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.close_active_tab_in_child(id) {
                        // When: close_active_tab_in_child succeeds for id, the child fully consumed CloseTab.
                        return true;
                    }
                    self.frontmost_window = None;
                }
                let i = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
                self.close_tab_at(i);
                self.reap_empty_main_window_after_close();
            }
            Action::NextTab => {
                // When: action is Action::NextTab, activate the routed window's next tab.
                self.observe_intent(sonicterm_app_core::AppIntent::NextTab {
                    window: sonicterm_types::WindowKey::new(0),
                });
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    // When: frontmost_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.next_tab_in_child(id) {
                        // When: next_tab_in_child succeeds for id, the child fully consumed NextTab.
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.next_main_tab();
            }
            Action::PrevTab => {
                // When: action is Action::PrevTab, activate the routed window's previous tab.
                self.observe_intent(sonicterm_app_core::AppIntent::PrevTab {
                    window: sonicterm_types::WindowKey::new(0),
                });
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    // When: frontmost_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.prev_tab_in_child(id) {
                        // When: prev_tab_in_child succeeds for id, the child fully consumed PrevTab.
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.prev_main_tab();
            }
            Action::ActivateTab(i) => {
                // When: action is Action::ActivateTab(i), activate index i in the routed window.
                self.observe_intent(sonicterm_app_core::AppIntent::GoToTab {
                    window: sonicterm_types::WindowKey::new(0),
                    idx: *i,
                });
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    // When: frontmost_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.activate_tab_in_child(id, *i) {
                        // When: activate_tab_in_child succeeds for id and i, the child consumed ActivateTab.
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.activate_main_tab(*i);
            }
            Action::ActivateLastTab => {
                // When: action is Action::ActivateLastTab, activate the routed window's final tab.
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    // When: frontmost_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.activate_last_tab_in_child(id) {
                        // When: activate_last_tab_in_child succeeds for id, the child consumed ActivateLastTab.
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.activate_last_main_tab();
            }
            Action::SplitRight => {
                // When: action is Action::SplitRight, split the routed active pane to the right.

                // route to frontmost window so Cmd+D
                // typed in a torn-out child splits THAT window's active
                // pane, not the main window's.
                // Notify the reducer first so pane_count and focused_pane_idx
                // track the topology;
                // the boundary's `split_active*` remains source-of-truth
                // for actual geometry.
                self.observe_intent(sonicterm_app_core::AppIntent::SplitPane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Right,
                });
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    // When: `frontmost_kind` resolves a live child, even a refused split must not reach main.
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
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    // When: `frontmost_kind` resolves a live child, even a refused split must not reach main.
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
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    // When: frontmost_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.close_active_pane_in_child(id) {
                        // When: close_active_pane_in_child succeeds for id, the child consumed ClosePane.
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.close_active_pane();
            }
            Action::CloseActivePaneOrTab => {
                // When: action is Action::CloseActivePaneOrTab, close a split pane or its single-pane tab.

                // Cmd+W routes to frontmost window.
                // Without this, a Cmd+W typed in a torn-out child window
                // closed a tab in the original main window (bug #3).
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    // When: frontmost_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.close_active_pane_or_tab_in_child(id) {
                        // When: close_active_pane_or_tab_in_child succeeds, the child consumed the close.
                        return true;
                    }
                    self.frontmost_window = None;
                }
                // iTerm2/wezterm-style Cmd+W: when the active tab has more
                // than one pane, close just the focused pane; otherwise
                // close the whole tab. `close_active_pane` already folds
                // the "last pane → close tab" case internally, so a single
                // call covers both branches and the pane-count check below
                // is purely documentation of intent. The explicit branch
                // also keeps the dispatcher honest if `close_active_pane`
                // ever changes its fall-through.
                let (i, pane_count) = {
                    let ws = self.main();
                    let i = ws.map(|w| w.tabs.active_index()).unwrap_or(0);
                    let pc = ws
                        .and_then(|w| w.tab_states.get(i))
                        .map(|st| st.tree.leaves().len())
                        .unwrap_or(0);
                    (i, pc)
                };
                if pane_count > 1 {
                    self.close_active_pane();
                } else {
                    // When: pane_count is at most one, close the single-pane tab at i.
                    self.close_tab_at(i);
                }
                // Unified reap path: if the main window's tabs vec is
                // now empty, either hide it (Chrome-style) or set the
                // deferred-exit flag (traditional terminal-style).
                // `do_about_to_wait` drains `pending_exit` against the
                // live `ActiveEventLoop`. Mirrors the mouse close-button
                // path in `window_event.rs` (~line 637).
                self.reap_empty_main_window_after_close();
            }
            Action::TogglePaneZoom => {
                // When: action is Action::TogglePaneZoom, toggle zoom in the routed active pane.
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    // When: frontmost_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.toggle_active_pane_zoom_in_child(id) {
                        // When: toggle_active_pane_zoom_in_child succeeds, the child consumed TogglePaneZoom.
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.toggle_active_pane_zoom();
            }
            Action::ToggleBroadcast { scope } => self.toggle_broadcast(*scope),
            Action::FocusPane(d) => {
                // When: action is Action::FocusPane(d), move focus in direction d.

                // Notify the reducer; it emits Render(Focus) when pane_count
                // is at least two and otherwise leaves focus unchanged.
                let dir = match d {
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
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    // When: frontmost_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.focus_pane_dir_in_child(id, *d) {
                        // When: focus_pane_dir_in_child succeeds for id and d, the child consumed FocusPane.
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.focus_pane_dir(*d);
            }
            Action::ResizePaneLeft => {
                // When: action is Action::ResizePaneLeft, grow the routed pane leftward.
                self.observe_intent(sonicterm_app_core::AppIntent::ResizePane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Left,
                    cells: 1,
                });
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    // When: frontmost_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.resize_active_split_in_child(id, Direction::Left) {
                        // When: resize_active_split_in_child succeeds Left, the child consumed ResizePaneLeft.
                        return true;
                    }
                    self.frontmost_window = None;
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
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    // When: frontmost_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.resize_active_split_in_child(id, Direction::Right) {
                        // When: resize_active_split_in_child succeeds Right, the child consumed ResizePaneRight.
                        return true;
                    }
                    self.frontmost_window = None;
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
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    // When: frontmost_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.resize_active_split_in_child(id, Direction::Up) {
                        // When: resize_active_split_in_child succeeds Up, the child consumed ResizePaneUp.
                        return true;
                    }
                    self.frontmost_window = None;
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
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    // When: frontmost_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.resize_active_split_in_child(id, Direction::Down) {
                        // When: resize_active_split_in_child succeeds Down, the child consumed ResizePaneDown.
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.resize_active_split(Direction::Down);
            }
            Action::OpenSearch => {
                // When: action is Action::OpenSearch, open search in the routed terminal window.

                // Route to the frontmost child window so Cmd+F opens search in a
                // torn-out window instead of the main one. (#pane-search)
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    // When: frontmost_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.open_search_in_child(id) {
                        // When: open_search_in_child succeeds for id, the child consumed OpenSearch.
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.open_search();
            }
            Action::EditConfigFile => self.open_config_file(),
            Action::OpenKeymapFile => self.open_keymap_file(),
            Action::CheckForUpdates => self.start_update_check_for_kind(self.frontmost_kind()),
            Action::SaveCurrentSettings => {
                self.save_current_settings_for_kind(self.frontmost_kind())
            }
            Action::OpenCommandPalette => self.toggle_command_palette(),
            Action::ScrollToPrevPrompt => self.scroll_to_prompt(false),
            Action::ScrollToNextPrompt => self.scroll_to_prompt(true),
            Action::IncreaseFontSize => self.change_font_size(1.0),
            Action::DecreaseFontSize => self.change_font_size(-1.0),
            Action::ResetFontSize => self.reset_font_size(),
            Action::IncreaseFontWeight => self.change_font_weight(FONT_WEIGHT_STEP),
            Action::DecreaseFontWeight => self.change_font_weight(-FONT_WEIGHT_STEP),
            Action::ResetFontWeight => self.reset_font_weight(),
            Action::ApplyTheme(name) => self.apply_theme_by_name(name),
            Action::ToggleTabBar => self.toggle_tab_bar(),
            Action::RenameTab => self.start_rename_active_tab(),
            Action::RenameWindow => {
                let target = match self.frontmost_kind() {
                    FrontmostKind::Child(id) => Some(id),
                    _ => self.main_window_id,
                };
                if let Some(id) = target {
                    self.start_rename_window(id);
                }
            }
            Action::UpdateTabColor => self.start_update_tab_color(),
            Action::NewWindow => {
                let source = match self.frontmost_kind() {
                    FrontmostKind::Child(id) => Some(id),
                    FrontmostKind::Main => self.main_window_id,
                    FrontmostKind::None | FrontmostKind::Other => None,
                };
                self.pending_new_window = Some(self.window_request(source));
                // Notify the reducer that a new window was requested. It bumps
                // `live_window_count` and emits a `WindowOpen` Effect
                // (currently trace-stubbed in `dispatch_effects`; the
                // production `drain_pending_window_creates` boundary
                // above remains the source of truth for actually
                // building the platform surface).
                self.observe_intent(sonicterm_app_core::AppIntent::NewWindow {
                    role: sonicterm_app_core::WindowRole::Primary,
                });
            }
            Action::MoveTabToNewWindow => {
                // MoveTabToNewWindow resolves the active tab's source window.
                // MoveTabToNewWindow queues the routed active tab for tear-out.
                let source_window = match self.frontmost_kind() {
                    FrontmostKind::Child(id) => Some(id),
                    FrontmostKind::Main | FrontmostKind::None | FrontmostKind::Other => {
                        self.main_window_id
                    }
                };
                if let Some(source_window) = source_window {
                    self.queue_active_tab_tear_out(source_window);
                }
            }
            Action::Scroll(kind) => {
                // When: action is Action::Scroll(kind), translate kind into a signed pane delta.

                // replace the "not yet wired up" stub. Translate
                // ScrollAction → signed line delta and route through the
                // canonical `scroll_pane` mutator (which also handles
                // alt-screen no-op + clamping + auto-follow snap-back).
                let Some(pane_id) = self.active_pane_id() else {
                    // When: active_pane_id returns None, consume Scroll without changing a viewport.
                    return true;
                };
                let viewport_rows = self.active_pane_viewport_rows().unwrap_or(24);
                let delta: i32 = match kind {
                    ScrollAction::LineUp => -1,
                    ScrollAction::LineDown => 1,
                    ScrollAction::PageUp => -(viewport_rows as i32),
                    ScrollAction::PageDown => viewport_rows as i32,
                    ScrollAction::ToTop => i32::MIN,
                    ScrollAction::ToBottom => i32::MAX,
                };
                self.scroll_pane(pane_id, delta);
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
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    // When: frontmost_kind is FrontmostKind::Child(id), route the action to that child.
                    let mut routed = false;
                    for _ in 0..*amount {
                        routed = self.resize_active_split_in_child(id, *dir) || routed;
                    }
                    if routed {
                        // When: routed is true, at least one child resize consumed the action.
                        return true;
                    }
                    self.frontmost_window = None;
                }
                for _ in 0..*amount {
                    self.resize_active_split(*dir);
                }
            }
            Action::ToggleFullscreen => {
                self.toggle_fullscreen_for(self.frontmost_kind());
            }
            Action::QuitApp => {
                // Explicit (menu / command palette) invocation quits
                // immediately — the hold gate applies only to the keyboard
                // chord, which is intercepted before it reaches here.
                // `do_about_to_wait` drains `pending_exit` and calls
                // `el.exit()` on the next loop turn.
                self.quit_hold = super::quit_hold::QuitHold::new();
                self.pending_exit = true;
            }
        }
        true
    }

    /// Classify an explicit window id (rather than `self.frontmost_window`).
    /// Mirrors [`Self::frontmost_kind`] but takes the id from the caller —
    /// used by [`Self::run_action_for_window`] to route a keyboard chord
    /// to the window that produced it.
    pub(super) fn kind_for(&self, id: WindowId) -> FrontmostKind {
        if self.main_window_id == Some(id) && self.windows.contains_key(&id) {
            // When: main_window_id matches a live windows entry, native handle readiness cannot change its routing identity.
            return FrontmostKind::Main;
        }
        if self.windows.contains_key(&id) {
            // When: windows contains id, classify the explicit source as Child.
            return FrontmostKind::Child(id);
        }
        FrontmostKind::None
    }

    fn toggle_fullscreen_for(&mut self, kind: FrontmostKind) {
        if let FrontmostKind::Child(id) = kind {
            // When: kind is FrontmostKind::Child(id), toggle that window before falling back.
            if let Some(window) = self.windows.get(&id).and_then(|child| child.window.as_ref()) {
                // When: child.window is Some(window), toggle it and finish child routing.
                toggle_window_fullscreen(window);
                return;
            }
            self.frontmost_window = None;
        }
        if let Some(window) = self.main_window() {
            // The main fallback toggles its available window.
            toggle_window_fullscreen(window);
        }
    }
}

fn split_dir(dir: Direction) -> sonicterm_app_core::SplitDir {
    match dir {
        Direction::Left => sonicterm_app_core::SplitDir::Left,
        Direction::Right => sonicterm_app_core::SplitDir::Right,
        Direction::Up => sonicterm_app_core::SplitDir::Up,
        Direction::Down => sonicterm_app_core::SplitDir::Down,
    }
}

fn toggle_window_fullscreen(window: &Window) {
    if window.fullscreen().is_some() {
        window.set_fullscreen(None);
    } else {
        // When: window.fullscreen is None, enter borderless fullscreen.
        window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
    }
}

#[cfg(test)]
#[path = "keymap_dispatch_tests.rs"]
mod keymap_dispatch_tests;
