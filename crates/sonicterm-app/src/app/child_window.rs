//! Child-window event routing and redraw, plus the child close, resize, DPI,
//! scroll and pane-layout helpers. Tab and pane operations and the child PTY/VT
//! wiring live in `child_tabs`, pointer chrome and wheel routing in
//! `child_window_pointer`, and pane-divider input in `splitter_input`.

#![allow(unused_imports)]

use std::{
    collections::HashMap,
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};

use anyhow::Context;
use parking_lot::Mutex;
use sonicterm_cfg::{
    config::Config,
    keymap::{Action, Direction, Keymap, ScrollAction},
    theme::Theme,
};
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_grid::grid::Grid;
use sonicterm_io::pty::PtyHandle;
use sonicterm_ui::{
    overlays::{
        command_palette_query_caret_prefix, search_bar_label, search_query_caret_prefix,
        PaletteLayout, SearchBarLayout, PALETTE_ROW_PAD_X, SEARCH_BAR_ICON_GAP,
        SEARCH_BAR_PAD_LEFT, SEARCH_BAR_PAD_RIGHT,
    },
    pane::PaneTree,
    selection::{SelectMode, Selection},
    tabbar_view::{TabBarLayout, TabHit},
    tabs::{Tab, TabBar},
};
use sonicterm_vt::vt::Parser;
use winit::{
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::ModifiersState,
    window::{CursorIcon, Window, WindowAttributes, WindowId},
};

use super::{
    invalidate_selection_for_content, mark_all_panes_dirty, next_pane_id, pane_id_at_point,
    pick_prompt_target, poll_command_events_for_child_window, resize_all_panes,
    scrollbar_input::HitOutcome, shell_quote_posix, with_integrated_titlebar, wrap_paste, App,
    FrontmostKind, PaneState, PointerCell, PointerGestureOwner, RuntimeSmokeFailure, TabState,
    UserEvent, WindowState,
};

/// Route live child-window motion after applying every child chrome owner.
pub(super) fn child_no_button_motion_report(
    child: &WindowState,
    cell: PointerCell,
    tracking: sonicterm_vt::vt::MouseTracking,
    sgr: bool,
    scrollbar_owned: bool,
) -> Option<super::window_event::PointerMotionRoute> {
    let ui_consumed = child.splitter_hover.is_some()
        || child.hovered_url.is_some()
        || child.hover_link
        || scrollbar_owned
        || child
            .copy_mode
            .as_ref()
            .is_some_and(sonicterm_ui::copy_mode::CopyModeState::is_read_only);
    super::window_event::no_button_motion_report(cell, tracking, sgr, child.modifiers, ui_consumed)
}

/// Resize `renderer` to `width × height` and size every pane in `panes` to the
/// whole resulting cell grid, pushing the new size into each parser grid and
/// PTY winsize. Returns `true` when a renderer was present and accepted the
/// resize, so the caller knows a redraw is worth requesting.
///
/// Full-grid sizing is correct only for a single-pane tab; a split window must
/// use [`resize_renderer_and_split_panes`] so each pane keeps its own sub-rect.
#[doc(hidden)]
pub fn resize_renderer_and_panes_if_present(
    renderer: &mut Option<GpuRenderer>,
    panes: &HashMap<u64, PaneState>,
    width: u32,
    height: u32,
) -> bool {
    let Some(r) = renderer.as_mut() else {
        // When: `renderer` is absent — a headless or not-yet-initialized window
        // has no surface to size, and no pane geometry can be derived.
        return false;
    };
    if !r.try_resize(width, height) {
        // When: `try_resize` rejected `width`/`height` as unrepresentable, so
        // the old surface stands and resizing panes would desync them from it.
        return false;
    }
    let (cols, rows) = r.cells();
    for (pane_id, pane) in panes {
        pane.parser.lock().resize(cols, rows);
        pane.resize_pty(*pane_id, cols, rows);
    }
    true
}

