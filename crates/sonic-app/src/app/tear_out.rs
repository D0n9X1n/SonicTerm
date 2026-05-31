//! Extracted from `app/mod.rs` in refactor PR 8b (expose-then-extract).
//! `App`'s referenced fields are `pub(super)`; this submodule lives in
//! the same `app` module tree, so direct field access works.

#![allow(unused_imports)]

use sonic_ui::ime::ImeState;
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
    to_logical_pos, with_integrated_titlebar, wrap_paste, App, PaneState, TabState, UserEvent,
    WindowState,
};
use crate::app::window_geom;

impl App {
    pub(super) fn tear_out_tab(&mut self, el: &ActiveEventLoop, index: usize) -> bool {
        // Cross-window merge takes priority over the single-tab guard:
        // see [`Self::try_cross_window_merge`] for the gate.
        if self.try_cross_window_merge(index) {
            return true;
        }
        // OS-level cross-process drag: if a sink is installed AND the
        // cursor has left every Sonic-owned window, hand the tab off
        // to the OS (NSPasteboard / OLE) and KILL the local copy
        // (dropping the panes runs PtyHandle::Drop which signals the
        // child). The destination Sonic process picks up the payload
        // from its own pasteboard read and spawns a fresh tab with
        // the same cwd/cmd/env, showing scrollback as history.
        //
        // This must run before the single-tab no-op guard: on Windows,
        // dropping the only tab on the bare desktop returns
        // DROPEFFECT_NONE, which the OLE sink promotes into a real
        // child-process tear-out.
        if self.try_os_drag_handoff(index) {
            return true;
        }
        // Epic #289 Phase B: the single-tab guard is GONE. Tearing
        // out the only tab in main hides main (existing drained-main
        // path) and the tab becomes its own new top-level window. The
        // PtyHandle MOVES via `detach_tab_state` — no respawn, no
        // clone, same child PID — so the user's shell session
        // survives the gesture intact.
        let Some((tab, state, panes)) = self.detach_tab_state(index) else { return true };

        let attrs = super::with_backdrop_transparency(
            with_integrated_titlebar(
                Window::default_attributes()
                    .with_title(format!("Sonic — {}", tab.title))
                    .with_decorations(true)
                    .with_inner_size(winit::dpi::LogicalSize::new(800.0, 500.0)),
            ),
            self.config.appearance.backdrop,
        );
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                tracing::error!("tear-out: create_window failed: {e}; pane state dropped");
                // panes drop here, which kills the child shells via
                // PtyHandle::Drop — acceptable for an OS-level failure.
                // The gesture IS consumed — we already drained the
                // source tab — so the caller must clear drag state.
                return true;
            }
        };
        window.set_ime_allowed(true);

        // Build the renderer for the new surface. If GPU init fails
        // we drop the panes (kills shells) and bail — the child
        // window would otherwise be invisible/unusable.
        let mut renderer = match GpuRenderer::new(
            window.clone(),
            el,
            &self.theme,
            sonic_shared::render::RendererSettings {
                font_family: &self.config.font.family,
                font_size: self.config.font.size,
                line_height_mult: self.config.font.line_height,
                padding: [
                    self.config.window.padding_left,
                    self.config.window.padding_right,
                    self.config.window.padding_top,
                    self.config.window.padding_bottom,
                ],
                appearance: sonic_shared::render::SurfaceAppearance {
                    backdrop: self.config.appearance.backdrop,
                    opacity: self.config.appearance.opacity,
                    scrollbar: self.config.appearance.scrollbar,
                },
            },
        ) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("tear-out: renderer init failed: {e}; pane state dropped");
                return true;
            }
        };
        // Epic #300 P4 follow-up wire (tear-out path).
        if let Some(proxy) = self.event_loop_proxy.clone() {
            renderer.set_async_loader(super::build_async_fallback_loader_for_proxy(proxy));
        }
        // Inherit cursor config from the parent app so the torn-out
        // window doesn't suddenly revert to default block/blink.
        renderer.set_cursor_shape(self.config.terminal.cursor_shape);
        renderer.set_cursor_blink(self.config.terminal.cursor_blink);
        renderer.set_titlebar_inset(0.0);
        renderer.set_tab_close_override(self.config.tab_close_button_color.as_deref());

        // On macOS the freshly created window often reports
        // scale_factor=1.0 inside `GpuRenderer::new` because it hasn't
        // been placed on a display yet. Once the OS positions it on a
        // Retina display the real scale is 2.0 but no
        // `ScaleFactorChanged` necessarily fires synchronously, so the
        // child window would render with stale 1× glyph tiles + a
        // surface that's actually 2× — producing the "huge letter
        // spacing, no colors, missing nerd-font glyphs" repro. Force
        // an atlas rebuild against the window's CURRENT scale factor,
        // then re-configure the surface to the window's CURRENT
        // physical inner size so cells/rows are derived from real
        // numbers instead of the 800×500 logical seed.
        let real_sf = window.scale_factor() as f32;
        renderer.force_rebuild_for_scale(real_sf);
        let real_inner = window.inner_size();
        renderer.resize(real_inner.width.max(1), real_inner.height.max(1));

        let (cols, rows) = renderer.cells();
        // Resize the migrated panes to the child window's grid and
        // swap each pane's VT-thread redraw target so further pty
        // output triggers the CHILD window's redraw, not the parent.
        for pane in panes.values() {
            pane.parser.lock().grid_mut().resize(cols, rows);
            if let Some(pty) = pane.pty.as_ref() {
                (pty.resize)(cols, rows);
            }
            *pane.redraw_target.lock() = Some(window.clone());
        }

        let win_id = window.id();
        let mut child_tabs = TabBar::new();
        let active_pane = state.active_pane;
        child_tabs.push(tab);
        let child = WindowState {
            role: crate::app::WindowRole::Terminal,
            window: Some(window.clone()),
            renderer: Some(renderer),
            tabs: child_tabs,
            tab_states: vec![TabState {
                tree: state.tree,
                active_pane,
                search: state.search,
                command: state.command,
            }],
            panes,
            cursor_pos: (0.0, 0.0),
            mouse_down: false,
            selection: None,
            copy_mode: None,
            modifiers: ModifiersState::empty(),
            last_render: Instant::now(),
            hover_link: false,
            pressed_tab: None,
            drag_session: None,
            drag_target: None,
            scale_factor: 1.0,
            ime: ImeState::new(),
            ime_cursor_throttle: sonic_ui::ime::ImeCursorThrottle::new(),
            hovered_url: None,
            hidden: false,
        };
        self.windows.insert(win_id, child);
        // Phase C2 / Haiku #295: register the new window's HWND with
        // the OS-drag backend so drops on this child window reach
        // IDropTarget::Drop. No-op on mac (pasteboard model).
        self.register_window_with_os_drag_backend(win_id, &window);
        window.request_redraw();
        // Epic #289 Phase B: the new window is now OS-frontmost (we
        // just created and focused it). Update `frontmost_window` so
        // subsequent keymap actions (Cmd+T, Cmd+W, …) route to it.
        // A real Focused event will confirm this shortly, but setting
        // it eagerly avoids a frame of mis-routing.
        self.frontmost_window = Some(win_id);
        // Phase B source-side cleanup: hide main if drained, else
        // activate the LEFT neighbor of the removed slot (spec §B4).
        self.tear_out_apply_source_side(index);
        tracing::info!("tab torn out as new window; windows={}", self.windows.len());
        true
    }

    /// Epic #289 Phase B — source-side post-tear-out cleanup, factored
    /// out so unit tests can drive it without an `ActiveEventLoop`.
    ///
    /// * If main is now empty, hide it (existing drained-main path).
    /// * Else activate `max(0, removed_idx - 1)` (the left neighbor).
    ///
    /// `detach_tab_state` already adjusts the active index via
    /// `TabBar::close`, but its rule ("stay at the same numeric
    /// index, clamp on overflow") shifts focus RIGHT when the active
    /// tab was removed. Phase B overrides to consistently pick the
    /// LEFT neighbor, matching common terminal-emulator UX.
    pub fn tear_out_apply_source_side(&mut self, removed_idx: usize) {
        let is_empty = self.main_tabs().map(|t| t.is_empty()).unwrap_or(true);
        if is_empty {
            if self.child_window_count() > 0 {
                self.hide_main_window();
            }
            return;
        }
        if let Some(t) = self.main_tabs_mut() {
            let target = removed_idx.saturating_sub(1).min(t.len().saturating_sub(1));
            t.activate(target);
        }
    }
}

