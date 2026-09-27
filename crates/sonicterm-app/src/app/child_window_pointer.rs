//! Child-window pointer handlers called from `handle_child_window_event`.

use winit::{
    dpi::PhysicalPosition, event::MouseScrollDelta, event_loop::EventLoopProxy, window::WindowId,
};

use super::child_window::scroll_child_pane;
use super::{
    pane_id_at_point, scrollbar_input::HitOutcome, App, FrontmostKind, UserEvent, WindowState,
};

impl App {
    /// Apply child notification, scrollbar, splitter and target-open chrome to a left press;
    /// returns whether the press was consumed before pane input.
    pub(super) fn handle_child_left_press_chrome(&mut self, win_id: WindowId) -> bool {
        let cursor_pos = self.windows.get(&win_id).map(|c| c.cursor_pos).unwrap_or((0.0, 0.0));
        if self.dismiss_notification_at(
            FrontmostKind::Child(win_id),
            cursor_pos.0 as f32,
            cursor_pos.1 as f32,
        ) {
            // When: `dismiss_notification_at` consumed the press, so the
            // click dismissed a toast rather than reaching the grid.
            return true;
        }
        let (px, py) = self
            .windows
            .get(&win_id)
            .map(|c| (c.cursor_pos.0 as f32, c.cursor_pos.1 as f32))
            .unwrap_or((0.0, 0.0));
        match self.scrollbar_hit_at_in_child(win_id, px, py) {
            HitOutcome::Miss => {
                // When: `HitOutcome::Miss` — the press was not on a
                // scrollbar, so it falls through to the main match.
            }
            HitOutcome::StartDrag(state) => {
                // When: `StartDrag` — the press landed on the thumb, so
                // a drag is armed and tracked until release.
                if let Some(c) = self.windows.get_mut(&win_id) {
                    c.mouse_down = true;
                    c.scrollbar_drag = Some(state);
                    c.request_redraw();
                }
                return true;
            }
            HitOutcome::PageUp => {
                // When: `PageUp` — the press hit the track above the
                // thumb, so the view pages back through scrollback.
                self.scrollbar_track_page_in_child(win_id, false);
                return true;
            }
            HitOutcome::PageDown => {
                // When: `PageDown` — the press hit the track below the
                // thumb, so the view pages toward the live bottom.
                self.scrollbar_track_page_in_child(win_id, true);
                return true;
            }
        }
        // Start a divider drag if the press landed on a pane splitter.
        if let Some(hit) = self.splitter_hit_at_in_child(win_id, px, py) {
            // When: `splitter_hit_at_in_child` reports a `hit`, so the
            // press begins a divider drag instead of a selection.
            if let Some(c) = self.windows.get_mut(&win_id) {
                c.splitter_drag = Some(super::SplitterDragState {
                    splitter: hit.id,
                    axis: hit.axis,
                    last_pos: (px, py),
                });
                c.selection = None;
                c.mouse_down = true;
                c.request_redraw();
            }
            self.set_child_splitter_cursor(win_id, hit.axis);
            return true;
        }
        // Modifier-click opens a URI or existence-authorized path in
        // the exact rendered pane. Pane identity and cell coordinates
        // come from one device-pixel-snapped renderer snapshot.
        {
            let pixel_target = self
                .windows
                .get(&win_id)
                .and_then(|child| child.renderer.as_ref())
                .and_then(|renderer| renderer.pixel_to_pane_cell(px, py));
            let geometry_pane = self.windows.get(&win_id).and_then(|child| {
                let rects = App::compute_pane_rects_for(child);
                pane_id_at_point(&rects, px, py).or_else(|| {
                    let tab_idx = child.tabs.active_index();
                    child.tab_states.get(tab_idx).map(|tab| tab.active_pane)
                })
            });
            let mods_held = self.child_url_open_modifier_held(win_id);
            if let Some((rendered_pane, row, col)) = pixel_target {
                // When: `pixel_target` identifies a rendered cell, activate only the pane identity from that same snapshot or its fallback.
                let pane_id = (rendered_pane != 0).then_some(rendered_pane).or(geometry_pane);
                let opened = pane_id.is_some_and(|pane_id| {
                    mods_held && self.activate_target_at(win_id, pane_id, row, col)
                });
                if let Some(pane_id) = pane_id.filter(|_| opened) {
                    // When: `pane_id` survives the `opened` filter, focus and flash that exact pane before consuming the click.
                    if let Some(child) = self.windows.get_mut(&win_id) {
                        child.mouse_down = false;
                        if let Some(change) = child.begin_pointer_pane_focus_change(pane_id) {
                            child.finish_pane_focus_change(change);
                        }
                    }
                    return true;
                }
            }
        }
        false
    }

