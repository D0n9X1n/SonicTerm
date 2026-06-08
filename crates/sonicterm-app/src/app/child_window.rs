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
use sonicterm_ui::command_palette::CommandPalette;
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
    key_encoding::{encode_key, encode_logical, key_event_to_string, key_name, key_to_strings},
    mark_all_panes_dirty, next_pane_id, pick_prompt_target, poll_command_events_for_child_window,
    resize_all_panes, shell_quote_posix, with_integrated_titlebar, wrap_paste, App, PaneState,
    TabState, UserEvent, WindowState,
};

#[doc(hidden)]
pub fn resize_renderer_and_panes_if_present(
    renderer: &mut Option<GpuRenderer>,
    panes: &HashMap<u64, PaneState>,
    width: u32,
    height: u32,
) -> bool {
    let Some(r) = renderer.as_mut() else { return false };
    r.resize(width, height);
    let (cols, rows) = r.cells();
    for pane in panes.values() {
        pane.parser.lock().grid_mut().resize(cols, rows);
        if let Some(pty) = pane.pty.as_ref() {
            (pty.resize)(cols, rows);
        }
    }
    true
}

#[doc(hidden)]
pub fn apply_dpi_to_renderer_if_present(
    renderer: &mut Option<GpuRenderer>,
    dpi_scale: f64,
) -> bool {
    let Some(r) = renderer.as_mut() else { return false };
    r.set_scale_factor(dpi_scale as f32);
    true
}

#[doc(hidden)]
pub fn child_window_resized_handles_no_renderer(child: &mut WindowState, width: u32, height: u32) {
    if resize_renderer_and_panes_if_present(&mut child.renderer, &child.panes, width, height) {
        child.request_redraw();
    }
}

#[doc(hidden)]
pub fn child_window_dpi_changed_handles_no_renderer(child: &mut WindowState, dpi_scale: f64) {
    child.dpi_scale = dpi_scale;
    if apply_dpi_to_renderer_if_present(&mut child.renderer, dpi_scale) {
        child.request_redraw();
    }
}

