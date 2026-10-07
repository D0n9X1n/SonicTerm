//! `App::do_window_event`: the `WindowEvent` dispatcher, the main-window redraw
//! handler, and the pointer, wheel, key-repeat and quit-chord helpers both window
//! roles share. Keyboard, IME and focus routing live in `window_keyboard`,
//! main-window pointer handlers in `window_pointer`, and pane-divider input in
//! `splitter_input`.

use std::time::Instant;

use sonicterm_cfg::{config::ScrollbarMode, keymap::Action};
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_ui::selection::Selection;
use sonicterm_vt::vt::MouseTracking;
use winit::{
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::ActiveEventLoop,
    keyboard::{ModifiersState, PhysicalKey},
    window::WindowId,
};

use super::{
    invalidate_selection_for_content,
    runtime_smoke::{grid_contains_marker, RuntimeSmokeFailure},
    App, PointerCell, PointerGesture, PointerGestureOwner, TabState, WindowState,
};

/// Pointer event encoded for a terminal mouse protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PointerReportKind {
    LeftPress,
    LeftRelease,
    HeldLeftMotion,
    NoButtonMotion,
}

/// Pure terminal pointer route produced after ownership and cell resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PointerMotionRoute {
    Local,
    None,
    Report { pane_id: u64, sgr: bool, row: u16, col: u16, modifiers: ModifiersState },
}

/// Encode one pane-local pointer report in SGR or classic X10 form.
pub(super) fn pointer_report_bytes(
    sgr: bool,
    kind: PointerReportKind,
    modifiers: ModifiersState,
    row: u16,
    col: u16,
) -> Vec<u8> {
    let mut modifier_bits = 0;
    if modifiers.shift_key() {
        modifier_bits += 4;
    }
    if modifiers.alt_key() || modifiers.super_key() {
        modifier_bits += 8;
    }
    if modifiers.control_key() {
        modifier_bits += 16;
    }
    let base: u32 = match kind {
        PointerReportKind::LeftPress | PointerReportKind::LeftRelease => 0,
        PointerReportKind::HeldLeftMotion => 32,
        PointerReportKind::NoButtonMotion => 35,
    };
    let button_code = base + modifier_bits;
    let col = u32::from(col) + 1;
    let row = u32::from(row) + 1;
    if sgr {
        let terminator = match kind {
            PointerReportKind::LeftRelease => 'm',
            PointerReportKind::LeftPress
            | PointerReportKind::HeldLeftMotion
            | PointerReportKind::NoButtonMotion => 'M',
        };
        format!("\x1b[<{button_code};{col};{row}{terminator}").into_bytes()
    } else {
        // When: `sgr` is false, legacy release uses no-button base 3 while other events retain `button_code`.
        let legacy_button_code = match kind {
            PointerReportKind::LeftRelease => 3 + modifier_bits,
            PointerReportKind::LeftPress
            | PointerReportKind::HeldLeftMotion
            | PointerReportKind::NoButtonMotion => button_code,
        };
        vec![
            0x1b,
            b'[',
            b'M',
            (legacy_button_code + 32).min(255) as u8,
            (col.min(223) + 32) as u8,
            (row.min(223) + 32) as u8,
        ]
    }
}

/// Return an existing terminal press's destinations only for repeat events.
pub(super) fn terminal_repeat_targets(
    pressed_keys: &super::keyboard_protocol::PressedKeys,
    physical_key: PhysicalKey,
    repeat: bool,
) -> Option<super::keyboard_protocol::KeyRoutes> {
    repeat.then(|| pressed_keys.get(&physical_key).cloned()).flatten()
}

/// Choose and latch the owner of a left-button grid press.
pub(super) fn begin_pointer_gesture(
    cell: PointerCell,
    tracking: MouseTracking,
    sgr: bool,
    modifiers: ModifiersState,
    ui_consumed: bool,
) -> Option<PointerGesture> {
    if ui_consumed {
        // When: `ui_consumed` is true, SonicTerm chrome consumed the press and no grid gesture exists.
        return None;
    }
    let owner = match (tracking, modifiers.shift_key()) {
        (MouseTracking::Off, _) | (_, true) => PointerGestureOwner::Local,
        (MouseTracking::Button | MouseTracking::ButtonMotion | MouseTracking::AnyMotion, false) => {
            PointerGestureOwner::Terminal { tracking, sgr }
        }
    };
    Some(PointerGesture { owner, press_pane: cell.pane_id, last_cell: cell, anchor: None })
}

impl WindowState {
    /// Begin one grid press from this window's live modifier state.
    pub(super) fn begin_pointer_press(
        &mut self,
        cell: PointerCell,
        tracking: MouseTracking,
        sgr: bool,
    ) -> Option<Vec<u8>> {
        let gesture = if self.copy_mode.as_ref().is_some_and(CopyModeState::is_read_only) {
            // READONLY selects local ownership only at the press; an earlier terminal hold keeps its route.
            Some(PointerGesture {
                owner: PointerGestureOwner::Local,
                press_pane: cell.pane_id,
                last_cell: cell,
                anchor: None,
            })
        } else {
            // When: copy_mode is not READONLY, retain the normal Shift and tracking ownership decision.
            begin_pointer_gesture(cell, tracking, sgr, self.modifiers, false)
        };
        self.pointer_gesture = gesture;
        let PointerGestureOwner::Terminal { sgr, .. } = gesture?.owner else {
            // When: `gesture.owner` is Local, preserve the selection and emit no terminal report.
            return None;
        };
        self.selection = None;
        Some(pointer_report_bytes(
            sgr,
            PointerReportKind::LeftPress,
            self.modifiers,
            cell.row,
            cell.col,
        ))
    }
}

