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

use super::{
    key_encoding::{encode_key, encode_logical, key_event_to_string, key_name},
    mark_all_panes_dirty, next_pane_id, pick_prompt_target, resize_all_panes, shell_quote_posix,
    with_integrated_titlebar, wrap_paste, App, FrontmostKind, PaneState, TabState, UserEvent,
    WindowState,
};

const NOTIFICATION_AUTO_CLOSE_DURATION: Duration = Duration::from_secs(5);

pub(super) fn read_only_allows_action(action: &Action) -> bool {
    matches!(
        action,
        Action::NextTab
            | Action::PrevTab
            | Action::ActivateTab(_)
            | Action::ActivateLastTab
            | Action::FocusPane(_)
            | Action::OpenSearch
            | Action::CheckForUpdates
    )
}

pub(super) fn terminal_input_passthrough_binding(key_str: &str, action: &Action) -> bool {
    cfg!(target_os = "windows")
        && key_str == "alt+v"
        && matches!(action, Action::PasteFromClipboard)
}

/// One palette/keymap step of regular-text weight. Four steps span
/// 1.0 -> 2.0, so a useful weight is a few presses away while the full
/// 0.5..=5.0 range stays reachable.
const FONT_WEIGHT_STEP: f32 = 0.25;

const _: () = {
    // Keep the step meaningful relative to the range it moves through.
    assert!(FONT_WEIGHT_STEP > 0.0);
    assert!(FONT_WEIGHT_STEP < WEIGHT_SCALE_MAX - WEIGHT_SCALE_MIN);
};

