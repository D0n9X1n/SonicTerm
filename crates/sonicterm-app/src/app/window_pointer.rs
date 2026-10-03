//! Main-window pointer handlers called from the `WindowEvent` dispatcher.

use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_ui::tabbar_view::TabBarLayout;
use sonicterm_vt::vt::MouseTracking;
use winit::{
    dpi::PhysicalPosition,
    event::{ElementState, MouseScrollDelta},
    event_loop::ActiveEventLoop,
    keyboard::ModifiersState,
    window::{CursorIcon, WindowId},
};

use super::tab_gesture::{TabMotion, TabPress, TabRelease};
use super::window_event::{
    native_scrollbar_owns_pointer, no_button_motion_report, parser_mouse_profile,
    pointer_route_bytes, pointer_scrollbar_content_rect, route_pressed_pointer_motion,
    take_pointer_release, wheel_report_bytes, wheel_route, PointerMotionRoute, PointerReportKind,
    WheelRoute,
};
use super::{
    mark_all_panes_dirty, pane_id_at_point, App, FrontmostKind, PointerCell, PointerGestureOwner,
    PtyInputSource,
};

impl App {
    /// Clear main-window hover state when the pointer leaves the window.
    pub(super) fn handle_main_cursor_left(&mut self) {
        let mut redraw = false;
        if let Some(window) = self.main_mut() {
            if let Some(renderer) = window.renderer.as_mut() {
                // A tab hovered before the pointer left must repaint unhovered.
                redraw = renderer.set_hover_cursor(None, &window.tabs);
            }
        }
        if let Some(window) = self.main_mut() {
            window.splitter_hover = None;
            window.cursor_pos = (-1.0, -1.0);
        }
        if let Some(window_id) = self.main_window_id {
            self.clear_target_hover(window_id);
        }
        if let Some(main_window) = self.main_window() {
            main_window.set_cursor(CursorIcon::Default);
        }
        if self.clear_scrollbar_hover() {
            redraw = true;
        }
        if redraw {
            if let Some(main_window) = self.main_window() {
                crate::app::frame_counters::request_native_redraw(main_window);
            }
        }
    }

