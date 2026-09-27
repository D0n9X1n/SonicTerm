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
use sonicterm_ui::command_palette::{CommandPaletteMode, PaletteEntry, TabColorChoice};
use sonicterm_ui::overlays::{
    command_palette_query_caret_prefix, PaletteLayout, PALETTE_ROW_PAD_X,
};
use sonicterm_ui::pane::PaneTree;
use sonicterm_ui::search::SearchState;
use sonicterm_ui::selection::Selection;
use sonicterm_ui::tabbar_view::{TabBarLayout, TabHit};
use sonicterm_ui::tabs::{Tab, TabBar};
use sonicterm_vt::vt::{Parser, VtEvent};
use winit::{
    event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowAttributes, WindowId},
};

use super::text_edit;
use super::{
    key_encoding::{encode_key, encode_logical, key_event_to_string, key_name},
    mark_all_panes_dirty, next_pane_id, pick_prompt_target, resize_all_panes, shell_quote_posix,
    with_integrated_titlebar, wrap_paste, App, FrontmostKind, PaneState, TabState, UserEvent,
    WindowState,
};

mod palette_ime;
mod palette_keys;
mod palette_pointer;
mod tab_edit;
mod window_rename;

pub(super) use palette_pointer::PalettePointerCapture;
pub(super) use tab_edit::TabEditTarget;

/// Derive window-local command facts; an optional grid is the caller's already-held active-pane view.
pub(super) fn command_palette_context(
    window: &WindowState,
    active_grid: Option<&Grid>,
) -> sonicterm_ui::command_label::CommandContext {
    use sonicterm_ui::command_label::CommandContext;
    if window.hidden {
        // When: `window` is hidden, it cannot lend command targets to an attached palette.
        return CommandContext::default();
    }
    let tab = window.tab_states.get(window.tabs.active_index());
    let active_pane = tab.map(|tab| tab.active_pane).filter(|id| window.panes.contains_key(id));
    let selection_available = active_pane.is_some_and(|pane_id| {
        let valid = |grid: &Grid| {
            window.selection.is_some_and(|mut selection| {
                !selection.is_empty()
                    && !sonicterm_ui::selection::revalidate_selection(&mut selection, pane_id, grid)
            })
        };
        match active_grid {
            Some(grid) => valid(grid),
            None => window
                .panes
                .get(&pane_id)
                .and_then(|pane| pane.parser.try_lock())
                .is_some_and(|parser| valid(parser.grid())),
        }
    });
    let focus_available =
        tab.map_or([None; 4], |tab| tab.tree.focus_neighbors(tab.active_pane)).map(|neighbor| {
            neighbor.is_some_and(|id| active_pane.is_some() && window.panes.contains_key(&id))
        });
    CommandContext {
        window_available: true,
        tab_count: window.tabs.len(),
        pane_available: active_pane.is_some(),
        selection_available,
        read_only: window.copy_mode.as_ref().is_some_and(|state| state.is_read_only()),
        focus_available,
    }
}

impl App {
    pub(super) fn refresh_command_palette_context(&mut self) {
        use sonicterm_ui::command_label::CommandContext;
        let window_id = if self.command_palette.is_open() {
            self.palette_attached_window.or(self.main_window_id)
        } else {
            // When: `command_palette` is closed, the next open follows the existing frontmost policy.
            match self.frontmost_kind() {
                FrontmostKind::Child(id) => Some(id),
                _ => self.main_window_id,
            }
        };
        let window = window_id.and_then(|id| self.windows.get(&id)).filter(|window| !window.hidden);
        let context = window
            .map_or_else(CommandContext::default, |window| command_palette_context(window, None));
        self.command_palette.set_context(context);
        let empty = TabBar::new();
        self.command_palette
            .set_tabs(window.map(|window| &window.tabs).unwrap_or(&empty), &self.i18n);
    }

    /// Match native input to the window that owns the visible palette.
    pub(super) fn command_palette_owns_input(&self, window_id: WindowId) -> bool {
        self.command_palette.is_open()
            && self.palette_attached_window.or(self.main_window_id) == Some(window_id)
    }

    /// Open live-tab navigation for the explicit window without changing action routing ownership.
    pub(super) fn open_tab_selector(&mut self, window_id: WindowId) {
        if self.windows.get(&window_id).is_none_or(|window| window.hidden) {
            // When: `window_id` is missing or hidden, do not populate a selector from another window.
            return;
        }
        self.palette_pointer_capture = None;
        self.palette_attached_window =
            (Some(window_id) != self.main_window_id).then_some(window_id);
        // Seed the requested window before opening selects a row from the cached tab inventory.
        self.command_palette.set_tabs(&self.windows[&window_id].tabs, &self.i18n);
        self.command_palette.open_tabs();
        self.refresh_command_palette_context();
        self.update_command_palette_ime_cursor_area();
        self.request_redraw_for_overlay(self.palette_attached_window);
    }