/// Resize the child renderer to `width × height`, then size each pane to its
/// own split sub-rect via [`resize_visible_panes_in_child`] rather than to the
/// whole grid. Returns `true` if a renderer was present, so the caller can
/// request a redraw.
///
/// Sizing every pane to the full `(cols, rows)` — what
/// [`resize_renderer_and_panes_if_present`] does — makes a split overlap: each
/// pane stays full-window wide and wraps across the divider. The child
/// `Resized` handler routes here so per-split geometry survives every resize.
pub(super) fn resize_renderer_and_split_panes(
    child: &mut WindowState,
    width: u32,
    height: u32,
) -> bool {
    let Some(r) = child.renderer.as_mut() else {
        // When: this `child` has no `renderer`, so there is no surface to
        // resize and no cell metrics to lay the panes out against.
        return false;
    };
    if !r.try_resize(width, height) {
        // When: `try_resize` refused `width`/`height`, so the panes must keep
        // matching the surface that is still live.
        return false;
    }
    resize_visible_panes_in_child(child);
    true
}

/// Apply `dpi_scale` to `renderer` so glyph rasterization and overlay geometry
/// track the monitor the window now sits on. Returns `true` when a renderer was
/// present to receive the new scale, so the caller can request a redraw.
#[doc(hidden)]
pub fn apply_dpi_to_renderer_if_present(
    renderer: &mut Option<GpuRenderer>,
    dpi_scale: f64,
) -> bool {
    let Some(r) = renderer.as_mut() else {
        // When: `renderer` is absent, so there is nothing holding a scale
        // factor; the caller's recorded `dpi_scale` is applied at creation.
        return false;
    };
    r.set_scale_factor(dpi_scale as f32);
    true
}

/// Resize a child window's renderer and panes, requesting a redraw only when a
/// renderer was actually resized. Tolerates a child that has no renderer yet.
#[doc(hidden)]
pub fn child_window_resized_handles_no_renderer(child: &mut WindowState, width: u32, height: u32) {
    if resize_renderer_and_panes_if_present(&mut child.renderer, &child.panes, width, height) {
        child.request_redraw();
    }
}

/// Record `dpi_scale` on the child and push it into the renderer, requesting a
/// redraw only when a renderer took the new scale. Tolerates a child that has
/// no renderer yet, so the recorded scale still applies once one is built.
#[doc(hidden)]
pub fn child_window_dpi_changed_handles_no_renderer(child: &mut WindowState, dpi_scale: f64) {
    child.dpi_scale = dpi_scale;
    if apply_dpi_to_renderer_if_present(&mut child.renderer, dpi_scale) {
        child.request_redraw();
    }
}

impl App {
    /// Release a child through the same pane, owner, registry, renderer, and PTY boundary.
    pub(super) fn close_child_window(&mut self, win_id: WindowId) -> bool {
        let Some(mut removed) = self.windows.remove(&win_id) else {
            // When: `windows.remove(&win_id)` is `None`, no child resources remain to release.
            return false;
        };
        for pane in std::mem::take(&mut removed.panes).into_values() {
            self.retire_pane(pane);
        }
        self.release_owners_of(&mut removed);
        self.release_child_window_registries(win_id);
        drop(removed);
        true
    }