    /// The main bar as drawn, to build a held tab's drag chip against; `None` while no tab
    /// is held, so a plain pointer move lays nothing out.
    fn main_held_tab_layout(&self) -> Option<TabBarLayout> {
        if self.main().is_none_or(|window| window.drag_session.is_none()) {
            // When: the main window holds no `drag_session`, so no drag chip needs the bar's layout.
            return None;
        }
        let window_width = self
            .main_window()
            .map(|main_window| main_window.inner_size().width as f32)
            .unwrap_or(0.0);
        let (bar_h, top_off, visible) = self
            .main_renderer()
            .map(|renderer| {
                (
                    renderer.tab_bar_logical_height(),
                    renderer.tab_bar_y_offset(),
                    renderer.tab_bar_visible(),
                )
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
        Some(layout)
    }

    /// The main bar as laid out for resolving a tab drop on release.
    fn main_release_tab_layout(&self) -> TabBarLayout {
        let window_width = self
            .main_window()
            .map(|main_window| main_window.inner_size().width as f32)
            .unwrap_or(0.0);
        let empty_tabs = sonicterm_ui::tabs::TabBar::new();
        TabBarLayout::compute_with_height(
            self.main_tabs().unwrap_or(&empty_tabs),
            window_width,
            self.main_renderer()
                .map(|renderer| renderer.tab_bar_logical_height())
                .unwrap_or(sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT),
        )
        .with_top_offset(
            self.main_renderer().map(|renderer| renderer.tab_bar_y_offset()).unwrap_or(0.0),
        )
    }

    /// Update main-window pointer interaction from a `CursorMoved` position.
    pub(super) fn handle_main_cursor_moved(&mut self, position: PhysicalPosition<f64>) {
        // CursorMoved refreshes pointer-driven overlays, drags, selection, and cross-window targets.
        let (cursor_x, cursor_y) = (position.x as f32, position.y as f32);
        if let Some(window) = self.main_mut() {
            window.cursor_pos = (position.x, position.y);
        }
        // Notify the reducer so its compatibility state tracks the
        // cursor. Its effects are discarded, so hover repaints come from
        // the app's own checks below.
        self.observe_intent(sonicterm_app_core::AppIntent::MouseMove {
            window: sonicterm_types::WindowKey::new(0),
            pos: sonicterm_app_core::LogicalPos {
                pixel_x: cursor_x as f64,
                pixel_y: cursor_y as f64,
            },
        });
        let mut hover_redraw = false;
        if let Some(window) = self.main_mut() {
            if let Some(renderer) = window.renderer.as_mut() {
                hover_redraw = renderer.set_hover_cursor(Some((cursor_x, cursor_y)), &window.tabs);
            }
        }
        if hover_redraw {
            // The hovered tab changed, so the bar must repaint now; a move
            // that keeps the hovered tab draws nothing new and asks for no frame.
            if let Some(main_window) = self.main_window() {
                crate::app::frame_counters::request_native_redraw(main_window);
            }
        }
        // Auto scrollbar hover is also pure cursor state. Terminal
        // cursor moves from normal PTY output do not wake the renderer,
        // so request a frame exactly when the pointer crosses the
        // right-edge proximity threshold.
        let _ = self.refresh_scrollbar_hover_from_cursor();
        if self.apply_splitter_drag(cursor_x, cursor_y) {
            // When: apply_splitter_drag consumes the motion, skip tab and text drag routing.
            return;
        }
        // A held tab's drag session follows the pointer and draws its chip against the main
        // bar as laid out; while the button is down the drop target follows across windows.
        let held_tab_layout = self.main_held_tab_layout();
        if let Some(main_id) = self.main_window_id {
            // When: `main_window_id` names the main window, whose held tab this move may drive.
            let motion = self
                .windows
                .get_mut(&main_id)
                .map(|window| {
                    window.route_tab_motion(held_tab_layout.as_ref(), (cursor_x, cursor_y))
                })
                .unwrap_or(TabMotion::Idle);
            if self.apply_tab_motion(main_id, motion, (position.x, position.y)) {
                // When: `apply_tab_motion` took the move for the tab gesture, so no selection or pane input follows.
                return;
            }
        }
        if self.main().map(|window| window.mouse_down).unwrap_or(false) {
            // When: mouse_down is true, apply scrollbar, terminal, or text-selection drag semantics.

            let cell = self
                .main_renderer()
                .and_then(|renderer| renderer.pixel_to_pane_cell(cursor_x, cursor_y))
                .map(|(pane_id, row, col)| PointerCell { pane_id, row, col });
            let gesture_route = self.main_mut().and_then(|window| {
                let modifiers = window.modifiers;
                window
                    .pointer_gesture
                    .as_mut()
                    .map(|gesture| route_pressed_pointer_motion(gesture, cell, modifiers))
            });
            if let Some(route) = gesture_route {
                // When: `gesture_route` exists, its press-time owner wins before local continuation paths.
                match route {
                    PointerMotionRoute::Local => {
                        // When: `route` is Local, continue into SonicTerm's selection or scrollbar motion.
                    }
                    PointerMotionRoute::None => {
                        // When: `route` is None, Button mode suppresses held motion and consumes the move.
                        return;
                    }
                    report @ PointerMotionRoute::Report { .. } => {
                        // When: `route` carries Report data, encode and enqueue held-left motion.
                        if let Some((pane_id, bytes)) =
                            pointer_route_bytes(report, PointerReportKind::HeldLeftMotion)
                        {
                            self.write_to_pane(pane_id, bytes, PtyInputSource::PointerMotion);
                        }
                        return;
                    }
                }
            }

            // scrollbar drag takes priority over
            // selection extension while a thumb is held. Match
            // CLAUDE.md §4 — keep this branch fast; no parser
            // lock is needed (geometry was snapshotted at press).
            if let Some((pane_id, new_view_top)) = self.scrollbar_drag_apply(cursor_x, cursor_y) {
                // When: scrollbar_drag_apply returns pane_id and new_view_top, update that viewport.

                // Resolve `live_top` for the dragged pane (not
                // necessarily the active one — keep the gesture
                // pinned to the press pane even if focus shifted).
                let live_top_opt = self.main().and_then(|window| {
                    window.panes.get(&pane_id).and_then(|pane| {
                        pane.parser.try_lock().map(|parser| {
                            let grid = parser.grid();
                            let at = super::viewport_anchor::ViewportBaseline::of(grid);
                            (grid.scrollback_len() as u64, at)
                        })
                    })
                });
                if let Some((live_top, at)) = live_top_opt {
                    // The parser snapshot clamps the dragged viewport against live output.
                    if let Some(window) = self.main_mut() {
                        if let Some(pane) = window.panes.get_mut(&pane_id) {
                            let top = if new_view_top >= live_top {
                                // Reaching current output resumes following the live bottom.
                                None
                            } else {
                                // When: new_view_top is below live_top, retain its absolute history position.
                                Some(new_view_top)
                            };
                            pane.set_viewport_top_at(at, top);
                        }
                        super::mark_all_panes_dirty(&window.panes);
                        if let Some(native_window) = window.window.as_ref() {
                            crate::app::frame_counters::request_native_redraw(native_window);
                        }
                    }
                }
                // drag also counts as scrollbar activity.
                self.mark_scrollbar_active(pane_id);
                return;
            }
            // Local selection motion resolves against the press pane's rendered
            // rectangle, so crossing a split, gap, or window edge clamps into it.
            let (pixel_x, pixel_y) = (position.x as f32, position.y as f32);
            if let Some(window) = self.main_mut() {
                if window.extend_local_selection(pixel_x, pixel_y) {
                    mark_all_panes_dirty(&window.panes);
                    window.request_window_redraw();
                }
            }
        } else {
            // When: mouse_down is false, update SonicTerm hover owners before terminal motion.

            // Splitters own the pointer ahead of terminal target hints and modifier-authorized actions.
            let splitter_hover = self.refresh_splitter_hover(cursor_x, cursor_y);
            if !splitter_hover {
                self.refresh_hovered_url();
            } else if let Some(window_id) = self.main_window_id {
                // When: splitter hover owns the pointer in `window_id`, clear any terminal target beneath it.
                self.clear_target_hover(window_id);
            }
            let scrollbar_owned = self
                .pane_at_cursor(cursor_x, cursor_y)
                .and_then(|pane_id| {
                    let pane = self
                        .compute_active_pane_rects()
                        .into_iter()
                        .find_map(|(id, rect)| (id == pane_id).then_some(rect))?;
                    let (edge_active, visible) = self.main().map_or((false, false), |window| {
                        window.scrollbar_vis.get(&pane_id).map_or((false, false), |state| {
                            (
                                state.mouse_near_right_edge,
                                state.alpha > crate::app::scrollbar_visibility::ALPHA_EMIT_FLOOR,
                            )
                        })
                    });
                    let (content, gutter_width) = self.main_renderer().map_or(
                        (pane, crate::app::scrollbar_input::SCROLLBAR_WIDTH_PX),
                        |renderer| {
                            let content = pointer_scrollbar_content_rect(
                                pane,
                                [
                                    renderer.padding_left_px(),
                                    renderer.padding_right_px(),
                                    renderer.padding_top_px(),
                                    renderer.padding_bottom_px(),
                                ],
                                renderer.cell_size(),
                            );
                            (
                                content,
                                crate::app::scrollbar_input::SCROLLBAR_WIDTH_PX
                                    * renderer.scale_factor(),
                            )
                        },
                    );
                    Some(native_scrollbar_owns_pointer(
                        self.config.appearance.scrollbar,
                        content,
                        cursor_x,
                        cursor_y,
                        gutter_width,
                        edge_active,
                        visible,
                    ))
                })
                .unwrap_or(false);
            let target_hover =
                self.main().is_some_and(|window| window.hovered_url.is_some() || window.hover_link);
            // Only unheld motion checks READONLY here; latched gestures were routed above.
            let read_only = self.main().is_some_and(|window| {
                window.copy_mode.as_ref().is_some_and(CopyModeState::is_read_only)
            });
            let ui_consumed_motion = splitter_hover || scrollbar_owned || target_hover || read_only;
            if !ui_consumed_motion {
                let pointer_cell = self
                    .main_renderer()
                    .and_then(|renderer| renderer.pixel_to_pane_cell(cursor_x, cursor_y))
                    .map(|(pane_id, row, col)| PointerCell { pane_id, row, col });
                let pointer_profile = pointer_cell.and_then(|cell| {
                    self.main().and_then(|window| window.panes.get(&cell.pane_id)).map(|pane| {
                        let parser = crate::app::frame_counters::lock_parser(&pane.parser);
                        let (tracking, sgr) = parser_mouse_profile(&parser);
                        (cell, tracking, sgr)
                    })
                });
                if let Some((cell, tracking, sgr)) = pointer_profile {
                    // The parser snapshot ends before the bounded PTY effect path.
                    let modifiers = self
                        .main()
                        .map(|window| window.modifiers)
                        .unwrap_or_else(ModifiersState::empty);
                    if let Some(route) =
                        no_button_motion_report(cell, tracking, sgr, modifiers, false)
                    {
                        if let Some((pane_id, bytes)) =
                            pointer_route_bytes(route, PointerReportKind::NoButtonMotion)
                        {
                            self.write_to_pane(pane_id, bytes, PtyInputSource::PointerMotion);
                        }
                    }
                }
            }
        }
    }

    /// Route one main-window wheel delta to the pane under the cursor.
    pub(super) fn handle_main_mouse_wheel(&mut self, delta: MouseScrollDelta) {
        // route wheel events to the pane under the cursor.
        // Default 3 lines per LineDelta tick (matches stock GTK
        // / Cocoa wheel feel). PixelDelta divides by the live
        // cell height so trackpad scrolls match font size.
        let cursor_pos = self.main().map(|window| window.cursor_pos).unwrap_or((0.0, 0.0));
        let (cursor_x, cursor_y) = (cursor_pos.0 as f32, cursor_pos.1 as f32);
        let cell_h = self
            .main_renderer()
            .map(|renderer| renderer.cell_size().1)
            .filter(|height| *height > 0.0)
            .unwrap_or(16.0);
        let lines_per_tick: f32 = 3.0;
        let delta_lines_f: f32 = match delta {
            // winit's vertical delta is positive when scrolling UP (away from
            // user); we want negative delta_lines for "scroll
            // back into history".
            MouseScrollDelta::LineDelta(_x, vertical_lines) => -vertical_lines * lines_per_tick,
            MouseScrollDelta::PixelDelta(pos) => -(pos.y as f32) / cell_h,
        };
        // Round away from zero so a tiny trackpad nudge still
        // produces at least one line of motion.
        let delta_lines = if delta_lines_f >= 0.0 {
            // Positive wheel motion rounds upward to preserve a fractional tick.
            delta_lines_f.ceil() as i32
        } else {
            // When: delta_lines_f is negative, round downward to preserve a fractional tick.
            delta_lines_f.floor() as i32
        };
        if delta_lines != 0 {
            // Nonzero wheel motion routes to the hovered pane.
            if let Some(pane_id) = self.pane_at_cursor(cursor_x, cursor_y) {
                // Tracking owns wheel input on either screen; snapshot modes before releasing the parser lock for PTY admission.
                let cell = self
                    .main_renderer()
                    .and_then(|renderer| renderer.pixel_to_cell(cursor_x, cursor_y));
                let (is_alt, tracking, sgr, app_cursor) = self
                    .main()
                    .and_then(|window| window.panes.get(&pane_id))
                    .map(|pane| {
                        let parser = crate::app::frame_counters::lock_parser(&pane.parser);
                        let is_alt = parser.grid().is_alt();
                        let (tracking, sgr) = parser_mouse_profile(&parser);
                        let app_cursor = parser.application_cursor_keys();
                        (is_alt, tracking, sgr, app_cursor)
                    })
                    .unwrap_or((false, MouseTracking::Off, false, false));
                // READONLY keeps wheel input local even when tracking or an alternate screen requests bytes.
                let route = if self.admits_new_user_input(pane_id) {
                    wheel_route(tracking, is_alt)
                } else {
                    // When: admits_new_user_input rejects this pane, wheel cannot produce reports or cursor keys.
                    WheelRoute::LocalScrollback
                };
                if route == WheelRoute::MouseReport {
                    // MouseReport routes negotiated tracking to the PTY before screen-specific fallbacks.
                    // App wants mouse events: emit one wheel report per
                    // line of motion at the cell under the cursor.
                    let up = delta_lines < 0;
                    let (col1, row1) =
                        cell.map(|(row, col)| (col as u32 + 1, row as u32 + 1)).unwrap_or((1, 1));
                    let count = delta_lines.unsigned_abs() as usize;
                    let payload = wheel_report_bytes(sgr, up, col1, row1, count);
                    self.write_to_pane(pane_id, payload, PtyInputSource::Wheel);
                } else if route == WheelRoute::CursorKeys {
                    // When: route is CursorKeys, untracked alternate-screen wheel motion becomes arrows.

                    // Build the arrow sequence: ESC O A/B in
                    // application-cursor-keys mode, else ESC [ A/B.
                    // Up when scrolling back into history
                    // (delta_lines < 0), down otherwise. Emit one
                    // copy per line of motion.
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
                    self.write_to_pane(pane_id, payload, PtyInputSource::Wheel);
                } else {
                    // When: route is LocalScrollback, move the untracked primary-screen viewport.
                    self.scroll_pane(pane_id, delta_lines);
                }
            }
        }
    }

    /// Route a main-window left-button press or release to primary-pointer interaction.
    pub(super) fn handle_main_left_mouse_input(
        &mut self,
        event_loop: &ActiveEventLoop,
        win_id: WindowId,
        state: ElementState,
    ) {
        match state {
            ElementState::Pressed => {
                // When: state is ElementState::Pressed, begin primary-pointer interaction.

                // Notify the reducer of the press transition so selection
                // observability emits Render(Selection).
                {
                    let cursor = self.main().map(|window| window.cursor_pos).unwrap_or((0.0, 0.0));
                    let (cursor_x, cursor_y) = (cursor.0 as f32, cursor.1 as f32);
                    self.observe_intent(sonicterm_app_core::AppIntent::MouseButton {
                        window: sonicterm_types::WindowKey::new(0),
                        pressed: true,
                        button: sonicterm_app_core::MouseButton::Left,
                        mods: sonicterm_types::ModKey::empty(),
                        pos: sonicterm_app_core::LogicalPos {
                            pixel_x: cursor_x as f64,
                            pixel_y: cursor_y as f64,
                        },
                    });
                }
                let cursor_pos = self.main().map(|window| window.cursor_pos).unwrap_or((0.0, 0.0));
                if self.dismiss_notification_at(
                    FrontmostKind::Main,
                    cursor_pos.0 as f32,
                    cursor_pos.1 as f32,
                ) {
                    // When: dismiss_notification_at returns true, consume the press before terminal interaction.
                    return;
                }
                if let Some(window) = self.main_mut() {
                    window.mouse_down = true;
                    window.pointer_gesture = None;
                }
                // re-arm the OS-drag
                // handoff gate so the CursorMoved threshold check
                // can fire once for the new gesture.
                self.os_drag_handoff_started = false;
                let cursor_pos = self.main().map(|window| window.cursor_pos).unwrap_or((0.0, 0.0));
                let (pixel_x, pixel_y) = (cursor_pos.0 as f32, cursor_pos.1 as f32);
                let window_width = self
                    .main_window()
                    .map(|main_window| main_window.inner_size().width as f32)
                    .unwrap_or(0.0);
                let empty_tabs2 = sonicterm_ui::tabs::TabBar::new();
                let layout = TabBarLayout::compute_with_height(
                    self.main_tabs().unwrap_or(&empty_tabs2),
                    window_width,
                    self.main_renderer()
                        .map(|renderer| renderer.tab_bar_logical_height())
                        .unwrap_or(sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT),
                )
                .with_top_offset(
                    self.main_renderer().map(|renderer| renderer.tab_bar_y_offset()).unwrap_or(0.0),
                )
                .with_visible(self.tab_bar_visible);
                let tab_press = self
                    .main_mut()
                    .map(|window| window.route_tab_press(win_id, &layout, (pixel_x, pixel_y)))
                    .unwrap_or(TabPress::Miss);
                if tab_press != TabPress::Miss {
                    // When: `tab_press` hit the bar, select, close or open the selector before pane input.
                    self.apply_tab_press(win_id, tab_press);
                    if tab_press == TabPress::OpenSelector {
                        // When: `tab_press` opened the selector, so no tab press is recorded and no pane input follows.
                        return;
                    }
                    if self.main_tabs().map(|tab_bar| tab_bar.is_empty()).unwrap_or(true) {
                        // Empty main_tabs hides main and exits only if no child terminal survives.
                        if self.child_window_count() == 0 {
                            self.hide_main_window();
                            event_loop.exit();
                        } else {
                            // When: child_window_count is nonzero, keep the app alive after hiding main.
                            self.hide_main_window();
                        }
                    }
                    if let Some(main_window) = self.main_window() {
                        crate::app::frame_counters::request_native_redraw(main_window);
                    }
                    // Keep mouse_down=true when we recorded a tab
                    // press so cursor-move can promote it to a
                    // tear-out. Close hits consume the click fully.
                    if let Some(window) = self.main_mut() {
                        if window.pressed_tab.is_none() {
                            window.mouse_down = false;
                        }
                    }
                    return;
                }
                if let Some(hit) = self.splitter_hit_at(pixel_x, pixel_y) {
                    // When: splitter_hit_at returns hit, capture a resize gesture instead of selecting text.
                    if let Some(window) = self.main_mut() {
                        window.splitter_drag = Some(super::SplitterDragState {
                            splitter: hit.id,
                            axis: hit.axis,
                            last_pos: (pixel_x, pixel_y),
                        });
                        window.selection = None;
                    }
                    self.set_splitter_cursor(hit.axis);
                    if let Some(main_window) = self.main_window() {
                        crate::app::frame_counters::request_native_redraw(main_window);
                    }
                    return;
                }
                // B1b borrow-split: snapshot renderer geometry up front so the
                // pane-rect compute can run alongside `self.tab_states.get_mut()`
                // and the hyperlink path can re-borrow `self`.
                let renderer_geom = self.main_renderer().map(|renderer| {
                    let (width, height) = renderer.logical_size();
                    (
                        width,
                        height,
                        (renderer.top_inset() - renderer.padding_top_px()).max(0.0),
                        0.0,
                        0.0,
                        renderer.bottom_inset(),
                        0.0,
                    )
                });
                let pixel_target = {
                    let cursor = self.main().map(|window| window.cursor_pos).unwrap_or((0.0, 0.0));
                    self.main_renderer().and_then(|renderer| {
                        renderer.pixel_to_pane_cell(cursor.0 as f32, cursor.1 as f32)
                    })
                };
                // scrollbar input has priority over
                // selection start. Done BEFORE the pane-focus switch
                // and selection-anchor path so a thumb-drag never
                // doubles as a text drag. `scrollbar_hit_at` returns
                // `Miss` for any click outside the active pane's bar,
                // including clicks on inactive panes' bars (those
                // need a focus-switch click first — matches the
                // behaviour of other terminals).
                {
                    let cursor = self.main().map(|window| window.cursor_pos).unwrap_or((0.0, 0.0));
                    let (cursor_x, cursor_y) = (cursor.0 as f32, cursor.1 as f32);
                    match self.scrollbar_hit_at(cursor_x, cursor_y) {
                        crate::app::scrollbar_input::HitOutcome::Miss => {
                            // When: HitOutcome::Miss leaves the press for pane selection routing.
                        }
                        crate::app::scrollbar_input::HitOutcome::StartDrag(state) => {
                            // When: HitOutcome::StartDrag carries state, capture the scrollbar drag.
                            let mode = self.config.appearance.scrollbar;
                            if let Some(window) = self.main_mut() {
                                window.begin_scrollbar_drag(state, mode, std::time::Instant::now());
                                // Suppress the residual selection-drag
                                // path: mouse_down stays true (so
                                // CursorMoved routes here) but no
                                // Selection was created.
                            }
                            if let Some(main_window) = self.main_window() {
                                crate::app::frame_counters::request_native_redraw(main_window);
                            }
                            return;
                        }
                        crate::app::scrollbar_input::HitOutcome::PageUp => {
                            // When: HitOutcome::PageUp pages the track toward older scrollback.
                            self.scrollbar_track_page(false);
                            return;
                        }
                        crate::app::scrollbar_input::HitOutcome::PageDown => {
                            // When: HitOutcome::PageDown pages the track toward live output.
                            self.scrollbar_track_page(true);
                            return;
                        }
                    }
                }
                if let Some((
                    width,
                    height,
                    top,
                    padding_left,
                    padding_right,
                    bottom,
                    padding_bottom,
                )) = renderer_geom
                {
                    // When: renderer_geom is Some, derive pane hit regions for focus and selection.
                    let tab_idx =
                        self.main_tabs().map(|tab_bar| tab_bar.active_index()).unwrap_or(0);
                    let pane_rects = self
                        .main_tab_states()
                        .and_then(|tab_states| tab_states.get(tab_idx))
                        .map(|tab_state| {
                            let outer = sonicterm_ui::pane::Rect::new(
                                padding_left,
                                top,
                                (width - padding_left - padding_right).max(0.0),
                                (height - top - bottom - padding_bottom).max(0.0),
                            );
                            tab_state.tree.layout(outer)
                        })
                        .unwrap_or_default();
                    let cursor = self.main().map(|window| window.cursor_pos).unwrap_or((0.0, 0.0));
                    let geometry_pane =
                        pane_id_at_point(&pane_rects, cursor.0 as f32, cursor.1 as f32);
                    if pixel_target.is_none() && pane_rects.len() > 1 {
                        // Padding clicks may focus without attempting a local selection.
                        if let (Some(target), Some(window)) = (geometry_pane, self.main_mut()) {
                            if let Some(change) = window.begin_pointer_pane_focus_change(target) {
                                window.finish_pane_focus_change(change);
                            }
                        }
                    }
                    let clicked_pane = pixel_target
                        .and_then(|(pane_id, _, _)| (pane_id != 0).then_some(pane_id))
                        .or(geometry_pane);
                    if let Some((_, row, col)) = pixel_target {
                        // When: `pixel_target` identifies a rendered cell, pair its coordinates with the same-snapshot pane before activation.
                        let opened = self.main_window_id.zip(clicked_pane).is_some_and(
                            |(window_id, pane_id)| {
                                self.activate_target_at(window_id, pane_id, row, col)
                            },
                        );
                        if opened {
                            // When: `opened` is true, consume the target click after presenting any focus change for the clicked pane.
                            if let Some(window) = self.main_mut() {
                                window.mouse_down = false;
                                if let Some(change) = clicked_pane.and_then(|pane_id| {
                                    window.begin_pointer_pane_focus_change(pane_id)
                                }) {
                                    window.finish_pane_focus_change(change);
                                }
                            }
                            return;
                        }
                        let pointer_cell =
                            clicked_pane.map(|pane_id| PointerCell { pane_id, row, col });
                        if let Some(cell) = pointer_cell {
                            // When: `pointer_cell` resolves the rendered grid, snapshot that exact pane's protocol profile.
                            let profile = self
                                .main()
                                .and_then(|window| window.panes.get(&cell.pane_id))
                                .map(|pane| {
                                    let parser =
                                        crate::app::frame_counters::lock_parser(&pane.parser);
                                    parser_mouse_profile(&parser)
                                })
                                .unwrap_or((MouseTracking::Off, false));
                            let terminal_press = self.main_mut().and_then(|window| {
                                window.begin_pointer_press(cell, profile.0, profile.1)
                            });
                            if let Some(bytes) = terminal_press {
                                // When: `terminal_press` contains bytes, the window latched terminal ownership before the unguarded enqueue.
                                self.write_to_pane(
                                    cell.pane_id,
                                    bytes,
                                    PtyInputSource::PointerButton,
                                );
                                if let Some(window) = self.main_mut() {
                                    if let Some(change) =
                                        window.begin_pointer_pane_focus_change(cell.pane_id)
                                    {
                                        window.finish_pane_focus_change(change);
                                    }
                                }
                                return;
                            }
                        }
                        // Multi-click selection: 1 = point, 2 = word,
                        // 3 = line. Record the click against the main
                        // window's streak state, then bind it below.
                        let click_count = self
                            .main_mut()
                            .map(|window| window.register_click(row, col))
                            .unwrap_or(1);
                        // Bind the press to its pane from one parser snapshot. A
                        // contended snapshot binds nothing, leaving a valid selection.
                        let bound = clicked_pane.is_some_and(|pane_id| {
                            self.main_mut().is_some_and(|window| {
                                window.begin_local_selection(pane_id, (row, col), click_count)
                            })
                        });
                        if bound {
                            if let Some(panes) = self.main_panes() {
                                mark_all_panes_dirty(panes);
                            }
                        }
                    }
                }
                if let Some(main_window) = self.main_window() {
                    crate::app::frame_counters::request_native_redraw(main_window);
                }
            }
            ElementState::Released => {
                // When: `state` is Released, commit or cancel the press-latched pointer gesture.

                // Notify the reducer of the release transition so selection
                // observability emits Render(Selection).
                {
                    let cursor = self.main().map(|window| window.cursor_pos).unwrap_or((0.0, 0.0));
                    let (cursor_x, cursor_y) = (cursor.0 as f32, cursor.1 as f32);
                    self.observe_intent(sonicterm_app_core::AppIntent::MouseButton {
                        window: sonicterm_types::WindowKey::new(0),
                        pressed: false,
                        button: sonicterm_app_core::MouseButton::Left,
                        mods: sonicterm_types::ModKey::empty(),
                        pos: sonicterm_app_core::LogicalPos {
                            pixel_x: cursor_x as f64,
                            pixel_y: cursor_y as f64,
                        },
                    });
                }
                let (terminal_owned, pointer_release) = self
                    .main_mut()
                    .map(|window| {
                        let terminal_owned = matches!(
                            window.pointer_gesture.as_ref().map(|gesture| gesture.owner),
                            Some(PointerGestureOwner::Terminal { .. })
                        );
                        let modifiers = window.modifiers;
                        let release = take_pointer_release(&mut window.pointer_gesture, modifiers);
                        (terminal_owned, release)
                    })
                    .unwrap_or((false, None));
                // Clear before enqueue: saturation or disconnect must not
                // leave the completed gesture latched for a later event.
                if let Some(route) = pointer_release {
                    if let Some((pane_id, bytes)) =
                        pointer_route_bytes(route, PointerReportKind::LeftRelease)
                    {
                        self.write_to_pane(pane_id, bytes, PtyInputSource::PointerButton);
                    }
                }
                if terminal_owned {
                    // When: `terminal_owned` is true, release skips selection, tab-drag, and chrome cleanup.
                    if let Some(window) = self.main_mut() {
                        window.mouse_down = false;
                    }
                    return;
                }
                // end any active scrollbar drag — do this
                // unconditionally on release so a drag that ended
                // outside the bar still clears state.
                let mode = self.config.appearance.scrollbar;
                if let Some(window) = self.main_mut() {
                    window.end_scrollbar_drag(mode, std::time::Instant::now());
                    window.splitter_drag = None;
                    window.splitter_hover = None;
                }
                // Commit-on-release: route the release against the bar as laid out, which ends
                // the gesture and decides the drop, then carry it out.
                let release_layout = self.main_release_tab_layout();
                let release = self
                    .main_mut()
                    .map(|window| window.route_tab_release(Some(&release_layout)))
                    .unwrap_or(TabRelease::Idle);
                if let Some(renderer) = self.main_renderer_mut() {
                    renderer.set_drag_chip(None);
                }
                match release {
                    TabRelease::Idle => {
                        // When: `release` is Idle, no tab was pressed and dragged, so nothing moves.
                    }
                    TabRelease::Cancel => {
                        // When: `release` is Cancel, the captured tab closed and the release must not move another tab.
                        self.cancel_drag_session();
                        return;
                    }
                    TabRelease::Finish(..) => {
                        self.apply_tab_release(release, |app, _, index| {
                            app.tear_out_tab(event_loop, index);
                        });
                        if let Some(main_window) = self.main_window() {
                            crate::app::frame_counters::request_native_redraw(main_window);
                        }
                    }
                }
                if let Some(sel_present) = self
                    .main()
                    .map(|window| window.selection.as_ref().map(|selection| selection.is_empty()))
                {
                    // Main selection presence distinguishes no selection from an empty range.
                    if sel_present == Some(true) {
                        // An empty completed selection is cleared instead of rendered.
                        self.selection_set(None);
                        if let Some(panes) = self.main_panes() {
                            mark_all_panes_dirty(panes);
                        }
                        if let Some(main_window) = self.main_window() {
                            crate::app::frame_counters::request_native_redraw(main_window);
                        }
                    }
                }
                let cursor = self.main().map(|window| window.cursor_pos).unwrap_or((0.0, 0.0));
                if !self.refresh_splitter_hover(cursor.0 as f32, cursor.1 as f32) {
                    self.refresh_hovered_url();
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "window_pointer_tests.rs"]
mod window_pointer_tests;
