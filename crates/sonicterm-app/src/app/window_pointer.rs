//! Main-window pointer handlers called from the `WindowEvent` dispatcher.

use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_ui::tabbar_view::TabBarLayout;
use winit::{dpi::PhysicalPosition, keyboard::ModifiersState};

use super::window_event::{
    native_scrollbar_owns_pointer, no_button_motion_report, parser_mouse_profile,
    pointer_route_bytes, pointer_scrollbar_content_rect, route_pressed_pointer_motion,
    PointerMotionRoute, PointerReportKind,
};
use super::{mark_all_panes_dirty, App, PointerCell, PtyInputSource};

impl App {
    /// Update main-window pointer interaction from a `CursorMoved` position.
    pub(super) fn handle_main_cursor_moved(&mut self, position: PhysicalPosition<f64>) {
        // CursorMoved refreshes pointer-driven overlays, drags, selection, and cross-window targets.
        let (lx, ly) = (position.x as f32, position.y as f32);
        if let Some(ws) = self.main_mut() {
            ws.cursor_pos = (position.x, position.y);
        }
        // Notify the reducer so its compatibility state tracks the
        // cursor. Its effects are discarded, so hover repaints come from
        // the app's own checks below.
        self.observe_intent(sonicterm_app_core::AppIntent::MouseMove {
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
            // When: apply_splitter_drag consumes the motion, skip tab and text drag routing.
            return;
        }
        // Update the live drag session position so the chip
        // can follow the cursor in the renderer overlay.
        let drag_snapshot = self.main_mut().and_then(|ws| {
            ws.drag_session.as_mut().map(|s| {
                s.current_pos = (lx, ly);
                *s
            })
        });
        let resolved_drag = drag_snapshot.and_then(|session| {
            self.tab_index_of_id(session.source_window, session.source_tab)
                .map(|index| (index, session))
        });
        if drag_snapshot.is_some() && resolved_drag.is_none() {
            // When: `resolved_drag` cannot find the captured tab, cancel the existing gesture before any pointer fallthrough.
            self.cancel_drag_session();
            return;
        }
        if let Some((press_idx, session_snapshot)) = resolved_drag {
            let title = self
                .main_tabs()
                .and_then(|t| t.tabs().get(press_idx).map(|tab| tab.title.clone()))
                .unwrap_or_default();
            let window_width =
                self.main_window().map(|w| w.inner_size().width as f32).unwrap_or(0.0);
            let (bar_h, top_off, visible) = self
                .main_renderer()
                .map(|r| (r.tab_bar_logical_height(), r.tab_bar_y_offset(), r.tab_bar_visible()))
                .unwrap_or((sonicterm_ui::tabbar_view::TAB_BAR_HEIGHT, 0.0, true));
            let empty_tabs = sonicterm_ui::tabs::TabBar::new();
            let layout = TabBarLayout::compute_with_height(
                self.main_tabs().unwrap_or(&empty_tabs),
                window_width,
                bar_h,
            )
            .with_top_offset(top_off)
            .with_visible(visible);
            let chip = crate::tab_drag::build_drag_chip_overlay(
                &session_snapshot,
                &layout,
                press_idx,
                title,
            );
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
            // When: mouse_down and has_press are true, update the tab drop target and OS handoff.
            let target = self.compute_main_drag_target((position.x, position.y));
            if let Some(ws) = self.main_mut() {
                ws.drag_target = target;
            }
            // start the OS-level
            // drag session AS SOON AS the cursor crosses the
            // drag-start threshold from its press point, not on
            // mouse-release. Windows `DoDragDrop` needs the live
            // button state for cursor capture. The current macOS
            // backend is pasteboard-only, but shares this trigger so
            // the payload is published once per gesture. The `os_drag_handoff_started` flag
            // ensures we only attempt the handoff once per
            // gesture; if it succeeds the backend owns the
            // gesture end-to-end (Windows) or has already
            // written the pasteboard (macOS).
            if !self.os_drag_handoff_started {
                // When: os_drag_handoff_started is false, test whether the gesture crossed the threshold.
                let started_idx = self.main().and_then(|ws| {
                    ws.drag_session
                        .as_ref()
                        .filter(|s| crate::tab_drag::drag_moved_enough(s))
                        .and_then(|s| self.tab_index_of_id(s.source_window, s.source_tab))
                });
                if let Some(idx) = started_idx {
                    // When: started_idx is Some, transfer this tab gesture to the OS backend once.
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
            // When: mouse_down is true, apply scrollbar, terminal, or text-selection drag semantics.

            let cell = self
                .main_renderer()
                .and_then(|r| r.pixel_to_pane_cell(lx, ly))
                .map(|(pane_id, row, col)| PointerCell { pane_id, row, col });
            let gesture_route = self.main_mut().and_then(|ws| {
                let modifiers = ws.modifiers;
                ws.pointer_gesture
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
            if let Some((pane_id, new_view_top)) = self.scrollbar_drag_apply(lx, ly) {
                // When: scrollbar_drag_apply returns pane_id and new_view_top, update that viewport.

                // Resolve `live_top` for the dragged pane (not
                // necessarily the active one — keep the gesture
                // pinned to the press pane even if focus shifted).
                let live_top_opt = self.main().and_then(|ws| {
                    ws.panes.get(&pane_id).and_then(|p| {
                        p.parser.try_lock().map(|parser| {
                            let g = parser.grid();
                            let at = super::viewport_anchor::ViewportBaseline::of(g);
                            (g.scrollback_len() as u64, at)
                        })
                    })
                });
                if let Some((live_top, at)) = live_top_opt {
                    // The parser snapshot clamps the dragged viewport against live output.
                    if let Some(ws) = self.main_mut() {
                        if let Some(pane) = ws.panes.get_mut(&pane_id) {
                            let top = if new_view_top >= live_top {
                                // Reaching current output resumes following the live bottom.
                                None
                            } else {
                                // When: new_view_top is below live_top, retain its absolute history position.
                                Some(new_view_top)
                            };
                            pane.set_viewport_top_at(at, top);
                        }
                        super::mark_all_panes_dirty(&ws.panes);
                        if let Some(w) = ws.window.as_ref() {
                            w.request_redraw();
                        }
                    }
                }
                // drag also counts as scrollbar activity.
                self.mark_scrollbar_active(pane_id);
                return;
            }
            // Local selection motion resolves against the press pane's rendered
            // rectangle, so crossing a split, gap, or window edge clamps into it.
            let (px, py) = (position.x as f32, position.y as f32);
            if let Some(ws) = self.main_mut() {
                if ws.extend_local_selection(px, py) {
                    mark_all_panes_dirty(&ws.panes);
                    ws.request_redraw();
                }
            }
        } else {
            // When: mouse_down is false, update SonicTerm hover owners before terminal motion.

            // Splitters own the pointer ahead of terminal target hints and modifier-authorized actions.
            let splitter_hover = self.refresh_splitter_hover(lx, ly);
            if !splitter_hover {
                self.refresh_hovered_url();
            } else if let Some(window_id) = self.main_window_id {
                // When: splitter hover owns the pointer in `window_id`, clear any terminal target beneath it.
                self.clear_target_hover(window_id);
            }
            let scrollbar_owned = self
                .pane_at_cursor(lx, ly)
                .and_then(|pane_id| {
                    let pane = self
                        .compute_active_pane_rects()
                        .into_iter()
                        .find_map(|(id, rect)| (id == pane_id).then_some(rect))?;
                    let (edge_active, visible) = self.main().map_or((false, false), |ws| {
                        ws.scrollbar_vis.get(&pane_id).map_or((false, false), |state| {
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
                        lx,
                        ly,
                        gutter_width,
                        edge_active,
                        visible,
                    ))
                })
                .unwrap_or(false);
            let target_hover =
                self.main().is_some_and(|ws| ws.hovered_url.is_some() || ws.hover_link);
            // Only unheld motion checks READONLY here; latched gestures were routed above.
            let read_only = self.main().is_some_and(|window| {
                window.copy_mode.as_ref().is_some_and(CopyModeState::is_read_only)
            });
            let ui_consumed_motion = splitter_hover || scrollbar_owned || target_hover || read_only;
            if !ui_consumed_motion {
                let pointer_cell = self
                    .main_renderer()
                    .and_then(|r| r.pixel_to_pane_cell(lx, ly))
                    .map(|(pane_id, row, col)| PointerCell { pane_id, row, col });
                let pointer_profile = pointer_cell.and_then(|cell| {
                    self.main().and_then(|ws| ws.panes.get(&cell.pane_id)).map(|pane| {
                        let parser = pane.parser.lock();
                        let (tracking, sgr) = parser_mouse_profile(&parser);
                        (cell, tracking, sgr)
                    })
                });
                if let Some((cell, tracking, sgr)) = pointer_profile {
                    // The parser snapshot ends before the bounded PTY effect path.
                    let modifiers =
                        self.main().map(|ws| ws.modifiers).unwrap_or_else(ModifiersState::empty);
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
}
