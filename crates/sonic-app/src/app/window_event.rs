//! `App::do_window_event` — the full `WindowEvent` dispatch body,
//! extracted from `ApplicationHandler::window_event` in refactor PR 8b.
//!
//! This is mechanically the original body wrapped in a separate `impl App`
//! block; field access works because all referenced `App` fields are
//! `pub(super)`.

use std::time::Instant;

use sonic_core::keymap::Action;
use sonic_ui::selection::Selection;
use sonic_ui::tabbar_view::{TabBarLayout, TabHit};
use winit::{
    event::{ElementState, Ime, MouseButton, WindowEvent},
    event_loop::ActiveEventLoop,
    keyboard::{Key, NamedKey},
    window::{CursorIcon, WindowId},
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
        // covers main, prefs, and child windows uniformly. PTY-byte
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
        // v0.6: route events to the preferences window if it owns this id.
        if let Some(pw) = self.prefs_window.as_ref() {
            if pw.id() == win_id {
                self.handle_prefs_event(el, event);
                return;
            }
        }
        // Tear-out child windows: route to the dedicated handler so
        // each child renders/handles input on its own surface.
        if self.child_windows.contains_key(&win_id) {
            self.handle_child_window_event(el, win_id, event);
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                // If child windows still own tabs, hide the main
                // window instead of exiting the app — the children
                // are independent live terminals and must keep
                // running. Only exit when nothing else is alive.
                if self.child_windows.is_empty() {
                    el.exit();
                } else {
                    self.hide_main_window();
                }
            }

            WindowEvent::RedrawRequested => {
                let was_dirty = self.input_dirty;
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
                if !was_dirty && self.last_render.elapsed() < self.frame_period {
                    self.pending_redraw = true;
                    return;
                }
                self.pending_redraw = false;
                // Compute per-pane rects in window pixels so the renderer can
                // draw a border around each one (and a brighter one around
                // the focused pane). The active pane's grid is rendered into
                // the full content area; per-pane Buffer rendering is v0.4.
                let tab_idx = self.tabs.active_index();
                let pane_rects: Vec<(u64, sonic_ui::pane::Rect)> = self
                    .tab_states
                    .get(tab_idx)
                    .map(|st| {
                        if let Some(r) = self.renderer.as_ref() {
                            let (w, h) = r.logical_size();
                            let top = r.top_inset();
                            let pl = r.padding_left();
                            let pr = r.padding_right();
                            let pb = r.padding_bottom();
                            let outer = sonic_ui::pane::Rect::new(
                                pl,
                                top,
                                (w - pl - pr).max(0.0),
                                (h - top - pb).max(0.0),
                            );
                            st.tree.layout(outer)
                        } else {
                            Vec::new()
                        }
                    })
                    .unwrap_or_default();
                let active_id = self.tab_states.get(tab_idx).map(|st| st.active_pane).unwrap_or(0);

                // Collect cursor positions for every INACTIVE pane in
                // the current tab so the renderer can draw a hollow
                // outline at each — wezterm-style multi-cursor split
                // affordance. The active pane's cursor is rendered
                // separately (filled or hollow depending on window
                // focus). PR #81 review.
                let inactive_cursors: Vec<sonic_shared::render::InactivePaneCursor> = pane_rects
                    .iter()
                    .filter(|(id, _)| *id != active_id)
                    .filter_map(|(id, rect)| {
                        let p = self.panes.get(id)?;
                        // CLAUDE.md §4 land-mine: render path must not block on
                        // the parser lock, or VT bursts can AB-BA deadlock the UI.
                        let g = p.parser.try_lock()?;
                        let grid = g.grid();
                        Some(sonic_shared::render::InactivePaneCursor {
                            row: grid.cursor.row,
                            col: grid.cursor.col,
                            rect: *rect,
                        })
                    })
                    .collect();
                if let Some(r) = self.renderer.as_mut() {
                    r.set_inactive_pane_cursors(inactive_cursors);
                }

                if let (Some(r), Some(pane)) =
                    (self.renderer.as_mut(), self.panes.get_mut(&active_id))
                {
                    let cursor_rc = {
                        // CLAUDE.md §4 land-mine: render path must not block on
                        // the parser lock, or VT bursts can AB-BA deadlock the UI.
                        let Some(mut grid) = pane.parser.try_lock() else {
                            return;
                        };
                        // Wezterm-style tab title: `#N icon parent/leaf`.
                        // Pull cwd from OSC 7, the foreground process from
                        // the pid probe (macOS only for now), and the OSC
                        // 0/2 title as the last-resort body (so `ssh
                        // user@host` still labels itself).
                        let cwd = grid.cwd().map(str::to_string);
                        let raw_title = grid.title().map(str::to_string);
                        let proc_name = {
                            // TTL-cache the foreground-process probe: it
                            // walks the entire macOS process table (~600
                            // procs, ~6ms) and the render path now ticks
                            // ~26×/sec while the cursor blinks. Without
                            // this cache an idle window pegs ~17% CPU
                            // (regression: `scripts/bench_headless_gui.sh`).
                            // 500ms is short enough that `nvim foo` still
                            // flips the icon promptly.
                            const TTL: std::time::Duration = std::time::Duration::from_millis(500);
                            let now = Instant::now();
                            let fresh = pane
                                .fg_proc_cache
                                .as_ref()
                                .is_some_and(|(t, _)| now.duration_since(*t) < TTL);
                            if !fresh {
                                let probed = pane
                                    .pty
                                    .as_ref()
                                    .and_then(|p| p.pid())
                                    .and_then(sonic_core::proc_info::foreground_process);
                                pane.fg_proc_cache = Some((now, probed));
                            }
                            pane.fg_proc_cache.as_ref().and_then(|(_, v)| v.clone())
                        };
                        let pretty = sonic_ui::tab_title::format_tab_title(
                            tab_idx,
                            cwd.as_deref(),
                            proc_name.as_deref(),
                            raw_title.as_deref(),
                        );
                        let cur = self.tabs.active().map(|tab| tab.title.clone());
                        if cur.as_deref() != Some(pretty.as_str()) {
                            self.tabs.set_active_title(pretty);
                        }
                        if let Some(search) =
                            self.tab_states.get_mut(tab_idx).and_then(|t| t.search.as_mut())
                        {
                            search.maybe_refresh_for_revision(grid.grid_mut());
                        }
                        let search = self.tab_states.get(tab_idx).and_then(|t| t.search.as_ref());
                        if let Err(e) = r.render(
                            grid.grid_mut(),
                            &self.theme,
                            self.cursor_visible.load(std::sync::atomic::Ordering::Relaxed),
                            self.selection.as_ref(),
                            &self.tabs,
                            &pane_rects,
                            active_id,
                            search,
                            Some(&mut self.command_palette),
                            Some(&self.ime),
                            pane.viewport_top_abs,
                        ) {
                            tracing::warn!("render error: {e}");
                        }
                        self.input_dirty = false;
                        self.last_render = Instant::now();
                        let g = grid.grid_mut();
                        (g.cursor.row, g.cursor.col)
                    };
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
            }

            WindowEvent::Focused(focused) => {
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
                    let (cols, rows) = r.cells();
                    for pane in self.panes.values() {
                        pane.parser.lock().grid_mut().resize(cols, rows);
                        if let Some(pty) = pane.pty.as_ref() {
                            (pty.resize)(cols, rows);
                        }
                    }
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
            }

            // -- Mouse --
            WindowEvent::CursorLeft { .. } => {
                let mut redraw = false;
                if let Some(r) = self.renderer.as_mut() {
                    redraw = r.set_hover_cursor(None);
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
                            (r.tab_bar_logical_height(), r.titlebar_inset(), r.tab_bar_visible())
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
                    // Hover-without-button: switch the OS cursor to a pointer
                    // when the cell under the mouse is part of a hyperlink,
                    // and reset to Default when leaving.
                    let over_link = self
                        .renderer
                        .as_ref()
                        .and_then(|r| r.pixel_to_cell(position.x as f32, position.y as f32))
                        .and_then(|(row, col)| self.hyperlink_uri_at(row, col))
                        .is_some();
                    if over_link != self.hover_link {
                        self.hover_link = over_link;
                        if let Some(w) = &self.window {
                            w.set_cursor(if over_link {
                                CursorIcon::Pointer
                            } else {
                                CursorIcon::Default
                            });
                        }
                    }
                }
            }

            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => match state {
                ElementState::Pressed => {
                    self.mouse_down = true;
                    let sf = self.scale_factor as f32;
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
                        self.renderer.as_ref().map(|r| r.titlebar_inset()).unwrap_or(0.0),
                    )
                    .with_visible(self.tab_bar_visible);
                    if let Some(hit) = layout.hit(px, py) {
                        match hit {
                            TabHit::Activate(i) => {
                                self.tabs.activate(i);
                                // Record the press so a subsequent drag
                                // below the tab bar can be promoted to a
                                // tear-out gesture.
                                self.pressed_tab = Some(i);
                                self.drag_session =
                                    Some(crate::tab_drag::DragSession::new(i, (px, py)));
                            }
                            TabHit::Close(i) => self.close_tab_at(i),
                            TabHit::NewTab => {
                                let n = self.tabs.len() + 1;
                                self.new_tab(format!("shell {n}"));
                            }
                        }
                        if self.tabs.is_empty() {
                            if self.child_windows.is_empty() {
                                el.exit();
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
                        // `pixel_to_cell` expects PHYSICAL px.
                        if let Some((row, col)) =
                            r.pixel_to_cell(self.cursor_pos.0 as f32, self.cursor_pos.1 as f32)
                        {
                            // Cmd/Super-click on a hyperlink opens it. The
                            // parser lock is released inside hyperlink_uri_at
                            // before we ever call sonic_core::url_open::open,
                            // so no grid lock is held across the spawn.
                            if self.modifiers.super_key() {
                                if let Some(uri) = self.hyperlink_uri_at(row, col) {
                                    if let Err(e) = sonic_core::url_open::open(&uri) {
                                        tracing::warn!("url_open failed: {e}");
                                    }
                                    self.mouse_down = false;
                                    return;
                                }
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
                            self.renderer.as_ref().map(|r| r.titlebar_inset()).unwrap_or(0.0),
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
