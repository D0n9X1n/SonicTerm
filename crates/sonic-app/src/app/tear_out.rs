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
use crate::app::window_geom;
use sonic_ui::prefs::PrefsHit;

impl App {
    pub(super) fn tear_out_tab(&mut self, el: &ActiveEventLoop, index: usize) -> bool {
        // Cross-window merge takes priority over the single-tab guard:
        // see [`Self::try_cross_window_merge`] for the gate.
        if self.try_cross_window_merge(index) {
            return true;
        }
        // Don't tear the only tab when there's no cross-window target —
        // that's a no-op (the new window would be identical to the old
        // one, minus its renderer). Critically: return `false` so the
        // CursorMoved caller keeps the drag gesture alive; otherwise
        // the user can never recover by moving the cursor onto a
        // sibling window's tab bar.
        if self.tabs.len() <= 1 {
            return false;
        }
        // OS-level cross-process drag: if a sink is installed AND the
        // cursor has left every Sonic-owned window, hand the tab off
        // to the OS (NSPasteboard / OLE) and KILL the local copy
        // (dropping the panes runs PtyHandle::Drop which signals the
        // child). The destination Sonic process picks up the payload
        // from its own pasteboard read and spawns a fresh tab with
        // the same cwd/cmd/env, showing scrollback as history.
        if self.try_os_drag_handoff(index) {
            return true;
        }
        let Some((tab, state, panes)) = self.detach_tab_state(index) else { return true };

        let attrs = with_integrated_titlebar(
            Window::default_attributes()
                .with_title(format!("Sonic — {}", tab.title))
                .with_inner_size(winit::dpi::LogicalSize::new(800.0, 500.0)),
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
            &self.config.font.family,
            self.config.font.size,
            self.config.font.line_height,
            [
                self.config.window.padding_left,
                self.config.window.padding_right,
                self.config.window.padding_top,
                self.config.window.padding_bottom,
            ],
        ) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("tear-out: renderer init failed: {e}; pane state dropped");
                return true;
            }
        };
        // Inherit cursor config from the parent app so the torn-out
        // window doesn't suddenly revert to default block/blink.
        renderer.set_cursor_shape(self.config.terminal.cursor_shape);
        renderer.set_cursor_blink(self.config.terminal.cursor_blink);
        renderer.set_titlebar_inset(integrated_titlebar_inset());
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
        let child = ChildWindow {
            window: window.clone(),
            renderer,
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
            modifiers: ModifiersState::empty(),
            cursor_visible: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            last_render: Instant::now(),
            pressed_tab: None,
            drag_session: None,
            drag_target: None,
        };
        self.child_windows.insert(win_id, child);
        window.request_redraw();
        tracing::info!("tab torn out as new window; child_windows={}", self.child_windows.len());
        true
    }
}

impl App {
    pub(super) fn compute_child_drag_target(
        &self,
        src_id: WindowId,
        local_in_src: (f64, f64),
    ) -> Option<crate::tab_drag::DropTarget<WindowId>> {
        let src_child = self.child_windows.get(&src_id)?;
        let src_origin =
            src_child.window.inner_position().map(|p| (p.x, p.y)).unwrap_or_else(|_| (0, 0));
        let global = crate::tab_drag::local_to_global(src_origin, local_in_src);
        let mut candidates: Vec<(WindowId, crate::tab_drag::WindowGeom, TabBarLayout)> = Vec::new();
        if let Some(main) = self.window.as_ref() {
            let geom = window_geom(main);
            let width =
                self.renderer.as_ref().map(|r| r.width() as f32 / r.scale_factor()).unwrap_or(0.0);
            let inset = self.renderer.as_ref().map(|r| r.titlebar_inset()).unwrap_or(0.0);
            let bar_h = self
                .renderer
                .as_ref()
                .map(|r| r.tab_bar_logical_height())
                .unwrap_or(sonic_ui::tabbar_view::TAB_BAR_HEIGHT);
            candidates.push((
                main.id(),
                geom,
                TabBarLayout::compute_with_height(&self.tabs, width, bar_h)
                    .with_top_offset(inset)
                    .with_visible(self.tab_bar_visible),
            ));
        }
        for (id, c) in &self.child_windows {
            if *id == src_id {
                continue;
            }
            let geom = window_geom(&c.window);
            let bar_width = c.renderer.width() as f32 / c.renderer.scale_factor();
            let layout = TabBarLayout::compute_with_height(
                &c.tabs,
                bar_width,
                c.renderer.tab_bar_logical_height(),
            )
            .with_top_offset(c.renderer.titlebar_inset())
            .with_visible(c.renderer.tab_bar_visible());
            candidates.push((*id, geom, layout));
        }
        crate::tab_drag::find_drop_target(global, candidates)
    }
    pub(super) fn compute_main_drag_target(
        &self,
        local_in_main: (f64, f64),
    ) -> Option<crate::tab_drag::DropTarget<WindowId>> {
        let main_window = self.window.as_ref()?;
        let main_origin =
            main_window.inner_position().map(|p| (p.x, p.y)).unwrap_or_else(|_| (0, 0));
        let global = crate::tab_drag::local_to_global(main_origin, local_in_main);
        let candidates = self.child_windows.iter().map(|(id, c)| {
            let geom = window_geom(&c.window);
            let bar_width = c.renderer.width() as f32 / c.renderer.scale_factor();
            let layout = TabBarLayout::compute_with_height(
                &c.tabs,
                bar_width,
                c.renderer.tab_bar_logical_height(),
            )
            .with_top_offset(c.renderer.titlebar_inset())
            .with_visible(c.renderer.tab_bar_visible());
            (*id, geom, layout)
        });
        crate::tab_drag::find_drop_target(global, candidates)
    }
    pub(super) fn try_os_drag_handoff(&mut self, index: usize) -> bool {
        let Some(sink) = self.os_drag_sink.clone() else { return false };
        if self.cursor_inside_any_window() {
            return false;
        }
        let Some(payload) = self.build_payload_for_tab(index) else { return false };
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
        let tab = self.tabs.tabs().get(index)?.clone();
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
        let Some(main) = self.window.as_ref() else { return false };
        let main_origin = main.inner_position().map(|p| (p.x, p.y)).unwrap_or_else(|_| (0, 0));
        let global = crate::tab_drag::local_to_global(main_origin, self.cursor_pos);
        if crate::tab_drag::global_to_local(window_geom(main), global).is_some() {
            return true;
        }
        for c in self.child_windows.values() {
            if crate::tab_drag::global_to_local(window_geom(&c.window), global).is_some() {
                return true;
            }
        }
        false
    }
    pub fn try_cross_window_merge(&mut self, index: usize) -> bool {
        let main_id = self.window.as_ref().map(|w| w.id());
        let Some(target) = self.drag_target.filter(|t| Some(t.window) != main_id) else {
            return false;
        };
        self.drag_target = None;
        self.pressed_tab = None;
        self.mouse_down = false;
        self.merge_main_into_child(index, target);
        true
    }
    pub fn tear_out_would_be_noop(&self) -> bool {
        let main_id = self.window.as_ref().map(|w| w.id());
        let no_target = self.drag_target.filter(|t| Some(t.window) != main_id).is_none();
        no_target && self.tabs.len() <= 1
    }
}