impl App {
    /// Handle a Cmd+Q chord press from the keyboard path. First press arms the
    /// quit confirmation guard and surfaces the red prompt; a second non-repeat
    /// press quits. Returns `true` when the chord was consumed (so the caller
    /// does not forward it as a normal action or to the PTY).
    pub(super) fn on_quit_chord_pressed(&mut self, is_repeat: bool) -> bool {
        let now = Instant::now();
        match self.quit_hold.on_press(now, is_repeat) {
            super::quit_hold::QuitHoldAction::ShowPrompt { .. } => {
                self.show_notification_for_kind(
                    self.frontmost_kind(),
                    sonicterm_ui::overlays::NotificationLevel::Error,
                    super::quit_hold::QUIT_CONFIRM_PROMPT.to_string(),
                );
                if let Some(w) = self.main_window() {
                    w.request_redraw();
                }
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

    pub(super) fn show_notification_for_kind(
        &mut self,
        kind: FrontmostKind,
        level: sonicterm_ui::overlays::NotificationLevel,
        message: String,
    ) {
        self.show_notification_for_kind_until(kind, level, message, None);
    }

    pub(super) fn show_notification_for_kind_until(
        &mut self,
        kind: FrontmostKind,
        level: sonicterm_ui::overlays::NotificationLevel,
        message: String,
        expires_at: Option<std::time::Instant>,
    ) {
        let auto_close_at = std::time::Instant::now() + NOTIFICATION_AUTO_CLOSE_DURATION;
        let expires_at = Some(expires_at.map_or(auto_close_at, |at| at.min(auto_close_at)));
        let bubble = sonicterm_ui::overlays::NotificationBubble { level, message, expires_at };
        match kind {
            FrontmostKind::Child(id) => {
                // When: kind is FrontmostKind::Child(id), prefer that child's notification surface.
                if let Some(child) = self.windows.get_mut(&id) {
                    // When: windows.get_mut finds id, install the bubble and finish child routing.
                    child.notification = Some(bubble);
                    child.request_redraw();
                    return;
                }
            }
            FrontmostKind::Main | FrontmostKind::None | FrontmostKind::Other => {
                // When: kind is Main, None, or Other, use the main notification surface.
            }
        }
        if let Some(ws) = self.main_mut() {
            ws.notification = Some(bubble);
        }
        if let Some(w) = self.main_window() {
            w.request_redraw();
        }
    }

    pub(super) fn dismiss_notification_at(&mut self, kind: FrontmostKind, x: f32, y: f32) -> bool {
        let Some((message, scale, font_size, window_w, window_h, read_only, search_open)) =
            self.notification_hit_inputs(kind)
        else {
            // When: notification_hit_inputs returns None, no visible close control can be hit.
            return false;
        };
        let content_w =
            message.chars().map(|ch| if ch.is_ascii() { 0.58 } else { 1.0 }).sum::<f32>()
                * font_size;
        let row = u8::from(read_only) + u8::from(search_open);
        let layout = sonicterm_ui::overlays::NotificationBubbleLayout::compute(
            window_w, window_h, content_w, row, scale,
        );
        let inside = x >= layout.close.x
            && x < layout.close.x + layout.close.w
            && y >= layout.close.y
            && y < layout.close.y + layout.close.h;
        if !inside {
            // When: inside is false, the pointer missed the notification close control.
            return false;
        }
        match kind {
            FrontmostKind::Child(id) => {
                // When: kind is FrontmostKind::Child(id), dismiss that child's notification first.
                if let Some(child) = self.windows.get_mut(&id) {
                    // When: windows.get_mut finds id, clear its notification and finish child routing.
                    child.notification = None;
                    child.request_redraw();
                    return true;
                }
            }
            FrontmostKind::Main | FrontmostKind::None | FrontmostKind::Other => {
                // When: kind is Main, None, or Other, use the main notification surface.
            }
        }
        if let Some(ws) = self.main_mut() {
            ws.notification = None;
        }
        if let Some(w) = self.main_window() {
            w.request_redraw();
        }
        true
    }

    fn notification_hit_inputs(
        &self,
        kind: FrontmostKind,
    ) -> Option<(String, f32, f32, f32, f32, bool, bool)> {
        match kind {
            FrontmostKind::Child(id) => {
                let child = self.windows.get(&id)?;
                let message = child.notification.as_ref()?.message.clone();
                let renderer = child.renderer.as_ref()?;
                let window = child.window.as_ref()?;
                let size = window.inner_size();
                let tab_idx = child.tabs.active_index();
                let search_open =
                    child.tab_states.get(tab_idx).is_some_and(|tab| tab.search.is_some());
                let read_only = child.copy_mode.as_ref().is_some_and(|mode| mode.is_read_only());
                Some((
                    message,
                    renderer.scale_factor(),
                    sonicterm_ui::tab_spans::tab_title_font_size(renderer.font_size())
                        * renderer.scale_factor(),
                    size.width as f32,
                    size.height as f32,
                    read_only,
                    search_open,
                ))
            }
            FrontmostKind::Main | FrontmostKind::None | FrontmostKind::Other => {
                let ws = self.main()?;
                let message = ws.notification.as_ref()?.message.clone();
                let renderer = ws.renderer.as_ref()?;
                let window = ws.window.as_ref()?;
                let size = window.inner_size();
                let tab_idx = ws.tabs.active_index();
                let search_open =
                    ws.tab_states.get(tab_idx).is_some_and(|tab| tab.search.is_some());
                let read_only = ws.copy_mode.as_ref().is_some_and(|mode| mode.is_read_only());
                Some((
                    message,
                    renderer.scale_factor(),
                    sonicterm_ui::tab_spans::tab_title_font_size(renderer.font_size())
                        * renderer.scale_factor(),
                    size.width as f32,
                    size.height as f32,
                    read_only,
                    search_open,
                ))
            }
        }
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

    pub fn run_action(&mut self, action: &Action) -> bool {
        // if `frontmost_window` was set to a stale id
        // (window closed between focus event + this dispatch), clear it
        // now so the routing arms below see `None` (safe main fallback)
        // AND the next action doesn't retry the dead window. This single
        // up-front check covers every routed arm.
        let _ = self.clear_stale_frontmost();
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
                self.dispatch_intent(sonicterm_app_core::AppIntent::NewTab {
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
                self.dispatch_intent(sonicterm_app_core::AppIntent::CloseTab {
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
                self.dispatch_intent(sonicterm_app_core::AppIntent::NextTab {
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
                self.dispatch_intent(sonicterm_app_core::AppIntent::PrevTab {
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
                self.dispatch_intent(sonicterm_app_core::AppIntent::GoToTab {
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
                self.dispatch_intent(sonicterm_app_core::AppIntent::SplitPane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Right,
                });
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    // When: frontmost_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.split_active_pane_in_child(id, Direction::Right) {
                        // When: split_active_pane_in_child succeeds to the Right, the child consumed SplitRight.
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.split_active(Direction::Right);
            }
            Action::SplitDown => {
                // When: action is Action::SplitDown, split the routed active pane downward.
                self.dispatch_intent(sonicterm_app_core::AppIntent::SplitPane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Down,
                });
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    // When: frontmost_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.split_active_pane_in_child(id, Direction::Down) {
                        // When: split_active_pane_in_child succeeds Down, the child consumed SplitDown.
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.split_active(Direction::Down);
            }
            Action::ClosePane => {
                // When: action is Action::ClosePane, close the routed active pane.
                self.dispatch_intent(sonicterm_app_core::AppIntent::ClosePane {
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
                self.dispatch_intent(intent);
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
                self.dispatch_intent(sonicterm_app_core::AppIntent::ResizePane {
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
                self.dispatch_intent(sonicterm_app_core::AppIntent::ResizePane {
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
                self.dispatch_intent(sonicterm_app_core::AppIntent::ResizePane {
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
                self.dispatch_intent(sonicterm_app_core::AppIntent::ResizePane {
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
            Action::OpenCommandPalette => self.toggle_command_palette(),
            Action::ScrollToPrevPrompt => self.scroll_to_prompt(false),
            Action::ScrollToNextPrompt => self.scroll_to_prompt(true),
            Action::OpenSshPane(target) => self.open_ssh_pane(target),
            Action::IncreaseFontSize => self.change_font_size(1.0),
            Action::DecreaseFontSize => self.change_font_size(-1.0),
            Action::ResetFontSize => self.reset_font_size(),
            Action::IncreaseFontWeight => self.change_font_weight(FONT_WEIGHT_STEP),
            Action::DecreaseFontWeight => self.change_font_weight(-FONT_WEIGHT_STEP),
            Action::ResetFontWeight => self.reset_font_weight(),
            Action::ApplyTheme(name) => self.apply_theme_by_name(name),
            Action::ToggleTabBar => self.toggle_tab_bar(),
            Action::RenameTab => self.start_rename_active_tab(),
            Action::UpdateTabColor => self.start_update_tab_color(),
            Action::NewWindow => {
                // NewWindow queues a fresh top-level terminal window.
                // set the pending
                // flag; `drain_pending_window_creates` consumes it with
                // the live `ActiveEventLoop` and builds a fresh
                // top-level terminal window. This also works when
                // `self.windows` is empty, preserving the dock-alive
                // post-close-last-window case on macOS when
                // quit_on_last_window_close=false.
                self.pending_new_window = true;
                // Notify the reducer that a new window was requested. It bumps
                // `live_window_count` and emits a `WindowOpen` Effect
                // (currently trace-stubbed in `dispatch_effects`; the
                // production `drain_pending_window_creates` boundary
                // above remains the source of truth for actually
                // building the platform surface).
                self.dispatch_intent(sonicterm_app_core::AppIntent::NewWindow {
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
                self.dispatch_intent(sonicterm_app_core::AppIntent::ResizePane {
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

    /// — source-aware action dispatch. Identical to
    /// [`Self::run_action`] for every action that does NOT depend on
    /// the frontmost window, but for routed arms (NewTab, CloseTab,
    /// tab nav, Split*, ClosePane, FocusPane, resize/zoom/fullscreen,
    /// CloseActivePaneOrTab) it classifies `source_window_id` rather
    /// than reading `self.frontmost_window`.
    ///
    /// Bug: when a Ctrl+T fires in window A but `self.frontmost_window`
    /// still references B (race: Focused(B) event scheduled but not yet
    /// drained by the time A's KeyboardInput is processed, or any other
    /// frontmost-tracking glitch), the cached-frontmost path opens the
    /// new tab in B. Routing keyboard chords through this helper with
    /// the WindowId from the KeyboardInput event itself eliminates the
    /// race — the chord ALWAYS lands on the window that produced it.
    ///
    /// Source-less callers (menubar, palette execution, overlay
    /// dismissal, scrollbar) should continue calling [`Self::run_action`]
    /// which falls back to the cached frontmost.
    ///
    /// `NewWindow` is intentionally NOT routed — it is correct for it
    /// to create a fresh window regardless of the source.
    pub fn run_action_for_window(&mut self, action: &Action, source_window_id: WindowId) -> bool {
        let _ = self.clear_stale_frontmost();
        let source_kind = self.kind_for(source_window_id);
        if self.read_only_active_for_kind(source_kind) && !read_only_allows_action(action) {
            // When: source_kind is READONLY and action is not allowed, consume it without dispatch.
            return true;
        }
        match action {
            Action::CopyToClipboard => self.copy_selection_for_kind(source_kind),
            Action::EnterCopyMode => self.enter_copy_mode_for_kind(source_kind),
            Action::EnterQuickSelect => self.enter_quick_select(),
            Action::PasteFromClipboard => self.paste_clipboard_for_kind(source_kind),
            Action::ReloadConfig => self.force_reload_config(),
            Action::NewTab => {
                // When: action is Action::NewTab, create a tab in the routed terminal window.
                self.dispatch_intent(sonicterm_app_core::AppIntent::NewTab {
                    window: sonicterm_types::WindowKey::new(0),
                    cwd: None,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    // When: source_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.spawn_tab_in_child(id) {
                        // When: spawn_tab_in_child succeeds for id, the child fully consumed NewTab.
                        return true;
                    }
                }
                let n = self.main_tabs().map(|t| t.len() + 1).unwrap_or(1);
                self.new_tab(format!("shell {n}"));
            }
            Action::CloseTab => {
                // When: action is Action::CloseTab, close the routed window's active tab.
                let active_idx = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
                self.dispatch_intent(sonicterm_app_core::AppIntent::CloseTab {
                    window: sonicterm_types::WindowKey::new(0),
                    idx: active_idx,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    // When: source_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.close_active_tab_in_child(id) {
                        // When: close_active_tab_in_child succeeds for id, the child fully consumed CloseTab.
                        return true;
                    }
                }
                let i = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
                self.close_tab_at(i);
                self.reap_empty_main_window_after_close();
            }
            Action::NextTab => {
                // When: action is Action::NextTab, activate the routed window's next tab.
                self.dispatch_intent(sonicterm_app_core::AppIntent::NextTab {
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
                self.dispatch_intent(sonicterm_app_core::AppIntent::PrevTab {
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
            Action::ActivateTab(i) => {
                // When: action is Action::ActivateTab(i), activate index i in the routed window.
                self.dispatch_intent(sonicterm_app_core::AppIntent::GoToTab {
                    window: sonicterm_types::WindowKey::new(0),
                    idx: *i,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    // When: source_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.activate_tab_in_child(id, *i) {
                        // When: activate_tab_in_child succeeds for id and i, the child consumed ActivateTab.
                        return true;
                    }
                }
                self.activate_main_tab(*i);
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
                self.dispatch_intent(sonicterm_app_core::AppIntent::SplitPane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Right,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    // When: source_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.split_active_pane_in_child(id, Direction::Right) {
                        // When: split_active_pane_in_child succeeds to the Right, the child consumed SplitRight.
                        return true;
                    }
                }
                self.split_active(Direction::Right);
            }
            Action::SplitDown => {
                // When: action is Action::SplitDown, split the routed active pane downward.
                self.dispatch_intent(sonicterm_app_core::AppIntent::SplitPane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Down,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    // When: source_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.split_active_pane_in_child(id, Direction::Down) {
                        // When: split_active_pane_in_child succeeds Down, the child consumed SplitDown.
                        return true;
                    }
                }
                self.split_active(Direction::Down);
            }
            Action::ClosePane => {
                // When: action is Action::ClosePane, close the routed active pane.
                self.dispatch_intent(sonicterm_app_core::AppIntent::ClosePane {
                    window: sonicterm_types::WindowKey::new(0),
                });
                if let FrontmostKind::Child(id) = source_kind {
                    // When: source_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.close_active_pane_in_child(id) {
                        // When: close_active_pane_in_child succeeds for id, the child consumed ClosePane.
                        return true;
                    }
                }
                self.close_active_pane();
            }
            Action::CloseActivePaneOrTab => {
                // When: action is Action::CloseActivePaneOrTab, close a split pane or its single-pane tab.
                if let FrontmostKind::Child(id) = source_kind {
                    // When: source_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.close_active_pane_or_tab_in_child(id) {
                        // When: close_active_pane_or_tab_in_child succeeds, the child consumed the close.
                        return true;
                    }
                }
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
            Action::FocusPane(d) => {
                // When: action is Action::FocusPane(d), move focus in direction d.
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
                self.dispatch_intent(intent);
                if let FrontmostKind::Child(id) = source_kind {
                    // When: source_kind is FrontmostKind::Child(id), route the action to that child.
                    if self.focus_pane_dir_in_child(id, *d) {
                        // When: focus_pane_dir_in_child succeeds for id and d, the child consumed FocusPane.
                        return true;
                    }
                }
                self.focus_pane_dir(*d);
            }
            Action::ResizePaneLeft => {
                // When: action is Action::ResizePaneLeft, grow the routed pane leftward.
                self.dispatch_intent(sonicterm_app_core::AppIntent::ResizePane {
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
                self.dispatch_intent(sonicterm_app_core::AppIntent::ResizePane {
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
                self.dispatch_intent(sonicterm_app_core::AppIntent::ResizePane {
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
                self.dispatch_intent(sonicterm_app_core::AppIntent::ResizePane {
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
                self.dispatch_intent(sonicterm_app_core::AppIntent::ResizePane {
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
            Action::ToggleFullscreen => self.toggle_fullscreen_for(source_kind),
            // Non-routed arms delegate to the cached-frontmost dispatcher.
            // Clipboard, theme, and config avoid window-local state; NewWindow
            // creates its own top level; search and palette use the main overlay.
            _ => {
                // When: action is not source-routed here, delegate it to run_action.
                return self.run_action(action);
            }
        }
        true
    }

    /// Classify an explicit window id (rather than `self.frontmost_window`).
    /// Mirrors [`Self::frontmost_kind`] but takes the id from the caller —
    /// used by [`Self::run_action_for_window`] to route a keyboard chord
    /// to the window that produced it.
    fn kind_for(&self, id: WindowId) -> FrontmostKind {
        if let Some(w) = self.main_window() {
            // When: main_window returns w, compare its id before checking children.
            if w.id() == id {
                // When: w.id equals id, classify the explicit source as Main.
                return FrontmostKind::Main;
            }
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
