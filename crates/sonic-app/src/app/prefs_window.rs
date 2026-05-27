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
use sonic_ui::prefs::{PrefsHit, PrefsState};

impl App {
    pub(super) fn handle_prefs_event(&mut self, _el: &ActiveEventLoop, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                // Persist edits on close (per spec: "persist on close").
                self.commit_prefs_and_apply_live();
                self.prefs_window = None;
                self.prefs_state = None;
                self.prefs_renderer = None;
            }
            WindowEvent::RedrawRequested => {
                if let (Some(r), Some(s)) =
                    (self.prefs_renderer.as_mut(), self.prefs_state.as_ref())
                {
                    if let Err(e) = r.render(s, &self.theme) {
                        tracing::warn!("prefs render failed: {e}");
                    }
                }
            }
            WindowEvent::Resized(sz) => {
                if let Some(r) = self.prefs_renderer.as_mut() {
                    r.resize(sz.width, sz.height);
                }
                if let Some(w) = self.prefs_window.as_ref() {
                    w.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(r) = self.prefs_renderer.as_mut() {
                    r.set_scale_factor(scale_factor as f32);
                }
                if let Some(w) = self.prefs_window.as_ref() {
                    w.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let (x, y) = (self.cursor_pos.0 as f32, self.cursor_pos.1 as f32);
                let hit = self.prefs_state.as_ref().and_then(|s| s.classify_click(x, y));
                match hit {
                    Some(PrefsHit::Apply) => {
                        self.commit_prefs_and_apply_live();
                    }
                    Some(PrefsHit::Cancel) => {
                        if let Some(s) = self.prefs_state.as_mut() {
                            s.cancel();
                        }
                        self.prefs_window = None;
                        self.prefs_state = None;
                        self.prefs_renderer = None;
                    }
                    other => {
                        let Some(s) = self.prefs_state.as_mut() else { return };
                        match other {
                            Some(PrefsHit::Sidebar(cat)) => {
                                s.blur_text_fields();
                                s.set_category(cat);
                            }
                            Some(PrefsHit::Toggle(id)) => {
                                s.blur_text_fields();
                                let _ = s.flip_toggle(id);
                            }
                            Some(PrefsHit::SliderTrack(id)) => {
                                s.blur_text_fields();
                                let _ = s.drag_slider(id, x);
                            }
                            Some(PrefsHit::DropdownHeader(id)) => {
                                s.blur_text_fields();
                                let _ = s.toggle_dropdown(id);
                            }
                            Some(PrefsHit::DropdownOption { id, index }) => {
                                s.blur_text_fields();
                                let _ = s.select_dropdown(id, index);
                            }
                            Some(PrefsHit::ColorCell { id, index }) => {
                                s.blur_text_fields();
                                let _ = s.pick_color(id, index);
                            }
                            Some(PrefsHit::TextField(id)) => {
                                let _ = s.focus_text_field(id);
                            }
                            Some(PrefsHit::Apply) | Some(PrefsHit::Cancel) => unreachable!(),
                            None => {
                                s.blur_text_fields();
                            }
                        }
                    }
                }
                // Redraw AFTER mutation so the next frame reflects the
                // new state. Calling request_redraw BEFORE the
                // mutation (as the previous code did) painted the
                // pre-click state — the user saw their previous click's
                // result instead of the current one, and on a fresh
                // prefs window the result was a stale blank surface.
                if let Some(w) = self.prefs_window.as_ref() {
                    w.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let Some(w) = self.prefs_window.as_ref() {
                    w.request_redraw();
                }
                let Some(s) = self.prefs_state.as_mut() else { return };
                match &event.logical_key {
                    Key::Named(NamedKey::Backspace) => {
                        if let Some(id) = s.focused_field {
                            let new_val = if let Some(sonic_ui::prefs::Control::TextField(tf)) =
                                s.controls.iter_mut().find(|c| c.id() == id)
                            {
                                tf.pop_char();
                                let v = tf.get().to_string();
                                Some(if v.is_empty() { None } else { Some(v) })
                            } else {
                                None
                            };
                            // Best-effort: only the Shell text field exists
                            // today; mirror its value into config.
                            if let Some(v) = new_val {
                                if s.config.terminal.shell != v {
                                    s.config.terminal.shell = v;
                                    s.dirty = true;
                                }
                            }
                        }
                    }
                    Key::Named(NamedKey::Escape) => {
                        s.cancel();
                        self.prefs_window = None;
                        self.prefs_state = None;
                        self.prefs_renderer = None;
                    }
                    Key::Character(chs) => {
                        for ch in chs.chars() {
                            if !ch.is_control() {
                                s.type_into_focused(ch);
                            }
                        }
                    }
                    _ => {}
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = (position.x, position.y);
            }
            _ => {}
        }
        // NB: do NOT unconditionally request_redraw here — RedrawRequested
        // itself flows through this handler, so a tail redraw creates an
        // idle vsync feedback loop (CLAUDE.md §4 land-mine). Redraws are
        // requested only inside the arms that actually mutate visible
        // state (MouseInput, KeyboardInput, Resize, ScaleFactorChanged).
    }
}

impl App {
    pub(super) fn create_prefs_window(&mut self, el: &ActiveEventLoop) {
        let attrs = with_integrated_titlebar(
            Window::default_attributes()
                .with_title("Sonic Preferences")
                .with_inner_size(winit::dpi::LogicalSize::new(
                    sonic_ui::prefs::PREFS_WIN_W,
                    sonic_ui::prefs::PREFS_WIN_H,
                ))
                .with_min_inner_size(winit::dpi::LogicalSize::new(
                    sonic_ui::prefs::PREFS_MIN_W,
                    sonic_ui::prefs::PREFS_MIN_H,
                ))
                .with_resizable(true),
        );
        let w = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                tracing::error!("prefs window create failed: {e}");
                return;
            }
        };
        let path = sonic_core::config::Config::default_path()
            .unwrap_or_else(|| std::path::PathBuf::from("sonic.toml"));
        self.prefs_state = Some(PrefsState::new(self.config.clone(), path, self.theme.clone()));
        // Spin up a dedicated GPU renderer for the prefs surface.
        // Without this the window's wgpu surface is never drawn into,
        // which is what produced the "preferences window is solid
        // black" repro. Mirror the tear-out-window fix (PR #104) and
        // force the renderer's scale + physical size to match the
        // window's CURRENT values — on macOS the first scale_factor
        // reported inside the constructor is often the stale 1.0 even
        // when the window has been placed on a 2× display.
        // Install the window slot FIRST so any RedrawRequested events
        // posted during renderer construction (e.g. by
        // `force_rebuild_for_scale`, which calls
        // `self.window.request_redraw()` internally) are routed to the
        // prefs handler via `window_event` instead of falling through
        // to the main-window code path. Without this, the early redraw
        // is silently ignored and the prefs window stays blank until
        // an unrelated event happens to land in `handle_prefs_event`.
        self.prefs_window = Some(w.clone());
        match sonic_shared::prefs_renderer::PrefsRenderer::new(w.clone(), el) {
            Ok(mut r) => {
                let real_sf = w.scale_factor() as f32;
                r.force_rebuild_for_scale(real_sf);
                let real_inner = w.inner_size();
                r.resize(real_inner.width.max(1), real_inner.height.max(1));
                // Install the renderer BEFORE the explicit
                // request_redraw below so the queued RedrawRequested
                // finds `prefs_renderer` populated and actually draws.
                self.prefs_renderer = Some(r);
            }
            Err(e) => {
                tracing::error!("prefs renderer init failed: {e}");
            }
        }
        // Belt-and-suspenders: explicitly schedule the first frame now
        // that both renderer + window slot are populated.
        w.request_redraw();
    }
    pub(super) fn commit_prefs_and_apply_live(&mut self) {
        let Some(s) = self.prefs_state.as_mut() else { return };
        if !s.is_dirty() {
            return;
        }
        // Snapshot fields that drive live-apply BEFORE apply() resets
        // the original snapshot (which is what we'd otherwise diff).
        let new_theme_name = s.config.theme.clone();
        let new_keymap_name = s.config.keymap.clone();
        let old_theme_name = self.config.theme.clone();
        let old_keymap_name = self.config.keymap.clone();
        if let Err(e) = s.apply() {
            tracing::error!("prefs apply failed: {e}");
            return;
        }
        // Mirror the saved config into the live App config so any new
        // panes / windows pick up the change.
        self.config = s.config.clone();
        // Live theme apply.
        if new_theme_name != old_theme_name {
            if let Some(loader) = self.theme_loader.as_ref() {
                match loader(&new_theme_name) {
                    Ok(t) => {
                        self.theme = t;
                        if let Some(w) = self.window.as_ref() {
                            w.request_redraw();
                        }
                    }
                    Err(e) => tracing::warn!("live theme reload '{new_theme_name}' failed: {e}"),
                }
            }
        }
        // Live keymap apply.
        if new_keymap_name != old_keymap_name {
            if let Some(loader) = self.keymap_loader.as_ref() {
                match loader(&new_keymap_name) {
                    Ok(k) => self.keymap = k,
                    Err(e) => tracing::warn!("live keymap reload '{new_keymap_name}' failed: {e}"),
                }
            }
        }
    }
    pub(super) fn open_preferences(&mut self) {
        // Already open → just re-focus.
        if let Some(w) = self.prefs_window.as_ref() {
            w.focus_window();
            return;
        }
        // Defer until the event loop has resumed (we need an
        // ActiveEventLoop to create a Window).
        tracing::info!("OpenPreferences requested; awaiting resumed-event-loop hook");
        // The actual creation happens in window_event on next iteration
        // via a pending flag — but to keep diff small we lazily create
        // on the next `WindowEvent::RedrawRequested` of the main window.
        self.pending_prefs_open = true;
    }
}