    /// Apply an in-flight child splitter or scrollbar drag, or refresh child hover when no
    /// button is held; returns whether a drag consumed the move.
    pub(super) fn handle_child_cursor_moved_chrome(
        &mut self,
        win_id: WindowId,
        position: &PhysicalPosition<f64>,
    ) -> bool {
        // A splitter drag in flight resizes the divider, ahead of the
        // scrollbar and selection paths.
        let splitter_dragging =
            self.windows.get(&win_id).map(|c| c.splitter_drag.is_some()).unwrap_or(false);
        if splitter_dragging {
            // When: `splitter_dragging` — the pointer is moving a divider,
            // so the move resizes panes rather than hovering or selecting.
            let (cx, cy) = (position.x as f32, position.y as f32);
            if let Some(c) = self.windows.get_mut(&win_id) {
                c.cursor_pos = (position.x, position.y);
            }
            self.apply_splitter_drag_in_child(win_id, cx, cy);
            return true;
        }
        let dragging =
            self.windows.get(&win_id).map(|c| c.scrollbar_drag.is_some()).unwrap_or(false);
        if dragging {
            // When: `dragging` — a scrollbar thumb is held, so the move
            // scrolls that pane instead of updating hover state.
            let (cx, cy) = (position.x as f32, position.y as f32);
            if let Some(c) = self.windows.get_mut(&win_id) {
                c.cursor_pos = (position.x, position.y);
            }
            if let Some((pane_id, new_top)) = self.scrollbar_drag_apply_in_child(win_id, cx, cy) {
                let (live_top, at) = self
                    .windows
                    .get(&win_id)
                    .and_then(|c| c.panes.get(&pane_id))
                    .map(|p| {
                        let parser = p.parser.lock();
                        let grid = parser.grid();
                        let at = super::viewport_anchor::ViewportBaseline::of(grid);
                        (grid.scrollback_len() as u64, at)
                    })
                    .unwrap_or((new_top, Default::default()));
                self.set_child_pane_view_top(win_id, pane_id, new_top, live_top, at);
            }
            return true;
        }
        // Not dragging: update cursor pos + recompute the Cmd-hover URL
        // so the yellow hint / accent underline + pointer track the
        // cursor. Done here (free `self`) before the main match
        // re-borrows `child`. Mouse-down selection-drag still runs in the
        // main match below (it needs the renderer borrow).
        let mouse_down = self.windows.get(&win_id).map(|c| c.mouse_down).unwrap_or(false);
        if !mouse_down {
            if let Some(c) = self.windows.get_mut(&win_id) {
                c.cursor_pos = (position.x, position.y);
            }
            self.refresh_child_splitter_hover(win_id, position.x as f32, position.y as f32);
            self.refresh_scrollbar_hover_from_cursor_in_child(win_id);
            self.refresh_hovered_url_in_child(win_id);
        }
        false
    }

