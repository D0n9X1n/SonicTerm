//! Extracted from `app/mod.rs` from the monolithic app module.
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
            let Some(ws) = self.main() else {
                // When: main returns None, no main-window search can consume the IME commit.
                return false;
            };
            let i = ws.tabs.active_index();
            let Some(t) = ws.tab_states.get(i) else {
                // When: tab_states.get cannot find i, no active tab can consume the IME commit.
                return false;
            };
            if t.search.is_none() {
                // When: search is None, the IME commit belongs to another input route.
                return false;
            }
            (i, t.active_pane)
        };
        let mut search = {
            let Some(ws) = self.main_mut() else {
                // When: main_mut returns None, the search state cannot be taken for the IME edit.
                return false;
            };
            let Some(st) = ws.tab_states.get_mut(i) else {
                // When: tab_states.get_mut cannot find i, the active search state is unavailable.
                return false;
            };
            match st.search.take() {
                Some(s) => s,
                None => {
                    // When: search.take returns None, no search state remains to consume the IME commit.
                    return false;
                }
            }
        };
        let parser_arc = match self.main().and_then(|ws| ws.panes.get(&pane_id)) {
            Some(p) => p.parser.clone(),
            None => {
                // When: panes.get returns None for pane_id, restore the detached search state.
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
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(
            grid,
            self.main()
                .and_then(|ws| ws.panes.get(&pane_id))
                .and_then(|pane| pane.viewport_top_abs),
        );
        prepare_search(&mut search, pane_id, grid, view_top);
        search.input_str(text, grid);
        anchor_unfocused_search(&mut search, view_top);
        drop(grid_guard);
        if let Some(ws) = self.main_mut() {
            if let Some(st) = ws.tab_states.get_mut(i) {
                st.search = Some(search);
            }
        }
        true
    }

    /// Child-window mirror of [`Self::search_handle_ime_commit`]: feed an IME
    /// commit into the search box of the torn-out window `win_id`. Returns
    /// `true` if a search box was open and consumed the text.
    pub(super) fn search_handle_ime_commit_in_child(
        &mut self,
        win_id: WindowId,
        text: &str,
    ) -> bool {
        let (i, pane_id) = {
            let Some(child) = self.windows.get(&win_id) else {
                // When: windows.get cannot find win_id, no child search can consume the IME commit.
                return false;
            };
            let i = child.tabs.active_index();
            let Some(t) = child.tab_states.get(i) else {
                // When: tab_states.get cannot find i, no child tab can consume the IME commit.
                return false;
            };
            if t.search.is_none() {
                // When: search is None, the child IME commit belongs to another input route.
                return false;
            }
            (i, t.active_pane)
        };
        let mut search = {
            let Some(child) = self.windows.get_mut(&win_id) else {
                // When: windows.get_mut cannot find win_id, the child search state cannot be taken.
                return false;
            };
            let Some(st) = child.tab_states.get_mut(i) else {
                // When: tab_states.get_mut cannot find i, the child search state is unavailable.
                return false;
            };
            match st.search.take() {
                Some(s) => s,
                None => {
                    // When: search.take returns None, no child search remains to handle input.
                    return false;
                }
            }
        };
        let parser_arc = match self.windows.get(&win_id).and_then(|c| c.panes.get(&pane_id)) {
            Some(p) => p.parser.clone(),
            None => {
                // When: panes.get returns None for pane_id, restore the detached child search state.
                if let Some(child) = self.windows.get_mut(&win_id) {
                    if let Some(st) = child.tab_states.get_mut(i) {
                        st.search = Some(search);
                    }
                }
                return false;
            }
        };
        let grid_guard = parser_arc.lock();
        let grid = grid_guard.grid();
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(
            grid,
            self.windows
                .get(&win_id)
                .and_then(|ws| ws.panes.get(&pane_id))
                .and_then(|pane| pane.viewport_top_abs),
        );
        prepare_search(&mut search, pane_id, grid, view_top);
        search.input_str(text, grid);
        anchor_unfocused_search(&mut search, view_top);
        drop(grid_guard);
        if let Some(child) = self.windows.get_mut(&win_id) {
            if let Some(st) = child.tab_states.get_mut(i) {
                st.search = Some(search);
            }
            child.request_redraw();
        }
        true
    }

    pub(super) fn search_handle_key(&mut self, event: &KeyEvent, mods: ModifiersState) -> bool {
        let (i, pane_id) = {
            let Some(ws) = self.main() else {
                // When: main returns None, no main-window search can handle the key.
                return false;
            };
            let i = ws.tabs.active_index();
            let Some(t) = ws.tab_states.get(i) else {
                // When: tab_states.get cannot find i, no active tab can handle the search key.
                return false;
            };
            if t.search.is_none() {
                // When: search is None, the key belongs to another input route.
                return false;
            }
            (i, t.active_pane)
        };
        // Take the search state out of the tab so we can hold its
        // `&mut SearchState` alongside the parser's grid borrow without
        // double-borrowing through `self.main_mut()` and `self.panes`.
        let mut search = {
            let Some(ws) = self.main_mut() else {
                // When: main_mut returns None, the search state cannot be taken for key handling.
                return false;
            };
            let Some(st) = ws.tab_states.get_mut(i) else {
                // When: tab_states.get_mut cannot find i, the active search state is unavailable.
                return false;
            };
            match st.search.take() {
                Some(s) => s,
                None => {
                    // When: search.take returns None, no search state remains to handle the key.
                    return false;
                }
            }
        };
        let parser_arc = match self.main().and_then(|ws| ws.panes.get(&pane_id)) {
            Some(p) => p.parser.clone(),
            None => {
                // When: panes.get returns None for pane_id, restore the detached main search state.
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
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(
            grid,
            self.main()
                .and_then(|ws| ws.panes.get(&pane_id))
                .and_then(|pane| pane.viewport_top_abs),
        );
        prepare_search(&mut search, pane_id, grid, view_top);
        let (handled, keep_search, requested_view_top) =
            apply_search_key(&mut search, grid, event, mods, view_top);
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

    /// Child-window mirror of [`Self::search_handle_key`]: route a keystroke
    /// into the search box of the torn-out window `win_id`. Returns `true` if
    /// the key belonged to the search box (caller must not forward to the PTY).
    /// Shares `apply_search_key` with the main path so the two can't drift.
    pub(super) fn search_handle_key_in_child(
        &mut self,
        win_id: WindowId,
        event: &KeyEvent,
        mods: ModifiersState,
    ) -> bool {
        let (i, pane_id) = {
            let Some(child) = self.windows.get(&win_id) else {
                // When: windows.get cannot find win_id, no child search can handle the key.
                return false;
            };
            let i = child.tabs.active_index();
            let Some(t) = child.tab_states.get(i) else {
                // When: tab_states.get cannot find i, no child tab can handle the search key.
                return false;
            };
            if t.search.is_none() {
                // When: search is None, the child key belongs to another input route.
                return false;
            }
            (i, t.active_pane)
        };
        let mut search = {
            let Some(child) = self.windows.get_mut(&win_id) else {
                // When: windows.get_mut cannot find win_id, the child search state cannot be taken.
                return false;
            };
            let Some(st) = child.tab_states.get_mut(i) else {
                // When: tab_states.get_mut cannot find i, the child search state is unavailable.
                return false;
            };
            match st.search.take() {
                Some(s) => s,
                None => {
                    // When: search.take returns None, no child search remains to handle input.
                    return false;
                }
            }
        };
        let parser_arc = match self.windows.get(&win_id).and_then(|c| c.panes.get(&pane_id)) {
            Some(p) => p.parser.clone(),
            None => {
                // When: panes.get returns None for pane_id, restore the detached child search state.
                if let Some(child) = self.windows.get_mut(&win_id) {
                    if let Some(st) = child.tab_states.get_mut(i) {
                        st.search = Some(search);
                    }
                }
                return false;
            }
        };
        let grid_guard = parser_arc.lock();
        let grid = grid_guard.grid();
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(
            grid,
            self.windows
                .get(&win_id)
                .and_then(|ws| ws.panes.get(&pane_id))
                .and_then(|pane| pane.viewport_top_abs),
        );
        prepare_search(&mut search, pane_id, grid, view_top);
        let (handled, keep_search, requested_view_top) =
            apply_search_key(&mut search, grid, event, mods, view_top);
        drop(grid_guard);
        if let Some(child) = self.windows.get_mut(&win_id) {
            if let Some(view_top) = requested_view_top {
                if let Some(pane) = child.panes.get_mut(&pane_id) {
                    pane.viewport_top_abs = view_top;
                }
                mark_all_panes_dirty(&child.panes);
            }
            if keep_search {
                if let Some(st) = child.tab_states.get_mut(i) {
                    st.search = Some(search);
                }
            }
            child.request_redraw();
        }
        handled
    }
}

pub(super) fn prepare_search(
    search: &mut sonicterm_ui::search::SearchState,
    pane_id: u64,
    grid: &Grid,
    view_top: u64,
) {
    search.bind_pane(pane_id);
    search.maybe_refresh_for_revision(grid);
    anchor_unfocused_search(search, view_top);
}

fn anchor_unfocused_search(search: &mut sonicterm_ui::search::SearchState, view_top: u64) {
    if search.current.is_none() {
        // Query edits and identity changes refocus from the viewed rows without moving them.
        search.anchor_to_viewport(view_top);
    }
}

fn search_row_visible(row: u32, view_top: u64, rows: u16) -> bool {
    (view_top..view_top.saturating_add(u64::from(rows))).contains(&u64::from(row))
}

fn navigate_search(
    search: &mut sonicterm_ui::search::SearchState,
    view_top: u64,
    rows: u16,
    backwards: bool,
) {
    anchor_unfocused_search(search, view_top);
    if let Some(current) = search.current_match() {
        // When: current exists, first check visibility to avoid skipping an unseen query result.
        if !search_row_visible(current.row, view_top, rows) {
            // When: current is offscreen, reveal it before advancing so the initial result cannot be skipped.
            search.requested_scroll_row = Some(current.row);
            return;
        }
    }
    if backwards {
        search.prev();
    } else {
        // When: backwards is false, follow document order and let next wrap at the end.
        search.next();
    }
}

fn take_search_scroll(
    search: &mut sonicterm_ui::search::SearchState,
    grid: &Grid,
    view_top: u64,
) -> Option<Option<u64>> {
    let row = search.requested_scroll_row.take()?;
    (!search_row_visible(row, view_top, grid.rows)).then(|| centered_search_view_top(grid, row))
}

fn centered_search_view_top(grid: &Grid, row: u32) -> Option<u64> {
    let live_top = grid.scrollback_len() as u64;
    let half = u64::from(grid.rows) / 2;
    let desired = u64::from(row).saturating_sub(half).min(live_top);
    (desired < live_top).then_some(desired)
}

/// Pure core of search-box key handling, shared by the main-window
/// (`search_handle_key`) and child-window (`search_handle_key_in_child`)
/// paths so the two can't drift. Mutates `search` in place against `grid`
/// and returns `(handled, keep_search, requested_view_top)`.
///
/// `handled` = the key belonged to the search box (don't forward to PTY).
/// `keep_search` = leave the box open afterwards (Escape returns false).
/// `requested_view_top` = a scrollback view-top to apply so the matched
/// row is centered, or `None`.
/// `requested_view_top` = `Some(view_top_option)` to apply (where the inner
/// `None` means "snap to live bottom"), or outer `None` for "no view change".
fn apply_search_key(
    search: &mut sonicterm_ui::search::SearchState,
    grid: &Grid,
    event: &KeyEvent,
    mods: ModifiersState,
    view_top: u64,
) -> (bool, bool, Option<Option<u64>>) {
    let edit = super::text_edit::search_text_edit_for_event(event, mods);
    let (handled, keep_search) = if let Some(edit) = edit {
        search.apply_text_edit(edit, grid);
        (true, true)
    } else {
        // When: edit is None, interpret logical_key as search navigation or text input.
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => (true, false),
            Key::Named(NamedKey::Enter) => {
                navigate_search(search, view_top, grid.rows, mods.shift_key());
                (true, true)
            }
            Key::Named(NamedKey::ArrowDown) => {
                navigate_search(search, view_top, grid.rows, false);
                (true, true)
            }
            Key::Named(NamedKey::ArrowUp) => {
                navigate_search(search, view_top, grid.rows, true);
                (true, true)
            }
            Key::Named(NamedKey::Backspace) => {
                search.backspace(grid);
                (true, true)
            }
            Key::Named(NamedKey::Space) => {
                if let Some(text) = super::text_edit::printable_event_text(event, mods) {
                    for ch in text.chars() {
                        search.input_char(ch, grid);
                    }
                }
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
                            navigate_search(search, view_top, grid.rows, mods.shift_key());
                            consumed = true;
                        }
                        _ => {
                            // When: s is not i, r, or g, leave the command modifier key unconsumed.
                        }
                    }
                }
                if !consumed {
                    for ch in super::text_edit::printable_event_text(event, mods)
                        .unwrap_or_default()
                        .chars()
                    {
                        search.input_char(ch, grid);
                    }
                }
                (true, true)
            }
            _ =>
            // Unmatched logical keys remain available to another input route.
            {
                (false, true)
            }
        }
    };
    anchor_unfocused_search(search, view_top);
    let requested_view_top = if handled && keep_search {
        take_search_scroll(search, grid, view_top)
    } else {
        // When: handled or keep_search is false, request no search-driven viewport change.
        None
    };
    (handled, keep_search, requested_view_top)
}

#[cfg(test)]
#[path = "search_handle_tests.rs"]
mod search_handle_tests;
