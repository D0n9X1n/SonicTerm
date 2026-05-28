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
                // PR #199 Fix 1: try_lock EVERY pane in this child window's
                // tab and pass them all through to the renderer. Mirrors
                // the main-window path in window_event.rs.
                let parser_arcs: Vec<(
                    u64,
                    std::sync::Arc<parking_lot::Mutex<sonic_core::vt::Parser>>,
                    sonic_ui::pane::Rect,
                )> = pane_rects
                    .iter()
                    .filter_map(|(id, rect)| {
                        child.panes.get(id).map(|p| (*id, std::sync::Arc::clone(&p.parser), *rect))
                    })
                    .collect();
                let mut guards: Vec<(
                    u64,
                    parking_lot::MutexGuard<'_, sonic_core::vt::Parser>,
                    sonic_ui::pane::Rect,
                )> = Vec::with_capacity(parser_arcs.len());
                let mut all_locked = true;
                for (id, arc, rect) in &parser_arcs {
                    match arc.try_lock() {
                        Some(g) => {
                            // SAFETY: `parser_arcs` outlives `guards`; see
                            // window_event.rs Fix 1 for the full lifetime
                            // argument.
                            let g_ext: parking_lot::MutexGuard<'_, sonic_core::vt::Parser> =
                                unsafe { std::mem::transmute(g) };
                            guards.push((*id, g_ext, *rect));
                        }
                        None => {
                            all_locked = false;
                            break;
                        }
                    }
                }
                if !all_locked {
                    drop(guards);
                    drop(parser_arcs);
                    child.window.request_redraw();
                    return;
                }
                if let Some(pane) = child.panes.get(&active_id) {
                    let active_pos = guards
                        .iter()
                        .position(|(id, _, _)| *id == active_id)
                        // PANIC: safe — `guards` is populated immediately
                        // above in the same fn from the same `child.panes`
                        // map keyed by `active_id`, so a guard with this id
                        // must exist. Render hot path: no Result conversion.
                        .expect("active pane guard collected above");
                    if let Some(search) =
                        child.tab_states.get_mut(tab_idx).and_then(|t| t.search.as_mut())
                    {
                        search.maybe_refresh_for_revision(guards[active_pos].1.grid_mut());
                    }
                    let search = child.tab_states.get(tab_idx).and_then(|t| t.search.as_ref());
                    let mut panes_slice: Vec<sonic_render_model::PaneRender<'_>> = guards
                        .iter_mut()
                        .map(|(id, g, rect)| sonic_render_model::PaneRender {
                            id: *id,
                            rect_px: sonic_render_model::geometry::PixelRect {
                                x: rect.x as i32,
                                y: rect.y as i32,
                                w: rect.w as u32,
                                h: rect.h as u32,
                            },
                            grid: g.grid_mut(),
                            is_active: *id == active_id,
                            cursor_style: sonic_render_model::CursorStyle::default(),
                            is_broadcast_receiver: false,
                        })
                        .collect();
                    if let Err(e) = child.renderer.render(
                        &mut panes_slice,
                        &theme,
                        child.cursor_visible.load(std::sync::atomic::Ordering::Relaxed),
                        child.selection.as_ref(),
                        &child.tabs,
                        search,
                        None, // command palette: not exposed in child window yet
                        None, // keymap cheat sheet: not exposed in child window yet
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
            WindowEvent::Focused(focused) => {
                // Record focus so menubar-routed actions (Cmd+T, ...)
                // target this child window instead of the main App.
                // Release the child borrow before touching `self`.
                let _ = child;
                if focused {
                    self.focused_child = Some(win_id);
                } else if self.focused_child == Some(win_id) {
                    // Lost focus → clear; if another window claims it,
                    // its own Focused(true) arm will set it.
                    self.focused_child = None;
                }
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

    /// Spawn a new tab containing a single fresh pane inside the
    /// child window identified by `win_id`. Returns `false` if no
    /// such child window exists (caller should fall back to the main
    /// App's `new_tab`). The new pane's redraw target is bound to the
    /// child window so VT output redraws the child, not the main App.
    ///
    /// This is the routing target for `Action::NewTab` whenever
    /// `App::focused_child` points at a torn-out window — without it
    /// Cmd+T in the child silently spawned the tab in the main App.
    pub(super) fn spawn_tab_in_child(&mut self, win_id: WindowId) -> bool {
        use sonic_core::{grid::Grid, vt::Parser};
        // Snapshot everything we need from the child up-front so the
        // mutable borrow ends before we spawn the VT thread (which
        // captures clones), then re-borrow to install the new tab.
        let (cols, rows, child_window, cursor_visible_arc) = {
            let Some(child) = self.child_windows.get_mut(&win_id) else {
                return false;
            };
            let (c, r) = child.renderer.cells();
            (c, r, child.window.clone(), child.cursor_visible.clone())
        };
        let (reply_tx, reply_rx) = crossbeam_channel::unbounded::<Vec<u8>>();
        let parser = Arc::new(Mutex::new(Parser::new_with_reply(Grid::new(cols, rows), reply_tx)));
        let redraw_target: Arc<Mutex<Option<Arc<Window>>>> =
            Arc::new(Mutex::new(Some(child_window.clone())));
        let pty = match PtyHandle::spawn_default_shell(cols, rows) {
            Ok(pty) => {
                let parser_clone = parser.clone();
                let out_rx = pty.out_rx.clone();
                let in_tx_reply = pty.in_tx.clone();
                let redraw_target_thread = redraw_target.clone();
                let cursor_visible = cursor_visible_arc;
                let pty_burst_gen = self.pty_burst_gen.clone();
                std::thread::Builder::new()
                    .name("sonic-vt-reply-child".into())
                    .spawn(move || {
                        while let Ok(bytes) = reply_rx.recv() {
                            if in_tx_reply.send(bytes).is_err() {
                                break;
                            }
                        }
                    })
                    // PANIC: thread spawn at pane init — see sonic-io/pty.rs
                    // rationale. Unrecoverable OS-level failure.
                    .expect("spawn vt reply forwarder (child)");
                std::thread::Builder::new()
                    .name("sonic-vt-loop-child".into())
                    .spawn(move || {
                        // Lean variant of the main-window VT loop: drain
                        // bytes, advance parser, request a redraw. The
                        // child window does not currently update its
                        // title from OSC 0/2 (single-tab tear-out today),
                        // so we skip the title plumbing.
                        while let Ok(bytes) = out_rx.recv() {
                            if !bytes.is_empty() {
                                let prev = pty_burst_gen.fetch_add(1, Ordering::Release);
                                crate::app::invariants::debug_assert_burst_gen_monotonic(
                                    prev,
                                    prev.wrapping_add(1),
                                );
                            }
                            {
                                let mut p = parser_clone.lock();
                                for ev in p.advance(&bytes) {
                                    if let VtEvent::CursorVisibility(v) = ev {
                                        cursor_visible
                                            .store(v, std::sync::atomic::Ordering::Relaxed);
                                    }
                                }
                            }
                            if let Some(w) = redraw_target_thread.lock().as_ref() {
                                w.request_redraw();
                            }
                        }
                    })
                    // PANIC: thread spawn at pane init — see sonic-io/pty.rs
                    // rationale. Unrecoverable OS-level failure.
                    .expect("spawn vt loop (child)");
                Some(pty)
            }
            Err(e) => {
                tracing::error!("failed to spawn pty for child tab: {e}");
                None
            }
        };
        let mut pane_state = PaneState::new(parser, pty);
        pane_state.redraw_target = redraw_target;
        let pane_id = next_pane_id();
        let Some(child) = self.child_windows.get_mut(&win_id) else {
            return false;
        };
        child.panes.insert(pane_id, pane_state);
        let n = child.tabs.len() + 1;
        child.tabs.push(Tab::new(format!("shell {n}")));
        child.tab_states.push(TabState {
            tree: PaneTree::leaf(pane_id),
            active_pane: pane_id,
            search: None,
        });
        let last = child.tabs.len().saturating_sub(1);
        child.tabs.activate(last);
        child.window.request_redraw();
        true
    }
}
