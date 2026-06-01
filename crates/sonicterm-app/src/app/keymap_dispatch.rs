//! Extracted from `app/mod.rs` in refactor PR 8b (expose-then-extract).
//! `App`'s referenced fields are `pub(super)`; this submodule lives in
//! the same `app` module tree, so direct field access works.

#![allow(unused_imports)]

use std::collections::HashMap;
use std::sync::{atomic::Ordering, Arc};
use std::time::{Duration, Instant};

use anyhow::Context;
use parking_lot::Mutex;
use sonicterm_core::{
    config::Config,
    grid::Grid,
    keymap::{Action, Direction, Keymap, ScrollAction},
    pty::PtyHandle,
    theme::Theme,
    vt::{Parser, VtEvent},
};
use sonicterm_shared::render::GpuRenderer;
use sonicterm_ui::pane::PaneTree;
use sonicterm_ui::selection::Selection;
use sonicterm_ui::tabbar_view::{TabBarLayout, TabHit};
use sonicterm_ui::tabs::{Tab, TabBar};
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
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.next_tab_in_child(id) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                if let Some(t) = self.main_tabs_mut() {
                    t.next();
                }
            }
            Action::PrevTab => {
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.prev_tab_in_child(id) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                if let Some(t) = self.main_tabs_mut() {
                    t.prev();
                }
            }
            Action::ActivateTab(i) => {
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.activate_tab_in_child(id, *i) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                if let Some(t) = self.main_tabs_mut() {
                    t.activate(*i);
                }
            }
            Action::ActivateLastTab => {
                if let FrontmostKind::Child(id) = self.frontmost_kind() {
                    if self.activate_last_tab_in_child(id) {
                        return true;
                    }
                    self.frontmost_window = None;
                }
                if let Some(t) = self.main_tabs_mut() {
                    let last = t.len().saturating_sub(1);
                    t.activate(last);
                }
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
            Action::EditConfigFile => self.open_config_file(),
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
            Action::ToggleFullscreen | Action::ResizePane { .. } => {
                tracing::info!("action {action:?} accepted but not yet wired up");
            }
        }
        true
    }
}
