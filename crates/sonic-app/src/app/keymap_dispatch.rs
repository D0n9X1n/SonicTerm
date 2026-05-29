//! Extracted from `app/mod.rs` in refactor PR 8b (expose-then-extract).
//! `App`'s referenced fields are `pub(super)`; this submodule lives in
//! the same `app` module tree, so direct field access works.

#![allow(unused_imports)]

use std::collections::HashMap;
use std::sync::{atomic::Ordering, Arc};
use std::time::{Duration, Instant};

use anyhow::Context;
use parking_lot::Mutex;
use sonic_core::{
    config::Config,
    grid::Grid,
    keymap::{Action, Direction, Keymap, ScrollAction},
    pty::PtyHandle,
    theme::Theme,
    vt::{Parser, VtEvent},
};
use sonic_shared::render::GpuRenderer;
use sonic_ui::pane::PaneTree;
use sonic_ui::prefs::{PrefsHit, PrefsState};
use sonic_ui::selection::Selection;
use sonic_ui::tabbar_view::{TabBarLayout, TabHit};
use sonic_ui::tabs::{Tab, TabBar};
use winit::{
    event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowAttributes, WindowId},
};

use super::{
    key_encoding::{encode_key, encode_logical, key_event_to_string, key_name},
    mark_all_panes_dirty, next_pane_id, pick_prompt_target, resize_all_panes, shell_quote_posix,
    to_logical_pos, with_integrated_titlebar, wrap_paste, App, FrontmostKind, PaneState, TabState,
    UserEvent, WindowState,
};
use crate::app::integrated_titlebar_inset;

impl App {
    pub fn run_action(&mut self, action: &Action) -> bool {
        // Epic #289 Phase A — if `frontmost_window` was set to a stale id
        // (window closed between focus event + this dispatch), clear it
        // now so the routing arms below see `None` (safe main fallback)
        // AND the next action doesn't retry the dead window. This single
        // up-front check covers every routed arm.
        let _ = self.clear_stale_frontmost();
        match action {
            Action::CopyToClipboard => self.copy_selection(),
            Action::EnterCopyMode => self.enter_copy_mode(),
            Action::EnterQuickSelect => self.enter_quick_select(),
            Action::PasteFromClipboard => self.paste_clipboard(),
            Action::ReloadConfig => self.force_reload_config(),
            Action::NewTab => {
                // Epic #289 Phase A — route through the unified
                // `frontmost_window` discriminator first so a Cmd+T
                // typed in a torn-out child opens a tab in THAT child,
                // not in the main window. Falls back to the existing
                // `focused_child` logic for back-compat with any focus
                // event that updated only the old field, and finally
                // to the main App (safe default).
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.spawn_tab_in_child(id) {
                        return true;
                    }
                    // Child vanished between focus and dispatch — clear
                    // both trackers and fall through.
                    self.frontmost_window = None;
                    self.focused_child = None;
                } else if let Some(win_id) = self.focused_child {
                    if self.spawn_tab_in_child(win_id) {
                        return true;
                    }
                    self.focused_child = None;
                }
                let n = self.tabs.len() + 1;
                self.new_tab(format!("shell {n}"));
            }
            Action::CloseTab => {
                // Epic #289 Phase A — route to frontmost window.
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.close_active_tab_in_child(id) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                let i = self.tabs.active_index();
                self.close_tab_at(i);
            }
            Action::NextTab => {
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.next_tab_in_child(id) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.tabs.next();
            }
            Action::PrevTab => {
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.prev_tab_in_child(id) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.tabs.prev();
            }
            Action::ActivateTab(i) => {
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.activate_tab_in_child(id, *i) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.tabs.activate(*i);
            }
            Action::ActivateLastTab => {
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.activate_last_tab_in_child(id) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                let last = self.tabs.len().saturating_sub(1);
                self.tabs.activate(last);
            }
            Action::SplitRight => {
                // Epic #289 Phase A — route to frontmost window so Cmd+D
                // typed in a torn-out child splits THAT window's active
                // pane, not the main window's.
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.split_active_pane_in_child(id, Direction::Right) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.split_active(Direction::Right);
            }
            Action::SplitDown => {
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.split_active_pane_in_child(id, Direction::Down) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.split_active(Direction::Down);
            }
            Action::ClosePane => {
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
                let i = self.tabs.active_index();
                let pane_count =
                    self.tab_states.get(i).map(|st| st.tree.leaves().len()).unwrap_or(0);
                if pane_count > 1 {
                    self.close_active_pane();
                } else {
                    self.close_tab_at(i);
                }
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
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.focus_pane_dir_in_child(id, *d) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.focus_pane_dir(*d);
            }
            Action::ResizePaneLeft => {
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.resize_active_split_in_child(id, Direction::Left) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.resize_active_split(Direction::Left);
            }
            Action::ResizePaneRight => {
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.resize_active_split_in_child(id, Direction::Right) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.resize_active_split(Direction::Right);
            }
            Action::ResizePaneUp => {
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.resize_active_split_in_child(id, Direction::Up) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.resize_active_split(Direction::Up);
            }
            Action::ResizePaneDown => {
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.resize_active_split_in_child(id, Direction::Down) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                self.resize_active_split(Direction::Down);
            }
            Action::OpenSearch => self.open_search(),
            Action::OpenPreferences => self.open_preferences(),
            Action::OpenKeymapFile => self.open_keymap_file(),
            Action::OpenCommandPalette => self.toggle_command_palette(),
            Action::ShowKeymapCheatsheet => self.toggle_cheatsheet(),
            Action::ScrollToPrevPrompt => self.scroll_to_prompt(false),
            Action::ScrollToNextPrompt => self.scroll_to_prompt(true),
            Action::OpenSshPane(target) => self.open_ssh_pane(target),
            Action::IncreaseFontSize => self.change_font_size(1.0),
            Action::DecreaseFontSize => self.change_font_size(-1.0),
            Action::ResetFontSize => self.reset_font_size(),
            Action::ApplyTheme(name) => self.apply_theme_by_name(name),
            Action::ToggleTabBar => self.toggle_tab_bar(),
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
            }
            Action::Scroll(_) | Action::ToggleFullscreen | Action::ResizePane { .. } => {
                tracing::info!("action {action:?} accepted but not yet wired up");
            }
        }
        true
    }
}
