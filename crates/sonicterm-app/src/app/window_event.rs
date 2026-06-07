//! `App::do_window_event` — the full `WindowEvent` dispatch body,
//! extracted from `ApplicationHandler::window_event` in refactor PR 8b.
//!
//! This is mechanically the original body wrapped in a separate `impl App`
//! block; field access works because all referenced `App` fields are
//! `pub(super)`.

use std::sync::atomic::Ordering;
use std::time::Instant;

use sonicterm_cfg::keymap::Action;
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_grid::grid::Grid;
use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_ui::selection::Selection;
use sonicterm_ui::tabbar_view::TabBarLayout;
use winit::{
    event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::ActiveEventLoop,
    keyboard::{Key, NamedKey},
    window::{CursorIcon, WindowId},
};

use super::key_encoding::{encode_key, key_event_to_string, key_to_strings};
use super::{mark_all_panes_dirty, App, TabState};

const SPLITTER_HIT_THICKNESS: f32 = 8.0;

impl App {
    pub(super) fn do_window_event(
        &mut self,
        el: &ActiveEventLoop,
        win_id: WindowId,
        event: WindowEvent,
    ) {
        // PR #132: mark any user-driven event so the next
        // RedrawRequested bypasses the vsync coalescing gate. This
        // covers main and child windows uniformly. PTY-byte
        // redraws (the high-volume path) arrive as RedrawRequested
        // with this flag still false and continue to coalesce.
        if matches!(
            event,
            WindowEvent::KeyboardInput { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::CursorMoved { .. }
                | WindowEvent::CursorEntered { .. }
                | WindowEvent::CursorLeft { .. }
                | WindowEvent::ModifiersChanged(_)
                | WindowEvent::Ime(_)
                | WindowEvent::Resized(_)
                | WindowEvent::ScaleFactorChanged { .. }
                | WindowEvent::Focused(_)
        ) {
            self.input_dirty = true;
        }
        // Drain any pending sonicterm.toml live-reload deliveries before
        // dispatching the event — guarantees font/theme/keymap swaps
        // land on the same redraw tick they were detected on.
        self.poll_config_reload();
        // Tear-out child windows: route to the dedicated handler so
        // each child renders/handles input on its own surface.
        // Phase B2 PR-A: the main window also lives in `self.windows`
        // now (shadow entry, `Some(main_window_id)`), but its events
        // must continue to flow through the legacy `App.*` paths
        // below until PR-B swaps readers. Skip the shadow id explicitly.
        if self.windows.contains_key(&win_id) && Some(win_id) != self.main_window_id {
            self.handle_child_window_event(el, win_id, event);
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                // M6a-expand-2c-window: notify the reducer of the
                // close request. The reducer mutates
                // `AppState::{live_window_count, focused_window}` and
                // emits `WindowClose` [+ `Quit` if last]. The
                // boundary's existing macOS-style "hide instead of
                // exit" policy below is the source of truth for what
                // the platform actually does; the reducer's Effects
                // are observability-only in this slice (the
                // `dispatch_effects` arms for `WindowClose` /
                // `WindowOpen` / `WindowResize` are trace-stubs per
                // §9). The `Quit` cascade does flip `pending_exit` —
                // suppress that here so we don't override the
                // "hide-on-last-close" policy. Real Quit cascading
                // moves to the reducer in 2c-misc.
                let intent = sonicterm_app_core::AppIntent::WindowCloseRequested {
                    window: sonicterm_types::WindowKey::new(0),
                };
                for effect in self.machine.handle(intent) {
                    if !matches!(
                        effect,
                        sonicterm_app_core::AppEffect::Quit
                            | sonicterm_app_core::AppEffect::WindowClose { .. }
                    ) {
                        self.dispatch_effects(smallvec::smallvec![effect]);
                    }
                }
                // If child windows still own tabs, hide the main
                // window instead of exiting the app — the children
                // are independent live terminals and must keep
                // running. Only exit when nothing else is alive.
                if self.child_window_count() == 0 {
                    if Self::should_exit_on_last_window_close(&self.config) {
                        el.exit();
                    } else {
                        // Chrome/Firefox/Safari-style on macOS: keep the
                        // process alive after the last window closes so
                        // the user can `Cmd+N` (or use the dock menu) to
                        // open a fresh window without cold-start cost.
                        // The main window is hidden either way — on
                        // non-macOS we would have exited above.
                        self.hide_main_window();
                    }
                } else {
                    self.hide_main_window();
                }
            }

            WindowEvent::RedrawRequested => {
                let was_dirty = self.input_dirty;
                let pty_burst_snapshot = self.pty_burst_gen.load(Ordering::Acquire);
                let pty_burst = pty_burst_snapshot != self.last_seen_burst_gen;
                // Perf audit #9: if we already rendered within the
                // current vsync window, defer this redraw until the
                // next monitor refresh boundary. `about_to_wait` will
                // see `pending_redraw` and call
                // `set_control_flow(WaitUntil(last_render +
                // frame_period))`; `new_events`' ResumeTimeReached arm
                // then re-requests the redraw. Net effect: bursty PTY
                // output coalesces into one frame per vsync instead of
                // burning the GPU at the VT thread's 16ms tick rate.
                // PR #132 review: input-driven redraws must be
                // immediate — gating them on the vsync deadline adds
                // perceptible latency to typing/resize/theme changes.
                // Only redraws that arrive purely from streaming PTY
                // bytes (input_dirty stays false) get coalesced.
                let last_render = self.main().map(|ws| ws.last_render).unwrap_or_else(Instant::now);
                if !was_dirty && !pty_burst && last_render.elapsed() < self.frame_period {
                    self.pending_redraw = true;
                    return;
                }
                self.pending_redraw = false;
                let main_id_opt = self.main_window_id;
                if let Some(id) = main_id_opt {
                    if let Some(ws) = self.windows.get_mut(&id) {
                        ws.tabs.clear_expired_command_badges(Instant::now());
                    }
                }
                self.poll_command_events_for_all_tabs();
                let tab_idx = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
                // Compute per-pane rects in window pixels so the renderer can
                // draw a border around each one (and a brighter one around
                // the focused pane). The active pane's grid is rendered into
                // the full content area; per-pane Buffer rendering is v0.4.
                let pane_rects: Vec<(u64, sonicterm_ui::pane::Rect)> = self
                    .main_tab_states()
                    .and_then(|ts| ts.get(tab_idx))
                    .map(|st| {
                        if let Some(r) = self.main_renderer() {
                            let (w, h) = r.logical_size();
                            let top = (r.top_inset() - r.padding_top_px()).max(0.0);
                            let bottom = r.bottom_inset();
                            let outer = sonicterm_ui::pane::Rect::new(
                                0.0,
                                top,
                                w.max(0.0),
                                (h - top - bottom).max(0.0),
                            );
                            st.tree.layout(outer)
                        } else {
                            Vec::new()
                        }
                    })
                    .unwrap_or_default();
                let active_id = self
                    .main_tab_states()
                    .and_then(|ts| ts.get(tab_idx))
                    .map(|st| st.active_pane)
                    .unwrap_or(0);
                let broadcast_receivers = self.broadcast_receivers();

                // #386 PR-D: per-pane scrollbar visibility/fade tick.
                // Built BEFORE the try_lock pass since it only needs
                // logical-px rects (already in `pane_rects`) and the
                // already-captured cursor pos / scrollbar_drag — no
                // parser lock needed. Result feeds each PaneRender's
                // `scrollbar_alpha` below.
                let scrollbar_alpha_map: std::collections::HashMap<u64, f32> = {
                    let mode = self.config.appearance.scrollbar;
                    let drag_pane =
                        self.main().and_then(|ws| ws.scrollbar_drag.as_ref().map(|s| s.pane_id));
                    let (cx, cy) = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                    let cursor = (cx as f32, cy as f32);
                    let rects: Vec<(u64, f32, f32, f32, f32)> =
                        pane_rects.iter().map(|(id, r)| (*id, r.x, r.y, r.w, r.h)).collect();
                    let now = Instant::now();
                    if let Some(ws) = self.main_mut() {
                        crate::app::scrollbar_visibility::update_and_collect(
                            &mut ws.scrollbar_vis,
                            &rects,
                            cursor,
                            active_id,
                            drag_pane,
                            mode,
                            now,
                        )
                    } else {
                        std::collections::HashMap::new()
                    }
                };
                // Keep redrawing while any pane's scrollbar fade is
                // animating so a paused mouse-leave still completes the
                // 300 ms fade-out (otherwise the bar would stay frozen
                // mid-fade until the next external event).
                let scrollbar_needs_more_frames = {
                    let mode = self.config.appearance.scrollbar;
                    let drag_pane =
                        self.main().and_then(|ws| ws.scrollbar_drag.as_ref().map(|s| s.pane_id));
                    self.main()
                        .map(|ws| {
                            ws.scrollbar_vis.iter().any(|(id, st)| {
                                crate::app::scrollbar_visibility::is_animating(
                                    st,
                                    mode,
                                    drag_pane == Some(*id),
                                )
                            })
                        })
                        .unwrap_or(false)
                };
                if scrollbar_needs_more_frames {
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                }

                // PR #199 Fix 1: try_lock EVERY pane in the tab and pass
                // them ALL through to the renderer. The previous single-
                // element slice meant the per-pane loop inside
                // `GpuRenderer::render` never iterated inactive panes in
                // production frames — that was the visible "right pane
                // empty after split" bug.
                //
                // Strategy: clone every pane's parser Arc, try to lock
                // all of them in one pass. If ANY lock fails, defer the
                // redraw (§4 land-mine) and bail — partial frames are
                // not allowed because the renderer needs a coherent
                // multi-pane view, and a re-locked sub-pane would
                // produce torn output. Order is pane_rects order;
                // active position is recorded separately.
                let main_panes_for_arcs = self.main_panes();
                let inline_images_by_pane: std::collections::HashMap<
                    u64,
                    Vec<sonicterm_render_model::InlineImage>,
                > = main_panes_for_arcs
                    .map(|panes| {
                        panes
                            .iter()
                            .map(|(id, pane)| (*id, pane.inline_images.lock().clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                let parser_arcs: Vec<(
                    u64,
                    std::sync::Arc<parking_lot::Mutex<sonicterm_vt::vt::Parser>>,
                    sonicterm_ui::pane::Rect,
                )> = pane_rects
                    .iter()
                    .filter_map(|(id, rect)| {
                        main_panes_for_arcs
                            .and_then(|panes| panes.get(id))
                            .map(|p| (*id, std::sync::Arc::clone(&p.parser), *rect))
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
                            // SAFETY: extending the guard's lifetime to
                            // the outer scope. `arc` lives in `parser_arcs`
                            // which is dropped strictly after `guards`, so
                            // the underlying Mutex outlives every guard.
                            // parking_lot guards carry a `*const Mutex`
                            // internally and no `'a` tied to `arc`.
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
                    self.defer_redraw_on_lock_contention(was_dirty);
                    return;
                }

                if let Some(r) = self.main_renderer_mut() {
                    r.set_inactive_pane_cursors(Vec::new());
                }

                let cheatsheet_render = (self.cheatsheet_open
                    && self.cheatsheet_attached_window.is_none())
                .then(|| (self.cheatsheet.clone(), self.cheatsheet_bindings()));
                // PR-B1a: lift the main window Arc clone before the
                // mut borrow on `self.renderer` below, so the IME
                // cursor-area branch can still touch
                // `ws.ime_cursor_throttle` (mut) without re-borrowing
                // `self`.
                let main_window_for_ime = self.main_window().cloned();
                // PR-B1b borrow-split: pull the renderer out via direct
                // map-lookup on `self.windows` (NOT through `main_renderer_mut`,
                // which would borrow all of `self`). That keeps
                // `self.command_palette`, `self.ime` available for the
                // disjoint mut borrows the render call needs in the same
                // expression scope.
                // PR-B2c (#365): panes now live in `ws` too, so they're
                // pulled from the same field-disjoint split borrow.
                let main_id_opt = self.main_window_id;
                let ws_opt = main_id_opt.and_then(|id| self.windows.get_mut(&id));
                #[allow(clippy::type_complexity)]
                let (
                    renderer_opt,
                    tabs_opt,
                    tab_states_opt,
                    panes_opt,
                    cursor_visible_now,
                    last_render_slot,
                    ws_selection_ref,
                    ws_copy_mode_ref,
                    ws_ime_ref,
                    ws_ime_throttle_ref,
                    ws_viewport_tops,
                ): (
                    Option<&mut GpuRenderer>,
                    Option<&mut sonicterm_ui::tabs::TabBar>,
                    Option<&mut Vec<TabState>>,
                    Option<&mut std::collections::HashMap<u64, crate::app::PaneState>>,
                    bool,
                    Option<&mut Instant>,
                    Option<&Selection>,
                    Option<&CopyModeState>,
                    Option<&sonicterm_ui::ime::ImeState>,
                    Option<&mut sonicterm_ui::ime::ImeCursorThrottle>,
                    std::collections::HashMap<u64, Option<u64>>,
                ) = match ws_opt {
                    Some(ws) => {
                        // PR #400: cursor_visible is now per-pane; read
                        // it from the active pane before splitting the
                        // mut borrow of `ws.panes`. Bool read, no
                        // lasting borrow.
                        let cv = ws
                            .panes
                            .get(&active_id)
                            .map(|p| p.cursor_visible.load(std::sync::atomic::Ordering::Relaxed))
                            .unwrap_or(true);
                        // PR-B3c (#365): selection + copy_mode now live on
                        // `ws`. Pull immutable refs disjoint from the mut
                        // borrows of `ws.{renderer,tabs,tab_states,panes,last_render}`.
                        // PR-B3d (#365): ime + ime_cursor_throttle also live
                        // on `ws`; split-borrow disjointly too.
                        let sel_ref = ws.selection.as_ref();
                        let cm_ref = ws.copy_mode.as_ref();
                        let viewport_tops = ws
                            .panes
                            .iter()
                            .map(|(id, pane)| (*id, pane.viewport_top_abs))
                            .collect();
                        (
                            ws.renderer.as_mut(),
                            Some(&mut ws.tabs),
                            Some(&mut ws.tab_states),
                            Some(&mut ws.panes),
                            cv,
                            Some(&mut ws.last_render),
                            sel_ref,
                            cm_ref,
                            Some(&ws.ime),
                            Some(&mut ws.ime_cursor_throttle),
                            viewport_tops,
                        )
                    }
                    None => (
                        None,
                        None,
                        None,
                        None,
                        true,
                        None,
                        None,
                        None,
                        None,
                        None,
                        std::collections::HashMap::new(),
                    ),
                };
                if let (Some(r), Some(pane), Some(tabs_mref), Some(tab_states_mref)) = (
                    renderer_opt,
                    panes_opt.and_then(|p| p.get_mut(&active_id)),
                    tabs_opt,
                    tab_states_opt,
                ) {
                    let cursor_rc = {
                        // PR #199 Fix 1: the active pane's parser guard is
                        // already in `guards` from the global try_lock pass
                        // above; locking it again here would AB-BA deadlock
                        // (we already hold it). Find the active guard via
                        // a mut borrow over `guards`.
                        let active_pos = guards
                            .iter()
                            .position(|(id, _, _)| *id == active_id)
                            // PANIC: safe — `guards` was populated immediately
                            // above from `tab.panes` keyed by `active_id`, so
                            // a guard with this id is present. Render hot
                            // path: do NOT convert to Result (CLAUDE.md §4 —
                            // this fn must never block or crash the terminal).
                            .expect("active pane guard collected above");
                        // Wezterm-style tab title: `#N icon parent/leaf`.
                        // Pull cwd from OSC 7, the foreground process from
                        // the pid probe (macOS only for now), and the OSC
                        // 0/2 title as the last-resort body (so `ssh
                        // user@host` still labels itself).
                        //
                        // Shared with `app/child_window.rs` via
                        // `refresh_active_tab_title` so Cmd+N / tear-out
                        // windows pick up cwd-based titles too (was
                        // previously stuck on the literal "shell N"
                        // placeholder set at spawn time).
                        let _ = crate::app::refresh_active_tab_title(
                            tabs_mref,
                            pane,
                            &guards[active_pos].1,
                            tab_idx,
                        );
                        if let Some(search) =
                            tab_states_mref.get_mut(tab_idx).and_then(|t| t.search.as_mut())
                        {
                            search.maybe_refresh_for_revision(guards[active_pos].1.grid_mut());
                        }
                        let search = tab_states_mref.get(tab_idx).and_then(|t| t.search.as_ref());
                        // PR #199 Fix 1: build the slice from ALL panes
                        // (was previously a single-element slice for the
                        // active pane only). The renderer's per-pane loop
                        // now actually iterates every pane in production
                        // frames, so split panes paint.
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
                                viewport_top_abs: ws_viewport_tops.get(id).copied().flatten(),
                                is_active: *id == active_id,
                                cursor_style: sonicterm_render_model::CursorStyle::default(),
                                is_broadcast_receiver: broadcast_receivers.contains(id),
                                scrollbar_alpha: scrollbar_alpha_map
                                    .get(id)
                                    .copied()
                                    .unwrap_or(0.0),
                                inline_images: inline_images_by_pane
                                    .get(id)
                                    .cloned()
                                    .unwrap_or_default(),
                            })
                            .collect();
                        if let Err(e) = r.render(
                            &mut panes_slice,
                            &self.theme,
                            cursor_visible_now,
                            ws_selection_ref,
                            ws_copy_mode_ref,
                            tabs_mref,
                            search,
                            // Epic #289 follow-up: only feed the palette
                            // to the main window when it's actually
                            // attached here (None == main). Otherwise the
                            // palette would paint on the wrong window.
                            self.palette_attached_window
                                .is_none()
                                .then_some(&mut self.command_palette),
                            cheatsheet_render,
                            ws_ime_ref,
                            pane.viewport_top_abs,
                        ) {
                            tracing::warn!("render error: {e}");
                        }
                        self.input_dirty = false;
                        // PR #162: mark only the generation sampled at
                        // the start of this RedrawRequested as seen.
                        // A burst arriving during render keeps the
                        // counter ahead of last_seen_burst_gen so the
                        // next redraw bypasses the vsync gate.
                        self.last_seen_burst_gen = pty_burst_snapshot;
                        if let Some(lr) = last_render_slot {
                            *lr = Instant::now();
                        }
                        let g = guards[active_pos].1.grid_mut();
                        (g.cursor.row, g.cursor.col)
                    };
                    // Epic #289 Phase C2 — refresh the OS-drag tab bar
                    // snapshot so cross-window drop hit-tests see the
                    // current layout. Drops between PRs read this; an
                    // empty registry means every drop resolves to
                    // `DroppedOnEmpty` (the bug PR #295 review caught).
                    // (moved after the renderer borrow scope below)
                    // Tell the OS where the active text cursor lives so the
                    // IME candidate window (pinyin candidates, Japanese
                    // romaji selector, Korean Hangul composer) appears
                    // immediately below the cell being edited — not
                    // pinned to the top-left corner of the screen as
                    // happens when the area is never set.
                    if let Some(w) = main_window_for_ime {
                        if let Some(throttle) = ws_ime_throttle_ref {
                            if throttle.should_update(cursor_rc.0, cursor_rc.1) {
                                let x = r.padding_left_px() + f32::from(cursor_rc.1) * r.cell_w;
                                let y = r.top_inset() + f32::from(cursor_rc.0) * r.cell_h;
                                let pos = winit::dpi::PhysicalPosition::new(x as i32, y as i32);
                                let size = winit::dpi::PhysicalSize::new(
                                    r.cell_w.ceil() as u32,
                                    r.cell_h.ceil() as u32,
                                );
                                w.set_ime_cursor_area(pos, size);
                            }
                        }
                    }
                }
                // Epic #289 Phase C2 — refresh OS-drag tab bar snapshot
                // for the main window. Outside the renderer borrow scope
                // so the immutable self borrow doesn't conflict with `r`.
                self.publish_main_window_tab_bar();
            }

            WindowEvent::Focused(focused) => {
                // M6a-expand-2c-window: route focus transitions
                // through the reducer. The reducer mutates
                // `AppState::focused_window` and emits a
                // `Render(Focus)` only on actual transition (no spam
                // on duplicate Focused(true)). The boundary's
                // existing per-pane dirty-mark + `request_redraw`
                // below stays as the production paint path; the
                // reducer's Render is observability-only here (and
                // dedups via `dispatch_effects`' redraw counter).
                let wk = sonicterm_types::WindowKey::new(0);
                let intent = if focused {
                    sonicterm_app_core::AppIntent::WindowFocused { window: wk }
                } else {
                    sonicterm_app_core::AppIntent::WindowBlurred { window: wk }
                };
                self.dispatch_intent(intent);
                if focused {
                    // Epic #289 Phase A — record the main window as
                    // OS-frontmost so keymap_dispatch / menubar drain
                    // route subsequent Cmd+T / Cmd+W / Cmd+\\ to the
                    // main window's tabs vec. PR-B4 (#365) removed the
                    // sibling `focused_child` clear — `frontmost_window`
                    // discriminates main vs child via `frontmost_kind()`.
                    self.frontmost_window = Some(win_id);
                } else if self.frontmost_window == Some(win_id) {
                    // Only clear if WE were the recorded frontmost.
                    // Focus moving to a sibling sonic window arrives as
                    // that window's own `Focused(true)` and overwrites
                    // frontmost in the right order; if the user is just
                    // switching to another app we end up at `None` here
                    // which makes terminal actions fall back to main
                    // (safe default).
                    self.frontmost_window = None;
                }
                // Reset IME state across focus transitions. When focus is
                // lost mid-composition, the OS IME panel detaches without
                // sending us a Commit; dropping the preedit avoids replaying
                // stale composition state on the next focus-in. Toggling
                // `set_ime_allowed` nudges the OS to re-attach the input
                // context cleanly on macOS / Windows.
                if let Some(ws) = self.main_mut() {
                    ws.ime.cancel();
                }
                // Propagate window focus to the renderer so the text cursor
                // disappears when the window is inactive.
                if let Some(r) = self.main_renderer_mut() {
                    r.set_window_focused(focused);
                }
                // Focus transition changes cursor visibility only, so mark
                // every pane dirty without bumping grid revision.
                if let Some(panes) = self.main_panes() {
                    mark_all_panes_dirty(panes);
                }
                // Forward focus in/out to the active pane if it asked for
                // focus reporting via DECSET ?1004 (CSI ?1004h).
                if let Some(pane) = self.active_pane() {
                    let enabled = pane.parser.lock().focus_reporting_enabled();
                    if enabled {
                        if let Some(pty) = pane.pty.as_ref() {
                            let seq: &[u8] = if focused { b"\x1b[I" } else { b"\x1b[O" };
                            let _ = pty.in_tx.send(seq.to_vec());
                        }
                    }
                }
                if let Some(w) = self.main_window().cloned() {
                    // Intentionally do NOT toggle `set_ime_allowed` on
                    // focus transitions. macOS' IMK posts a runloop
                    // wake message on every toggle; doing it on every
                    // focus in/out (which SonicTerm also receives when the
                    // OS shows a notification, switches Spaces, etc.)
                    // floods stderr with
                    // `IMKCFRunLoopWakeUpReliable` errors and is a
                    // suspected cause of long-session hangs. IME is
                    // already enabled once at window creation; winit
                    // suspends delivery on focus-out automatically.
                    // Also invalidate the cursor-area throttle so the
                    // first redraw after refocus re-teaches the OS the
                    // current cell position.
                    if focused {
                        if let Some(ws) = self.main_mut() {
                            ws.ime_cursor_throttle.reset();
                        }
                    }
                    w.request_redraw();
                }
            }

            WindowEvent::Resized(size) => {
                if let Some(r) = self.main_renderer_mut() {
                    r.resize(size.width, size.height);
                }
                // M6a-expand-2c-window: notify the reducer of the
                // new logical grid dimensions. Derive cols/rows from
                // the renderer's cell size; fall back to zero when
                // unavailable (smoke-test environments). The
                // reducer's `WindowResize` Effect is observability-
                // only — the boundary above already drove the wgpu
                // resize, and the existing `request_redraw` below is
                // the production paint path.
                let (cols_u16, rows_u16) = {
                    let cell = self.main_renderer().map(GpuRenderer::cell_size);
                    match cell {
                        Some((cw, ch)) if cw > 0.0 && ch > 0.0 => (
                            ((size.width as f32 / cw).floor() as u32).min(u16::MAX as u32) as u16,
                            ((size.height as f32 / ch).floor() as u32).min(u16::MAX as u32) as u16,
                        ),
                        _ => (0u16, 0u16),
                    }
                };
                self.dispatch_intent(sonicterm_app_core::AppIntent::WindowResized {
                    window: sonicterm_types::WindowKey::new(0),
                    cols: cols_u16,
                    rows: rows_u16,
                });
                // Per-pane sizing: each pane's grid + PTY is resized to
                // its own PaneRect within the (new) window content area,
                // not the whole window's (cols, rows). Pre-fix, inactive
                // panes thought they were full-window-wide and TUIs drew
                // past their visible border. See docs/specs/per-pane-grids.md.
                let rects = self.compute_active_pane_rects();
                let metrics = self.main_renderer().map(|r| {
                    (
                        r.cell_size(),
                        [
                            r.padding_left_px(),
                            r.padding_right_px(),
                            r.padding_top_px(),
                            r.padding_bottom_px(),
                        ],
                    )
                });
                if let (Some(((cw, ch), inset)), Some(panes)) = (metrics, self.main_panes()) {
                    crate::app::resize_panes_to_rects(panes, &rects, cw, ch, inset);
                }
                // Cell geometry changed — force the next render to
                // re-publish the IME cursor area even if (row, col) is
                // unchanged, otherwise the OS candidate window stays
                // pinned to the pre-resize pixel location.
                if let Some(ws) = self.main_mut() {
                    ws.ime_cursor_throttle.reset();
                }
                if let Some(w) = self.main_window() {
                    w.request_redraw();
                }
            }

            WindowEvent::ScaleFactorChanged { scale_factor: dpi_scale, .. } => {
                if let Some(ws) = self.main_mut() {
                    ws.dpi_scale = dpi_scale;
                }
                if let Some(id) = self.main_window_id {
                    if let Some(ws) = self.windows.get_mut(&id) {
                        crate::app::apply_dpi_to_renderer_if_present(&mut ws.renderer, dpi_scale);
                    }
                }
                if let Some(w) = self.main_window() {
                    w.request_redraw();
                }
            }

            WindowEvent::ModifiersChanged(m) => {
                if let Some(ws) = self.main_mut() {
                    ws.modifiers = m.state();
                }
                // Releasing the open-URL modifier must clear any
                // visible Cmd+hover URL underline (and revert the
                // pointer to default if it was previously shown). We
                // recompute hover state from the last cursor position
                // so a subsequent re-press while still hovering brings
                // the affordance back without needing a CursorMoved.
                self.refresh_hovered_url();
                if let Some(w) = self.main_window() {
                    w.request_redraw();
                }
            }

            // -- Mouse --
            WindowEvent::CursorLeft { .. } => {
                let mut redraw = false;
                if let Some(r) = self.main_renderer_mut() {
                    redraw = r.set_hover_cursor(None);
                }
                if let Some(ws) = self.main_mut() {
                    ws.splitter_hover = None;
                }
                if let Some(w) = self.main_window() {
                    w.set_cursor(CursorIcon::Default);
                }
                if self.main_mut().and_then(|ws| ws.hovered_url.take()).is_some() {
                    redraw = true;
                }
                if redraw {
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(ws) = self.main_mut() {
                    ws.cursor_pos = (position.x, position.y);
                }
                let (lx, ly) = (position.x as f32, position.y as f32);
                // M6a-expand-2c-mouse: notify the reducer so
                // `last_mouse_pos` tracks the cursor; the reducer's
                // identity check implicitly coalesces sub-pixel jitter
                // bursts into a single Render(Hover) per frame.
                self.dispatch_intent(sonicterm_app_core::AppIntent::MouseMove {
                    window: sonicterm_types::WindowKey::new(0),
                    pos: sonicterm_app_core::LogicalPos { x: lx as f64, y: ly as f64 },
                });
                let mut hover_redraw = false;
                if let Some(r) = self.main_renderer_mut() {
                    hover_redraw = r.set_hover_cursor(Some((lx, ly)));
                }
                if hover_redraw {
                    // A bare hover-move over the tab bar must repaint —
                    // otherwise the muted × → bright × transition lags
                    // until the next unrelated event.
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                }
                // Auto scrollbar hover is also pure cursor state. Terminal
                // cursor moves from normal PTY output do not wake the renderer,
                // so request a frame exactly when the pointer crosses the
                // right-edge proximity threshold.
                let _ = self.refresh_scrollbar_hover_from_cursor();
                if self.apply_splitter_drag(lx, ly) {
                    return;
                }
                // Update the live drag session position so the chip
                // can follow the cursor in the renderer overlay.
                let drag_snapshot = self.main_mut().and_then(|ws| {
                    ws.drag_session.as_mut().map(|s| {
                        s.current_pos = (lx, ly);
                        (s.press_tab_index, *s)
                    })
                });
                if let Some((press_idx, session_snapshot)) = drag_snapshot {
                    let title = self
                        .main_tabs()
                        .and_then(|t| t.tabs().get(press_idx).map(|tab| tab.title.clone()))
                        .unwrap_or_default();
                    let window_width =
                        self.main_window().map(|w| w.inner_size().width as f32).unwrap_or(0.0);
                    let (bar_h, top_off, visible) = self
                        .main_renderer()
                        .map(|r| {
                            (r.tab_bar_logical_height(), r.tab_bar_y_offset(), r.tab_bar_visible())
                        })
                        .unwrap_or((sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT, 0.0, true));
                    let empty_tabs = sonicterm_ui::tabs::TabBar::new();
                    let layout = TabBarLayout::compute_with_height(
                        self.main_tabs().unwrap_or(&empty_tabs),
                        window_width,
                        bar_h,
                    )
                    .with_top_offset(top_off)
                    .with_visible(visible);
                    let chip =
                        crate::tab_drag::build_drag_chip_overlay(&session_snapshot, &layout, title);
                    if let Some(r) = self.main_renderer_mut() {
                        r.set_drag_chip(chip);
                    }
                }
                // Cross-window drag-merge: if a tab is held, update the
                // pending drop target based on the global cursor
                // position. The actual decision (tear / merge / cancel)
                // is deferred to mouse-up via `compute_action`.
                let (mouse_down, has_press) = self
                    .main()
                    .map(|ws| (ws.mouse_down, ws.pressed_tab.is_some()))
                    .unwrap_or((false, false));
                if mouse_down && has_press {
                    let target = self.compute_main_drag_target((position.x, position.y));
                    if let Some(ws) = self.main_mut() {
                        ws.drag_target = target;
                    }
                    // Phase C2 (PR #295 review fix): start the OS-level
                    // drag session AS SOON AS the cursor crosses the
                    // drag-start threshold from its press point, not on
                    // mouse-release. Releasing first means `DoDragDrop`
                    // (Windows) or NSDraggingSession (macOS) get no
                    // live button to capture the cursor with, so the
                    // OS-level cross-window cursor capture never
                    // engages. The `os_drag_handoff_started` flag
                    // ensures we only attempt the handoff once per
                    // gesture; if it succeeds the backend owns the
                    // gesture end-to-end (Windows) or has already
                    // written the pasteboard (macOS).
                    if !self.os_drag_handoff_started {
                        let started_idx = self.main().and_then(|ws| {
                            ws.drag_session
                                .as_ref()
                                .filter(|s| crate::tab_drag::drag_moved_enough(s))
                                .map(|s| s.press_tab_index)
                        });
                        if let Some(idx) = started_idx {
                            self.os_drag_handoff_started = true;
                            let _ = self.try_os_drag_handoff(idx);
                        }
                    }
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                    return;
                }
                if self.main().map(|ws| ws.mouse_down).unwrap_or(false) {
                    // #386 PR-C: scrollbar drag takes priority over
                    // selection extension while a thumb is held. Match
                    // CLAUDE.md §4 — keep this branch fast; no parser
                    // lock is needed (geometry was snapshotted at press).
                    if let Some((pane_id, new_view_top)) = self.scrollbar_drag_apply(lx, ly) {
                        // Resolve `live_top` for the dragged pane (not
                        // necessarily the active one — keep the gesture
                        // pinned to the press pane even if focus shifted).
                        let live_top_opt = self.main().and_then(|ws| {
                            ws.panes.get(&pane_id).and_then(|p| {
                                p.parser.try_lock().map(|parser| {
                                    let g = parser.grid();
                                    g.scrollback_len() as u64
                                })
                            })
                        });
                        if let Some(live_top) = live_top_opt {
                            if let Some(ws) = self.main_mut() {
                                if let Some(pane) = ws.panes.get_mut(&pane_id) {
                                    pane.viewport_top_abs = if new_view_top >= live_top {
                                        None
                                    } else {
                                        Some(new_view_top)
                                    };
                                }
                                super::mark_all_panes_dirty(&ws.panes);
                                if let Some(w) = ws.window.as_ref() {
                                    w.request_redraw();
                                }
                            }
                        }
                        // #386 PR-D: drag also counts as scrollbar activity.
                        self.mark_scrollbar_active(pane_id);
                        return;
                    }
                    if let Some(r) = self.main_renderer() {
                        if let Some((row, col)) =
                            r.pixel_to_cell(position.x as f32, position.y as f32)
                        {
                            // PR-B3c (#365): selection lives on WindowState.
                            // Split-borrow `ws.selection` and `ws.panes`
                            // disjointly.
                            if let Some(ws) = self.main_mut() {
                                if let Some(sel) = ws.selection.as_mut() {
                                    sel.extend(row, col);
                                    mark_all_panes_dirty(&ws.panes);
                                    if let Some(w) = ws.window.as_ref() {
                                        w.request_redraw();
                                    }
                                }
                            }
                        }
                    }
                } else {
                    // Hover-without-button: recompute the OSC8/auto-URL
                    // hover state. Auto-detected URLs are gated on the
                    // platform open-URL modifier (Cmd / Ctrl) per the
                    // v1.0 Cmd-held-hover affordance; OSC 8 keeps its
                    // unconditional pointer affordance.
                    if !self.refresh_splitter_hover(lx, ly) {
                        self.refresh_hovered_url();
                    }
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                // #412: route wheel events to the pane under the cursor.
                // Default 3 lines per LineDelta tick (matches stock GTK
                // / Cocoa wheel feel). PixelDelta divides by the live
                // cell height so trackpad scrolls match font size.
                let cursor_pos = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                let (lx, ly) = (cursor_pos.0 as f32, cursor_pos.1 as f32);
                let cell_h = self
                    .main_renderer()
                    .map(|r| r.cell_size().1)
                    .filter(|h| *h > 0.0)
                    .unwrap_or(16.0);
                let lines_per_tick: f32 = 3.0;
                let delta_lines_f: f32 = match delta {
                    // winit's y is positive when scrolling UP (away from
                    // user); we want negative delta_lines for "scroll
                    // back into history".
                    MouseScrollDelta::LineDelta(_x, y) => -y * lines_per_tick,
                    MouseScrollDelta::PixelDelta(pos) => -(pos.y as f32) / cell_h,
                };
                // Round away from zero so a tiny trackpad nudge still
                // produces at least one line of motion.
                let delta_lines = if delta_lines_f >= 0.0 {
                    delta_lines_f.ceil() as i32
                } else {
                    delta_lines_f.floor() as i32
                };
                if delta_lines != 0 {
                    if let Some(pane_id) = self.pane_at_cursor(lx, ly) {
                        self.scroll_pane(pane_id, delta_lines);
                    }
                }
            }

            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => match state {
                ElementState::Pressed => {
                    // M6a-expand-2c-mouse: notify reducer of the
                    // press/release transition (Render(Selection)).
                    {
                        let cp = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                        let (lx, ly) = (cp.0 as f32, cp.1 as f32);
                        self.dispatch_intent(sonicterm_app_core::AppIntent::MouseButton {
                            window: sonicterm_types::WindowKey::new(0),
                            pressed: true,
                            button: sonicterm_app_core::MouseButton::Left,
                            mods: sonicterm_types::ModKey::empty(),
                            pos: sonicterm_app_core::LogicalPos { x: lx as f64, y: ly as f64 },
                        });
                    }
                    if let Some(ws) = self.main_mut() {
                        ws.mouse_down = true;
                    }
                    // Phase C2 (PR #295 review fix): re-arm the OS-drag
                    // handoff gate so the CursorMoved threshold check
                    // can fire once for the new gesture.
                    self.os_drag_handoff_started = false;
                    let cursor_pos = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                    let (px, py) = (cursor_pos.0 as f32, cursor_pos.1 as f32);
                    let window_width =
                        self.main_window().map(|w| w.inner_size().width as f32).unwrap_or(0.0);
                    let empty_tabs2 = sonicterm_ui::tabs::TabBar::new();
                    let layout = TabBarLayout::compute_with_height(
                        self.main_tabs().unwrap_or(&empty_tabs2),
                        window_width,
                        self.main_renderer()
                            .map(|r| r.tab_bar_logical_height())
                            .unwrap_or(sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT),
                    )
                    .with_top_offset(
                        self.main_renderer().map(|r| r.tab_bar_y_offset()).unwrap_or(0.0),
                    )
                    .with_visible(self.tab_bar_visible);
                    let tab_action = layout.hit(px, py);
                    if tab_action.is_some() {
                        match tab_action {
                            Some(sonicterm_ui::tabbar_view::TabHit::Activate(i)) => {
                                if let Some(t) = self.main_tabs_mut() {
                                    t.activate(i);
                                }
                                // Record the press so a subsequent drag
                                // below the tab bar can be promoted to a
                                // tear-out gesture.
                                if let Some(ws) = self.main_mut() {
                                    ws.pressed_tab = Some(i);
                                    ws.drag_session =
                                        Some(crate::tab_drag::DragSession::new(i, (px, py)));
                                }
                                // #508: tab click changed active tab → republish.
                                self.refresh_harness_sink();
                            }
                            Some(sonicterm_ui::tabbar_view::TabHit::Close(i)) => {
                                self.close_tab_at(i)
                            }
                            None => unreachable!("tab_action.is_some() checked above"),
                        }
                        if self.main_tabs().map(|t| t.is_empty()).unwrap_or(true) {
                            if self.child_window_count() == 0 {
                                if Self::should_exit_on_last_window_close(&self.config) {
                                    el.exit();
                                } else {
                                    self.hide_main_window();
                                }
                            } else {
                                self.hide_main_window();
                            }
                        }
                        if let Some(w) = self.main_window() {
                            w.request_redraw();
                        }
                        // Keep mouse_down=true when we recorded a tab
                        // press so cursor-move can promote it to a
                        // tear-out. Close hits consume the click fully.
                        if let Some(ws) = self.main_mut() {
                            if ws.pressed_tab.is_none() {
                                ws.mouse_down = false;
                            }
                        }
                        return;
                    }
                    if let Some(hit) = self.splitter_hit_at(px, py) {
                        if let Some(ws) = self.main_mut() {
                            ws.splitter_drag = Some(super::SplitterDragState {
                                splitter: hit.id,
                                axis: hit.axis,
                                last_pos: (px, py),
                            });
                            ws.selection = None;
                        }
                        self.set_splitter_cursor(hit.axis);
                        if let Some(w) = self.main_window() {
                            w.request_redraw();
                        }
                        return;
                    }
                    // B1b borrow-split: snapshot renderer geometry up front so the
                    // pane-rect compute can run alongside `self.tab_states.get_mut()`
                    // and the hyperlink path can re-borrow `self`.
                    let renderer_geom = self.main_renderer().map(|r| {
                        let (w, h) = r.logical_size();
                        (
                            w,
                            h,
                            (r.top_inset() - r.padding_top_px()).max(0.0),
                            0.0,
                            0.0,
                            r.bottom_inset(),
                            0.0,
                        )
                    });
                    let pixel_to_cell = {
                        let cp = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                        self.main_renderer().and_then(|r| r.pixel_to_cell(cp.0 as f32, cp.1 as f32))
                    };
                    // #386 PR-C: scrollbar input has priority over
                    // selection start. Done BEFORE the pane-focus switch
                    // and selection-anchor path so a thumb-drag never
                    // doubles as a text drag. `scrollbar_hit_at` returns
                    // `Miss` for any click outside the active pane's bar,
                    // including clicks on inactive panes' bars (those
                    // need a focus-switch click first — matches the
                    // behaviour of other terminals).
                    {
                        let cp = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                        let (lx, ly) = (cp.0 as f32, cp.1 as f32);
                        match self.scrollbar_hit_at(lx, ly) {
                            crate::app::scrollbar_input::HitOutcome::Miss => {}
                            crate::app::scrollbar_input::HitOutcome::StartDrag(state) => {
                                if let Some(ws) = self.main_mut() {
                                    ws.scrollbar_drag = Some(state);
                                    // Suppress the residual selection-drag
                                    // path: mouse_down stays true (so
                                    // CursorMoved routes here) but no
                                    // Selection was created.
                                }
                                if let Some(w) = self.main_window() {
                                    w.request_redraw();
                                }
                                return;
                            }
                            crate::app::scrollbar_input::HitOutcome::PageUp => {
                                self.scrollbar_track_page(false);
                                return;
                            }
                            crate::app::scrollbar_input::HitOutcome::PageDown => {
                                self.scrollbar_track_page(true);
                                return;
                            }
                        }
                    }
                    if let Some((w, h, top, pl, pr_pad, bottom, pb)) = renderer_geom {
                        let tab_idx = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
                        let pane_rects = self
                            .main_tab_states()
                            .and_then(|ts| ts.get(tab_idx))
                            .map(|st| {
                                let outer = sonicterm_ui::pane::Rect::new(
                                    pl,
                                    top,
                                    (w - pl - pr_pad).max(0.0),
                                    (h - top - bottom - pb).max(0.0),
                                );
                                st.tree.layout(outer)
                            })
                            .unwrap_or_default();
                        if pane_rects.len() > 1 {
                            let cp = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                            let (lx, ly) = (cp.0 as f32, cp.1 as f32);
                            let mut newly_focused = None;
                            for (id, rect) in &pane_rects {
                                if lx >= rect.x
                                    && lx < rect.x + rect.w
                                    && ly >= rect.y
                                    && ly < rect.y + rect.h
                                {
                                    if let Some(st) = self
                                        .main_tab_states_mut()
                                        .and_then(|ts| ts.get_mut(tab_idx))
                                    {
                                        if st.active_pane != *id {
                                            st.active_pane = *id;
                                            newly_focused = Some(*id);
                                            if let Some(panes) = self.main_panes() {
                                                mark_all_panes_dirty(panes);
                                            }
                                            // #508: pane-click focus → republish.
                                            self.refresh_harness_sink();
                                        }
                                    }
                                    break;
                                }
                            }
                            if let Some(id) = newly_focused {
                                if let Some(r) = self.main_renderer_mut() {
                                    r.flash_pane_focus(id);
                                }
                            }
                        }
                        // `pixel_to_cell` expects PHYSICAL px.
                        if let Some((row, col)) = pixel_to_cell {
                            // Modifier-click on a hyperlink opens it.
                            // On macOS the modifier is Cmd (super); on
                            // Windows / Linux it's Ctrl. The parser lock
                            // is released inside hyperlink_uri_at before
                            // we ever call sonicterm_cfg::url_open::open,
                            // so no grid lock is held across the spawn.
                            // Dispatch decision lives in the pure
                            // `dispatch_modifier_click` helper so it can
                            // be unit-tested without a real winit mouse
                            // event (see its tests in sonicterm-cfg).
                            let opened = sonicterm_cfg::url_open::dispatch_modifier_click(
                                self.url_open_modifier_held(),
                                self.hyperlink_uri_at(row, col),
                                |uri| {
                                    let r = sonicterm_cfg::url_open::open(uri);
                                    if let Err(ref e) = r {
                                        tracing::warn!("url_open failed: {e}");
                                    }
                                    r
                                },
                            );
                            if opened.is_some() {
                                if let Some(ws) = self.main_mut() {
                                    ws.mouse_down = false;
                                }
                                return;
                            }
                            self.selection_set(Some(Selection::new(row, col)));
                            if let Some(panes) = self.main_panes() {
                                mark_all_panes_dirty(panes);
                            }
                        }
                    }
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                }
                ElementState::Released => {
                    // M6a-expand-2c-mouse: notify reducer of the
                    // release transition (Render(Selection)).
                    {
                        let cp = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                        let (lx, ly) = (cp.0 as f32, cp.1 as f32);
                        self.dispatch_intent(sonicterm_app_core::AppIntent::MouseButton {
                            window: sonicterm_types::WindowKey::new(0),
                            pressed: false,
                            button: sonicterm_app_core::MouseButton::Left,
                            mods: sonicterm_types::ModKey::empty(),
                            pos: sonicterm_app_core::LogicalPos { x: lx as f64, y: ly as f64 },
                        });
                    }
                    // #386 PR-C: end any active scrollbar drag — do this
                    // unconditionally on release so a drag that ended
                    // outside the bar still clears state.
                    if let Some(ws) = self.main_mut() {
                        ws.scrollbar_drag = None;
                        ws.splitter_drag = None;
                        ws.splitter_hover = None;
                    }
                    // Commit-on-release: read the live drag session and
                    // foreign drop target, decide what to do via the
                    // pure compute_action helper, then execute.
                    let (session, foreign, pressed) = self
                        .main_mut()
                        .map(|ws| {
                            let s = ws.drag_session.take();
                            let f = ws.drag_target.take();
                            let p = ws.pressed_tab.take();
                            ws.mouse_down = false;
                            (s, f, p)
                        })
                        .unwrap_or((None, None, None));
                    if let Some(r) = self.main_renderer_mut() {
                        r.set_drag_chip(None);
                    }
                    if let (Some(s), Some(idx)) = (session, pressed) {
                        let window_width =
                            self.main_window().map(|w| w.inner_size().width as f32).unwrap_or(0.0);
                        let empty_tabs3 = sonicterm_ui::tabs::TabBar::new();
                        let layout = TabBarLayout::compute_with_height(
                            self.main_tabs().unwrap_or(&empty_tabs3),
                            window_width,
                            self.main_renderer()
                                .map(|r| r.tab_bar_logical_height())
                                .unwrap_or(sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT),
                        )
                        .with_top_offset(
                            self.main_renderer().map(|r| r.tab_bar_y_offset()).unwrap_or(0.0),
                        );
                        let action = crate::tab_drag::compute_action(&s, foreign, &layout);
                        match action {
                            crate::tab_drag::DragAction::ReturnToOriginalBar => {
                                // No-op — moving back over the source
                                // bar before releasing cancels the drag.
                            }
                            crate::tab_drag::DragAction::ReorderTab { from, to } => {
                                // #535 + #540 — must move Tab +
                                // TabState in lock-step, otherwise the
                                // title moves but `tab_states[i]`
                                // (active pane + PaneTree leaf-ids)
                                // stays bound to the old slot →
                                // title-N points at the OTHER tab's
                                // PTY. Also clamps `to` for the
                                // drag-past-last case (`TabBar::reorder`
                                // silently no-ops when `to == len`,
                                // which looked like the tab vanished).
                                // Logic lives on `WindowState::reorder_tab`
                                // so the regression tests in
                                // `tests/reorder_main_window_pane_follows_title.rs`
                                // exercise the same path production runs.
                                if let Some(id) = self.main_window_id {
                                    if let Some(ws) = self.windows.get_mut(&id) {
                                        ws.reorder_tab(from, to);
                                    }
                                }
                            }
                            crate::tab_drag::DragAction::MergeIntoWindow(target) => {
                                self.merge_main_into_child(idx, target);
                            }
                            crate::tab_drag::DragAction::TearOutToNewWindow { .. } => {
                                self.tear_out_tab(el, idx);
                            }
                        }
                        if let Some(w) = self.main_window() {
                            w.request_redraw();
                        }
                    }
                    if let Some(sel_present) =
                        self.main().map(|ws| ws.selection.as_ref().map(|s| s.is_empty()))
                    {
                        if sel_present == Some(true) {
                            self.selection_set(None);
                            if let Some(panes) = self.main_panes() {
                                mark_all_panes_dirty(panes);
                            }
                            if let Some(w) = self.main_window() {
                                w.request_redraw();
                            }
                        }
                    }
                    let cp = self.main().map(|ws| ws.cursor_pos).unwrap_or((0.0, 0.0));
                    if !self.refresh_splitter_hover(cp.0 as f32, cp.1 as f32) {
                        self.refresh_hovered_url();
                    }
                }
            },

            // -- IME (CJK / multi-key input methods) --
            WindowEvent::Ime(ime_event) => {
                let committed = if let Some(ws) = self.main_mut() {
                    match ime_event {
                        Ime::Enabled => {
                            ws.ime.handle_enabled();
                            String::new()
                        }
                        Ime::Disabled => {
                            ws.ime.handle_disabled();
                            String::new()
                        }
                        Ime::Preedit(text, cursor) => {
                            ws.ime.handle_preedit(&text, cursor);
                            String::new()
                        }
                        Ime::Commit(text) => {
                            ws.ime.handle_commit(&text);
                            ws.ime.take_commits()
                        }
                    }
                } else {
                    String::new()
                };
                if !committed.is_empty() {
                    self.write_to_pty(committed.into_bytes());
                }
                if let Some(w) = self.main_window() {
                    w.request_redraw();
                }
            }

            // -- Keyboard --
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if self.cheatsheet_open {
                    // Let the toggle binding (super+?) still close the cheat
                    // sheet; everything else routes into overlay state and is
                    // NOT forwarded to the pty.
                    if let Some(key_str) = key_event_to_string(&event, self.main_modifiers()) {
                        if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                            if matches!(action, Action::ShowKeymapCheatsheet) {
                                self.run_action_for_window(&action, win_id);
                                if let Some(w) = self.main_window() {
                                    w.request_redraw();
                                }
                                return;
                            }
                        }
                    }
                    self.cheatsheet_handle_key(&event);
                    self.drain_pending_window_creates(el);
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                    return;
                }
                if self.command_palette.is_open() {
                    // Let the toggle binding (super+shift+P) still close
                    // the palette; everything else routes into palette
                    // state and is NOT forwarded to the pty.
                    if let Some(key_str) = key_event_to_string(&event, self.main_modifiers()) {
                        if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                            if matches!(action, Action::OpenCommandPalette) {
                                self.run_action_for_window(&action, win_id);
                                if let Some(w) = self.main_window() {
                                    w.request_redraw();
                                }
                                return;
                            }
                        }
                    }
                    self.command_palette_handle_key(&event);
                    self.drain_pending_window_creates(el);
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                    return;
                }
                // While an IME composition is in flight, the OS owns the
                // keystrokes — they will be delivered to us as Ime events
                // instead. Forwarding them here would double-type. Esc
                // cancels the in-flight composition (preedit dropped, no
                // bytes sent to the PTY) instead of being forwarded.
                if self.main().map(|ws| ws.ime.is_composing()).unwrap_or(false) {
                    if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
                        if let Some(ws) = self.main_mut() {
                            ws.ime.cancel();
                        }
                        if let Some(w) = self.main_window() {
                            w.request_redraw();
                        }
                    }
                    return;
                }
                if self.main().map(|ws| ws.copy_mode.is_some()).unwrap_or(false) {
                    self.copy_mode_handle_key(&event);
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                    return;
                }
                if self.search_active() {
                    if let Some(key_str) = key_event_to_string(&event, self.main_modifiers()) {
                        if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                            if !matches!(action, Action::OpenSearch) {
                                self.run_action_for_window(&action, win_id);
                                if let Some(w) = self.main_window() {
                                    w.request_redraw();
                                }
                                return;
                            }
                        }
                    }
                    self.search_handle_key(&event, self.main_modifiers());
                    if let Some(w) = self.main_window() {
                        w.request_redraw();
                    }
                    return;
                }
                for key_str in key_to_strings(&event.logical_key, self.main_modifiers()) {
                    if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                        if self.run_action_for_window(&action, win_id) {
                            self.drain_pending_window_creates(el);
                            if let Some(w) = self.main_window() {
                                w.request_redraw();
                            }
                            return;
                        }
                    }
                }
                if let Some(bytes) = encode_key(&event, self.main_modifiers()) {
                    self.write_to_pty(bytes);
                    if self.main().map(|ws| ws.selection.is_some()).unwrap_or(false) {
                        self.selection_set(None);
                        if let Some(panes) = self.main_panes() {
                            mark_all_panes_dirty(panes);
                        }
                        if let Some(w) = self.main_window() {
                            w.request_redraw();
                        }
                    }
                }
            }

            _ => {}
        }
    }
}
impl App {
    fn copy_mode_handle_key(&mut self, event: &KeyEvent) {
        let Some(mut state) = self.main_mut().and_then(|ws| ws.copy_mode.take()) else { return };
        let mut should_copy = false;
        let mut should_exit = false;

        let active_pane_id = self.active_pane_id();
        if let Some(pane) =
            active_pane_id.and_then(|id| self.main().and_then(|ws| ws.panes.get(&id)))
        {
            let guard = pane.parser.lock();
            let grid = guard.grid();
            if let Some(quick_select) = state.quick_select.as_ref() {
                let mut copied_text = None;
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
                drop(guard);
                if let Some(text) = copied_text {
                    self.set_clipboard_text(text);
                }
                if !should_exit {
                    self.copy_mode_set(Some(state));
                }
                if let Some(panes) = self.main_panes() {
                    mark_all_panes_dirty(panes);
                }
                return;
            }
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
                if let Some(text) = copy_mode_selected_text(&state, grid) {
                    drop(guard);
                    self.set_clipboard_text(text);
                }
                should_exit = true;
            } else {
                let new_view_top = GpuRenderer::copy_mode_view_top_after_move_legacy(
                    &state,
                    grid,
                    pane.viewport_top_abs,
                );
                drop(guard);
                if let Some(id) = active_pane_id {
                    if let Some(pane) = self.main_mut().and_then(|ws| ws.panes.get_mut(&id)) {
                        pane.viewport_top_abs = new_view_top;
                    }
                }
            }
        }