impl App {
    pub(super) fn compute_child_drag_target(
        &self,
        src_id: WindowId,
        local_in_src: (f64, f64),
    ) -> Option<crate::tab_drag::DropTarget<WindowId>> {
        let src_child = self.windows.get(&src_id)?;
        let src_origin = src_child
            .window
            .as_ref()?
            .inner_position()
            .map(|p| (p.x, p.y))
            .unwrap_or_else(|_| (0, 0));
        let global = crate::tab_drag::local_to_global(src_origin, local_in_src);
        let mut candidates: Vec<(WindowId, crate::tab_drag::WindowGeom, Option<TabBarLayout>)> =
            Vec::new();
        if let Some(main) = self.main_window() {
            let geom = window_geom(main);
            let width =
                self.main_renderer().map(|r| r.width() as f32 / r.scale_factor()).unwrap_or(0.0);
            let inset = self.main_renderer().map(|r| r.tab_bar_y_offset()).unwrap_or(0.0);
            let bar_h = self
                .main_renderer()
                .map(|r| r.tab_bar_logical_height())
                .unwrap_or(sonic_ui::tabbar_view::TAB_BAR_HEIGHT);
            candidates.push((
                main.id(),
                geom,
                self.main_tabs().map(|t| {
                    TabBarLayout::compute_with_height(t, width, bar_h)
                        .with_top_offset(inset)
                        .with_visible(self.tab_bar_visible)
                }),
            ));
        }
        for (id, c) in &self.windows {
            if *id == src_id || Some(*id) == self.main_window_id {
                continue;
            }
            let Some(r) = c.renderer.as_ref() else {
                continue;
            };
            let Some(cw) = c.window.as_ref() else {
                continue;
            };
            let geom = window_geom(cw);
            let bar_width = r.width() as f32 / r.scale_factor();
            let layout =
                TabBarLayout::compute_with_height(&c.tabs, bar_width, r.tab_bar_logical_height())
                    .with_top_offset(r.tab_bar_y_offset())
                    .with_visible(r.tab_bar_visible());
            candidates.push((*id, geom, Some(layout)));
        }
        crate::tab_drag::find_drop_target_skipping_unrendered(global, candidates)
    }
    pub(super) fn compute_main_drag_target(
        &self,
        local_in_main: (f64, f64),
    ) -> Option<crate::tab_drag::DropTarget<WindowId>> {
        let main_window = self.main_window()?;
        let main_origin =
            main_window.inner_position().map(|p| (p.x, p.y)).unwrap_or_else(|_| (0, 0));
        let global = crate::tab_drag::local_to_global(main_origin, local_in_main);
        let candidates = self.windows.iter().filter_map(|(id, c)| {
            if Some(*id) == self.main_window_id {
                return None;
            }
            let r = c.renderer.as_ref()?;
            let cw = c.window.as_ref()?;
            let geom = window_geom(cw);
            let bar_width = r.width() as f32 / r.scale_factor();
            let layout =
                TabBarLayout::compute_with_height(&c.tabs, bar_width, r.tab_bar_logical_height())
                    .with_top_offset(r.tab_bar_y_offset())
                    .with_visible(r.tab_bar_visible());
            Some((*id, geom, Some(layout)))
        });
        crate::tab_drag::find_drop_target_skipping_unrendered(global, candidates)
    }
    pub(super) fn try_os_drag_handoff(&mut self, index: usize) -> bool {
        let Some(sink) = self.os_drag_sink.clone() else { return false };
        if self.cursor_inside_any_window() {
            return false;
        }
        let Some(payload) = self.build_payload_for_tab(index) else { return false };

        // Phase C2: hand the gesture to the installed OsTabDragBackend
        // first. The backend is responsible for OS cursor capture +
        // pasteboard / OLE handoff. If `handles_full_gesture()` returns
        // true (Windows: DoDragDrop ran end-to-end inside the backend)
        // we MUST NOT also invoke the legacy `sink.begin_drag` — that
        // would re-enter DoDragDrop with no live gesture, immediately
        // returning NONE and falsely triggering `spawn_tearout_child`.
        // The backend's DragOutcome routes through `handle_os_drag_ended`
        // (transfer_tab / cancel_drag_session); when the backend owns
        // the gesture we return true here without detaching — the
        // dispatcher will handle source-side removal via transfer_tab.
        //
        // On Mac (handles_full_gesture == false) the backend only
        // writes the pasteboard (winit intercepts mouse events, so
        // NSDraggingSession proper isn't reachable) — we still fall
        // through to the legacy sink, which also writes the pasteboard
        // + returns NotAcknowledged (the PR #59 data-loss fix).
        if self.os_drag_backend.is_some() {
            let payload_json = payload.to_json().unwrap_or_default();
            let source_window = self.main_window().map(|w| w.id());
            if let Some(src_id) = source_window {
                // Issue #296: render a small PNG thumbnail of the
                // dragged tab so NSDraggingSession / OLE DoDragDrop
                // have a real preview instead of an empty Vec<u8>.
                // See `crates/sonic-app/src/tab_thumbnail.rs` for the
                // rationale behind the CPU-side renderer (vs the
                // originally-spec'd offscreen wgpu readback).
                let scale_factor =
                    self.main_window().map(|w| w.scale_factor() as f32).unwrap_or(1.0);
                let thumb_inputs = crate::tab_thumbnail::tab_thumbnail_inputs_from_payload(
                    &payload.tab_title,
                    scale_factor,
                );
                let drag_image_png = crate::tab_thumbnail::render_tab_thumbnail_png(&thumb_inputs);
                let started = self.begin_os_tab_drag(src_id, index, payload_json, drag_image_png);
                if started && self.os_drag_backend_handles_full_gesture() {
                    tracing::info!(
                        tab = %payload.tab_title,
                        "Phase C2: backend owns gesture end-to-end; legacy sink skipped"
                    );
                    return true;
                }
            }
        }

        let ack = sink.begin_drag(&payload);
        match ack {
            crate::os_drag::DragAck::Accepted => {
                let _ = self.detach_tab_state(index);
                tracing::info!(
                    tab = %payload.tab_title,
                    "OS drag: destination acknowledged; local tab dropped"
                );
                true
            }
            crate::os_drag::DragAck::NotAcknowledged => {
                // DATA-LOSS FIX (PR #59 review): no destination
                // confirmed adoption. Leave the source tab alive
                // and fall back to the in-process tear-out path so
                // the user does not lose a live shell.
                tracing::warn!(
                    tab = %payload.tab_title,
                    "OS drag: sink NotAcknowledged; keeping source tab, falling back to in-process tear-out"
                );
                false
            }
        }
    }
    pub(super) fn build_payload_for_tab(&self, index: usize) -> Option<crate::os_drag::TabPayload> {
        let tab = self.main_tabs()?.tabs().get(index)?.clone();
        // Scrollback extraction TBD — Grid does not yet expose a
        // "give me the full visible+scrollback text" accessor. v1
        // ships an empty buffer (the destination shell starts with a
        // fresh prompt); v2 will add the accessor + populate.
        let scrollback_bytes: Vec<u8> = Vec::new();
        Some(crate::os_drag::TabPayload {
            pty_pid: 0,
            tab_title: tab.title,
            scrollback_b64: crate::os_drag::TabPayload::encode_scrollback(&scrollback_bytes),
            cwd: String::new(),
            cmd: self.config.terminal.shell.clone().unwrap_or_default(),
            env: Vec::new(),
        })
    }
    pub(super) fn cursor_inside_any_window(&self) -> bool {
        let Some(main) = self.main_window() else { return false };
        let main_origin = main.inner_position().map(|p| (p.x, p.y)).unwrap_or_else(|_| (0, 0));
        let cursor_pos = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
        let global = crate::tab_drag::local_to_global(main_origin, cursor_pos);
        if crate::tab_drag::global_to_local(window_geom(main), global).is_some() {
            return true;
        }
        for c in self.windows.values() {
            let Some(cw) = c.window.as_ref() else { continue };
            if crate::tab_drag::global_to_local(window_geom(cw), global).is_some() {
                return true;
            }
        }
        false
    }
    pub fn try_cross_window_merge(&mut self, index: usize) -> bool {
        let main_id = self.main_window().map(|w| w.id());
        let Some(target) =
            self.main().and_then(|ws| ws.drag_target).filter(|t| Some(t.window) != main_id)
        else {
            return false;
        };
        if let Some(ws) = self.main_mut() {
            ws.drag_target = None;
            ws.pressed_tab = None;
            ws.mouse_down = false;
        }
        self.merge_main_into_child(index, target);
        true
    }
    pub fn tear_out_would_be_noop(&self) -> bool {
        // Epic #289 Phase B: tear-out is now ALWAYS productive — a
        // single-tab tear creates a new window with that tab and
        // hides the now-empty main. The CursorMoved handler no
        // longer needs to preserve gesture state for a "no-op" case;
        // tear-out simply fires. Kept as a `false` constant for
        // back-compat with the existing call sites that consult the
        // predicate before triggering tear-out.
        false
    }

