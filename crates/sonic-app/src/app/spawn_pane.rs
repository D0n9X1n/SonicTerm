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
    vt::{CommandEvent, Parser, VtEvent},
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
use crate::app::integrated_titlebar_inset;
use sonic_ui::prefs::PrefsHit;

impl App {
    pub(super) fn spawn_pane(&self) -> PaneState {
        let (cols, rows) = self.renderer.as_ref().map(|r| r.cells()).unwrap_or((80, 24));
        let (reply_tx, reply_rx) = crossbeam_channel::unbounded::<Vec<u8>>();
        let parser = Arc::new(Mutex::new(Parser::new_with_reply(Grid::new(cols, rows), reply_tx)));
        // Pre-create the redraw target Arc bound to the current parent
        // window. If the pane later tears out, `tear_out_tab` swaps the
        // inner Option to the child window's Arc<Window> so the VT
        // thread re-targets without restarting.
        let redraw_target: Arc<Mutex<Option<Arc<Window>>>> =
            Arc::new(Mutex::new(self.window.clone()));
        let command_events: Arc<Mutex<Vec<super::PaneCommandEvent>>> =
            Arc::new(Mutex::new(Vec::new()));
        let pty = match PtyHandle::spawn_default_shell(cols, rows) {
            Ok(pty) => {
                let parser_clone = parser.clone();
                let out_rx = pty.out_rx.clone();
                let in_tx_reply = pty.in_tx.clone();
                let redraw_target_thread = redraw_target.clone();
                let cursor_visible = self.cursor_visible.clone();
                let pty_burst_gen = self.pty_burst_gen.clone();
                let command_events_thread = command_events.clone();
                // Forward parser replies (DSR/DA/XTVERSION/focus) to the pty
                // master. Kept on its own thread so the VT loop never blocks
                // pushing replies, and so a slow pty doesn't stall parsing.
                std::thread::Builder::new()
                    .name("sonic-vt-reply".into())
                    .spawn(move || {
                        while let Ok(bytes) = reply_rx.recv() {
                            if in_tx_reply.send(bytes).is_err() {
                                break;
                            }
                        }
                    })
                    // PANIC: thread spawn at pane init — see sonic-io/pty.rs
                    // rationale. Unrecoverable OS-level failure.
                    .expect("spawn vt reply forwarder");
                std::thread::Builder::new()
                    .name("sonic-vt-loop".into())
                    .spawn(move || {
                        // Coalesce redraw requests so a burst of pty output
                        // (oh-my-zsh banners, `cat largefile`) doesn't pin
                        // the main thread at 100% CPU re-rendering for every
                        // byte. Drain at least min_interval between bursts,
                        // but ALWAYS schedule a trailing redraw when the
                        // channel briefly quiesces so the final batch lands
                        // on screen (this is the "Enter needs 2 presses" bug
                        // — without the trailing flush, the redraw request
                        // after the prompt redraw was dropped silently).
                        let mut last_request = Instant::now() - Duration::from_secs(1);
                        let mut pending = false;
                        // Debug-only invariant probe: consecutive
                        // request_redraw() calls must respect the same
                        // min_interval the loop enforces; CLAUDE.md §4.
                        let mut redraw_probe = crate::app::invariants::RedrawCoalescerProbe::new();
                        // 16ms min interval keeps the OS from marking the app
                        // unresponsive under bursty pty output (cat largefile,
                        // shell startup banner) while staying within one vsync
                        // frame at 60Hz. See CLAUDE.md §4.
                        let min_interval = Duration::from_millis(16);
                        let mut command_started: Option<Instant> = None;
                        loop {
                            // Try to drain quickly; if nothing comes for
                            // ~min_interval and we have a pending redraw,
                            // flush it before going back to blocking recv.
                            match out_rx.recv_timeout(if pending {
                                min_interval
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
                                                _ => {}
                                            }
                                        }
                                    }
                                    if !command_side_effects.is_empty() {
                                        command_events_thread.lock().extend(command_side_effects);
                                    }
                                    if let Some(t) = new_title {
                                        if let Some(w) = redraw_target_thread.lock().as_ref() {
                                            w.set_title(&format!("Sonic — {t}"));
                                        }
                                    }
                                    if last_request.elapsed() >= min_interval {
                                        if let Some(w) = redraw_target_thread.lock().as_ref() {
                                            w.request_redraw();
                                        }
                                        redraw_probe.note_redraw(min_interval);
                                        last_request = Instant::now();
                                        pending = false;
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
                                        redraw_probe.note_redraw(min_interval);
                                        last_request = Instant::now();
                                        pending = false;
                                    }
                                }
                                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                            }
                        }
                    })
                    // PANIC: thread spawn at pane init — see sonic-io/pty.rs
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
        state
    }
}

impl App {
    pub(super) fn split_active(&mut self, dir: Direction) {
        let new_id = next_pane_id();
        let new_pane = self.spawn_pane();
        let i = self.tabs.active_index();
        let Some(st) = self.tab_states.get_mut(i) else { return };
        let focus = st.active_pane;
        if st.tree.split(focus, dir, new_id) {
            st.active_pane = new_id;
            self.panes.insert(new_id, new_pane);
            self.resize_visible_panes();
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }
    pub(super) fn close_active_pane(&mut self) {
        let i = self.tabs.active_index();
        let Some(st) = self.tab_states.get_mut(i) else { return };
        let focus = st.active_pane;
        if matches!(st.tree, PaneTree::Leaf { id, .. } if id == focus) {
            self.close_tab_at(i);
            return;
        }
        let new_focus = st.tree.leaves().into_iter().find(|id| *id != focus).unwrap_or(focus);
        if st.tree.close(focus) {
            st.active_pane = new_focus;
            self.panes.remove(&focus);
        }
    }
    pub(super) fn focus_pane_dir(&mut self, dir: Direction) {
        let i = self.tabs.active_index();
        let Some(st) = self.tab_states.get_mut(i) else { return };
        if let Some(next) = st.tree.focus_neighbor(st.active_pane, dir) {
            st.active_pane = next;
        }
    }

    pub(super) fn toggle_active_pane_zoom(&mut self) {
        let i = self.tabs.active_index();
        let Some(st) = self.tab_states.get_mut(i) else { return };
        if st.tree.toggle_zoom(st.active_pane) {
            self.resize_visible_panes();
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    pub(super) fn toggle_broadcast(&mut self, scope: sonic_core::keymap::BroadcastScope) {
        let Some(source_pane) = self.active_pane_id() else { return };
        self.broadcast = self.broadcast.toggled(scope, source_pane);
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    pub(super) fn resize_active_split(&mut self, dir: Direction) {
        let i = self.tabs.active_index();
        let Some(st) = self.tab_states.get_mut(i) else { return };
        if st.tree.resize_split(st.active_pane, dir, 0.05) {
            self.resize_visible_panes();
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    fn resize_visible_panes(&mut self) {
        let rects = self.compute_active_pane_rects();
        if let Some(r) = self.renderer.as_ref() {
            let (cw, ch) = r.cell_size();
            crate::app::resize_panes_to_rects(&self.panes, &rects, cw, ch);
        }
    }
}
