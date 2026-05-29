//! `App::do_window_event` — the full `WindowEvent` dispatch body,
//! extracted from `ApplicationHandler::window_event` in refactor PR 8b.
//!
//! This is mechanically the original body wrapped in a separate `impl App`
//! block; field access works because all referenced `App` fields are
//! `pub(super)`.

use std::sync::atomic::Ordering;
use std::time::Instant;

use sonic_core::{grid::Grid, keymap::Action};
use sonic_shared::render::GpuRenderer;
use sonic_ui::copy_mode::CopyModeState;
use sonic_ui::selection::Selection;
use sonic_ui::tabbar_view::TabBarLayout;
use winit::{
    event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent},
    event_loop::ActiveEventLoop,
    keyboard::{Key, NamedKey},
    window::WindowId,
};

use super::key_encoding::{encode_key, key_event_to_string};
use super::{mark_all_panes_dirty, to_logical_pos, App};

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
        // Drain any pending sonic.toml live-reload deliveries before
        // dispatching the event — guarantees font/theme/keymap swaps
        // land on the same redraw tick they were detected on.
        self.poll_config_reload();
        // Tear-out child windows: route to the dedicated handler so
        // each child renders/handles input on its own surface.
        if self.windows.contains_key(&win_id) {
            self.handle_child_window_event(el, win_id, event);
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                // If child windows still own tabs, hide the main
                // window instead of exiting the app — the children
                // are independent live terminals and must keep
                // running. Only exit when nothing else is alive.
                if self.windows.is_empty() {
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
                if !was_dirty && !pty_burst && self.last_render.elapsed() < self.frame_period {
                    self.pending_redraw = true;
                    return;
                }
                self.pending_redraw = false;
                self.tabs.clear_expired_command_badges(Instant::now());
                self.poll_command_events_for_all_tabs();
                let tab_idx = self.tabs.active_index();
                // Compute per-pane rects in window pixels so the renderer can
                // draw a border around each one (and a brighter one around
                // the focused pane). The active pane's grid is rendered into
                // the full content area; per-pane Buffer rendering is v0.4.
                let pane_rects: Vec<(u64, sonic_ui::pane::Rect)> = self
                    .tab_states
                    .get(tab_idx)
                    .map(|st| {
                        if let Some(r) = self.renderer.as_ref() {
                            let (w, h) = r.logical_size();
                            let top = r.top_inset();
                            let pl = r.padding_left();
                            let pr = r.padding_right();
                            let bottom = r.bottom_inset();
                            let pb = r.padding_bottom();
                            let outer = sonic_ui::pane::Rect::new(
                                pl,
                                top,
                                (w - pl - pr).max(0.0),
                                (h - top - bottom - pb).max(0.0),
                            );
                            st.tree.layout(outer)
                        } else {
                            Vec::new()
                        }
                    })
                    .unwrap_or_default();
                let active_id = self.tab_states.get(tab_idx).map(|st| st.active_pane).unwrap_or(0);
                let broadcast_receivers = self.broadcast_receivers();

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
                let parser_arcs: Vec<(
                    u64,
                    std::sync::Arc<parking_lot::Mutex<sonic_core::vt::Parser>>,
                    sonic_ui::pane::Rect,
                )> = pane_rects
                    .iter()
                    .filter_map(|(id, rect)| {
                        self.panes.get(id).map(|p| (*id, std::sync::Arc::clone(&p.parser), *rect))
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
                            // SAFETY: extending the guard's lifetime to
                            // the outer scope. `arc` lives in `parser_arcs`
                            // which is dropped strictly after `guards`, so
                            // the underlying Mutex outlives every guard.
                            // parking_lot guards carry a `*const Mutex`
                            // internally and no `'a` tied to `arc`.
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
                    self.defer_redraw_on_lock_contention(was_dirty);
                    return;
                }

                // Inactive-pane cursor outlines come from the same guard set
                // (no extra lock pass — PR #81's separate try_lock loop is
                // subsumed by the global pass above).
                let inactive_cursors: Vec<sonic_shared::render::InactivePaneCursor> = guards
                    .iter()
                    .filter(|(id, _, _)| *id != active_id)
                    .map(|(_, g, rect)| {
                        let grid = g.grid();
                        sonic_shared::render::InactivePaneCursor {
                            row: grid.cursor.row,
                            col: grid.cursor.col,
                            rect: *rect,
                        }
                    })
                    .collect();
                if let Some(r) = self.renderer.as_mut() {
                    r.set_inactive_pane_cursors(inactive_cursors);
                }

                let cheatsheet_render = (self.cheatsheet_open
                    && self.cheatsheet_attached_window.is_none())
                .then(|| (self.cheatsheet.clone(), self.cheatsheet_bindings()));
                if let (Some(r), Some(pane)) =
                    (self.renderer.as_mut(), self.panes.get_mut(&active_id))
                {
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
                            &mut self.tabs,
                            pane,
                            &guards[active_pos].1,
                            tab_idx,
                        );
                        if let Some(search) =
                            self.tab_states.get_mut(tab_idx).and_then(|t| t.search.as_mut())
                        {
                            search.maybe_refresh_for_revision(guards[active_pos].1.grid_mut());
                        }
                        let search = self.tab_states.get(tab_idx).and_then(|t| t.search.as_ref());
                        // PR #199 Fix 1: build the slice from ALL panes
                        // (was previously a single-element slice for the
                        // active pane only). The renderer's per-pane loop
                        // now actually iterates every pane in production
                        // frames, so split panes paint.
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
                                is_broadcast_receiver: broadcast_receivers.contains(id),
                            })
                            .collect();
                        if let Err(e) = r.render(
                            &mut panes_slice,
                            &self.theme,
                            self.cursor_visible.load(std::sync::atomic::Ordering::Relaxed),
                            self.selection.as_ref(),
                            self.copy_mode.as_ref(),
                            &self.tabs,
                            search,
                            // Epic #289 follow-up: only feed the palette
                            // to the main window when it's actually
                            // attached here (None == main). Otherwise the
                            // palette would paint on the wrong window.
                            self.palette_attached_window
                                .is_none()
                                .then_some(&mut self.command_palette),
                            cheatsheet_render,
                            Some(&self.ime),
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
                        self.last_render = Instant::now();
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
                    if let Some(w) = &self.window {
                        if self.ime_cursor_throttle.should_update(cursor_rc.0, cursor_rc.1) {
                            let x = r.padding() + f32::from(cursor_rc.1) * r.cell_w;
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
                // Epic #289 Phase C2 — refresh OS-drag tab bar snapshot
                // for the main window. Outside the renderer borrow scope
                // so the immutable self borrow doesn't conflict with `r`.
                self.publish_main_window_tab_bar();
            }

            WindowEvent::Focused(focused) => {
                // Main window gained focus → clear `focused_child` so
                // menubar-routed actions (NewTab, …) target the main
                // App again. See `App::focused_child` doc for context.
                if focused {
                    self.focused_child = None;
                    // Epic #289 Phase A — record the main window as
                    // OS-frontmost so keymap_dispatch / menubar drain
                    // route subsequent Cmd+T / Cmd+W / Cmd+\\ to the
                    // main window's tabs vec.
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
                self.ime.cancel();
                // Propagate window focus to the renderer so the active
                // pane's block cursor flips between filled (focused) and
                // hollow (unfocused) — wezterm/iTerm2 parity. PR #81
                // review.
                if let Some(r) = self.renderer.as_mut() {
                    r.set_window_focused(focused);
                }
                // Focus transition flips the cursor between filled and
                // hollow — that's a presentation change only, so mark
                // every pane dirty without bumping grid revision.
                mark_all_panes_dirty(&self.panes);
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
                if let Some(w) = &self.window {
                    // Intentionally do NOT toggle `set_ime_allowed` on
                    // focus transitions. macOS' IMK posts a runloop
                    // wake message on every toggle; doing it on every
                    // focus in/out (which Sonic also receives when the
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
                        self.ime_cursor_throttle.reset();
                    }
                    w.request_redraw();
                }
            }

            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(size.width, size.height);
                }
                // Per-pane sizing: each pane's grid + PTY is resized to
                // its own PaneRect within the (new) window content area,
                // not the whole window's (cols, rows). Pre-fix, inactive
                // panes thought they were full-window-wide and TUIs drew
                // past their visible border. See docs/specs/per-pane-grids.md.
                let rects = self.compute_active_pane_rects();
                if let Some(r) = self.renderer.as_ref() {
                    let (cw, ch) = r.cell_size();
                    crate::app::resize_panes_to_rects(&self.panes, &rects, cw, ch);
                }
                // Cell geometry changed — force the next render to
                // re-publish the IME cursor area even if (row, col) is
                // unchanged, otherwise the OS candidate window stays
                // pinned to the pre-resize pixel location.
                self.ime_cursor_throttle.reset();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale_factor = scale_factor;
                if let Some(r) = self.renderer.as_mut() {
                    r.set_scale_factor(scale_factor as f32);
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            WindowEvent::ModifiersChanged(m) => {
                self.modifiers = m.state();
                // Releasing the open-URL modifier must clear any
                // visible Cmd+hover URL underline (and revert the
                // pointer to default if it was previously shown). We
                // recompute hover state from the last cursor position
                // so a subsequent re-press while still hovering brings
                // the affordance back without needing a CursorMoved.
                self.refresh_hovered_url();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            // -- Mouse --
            WindowEvent::CursorLeft { .. } => {
                let mut redraw = false;
                if let Some(r) = self.renderer.as_mut() {
                    redraw = r.set_hover_cursor(None);
                }
                if self.hovered_url.take().is_some() {
                    redraw = true;
                }
                if redraw {
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = (position.x, position.y);
                let sf = self.scale_factor as f32;
                let (lx, ly) = to_logical_pos(position.x, position.y, sf);
                let mut hover_redraw = false;
                if let Some(r) = self.renderer.as_mut() {
                    hover_redraw = r.set_hover_cursor(Some((lx, ly)));
                }
                if hover_redraw {
                    // A bare hover-move over the tab bar must repaint —
                    // otherwise the muted × → bright × transition lags
                    // until the next unrelated event.
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
                // Update the live drag session position so the chip
                // can follow the cursor in the renderer overlay.
                if let Some(s) = self.drag_session.as_mut() {
                    s.current_pos = (lx, ly);
                    let title = self
                        .tabs
                        .tabs()
                        .get(s.press_tab_index)
                        .map(|t| t.title.clone())
                        .unwrap_or_default();
                    let session_snapshot = *s;
                    let window_width = self
                        .window
                        .as_ref()
                        .map(|w| w.inner_size().to_logical::<f32>(w.scale_factor()).width)
                        .unwrap_or(0.0);
                    let (bar_h, top_off, visible) = self
                        .renderer
                        .as_ref()
                        .map(|r| {
                            (r.tab_bar_logical_height(), r.tab_bar_y_offset(), r.tab_bar_visible())
                        })
                        .unwrap_or((sonic_ui::tabbar_view::TAB_BAR_HEIGHT, 0.0, true));
                    let layout = TabBarLayout::compute_with_height(&self.tabs, window_width, bar_h)
                        .with_top_offset(top_off)
                        .with_visible(visible);
                    let chip =
                        crate::tab_drag::build_drag_chip_overlay(&session_snapshot, &layout, title);
                    if let Some(r) = self.renderer.as_mut() {
                        r.set_drag_chip(chip);
                    }
                }
                // Cross-window drag-merge: if a tab is held, update the
                // pending drop target based on the global cursor
                // position. The actual decision (tear / merge / cancel)
                // is deferred to mouse-up via `compute_action`.
                if self.mouse_down && self.pressed_tab.is_some() {
                    self.drag_target = self.compute_main_drag_target((position.x, position.y));
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
                        if let Some(session) = self.drag_session.as_ref() {
                            if crate::tab_drag::drag_moved_enough(session) {
                                let idx = session.press_tab_index;
                                self.os_drag_handoff_started = true;
                                let _ = self.try_os_drag_handoff(idx);
                            }
                        }
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
                if self.mouse_down {
                    if let Some(r) = self.renderer.as_ref() {
                        if let Some((row, col)) =
                            r.pixel_to_cell(position.x as f32, position.y as f32)
                        {
                            if let Some(sel) = self.selection.as_mut() {
                                sel.extend(row, col);
                                mark_all_panes_dirty(&self.panes);
                                if let Some(w) = &self.window {
                                    w.request_redraw();
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
                    self.refresh_hovered_url();
                }
            }

            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => match state {
                ElementState::Pressed => {
                    self.mouse_down = true;
                    // Phase C2 (PR #295 review fix): re-arm the OS-drag
                    // handoff gate so the CursorMoved threshold check
                    // can fire once for the new gesture.
                    self.os_drag_handoff_started = false;
                    let sf = self
                        .window
                        .as_ref()
                        .map(|w| w.scale_factor() as f32)
                        .unwrap_or(self.scale_factor as f32);
                    let (px, py) = to_logical_pos(self.cursor_pos.0, self.cursor_pos.1, sf);
                    let window_width = self
                        .window
                        .as_ref()
                        .map(|w| w.inner_size().to_logical::<f32>(w.scale_factor()).width)
                        .unwrap_or(0.0);
                    let layout = TabBarLayout::compute_with_height(
                        &self.tabs,
                        window_width,
                        self.renderer
                            .as_ref()
                            .map(|r| r.tab_bar_logical_height())
                            .unwrap_or(sonic_ui::tabbar_view::TAB_BAR_HEIGHT),
                    )
                    .with_top_offset(
                        self.renderer.as_ref().map(|r| r.tab_bar_y_offset()).unwrap_or(0.0),
                    )
                    .with_visible(self.tab_bar_visible);
                    let tab_action = layout.hit(px, py);
                    if tab_action.is_some() {
                        match tab_action {
                            Some(sonic_ui::tabbar_view::TabHit::Activate(i)) => {
                                self.tabs.activate(i);
                                // Record the press so a subsequent drag
                                // below the tab bar can be promoted to a
                                // tear-out gesture.
                                self.pressed_tab = Some(i);
                                self.drag_session =
                                    Some(crate::tab_drag::DragSession::new(i, (px, py)));
                            }
                            Some(sonic_ui::tabbar_view::TabHit::Close(i)) => self.close_tab_at(i),
                            Some(sonic_ui::tabbar_view::TabHit::NewTab) => {
                                tracing::trace!(
                                    coords = ?(px, py),
                                    "new_tab_button hit at {:?}, dispatching",
                                    (px, py)
                                );
                                self.run_action(&Action::NewTab);
                            }
                            None => unreachable!("tab_action.is_some() checked above"),
                        }
                        if self.tabs.is_empty() {
                            if self.windows.is_empty() {
                                if Self::should_exit_on_last_window_close(&self.config) {
                                    el.exit();
                                } else {
                                    self.hide_main_window();
                                }
                            } else {
                                self.hide_main_window();
                            }
                        }
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                        // Keep mouse_down=true when we recorded a tab
                        // press so cursor-move can promote it to a
                        // tear-out. Other hits (Close, NewTab) consume
                        // the click fully.
                        if self.pressed_tab.is_none() {
                            self.mouse_down = false;
                        }
                        return;
                    }
                    if let Some(r) = self.renderer.as_ref() {
                        // Click-to-focus: figure out which pane rect
                        // contains the click and make it the active
                        // pane. Without this, splitting a pane spawned
                        // a working PTY that the user could never
                        // type into — there was no way to move focus
                        // off the originally-active pane other than
                        // the (undiscoverable) keyboard shortcuts.
                        // User report v0.6: "split window 这个功能是
                        // 坏的，没有能够形成两个可以输入的 windows".
                        let tab_idx = self.tabs.active_index();
                        let pane_rects = self
                            .tab_states
                            .get(tab_idx)
                            .map(|st| {
                                let (w, h) = r.logical_size();
                                let top = r.top_inset();
                                let pl = r.padding_left();
                                let pr = r.padding_right();
                                let bottom = r.bottom_inset();
                                let pb = r.padding_bottom();
                                let outer = sonic_ui::pane::Rect::new(
                                    pl,
                                    top,
                                    (w - pl - pr).max(0.0),
                                    (h - top - bottom - pb).max(0.0),
                                );
                                st.tree.layout(outer)
                            })
                            .unwrap_or_default();
                        if pane_rects.len() > 1 {
                            let sf = self.scale_factor as f32;
                            let (lx, ly) = to_logical_pos(self.cursor_pos.0, self.cursor_pos.1, sf);
                            for (id, rect) in &pane_rects {
                                if lx >= rect.x
                                    && lx < rect.x + rect.w
                                    && ly >= rect.y
                                    && ly < rect.y + rect.h
                                {
                                    if let Some(st) = self.tab_states.get_mut(tab_idx) {
                                        if st.active_pane != *id {
                                            st.active_pane = *id;
                                            mark_all_panes_dirty(&self.panes);
                                        }
                                    }
                                    break;
                                }
                            }
                        }
                        // `pixel_to_cell` expects PHYSICAL px.
                        if let Some((row, col)) =
                            r.pixel_to_cell(self.cursor_pos.0 as f32, self.cursor_pos.1 as f32)
                        {
                            // Modifier-click on a hyperlink opens it.
                            // On macOS the modifier is Cmd (super); on
                            // Windows / Linux it's Ctrl. The parser lock
                            // is released inside hyperlink_uri_at before
                            // we ever call sonic_core::url_open::open,
                            // so no grid lock is held across the spawn.
                            // Dispatch decision lives in the pure
                            // `dispatch_modifier_click` helper so it can
                            // be unit-tested without a real winit mouse
                            // event (see its tests in sonic-cfg).
                            let opened = sonic_core::url_open::dispatch_modifier_click(
                                self.url_open_modifier_held(),
                                self.hyperlink_uri_at(row, col),
                                |uri| {
                                    let r = sonic_core::url_open::open(uri);
                                    if let Err(ref e) = r {
                                        tracing::warn!("url_open failed: {e}");
                                    }
                                    r
                                },
                            );
                            if opened.is_some() {
                                self.mouse_down = false;
                                return;
                            }
                            self.selection = Some(Selection::new(row, col));
                            mark_all_panes_dirty(&self.panes);
                        }
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
                ElementState::Released => {
                    // Commit-on-release: read the live drag session and
                    // foreign drop target, decide what to do via the
                    // pure compute_action helper, then execute.
                    let session = self.drag_session.take();
                    let foreign = self.drag_target.take();
                    let pressed = self.pressed_tab.take();
                    self.mouse_down = false;
                    if let Some(r) = self.renderer.as_mut() {
                        r.set_drag_chip(None);
                    }
                    if let (Some(s), Some(idx)) = (session, pressed) {
                        let window_width = self
                            .window
                            .as_ref()
                            .map(|w| w.inner_size().to_logical::<f32>(w.scale_factor()).width)
                            .unwrap_or(0.0);
                        let layout = TabBarLayout::compute_with_height(
                            &self.tabs,
                            window_width,
                            self.renderer
                                .as_ref()
                                .map(|r| r.tab_bar_logical_height())
                                .unwrap_or(sonic_ui::tabbar_view::TAB_BAR_HEIGHT),
                        )
                        .with_top_offset(
                            self.renderer.as_ref().map(|r| r.tab_bar_y_offset()).unwrap_or(0.0),
                        );
                        let action = crate::tab_drag::compute_action(&s, foreign, &layout);
                        match action {
                            crate::tab_drag::DragAction::ReturnToOriginalBar => {
                                // No-op — moving back over the source
                                // bar before releasing cancels the drag.
                            }
                            crate::tab_drag::DragAction::ReorderTab { from, to } => {
                                self.tabs.reorder(from, to);
                            }
                            crate::tab_drag::DragAction::MergeIntoWindow(target) => {
                                self.merge_main_into_child(idx, target);
                            }
                            crate::tab_drag::DragAction::TearOutToNewWindow { .. } => {
                                self.tear_out_tab(el, idx);
                            }
                        }
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                    if let Some(sel) = self.selection.as_ref() {
                        if sel.is_empty() {
                            self.selection = None;
                            mark_all_panes_dirty(&self.panes);
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                        }
                    }
                }
            },

            // -- IME (CJK / multi-key input methods) --
            WindowEvent::Ime(ime_event) => {
                match ime_event {
                    Ime::Enabled => self.ime.handle_enabled(),
                    Ime::Disabled => self.ime.handle_disabled(),
                    Ime::Preedit(text, cursor) => {
                        self.ime.handle_preedit(&text, cursor);
                    }
                    Ime::Commit(text) => {
                        self.ime.handle_commit(&text);
                        let committed = self.ime.take_commits();
                        if !committed.is_empty() {
                            self.write_to_pty(committed.into_bytes());
                        }
                    }
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }

            // -- Keyboard --
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if self.cheatsheet_open {
                    // Let the toggle binding (super+?) still close the cheat
                    // sheet; everything else routes into overlay state and is
                    // NOT forwarded to the pty.
                    if let Some(key_str) = key_event_to_string(&event, self.modifiers) {
                        if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                            if matches!(action, Action::ShowKeymapCheatsheet) {
                                self.run_action(&action);
                                if let Some(w) = &self.window {
                                    w.request_redraw();
                                }
                                return;
                            }
                        }
                    }
                    self.cheatsheet_handle_key(&event);
                    self.drain_pending_window_creates(el);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
                if self.command_palette.is_open() {
                    // Let the toggle binding (super+shift+P) still close
                    // the palette; everything else routes into palette
                    // state and is NOT forwarded to the pty.
                    if let Some(key_str) = key_event_to_string(&event, self.modifiers) {
                        if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                            if matches!(action, Action::OpenCommandPalette) {
                                self.run_action(&action);
                                if let Some(w) = &self.window {
                                    w.request_redraw();
                                }
                                return;
                            }
                        }
                    }
                    self.command_palette_handle_key(&event);
                    self.drain_pending_window_creates(el);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
                // While an IME composition is in flight, the OS owns the
                // keystrokes — they will be delivered to us as Ime events
                // instead. Forwarding them here would double-type. Esc
                // cancels the in-flight composition (preedit dropped, no
                // bytes sent to the PTY) instead of being forwarded.
                if self.ime.is_composing() {
                    if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
                        self.ime.cancel();
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                    return;
                }
                if self.copy_mode.is_some() {
                    self.copy_mode_handle_key(&event);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
                if self.search_active() {
                    if let Some(key_str) = key_event_to_string(&event, self.modifiers) {
                        if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                            if !matches!(action, Action::OpenSearch) {
                                self.run_action(&action);
                                if let Some(w) = &self.window {
                                    w.request_redraw();
                                }
                                return;
                            }
                        }
                    }
                    self.search_handle_key(&event, self.modifiers);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                    return;
                }
                if let Some(key_str) = key_event_to_string(&event, self.modifiers) {
                    if let Some(action) = self.keymap.lookup(&key_str).cloned() {
                        if self.run_action(&action) {
                            self.drain_pending_window_creates(el);
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                            return;
                        }
                    }
                }
                if let Some(bytes) = encode_key(&event, self.modifiers) {
                    self.write_to_pty(bytes);
                    if self.selection.is_some() {
                        self.selection = None;
                        mark_all_panes_dirty(&self.panes);
                        if let Some(w) = &self.window {
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
        let Some(mut state) = self.copy_mode.take() else { return };
        let mut should_copy = false;
        let mut should_exit = false;

        let active_pane_id = self.active_pane_id();
        if let Some(pane) = active_pane_id.and_then(|id| self.panes.get(&id)) {
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
                    self.copy_mode = Some(state);
                }
                mark_all_panes_dirty(&self.panes);
                return;
            }
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => should_exit = true,
                Key::Named(NamedKey::Enter) => should_copy = true,
                Key::Named(NamedKey::ArrowLeft) => state.move_left(grid),
                Key::Named(NamedKey::ArrowRight) => state.move_right(grid),
                Key::Named(NamedKey::ArrowUp) => state.move_up(grid),
                Key::Named(NamedKey::ArrowDown) => state.move_down(grid),
                Key::Character(s) if s.eq_ignore_ascii_case("h") => state.move_left(grid),
                Key::Character(s) if s.eq_ignore_ascii_case("j") => state.move_down(grid),
                Key::Character(s) if s.eq_ignore_ascii_case("k") => state.move_up(grid),
                Key::Character(s) if s.eq_ignore_ascii_case("l") => state.move_right(grid),
                Key::Character(s) if s == "v" => state.start_select(),
                Key::Character(s) if s == "y" => should_copy = true,
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
                let new_view_top =
                    GpuRenderer::copy_mode_view_top_after_move(&state, grid, pane.viewport_top_abs);
                drop(guard);
                if let Some(id) = active_pane_id {
                    if let Some(pane) = self.panes.get_mut(&id) {
                        pane.viewport_top_abs = new_view_top;
                    }
                }
            }
        }

        if should_exit {
            self.copy_mode = None;
        } else {
            self.copy_mode = Some(state);
        }
        mark_all_panes_dirty(&self.panes);
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
        for cell in &row[col_start.min(row.len())..col_end] {
            if cell.flags.contains(sonic_core::grid::CellFlags::WIDE_CONT) {
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

fn copy_mode_row(grid: &Grid, row_idx: usize) -> Option<&sonic_core::grid::Row> {
    let sb = grid.scrollback_len();
    if row_idx < sb {
        grid.scrollback_row(row_idx)
    } else {
        let live = row_idx - sb;
        (live < grid.rows as usize).then(|| grid.row(live as u16))
    }
}
