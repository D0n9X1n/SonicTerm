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
    pub(super) fn search_handle_ime_commit(&mut self, text: &str) -> bool {
        let (i, pane_id) = {
            let Some(ws) = self.main() else { return false };
            let i = ws.tabs.active_index();
            let Some(t) = ws.tab_states.get(i) else { return false };
            if t.search.is_none() {
                return false;
            }
            (i, t.active_pane)
        };
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
                if let Some(ws) = self.main_mut() {
                    if let Some(st) = ws.tab_states.get_mut(i) {
                        st.search = Some(search);
                    }
                }
                return false;
            }
        };
        let grid_guard = parser_arc.lock();
        search.input_str(text, grid_guard.grid());
        drop(grid_guard);
        if let Some(ws) = self.main_mut() {
            if let Some(st) = ws.tab_states.get_mut(i) {
                st.search = Some(search);
            }
        }
        true
    }

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
        let anchor_row = (grid.scrollback_len() as u32).saturating_add(u32::from(grid.cursor.row));
        let anchor_col = grid.cursor.col;

        let (handled, keep_search) = match &event.logical_key {
            Key::Named(NamedKey::Escape) => (true, false),
            Key::Named(NamedKey::Enter) => {
                if search.current.is_none() {
                    search.select_nearest(anchor_row, anchor_col);
                } else if mods.shift_key() {
                    search.prev();
                } else {
                    search.next();
                }
                (true, true)
            }
            Key::Named(NamedKey::ArrowDown) => {
                if search.current.is_none() {
                    search.next_from(anchor_row, anchor_col);
                } else {
                    search.next();
                }
                (true, true)
            }
            Key::Named(NamedKey::ArrowUp) => {
                if search.current.is_none() {
                    search.prev_from(anchor_row, anchor_col);
                } else {
                    search.prev();
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
                            if search.current.is_none() {
                                search.select_nearest(anchor_row, anchor_col);
                            } else if mods.shift_key() {
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
        let requested_view_top = if handled && keep_search {
            search.requested_scroll_row.map(|row| centered_search_view_top(grid, row))
        } else {
            None
        };
        drop(grid_guard);
        if let Some(view_top) = requested_view_top {
            if let Some(ws) = self.main_mut() {
                if let Some(pane) = ws.panes.get_mut(&pane_id) {
                    pane.viewport_top_abs = view_top;
                }
                mark_all_panes_dirty(&ws.panes);
            }
        }
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

fn centered_search_view_top(grid: &Grid, row: u32) -> Option<u64> {
    let live_top = grid.scrollback_len() as u64;
    let half = u64::from(grid.rows) / 2;
    let desired = u64::from(row).saturating_sub(half).min(live_top);
    (desired < live_top).then_some(desired)
}
