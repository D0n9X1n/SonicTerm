//! `App::do_window_event`: the `WindowEvent` dispatcher, the main-window redraw
//! handler, and the pointer, wheel, key-repeat and quit-chord helpers both window
//! roles share. Keyboard, IME and focus routing live in `window_keyboard`,
//! main-window pointer handlers in `window_pointer`, and pane-divider input in
//! `splitter_input`.

use std::time::Instant;

use sonicterm_cfg::{config::ScrollbarMode, keymap::Action};
use sonicterm_gpu::core::GpuRenderer;
use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_ui::overlays::{
    search_bar_label, search_query_caret_prefix, SearchBarLayout, SEARCH_BAR_ICON_GAP,
    SEARCH_BAR_PAD_LEFT, SEARCH_BAR_PAD_RIGHT,
};
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

const SEARCH_BADGE_ICON: &str = "";

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

/// Snapshot the terminal's tracking mode and SGR/legacy encoding profile.
pub(super) fn parser_mouse_profile(parser: &sonicterm_vt::vt::Parser) -> (MouseTracking, bool) {
    (parser.mouse_tracking(), parser.mouse_sgr_enabled())
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
        if let WindowEvent::Occluded(occluded) = &event {
            // When: `Occluded` arrives for a live owner, apply visibility before either role can collect a frame.
            self.handle_window_occlusion(win_id, *occluded);
            return;
        }
        if let Some(window) = self.windows.get_mut(&win_id) {
            if matches!(event, WindowEvent::Moved(_) | WindowEvent::ScaleFactorChanged { .. }) {
                window.refresh_monitor_period();
            }
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
                    window.request_redraw();
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
                if self
                    .main_renderer_mut()
                    .is_some_and(|renderer| !renderer.try_resize(size.width, size.height))
                {
                    // When: try_resize returns false for size, retain the previous surface.
                    tracing::warn!(
                        width = size.width,
                        height = size.height,
                        "main window resize ignored after renderer safety rejection"
                    );
                    return;
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
                    main_window.request_redraw();
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
                self.handle_main_left_mouse_input(event_loop, win_id, state)
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
        if !self.begin_window_redraw(win_id, Instant::now()) {
            // When: `begin_window_redraw` refuses this owner, no parser or image collection follows.
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
        if let Some(id) = main_id_opt {
            if let Some(window) = self.windows.get_mut(&id) {
                crate::app::refresh_window_tab_privileges(
                    &mut window.tabs,
                    &window.tab_states,
                    &mut window.panes,
                    !pty_burst,
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
                self.visible_frame_unavailable(win_id, why, was_dirty, Instant::now());
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
            let drag_pane = self
                .main()
                .and_then(|window| window.scrollbar_drag.as_ref().map(|drag| drag.pane_id));
            self.main()
                .map(|window| {
                    window.scrollbar_vis.iter().any(|(id, visibility)| {
                        crate::app::scrollbar_visibility::is_animating(
                            visibility,
                            mode,
                            drag_pane == Some(*id),
                            scrollbar_motion,
                            scrollbar_now,
                        )
                    })
                })
                .unwrap_or(false)
        };
        if let Some(timer) = timing.as_mut() {
            timer.lap("scrollbar");
        }
        if scrollbar_needs_more_frames {
            if let Some(w) = self.main_window() {
                w.request_redraw();
            }
        }

        // The callback is the scheduler generation snapshot slot, before either lock family.
        let super::visible_frame::HeldVisibleFrame {
            snapshot: frame_snapshot,
            mut guards,
            mut images,
        } = {
            // End the Result's drop scope before later branches release its borrowed sources.
            let collected = sources.try_collect(|| self.snapshot_window_redraw(win_id));
            match collected {
                Ok(frame) => frame,
                Err(why) => {
                    // When: `why` is contention, the collector already released every partial guard and image clone.
                    drop(collected);
                    drop(sources);
                    self.visible_frame_unavailable(win_id, why, was_dirty, Instant::now());
                    return;
                }
            }
        };
        // One anchored viewport projection feeds both the per-pane and active-frame viewports.
        let frame_viewports = match self
            .main_mut()
            .map(|window| sources.reconcile_viewports(&mut window.panes, &guards))
        {
            Some(Ok(viewports)) => viewports,
            result => {
                // When: `result` has no coherent owner, release the complete collection before handling it.
                let why = result
                    .and_then(|result| result.err())
                    .unwrap_or(super::visible_frame::FrameUnavailable::NoLayout);
                drop(guards);
                drop(images);
                drop(sources);
                self.visible_frame_unavailable(win_id, why, was_dirty, Instant::now());
                return;
            }
        };
        let broadcast_participants = self.broadcast_participants();
        if let Some(timer) = timing.as_mut() {
            timer.lap("layout");
        }

        self.refresh_target_hover_from_parsers(
            win_id,
            guards.iter().map(|(id, parser, _)| (*id, &**parser)),
        );
        if let Some(window) = self.main_mut() {
            window.coherent_frame_collected();
        }
        let marker_observed = self.runtime_smoke.as_ref().is_some_and(|smoke| {
            smoke.is_waiting_for_marker()
                && guards
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

        // lift the main window Arc clone before the
        // mut borrow on `self.renderer` below, so the IME
        // cursor-area branch can still touch
        // `window.ime_cursor_throttle` (mut) without re-borrowing
        // `self`.
        let main_window_for_ime = self.main_window().cloned();
        let main_palette_ime_area = main_window_for_ime.as_ref().and_then(|main_window| {
            let main_renderer = self.main_renderer()?;
            if self.palette_attached_window.is_some() || !self.command_palette.is_open() {
                // When: palette_attached_window is Some or command_palette is closed, main has no IME anchor.
                return None;
            }
            self.command_palette_ime_cursor_area(
                main_window.inner_size().width as f32,
                main_window.inner_size().height as f32,
                self.config.appearance.panel_padding,
                main_renderer.scale_factor(),
                main_renderer.font_size() * main_renderer.scale_factor(),
                main_renderer.cell_w,
            )
        });
        // Search-bar IME geometry: the full marker-free label drives
        // box width, but the caret/candidate-window anchor must follow
        // the current query caret, not the end of the label. Produce
        // both strings from the same state so the OS candidate area
        // agrees with the renderer-owned block cursor.
        let (search_ime_label, search_ime_prefix) = self
            .main()
            .and_then(|window| {
                let preedit = window.ime.preedit();
                let active_index = window.tabs.active_index();
                window
                    .tab_states
                    .get(active_index)
                    .and_then(|tab_state| tab_state.search.as_ref())
                    .map(|search| {
                        (
                            search_bar_label(search, preedit),
                            search_query_caret_prefix(search, preedit),
                        )
                    })
            })
            .unzip();
        // Borrow-split: pull the renderer out via direct
        // map-lookup on `self.windows` (NOT through `main_renderer_mut`,
        // which would borrow all of `self`). That keeps
        // `self.command_palette`, `self.ime` available for the
        // disjoint mut borrows the render call needs in the same
        // expression scope.
        // panes now live in `ws` too, so they're
        // pulled from the same field-disjoint split borrow.
        let main_id_opt = self.main_window_id;
        let mut ws_opt = main_id_opt.and_then(|id| self.windows.get_mut(&id));
        if let Some(ws) = ws_opt.as_deref_mut() {
            // The collector validated the actual active position before taking any lock.
            invalidate_selection_for_content(
                &mut ws.selection,
                &mut ws.select_anchor,
                active_id,
                guards[active_pos].1.grid(),
            );
            if self.command_palette.is_open() && self.palette_attached_window.is_none() {
                self.command_palette.set_context(super::overlays::command_palette_context(
                    ws,
                    Some(guards[active_pos].1.grid()),
                ));
                self.command_palette.set_tabs(&ws.tabs, &self.i18n);
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
            let (cursor_rc, cursor_pane_rect) = {
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
                let _ = crate::app::refresh_active_tab_title(
                    tabs_mref,
                    pane,
                    &guards[active_pos].1,
                    tab_idx,
                    !pty_burst,
                );
                if let Some(search) =
                    tab_states_mref.get_mut(tab_idx).and_then(|tab_state| tab_state.search.as_mut())
                {
                    let grid = guards[active_pos].1.grid();
                    let view_top =
                        GpuRenderer::resolved_view_top_abs_legacy(grid, frame_viewports.active);
                    super::search_handle::prepare_search(search, active_id, grid, view_top);
                }
                let search =
                    tab_states_mref.get(tab_idx).and_then(|tab_state| tab_state.search.as_ref());
                // The shared builder borrows all visible grids and moves each image snapshot once.
                let mut panes_slice = super::visible_frame::pane_renders(
                    &mut guards,
                    &mut images,
                    &frame_viewports,
                    active_id,
                    &broadcast_participants,
                    &scrollbar_alpha_map,
                );
                r.set_render_timing_label("main");
                let outcome = r.render_with_outcome(
                    &mut panes_slice,
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
                if let Some(recovery) = self.gpu_recovery.as_mut() {
                    recovery.observe_frame(r.device_generation(), &outcome, Instant::now());
                }
                if let Some(smoke) = self.runtime_smoke.as_mut() {
                    smoke.observe_recovery_frame(
                        win_id,
                        r.device_generation(),
                        &panes_slice,
                        &outcome,
                    );
                }
                frame_completion =
                    Some((super::redraw::FrameSettlement::of(&outcome), Instant::now()));
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
                let grid = guards[active_pos].1.grid_mut();
                ((grid.cursor.row, grid.cursor.col), guards[active_pos].2)
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
                if main_palette_ime_area.is_some() || search_ime_label.is_some() {
                    if let Some(throttle) = ws_ime_throttle_ref.as_deref_mut() {
                        throttle.reset();
                    }
                }
                if let Some((pos, size)) = main_palette_ime_area {
                    // The main-hosted palette anchors the candidate window to its caret.
                    main_window.set_ime_cursor_area(pos, size);
                } else if let Some(search_label) = search_ime_label.as_ref() {
                    // When: search_ime_label is Some, derive the candidate anchor from its query caret.
                    let window_size = main_window.inner_size();
                    // window_size + the SearchBarLayout it feeds are
                    // physical px, so every logical-px term here must be
                    // scaled by the renderer's scale factor or the IME
                    // caret rect drifts on HiDPI displays.
                    let scale = r.scale_factor();
                    let font_size = r.font_size() * scale;
                    let icon_w = r.measure_overlay_text_width(SEARCH_BADGE_ICON, font_size);
                    let content_w = icon_w
                        + SEARCH_BAR_ICON_GAP * scale
                        + r.measure_overlay_text_width(search_label, font_size);
                    let row = u8::from(
                        ws_copy_mode_ref.is_some_and(|copy_mode| copy_mode.is_read_only()),
                    );
                    let layout = SearchBarLayout::compute_at_row(
                        window_size.width as f32,
                        window_size.height as f32,
                        content_w,
                        row,
                        scale,
                    );
                    let text_x = layout.border.x
                        + SEARCH_BAR_PAD_LEFT * scale
                        + icon_w
                        + SEARCH_BAR_ICON_GAP * scale;
                    // Right inner edge: the candidate window must never
                    // push past the box padding.
                    let right_edge = (layout.border.x + layout.border.w
                        - SEARCH_BAR_PAD_RIGHT * scale)
                        .max(text_x);
                    // Anchor the OS candidate window at the END OF THE
                    // QUERY (`text_x + width("/ " + query)`), matching
                    // the inline preedit caret, then clamp to the right
                    // inner edge. `font_size` already folds in `scale`.
                    let prefix_w = search_ime_prefix
                        .as_ref()
                        .map(|prefix| r.measure_overlay_text_width(prefix, font_size))
                        .unwrap_or(0.0);
                    let caret_x = (text_x + prefix_w).clamp(text_x, right_edge);
                    let pos =
                        winit::dpi::PhysicalPosition::new(caret_x as i32, layout.border.y as i32);
                    let size = winit::dpi::PhysicalSize::new(
                        r.cell_w.ceil() as u32,
                        layout.border.h.ceil() as u32,
                    );
                    main_window.set_ime_cursor_area(pos, size);
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
        }
        if let (Some(snapshot), Some((outcome, at))) = (frame_snapshot.as_ref(), frame_completion) {
            self.finish_window_redraw(win_id, snapshot, outcome, at);
        }
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
