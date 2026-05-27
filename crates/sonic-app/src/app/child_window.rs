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
use sonic_ui::prefs::PrefsHit;

impl App {
    pub(super) fn handle_child_window_event(
        &mut self,
        el: &ActiveEventLoop,
        win_id: WindowId,
        event: WindowEvent,
    ) {
        let theme = self.theme.clone();
        let Some(child) = self.child_windows.get_mut(&win_id) else { return };
        match event {
            WindowEvent::CloseRequested => {
                // Clear redraw targets so the VT thread stops trying
                // to redraw a dropped window (it will then notice the
                // pty channel close on Drop and exit). Dropping the
                // ChildWindow drops PaneState → PtyHandle → kills the
                // child shells.
                if let Some(removed) = self.child_windows.remove(&win_id) {
                    for pane in removed.panes.values() {
                        *pane.redraw_target.lock() = None;
                    }
                    drop(removed);
                }
                // If this was the last child AND the main window had
                // been previously drained/hidden, nothing is alive
                // anymore — exit the loop.
                if self.should_exit() {
                    el.exit();
                }
            }
            WindowEvent::RedrawRequested => {
                let tab_idx = child.tabs.active_index();
                let pane_rects: Vec<(u64, sonic_ui::pane::Rect)> = child
                    .tab_states
                    .get(tab_idx)
                    .map(|st| {
                        let (w, h) = child.renderer.logical_size();
                        let top = child.renderer.top_inset();
                        let pl = child.renderer.padding_left();
                        let pr = child.renderer.padding_right();
                        let pb = child.renderer.padding_bottom();
                        let outer = sonic_ui::pane::Rect::new(
                            pl,
                            top,
                            (w - pl - pr).max(0.0),
                            (h - top - pb).max(0.0),
                        );
                        st.tree.layout(outer)
                    })
                    .unwrap_or_default();
                let active_id = child.tab_states.get(tab_idx).map(|st| st.active_pane).unwrap_or(0);
                if let Some(pane) = child.panes.get(&active_id) {
                    let mut grid = pane.parser.lock();
                    if let Some(search) =
                        child.tab_states.get_mut(tab_idx).and_then(|t| t.search.as_mut())
                    {
                        search.maybe_refresh_for_revision(grid.grid_mut());
                    }
                    let search = child.tab_states.get(tab_idx).and_then(|t| t.search.as_ref());
                    if let Err(e) = child.renderer.render(
                        grid.grid_mut(),
                        &theme,
                        child.cursor_visible.load(std::sync::atomic::Ordering::Relaxed),
                        child.selection.as_ref(),
                        &child.tabs,
                        &pane_rects,
                        active_id,
                        search,
                        None, // command palette: not exposed in child window yet
                        None, // ime preedit: not exposed in child window yet
                        pane.viewport_top_abs,
                    ) {
                        tracing::warn!("child render error: {e}");
                    }
                    child.last_render = Instant::now();
                }
            }
            WindowEvent::Resized(size) => {
                child.renderer.resize(size.width, size.height);
                let (cols, rows) = child.renderer.cells();
                for pane in child.panes.values() {
                    pane.parser.lock().grid_mut().resize(cols, rows);
                    if let Some(pty) = pane.pty.as_ref() {
                        (pty.resize)(cols, rows);
                    }
                }
                child.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                child.renderer.set_scale_factor(scale_factor as f32);
                child.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => {
                child.modifiers = m.state();
            }
            WindowEvent::CursorLeft { .. } => {
                let changed = child.renderer.set_hover_cursor(None);
                if changed {
                    child.window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                child.cursor_pos = (position.x, position.y);
                let sf = child.renderer.scale_factor();
                let (lx, ly) = to_logical_pos(position.x, position.y, sf);
                // Child window also drives the close-button hover dance
                // through its OWN renderer — without this push the dim
                // × stays the wrong brightness when the cursor crosses
                // the glyph in a torn-out window.
                if child.renderer.set_hover_cursor(Some((lx, ly))) {
                    child.window.request_redraw();
                }
                if let Some(s) = child.drag_session.as_mut() {
                    s.current_pos = (lx, ly);
                    let title = child
                        .tabs
                        .tabs()
                        .get(s.press_tab_index)
                        .map(|t| t.title.clone())
                        .unwrap_or_default();
                    let session_snapshot = *s;
                    let bar_width = child.renderer.width() as f32 / child.renderer.scale_factor();
                    let layout = TabBarLayout::compute_with_height(
                        &child.tabs,
                        bar_width,
                        child.renderer.tab_bar_logical_height(),
                    )
                    .with_top_offset(child.renderer.titlebar_inset())
                    .with_visible(child.renderer.tab_bar_visible());
                    let chip =
                        crate::tab_drag::build_drag_chip_overlay(&session_snapshot, &layout, title);
                    child.renderer.set_drag_chip(chip);
                }
                // Cross-window drag-merge from child: when a tab in the
                // child's bar is held, look for a destination on another
                // window (main or sibling). The final action (tear /
                // merge / cancel) is deferred to mouse-up.
                if child.mouse_down && child.pressed_tab.is_some() {
                    let local = (position.x, position.y);
                    // child borrow ends at last use; safe to call &mut self next
                    let _ = child;
                    let tgt = self.compute_child_drag_target(win_id, local);
                    if let Some(c) = self.child_windows.get_mut(&win_id) {
                        c.drag_target = tgt;
                        c.window.request_redraw();
                    }
                    return;
                }
                if child.mouse_down {
                    if let Some((row, col)) =
                        child.renderer.pixel_to_cell(position.x as f32, position.y as f32)
                    {
                        if let Some(sel) = child.selection.as_mut() {
                            sel.extend(row, col);
                            mark_all_panes_dirty(&child.panes);
                            child.window.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => match state {
                ElementState::Pressed => {
                    let sf = child.renderer.scale_factor();
                    let (px, py) = to_logical_pos(child.cursor_pos.0, child.cursor_pos.1, sf);
                    let bar_width = child.renderer.width() as f32 / sf;
                    let layout = TabBarLayout::compute_with_height(
                        &child.tabs,
                        bar_width,
                        child.renderer.tab_bar_logical_height(),
                    )
                    .with_top_offset(child.renderer.titlebar_inset())
                    .with_visible(child.renderer.tab_bar_visible());
                    if let Some(hit) = layout.hit(px, py) {
                        match hit {
                            TabHit::Activate(i) => {
                                child.tabs.activate(i);
                                child.pressed_tab = Some(i);
                                child.mouse_down = true;
                                child.drag_session =
                                    Some(crate::tab_drag::DragSession::new(i, (px, py)));
                            }
                            TabHit::Close(_) | TabHit::NewTab => {
                                // close/new-tab in child are deferred —
                                // single-tab children today. Swallow.
                            }
                        }
                        child.window.request_redraw();
                        return;
                    }
                    child.mouse_down = true;
                    // `pixel_to_cell` still expects PHYSICAL px (it
                    // divides by scale_factor internally — PR #76).
                    if let Some((row, col)) = child
                        .renderer
                        .pixel_to_cell(child.cursor_pos.0 as f32, child.cursor_pos.1 as f32)
                    {
                        child.selection = Some(Selection::new(row, col));
                        mark_all_panes_dirty(&child.panes);
                    }
                    child.window.request_redraw();
                }
                ElementState::Released => {
                    let session = child.drag_session.take();
                    let foreign = child.drag_target.take();
                    let pressed = child.pressed_tab.take();
                    child.mouse_down = false;
                    child.renderer.set_drag_chip(None);
                    if let Some(sel) = child.selection.as_ref() {
                        if sel.is_empty() {
                            child.selection = None;
                            mark_all_panes_dirty(&child.panes);
                            child.window.request_redraw();
                        }
                    }
                    if let (Some(s), Some(src_idx)) = (session, pressed) {
                        let sf = child.renderer.scale_factor();
                        let bar_width = child.renderer.width() as f32 / sf;
                        let layout = TabBarLayout::compute_with_height(
                            &child.tabs,
                            bar_width,
                            child.renderer.tab_bar_logical_height(),
                        )
                        .with_top_offset(child.renderer.titlebar_inset());
                        let action = crate::tab_drag::compute_action(&s, foreign, &layout);
                        // Release the child borrow before re-entering
                        // &mut self via the merge / tear path.
                        let _ = child;
                        match action {
                            crate::tab_drag::DragAction::ReturnToOriginalBar => {
                                // No-op cancel.
                            }
                            crate::tab_drag::DragAction::ReorderTab { from, to } => {
                                // Re-borrow via self.child_windows
                                // because `let _ = child;` above
                                // released the long-lived mut borrow.
                                if let Some(c) = self.child_windows.get_mut(&win_id) {
                                    c.tabs.reorder(from, to);
                                    if from < c.tab_states.len() && to < c.tab_states.len() {
                                        let st = c.tab_states.remove(from);
                                        c.tab_states.insert(to, st);
                                    }
                                    c.window.request_redraw();
                                }
                            }
                            crate::tab_drag::DragAction::MergeIntoWindow(target) => {
                                self.merge_child_into_target(win_id, src_idx, target);
                            }
                            crate::tab_drag::DragAction::TearOutToNewWindow { .. } => {
                                // Tearing out of a child today is a
                                // no-op: single-tab children are the
                                // common case and re-parenting the
                                // only tab would be visually identical.
                            }
                        }
                    }
                }
            },
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let mods = child.modifiers;
                let tab_idx = child.tabs.active_index();
                let active_id = match child.tab_states.get(tab_idx) {
                    Some(st) => st.active_pane,
                    None => return,
                };
                if let Some(bytes) = encode_key(&event, mods) {
                    if let Some(pane) = child.panes.get(&active_id) {
                        if let Some(pty) = pane.pty.as_ref() {
                            let _ = pty.in_tx.send(bytes);
                        }
                    }
                    if child.selection.is_some() {
                        child.selection = None;
                        mark_all_panes_dirty(&child.panes);
                        child.window.request_redraw();
                    }
                }
            }
            _ => {}
        }
    }
}

impl App {
    pub(super) fn merge_child_into_target(
        &mut self,
        src_id: WindowId,
        src_idx: usize,
        target: crate::tab_drag::DropTarget<WindowId>,
    ) {
        let Some((tab, state, panes)) = self.detach_from_child(src_id, src_idx) else { return };
        let main_id = self.window.as_ref().map(|w| w.id());
        let attached = if Some(target.window) == main_id {
            self.attach_tab_state(target.slot, tab, state, panes);
            // Receiving a tab back into main un-hides the window if it
            // had been drained.
            if self.main_hidden {
                self.show_main_window();
            }
            true
        } else {
            self.attach_to_child(target.window, target.slot, tab, state, panes)
        };
        if !attached {
            tracing::warn!(
                "drag-merge: destination {:?} disappeared mid-drop; panes dropped",
                target.window
            );
        }
        self.reap_empty_child(src_id);
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    pub(super) fn reap_empty_child(&mut self, win_id: WindowId) {
        if let Some(child) = self.child_windows.get(&win_id) {
            if child.tabs.is_empty() {
                if let Some(removed) = self.child_windows.remove(&win_id) {
                    // panes map should already be empty; defensively
                    // null out any stragglers' redraw targets.
                    for pane in removed.panes.values() {
                        *pane.redraw_target.lock() = None;
                    }
                    drop(removed);
                    tracing::info!(
                        "child window reaped after drag-merge; remaining children={}",
                        self.child_windows.len()
                    );
                }
            }
        }
    }
    pub(super) fn merge_main_into_child(
        &mut self,
        src_idx: usize,
        target: crate::tab_drag::DropTarget<WindowId>,
    ) {
        let Some((tab, state, panes)) = self.detach_tab_state(src_idx) else { return };
        if !self.attach_to_child(target.window, target.slot, tab, state, panes) {
            tracing::warn!(
                "drag-merge: destination child {:?} disappeared mid-drop; panes dropped",
                target.window
            );
        }
        // If main has been drained but child windows are still alive,
        // hide the main window without exiting the app.
        if self.tabs.is_empty() && !self.child_windows.is_empty() {
            self.hide_main_window();
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
    pub(super) fn hide_main_window(&mut self) {
        if let Some(w) = &self.window {
            w.set_visible(false);
        }
        self.main_hidden = true;
        tracing::info!("main window hidden (drained); child_windows={}", self.child_windows.len());
    }
    pub(super) fn show_main_window(&mut self) {
        if let Some(w) = &self.window {
            w.set_visible(true);
        }
        self.main_hidden = false;
    }
}
