//! Extracted from `app/mod.rs` in refactor PR 8b (expose-then-extract).
//! `App`'s referenced fields are `pub(super)`; this submodule lives in
//! the same `app` module tree, so direct field access works.

#![allow(unused_imports)]

use std::collections::HashMap;
use std::sync::{atomic::Ordering, Arc};
use std::time::{Duration, Instant};

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

pub(super) fn read_only_allows_action(action: &Action) -> bool {
    matches!(
        action,
        Action::NextTab
            | Action::PrevTab
            | Action::ActivateTab(_)
            | Action::ActivateLastTab
            | Action::FocusPane(_)
            | Action::OpenSearch
    )
}

impl App {
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
        // Epic #289 Phase A — if `frontmost_window` was set to a stale id
        // (window closed between focus event + this dispatch), clear it
        // now so the routing arms below see `None` (safe main fallback)
        // AND the next action doesn't retry the dead window. This single
        // up-front check covers every routed arm.
        let _ = self.clear_stale_frontmost();
        if self.read_only_active_for_kind(self.frontmost_kind()) && !read_only_allows_action(action)
        {
            return true;
        }
        match action {
            Action::CopyToClipboard => self.copy_selection(),
            Action::EnterCopyMode => self.enter_copy_mode(),
            Action::EnterQuickSelect => self.enter_quick_select(),
            Action::PasteFromClipboard => self.paste_clipboard(),
            Action::ReloadConfig => self.force_reload_config(),
            Action::NewTab => {
                // M6a-expand-2c-tab: notify the reducer the user
                // asked for a new tab. The reducer bumps tab_count,
                // sets active_tab_idx, and emits Render(TabAdded).
                // Boundary below remains source-of-truth for the
                // actual tab spawn (it owns the PtyHandle/Grid/Parser
                // tree that the renderer paints).
                self.dispatch_intent(sonicterm_app_core::AppIntent::NewTab {
                    window: sonicterm_types::WindowKey::new(0),
                    cwd: None,
                });
                // Epic #289 Phase A — route through the unified
                // `frontmost_window` discriminator so a Cmd+T typed in a
                // torn-out child opens a tab in THAT child, not in the
                // main window. PR-B4 (#365) removed the `focused_child`
                // fallback — `frontmost_window` is set by the same focus
                // event so the back-compat path was redundant.
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.spawn_tab_in_child(id) {
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
                // M6a-expand-2c-tab: notify reducer first so
                // tab_count/active_tab_idx stay in sync.
                let active_idx = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
                self.dispatch_intent(sonicterm_app_core::AppIntent::CloseTab {
                    window: sonicterm_types::WindowKey::new(0),
                    idx: active_idx,
                });
                // Epic #289 Phase A — route to frontmost window.
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.close_active_tab_in_child(id) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                let i = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
                self.close_tab_at(i);
                self.reap_empty_main_window_after_close();
            }
            Action::NextTab => {
                self.dispatch_intent(sonicterm_app_core::AppIntent::NextTab {
                    window: sonicterm_types::WindowKey::new(0),
                });
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.next_tab_in_child(id) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.next_main_tab();
            }
            Action::PrevTab => {
                self.dispatch_intent(sonicterm_app_core::AppIntent::PrevTab {
                    window: sonicterm_types::WindowKey::new(0),
                });
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.prev_tab_in_child(id) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.prev_main_tab();
            }
            Action::ActivateTab(i) => {
                self.dispatch_intent(sonicterm_app_core::AppIntent::GoToTab {
                    window: sonicterm_types::WindowKey::new(0),
                    idx: *i,
                });
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.activate_tab_in_child(id, *i) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.activate_main_tab(*i);
            }
            Action::ActivateLastTab => {
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.activate_last_tab_in_child(id) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.activate_last_main_tab();
            }
            Action::SplitRight => {
                // Epic #289 Phase A — route to frontmost window so Cmd+D
                // typed in a torn-out child splits THAT window's active
                // pane, not the main window's.
                // M6a-expand-2c-pane: notify the reducer first so
                // `pane_count` / `focused_pane_idx` track the topology;
                // the boundary's `split_active*` remains source-of-truth
                // for actual geometry.
                self.dispatch_intent(sonicterm_app_core::AppIntent::SplitPane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Right,
                });
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.split_active_pane_in_child(id, Direction::Right) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.split_active(Direction::Right);
            }
            Action::SplitDown => {
                self.dispatch_intent(sonicterm_app_core::AppIntent::SplitPane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Down,
                });
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.split_active_pane_in_child(id, Direction::Down) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.split_active(Direction::Down);
            }
            Action::ClosePane => {
                self.dispatch_intent(sonicterm_app_core::AppIntent::ClosePane {
                    window: sonicterm_types::WindowKey::new(0),
                });
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.close_active_pane_in_child(id) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.close_active_pane();
            }
            Action::CloseActivePaneOrTab => {
                // Epic #289 Phase A — Cmd+W routes to frontmost window.
                // Without this, a Cmd+W typed in a torn-out child window
                // closed a tab in the original main window (bug #3).
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.close_active_pane_or_tab_in_child(id) {
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
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.toggle_active_pane_zoom_in_child(id) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.toggle_active_pane_zoom();
            }
            Action::ToggleBroadcast { scope } => self.toggle_broadcast(*scope),
            Action::FocusPane(d) => {
                // M6a-expand-2c-pane: notify reducer (emits
                // Render(Focus) when pane_count >= 2; no-op otherwise).
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
                    if self.focus_pane_dir_in_child(id, *d) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.focus_pane_dir(*d);
            }
            Action::ResizePaneLeft => {
                self.dispatch_intent(sonicterm_app_core::AppIntent::ResizePane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Left,
                    cells: 1,
                });
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.resize_active_split_in_child(id, Direction::Left) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.resize_active_split(Direction::Left);
            }
            Action::ResizePaneRight => {
                self.dispatch_intent(sonicterm_app_core::AppIntent::ResizePane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Right,
                    cells: 1,
                });
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.resize_active_split_in_child(id, Direction::Right) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.resize_active_split(Direction::Right);
            }
            Action::ResizePaneUp => {
                self.dispatch_intent(sonicterm_app_core::AppIntent::ResizePane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Up,
                    cells: 1,
                });
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.resize_active_split_in_child(id, Direction::Up) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.resize_active_split(Direction::Up);
            }
            Action::ResizePaneDown => {
                self.dispatch_intent(sonicterm_app_core::AppIntent::ResizePane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Down,
                    cells: 1,
                });
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.resize_active_split_in_child(id, Direction::Down) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.resize_active_split(Direction::Down);
            }
            Action::OpenSearch => {
                // Route to the frontmost child window so Cmd+F opens search in a
                // torn-out window instead of the main one. (#pane-search)
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.open_search_in_child(id) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.open_search();
            }
            Action::EditConfigFile => self.open_config_file(),
            Action::OpenKeymapFile => self.open_keymap_file(),
            Action::OpenCommandPalette => self.toggle_command_palette(),
            Action::ScrollToPrevPrompt => self.scroll_to_prompt(false),
            Action::ScrollToNextPrompt => self.scroll_to_prompt(true),
            Action::OpenSshPane(target) => self.open_ssh_pane(target),
            Action::IncreaseFontSize => self.change_font_size(1.0),
            Action::DecreaseFontSize => self.change_font_size(-1.0),
            Action::ResetFontSize => self.reset_font_size(),
            Action::ApplyTheme(name) => self.apply_theme_by_name(name),
            Action::ToggleTabBar => self.toggle_tab_bar(),
            Action::RenameTab => self.start_rename_active_tab(),
            Action::NewWindow => {
                // Epic #289 Phase E (Haiku follow-up): set the pending
                // flag; `drain_pending_window_creates` consumes it with
                // the live `ActiveEventLoop` and builds a fresh
                // top-level terminal window. Works whether or not
                // `self.windows` is empty — the dock-alive
                // post-close-last-window case (macOS,
                // quit_on_last_window_close=false) is the motivating
                // bug Haiku flagged on PR #297.
                self.pending_new_window = true;
                // M6a-expand-2c-window: notify the reducer the user
                // asked for a new window. The reducer bumps
                // `live_window_count` and emits a `WindowOpen` Effect
                // (currently trace-stubbed in `dispatch_effects`; the
                // production `drain_pending_window_creates` boundary
                // above remains the source of truth for actually
                // building the platform surface).
                self.dispatch_intent(sonicterm_app_core::AppIntent::NewWindow {
                    role: sonicterm_app_core::WindowRole::Primary,
                });
            }
            Action::Scroll(kind) => {
                // #412: replace the "not yet wired up" stub. Translate
                // ScrollAction → signed line delta and route through the
                // canonical `scroll_pane` mutator (which also handles
                // alt-screen no-op + clamping + auto-follow snap-back).
                let Some(pane_id) = self.active_pane_id() else { return true };
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
                if *amount == 0 {
                    return true;
                }
                self.dispatch_intent(sonicterm_app_core::AppIntent::ResizePane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: split_dir(*dir),
                    cells: *amount,
                });
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    let mut routed = false;
                    for _ in 0..*amount {
                        routed = self.resize_active_split_in_child(id, *dir) || routed;
                    }
                    if routed {
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
        }
        true
    }

    /// Issue #539 — source-aware action dispatch. Identical to
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
            return true;
        }
        match action {
            Action::CopyToClipboard => self.copy_selection(),
            Action::EnterCopyMode => self.enter_copy_mode(),
            Action::EnterQuickSelect => self.enter_quick_select(),
            Action::PasteFromClipboard => self.paste_clipboard(),
            Action::ReloadConfig => self.force_reload_config(),
            Action::NewTab => {
                self.dispatch_intent(sonicterm_app_core::AppIntent::NewTab {
                    window: sonicterm_types::WindowKey::new(0),
                    cwd: None,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    if self.spawn_tab_in_child(id) {
                        return true;
                    }
                }
                let n = self.main_tabs().map(|t| t.len() + 1).unwrap_or(1);
                self.new_tab(format!("shell {n}"));
            }
            Action::CloseTab => {
                let active_idx = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
                self.dispatch_intent(sonicterm_app_core::AppIntent::CloseTab {
                    window: sonicterm_types::WindowKey::new(0),
                    idx: active_idx,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    if self.close_active_tab_in_child(id) {
                        return true;
                    }
                }
                let i = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
                self.close_tab_at(i);
                self.reap_empty_main_window_after_close();
            }
            Action::NextTab => {
                self.dispatch_intent(sonicterm_app_core::AppIntent::NextTab {
                    window: sonicterm_types::WindowKey::new(0),
                });
                if let FrontmostKind::Child(id) = source_kind {
                    if self.next_tab_in_child(id) {
                        return true;
                    }
                }
                self.next_main_tab();
            }
            Action::PrevTab => {
                self.dispatch_intent(sonicterm_app_core::AppIntent::PrevTab {
                    window: sonicterm_types::WindowKey::new(0),
                });
                if let FrontmostKind::Child(id) = source_kind {
                    if self.prev_tab_in_child(id) {
                        return true;
                    }
                }
                self.prev_main_tab();
            }
            Action::ActivateTab(i) => {
                self.dispatch_intent(sonicterm_app_core::AppIntent::GoToTab {
                    window: sonicterm_types::WindowKey::new(0),
                    idx: *i,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    if self.activate_tab_in_child(id, *i) {
                        return true;
                    }
                }
                self.activate_main_tab(*i);
            }
            Action::ActivateLastTab => {
                if let FrontmostKind::Child(id) = source_kind {
                    if self.activate_last_tab_in_child(id) {
                        return true;
                    }
                }
                self.activate_last_main_tab();
            }
            Action::SplitRight => {
                self.dispatch_intent(sonicterm_app_core::AppIntent::SplitPane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Right,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    if self.split_active_pane_in_child(id, Direction::Right) {
                        return true;
                    }
                }
                self.split_active(Direction::Right);
            }
            Action::SplitDown => {
                self.dispatch_intent(sonicterm_app_core::AppIntent::SplitPane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Down,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    if self.split_active_pane_in_child(id, Direction::Down) {
                        return true;
                    }
                }
                self.split_active(Direction::Down);
            }
            Action::ClosePane => {
                self.dispatch_intent(sonicterm_app_core::AppIntent::ClosePane {
                    window: sonicterm_types::WindowKey::new(0),
                });
                if let FrontmostKind::Child(id) = source_kind {
                    if self.close_active_pane_in_child(id) {
                        return true;
                    }
                }
                self.close_active_pane();
            }
            Action::CloseActivePaneOrTab => {
                if let FrontmostKind::Child(id) = source_kind {
                    if self.close_active_pane_or_tab_in_child(id) {
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
                    self.close_tab_at(i);
                }
                self.reap_empty_main_window_after_close();
            }
            Action::TogglePaneZoom => {
                if let FrontmostKind::Child(id) = source_kind {
                    if self.toggle_active_pane_zoom_in_child(id) {
                        return true;
                    }
                }
                self.toggle_active_pane_zoom();
            }
            Action::FocusPane(d) => {
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
                    if self.focus_pane_dir_in_child(id, *d) {
                        return true;
                    }
                }
                self.focus_pane_dir(*d);
            }
            Action::ResizePaneLeft => {
                self.dispatch_intent(sonicterm_app_core::AppIntent::ResizePane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Left,
                    cells: 1,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    if self.resize_active_split_in_child(id, Direction::Left) {
                        return true;
                    }
                }
                self.resize_active_split(Direction::Left);
            }
            Action::ResizePaneRight => {
                self.dispatch_intent(sonicterm_app_core::AppIntent::ResizePane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Right,
                    cells: 1,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    if self.resize_active_split_in_child(id, Direction::Right) {
                        return true;
                    }
                }
                self.resize_active_split(Direction::Right);
            }
            Action::ResizePaneUp => {
                self.dispatch_intent(sonicterm_app_core::AppIntent::ResizePane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Up,
                    cells: 1,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    if self.resize_active_split_in_child(id, Direction::Up) {
                        return true;
                    }
                }
                self.resize_active_split(Direction::Up);
            }
            Action::ResizePaneDown => {
                self.dispatch_intent(sonicterm_app_core::AppIntent::ResizePane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: sonicterm_app_core::SplitDir::Down,
                    cells: 1,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    if self.resize_active_split_in_child(id, Direction::Down) {
                        return true;
                    }
                }
                self.resize_active_split(Direction::Down);
            }
            Action::ResizePane { dir, amount } => {
                if *amount == 0 {
                    return true;
                }
                self.dispatch_intent(sonicterm_app_core::AppIntent::ResizePane {
                    window: sonicterm_types::WindowKey::new(0),
                    dir: split_dir(*dir),
                    cells: *amount,
                });
                if let FrontmostKind::Child(id) = source_kind {
                    for _ in 0..*amount {
                        self.resize_active_split_in_child(id, *dir);
                    }
                } else {
                    for _ in 0..*amount {
                        self.resize_active_split(*dir);
                    }
                }
            }
            Action::ToggleFullscreen => self.toggle_fullscreen_for(source_kind),
            // Non-routed arms — delegate to the cached-frontmost
            // dispatcher. These either don't touch per-window state
            // (clipboard, theme, config) or have their own routing
            // (NewWindow correctly creates a new top-level regardless
            // of source). OpenSearch / palette use the main-window
            // overlay singleton today — see #539 follow-up for
            // per-window overlay routing.
            _ => return self.run_action(action),
        }
        true
    }

    /// Classify an explicit window id (rather than `self.frontmost_window`).
    /// Mirrors [`Self::frontmost_kind`] but takes the id from the caller —
    /// used by [`Self::run_action_for_window`] to route a keyboard chord
    /// to the window that produced it.
    fn kind_for(&self, id: WindowId) -> FrontmostKind {
        if let Some(w) = self.main_window() {
            if w.id() == id {
                return FrontmostKind::Main;
            }
        }
        if self.windows.contains_key(&id) {
            return FrontmostKind::Child(id);
        }
        FrontmostKind::None
    }

    fn toggle_fullscreen_for(&mut self, kind: FrontmostKind) {
        if let FrontmostKind::Child(id) = kind {
            if let Some(window) = self.windows.get(&id).and_then(|child| child.window.as_ref()) {
                toggle_window_fullscreen(window);
                return;
            }
            self.frontmost_window = None;
        }
        if let Some(window) = self.main_window() {
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
        window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
    }
}
