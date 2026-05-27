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
    pub(super) fn search_handle_key(&mut self, event: &KeyEvent, mods: ModifiersState) -> bool {
        let i = self.tabs.active_index();
        let pane_id = match self.tab_states.get(i) {
            Some(t) if t.search.is_some() => t.active_pane,
            _ => return false,
        };
        let pane = match self.panes.get(&pane_id) {
            Some(p) => p,
            None => return false,
        };
        let grid_guard = pane.parser.lock();
        let grid = grid_guard.grid();

        let Some(st) = self.tab_states.get_mut(i) else { return false };
        let Some(search) = st.search.as_mut() else { return false };

        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                st.search = None;
                true
            }
            Key::Named(NamedKey::Enter) => {
                if mods.shift_key() {
                    search.prev();
                } else {
                    search.next();
                }
                true
            }
            Key::Named(NamedKey::Backspace) => {
                search.backspace(grid);
                true
            }
            Key::Named(NamedKey::Space) => {
                search.input_char(' ', grid);
                true
            }
            Key::Character(s) => {
                // Cmd+I toggles case sensitivity; Cmd+R toggles regex
                // mode; Cmd+G / Cmd+Shift+G jump to next/prev match.
                if mods.super_key() {
                    match s.as_ref() {
                        "i" | "I" => {
                            search.toggle_case_sensitive(grid);
                            return true;
                        }
                        "r" | "R" => {
                            search.toggle_regex(grid);
                            return true;
                        }
                        "g" | "G" => {
                            if mods.shift_key() {
                                search.prev();
                            } else {
                                search.next();
                            }
                            return true;
                        }
                        _ => {}
                    }
                }
                for ch in s.chars() {
                    search.input_char(ch, grid);
                }
                true
            }
            _ => false,
        }
    }
}