/// Route motion for an already-latched left-button gesture.
pub(super) fn route_pressed_pointer_motion(
    gesture: &mut PointerGesture,
    cell: Option<PointerCell>,
    modifiers: ModifiersState,
) -> PointerMotionRoute {
    let PointerGestureOwner::Terminal { tracking, sgr } = gesture.owner else {
        // When: local selection owns the gesture, current parser/modifier state cannot steal it.
        return PointerMotionRoute::Local;
    };
    if let Some(cell) = cell.filter(|cell| cell.pane_id == gesture.press_pane) {
        gesture.last_cell = cell;
    }
    if tracking == MouseTracking::Button {
        // When: Button mode reports transitions only, suppress held motion.
        return PointerMotionRoute::None;
    }
    PointerMotionRoute::Report {
        pane_id: gesture.press_pane,
        sgr,
        row: gesture.last_cell.row,
        col: gesture.last_cell.col,
        modifiers,
    }
}

/// Decide whether the visible native scrollbar owns a pointer in its gutter.
pub(super) fn native_scrollbar_owns_pointer(
    mode: ScrollbarMode,
    pane: sonicterm_ui::pane::Rect,
    pixel_x: f32,
    pixel_y: f32,
    gutter_width: f32,
    edge_active: bool,
    visible: bool,
) -> bool {
    let interactive = match mode {
        ScrollbarMode::Always => true,
        ScrollbarMode::Auto => edge_active || visible,
        ScrollbarMode::Never => false,
    };
    if !interactive {
        // When: no scrollbar is interactive, terminal motion keeps the pane's whole cell surface.
        return false;
    }
    let width = gutter_width.max(0.0).min(pane.w.max(0.0));
    pixel_x >= pane.x + pane.w - width
        && pixel_x < pane.x + pane.w
        && pixel_y >= pane.y
        && pixel_y < pane.y + pane.h
}

/// Inset a pane rect to the content area that owns the rendered scrollbar.
pub(super) fn pointer_scrollbar_content_rect(
    pane: sonicterm_ui::pane::Rect,
    insets: [f32; 4],
    minimum: (f32, f32),
) -> sonicterm_ui::pane::Rect {
    sonicterm_ui::pane::Rect::new(
        pane.x + insets[0],
        pane.y + insets[2],
        (pane.w - insets[0] - insets[1]).max(minimum.0),
        (pane.h - insets[2] - insets[3]).max(minimum.1),
    )
}

/// Build live no-button motion only for the current pane's AnyMotion mode.
pub(super) fn no_button_motion_report(
    cell: PointerCell,
    tracking: MouseTracking,
    sgr: bool,
    modifiers: ModifiersState,
    ui_consumed: bool,
) -> Option<PointerMotionRoute> {
    (!ui_consumed && tracking == MouseTracking::AnyMotion).then_some(PointerMotionRoute::Report {
        pane_id: cell.pane_id,
        sgr,
        row: cell.row,
        col: cell.col,
        modifiers,
    })
}

/// Consume a left-button gesture and return its terminal release route, if any.
pub(super) fn take_pointer_release(
    gesture: &mut Option<PointerGesture>,
    modifiers: ModifiersState,
) -> Option<PointerMotionRoute> {
    let gesture = gesture.take()?;
    let PointerGestureOwner::Terminal { sgr, .. } = gesture.owner else {
        // When: local selection owns the gesture, release stays entirely local.
        return None;
    };
    Some(PointerMotionRoute::Report {
        pane_id: gesture.press_pane,
        sgr,
        row: gesture.last_cell.row,
        col: gesture.last_cell.col,
        modifiers,
    })
}

/// Consume a gesture interrupted by focus loss and release terminal ownership.
pub(super) fn take_focus_loss_pointer_release(
    gesture: &mut Option<PointerGesture>,
    modifiers: ModifiersState,
) -> Option<PointerMotionRoute> {
    take_pointer_release(gesture, modifiers)
}

/// Clear a gesture whose release can no longer arrive.
pub(super) fn cancel_pointer_gesture(gesture: &mut Option<PointerGesture>) -> bool {
    gesture.take().is_some()
}

/// Convert a pure pointer route into one pane-targeted protocol payload.
pub(super) fn pointer_route_bytes(
    route: PointerMotionRoute,
    kind: PointerReportKind,
) -> Option<(u64, Vec<u8>)> {
    let PointerMotionRoute::Report { pane_id, sgr, row, col, modifiers } = route else {
        // When: `route` is Local or None, ownership or mode suppressed the terminal report.
        return None;
    };
    Some((pane_id, pointer_report_bytes(sgr, kind, modifiers, row, col)))
}

