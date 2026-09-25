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
    /// Apply committed text to the source window's search without changing its viewed rows.
    pub(super) fn search_handle_ime_commit(&mut self, win_id: WindowId, text: &str) -> bool {
        let Some(window) = self.windows.get_mut(&win_id) else {
            // When: win_id is gone, search input cannot select another window.
            return false;
        };
        let i = window.tabs.active_index();
        let Some(tab) = window.tab_states.get_mut(i) else {
            // When: tab_states has no active entry, there is no search owner.
            return false;
        };
        let Some(mut search) = tab.search.take() else {
            // When: search is absent, the commit belongs to another input owner.
            return false;
        };
        let pane_id = tab.active_pane;
        let Some(pane) = window.panes.get(&pane_id) else {
            // When: pane_id is missing, retain the open search rather than losing its query.
            tab.search = Some(search);
            return false;
        };
        let parser_arc = pane.parser.clone();
        let viewport_top_abs = pane.viewport_top_abs;
        let grid_guard = parser_arc.lock();
        let grid = grid_guard.grid();
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(grid, viewport_top_abs);
        prepare_search(&mut search, pane_id, grid, view_top);
        search.input_str(text, grid);
        anchor_unfocused_search(&mut search, view_top);
        drop(grid_guard);
        tab.search = Some(search);
        window.request_redraw();
        true
    }

    /// Apply a search keystroke only to the named window's active tab and viewport.
    pub(super) fn search_handle_key(
        &mut self,
        win_id: WindowId,
        event: &KeyEvent,
        mods: ModifiersState,
    ) -> bool {
        self.search_handle_key_parts(
            win_id,
            &event.logical_key,
            mods,
            super::text_edit::search_text_edit_for_event(event, mods),
            super::text_edit::printable_event_text(event, mods),
        )
    }

    /// Apply resolved search input without requiring a platform-owned native event object.
    pub(super) fn search_handle_key_parts(
        &mut self,
        win_id: WindowId,
        key: &Key,
        mods: ModifiersState,
        edit: Option<sonicterm_ui::text_edit::TextEdit>,
        text: Option<&str>,
    ) -> bool {
        let Some(window) = self.windows.get_mut(&win_id) else {
            // When: win_id is gone, search input cannot select another window.
            return false;
        };
        let i = window.tabs.active_index();
        let Some(tab) = window.tab_states.get_mut(i) else {
            // When: tab_states has no active entry, there is no search owner.
            return false;
        };
        let Some(mut search) = tab.search.take() else {
            // When: search is absent, the key belongs to another input owner.
            return false;
        };
        let pane_id = tab.active_pane;
        let Some(pane) = window.panes.get(&pane_id) else {
            // When: pane_id is missing, retain the open search rather than losing its query.
            tab.search = Some(search);
            return false;
        };
        let parser_arc = pane.parser.clone();
        let viewport_top_abs = pane.viewport_top_abs;
        let grid_guard = parser_arc.lock();
        let grid = grid_guard.grid();
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(grid, viewport_top_abs);
        prepare_search(&mut search, pane_id, grid, view_top);
        let (handled, keep_search, requested_view_top) =
            apply_search_key(&mut search, grid, key, mods, edit, text, view_top);
        drop(grid_guard);
        if keep_search {
            tab.search = Some(search);
        }
        if let Some(view_top) = requested_view_top {
            if let Some(pane) = window.panes.get_mut(&pane_id) {
                pane.viewport_top_abs = view_top;
            }
            mark_all_panes_dirty(&window.panes);
        }
        window.request_redraw();
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

/// Return (handled, keep_search, viewport update); an inner None restores live output.
fn apply_search_key(
    search: &mut sonicterm_ui::search::SearchState,
    grid: &Grid,
    key: &Key,
    mods: ModifiersState,
    edit: Option<sonicterm_ui::text_edit::TextEdit>,
    text: Option<&str>,
    view_top: u64,
) -> (bool, bool, Option<Option<u64>>) {
    let (handled, keep_search) = if let Some(edit) = edit {
        search.apply_text_edit(edit, grid);
        (true, true)
    } else {
        // When: edit is None, interpret logical_key as search navigation or text input.
        match key {
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
            Key::Named(NamedKey::Backspace)
                if !mods.intersects(
                    ModifiersState::CONTROL | ModifiersState::ALT | ModifiersState::SUPER,
                ) =>
            {
                search.backspace(grid);
                (true, true)
            }
            Key::Named(NamedKey::Space) => {
                if let Some(text) = text {
                    search.input_key_text(text, grid);
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
                    search.input_key_text(text.unwrap_or_default(), grid);
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