        if should_exit {
            self.copy_mode_set(None);
        } else {
            self.copy_mode_set(Some(state));
        }
        if let Some(panes) = self.main_panes() {
            mark_all_panes_dirty(panes);
        }
    }
}

impl App {
    fn main_pane_outer_rect(&self) -> Option<sonicterm_ui::pane::Rect> {
        let r = self.main_renderer()?;
        let (w, h) = r.logical_size();
        let top = (r.top_inset() - r.padding_top_px()).max(0.0);
        let bottom = r.bottom_inset();
        Some(sonicterm_ui::pane::Rect::new(0.0, top, w.max(0.0), (h - top - bottom).max(0.0)))
    }

    fn splitter_hit_at(&self, x: f32, y: f32) -> Option<sonicterm_ui::pane::SplitterHit> {
        let outer = self.main_pane_outer_rect()?;
        let tab_idx = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
        self.main_tab_states()
            .and_then(|states| states.get(tab_idx))
            .and_then(|state| state.tree.hit_splitter(outer, SPLITTER_HIT_THICKNESS, x, y))
    }

    fn set_splitter_cursor(&self, axis: sonicterm_ui::pane::SplitAxis) {
        if let Some(w) = self.main_window() {
            let icon = match axis {
                sonicterm_ui::pane::SplitAxis::Vertical => CursorIcon::ColResize,
                sonicterm_ui::pane::SplitAxis::Horizontal => CursorIcon::RowResize,
            };
            w.set_cursor(icon);
        }
    }