    pub(super) fn toggle_command_palette(&mut self) {
        self.palette_pointer_capture = None;
        self.refresh_command_palette_context();
        let now_open = self.command_palette.toggle();
        // Notify the reducer of the toggle. The reducer flips `palette_open`
        // and emits Render(Overlay) on every transition.
        self.observe_intent(sonicterm_app_core::AppIntent::ToggleCommandPalette {
            window: sonicterm_types::WindowKey::new(0),
        });
        if now_open {
            // Tag with the frontmost window so the palette appears on
            // whatever window the user is looking at, rather than on the
            // main window's render pass.
            self.palette_attached_window = match self.frontmost_kind() {
                FrontmostKind::Child(id) => Some(id),
                _ => None,
            };
            self.update_command_palette_ime_cursor_area();
        } else {
            // When: now_open is false the toggle just closed the palette; drop
            // the attachment so later redraws do not target a stale window.
            self.palette_attached_window = None;
        }
        tracing::info!(
            open = now_open,
            attached = ?self.palette_attached_window,
            "command palette toggled"
        );
        self.draw_command_palette_overlay();
        // Synchronous redraw request so the palette appears on the very
        // next frame instead of waiting for the next pty/timer event.
        // Without this, ⌘⇧P / Ctrl+Shift+P has a noticeable visible
        // delay on an otherwise-idle terminal because no other event
        // wakes the event loop. Targets the attached window when set
        // so child windows get a redraw too, not just main.
        self.request_redraw_for_overlay(self.palette_attached_window);
    }

    pub(super) fn native_window_title(&self, id: WindowId) -> Option<String> {
        let key = self.window_keys.get(id)?;
        let window = self.windows.get(&id)?;
        Some(super::compose_window_title(key, &window.custom_window_name))
    }

    pub(crate) fn draw_command_palette_overlay(&self) {
        if !self.command_palette.is_open() {
            // When: command_palette is closed there is no query or selection
            // state to report; this helper only emits a trace line.
            return;
        }
        tracing::info!(
            query = %self.command_palette.query(),
            selected = self.command_palette.selected(),
            visible_count = self.command_palette.len(),
            "command palette overlay (visual TODO)"
        );
    }
    pub(super) fn open_search(&mut self) {
        // Notify the reducer of the open transition (Render(Overlay) —
        // transition-guarded so a re-open against an already-open overlay
        // is a no-op).
        self.observe_intent(sonicterm_app_core::AppIntent::OpenSearch {
            window: sonicterm_types::WindowKey::new(0),
        });
        // Cmd+F typed in a torn-out child opens a search bar on THAT child's
        // active tab, not the main window's.
        if let FrontmostKind::Child(id) = self.frontmost_kind() {
            // When: frontmost_kind reports Child the frontmost window is a
            // torn-out child, so route the search bar to its active tab.
            if self.open_search_in_child(id) {
                // When: open_search_in_child succeeded the child window owns
                // the new search bar; return so main does not open a second.
                return;
            }
            // Child id was stale — fall through to main, clear stale.
            self.frontmost_window = None;
        }
        let (i, pane_id) = {
            let Some(ws) = self.main() else {
                // When: main is absent before the window exists there is no tab
                // to hold the new SearchState; leave search unopened.
                return;
            };
            let i = ws.tabs.active_index();
            let Some(t) = ws.tab_states.get(i) else {
                // When: tab_states has no entry at active index i, tabs and
                // tab_states have diverged; open no search bar rather than guess.
                return;
            };
            (i, t.active_pane)
        };
        let mut s = SearchState::new();
        if let Some(pane) = self.main().and_then(|ws| ws.panes.get(&pane_id)) {
            s.refresh(pane.parser.lock().grid());
        }
        if let Some(ws) = self.main_mut() {
            if let Some(st) = ws.tab_states.get_mut(i) {
                st.search = Some(s);
            }
        }
        if let Some(w) = self.main_window() {
            w.request_redraw();
        }
    }

    /// Child-window mirror of `open_search`. Opens a search bar on the
    /// active tab of the given child window. Returns `true` on success,
    /// `false` if the recorded id is stale so the caller can fall back to
    /// the main App default.
    pub(super) fn open_search_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: win_id is no longer in windows the child closed since it
            // was recorded; return false so open_search falls back to main.
            return false;
        };
        let i = child.tabs.active_index();
        // When: tab_states has no entry at the child's active index i, tabs
        // and tab_states have diverged; report failure instead of guessing.
        let pane_id = match child.tab_states.get(i) {
            Some(t) => t.active_pane,
            None => return false,
        };
        let mut s = SearchState::new();
        if let Some(pane) = child.panes.get(&pane_id) {
            s.refresh(pane.parser.lock().grid());
        }
        if let Some(st) = child.tab_states.get_mut(i) {
            st.search = Some(s);
        }
        child.request_redraw();
        true
    }

    /// Redraw helper for app-level overlays (palette) that need to wake
    /// whichever window is currently hosting them. `None` ⇒ main window;
    /// `Some(id)` ⇒ that child window. Silently no-ops if the recorded id
    /// is stale.
    pub(super) fn request_redraw_for_overlay(&mut self, attached: Option<WindowId>) {
        if let Some(id) = attached.or(self.main_window_id) {
            self.mark_window_redraw(id, super::redraw::RedrawCause::Input);
        }
        match attached {
            Some(id) => {
                if let Some(child) = self.windows.get(&id) {
                    child.request_redraw();
                }
            }
            None => {
                if let Some(w) = self.main_window() {
                    w.request_redraw();
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "overlays_tests.rs"]
mod overlays_tests;
