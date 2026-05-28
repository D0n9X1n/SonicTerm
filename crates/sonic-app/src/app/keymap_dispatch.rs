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
    to_logical_pos, with_integrated_titlebar, wrap_paste, App, ChildWindow, PaneState, TabState,
    UserEvent,
};
use crate::app::integrated_titlebar_inset;

impl App {
    pub fn run_action(&mut self, action: &Action) -> bool {
        match action {
            Action::CopyToClipboard => self.copy_selection(),
            Action::PasteFromClipboard => self.paste_clipboard(),
            Action::ReloadConfig => self.force_reload_config(),
            Action::NewTab => {
                // Route to the focused window so torn-out child windows
                // get their own tab instead of injecting one into the
                // main window. User report v0.6: see `App::focused_child`
                // doc for the original Chinese complaint + repro.
                if let Some(win_id) = self.focused_child {
                    if self.spawn_tab_in_child(win_id) {
                        return true;
                    }
                    // Child window vanished mid-dispatch — fall back to
                    // the main App rather than dropping the action.
                    self.focused_child = None;
                }
                let n = self.tabs.len() + 1;
                self.new_tab(format!("shell {n}"));
            }
            Action::CloseTab => {
                let i = self.tabs.active_index();
                self.close_tab_at(i);
            }
            Action::NextTab => self.tabs.next(),
            Action::PrevTab => self.tabs.prev(),
            Action::ActivateTab(i) => self.tabs.activate(*i),
            Action::ActivateLastTab => {
                let last = self.tabs.len().saturating_sub(1);
                self.tabs.activate(last);
            }
            Action::SplitRight => self.split_active(Direction::Right),
            Action::SplitDown => self.split_active(Direction::Down),
            Action::ClosePane => self.close_active_pane(),
            Action::FocusPane(d) => self.focus_pane_dir(*d),
            Action::OpenSearch => self.open_search(),
            Action::OpenPreferences => self.open_preferences(),
            Action::OpenCommandPalette => self.toggle_command_palette(),
            Action::ScrollToPrevPrompt => self.scroll_to_prompt(false),
            Action::ScrollToNextPrompt => self.scroll_to_prompt(true),
            Action::OpenSshPane(target) => self.open_ssh_pane(target),
            Action::IncreaseFontSize => self.change_font_size(1.0),
            Action::DecreaseFontSize => self.change_font_size(-1.0),
            Action::ResetFontSize => self.reset_font_size(),
            Action::ApplyTheme(name) => self.apply_theme_by_name(name),
            Action::ToggleTabBar => self.toggle_tab_bar(),
            Action::Scroll(_)
            | Action::ToggleFullscreen
            | Action::ResizePane { .. }
            | Action::NewWindow => {
                tracing::info!("action {action:?} accepted but not yet wired up");
            }
        }
        true
    }
}