    fn refresh_splitter_hover(&mut self, x: f32, y: f32) -> bool {
        if self.main().and_then(|ws| ws.splitter_drag.as_ref()).is_some() {
            return true;
        }
        let Some(hit) = self.splitter_hit_at(x, y) else {
            let was_splitter =
                self.main_mut().map(|ws| ws.splitter_hover.take().is_some()).unwrap_or(false);
            if was_splitter {
                if let Some(w) = self.main_window() {
                    w.set_cursor(CursorIcon::Default);
                }
            }
            return false;
        };
        if let Some(ws) = self.main_mut() {
            ws.hovered_url = None;
            ws.hover_link = false;
            ws.splitter_hover = Some(hit.axis);
        }
        self.set_splitter_cursor(hit.axis);
        true
    }

    fn apply_splitter_drag(&mut self, x: f32, y: f32) -> bool {
        let Some(drag) = self.main().and_then(|ws| ws.splitter_drag.clone()) else {
            return false;
        };
        let Some(outer) = self.main_pane_outer_rect() else {
            return false;
        };
        let dx = x - drag.last_pos.0;
        let dy = y - drag.last_pos.1;
        if dx == 0.0 && dy == 0.0 {
            return true;
        }

        let tab_idx = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
        let changed = self
            .main_tab_states_mut()
            .and_then(|states| states.get_mut(tab_idx))
            .map(|state| state.tree.resize_splitter_by_delta(&drag.splitter, outer, dx, dy))
            .unwrap_or(false);

        if changed {
            if let Some(((cell_w, cell_h), inset)) = self.main_renderer().map(|r| {
                (
                    r.cell_size(),
                    [
                        r.padding_left_px(),
                        r.padding_right_px(),
                        r.padding_top_px(),
                        r.padding_bottom_px(),
                    ],
                )
            }) {
                let rects = self
                    .main_tab_states()
                    .and_then(|states| states.get(tab_idx))
                    .map(|state| state.tree.layout(outer))
                    .unwrap_or_default();
                if let Some(panes) = self.main_panes() {
                    crate::app::resize_panes_to_rects(panes, &rects, cell_w, cell_h, inset);
                }
            }
        }

        if let Some(ws) = self.main_mut() {
            if let Some(active) = ws.splitter_drag.as_mut() {
                active.last_pos = (x, y);
            }
            if changed {
                mark_all_panes_dirty(&ws.panes);
            }
        }
        self.set_splitter_cursor(drag.axis);
        if changed {
            self.refresh_harness_sink();
            if let Some(w) = self.main_window() {
                w.request_redraw();
            }
        }
        true
    }
}

fn copy_mode_selected_text(state: &CopyModeState, grid: &Grid) -> Option<String> {
    let (start, end) = state.selected_range()?;
    if start == end {
        return None;
    }
    let mut out = String::new();
    let last_row = end.1.min(grid.scrollback_len() + grid.rows.saturating_sub(1) as usize);
    for row_idx in start.1..=last_row {
        let Some(row) = copy_mode_row(grid, row_idx) else { break };
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

fn copy_mode_row(grid: &Grid, row_idx: usize) -> Option<&sonicterm_grid::grid::Row> {
    let sb = grid.scrollback_len();
    if row_idx < sb {
        grid.scrollback_row(row_idx)
    } else {
        let live = row_idx - sb;
        (live < grid.rows as usize).then(|| grid.row(live as u16))
    }
}