impl App {
    pub(super) fn handle_child_window_event(
        &mut self,
        el: &ActiveEventLoop,
        win_id: WindowId,
        event: WindowEvent,
    ) {
        let theme = self.theme.clone();
        let config = self.config.clone();
        // Epic #289 follow-up: snapshot the app-level overlay
        // attachments here, BEFORE the mutable `child` borrow below
        // pins `self.windows` for the rest of the match. Used only by
        // the `RedrawRequested` arm but cheap enough to compute once.
        let palette_here = self.palette_attached_window == Some(win_id);
        // Split-borrow the palette out so the renderer can mutate it
        // even though `child` borrows `self.windows` below. Disjoint
        // fields — safe.
        let palette_for_render: Option<&mut CommandPalette> =
            if palette_here { Some(&mut self.command_palette) } else { None };
        let Some(child) = self.windows.get_mut(&win_id) else { return };
        match event {
            WindowEvent::CloseRequested => {
                // Clear redraw targets so the VT thread stops trying
                // to redraw a dropped window (it will then notice the
                // pty channel close on Drop and exit). Dropping the
                // WindowState drops PaneState → PtyHandle → kills the
                // child shells.
                if let Some(removed) = self.windows.remove(&win_id) {
                    for pane in removed.panes.values() {
                        *pane.redraw_target.lock() = None;
                    }
                    // Epic #289 Phase C2 — drop this window's tab bar
                    // snapshot so later OS drops can't false-positive
                    // hit-test against a stale rect.
                    self.os_drag_bars.remove(Some(win_id));
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
                child.tabs.clear_expired_command_badges(Instant::now());
                poll_command_events_for_child_window(child, &config);
                let tab_idx = child.tabs.active_index();
                let pane_rects: Vec<(u64, sonicterm_ui::pane::Rect)> = child
                    .tab_states
                    .get(tab_idx)
                    .and_then(|st| {
                        let r = child.renderer.as_ref()?;
                        let (w, h) = r.logical_size();
                        let top = (r.top_inset() - r.padding_top_px()).max(0.0);
                        let bottom = r.bottom_inset();
                        let outer = sonicterm_ui::pane::Rect::new(
                            0.0,
                            top,
                            w.max(0.0),
                            (h - top - bottom).max(0.0),
                        );
                        Some(st.tree.layout(outer))
                    })
                    .unwrap_or_default();
                let active_id = child.tab_states.get(tab_idx).map(|st| st.active_pane).unwrap_or(0);
                // PR #199 Fix 1: try_lock EVERY pane in this child window's
                // tab and pass them all through to the renderer. Mirrors
                // the main-window path in window_event.rs.
                let parser_arcs: Vec<(
                    u64,
                    std::sync::Arc<parking_lot::Mutex<sonicterm_vt::vt::Parser>>,
                    sonicterm_ui::pane::Rect,
                )> = pane_rects
                    .iter()
                    .filter_map(|(id, rect)| {
                        child.panes.get(id).map(|p| (*id, std::sync::Arc::clone(&p.parser), *rect))
                    })
                    .collect();
                let mut guards: Vec<(
                    u64,
                    parking_lot::MutexGuard<'_, sonicterm_vt::vt::Parser>,
                    sonicterm_ui::pane::Rect,
                )> = Vec::with_capacity(parser_arcs.len());
                let mut all_locked = true;
                for (id, arc, rect) in &parser_arcs {
                    match arc.try_lock() {
                        Some(g) => {
                            // SAFETY: `parser_arcs` outlives `guards`; see
                            // window_event.rs Fix 1 for the full lifetime
                            // argument.
                            let g_ext: parking_lot::MutexGuard<'_, sonicterm_vt::vt::Parser> =
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
                    child.request_redraw();
                    return;
                }
                let inline_images_by_pane: std::collections::HashMap<
                    u64,
                    Vec<sonicterm_render_model::InlineImage>,
                > = child
                    .panes
                    .iter()
                    .map(|(id, pane)| (*id, pane.inline_images.lock().clone()))
                    .collect();
                let viewport_tops: std::collections::HashMap<u64, Option<u64>> =
                    child.panes.iter().map(|(id, pane)| (*id, pane.viewport_top_abs)).collect();
                if let Some(pane) = child.panes.get_mut(&active_id) {
                    let active_pos = guards
                        .iter()
                        .position(|(id, _, _)| *id == active_id)
                        // PANIC: safe — `guards` is populated immediately
                        // above in the same fn from the same `child.panes`
                        // map keyed by `active_id`, so a guard with this id
                        // must exist. Render hot path: no Result conversion.
                        .expect("active pane guard collected above");
                    // Bug fix: child windows (Cmd+N, tear-out) were rendering
                    // their tab bar with the literal fallback title
                    // ("shell N") because the wezterm-style title formatter
                    // was only invoked on the main-window redraw path. Mirror
                    // the main-window logic so OSC 7 cwd + foreground-process
                    // probes flow into every window's tab bar uniformly.
                    let _ = crate::app::refresh_active_tab_title(
                        &mut child.tabs,
                        pane,
                        &guards[active_pos].1,
                        tab_idx,
                    );
                    if let Some(search) =
                        child.tab_states.get_mut(tab_idx).and_then(|t| t.search.as_mut())
                    {
                        search.maybe_refresh_for_revision(guards[active_pos].1.grid_mut());
                    }
                    let search = child.tab_states.get(tab_idx).and_then(|t| t.search.as_ref());
                    let mut panes_slice: Vec<sonicterm_render_model::PaneRender<'_>> = guards
                        .iter_mut()
                        .map(|(id, g, rect)| sonicterm_render_model::PaneRender {
                            id: *id,
                            rect_px: sonicterm_render_model::geometry::PixelRect {
                                x: rect.x as i32,
                                y: rect.y as i32,
                                w: rect.w as u32,
                                h: rect.h as u32,
                            },
                            grid: g.grid_mut(),
                            viewport_top_abs: viewport_tops.get(id).copied().flatten(),
                            is_active: *id == active_id,
                            cursor_style: sonicterm_render_model::CursorStyle::default(),
                            is_broadcast_receiver: false,
                            scrollbar_alpha: 0.0,
                            inline_images: inline_images_by_pane
                                .get(id)
                                .cloned()
                                .unwrap_or_default(),
                        })
                        .collect();
                    // PR #400: cursor_visible is per-pane (lives on
                    // PaneState). Read from the active pane (already
                    // borrowed mutably above) so the DECTCEM flag
                    // survives tear-out of this child.
                    let cursor_visible_now =
                        pane.cursor_visible.load(std::sync::atomic::Ordering::Relaxed);
                    if let Some(r) = child.renderer.as_mut() {
                        if let Err(e) = r.render(
                            &mut panes_slice,
                            &theme,
                            cursor_visible_now,
                            child.selection.as_ref(),
                            child.copy_mode.as_ref(),
                            &child.tabs,
                            search,
                            // Epic #289 follow-up: render the app-level
                            // command palette HERE when it
                            // was opened while this child window was OS
                            // frontmost. Pre-fix these were hardcoded to
                            // `None` so the palette silently appeared on
                            // the main window instead.
                            palette_for_render,
                            None, // ime preedit: not exposed in child window yet
                            pane.viewport_top_abs,
                        ) {
                            tracing::warn!("child render error: {e}");
                        }
                    }
                    child.last_render = Instant::now();
                    // Epic #289 Phase C2 — publish this child's tab bar
                    // snapshot for cross-window OS drag hit-tests. See
                    // `App::publish_child_window_tab_bar` for the
                    // rationale on the main-window mirror.
                    {
                        let Some(win) = child.window.as_ref() else { return };
                        let inner_origin =
                            win.inner_position().map(|p| (p.x, p.y)).unwrap_or((0, 0));
                        let isz = win.inner_size();
                        let inner_size = (isz.width, isz.height);
                        let raster_w = inner_size.0 as f32;
                        let Some(r) = child.renderer.as_ref() else { return };
                        let layout = TabBarLayout::compute_with_height(
                            &child.tabs,
                            raster_w,
                            r.tab_bar_logical_height(),
                        )
                        .with_top_offset(r.tab_bar_y_offset())
                        .with_visible(r.tab_bar_visible());
                        let snap = crate::app::os_drag::TabBarSnapshot::from_layout(
                            Some(win_id),
                            inner_origin,
                            inner_size,
                            &layout,
                        );
                        self.os_drag_bars.publish(snap);
                    }
                }
            }
            WindowEvent::Resized(size)
                if resize_renderer_and_panes_if_present(
                    &mut child.renderer,
                    &child.panes,
                    size.width,
                    size.height,
                ) =>
            {
                child.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor: dpi_scale, .. } => {
                child.dpi_scale = dpi_scale;
                if apply_dpi_to_renderer_if_present(&mut child.renderer, dpi_scale) {
                    child.request_redraw();
                }
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
                    // Epic #289 Phase A — unified frontmost tracker;
                    // discriminates main vs child via `frontmost_kind()`.
                    // PR-B4 (#365): `focused_child` removed — the child-only
                    // subset is now derivable from `frontmost_window`.
                    self.frontmost_window = Some(win_id);
                } else {
                    if self.frontmost_window == Some(win_id) {
                        // Same rule for frontmost: only clear if WE were
                        // the recorded one. A sibling sonic window's
                        // Focused(true) will arrive separately and
                        // overwrite.
                        self.frontmost_window = None;
                    }
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(r) = child.renderer.as_mut() {
                    let changed = r.set_hover_cursor(None);
                    if changed {
                        child.request_redraw();
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                child.cursor_pos = (position.x, position.y);
                let Some(r) = child.renderer.as_mut() else { return };
                let (lx, ly) = (position.x as f32, position.y as f32);
                // Child window also drives tab hover through its OWN
                // renderer so each torn-out window repaints independently.
                if r.set_hover_cursor(Some((lx, ly))) {
                    if let Some(w) = child.window.as_ref() {
                        w.request_redraw();
                    }
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
                    let bar_width = r.width() as f32;
                    let layout = TabBarLayout::compute_with_height(
                        &child.tabs,
                        bar_width,
                        r.tab_bar_logical_height(),
                    )
                    .with_top_offset(r.tab_bar_y_offset())
                    .with_visible(r.tab_bar_visible());
                    let chip =
                        crate::tab_drag::build_drag_chip_overlay(&session_snapshot, &layout, title);
                    r.set_drag_chip(chip);
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
                    if let Some(c) = self.windows.get_mut(&win_id) {
                        c.drag_target = tgt;
                        c.request_redraw();
                    }
                    return;
                }
                if child.mouse_down {
                    if let Some((row, col)) = r.pixel_to_cell(position.x as f32, position.y as f32)
                    {
                        if let Some(sel) = child.selection.as_mut() {
                            sel.extend(row, col);
                            mark_all_panes_dirty(&child.panes);
                            child.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => match state {
                ElementState::Pressed => {
                    let Some(r) = child.renderer.as_ref() else { return };
                    let (px, py) = (child.cursor_pos.0 as f32, child.cursor_pos.1 as f32);
                    let bar_width = r.width() as f32;
                    let layout = TabBarLayout::compute_with_height(
                        &child.tabs,
                        bar_width,
                        r.tab_bar_logical_height(),
                    )
                    .with_top_offset(r.tab_bar_y_offset())
                    .with_visible(r.tab_bar_visible());
                    if let Some(hit) = layout.hit(px, py) {
                        match hit {
                            TabHit::Activate(i) => {
                                child.tabs.activate(i);
                                child.pressed_tab = Some(i);
                                child.mouse_down = true;
                                child.drag_session =
                                    Some(crate::tab_drag::DragSession::new(i, (px, py)));
                            }
                            TabHit::Close(idx) => {
                                // Drop the &mut child borrow before
                                // re-entering &mut self via helpers.
                                let _ = child;
                                // Auto-reap is now inside
                                // close_tab_at_in_child (PR #302
                                // follow-up); no explicit reap call
                                // needed at the call-site.
                                self.close_tab_at_in_child(win_id, idx);
                                if let Some(c) = self.windows.get(&win_id) {
                                    c.request_redraw();
                                }
                                return;
                            }
                        }
                        child.request_redraw();
                        return;
                    }
                    child.mouse_down = true;
                    // `pixel_to_cell` expects raster px.
                    if let Some((row, col)) =
                        r.pixel_to_cell(child.cursor_pos.0 as f32, child.cursor_pos.1 as f32)
                    {
                        child.selection = Some(Selection::new(row, col));
                        mark_all_panes_dirty(&child.panes);
                    }
                    child.request_redraw();
                }
                ElementState::Released => {
                    let session = child.drag_session.take();
                    let foreign = child.drag_target.take();
                    let pressed = child.pressed_tab.take();
                    child.mouse_down = false;
                    if let Some(r) = child.renderer.as_mut() {
                        r.set_drag_chip(None);
                    }
                    if let Some(sel) = child.selection.as_ref() {
                        if sel.is_empty() {
                            child.selection = None;
                            mark_all_panes_dirty(&child.panes);
                            child.request_redraw();
                        }
                    }
                    if let (Some(s), Some(src_idx)) = (session, pressed) {
                        let Some(r) = child.renderer.as_ref() else { return };
                        let bar_width = r.width() as f32;
                        let layout = TabBarLayout::compute_with_height(
                            &child.tabs,
                            bar_width,
                            r.tab_bar_logical_height(),
                        )
                        .with_top_offset(r.tab_bar_y_offset());
                        let action = crate::tab_drag::compute_action(&s, foreign, &layout);
                        // Release the child borrow before re-entering
                        // &mut self via the merge / tear path.
                        let _ = child;
                        match action {
                            crate::tab_drag::DragAction::ReturnToOriginalBar => {
                                // No-op cancel.
                            }
                            crate::tab_drag::DragAction::ReorderTab { from, to } => {
                                // Re-borrow via self.windows
                                // because `let _ = child;` above
                                // released the long-lived mut borrow.
                                if let Some(c) = self.windows.get_mut(&win_id) {
                                    c.tabs.reorder(from, to);
                                    if from < c.tab_states.len() && to < c.tab_states.len() {
                                        let st = c.tab_states.remove(from);
                                        c.tab_states.insert(to, st);
                                    }
                                    c.request_redraw();
                                }
                            }
                            crate::tab_drag::DragAction::MergeIntoWindow(target) => {
                                self.merge_child_into_target(win_id, src_idx, target);
                            }
                            crate::tab_drag::DragAction::TearOutToNewWindow { .. } => {
                                // Epic #289 Phase B: tear out from a
                                // child window into a NEW top-level
                                // window. The Tab + PaneState (incl.
                                // PtyHandle) MOVE — no clone, no
                                // respawn, same child PID.
                                self.tear_out_from_child(el, win_id, src_idx);
                            }
                        }
                    }
                }
            },
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                self.frontmost_window = Some(win_id);
                if child.copy_mode.is_some() {
                    if child.copy_mode.as_ref().is_some_and(|mode| mode.is_read_only()) {
                        let child_mods = child.modifiers;
                        let _ = child;
                        for key_str in key_to_strings(&event.logical_key, child_mods) {
                            if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                                if super::keymap_dispatch::read_only_allows_action(&action)
                                    && self.run_action_for_window(&action, win_id)
                                {
                                    self.drain_pending_window_creates(el);
                                    if let Some(c) = self.windows.get(&win_id) {
                                        c.request_redraw();
                                    }
                                    return;
                                }
                            }
                        }
                        let Some(child) = self.windows.get_mut(&win_id) else { return };
                        child_copy_mode_handle_key(child, &event);
                        child.request_redraw();
                    } else {
                        child_copy_mode_handle_key(child, &event);
                        child.request_redraw();
                    }
                    return;
                }
                // Epic #289 follow-up: when the command
                // palette is attached to THIS child window, route the
                // keystroke into the overlay handler exactly like the
                // main window does in window_event.rs ~line 855. Without
                // this branch, every key while the palette was open in
                // a child got forwarded to the PTY instead of filtering
                // the palette query.
                let palette_here = self.palette_attached_window == Some(win_id);
                if palette_here {
                    let child_mods = child.modifiers;
                    let _ = child;
                    if let Some(key_str) = key_event_to_string(&event, child_mods) {
                        if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                            if matches!(action, Action::OpenCommandPalette) {
                                self.run_action_for_window(&action, win_id);
                                self.drain_pending_window_creates(el);
                                if let Some(c) = self.windows.get(&win_id) {
                                    c.request_redraw();
                                }
                                return;
                            }
                        }
                    }
                    self.command_palette_handle_key(&event);
                    if let Some(c) = self.windows.get(&win_id) {
                        c.request_redraw();
                    }
                    return;
                }
                // Issue #370: the previous narrow special-case only
                // handled `EnterCopyMode` / `EnterQuickSelect` and
                // dropped every other action (NextTab / PrevTab /
                // ActivateTab / SplitRight / Cmd+T / Cmd+W / ...) into
                // the PTY-byte path. Cmd+T appeared to work only because
                // the macOS menubar bypassed this handler entirely.
                //
                // Mirror the main window handler (window_event.rs ~916):
                // run the full keymap dispatch first, fall through to the
                // PTY-byte path only when no binding matches. `run_action`
                // already routes to the frontmost child via
                // `frontmost_kind()` (see keymap_dispatch.rs), and the
                // child-window Focused(true) arm sets
                // `self.frontmost_window = Some(win_id)` (~line 329) so a
                // chord typed in this child reaches THIS child's per-window
                // helpers.
                //
                // EnterCopyMode / EnterQuickSelect keep their child-local
                // entry helpers because they install copy/quick-select
                // state on this specific child WindowState, which
                // `App::run_action` (main-only) wouldn't touch.
                let child_mods = child.modifiers;
                let _ = child;
                let mut handled = false;
                for key_str in key_to_strings(&event.logical_key, child_mods) {
                    if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                        match action {
                            Action::EnterCopyMode => {
                                if let Some(c) = self.windows.get_mut(&win_id) {
                                    child_enter_copy_mode(c);
                                    c.request_redraw();
                                }
                                return;
                            }
                            Action::EnterQuickSelect => {
                                if let Some(c) = self.windows.get_mut(&win_id) {
                                    child_enter_quick_select(c);
                                    c.request_redraw();
                                }
                                return;
                            }
                            _ => {
                                if self.run_action_for_window(&action, win_id) {
                                    self.drain_pending_window_creates(el);
                                    if let Some(c) = self.windows.get(&win_id) {
                                        c.request_redraw();
                                    }
                                    handled = true;
                                    break;
                                }
                            }
                        }
                    }
                }
                if handled {
                    return;
                }
                let Some(child) = self.windows.get_mut(&win_id) else { return };
                let mods = child.modifiers;
                let tab_idx = child.tabs.active_index();
                let active_id = match child.tab_states.get(tab_idx) {
                    Some(st) => st.active_pane,
                    None => return,
                };
                // Read the active child pane's kitty keyboard flags under the
                // parser lock, then drop it before the PTY write (CLAUDE.md
                // §4). Non-zero flags drive CSI-u key encoding (Shift+Enter).
                let kitty_flags = child
                    .panes
                    .get(&active_id)
                    .map(|pane| pane.parser.lock().kitty_keyboard_flags())
                    .unwrap_or(0);
                if let Some(bytes) = encode_key(&event, mods, kitty_flags) {
                    if let Some(pane) = child.panes.get(&active_id) {
                        if let Some(pty) = pane.pty.as_ref() {
                            let _ = pty.in_tx.send(bytes);
                        }
                    }
                    if child.selection.is_some() {
                        child.selection = None;
                        mark_all_panes_dirty(&child.panes);
                        child.request_redraw();
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
        let main_id = self.main_window().map(|w| w.id());
        let attached = if Some(target.window) == main_id {
            self.attach_tab_state(target.slot, tab, state, panes);
            // Receiving a tab back into main un-hides the window if it
            // had been drained.
            if self.main_is_hidden() {
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
        if let Some(w) = self.main_window() {
            w.request_redraw();
        }
    }
    pub(super) fn reap_empty_child(&mut self, win_id: WindowId) {
        // PR #302 follow-up: bump the test-observable counter on EVERY
        // invocation (even no-ops on stale ids) so tests can pin that
        // child-window cleanup truly routed through this contract rather
        // than a raw `windows.remove`.
        self.reap_call_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(child) = self.windows.get(&win_id) {
            if child.tabs.is_empty() {
                if let Some(removed) = self.windows.remove(&win_id) {
                    // panes map should already be empty; defensively
                    // null out any stragglers' redraw targets.
                    for pane in removed.panes.values() {
                        *pane.redraw_target.lock() = None;
                    }
                    drop(removed);
                    tracing::info!(
                        "child window reaped after drag-merge; remaining children={}",
                        self.windows.len()
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
        if self.main_tabs().map(|t| t.is_empty()).unwrap_or(true) && self.child_window_count() > 0 {
            self.hide_main_window();
        }
        if let Some(w) = self.main_window() {
            w.request_redraw();
        }
    }
    pub(super) fn hide_main_window(&mut self) {
        if let Some(w) = self.main_window() {
            w.set_visible(false);
        }
        if let Some(ws) = self.main_mut() {
            ws.hidden = true;
        }
        tracing::info!("main window hidden (drained); windows={}", self.windows.len());
    }
    pub(super) fn show_main_window(&mut self) {
        if let Some(w) = self.main_window() {
            w.set_visible(true);
        }
        if let Some(ws) = self.main_mut() {
            ws.hidden = false;
        }
    }

    /// Build a fresh `PaneState` bound to the given child window's
    /// (cols, rows, Arc<Window>) snapshot. Extracted
    /// from `spawn_tab_in_child` so `split_active_pane_in_child` can
    /// reuse the same VT-loop + reply-forwarder setup without
    /// duplicating ~50 lines of thread-spawn boilerplate.
    ///
    /// PR #400: cursor_visible is now a fresh per-pane Arc owned by
    /// `PaneState`, no longer threaded in from the WindowState.
    pub(super) fn spawn_pane_state_for_child(
        &self,
        cols: u16,
        rows: u16,
        child_window: Arc<Window>,
    ) -> PaneState {
        use sonicterm_grid::grid::Grid;
        use sonicterm_vt::vt::Parser;
        let (reply_tx, reply_rx) = crossbeam_channel::unbounded::<Vec<u8>>();
        let parser = Arc::new(Mutex::new(Parser::new_with_reply(Grid::new(cols, rows), reply_tx)));
        // Seed theme defaults for OSC 10/11/12 query replies (#369).
        {
            let mut p = parser.lock();
            if let Some((r, g, b)) = self.theme.colors.foreground.rgb() {
                p.set_theme_fg(r, g, b);
            }
            if let Some((r, g, b)) = self.theme.colors.background.rgb() {
                p.set_theme_bg(r, g, b);
            }
            if let Some((r, g, b)) = self.theme.colors.cursor.rgb() {
                p.set_theme_cursor(r, g, b);
            }
        }
        let redraw_target: Arc<Mutex<Option<Arc<Window>>>> =
            Arc::new(Mutex::new(Some(child_window)));
        // PR #400: per-pane cursor_visible Arc.
        let cursor_visible_pane: Arc<std::sync::atomic::AtomicBool> =
            Arc::new(std::sync::atomic::AtomicBool::new(true));
        let pty = match PtyHandle::spawn_default_shell(
            cols,
            rows,
            sonicterm_io::pty::ShellSpawnOpts::default(),
        ) {
            Ok(pty) => {
                let parser_clone = parser.clone();
                let out_rx = pty.out_rx.clone();
                let in_tx_reply = pty.in_tx.clone();
                let redraw_target_thread = redraw_target.clone();
                let cursor_visible = cursor_visible_pane.clone();
                let pty_burst_gen = self.pty_burst_gen.clone();
                std::thread::Builder::new()
                    .name("sonicterm-vt-reply-child".into())
                    .spawn(move || {
                        while let Ok(bytes) = reply_rx.recv() {
                            if in_tx_reply.send(bytes).is_err() {
                                break;
                            }
                        }
                    })
                    .expect("spawn vt reply forwarder (child)");
                std::thread::Builder::new()
                    .name("sonicterm-vt-loop-child".into())
                    .spawn(move || {
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
                    .expect("spawn vt loop (child)");
                Some(pty)
            }
            Err(e) => {
                tracing::error!("failed to spawn pty for child pane: {e}");
                None
            }
        };
        let mut pane_state = PaneState::new(parser, pty);
        pane_state.redraw_target = redraw_target;
        pane_state.cursor_visible = cursor_visible_pane;
        pane_state
    }

    /// Spawn a new tab containing a single fresh pane inside the
    /// child window identified by `win_id`. Returns `false` if no
    /// such child window exists (caller should fall back to the main
    /// App's `new_tab`). The new pane's redraw target is bound to the
    /// child window so VT output redraws the child, not the main App.
    pub(super) fn spawn_tab_in_child(&mut self, win_id: WindowId) -> bool {
        // Snapshot everything we need from the child up-front so the
        // mutable borrow ends before we spawn the VT thread (which
        // captures clones), then re-borrow to install the new tab.
        let (cols, rows, child_window) = {
            let Some(child) = self.windows.get_mut(&win_id) else {
                return false;
            };
            let Some(renderer) = child.renderer.as_ref() else {
                return false;
            };
            let Some(win) = child.window.as_ref() else {
                return false;
            };
            let (c, r) = renderer.cells();
            (c, r, win.clone())
        };
        let pane_state = self.spawn_pane_state_for_child(cols, rows, child_window.clone());
        let pane_id = next_pane_id();
        let Some(child) = self.windows.get_mut(&win_id) else {
            return false;
        };
        child.panes.insert(pane_id, pane_state);
        let n = child.tabs.len() + 1;
        child.tabs.push(Tab::new(format!("shell {n}")));
        child.tab_states.push(TabState::new(PaneTree::leaf(pane_id), pane_id));
        let last = child.tabs.len().saturating_sub(1);
        child.tabs.activate(last);
        child.request_redraw();
        true
    }

    // ──────────────────────────────────────────────────────────────────
    // Epic #289 Phase A — per-child action helpers
    //
    // These mirror the equivalent main-window mutators in
    // `app/misc.rs` and `app/spawn_pane.rs` but operate on a child
    // window's owned (tabs / tab_states / panes) triple. Each helper:
    //   * returns `true` if it mutated state (so the caller knows to
    //     bump `redraw_request_count`),
    //   * issues `child.request_redraw()` on the child handle
    //     when state changed,
    //   * returns `false` (no-op + no redraw) when the recorded child
    //     no longer exists — the keymap_dispatch caller then falls
    //     through to the main-window default.
    //
    // The empty-tab-vec post-condition (close the window? leave it
    // dangling? merge into main?) is deliberately left to the existing
    // teardown plumbing — `reap_empty_child` runs on user-event drain
    // and on the next focus event, so we don't replicate that
    // single-source-of-truth here.
    // ──────────────────────────────────────────────────────────────────

    /// Close the active tab of the given child window. Returns `true`
    /// on success.
    pub(super) fn close_active_tab_in_child(&mut self, win_id: WindowId) -> bool {
        let idx = {
            let Some(child) = self.windows.get(&win_id) else { return false };
            child.tabs.active_index()
        };
        self.close_tab_at_in_child(win_id, idx)
    }

    /// Close the tab at `idx` in the given child window. Used by the
    /// close-button (×) hit-test path in the child's tab bar, which
    /// passes the clicked index directly (not the active one). Returns
    /// `true` on success.
    ///
    /// Auto-reap behavior (PR #302 follow-up): when this drains the
    /// child to zero tabs we immediately invoke
    /// [`Self::reap_empty_child`] so callers never have to remember.
    /// The previous contract (caller-responsible reap) left Cmd+W and
    /// `CloseActivePaneOrTab` on a single-pane child window leaking a
    /// ghost frame — Haiku finding on PR #302. Centralising the reap
    /// here is the single-source-of-truth pattern: every close path
    /// (× button, Cmd+W, close-active-pane-or-tab) now flows through
    /// this function and gets the reap for free.
    pub(super) fn close_tab_at_in_child(&mut self, win_id: WindowId, idx: usize) -> bool {
        let drained = {
            let Some(child) = self.windows.get_mut(&win_id) else { return false };
            if idx >= child.tab_states.len() {
                return false;
            }
            let st = child.tab_states.remove(idx);
            for id in st.tree.leaves() {
                // PaneState::Drop → PtyHandle::Drop kills the shell.
                child.panes.remove(&id);
            }
            if let Some(tab_id) = child.tabs.tabs().get(idx).map(|t| t.id) {
                child.tabs.close(tab_id);
            }
            child.request_redraw();
            child.tabs.is_empty()
        };
        if drained {
            self.reap_empty_child(win_id);
        }
        true
    }

    /// Close-active-pane-or-tab inside a child window. Mirrors the
    /// iTerm2/wezterm rule: > 1 pane → close the focused pane only,
    /// else → close the whole tab.
    pub(super) fn close_active_pane_or_tab_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else { return false };
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get_mut(tab_idx) else { return false };
        let pane_count = st.tree.leaves().len();
        if pane_count <= 1 {
            // Single pane → degrade to close-tab path. Drop the
            // child borrow so close_active_tab_in_child can re-borrow.
            let _ = st;
            let _ = child;
            return self.close_active_tab_in_child(win_id);
        }
        let focus = st.active_pane;
        let new_focus = st.tree.leaves().into_iter().find(|id| *id != focus).unwrap_or(focus);
        if st.tree.close(focus) {
            st.active_pane = new_focus;
            child.panes.remove(&focus);
            child.request_redraw();
            return true;
        }
        false
    }

    /// Advance the active tab in the child window.
    pub(super) fn next_tab_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else { return false };
        child.tabs.next();
        child.request_redraw();
        true
    }

    /// Step back one tab in the child window.
    pub(super) fn prev_tab_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else { return false };
        child.tabs.prev();
        child.request_redraw();
        true
    }

    /// Activate a specific tab index in the child window.
    pub(super) fn activate_tab_in_child(&mut self, win_id: WindowId, idx: usize) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else { return false };
        child.tabs.activate(idx);
        child.request_redraw();
        true
    }

    /// Activate the last tab in the child window.
    pub(super) fn activate_last_tab_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else { return false };
        let last = child.tabs.len().saturating_sub(1);
        child.tabs.activate(last);
        child.request_redraw();
        true
    }

    // ──────────────────────────────────────────────────────────────────
    // Epic #289 Phase A — per-child PANE mutators
    //
    // Mirror of the per-child tab helpers above, but for pane-level
    // actions (`Action::SplitRight`, `SplitDown`, `ClosePane`,
    // `FocusPane(_)`, `TogglePaneZoom`, `ResizePane{Left,Right,Up,Down}`).
    // Same contract as the tab helpers: return `true` if mutated state
    // and request_redraw on the child's window; return `false` (no-op)
    // when the recorded child no longer exists so keymap_dispatch can
    // fall back to the main-window default.
    //
    // Without these, Cmd+D / Cmd+Shift+D / Cmd+[ / Cmd+] / Cmd+Z typed
    // in a torn-out child window would silently mutate the MAIN App's
    // active tab — same bug class as #2/#3 but for pane actions. Haiku
    // review of PR #291 caught this gap.
    // ──────────────────────────────────────────────────────────────────

    /// Split the active pane of the given child window in `dir`. Returns
    /// `true` on success.
    pub(super) fn split_active_pane_in_child(&mut self, win_id: WindowId, dir: Direction) -> bool {
        // Snapshot what we need to spawn a PTY before any mutable borrow
        // of self.windows is taken — `spawn_pane_state_for_child`
        // captures clones of (pty_burst_gen, window, cursor_visible) and
        // we want the borrow checker happy when we re-borrow `child`
        // below to install the new pane.
        let Some(child) = self.windows.get(&win_id) else {
            return false;
        };
        let new_id = next_pane_id();
        let pane_state =
            if let (Some(renderer), Some(win)) = (child.renderer.as_ref(), child.window.as_ref()) {
                let (cols, rows) = renderer.cells();
                self.spawn_pane_state_for_child(cols, rows, win.clone())
            } else if child.renderer.is_none() && child.window.is_none() {
                // Test-only synthetic child windows intentionally have no
                // renderer/window, but they still need to exercise pane
                // ownership/routing without falling through to main.
                let parser = Arc::new(Mutex::new(Parser::new(Grid::new(80, 24))));
                PaneState::new(parser, None)
            } else {
                return false;
            };
        let Some(child) = self.windows.get_mut(&win_id) else {
            return false;
        };
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get_mut(tab_idx) else { return false };
        let focus = st.active_pane;
        if !st.tree.split(focus, dir, new_id) {
            return false;
        }
        st.active_pane = new_id;
        child.panes.insert(new_id, pane_state);
        resize_visible_panes_in_child(child);
        if let Some(r) = child.renderer.as_mut() {
            r.flash_pane_focus(new_id);
        }
        child.request_redraw();
        true
    }

    /// Close the active pane in the given child window. If the active
    /// tab has only one pane left, degrades to closing the tab (same
    /// iTerm2/wezterm rule as the main-window `close_active_pane`).
    pub(super) fn close_active_pane_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else { return false };
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get_mut(tab_idx) else { return false };
        let focus = st.active_pane;
        if matches!(st.tree, PaneTree::Leaf { id, .. } if id == focus) {
            // Single-leaf → fall back to tab close. Release the &mut
            // WindowState borrow first so `close_active_tab_in_child`
            // can re-borrow `self.windows`.
            let _ = child;
            return self.close_active_tab_in_child(win_id);
        }
        let new_focus = st.tree.leaves().into_iter().find(|id| *id != focus).unwrap_or(focus);
        if st.tree.close(focus) {
            st.active_pane = new_focus;
            child.panes.remove(&focus);
            resize_visible_panes_in_child(child);
            if let Some(r) = child.renderer.as_mut() {
                r.flash_pane_focus(new_focus);
            }
            child.request_redraw();
            return true;
        }
        false
    }

    /// Move pane focus in the given direction within the active tab of
    /// the given child window.
    pub(super) fn focus_pane_dir_in_child(&mut self, win_id: WindowId, dir: Direction) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else { return false };
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get_mut(tab_idx) else { return false };
        if let Some(next) = st.tree.focus_neighbor(st.active_pane, dir) {
            if st.active_pane == next {
                return true;
            }
            st.active_pane = next;
            if let Some(r) = child.renderer.as_mut() {
                r.flash_pane_focus(next);
            }
            child.request_redraw();
            true
        } else {
            // Recognized as the right child, but no neighbor existed in
            // that direction — still considered "routed": consume the
            // action so we don't fall through to mutate the main window.
            true
        }
    }

    /// Toggle zoom on the active pane in the given child window.
    pub(super) fn toggle_active_pane_zoom_in_child(&mut self, win_id: WindowId) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else { return false };
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get_mut(tab_idx) else { return false };
        let active = st.active_pane;
        if st.tree.toggle_zoom(active) {
            resize_visible_panes_in_child(child);
            child.request_redraw();
        }
        // Routed regardless of toggle result so the action does not leak
        // to the main window.
        true
    }

    /// Resize the active split edge in the given direction within the
    /// active tab of the given child window.
    pub(super) fn resize_active_split_in_child(
        &mut self,
        win_id: WindowId,
        dir: Direction,
    ) -> bool {
        let Some(child) = self.windows.get_mut(&win_id) else { return false };
        let tab_idx = child.tabs.active_index();
        let Some(st) = child.tab_states.get_mut(tab_idx) else { return false };
        if st.tree.resize_split(st.active_pane, dir, 0.05) {
            resize_visible_panes_in_child(child);
            child.request_redraw();
        }
        // Routed regardless of resize result.
        true
    }
}

/// Resize all panes in the active tab of a child window to match the
/// current pane tree layout. Mirrors `App::resize_visible_panes` for the
/// child case so split/close/zoom on a torn-out window propagate to the
/// PTY winsize the same way.
pub(super) fn resize_visible_panes_in_child(child: &mut WindowState) {
    let rects = App::compute_pane_rects_for(child);
    let Some(r) = child.renderer.as_ref() else { return };
    let (cw, ch) = r.cell_size();
    let inset =
        [r.padding_left_px(), r.padding_right_px(), r.padding_top_px(), r.padding_bottom_px()];
    crate::app::resize_panes_to_rects(&child.panes, &rects, cw, ch, inset);
}
fn child_enter_copy_mode(child: &mut WindowState) {
    let tab_idx = child.tabs.active_index();
    let Some(active_id) = child.tab_states.get(tab_idx).map(|st| st.active_pane) else { return };
    let Some(pane) = child.panes.get(&active_id) else { return };
    let cursor = {
        let guard = pane.parser.lock();
        let grid = guard.grid();
        (grid.cursor.col as usize, grid.scrollback_len() + grid.cursor.row as usize)
    };
    child.copy_mode = Some(sonicterm_ui::copy_mode::CopyModeState::read_only_at(cursor));
    mark_all_panes_dirty(&child.panes);
}

fn child_enter_quick_select(child: &mut WindowState) {
    let tab_idx = child.tabs.active_index();
    let Some(active_id) = child.tab_states.get(tab_idx).map(|st| st.active_pane) else { return };
    let Some(pane) = child.panes.get(&active_id) else { return };
    let state = {
        let guard = pane.parser.lock();
        let grid = guard.grid();
        let mut state = sonicterm_ui::copy_mode::CopyModeState::new_at((0, grid.scrollback_len()));
        state.quick_select = Some(sonicterm_ui::copy_mode::QuickSelectState::from_grid(grid));
        state
    };
    child.copy_mode = Some(state);
    mark_all_panes_dirty(&child.panes);
}

fn child_copy_mode_handle_key(child: &mut WindowState, event: &KeyEvent) {
    let Some(mut state) = child.copy_mode.take() else { return };
    let mut should_copy = false;
    let mut should_exit = false;
    let mut copied_text: Option<String> = None;

    let tab_idx = child.tabs.active_index();
    let Some(active_id) = child.tab_states.get(tab_idx).map(|st| st.active_pane) else { return };
    if let Some(pane) = child.panes.get_mut(&active_id) {
        let guard = pane.parser.lock();
        let grid = guard.grid();
        if let Some(quick_select) = state.quick_select.as_ref() {
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => should_exit = true,
                Key::Character(s) => {
                    if let Some(ch) = s.chars().next() {
                        if let Some(text) = quick_select.text_for_hint(ch) {
                            copied_text = Some(text.to_string());
                            should_exit = true;
                        }
                    }
                }
                _ => {}
            }
        } else {
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => should_exit = true,
                Key::Named(NamedKey::Enter) if !state.is_read_only() => should_copy = true,
                Key::Named(NamedKey::ArrowLeft) => state.move_left(grid),
                Key::Named(NamedKey::ArrowRight) => state.move_right(grid),
                Key::Named(NamedKey::ArrowUp) => state.move_up(grid),
                Key::Named(NamedKey::ArrowDown) => state.move_down(grid),
                Key::Character(s) if s.eq_ignore_ascii_case("h") => state.move_left(grid),
                Key::Character(s) if s.eq_ignore_ascii_case("j") => state.move_down(grid),
                Key::Character(s) if s.eq_ignore_ascii_case("k") => state.move_up(grid),
                Key::Character(s) if s.eq_ignore_ascii_case("l") => state.move_right(grid),
                Key::Character(s) if s == "v" && !state.is_read_only() => state.start_select(),
                Key::Character(s) if s == "y" && !state.is_read_only() => should_copy = true,
                Key::Character(s) if s == "w" => state.move_word_fwd(grid),
                Key::Character(s) if s == "b" => state.move_word_back(grid),
                Key::Character(s) if s == "0" => state.move_line_start(grid),
                Key::Character(s) if s == "$" => state.move_line_end(grid),
                Key::Character(s) if s == "g" => state.move_top(grid),
                Key::Character(s) if s == "G" => state.move_bottom(grid),
                _ => {}
            }
            if should_copy {
                copied_text = child_copy_mode_selected_text(&state, grid);
                should_exit = true;
            } else {
                pane.viewport_top_abs = GpuRenderer::copy_mode_view_top_after_move_legacy(
                    &state,
                    grid,
                    pane.viewport_top_abs,
                );
            }
        }
    }

    if let Some(text) = copied_text {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            if let Err(e) = cb.set_text(text.clone()) {
                tracing::warn!("clipboard set failed: {e}");
            } else {
                tracing::info!("copied {} bytes", text.len());
            }
        }
    }
    if !should_exit {
        child.copy_mode = Some(state);
    }
    mark_all_panes_dirty(&child.panes);
}

fn child_copy_mode_selected_text(
    state: &sonicterm_ui::copy_mode::CopyModeState,
    grid: &Grid,
) -> Option<String> {
    let (start, end) = state.selected_range()?;
    if start == end {
        return None;
    }
    let mut out = String::new();
    let last_row = end.1.min(grid.scrollback_len() + grid.rows.saturating_sub(1) as usize);
    for row_idx in start.1..=last_row {
        let Some(row) = child_copy_mode_row(grid, row_idx) else { break };
        let col_start = if row_idx == start.1 { start.0 } else { 0 };
        let col_end = if row_idx == end.1 { (end.0 + 1).min(row.len()) } else { row.len() };
        let mut line = String::new();
        for cell in row.get_range(col_start.min(row.len()), col_end) {
            if cell.flags.contains(sonicterm_grid::grid::CellFlags::WIDE_CONT) {
                continue;
            }
            line.push(cell.ch);
        }
        out.push_str(line.trim_end());
        if row_idx < last_row {
            out.push('\n');
        }
    }
    (!out.is_empty()).then_some(out)
}

fn child_copy_mode_row(grid: &Grid, row_idx: usize) -> Option<&sonicterm_grid::grid::Row> {
    let sb = grid.scrollback_len();
    if row_idx < sb {
        grid.scrollback_row(row_idx)
    } else {
        let live = row_idx - sb;
        (live < grid.rows as usize).then(|| grid.row(live as u16))
    }
}