    /// Route one child-window wheel delta to the hovered pane.
    pub(super) fn handle_child_mouse_wheel(
        child: &mut WindowState,
        delta: MouseScrollDelta,
        pty_event_proxy: &Option<EventLoopProxy<UserEvent>>,
    ) {
        // The hovered pane's tracking mode takes precedence over screen-specific wheel fallbacks.
        let (lx, ly) = (child.cursor_pos.0 as f32, child.cursor_pos.1 as f32);
        let cell_h =
            child.renderer.as_ref().map(|r| r.cell_size().1).filter(|h| *h > 0.0).unwrap_or(16.0);
        let lines_per_tick: f32 = 3.0;
        let delta_lines_f: f32 = match delta {
            MouseScrollDelta::LineDelta(_x, y) => -y * lines_per_tick,
            MouseScrollDelta::PixelDelta(pos) => -(pos.y as f32) / cell_h,
        };
        let delta_lines = if delta_lines_f >= 0.0 {
            delta_lines_f.ceil() as i32
        } else {
            // When: `delta_lines_f` is negative, so rounding must go away
            // from zero to keep a small upward tick from vanishing.
            delta_lines_f.floor() as i32
        };
        if delta_lines != 0 {
            if let Some(pane_id) = child_pane_at_cursor(child, lx, ly) {
                let cell = child.renderer.as_ref().and_then(|r| r.pixel_to_cell(lx, ly));
                let (is_alt, tracking, sgr, app_cursor) = child
                    .panes
                    .get(&pane_id)
                    .map(|pane| {
                        let parser = pane.parser.lock();
                        let (tracking, sgr) = super::window_event::parser_mouse_profile(&parser);
                        (parser.grid().is_alt(), tracking, sgr, parser.application_cursor_keys())
                    })
                    .unwrap_or((false, sonicterm_vt::vt::MouseTracking::Off, false, false));
                // READONLY forbids both mouse reports and alternate-screen arrows from new wheel gestures.
                let route = if child
                    .copy_mode
                    .as_ref()
                    .is_some_and(sonicterm_ui::copy_mode::CopyModeState::is_read_only)
                {
                    super::window_event::WheelRoute::LocalScrollback
                } else {
                    // When: copy_mode is not READONLY, preserve tracking and screen-specific wheel routing.
                    super::window_event::wheel_route(tracking, is_alt)
                };
                if route == super::window_event::WheelRoute::MouseReport {
                    let up = delta_lines < 0;
                    let (col1, row1) =
                        cell.map(|(r, c)| (c as u32 + 1, r as u32 + 1)).unwrap_or((1, 1));
                    let count = delta_lines.unsigned_abs() as usize;
                    let payload =
                        super::window_event::wheel_report_bytes(sgr, up, col1, row1, count);
                    if let Some(pane) = child.panes.get_mut(&pane_id) {
                        Self::queue_pane_input(
                            pty_event_proxy.as_ref(),
                            pane,
                            pane_id,
                            super::PtyInputSource::Wheel,
                            payload,
                        );
                    }
                } else if route == super::window_event::WheelRoute::CursorKeys {
                    // When: route is CursorKeys, untracked alternate-screen motion becomes terminal arrows.
                    let up = delta_lines < 0;
                    let seq: &[u8] = match (app_cursor, up) {
                        (true, true) => b"\x1bOA",
                        (true, false) => b"\x1bOB",
                        (false, true) => b"\x1b[A",
                        (false, false) => b"\x1b[B",
                    };
                    let count = delta_lines.unsigned_abs() as usize;
                    let mut payload = Vec::with_capacity(seq.len() * count);
                    for _ in 0..count {
                        payload.extend_from_slice(seq);
                    }
                    if let Some(pane) = child.panes.get_mut(&pane_id) {
                        Self::queue_pane_input(
                            pty_event_proxy.as_ref(),
                            pane,
                            pane_id,
                            super::PtyInputSource::Wheel,
                            payload,
                        );
                    }
                } else {
                    // When: route is LocalScrollback, move the untracked primary-screen viewport.
                    scroll_child_pane(child, pane_id, delta_lines);
                }
            }
        }
    }
}

/// Pane id under logical-px `(lx, ly)` in a CHILD window's active tab, or
/// `None` outside every pane. Mirror of `App::pane_at_cursor`.
fn child_pane_at_cursor(child: &WindowState, lx: f32, ly: f32) -> Option<u64> {
    for (pane_id, rect) in App::compute_pane_rects_for(child) {
        if lx >= rect.x && lx < rect.x + rect.w && ly >= rect.y && ly < rect.y + rect.h {
            // When: `lx`/`ly` fall inside this `rect`, so this pane owns the
            // pointer and the walk stops at the first containing pane.
            return Some(pane_id);
        }
    }
    None
}
