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
    with_integrated_titlebar, wrap_paste, App, PaneState, TabState, UserEvent, WindowState,
};

impl App {
    pub(super) fn search_handle_key(&mut self, event: &KeyEvent, mods: ModifiersState) -> bool {
        let (i, pane_id) = {
            let Some(ws) = self.main() else { return false };
            let i = ws.tabs.active_index();
            let Some(t) = ws.tab_states.get(i) else { return false };
            if t.search.is_none() {
                return false;
            }
            (i, t.active_pane)
        };
        // Take the search state out of the tab so we can hold its
        // `&mut SearchState` alongside the parser's grid borrow without
        // double-borrowing through `self.main_mut()` and `self.panes`.
        let mut search = {
            let Some(ws) = self.main_mut() else { return false };
            let Some(st) = ws.tab_states.get_mut(i) else { return false };
            match st.search.take() {
                Some(s) => s,
                None => return false,
            }
        };
        let parser_arc = match self.main().and_then(|ws| ws.panes.get(&pane_id)) {
            Some(p) => p.parser.clone(),
            None => {
                // Restore so we don't drop user state on a missing pane.
                if let Some(ws) = self.main_mut() {
                    if let Some(st) = ws.tab_states.get_mut(i) {
                        st.search = Some(search);
                    }
                }
                return false;
            }
        };
        let grid_guard = parser_arc.lock();
        let grid = grid_guard.grid();

        let (handled, keep_search) = match &event.logical_key {
            Key::Named(NamedKey::Escape) => (true, false),
            Key::Named(NamedKey::Enter) => {
                if mods.shift_key() {
                    search.prev();
                } else {
                    search.next();
                }
                (true, true)
            }
            Key::Named(NamedKey::Backspace) => {
                search.backspace(grid);
                (true, true)
            }
            Key::Named(NamedKey::Space) => {
                search.input_char(' ', grid);
                (true, true)
            }
            Key::Character(s) => {
                let mut consumed = false;
                if mods.super_key() {
                    match s.as_ref() {
                        "i" | "I" => {
                            search.toggle_case_sensitive(grid);
                            consumed = true;
                        }
                        "r" | "R" => {
                            search.toggle_regex(grid);
                            consumed = true;
                        }
                        "g" | "G" => {
                            if mods.shift_key() {
                                search.prev();
                            } else {
                                search.next();
                            }
                            consumed = true;
                        }
                        _ => {}
                    }
                }
                if !consumed {
                    for ch in s.chars() {
                        search.input_char(ch, grid);
                    }
                }
                (true, true)
            }
            _ => (false, true),
        };
        drop(grid_guard);
        if keep_search {
            if let Some(ws) = self.main_mut() {
                if let Some(st) = ws.tab_states.get_mut(i) {
                    st.search = Some(search);
                }
            }
        }
        handled
    }
}