    /// Route child-local presentation and pointer events after shared source-window input dispatch.
    // Ordering: cursor_visible and the coherent keyboard_input word use Relaxed snapshots.
    pub(super) fn handle_child_window_event(
        &mut self,
        el: &ActiveEventLoop,
        win_id: WindowId,
        event: WindowEvent,
    ) {
        let theme = self.theme.clone();
        let config = self.config.clone();
        let process_privileged = self.process_privilege.is_privileged();
        // Snapshot the app-level overlay attachment before the mutable `child`
        // borrow below pins `self.windows` for the rest of the match. Used only
        // by the `RedrawRequested` arm but cheap enough to compute once.
        let palette_here = self.palette_attached_window == Some(win_id);
        let was_dirty =
            self.windows.get(&win_id).is_some_and(|window| window.redraw.input_pending());
        let pty_burst = self.windows.get(&win_id).is_some_and(WindowState::visible_output_advanced);
        let software_render_degrade = self.software_render_degrade;
        let scrollbar_motion = crate::app::scrollbar_visibility::window_scrollbar_motion(
            self.windows
                .get(&win_id)
                .and_then(|window| window.renderer.as_ref())
                .map(GpuRenderer::is_software_render_degraded),
            software_render_degrade,
        );
        if matches!(event, WindowEvent::RedrawRequested)
            && !self.begin_window_redraw(win_id, Instant::now())
        {
            // When: `begin_window_redraw` refuses `win_id`, no child parser or image collection follows.
            return;
        }
        // Scrollbar input is handled HERE, before the long-lived `child` borrow
        // below, because the scrollbar helpers take `&self`/`&mut self` and
        // would conflict with that borrow inside a match arm. A press on the
        // thumb starts a drag (track click pages); a cursor move while a drag is
        // in flight scrolls the pane. On a Miss we fall through to the normal
        // match so pane-focus / selection still work.
        match &event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // When: a left `MouseInput` press arrives, so notification,
                // scrollbar, splitter and URL hit-tests run before pane input.
                let consumed = self.handle_child_left_press_chrome(win_id);
                if consumed {
                    // When: `consumed` is true, child chrome handled the press, so the
                    // child pane match must not route it again.
                    return;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                // When: a `CursorMoved` event arrives, so an in-flight splitter or
                // scrollbar drag is applied before hover and selection work.
                let consumed = self.handle_child_cursor_moved_chrome(win_id, position);
                if consumed {
                    // When: `consumed` is true, a splitter or scrollbar drag took the move,
                    // so child hover and selection work must not also handle it.
                    return;
                }
            }
            _ => {
                // When: any other `event` needs no pre-match handling, so it
                // falls through to the main match below.
            }
        }
        let broadcast_participants = self.broadcast_participants();
        let pty_event_proxy = self.event_loop_proxy.clone();
        let Some(child) = self.windows.get_mut(&win_id) else {
            // When: `windows` no longer holds `win_id`, so the child was reaped
            // between the pre-match and here and has no state left to touch.
            return;
        };
        match event {
            WindowEvent::CloseRequested => {
                // When: `event` is `CloseRequested`, release this child's complete native and PTY state.
                let _ = child;
                self.close_child_window(win_id);
                // If this was the last child AND the main window had
                // been previously drained/hidden, nothing is alive
                // anymore — exit the loop.
                if self.should_exit() {
                    el.exit();
                }
            }
            WindowEvent::RedrawRequested => self.handle_child_redraw_requested(
                el,
                win_id,
                &theme,
                &config,
                process_privileged,
                palette_here,
                was_dirty,
                pty_burst,
                scrollbar_motion,
                &broadcast_participants,
            ),
            WindowEvent::Resized(size)
                if resize_renderer_and_split_panes(child, size.width, size.height) =>
            {
                // Cell geometry changed — force the next render to re-publish the
                // IME cursor area even if (row, col) is unchanged, else the OS
                // candidate window stays at the pre-resize pixel location.
                child.ime_cursor_throttle.reset();
                child.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor: dpi_scale, mut inner_size_writer } => {
                // When: ScaleFactorChanged arrives for a child, commit the same synchronous physical target as the main path.
                let _ = crate::app::apply_window_dpi_transition(
                    child,
                    dpi_scale,
                    &mut inner_size_writer,
                );
            }
            WindowEvent::CursorLeft { .. } => {
                // The pointer left this child, so every hover highlight it owns
                // must be dropped: URL, scrollbar and tab-bar alike.

                // Drop path authorization and all target visuals when the pointer leaves.
                child.cursor_pos = (-1.0, -1.0);
                child.invalidate_path_hover();
                if crate::app::scrollbar_visibility::clear_hover_states(&mut child.scrollbar_vis) {
                    child.request_redraw();
                }
                if let Some(r) = child.renderer.as_mut() {
                    let changed = r.set_hover_cursor(None);
                    if changed {
                        child.request_redraw();
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_child_cursor_moved(win_id, position, &config)
            }
            WindowEvent::MouseWheel { delta, .. } => {
                Self::handle_child_mouse_wheel(child, delta, &pty_event_proxy)
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                // When: a left `MouseInput` reaches the main match, so `state`
                // selects between beginning a gesture and resolving one.

                match state {
                    ElementState::Pressed => {
                        // When: the button was `Pressed`, so tab-bar hits, pane focus
                        // and a new selection anchor are resolved here.
                        let Some(r) = child.renderer.as_ref() else {
                            // When: this child has no `renderer`, so neither tab-bar
                            // layout nor cell coordinates can be computed.
                            return;
                        };
                        let (px, py) = (child.cursor_pos.0 as f32, child.cursor_pos.1 as f32);
                        let bar_width = r.width() as f32;
                        let layout = TabBarLayout::compute_with_height(
                            &child.tabs,
                            bar_width,
                            r.tab_bar_logical_height(),
                        )
                        .with_top_offset(r.tab_bar_y_offset())
                        .with_visible(r.tab_bar_visible());
                        if let Some(hit) = layout.hit(px, py) {
                            // When: `layout.hit` reports a tab-bar `hit`, so the press
                            // belongs to the bar and never reaches the grid.
                            match hit {
                                TabHit::Activate(i) => {
                                    child.tabs.activate(i);
                                    resize_visible_panes_in_child(child);
                                    child.pressed_tab = Some(i);
                                    child.mouse_down = true;
                                    child.drag_session = child.tabs.tabs().get(i).map(|tab| {
                                        crate::tab_drag::DragSession::new(win_id, tab.id, (px, py))
                                    });
                                }
                                TabHit::Overflow => {
                                    // When: `Overflow` is clicked, open the selector without starting a child tab drag.
                                    child.mouse_down = false;
                                    child.pressed_tab = None;
                                    child.drag_session = None;
                                    let _ = child;
                                    self.open_tab_selector(win_id);
                                    return;
                                }
                                TabHit::Close(idx) => {
                                    // When: `TabHit::Close` at `idx`, so the × was
                                    // clicked and that tab closes immediately.

                                    // Drop the &mut child borrow before re-entering
                                    // &mut self via helpers. `close_tab_at_in_child`
                                    // performs the reap itself.
                                    let _ = child;
                                    self.close_tab_at_in_child(win_id, idx);
                                    if let Some(c) = self.windows.get(&win_id) {
                                        c.request_redraw();
                                    }
                                    return;
                                }
                            }
                            child.request_redraw();
                            return;
                        }
                        child.mouse_down = true;
                        child.pointer_gesture = None;
                        let (px, py) = (child.cursor_pos.0 as f32, child.cursor_pos.1 as f32);
                        let pane_rects = App::compute_pane_rects_for(child);
                        // Pane and cell must come from one renderer snapshot; app
                        // geometry is only the early-render fallback for pane id 0.
                        let pixel_target = r.pixel_to_pane_cell(px, py);
                        let geometry_pane = pane_id_at_point(&pane_rects, px, py);
                        let pointer_cell = pixel_target.and_then(|(rendered_pane, row, col)| {
                            (rendered_pane != 0)
                                .then_some(rendered_pane)
                                .or(geometry_pane)
                                .map(|pane_id| PointerCell { pane_id, row, col })
                        });
                        if let Some(pointer_cell) = pointer_cell {
                            // When: `pointer_cell` resolves a rendered grid cell, route focus and press through its exact pane.
                            let PointerCell { pane_id, row, col } = pointer_cell;
                            if pane_id != 0 {
                                // When: `pane_id` is nonzero, snapshot that live pane's terminal mouse profile.
                                let (tracking, sgr) = child
                                    .panes
                                    .get(&pane_id)
                                    .map(|pane| {
                                        let parser = pane.parser.lock();
                                        super::window_event::parser_mouse_profile(&parser)
                                    })
                                    .unwrap_or((sonicterm_vt::vt::MouseTracking::Off, false));
                                let terminal_press =
                                    child.begin_pointer_press(pointer_cell, tracking, sgr);
                                if let Some(bytes) = terminal_press {
                                    // When: `terminal_press` contains bytes, the child latched terminal ownership before the unguarded enqueue.
                                    if let Some(change) =
                                        child.begin_pointer_pane_focus_change(pane_id)
                                    {
                                        child.finish_pane_focus_change(change);
                                    }
                                    let _ = child;
                                    self.write_to_pane(
                                        pane_id,
                                        bytes,
                                        super::PtyInputSource::PointerButton,
                                    );
                                    return;
                                }
                            }
                            // Multi-click selection: 1 = point, 2 = word, 3 = line, bound to
                            // the press pane from one parser snapshot like the main window. A
                            // contended snapshot binds nothing, leaving any valid selection.
                            let count = child.register_click(row, col);
                            let bound = child.begin_local_selection(pane_id, (row, col), count);
                            if bound {
                                mark_all_panes_dirty(&child.panes);
                            }
                        }
                        child.request_redraw();
                    }
                    ElementState::Released => {
                        // When: the button was `Released`, so any drag, selection or
                        // tab-move started by the press is resolved here.
                        let modifiers = child.modifiers;
                        let terminal_owned = matches!(
                            child.pointer_gesture.as_ref().map(|gesture| gesture.owner),
                            Some(PointerGestureOwner::Terminal { .. })
                        );
                        let pointer_release = super::window_event::take_pointer_release(
                            &mut child.pointer_gesture,
                            modifiers,
                        );
                        let release_report = pointer_release.and_then(|route| {
                            super::window_event::pointer_route_bytes(
                                route,
                                super::window_event::PointerReportKind::LeftRelease,
                            )
                        });
                        if terminal_owned {
                            // When: `terminal_owned` is true, consume state before bounded enqueue so rejection cannot relatch it.
                            child.mouse_down = false;
                            child.scrollbar_drag = None;
                            child.splitter_drag = None;
                            child.request_redraw();
                            let _ = child;
                            if let Some((pane_id, bytes)) = release_report {
                                self.write_to_pane(
                                    pane_id,
                                    bytes,
                                    super::PtyInputSource::PointerButton,
                                );
                            }
                            return;
                        }
                        let session = child.drag_session.take();
                        let foreign = child.drag_target.take();
                        let pressed = child.pressed_tab.take();
                        child.mouse_down = false;
                        // End any in-flight scrollbar thumb drag.
                        if child.scrollbar_drag.take().is_some() {
                            child.request_redraw();
                        }
                        // End any in-flight splitter divider drag and restore the
                        // default cursor.
                        if child.splitter_drag.take().is_some() {
                            if let Some(w) = child.window.as_ref() {
                                w.set_cursor(CursorIcon::Default);
                            }
                            child.request_redraw();
                        }
                        if let Some(r) = child.renderer.as_mut() {
                            r.set_drag_chip(None);
                        }
                        if let Some(sel) = child.selection.as_ref() {
                            if sel.is_empty() {
                                child.selection = None;
                                mark_all_panes_dirty(&child.panes);
                                child.request_redraw();
                            }
                        }
                        if let (Some(s), Some(_)) = (session, pressed) {
                            // When: `session` survived, resolve its stable tab before any release mutation.
                            let Some(src_idx) =
                                child.tabs.tabs().iter().position(|tab| tab.id == s.source_tab)
                            else {
                                // When: the captured tab has closed, cancel without substituting its former neighbor.
                                let _ = child;
                                self.cancel_drag_session();
                                return;
                            };
                            let Some(r) = child.renderer.as_ref() else {
                                // When: this child has no `renderer`, so no tab-bar
                                // layout exists to resolve the drop against.
                                return;
                            };
                            let bar_width = r.width() as f32;
                            let layout = TabBarLayout::compute_with_height(
                                &child.tabs,
                                bar_width,
                                r.tab_bar_logical_height(),
                            )
                            .with_top_offset(r.tab_bar_y_offset());
                            let action =
                                crate::tab_drag::compute_action(&s, foreign, &layout, src_idx);
                            // Release the child borrow before re-entering
                            // &mut self via the merge / tear path.
                            let _ = child;
                            self.finish_tab_drag(s, action, |app, source, index| {
                                app.tear_out_from_child(el, source, index);
                            });
                        }
                    }
                }
            }
            _ => {
                // When: any other `event` has no child-window handling, so it is
                // left to winit's defaults.
            }
        }
    }
}

/// Resize all panes in the active tab of a child window to match the
/// current pane tree layout. Mirrors `App::resize_visible_panes` for the
/// child case so split/close/zoom on a torn-out window propagate to the
/// PTY winsize the same way.
pub(super) fn resize_visible_panes_in_child(child: &mut WindowState) {
    child.complete_topology_change(
        super::TopologyChange { resize_visible: true, focus_feedback: None },
        None,
    );
}

/// Scroll a pane's scrollback view in a child window by `delta_lines`
/// (negative = back into history). Child-scoped mirror of `App::scroll_pane`.
/// Returns early on the alt screen: `App::handle_child_mouse_wheel` translates
/// alt-screen wheel input into key or mouse reports before ever calling this.
pub(super) fn scroll_child_pane(child: &mut WindowState, pane_id: u64, delta_lines: i32) {
    if delta_lines == 0 {
        // When: `delta_lines` rounded to zero, so a sub-line wheel tick moves
        // the view nowhere and nothing needs marking dirty.
        return;
    }
    let Some(pane) = child.panes.get(&pane_id) else {
        // When: `panes` no longer holds `pane_id`, so the pane closed between
        // the wheel event and this scroll.
        return;
    };
    let (live_top, current_view_top, at) = {
        let parser = pane.parser.lock();
        let grid = parser.grid();
        if grid.is_alt() {
            // When: `grid.is_alt()` — the alt screen keeps no scrollback, and
            // the wheel was already translated into reports for the child.
            return;
        }
        let live_top = grid.scrollback_len() as u64;
        let current = pane.resolved_view_top(grid);
        (live_top, current, super::viewport_anchor::ViewportBaseline::of(grid))
    };
    let new_view_top: u64 = if delta_lines < 0 {
        current_view_top.saturating_sub((-(delta_lines as i64)) as u64)
    } else {
        // When: `delta_lines` is positive, so the view walks forward and clamps
        // at `live_top` rather than running past the newest row.
        current_view_top.saturating_add(delta_lines as u64).min(live_top)
    };
    if let Some(pane) = child.panes.get_mut(&pane_id) {
        let top = if new_view_top >= live_top {
            None
        } else {
            // When: `new_view_top` stays above `live_top`, so the pane pins to
            // that scrollback row instead of following the live bottom.
            Some(new_view_top)
        };
        pane.set_viewport_top_at(at, top);
    }
    // Parity with the main window's wheel path (`scroll.rs` →
    // `mark_scrollbar_active`): a wheel scroll briefly shows the auto-hide
    // scrollbar so the user can see where they are in the scrollback. Use
    // `entry().or_insert_with` (not `get_mut`) so a scroll BEFORE the first
    // render — common right after tear-out — still lights the bar.
    let now = Instant::now();
    child
        .scrollbar_vis
        .entry(pane_id)
        .or_insert_with(|| crate::app::scrollbar_visibility::ScrollbarVisState::new(now))
        .mark_active(now);
    mark_all_panes_dirty(&child.panes);
    child.request_redraw();
}