    /// Epic #289 Phase B — tear a tab out of an existing child window
    /// into a brand-new top-level window. Mirrors
    /// [`Self::tear_out_tab`] (main → new) but with detach_from_child
    /// as the source. The torn Tab + its PaneState (incl. PtyHandle)
    /// MOVE — no clone, no respawn.
    pub(super) fn tear_out_from_child(
        &mut self,
        el: &ActiveEventLoop,
        src_id: WindowId,
        index: usize,
    ) -> bool {
        let Some((tab, state, panes)) = self.detach_from_child(src_id, index) else { return false };

        let attrs = super::with_backdrop_transparency(
            with_integrated_titlebar(
                Window::default_attributes()
                    .with_title(format!("Sonic — {}", tab.title))
                    .with_decorations(true)
                    .with_inner_size(winit::dpi::LogicalSize::new(800.0, 500.0)),
            ),
            self.config.appearance.backdrop,
        );
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                tracing::error!("tear-out (child→new): create_window failed: {e}; panes dropped");
                return true;
            }
        };
        window.set_ime_allowed(true);
        let mut renderer = match GpuRenderer::new(
            window.clone(),
            el,
            &self.theme,
            sonic_shared::render::RendererSettings {
                font_family: &self.config.font.family,
                font_size: self.config.font.size,
                line_height_mult: self.config.font.line_height,
                padding: [
                    self.config.window.padding_left,
                    self.config.window.padding_right,
                    self.config.window.padding_top,
                    self.config.window.padding_bottom,
                ],
                appearance: sonic_shared::render::SurfaceAppearance {
                    backdrop: self.config.appearance.backdrop,
                    opacity: self.config.appearance.opacity,
                    scrollbar: self.config.appearance.scrollbar,
                },
            },
        ) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("tear-out (child→new): renderer init failed: {e}; panes dropped");
                return true;
            }
        };
        // Epic #300 P4 follow-up wire (tear-out child→new path).
        if let Some(proxy) = self.event_loop_proxy.clone() {
            renderer.set_async_loader(super::build_async_fallback_loader_for_proxy(proxy));
        }
        renderer.set_cursor_shape(self.config.terminal.cursor_shape);
        renderer.set_cursor_blink(self.config.terminal.cursor_blink);
        renderer.set_titlebar_inset(0.0);
        renderer.set_tab_close_override(self.config.tab_close_button_color.as_deref());
        let real_sf = window.scale_factor() as f32;
        renderer.force_rebuild_for_scale(real_sf);
        let real_inner = window.inner_size();
        renderer.resize(real_inner.width.max(1), real_inner.height.max(1));

        let (cols, rows) = renderer.cells();
        for pane in panes.values() {
            pane.parser.lock().grid_mut().resize(cols, rows);
            if let Some(pty) = pane.pty.as_ref() {
                (pty.resize)(cols, rows);
            }
            *pane.redraw_target.lock() = Some(window.clone());
        }
        let win_id = window.id();
        let mut child_tabs = TabBar::new();
        let active_pane = state.active_pane;
        child_tabs.push(tab);
        let child = WindowState {
            role: crate::app::WindowRole::Terminal,
            window: Some(window.clone()),
            renderer: Some(renderer),
            tabs: child_tabs,
            tab_states: vec![TabState {
                tree: state.tree,
                active_pane,
                search: state.search,
                command: state.command,
            }],
            panes,
            cursor_pos: (0.0, 0.0),
            mouse_down: false,
            selection: None,
            copy_mode: None,
            modifiers: ModifiersState::empty(),
            last_render: Instant::now(),
            hover_link: false,
            pressed_tab: None,
            drag_session: None,
            drag_target: None,
            scale_factor: 1.0,
            ime: ImeState::new(),
            ime_cursor_throttle: sonic_ui::ime::ImeCursorThrottle::new(),
            hovered_url: None,
            hidden: false,
        };
        self.windows.insert(win_id, child);
        // Phase C2 / Haiku #295: register the new window's HWND with
        // the OS-drag backend so drops on this child window reach
        // IDropTarget::Drop. No-op on mac.
        self.register_window_with_os_drag_backend(win_id, &window);
        window.request_redraw();
        self.frontmost_window = Some(win_id);
        // Source child: if drained, drop it (PtyHandle::Drop on any
        // remaining panes — there shouldn't be any since we moved the
        // only tab's panes). Else activate left neighbor.
        self.tear_out_apply_child_source_side(src_id, index);
        tracing::info!(
            "tab torn out of child {:?} as new window; windows={}",
            src_id,
            self.windows.len()
        );
        true
    }

    /// Epic #289 Phase B — child-side post-tear-out cleanup. Mirrors
    /// [`Self::tear_out_apply_source_side`] for a torn-from-child
    /// origin. Removes the source child window from
    /// `self.windows` if it became empty; else activates the
    /// LEFT neighbor of the removed slot.
    pub fn tear_out_apply_child_source_side(&mut self, src_id: WindowId, removed_idx: usize) {
        let src_empty = self.windows.get(&src_id).map(|c| c.tabs.is_empty()).unwrap_or(false);
        if src_empty {
            if let Some(removed) = self.windows.remove(&src_id) {
                // Drop the renderer + window explicitly; any leftover
                // panes (there shouldn't be) drop here, which fires
                // PtyHandle::Drop and kills their child shells.
                drop(removed);
            }
            return;
        }
        if let Some(c) = self.windows.get_mut(&src_id) {
            let target = removed_idx.saturating_sub(1).min(c.tabs.len().saturating_sub(1));
            c.tabs.activate(target);
            c.request_redraw();
        }
    }
}
