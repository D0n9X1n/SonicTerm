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
use sonicterm_vt::vt::{CommandEvent, Parser, VtEvent};
use winit::{
    event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowAttributes, WindowId},
};

use super::{
    key_encoding::{encode_key, encode_logical, key_event_to_string, key_name},
    mark_all_panes_dirty, next_pane_id, pick_prompt_target, resize_all_panes, shell_quote_posix,
    with_integrated_titlebar, wrap_paste, App, FrontmostKind, PaneState, TabState, UserEvent,
    WindowState,
};

impl App {
    pub(super) fn spawn_pane(&self) -> PaneState {
        let (cols, rows) = self.main_renderer().map(|r| r.cells()).unwrap_or((80, 24));
        let (reply_tx, reply_rx) = crossbeam_channel::unbounded::<Vec<u8>>();
        // Honour the user's configured scrollback depth instead of the
        // Grid's built-in 10k default (issue #710).
        let mut grid = Grid::new(cols, rows);
        grid.set_scrollback_limit(self.config.terminal.scrollback);
        let parser = Arc::new(Mutex::new(Parser::new_with_reply(grid, reply_tx)));
        // Seed theme defaults so OSC 10/11/12 `?` queries get a truthful
        // reply — without this nvim guesses (27,29,30) for bg and the
        // neo-tree icon cells visibly differ from SonicTerm's clear surface
        // (#369). Also seeds the OSC 4 palette so CLIs like Copilot can read
        // the full colour set and enable their prompt frame (#661).
        {
            let mut p = parser.lock();
            super::seed_parser_theme_colors(&mut p, &self.theme);
        }
        // Pre-create the redraw target Arc bound to the current parent
        // window. If the pane later tears out, `tear_out_tab` swaps the
        // inner Option to the child window's Arc<Window> so the VT
        // thread re-targets without restarting.
        let redraw_target: Arc<Mutex<Option<Arc<Window>>>> =
            Arc::new(Mutex::new(self.main_window().cloned()));
        let command_events: Arc<Mutex<Vec<super::PaneCommandEvent>>> =
            Arc::new(Mutex::new(Vec::new()));
        let inline_images: Arc<Mutex<Vec<sonicterm_render_model::InlineImage>>> =
            Arc::new(Mutex::new(Vec::new()));
        // PR #400 fix: per-pane cursor_visible Arc lives outside the
        // pty-spawn match so we can store it on PaneState even if pty
        // spawn failed (and so a no-pty pane still has a valid Arc).
        let cursor_visible_pane: Arc<std::sync::atomic::AtomicBool> =
            Arc::new(std::sync::atomic::AtomicBool::new(true));
        // Per-pane kitty-keyboard flags snapshot, mirrored out of the parser
        // by the VT loop so the keypress path reads it lock-free (issue #710).
        let kitty_flags_pane: Arc<std::sync::atomic::AtomicU8> =
            Arc::new(std::sync::atomic::AtomicU8::new(0));
        let pty = match PtyHandle::spawn_default_shell(
            cols,
            rows,
            sonicterm_io::pty::ShellSpawnOpts {
                term_program: self.config.terminal.term_program.clone(),
                ..sonicterm_io::pty::ShellSpawnOpts::default()
            },
        ) {
            Ok(pty) => {
                let parser_clone = parser.clone();
                let out_rx = pty.out_rx.clone();
                let in_tx_reply = pty.in_tx.clone();
                let redraw_target_thread = redraw_target.clone();
                // PR #400 fix: VT thread captures the same Arc that
                // PaneState below will own. Pre-fix this read
                // `self.main().cursor_visible` on WindowState, which
                // got replaced with a fresh Arc on tear-out — leaving
                // the VT thread writing into an orphan AtomicBool.
                let cursor_visible = cursor_visible_pane.clone();
                let kitty_flags = kitty_flags_pane.clone();
                let pty_burst_gen = self.pty_burst_gen.clone();
                let command_events_thread = command_events.clone();
                let inline_images_thread = inline_images.clone();
                // Forward parser replies (DSR/DA/XTVERSION/focus) to the pty
                // master. Kept on its own thread so the VT loop never blocks
                // pushing replies, and so a slow pty doesn't stall parsing.
                std::thread::Builder::new()
                    .name("sonicterm-vt-reply".into())
                    .spawn(move || {
                        while let Ok(bytes) = reply_rx.recv() {
                            if in_tx_reply.send(bytes).is_err() {
                                break;
                            }
                        }
                    })
                    // PANIC: thread spawn at pane init — see sonicterm-io/pty.rs
                    // rationale. Unrecoverable OS-level failure.
                    .expect("spawn vt reply forwarder");
                std::thread::Builder::new()
                    .name("sonicterm-vt-loop".into())
                    .spawn(move || {
                        let mut pending = false;
                        let mut pending_since: Option<Instant> = None;
                        let mut redraw_probe = crate::app::invariants::RedrawCoalescerProbe::new();
                        let mut pending_bytes: usize = 0;
                        let mut command_started: Option<Instant> = None;
                        loop {
                            // Try to drain quickly; if nothing comes for
                            // ~min_interval and we have a pending redraw,
                            // flush it before going back to blocking recv.
                            match out_rx.recv_timeout(if pending {
                                crate::app::PTY_REDRAW_QUIESCENT
                            } else {
                                Duration::from_secs(3600)
                            }) {
                                Ok(bytes) => {
                                    // PR #133/#162: bump generation so the
                                    // next RedrawRequested bypasses the
                                    // vsync coalescing gate. Counter (not
                                    // bool) so a burst arriving during
                                    // render is not erased on completion.
                                    if !bytes.is_empty() {
                                        let prev = pty_burst_gen.fetch_add(1, Ordering::Release);
                                        crate::app::invariants::debug_assert_burst_gen_monotonic(
                                            prev,
                                            prev.wrapping_add(1),
                                        );
                                        pending_bytes = pending_bytes.saturating_add(bytes.len());
                                        pending_since.get_or_insert_with(Instant::now);
                                    }
                                    // Collect side-effects under the parser
                                    // lock, then DROP it before touching winit.
                                    // On macOS `Window::set_title` marshals to
                                    // the AppKit main thread synchronously; if
                                    // we held `parser` across that call and
                                    // the main thread happened to be sitting
                                    // in its RedrawRequested handler waiting
                                    // for `parser.lock()`, both threads would
                                    // deadlock (VT thread waiting on the
                                    // AppKit runloop, main thread waiting on
                                    // parser). This was the v0.6 tear-out
                                    // hang. Same reasoning for
                                    // `request_redraw` below — winit promises
                                    // it's thread-safe, but we keep all winit
                                    // calls outside the parser critical
                                    // section as a defence-in-depth rule.
                                    let mut new_title: Option<String> = None;
                                    let mut command_side_effects = Vec::new();
                                    let mut inline_images = Vec::new();
                                    {
                                        let mut p = parser_clone.lock();
                                        for ev in p.advance(&bytes) {
                                            match ev {
                                                VtEvent::SetTitle(t) => {
                                                    new_title = Some(t);
                                                }
                                                VtEvent::CursorVisibility(v) => {
                                                    cursor_visible.store(
                                                        v,
                                                        std::sync::atomic::Ordering::Relaxed,
                                                    );
                                                }
                                                VtEvent::Command(event) => {
                                                    let now = Instant::now();
                                                    match event {
                                                        CommandEvent::CmdStart => {
                                                            command_started = Some(now);
                                                            command_side_effects.push(
                                                                super::PaneCommandEvent {
                                                                    event,
                                                                    at: now,
                                                                    duration: None,
                                                                },
                                                            );
                                                        }
                                                        CommandEvent::CmdEnd(_) => {
                                                            let duration = command_started
                                                                .take()
                                                                .map(|start| {
                                                                    now.duration_since(start)
                                                                });
                                                            command_side_effects.push(
                                                                super::PaneCommandEvent {
                                                                    event,
                                                                    at: now,
                                                                    duration,
                                                                },
                                                            );
                                                        }
                                                        CommandEvent::PromptStart => {
                                                            command_side_effects.push(
                                                                super::PaneCommandEvent {
                                                                    event,
                                                                    at: now,
                                                                    duration: None,
                                                                },
                                                            );
                                                        }
                                                    }
                                                }
                                                VtEvent::Media(media) => {
                                                    if let Some(image) =
                                                        super::media::decode_inline_image(&media)
                                                    {
                                                        inline_images.push(image);
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                        // Mirror the parser's kitty-keyboard
                                        // flags while we still hold the lock,
                                        // so the keypress path can read them
                                        // without locking (issue #710).
                                        kitty_flags.store(
                                            p.kitty_keyboard_flags(),
                                            std::sync::atomic::Ordering::Relaxed,
                                        );
                                    }
                                    if !inline_images.is_empty() {
                                        let mut images = inline_images_thread.lock();
                                        images.extend(inline_images);
                                        const MAX_INLINE_IMAGES: usize = 128;
                                        if images.len() > MAX_INLINE_IMAGES {
                                            let drop = images.len() - MAX_INLINE_IMAGES;
                                            images.drain(0..drop);
                                        }
                                    }
                                    if !command_side_effects.is_empty() {
                                        command_events_thread.lock().extend(command_side_effects);
                                    }
                                    let _ = new_title;
                                    let pending_for = pending_since
                                        .map(|since| since.elapsed())
                                        .unwrap_or(Duration::ZERO);
                                    if crate::app::should_flush_pending_pty_redraw(
                                        pending_bytes,
                                        pending_for,
                                    ) {
                                        if let Some(w) = redraw_target_thread.lock().as_ref() {
                                            w.request_redraw();
                                        }
                                        let reason = if pending_bytes >= crate::app::PTY_REDRAW_FLUSH_BYTES {
                                            crate::app::invariants::FlushReason::Buffer
                                        } else {
                                            crate::app::invariants::FlushReason::Interval
                                        };
                                        redraw_probe.note_redraw(
                                            crate::app::PTY_REDRAW_QUIESCENT,
                                            reason,
                                        );
                                        pending = false;
                                        pending_since = None;
                                        pending_bytes = 0;
                                    } else {
                                        pending = true;
                                    }
                                }
                                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                                    // Quiescent: flush trailing redraw.
                                    if pending {
                                        if let Some(w) = redraw_target_thread.lock().as_ref() {
                                            w.request_redraw();
                                        }
                                        // Quiescent-timeout flush only fires
                                        // after the channel has been silent
                                        // for `min_interval`, so the spacing
                                        // is naturally satisfied — classify
                                        // as Interval.
                                        redraw_probe.note_redraw(
                                            crate::app::PTY_REDRAW_QUIESCENT,
                                            crate::app::invariants::FlushReason::Interval,
                                        );
                                        pending = false;
                                        pending_since = None;
                                        pending_bytes = 0;
                                    }
                                }
                                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                            }
                        }
                    })
                    // PANIC: thread spawn at pane init — see sonicterm-io/pty.rs
                    // rationale. Unrecoverable OS-level failure.
                    .expect("spawn vt loop");
                Some(pty)
            }
            Err(e) => {
                tracing::error!("failed to spawn pty: {e}");
                None
            }
        };
        let mut state = PaneState::new(parser, pty);
        state.redraw_target = redraw_target;
        state.command_events = command_events;
        state.cursor_visible = cursor_visible_pane;
        state.kitty_flags = kitty_flags_pane;
        state.inline_images = inline_images;
        state
    }
}

impl App {
    pub(super) fn split_active(&mut self, dir: Direction) {
        let new_id = next_pane_id();
        let new_pane = self.spawn_pane();
        let did_split = {
            let Some(ws) = self.main_mut() else { return };
            let i = ws.tabs.active_index();
            let split_ok = {
                let Some(st) = ws.tab_states.get_mut(i) else { return };
                let focus = st.active_pane;
                if st.tree.split(focus, dir, new_id) {
                    st.active_pane = new_id;
                    true
                } else {
                    false
                }
            };
            if split_ok {
                ws.panes.insert(new_id, new_pane);
            }
            split_ok
        };
        if did_split {
            self.resize_visible_panes();
            if let Some(r) = self.main_renderer_mut() {
                r.flash_pane_focus(new_id);
            }
            if let Some(w) = self.main_window() {
                w.request_redraw();
            }
        }
    }
    pub(super) fn close_active_pane(&mut self) {
        let outcome = {
            let Some(ws) = self.main_mut() else { return };
            let i = ws.tabs.active_index();
            let inner = {
                let Some(st) = ws.tab_states.get_mut(i) else { return };
                let focus = st.active_pane;
                if matches!(st.tree, PaneTree::Leaf { id, .. } if id == focus) {
                    (Some(i), None)
                } else {
                    let new_focus =
                        st.tree.leaves().into_iter().find(|id| *id != focus).unwrap_or(focus);
                    if st.tree.close(focus) {
                        st.active_pane = new_focus;
                        (None, Some(focus))
                    } else {
                        (None, None)
                    }
                }
            };
            if let (_, Some(focus)) = inner {
                ws.panes.remove(&focus);
            }
            inner
        };
        match outcome {
            (Some(i), _) => self.close_tab_at(i),
            (_, Some(_focus)) => {
                // #387: the surviving sibling's PaneRect just grew to cover
                // the closed pane's area. Push the new layout into its Grid
                // + PtyHandle (matches split / zoom / resize-split paths and
                // mirrors `close_active_pane_in_child`). Without this the
                // survivor keeps its narrow split-time column count and
                // shell output wraps at the old width until the OS window
                // is resized. The actual resize is delegated to
                // `resize_visible_panes` which routes through the pure
                // helper `resize_panes_to_rects` — the path tested by
                // `close_sibling_pane_resizes_survivor_to_full_width` in
                // `crates/sonicterm-app/tests/per_pane_resize.rs`.
                self.resize_visible_panes();
                if let Some(active_id) = self.active_pane_id() {
                    if let Some(r) = self.main_renderer_mut() {
                        r.flash_pane_focus(active_id);
                    }
                }
                if let Some(w) = self.main_window() {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }
    pub(super) fn focus_pane_dir(&mut self, dir: Direction) {
        let next = {
            let Some(ws) = self.main_mut() else { return };
            let i = ws.tabs.active_index();
            let Some(st) = ws.tab_states.get_mut(i) else { return };
            let Some(next) = st.tree.focus_neighbor(st.active_pane, dir) else { return };
            if st.active_pane == next {
                return;
            }
            st.active_pane = next;
            next
        };
        if let Some(r) = self.main_renderer_mut() {
            r.flash_pane_focus(next);
        }
        if let Some(w) = self.main_window() {
            w.request_redraw();
        }
    }

    pub(super) fn toggle_active_pane_zoom(&mut self) {
        let toggled = {
            let Some(ws) = self.main_mut() else { return };
            let i = ws.tabs.active_index();
            let Some(st) = ws.tab_states.get_mut(i) else { return };
            st.tree.toggle_zoom(st.active_pane)
        };
        if toggled {
            self.resize_visible_panes();
            if let Some(w) = self.main_window() {
                w.request_redraw();
            }
        }
    }

    pub(super) fn toggle_broadcast(&mut self, scope: sonicterm_cfg::keymap::BroadcastScope) {
        self.toggle_broadcast_for(self.frontmost_kind(), scope);
    }

    pub(super) fn toggle_broadcast_for(
        &mut self,
        kind: FrontmostKind,
        scope: sonicterm_cfg::keymap::BroadcastScope,
    ) {
        let Some(source_pane) = self.active_pane_id_for_kind(kind) else { return };
        self.broadcast = self.broadcast.toggled(scope, source_pane);
        self.request_redraw_all_terminal_windows();
    }

    pub(super) fn resize_active_split(&mut self, dir: Direction) {
        let resized = {
            let Some(ws) = self.main_mut() else { return };
            let i = ws.tabs.active_index();
            let Some(st) = ws.tab_states.get_mut(i) else { return };
            st.tree.resize_split(st.active_pane, dir, 0.05)
        };
        if resized {
            self.resize_visible_panes();
            if let Some(w) = self.main_window() {
                w.request_redraw();
            }
        }
    }

    pub(super) fn resize_visible_panes(&mut self) {
        let rects = self.compute_active_pane_rects();
        let (cw, ch) = match self.test_viewport_override {
            // Test-only viewport override (PR #393 follow-up for #387) —
            // lets tests exercise close_active_pane's resize wiring
            // without a live wgpu renderer. Production stays `None` and
            // falls through to the renderer-derived metrics below.
            Some((_, cw, ch)) => (cw, ch),
            None => match self.main_renderer() {
                Some(r) => r.cell_size(),
                None => return,
            },
        };
        if let Some(panes) = self.main_panes() {
            let inset = self
                .main_renderer()
                .map(|r| {
                    [
                        r.padding_left_px(),
                        r.padding_right_px(),
                        r.padding_top_px(),
                        r.padding_bottom_px(),
                    ]
                })
                .unwrap_or([0.0; 4]);
            crate::app::resize_panes_to_rects(panes, &rects, cw, ch, inset);
        }
    }
}
