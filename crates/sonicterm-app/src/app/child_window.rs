//! Child-window event routing, plus the child close, resize, DPI, scroll and
//! pane-layout helpers. Redraw lives in `child_window_redraw`, tab and pane
//! operations and the child PTY/VT wiring in `child_tabs`, pointer handling in
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
    invalidate_selection_for_content, next_pane_id, pane_id_at_point, pick_prompt_target,
    poll_command_events_for_child_window, resize_all_panes, scrollbar_input::HitOutcome,
    shell_quote_posix, with_integrated_titlebar, wrap_paste, App, FrontmostKind, PaneState,
    PointerCell, PointerGestureOwner, RuntimeSmokeFailure, TabState, UserEvent, WindowState,
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
    let Some(renderer) = renderer.as_mut() else {
        // When: `renderer` is absent — a headless or not-yet-initialized window
        // has no surface to size, and no pane geometry can be derived.
        return false;
    };
    // Only the doc-hidden no-renderer helper reaches this; the child `Resized` handler notes its
    // outcome through `resize_renderer_and_split_panes`.
    if renderer.try_resize_outcome(width, height) == sonicterm_gpu::core::ResizeOutcome::Rejected {
        // When: `try_resize` rejected `width`/`height` as unrepresentable, so
        // the old surface stands and resizing panes would desync them from it.
        return false;
    }
    let (cols, rows) = renderer.cells();
    for (pane_id, pane) in panes {
        crate::app::frame_counters::lock_parser(&pane.parser).resize(cols, rows);
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
    let Some(renderer) = child.renderer.as_mut() else {
        // When: this `child` has no `renderer`, so there is no surface to
        // resize and no cell metrics to lay the panes out against.
        return false;
    };
    let outcome = renderer.try_resize_outcome(width, height);
    if outcome == sonicterm_gpu::core::ResizeOutcome::Rejected {
        // When: `outcome` is `Rejected` for `width`/`height`, the panes keep matching the live surface.
        return false;
    }
    child.redraw.note_resize(outcome);
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
    let Some(renderer) = renderer.as_mut() else {
        // When: `renderer` is absent, so there is nothing holding a scale
        // factor; the caller's recorded `dpi_scale` is applied at creation.
        return false;
    };
    renderer.set_scale_factor(dpi_scale as f32);
    true
}

/// Resize a child window's renderer and panes, requesting a redraw only when a
/// renderer was actually resized. Tolerates a child that has no renderer yet.
#[doc(hidden)]
pub fn child_window_resized_handles_no_renderer(child: &mut WindowState, width: u32, height: u32) {
    if resize_renderer_and_panes_if_present(&mut child.renderer, &child.panes, width, height) {
        child.request_window_redraw();
    }
}

/// Record `dpi_scale` on the child and push it into the renderer, requesting a
/// redraw only when a renderer took the new scale. Tolerates a child that has
/// no renderer yet, so the recorded scale still applies once one is built.
#[doc(hidden)]
pub fn child_window_dpi_changed_handles_no_renderer(child: &mut WindowState, dpi_scale: f64) {
    child.dpi_scale = dpi_scale;
    if apply_dpi_to_renderer_if_present(&mut child.renderer, dpi_scale) {
        child.request_window_redraw();
    }
}

impl App {
    /// Release a child through the same pane, owner, registry, renderer, and PTY boundary.
    pub(super) fn close_child_window(&mut self, win_id: WindowId) -> bool {
        let Some(mut removed) = self.windows.remove(&win_id) else {
            // When: `windows.remove(&win_id)` is `None`, no child resources remain to release.
            return false;
        };
        self.retire_window_counters(win_id, &mut removed);
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
        event_loop: &ActiveEventLoop,
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
        if matches!(event, WindowEvent::RedrawRequested) && !self.admit_window_redraw(win_id) {
            // When: `admit_window_redraw` refuses `win_id`, no child parser or image collection follows.
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
                    event_loop.exit();
                }
            }
            WindowEvent::RedrawRequested => self.handle_child_redraw_requested(
                event_loop,
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
                child.request_window_redraw();
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
                if child.clear_scrollbar_hover_states(config.appearance.scrollbar, Instant::now()) {
                    child.request_window_redraw();
                }
                if let Some(renderer) = child.renderer.as_mut() {
                    // A tab hovered before the pointer left must repaint unhovered.
                    let changed = renderer.set_hover_cursor(None, &child.tabs);
                    if changed {
                        child.request_window_redraw();
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_child_cursor_moved(win_id, position, &config)
            }
            WindowEvent::MouseWheel { delta, .. } => Self::handle_child_mouse_wheel(
                child,
                delta,
                &pty_event_proxy,
                config.appearance.scrollbar,
            ),
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                self.handle_child_left_mouse_input(event_loop, win_id, state);
                if state == ElementState::Released {
                    // A release ends any press or drag that held every bar, so a bar whose
                    // held tab widths can apply now is redrawn without waiting for more input.
                    self.redraw_held_tab_widths();
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
        super::TopologyChange {
            resize_visible: true,
            focus_feedback: None,
            dirt: super::window_state::TopologyDirt::Window,
        },
        None,
    );
}

/// Resize the active tab's panes for a pointer gesture (a splitter drag). Only the grids
/// whose size changes are dirtied, by their own resize; unchanged panes keep their rows.
pub(super) fn resize_visible_panes_in_child_for_pointer(child: &mut WindowState) {
    child.complete_topology_change(
        super::TopologyChange {
            resize_visible: true,
            focus_feedback: None,
            dirt: super::window_state::TopologyDirt::ResizeOnly,
        },
        None,
    );
}

/// Scroll a pane's scrollback view in a child window by `delta_lines`
/// (negative = back into history). Child-scoped mirror of `App::scroll_pane`.
/// Returns early on the alt screen: `App::handle_child_mouse_wheel` translates
/// alt-screen wheel input into key or mouse reports before ever calling this.
pub(super) fn scroll_child_pane(
    child: &mut WindowState,
    pane_id: u64,
    delta_lines: i32,
    mode: sonicterm_cfg::config::ScrollbarMode,
) {
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
        let parser = crate::app::frame_counters::lock_parser(&pane.parser);
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
    child.note_scrollbar_activity(pane_id, mode, Instant::now());
    // The viewport is pane identity, so the frame plan repaints it without row dirt.
    child.request_window_redraw();
}