/// The pane and cell under a point from pane rectangles on a uniform `cell_size` grid.
///
/// Test-only: it stands in for `GpuRenderer::pixel_to_pane_cell` when a headless test supplies a
/// pane viewport and no renderer, so the real pointer handlers can run without a window.
#[cfg(test)]
pub(super) fn headless_pane_cell(
    rects: &[(u64, sonicterm_ui::pane::Rect)],
    cell_size: (f32, f32),
    cursor_x: f32,
    cursor_y: f32,
) -> Option<(u64, u16, u16)> {
    let (pane_id, rect) = rects.iter().find(|(_, rect)| {
        cursor_x >= rect.x
            && cursor_x < rect.x + rect.w
            && cursor_y >= rect.y
            && cursor_y < rect.y + rect.h
    })?;
    let row = ((cursor_y - rect.y) / cell_size.1) as u16;
    let col = ((cursor_x - rect.x) / cell_size.0) as u16;
    Some((*pane_id, row, col))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WheelRoute {
    MouseReport,
    CursorKeys,
    LocalScrollback,
}

/// Select terminal wheel reports or the screen's untracked fallback.
pub(super) fn wheel_route(tracking: MouseTracking, is_alt: bool) -> WheelRoute {
    if tracking != MouseTracking::Off {
        WheelRoute::MouseReport
    } else if is_alt {
        // When: is_alt is true without tracking, pagers receive terminal cursor keys instead of local history motion.
        WheelRoute::CursorKeys
    } else {
        // When: is_alt is false and tracking is Off, only the terminal's local primary-screen scrollback moves.
        WheelRoute::LocalScrollback
    }
}

/// Encode `count` mouse-wheel reports for an app that has mouse tracking on.
/// Wheel buttons per xterm: 64 = up, 65 = down (press only, no release).
/// `sgr` true → SGR encoding `ESC[<Btn;col;row M` (1-based, unbounded).
/// `sgr` false → legacy X10 `ESC[M` + 3 bytes (button+32, col+32, row+32),
/// each byte clamped to the classic 223 ceiling (col/row+32 ≤ 255).
/// `col`/`row` are 1-based cell coordinates of the cell under the cursor.
pub(super) fn wheel_report_bytes(sgr: bool, up: bool, col: u32, row: u32, count: usize) -> Vec<u8> {
    let btn: u32 = if up { 64 } else { 65 };
    let mut out = Vec::new();
    for _ in 0..count {
        if sgr {
            out.extend_from_slice(format!("\x1b[<{btn};{col};{row}M").as_bytes());
        } else {
            // When: sgr is false, clamp each legacy X10 coordinate to one byte.
            // X10: parameters are value+32, capped so col/row+32 fit a byte.
            let button_byte = (btn + 32).min(255) as u8;
            let column_byte = (col.min(223) + 32) as u8;
            let row_byte = (row.min(223) + 32) as u8;
            out.extend_from_slice(&[0x1b, b'[', b'M', button_byte, column_byte, row_byte]);
        }
    }
    out
}

/// Decide whether a key chord should trigger the quit confirmation guard.
///
/// `key_str` is the normalized chord (e.g. `"super+q"`) and `bound` is the
/// action the active keymap maps it to, if any. Cmd+Q is a macOS system
/// chord, so on macOS an *unbound* `super+q` still quits — a user's edited or
/// symlinked keymap frequently omits it, and falling through to the PTY (which
/// types a literal `q`) is never what the user wants. An explicit `quit_app`
/// binding is honored on every platform. If the user deliberately rebound
/// `super+q` to some other action, we stand down and let that action run.
pub(super) fn is_quit_chord(key_str: &str, bound: Option<&Action>) -> bool {
    if matches!(bound, Some(Action::QuitApp)) {
        // When: matches finds bound is Some(Action::QuitApp), trigger the quit guard.
        return true;
    }
    cfg!(target_os = "macos") && key_str == "super+q" && bound.is_none()
}

impl App {
    // Ordering: cursor_visible and the coherent keyboard_input word are Relaxed snapshots; output is read in the owner adapter.
    pub(super) fn do_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        win_id: WindowId,
        event: WindowEvent,
    ) {
        if self.is_warm_window_id(win_id) {
            // When: win_id is an unpromoted warm window, it owns no input scheduling state.
            return;
        }
        if !self.windows.contains_key(&win_id) {
            // When: win_id is stale, no sibling can inherit its input cause.
            return;
        }
        // Record the window's own pointer before an overlay, a modal or a handler can consume
        // the move or leave, so the tab-width hold never reads a stale position.
        self.record_window_pointer(win_id, &event);
        if let WindowEvent::Occluded(occluded) = &event {
            // When: `Occluded` arrives for a live owner, apply visibility before either role can collect a frame.
            self.handle_window_occlusion(win_id, *occluded);
            return;
        }
        // Main and child windows share one refresh, before the child dispatch below.
        self.refresh_monitor_for_event(win_id, &event);
        if let Some(window) = self.windows.get_mut(&win_id) {
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
                window.mark_redraw(super::redraw::RedrawCause::Input);
            }
        }
        if matches!(
            &event,
            WindowEvent::MouseWheel { .. }
                | WindowEvent::Ime(_)
                | WindowEvent::Resized(_)
                | WindowEvent::ScaleFactorChanged { .. }
                | WindowEvent::Focused(false)
        ) {
            if let Some(window) = self.windows.get_mut(&win_id) {
                window.invalidate_path_hover();
            }
        }
        if self.field_pointer_event(win_id, &event) {
            // When: `field_pointer_event` owns a query-field gesture, neither the modal nor the terminal sees it.
            return;
        }
        if self.command_palette_handle_pointer_event(win_id, &event) {
            // When: `command_palette_handle_pointer_event` consumes input, keep both window handlers from receiving it.
            if matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: MouseButton::Left,
                    ..
                }
            ) {
                self.drain_pending_window_creates(event_loop);
            }
            return;
        }
        match event {
            WindowEvent::DroppedFile(path) => {
                // When: DroppedFile arrives, retain its native path for a single window-local paste at the turn boundary.
                self.collect_winit_file_drop(win_id, path);
                return;
            }
            WindowEvent::Ime(ime_event) => {
                // When: Ime arrives, one source-window owner handles composition before child dispatch.
                self.handle_window_ime(win_id, ime_event);
                return;
            }
            WindowEvent::KeyboardInput { event, is_synthetic, .. } => {
                // When: KeyboardInput arrives, source ownership precedes deferred creation and source redraw.
                self.handle_window_keyboard(win_id, &event, is_synthetic);
                self.drain_pending_window_creates(event_loop);
                if let Some(window) = self.windows.get(&win_id) {
                    window.request_window_redraw();
                }
                return;
            }
            WindowEvent::Focused(focused) => {
                // When: Focused arrives, shared cleanup preserves held-input and native caret ownership.
                self.handle_window_focus_changed(win_id, focused);
                return;
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                // When: ModifiersChanged arrives, update only its source window and existing hover state.
                self.handle_window_modifiers_changed(win_id, modifiers.state());
                return;
            }
            _ => {
                // When: event is not shared native input, retain the existing presentation and pointer routing.
            }
        }
        // Tear-out child windows: route to the dedicated handler so
        // each child renders/handles input on its own surface.
        // the main window also lives in `self.windows`
        // now (shadow entry, `Some(main_window_id)`), but its events
        // must continue to flow through the legacy `App.*` paths
        // below. Skip the main window's id explicitly.
        if self.windows.contains_key(&win_id) && Some(win_id) != self.main_window_id {
            // When: windows contains win_id and main_window_id differs, delegate to child state.
            self.handle_child_window_event(event_loop, win_id, event);
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                if let Some(window) = self.window_key(win_id) {
                    self.observe_intent(sonicterm_app_core::AppIntent::WindowCloseRequested {
                        window,
                    });
                }
                // If child windows still own tabs, hide the main
                // window instead of exiting the app — the children
                // are independent live terminals and must keep
                // running. When no child remains, closing the main
                // leaves no active terminal window, so quit.
                if self.child_window_count() == 0 {
                    self.hide_main_window();
                    event_loop.exit();
                } else {
                    // When: child_window_count remains nonzero, hide main while child terminals stay live.
                    self.hide_main_window();
                }
            }

            WindowEvent::RedrawRequested => self.handle_main_redraw_requested(event_loop, win_id),

            WindowEvent::Resized(size) => {
                // When: WindowEvent::Resized supplies size, update geometry before scheduling.

                // Resized updates renderer and pane geometry before scheduling the replacement frame.
                let outcome = self
                    .main_renderer_mut()
                    .map(|renderer| renderer.try_resize_outcome(size.width, size.height));
                if outcome == Some(sonicterm_gpu::core::ResizeOutcome::Rejected) {
                    // When: `outcome` is `Rejected` for `size`, retain the previous surface.
                    tracing::warn!(
                        width = size.width,
                        height = size.height,
                        "main window resize ignored after renderer safety rejection"
                    );
                    return;
                }
                if let (Some(outcome), Some(window)) = (outcome, self.main_mut()) {
                    // A changed surface size owes a frame that a synchronized hold cannot keep back.
                    window.redraw.note_resize(outcome);
                }
                // Notify the reducer of the new logical grid dimensions.
                // Derive cols/rows from
                // the renderer's cell size; fall back to zero when
                // unavailable (smoke-test environments). The
                // reducer's `WindowResize` Effect is observability-
                // only — the boundary above already drove the wgpu
                // resize, and the existing `request_redraw` below is
                // the production paint path.
                let (cols_u16, rows_u16) = {
                    let cell = self.main_renderer().map(GpuRenderer::cell_size);
                    match cell {
                        Some((cell_w, cell_h)) if cell_w > 0.0 && cell_h > 0.0 => (
                            ((size.width as f32 / cell_w).floor() as u32).min(u16::MAX as u32)
                                as u16,
                            ((size.height as f32 / cell_h).floor() as u32).min(u16::MAX as u32)
                                as u16,
                        ),
                        _ => (0u16, 0u16),
                    }
                };
                self.observe_intent(sonicterm_app_core::AppIntent::WindowResized {
                    window: sonicterm_types::WindowKey::new(0),
                    cols: cols_u16,
                    rows: rows_u16,
                });
                // Per-pane sizing: each pane's grid + PTY is resized to
                // its own PaneRect within the new window content area,
                // never to the whole window's dimensions.
                let rects = self.compute_active_pane_rects();
                let metrics = self.main_renderer().map(|renderer| {
                    (
                        renderer.cell_size(),
                        [
                            renderer.padding_left_px(),
                            renderer.padding_right_px(),
                            renderer.padding_top_px(),
                            renderer.padding_bottom_px(),
                        ],
                    )
                });
                if let (Some(((cell_w, cell_h), inset)), Some(panes)) = (metrics, self.main_panes())
                {
                    crate::app::resize_panes_to_rects(panes, &rects, cell_w, cell_h, inset);
                }
                // Cell geometry changed — force the next render to
                // re-publish the IME cursor area even if (row, col) is
                // unchanged, otherwise the OS candidate window stays
                // pinned to the pre-resize pixel location.
                if let Some(window) = self.main_mut() {
                    window.ime_cursor_throttle.reset();
                }
                if let Some(main_window) = self.main_window() {
                    crate::app::frame_counters::request_native_redraw(main_window);
                }
            }

            WindowEvent::ScaleFactorChanged { scale_factor: dpi_scale, mut inner_size_writer } => {
                // When: ScaleFactorChanged arrives, synchronously bind native and renderer geometry to one physical target.
                if let Some(id) = self.main_window_id {
                    // When: main_window_id identifies the live main window, update exactly that state.
                    if let Some(window) = self.windows.get_mut(&id) {
                        // When: windows still contains id, apply the shared transition before winit commits WM_DPICHANGED.
                        let _ = crate::app::apply_window_dpi_transition(
                            window,
                            dpi_scale,
                            &mut inner_size_writer,
                        );
                    }
                }
            }

            // -- Mouse --
            WindowEvent::CursorLeft { .. } => self.handle_main_cursor_left(),
            WindowEvent::CursorMoved { position, .. } => self.handle_main_cursor_moved(position),

            WindowEvent::MouseWheel { delta, .. } => self.handle_main_mouse_wheel(delta),

            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                self.handle_main_left_mouse_input(event_loop, win_id, state);
                if state == ElementState::Released {
                    // A release ends any press or drag that held every bar, so a bar whose
                    // held tab widths can apply now is redrawn without waiting for more input.
                    self.redraw_held_tab_widths();
                }
            }

            _ => {
                // When: event matches no handled WindowEvent variant, leave application state unchanged.
            }
        }
    }

    /// Collect one parser snapshot for `RedrawRequested` before resolving hover and presenting.
    // Ordering: the active pane's cursor_visible flag is a Relaxed per-frame snapshot; no other
    // state is ordered by it.
    fn handle_main_redraw_requested(&mut self, event_loop: &ActiveEventLoop, win_id: WindowId) {
        let process_privileged = self.process_privilege.is_privileged();
        // The bar holds its widths while a tab gesture runs in any window or this window's own
        // pointer rests on it.
        let tab_bar_band = self
            .windows
            .get(&win_id)
            .and_then(|window| window.renderer.as_ref())
            .and_then(|renderer| renderer.tab_bar_band());
        let hold_tab_widths = self.tab_widths_held_in(win_id, tab_bar_band);
        if !self.admit_window_redraw(win_id) {
            // When: `admit_window_redraw` refuses this owner, no parser or image collection follows.
            return;
        }
        let was_dirty = self.main().is_some_and(|window| window.redraw.input_pending());
        let pty_burst = self.main().is_some_and(WindowState::visible_output_advanced);
        let mut timing = crate::app::render_timing::RenderTiming::start("main");
        self.pending_redraw = false;
        let main_id_opt = self.main_window_id;
        if let Some(id) = main_id_opt {
            if let Some(window) = self.windows.get_mut(&id) {
                window.tabs.clear_expired_command_badges(Instant::now());
            }
        }
        self.poll_command_events_for_all_tabs();
        // A burst frame reads the cache only; other frames set demand, never probe.
        let fg_probes = (!pty_burst).then(|| std::sync::Arc::clone(&self.fg_probes));
        if let Some(id) = main_id_opt {
            if let Some(window) = self.windows.get_mut(&id) {
                crate::app::refresh_window_tab_privileges(
                    &mut window.tabs,
                    &window.tab_states,
                    &mut window.panes,
                    fg_probes.as_deref(),
                    Instant::now(),
                );
            }
        }
        if let Some(timer) = timing.as_mut() {
            timer.lap("poll");
        }
        let Some(renderer) = self.main_renderer() else {
            // When: `renderer` is absent, no native geometry exists for this frame.
            return;
        };
        let (width, height) = renderer.logical_size();
        let top = (renderer.top_inset() - renderer.padding_top_px()).max(0.0);
        let outer = sonicterm_ui::pane::Rect::new(
            0.0,
            top,
            width.max(0.0),
            (height - top - renderer.bottom_inset()).max(0.0),
        );
        let sources = match self.main_visible_frame_sources(outer) {
            Ok(sources) => sources,
            Err(why) => {
                // When: `why` rejects topology, skip all assembly without treating it as contention.
                let now = self.dispatch_now();
                self.visible_frame_unavailable(win_id, why, was_dirty, now);
                return;
            }
        };
        let tab_idx = sources.tab_index;
        let active_id = sources.active_id();
        let active_pos = sources.active_pos;
        let pane_rects = sources.rects();
        // Scrollbar update/fade work uses geometry only and must be serviced even
        // when visible parser or image collection later encounters contention.
        let scrollbar_now = Instant::now();
        let scrollbar_motion = crate::app::scrollbar_visibility::window_scrollbar_motion(
            self.main_renderer().map(GpuRenderer::is_software_render_degraded),
            self.software_render_degrade,
        );
        let scrollbar_alpha_map: std::collections::HashMap<u64, f32> = {
            let mode = self.config.appearance.scrollbar;
            let drag_pane = self
                .main()
                .and_then(|window| window.scrollbar_drag.as_ref().map(|drag| drag.pane_id));
            let (cursor_x, cursor_y) =
                self.main().map(|window| window.cursor_pos).unwrap_or((0.0, 0.0));
            let cursor = (cursor_x as f32, cursor_y as f32);
            let rects: Vec<(u64, f32, f32, f32, f32)> =
                pane_rects.iter().map(|(id, rect)| (*id, rect.x, rect.y, rect.w, rect.h)).collect();
            if let Some(window) = self.main_mut() {
                crate::app::scrollbar_visibility::update_and_collect(
                    &mut window.scrollbar_vis,
                    &rects,
                    cursor,
                    active_id,
                    drag_pane,
                    mode,
                    scrollbar_motion,
                    scrollbar_now,
                )
            } else {
                // When: main_mut is None, no scrollbar visibility state can be collected.
                std::collections::HashMap::new()
            }
        };
        // Keep redrawing while any pane's scrollbar fade is
        // animating so a paused mouse-leave still completes the
        // 300 ms fade-out (otherwise the bar would stay frozen
        // mid-fade until the next external event).
        let scrollbar_needs_more_frames = {
            let mode = self.config.appearance.scrollbar;
            self.main()
                .map(|window| {
                    window.scrollbar_vis.values().any(|visibility| {
                        crate::app::scrollbar_visibility::is_animating(
                            visibility,
                            mode,
                            scrollbar_motion,
                        )
                    })
                })
                .unwrap_or(false)
        };
        if let Some(timer) = timing.as_mut() {
            timer.lap("scrollbar");
        }
        if scrollbar_needs_more_frames {
            if let Some(main_window) = self.main_window() {
                crate::app::frame_counters::request_native_redraw(main_window);
            }
        }

        // The callback is the scheduler generation snapshot slot, before either lock family.
        // The collected frame stays one owning value, so an unwind drops its fields in declaration
        // order: every parser guard, then the custody, the dispatch clock and the images.
        let mut frame = {
            // End the Result's drop scope before later branches release its borrowed sources.
            let collected = sources.try_collect(|| self.snapshot_window_redraw(win_id));
            match collected {
                Ok(frame) => frame,
                Err(why) => {
                    // When: `why` is contention, the collector already released every partial guard and image clone.
                    drop(collected);
                    drop(sources);
                    let now = self.dispatch_now();
                    self.visible_frame_unavailable(win_id, why, was_dirty, now);
                    return;
                }
            }
        };
        let frame_snapshot = frame.snapshot.take();
        // Recheck the synchronized-output hold under the guards; a held frame is abandoned unsettled.
        let sync_states: Vec<_> = frame
            .guards
            .iter()
            .map(|(id, parser, _)| (*id, parser.synchronized_output()))
            .collect();
        let now = self.dispatch_now();
        if self.abandon_synchronized_frame(win_id, &sync_states, now) {
            // When: `abandon_synchronized_frame` holds the frame, release the collection unsettled and unreceipted.
            drop(frame);
            drop(sources);
            return;
        }
        // Each watched pane's attribution record is copied here, under the guards the frame presents.
        let s10_candidates = self.s10_candidates(
            win_id,
            frame.guards.iter().map(|(id, parser, _)| (*id, &**parser)),
            now,
        );
        // One anchored viewport projection feeds both the per-pane and active-frame viewports.
        // Reconcile, then apply the previous frame's receipts under these guards, before planning.
        let frame_viewports = match self
            .main_mut()
            .map(|window| sources.reconcile_and_apply_receipts(window, &mut frame.guards))
        {
            Some(Ok(viewports)) => viewports,
            result => {
                // When: `result` has no coherent owner, release the complete collection before handling it.
                let why = result
                    .and_then(|result| result.err())
                    .unwrap_or(super::visible_frame::FrameUnavailable::NoLayout);
                drop(frame);
                drop(sources);
                let now = self.dispatch_now();
                self.visible_frame_unavailable(win_id, why, was_dirty, now);
                return;
            }
        };
        let broadcast_participants = self.broadcast_participants();
        if let Some(timer) = timing.as_mut() {
            timer.lap("layout");
        }

        self.refresh_target_hover_from_parsers(
            win_id,
            frame.guards.iter().map(|(id, parser, _)| (*id, &**parser)),
        );
        if let Some(window) = self.main_mut() {
            window.coherent_frame_collected();
        }
        let marker_observed = self.runtime_smoke.as_ref().is_some_and(|smoke| {
            smoke.is_waiting_for_marker()
                && frame
                    .guards
                    .iter()
                    .any(|(_, parser, _)| grid_contains_marker(parser.grid(), smoke.marker()))
        });
        if marker_observed {
            let baseline =
                self.main_renderer().map(GpuRenderer::successful_frame_count).unwrap_or(0);
            if let Some(smoke) = self.runtime_smoke.as_mut() {
                smoke.begin_present_wait(baseline);
            }
        }
        let smoke_waiting_for_present =
            self.runtime_smoke.as_ref().is_some_and(|smoke| smoke.is_waiting_for_present());
        let mut smoke_presented_count = None;
        let mut frame_completion = None;
        // A presented frame's receipts; bound and stored as the window's pending set after the call.
        let mut presented_receipts = Vec::new();

        // lift the main window Arc clone before the
        // mut borrow on `self.renderer` below, so the IME
        // cursor-area branch can still touch
        // `window.ime_cursor_throttle` (mut) without re-borrowing
        // `self`.
        let main_window_for_ime = self.main_window().cloned();
        // An owned handle for the watched dispatch's render pair; it holds no lock or borrow.
        let render_marker = self.render_marker(win_id);
        // Borrow-split: pull the renderer out via direct
        // map-lookup on `self.windows` (NOT through `main_renderer_mut`,
        // which would borrow all of `self`). That keeps
        // `self.command_palette`, `self.ime` available for the
        // disjoint mut borrows the render call needs in the same
        // expression scope.
        // panes live in the main `WindowState` too, so they're
        // pulled from the same field-disjoint split borrow.
        let main_id_opt = self.main_window_id;
        let mut ws_opt = main_id_opt.and_then(|id| self.windows.get_mut(&id));
        if let Some(main) = ws_opt.as_deref_mut() {
            // The collector validated the actual active position before taking any lock.
            invalidate_selection_for_content(
                &mut main.selection,
                &mut main.select_anchor,
                active_id,
                frame.guards[active_pos].1.grid(),
            );
            if self.command_palette.is_open() && self.palette_attached_window.is_none() {
                self.command_palette.set_context(super::overlays::command_palette_context(
                    main,
                    Some(frame.guards[active_pos].1.grid()),
                ));
                self.command_palette.set_tabs(&main.tabs, &self.i18n);
            }
        }
        #[allow(clippy::type_complexity)]
        let (
            renderer_opt,
            tabs_opt,
            tab_states_opt,
            panes_opt,
            cursor_visible_now,
            _last_render_slot,
            ws_selection_ref,
            ws_copy_mode_ref,
            ws_ime_ref,
            ws_ime_throttle_ref,
            ws_hovered_url_cells,
            ws_notification_ref,
            ws_link_preview_ref,
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
            Option<sonicterm_render_model::inputs::HoveredUrlCells>,
            Option<&sonicterm_ui::overlays::NotificationBubble>,
            Option<&sonicterm_render_model::inputs::LinkPreview>,
        ) = match ws_opt {
            Some(window) => {
                // Split the available WindowState render inputs into disjoint borrows.
                // cursor_visible is now per-pane; read
                // it from the active pane before splitting the
                // mut borrow of `window.panes`. Bool read, no
                // lasting borrow.
                let cursor_visible = window
                    .panes
                    .get(&active_id)
                    .map(|pane| pane.cursor_visible.load(std::sync::atomic::Ordering::Relaxed))
                    .unwrap_or(true);
                // selection + copy_mode now live on
                // `window`. Pull immutable refs disjoint from the mut
                // borrows of `window.{renderer,tabs,tab_states,panes,last_render}`.
                // ime + ime_cursor_throttle also live
                // on `window`; split-borrow disjointly too.
                let sel_ref = window.selection.as_ref();
                let cm_ref = window.copy_mode.as_ref();
                // Shared URI and OSC 8 fragments retain hint/accent state independently of glyph-row dirt.
                let hovered_url_cells =
                    window.hovered_url.as_ref().map(|hovered| hovered.to_cells());
                let notification_ref = window.notification.as_ref();
                (
                    window.renderer.as_mut(),
                    Some(&mut window.tabs),
                    Some(&mut window.tab_states),
                    Some(&mut window.panes),
                    cursor_visible,
                    Some(&mut window.last_render),
                    sel_ref,
                    cm_ref,
                    Some(&window.ime),
                    Some(&mut window.ime_cursor_throttle),
                    hovered_url_cells,
                    notification_ref,
                    window.link_preview.as_ref(),
                )
            }
            None => {
                // An absent WindowState produces empty render inputs without borrowing self.
                (None, None, None, None, true, None, None, None, None, None, None, None, None)
            }
        };
        if let (Some(renderer), Some(pane), Some(tabs_mref), Some(tab_states_mref)) = (
            renderer_opt,
            panes_opt.and_then(|panes| panes.get_mut(&active_id)),
            tabs_opt,
            tab_states_opt,
        ) {
            // When: renderer_opt, pane, tabs_mref, and tab_states_mref are Some, render one coherent frame.

            // Named by crates/sonicterm-gpu/src/lib_tests.rs, which pins the render call's text.
            #[allow(clippy::min_ident_chars)]
            let r = renderer;
            let (cursor_rc, cursor_pane_rect, field_ime) = {
                // `active_pos` comes from the validated layout, not an active-first assumption.
                // Wezterm-style tab title: `#N icon parent/leaf`.
                // Pull cwd from OSC 7, the foreground process from
                // the pid probe (macOS only for now), and the OSC
                // 0/2 title as the last-resort body (so `ssh
                // user@host` still labels itself).
                //
                // Shared with `app/child_window_redraw.rs` via
                // `refresh_active_tab_title` so Cmd+N / tear-out
                // windows pick up cwd-based titles too instead of
                // keeping the literal "shell N" placeholder set at
                // spawn time.
                if let Some(probes) = fg_probes.as_deref() {
                    super::privilege::demand_frame_foreground(
                        probes,
                        active_id,
                        pane,
                        Instant::now(),
                    );
                }
                let _ = crate::app::refresh_active_tab_title(
                    tabs_mref,
                    pane,
                    &frame.guards[active_pos].1,
                    tab_idx,
                );
                if let Some(search) =
                    tab_states_mref.get_mut(tab_idx).and_then(|tab_state| tab_state.search.as_mut())
                {
                    let grid = frame.guards[active_pos].1.grid();
                    let view_top =
                        GpuRenderer::resolved_view_top_abs_legacy(grid, frame_viewports.active);
                    super::search_handle::prepare_search(search, active_id, grid, view_top);
                }
                let search =
                    tab_states_mref.get(tab_idx).and_then(|tab_state| tab_state.search.as_ref());
                // Every reader after the render call takes its copy here: the call releases the guards.
                let cursor_copy = {
                    let grid = frame.guards[active_pos].1.grid();
                    (grid.cursor.row, grid.cursor.col)
                };
                let cursor_rect_copy = frame.guards[active_pos].2;
                let recovery_sample = self.runtime_smoke.as_ref().and_then(|smoke| {
                    smoke.recovery_marker_sample(
                        win_id,
                        frame
                            .guards
                            .iter()
                            .map(|(id, parser, _)| (*id, parser.grid(), frame_viewports.of(*id))),
                    )
                });
                // Keep the widths on screen, then measure changed titles with the tab font right
                // before drawing; hit-testing reads the stored widths of the frame on screen.
                let drawn_tab_widths = tabs_mref.laid_out_widths();
                // Prepare fonts first: a published fallback face invalidates placeholders and
                // stored tab widths before anything is measured or planned.
                let fonts = r.begin_frame_fonts();
                r.measure_tab_widths(
                    &fonts,
                    tabs_mref,
                    process_privileged,
                    hold_tab_widths,
                    Instant::now(),
                );
                r.set_render_timing_label("main");
                // Acquisition order here is parser guards, then the echo slot: the marker takes the slot only
                // inside enter and exit_at and releases it before the renderer runs, never across presentation.
                if let Some(marker) = render_marker.as_ref() {
                    marker.enter();
                }
                // The source owns the guards and media; they are released before presentation.
                let sonicterm_gpu::core::FrameOutcome { outcome, receipts } = r.render_releasing(
                    &fonts,
                    super::visible_frame::HeldFrameSource {
                        guards: std::mem::take(&mut frame.guards),
                        custody: frame.custody.take(),
                        images: std::mem::take(&mut frame.images),
                        viewports: &frame_viewports,
                        active: active_id,
                        broadcast: &broadcast_participants,
                        scrollbar_alpha: &scrollbar_alpha_map,
                    },
                    &self.theme,
                    cursor_visible_now
                        && !(self.command_palette.is_open()
                            && self.palette_attached_window.is_none()),
                    ws_selection_ref,
                    ws_copy_mode_ref,
                    tabs_mref,
                    process_privileged,
                    search,
                    // Feed the palette only to its attached window;
                    // `None` denotes main, and routing it elsewhere
                    // would paint the overlay on the wrong window.
                    self.palette_attached_window.is_none().then_some(&mut self.command_palette),
                    ws_ime_ref,
                    frame_viewports.active,
                    ws_notification_ref,
                    ws_hovered_url_cells,
                    ws_link_preview_ref,
                );
                // The renderer's return is read once, before either recorder works, and closes both.
                let dispatch = frame.dispatch.take();
                let returned_at =
                    (render_marker.is_some() || dispatch.is_some()).then(Instant::now);
                // An unwind skips the exit, leaving an unpaired enter that the analysis rejects.
                if let (Some(marker), Some(returned_at)) = (render_marker.as_ref(), returned_at) {
                    marker.exit_at(returned_at);
                }
                // The dispatch interval ends at the render call's return, in the rendered population.
                if let (Some(dispatch), Some(returned_at)) = (dispatch, returned_at) {
                    dispatch.rendered_at(returned_at);
                }
                // The frame now holds no guard and no timing; release it before the sources it borrowed.
                drop(frame);
                // Keep the new widths only if this frame reached the screen, so hit-testing
                // matches the bar the user sees.
                super::tab_widths::settle_tab_widths(tabs_mref, drawn_tab_widths, &outcome);
                if let Some(recovery) = self.gpu_recovery.as_mut() {
                    recovery.observe_frame(r.device_generation(), &outcome, Instant::now());
                }
                presented_receipts = receipts;
                if let (Some(smoke), Some(sample)) =
                    (self.runtime_smoke.as_mut(), recovery_sample.as_ref())
                {
                    smoke.observe_recovery_frame(win_id, r.device_generation(), sample, &outcome);
                }
                frame_completion = Some(super::redraw::FrameSettlement::of(&outcome));
                // Map the typed outcome back to the compatibility result: only a
                // failure or the device's first stopped frame is an error here.
                if let Err(error) = outcome.into_render_result() {
                    tracing::warn!("render error: {error}");
                    if smoke_waiting_for_present {
                        smoke_presented_count = Some(Err(RuntimeSmokeFailure::Present));
                    }
                } else if smoke_waiting_for_present {
                    // When: `smoke_waiting_for_present` is true, retain the post-render native-present count.
                    smoke_presented_count = Some(Ok(r.successful_frame_count()));
                }
                if let Some(smoke) = self.runtime_smoke.as_mut() {
                    smoke.note_render_attempt();
                }
                if let Some(timer) = timing.as_mut() {
                    timer.lap("render");
                }
                // A field that owns IME anchors to the caret this frame presented; a field
                // whose caret is not on screen yet suppresses the terminal anchor.
                let preedit = ws_ime_ref.map(|ime| ime.preedit()).unwrap_or("");
                let field_ime = super::overlays::field_ime_anchor(
                    r,
                    self.palette_attached_window.is_none().then_some(&self.command_palette),
                    search,
                    preedit,
                );
                (cursor_copy, cursor_rect_copy, field_ime)
            };
            // refresh the OS-drag tab bar
            // snapshot so cross-window drop hit-tests see the
            // current layout. Cross-window drops read this; an
            // empty registry means every drop resolves to
            // `DroppedOnEmpty` instead of a concrete destination.
            // (moved after the renderer borrow scope below)
            // Tell the OS where the active text cursor lives so the
            // IME candidate window (pinyin candidates, Japanese
            // romaji selector, Korean Hangul composer) appears
            // immediately below the cell being edited — not
            // pinned to the top-left corner of the screen as
            // happens when the area is never set.
            if let Some(main_window) = main_window_for_ime {
                let mut ws_ime_throttle_ref = ws_ime_throttle_ref;
                if field_ime != super::overlays::FieldImeAnchor::Terminal {
                    if let Some(throttle) = ws_ime_throttle_ref.as_deref_mut() {
                        throttle.reset();
                    }
                }
                if let super::overlays::FieldImeAnchor::Field(caret) = field_ime {
                    // A palette or search field owns IME, so the candidate window follows the
                    // caret this frame presented.
                    let (pos, size) = super::overlays::field_ime_area(caret);
                    main_window.set_ime_cursor_area(pos, size);
                } else if field_ime == super::overlays::FieldImeAnchor::Pending {
                    // When: `field_ime` is Pending, a field owns IME but its caret is unpresented; wait for the next frame.
                } else if let Some(throttle) = ws_ime_throttle_ref {
                    // When: ws_ime_throttle_ref is Some(throttle), use terminal cell IME geometry.
                    if let Some([origin_x, origin_y]) = r.pane_grid_origin(active_id) {
                        // IME follows the planned text origin rather than raw pane padding.
                        let rect = sonicterm_ui::pane::Rect::new(
                            origin_x,
                            origin_y,
                            cursor_pane_rect.w,
                            cursor_pane_rect.h,
                        );
                        super::update_terminal_ime_cursor_area(
                            throttle,
                            (active_id, rect),
                            cursor_rc,
                            (r.cell_w, r.cell_h),
                            (0.0, 0.0),
                            |pos, size| main_window.set_ime_cursor_area(pos, size),
                        );
                    }
                }
            }
        } else {
            // When: the main window has no renderer, nothing renders; release the guards, then close the timing.
            drop(frame);
        }
        if let Some(window) = self.main_mut() {
            // A presented frame's receipts replace the pending set emptied at collection.
            sources.store_presented(&mut window.pending_receipts, presented_receipts);
        }
        if let (Some(snapshot), Some(outcome)) = (frame_snapshot.as_ref(), frame_completion) {
            self.complete_window_redraw(win_id, snapshot, outcome);
        }
        // Only a presented attempt emits its records, numbered by the main renderer's presented count.
        let presented_seq =
            matches!(frame_completion, Some(super::redraw::FrameSettlement::Presented))
                .then(|| self.main_renderer().map(GpuRenderer::successful_frame_count))
                .flatten();
        self.commit_s10_candidates(s10_candidates, presented_seq);
        if let Some(presented) = smoke_presented_count {
            // When: `smoke_presented_count` contains `presented`, classify the marker-bearing frame.
            match presented {
                Ok(count) => {
                    // Compare the successful presentation count with the frozen baseline.
                    let advanced = self
                        .runtime_smoke
                        .as_mut()
                        .is_some_and(|smoke| smoke.observe_presented_frame(count));
                    if advanced {
                        // The marker-bearing main frame has now reached native presentation.
                        tracing::info!("runtime smoke main presentation exercised");
                    }
                }
                Err(failure) => {
                    // When: `presented` is `Err(failure)`, retain presentation as the failed boundary.
                    if let Some(smoke) = self.runtime_smoke.as_mut() {
                        smoke.fail(failure);
                    }
                    event_loop.exit();
                    return;
                }
            }
        }
        // refresh OS-drag tab bar snapshot
        // for the main window. Outside the renderer borrow scope
        // so the immutable self borrow doesn't conflict with `r`.
        self.publish_main_window_tab_bar();
        if let Some(timer) = timing {
            timer.finish();
        }
    }
}

#[cfg(test)]
#[path = "window_event_tests.rs"]
mod window_event_tests;
