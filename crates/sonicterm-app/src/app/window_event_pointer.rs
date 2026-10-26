//! Source-owned terminal pointer gesture and protocol routing.

use super::*;

/// Encode one pane-local pointer report in SGR or classic X10 form.
pub(in crate::app) fn pointer_report_bytes(
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
pub(in crate::app) fn terminal_repeat_targets(
    pressed_keys: &super::super::keyboard_protocol::PressedKeys,
    physical_key: PhysicalKey,
    repeat: bool,
) -> Option<super::super::keyboard_protocol::KeyRoutes> {
    repeat.then(|| pressed_keys.get(&physical_key).cloned()).flatten()
}

/// Choose and latch the owner of a left-button grid press.
pub(in crate::app) fn begin_pointer_gesture(
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
    pub(in crate::app) fn begin_pointer_press(
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
pub(in crate::app) fn route_pressed_pointer_motion(
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
pub(in crate::app) fn native_scrollbar_owns_pointer(
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
pub(in crate::app) fn pointer_scrollbar_content_rect(
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
pub(in crate::app) fn no_button_motion_report(
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
pub(in crate::app) fn take_pointer_release(
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
pub(in crate::app) fn take_focus_loss_pointer_release(
    gesture: &mut Option<PointerGesture>,
    modifiers: ModifiersState,
) -> Option<PointerMotionRoute> {
    take_pointer_release(gesture, modifiers)
}

/// Clear a gesture whose release can no longer arrive.
pub(in crate::app) fn cancel_pointer_gesture(gesture: &mut Option<PointerGesture>) -> bool {
    gesture.take().is_some()
}

/// Convert a pure pointer route into one pane-targeted protocol payload.
pub(in crate::app) fn pointer_route_bytes(
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
pub(in crate::app) fn headless_pane_cell(
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

/// Select terminal wheel reports or the screen's untracked fallback.
pub(in crate::app) fn wheel_route(tracking: MouseTracking, is_alt: bool) -> WheelRoute {
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
pub(in crate::app) fn wheel_report_bytes(
    sgr: bool,
    up: bool,
    col: u32,
    row: u32,
    count: usize,
) -> Vec<u8> {
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

impl App {
    pub(in crate::app) fn route_shared_pointer(
        &mut self,
        win_id: WindowId,
        event: &WindowEvent,
    ) -> Option<bool> {
        if matches!(event, WindowEvent::MouseInput { state: ElementState::Pressed, .. }) {
            // Any press outside the strip ends strip focus before a field, the palette, a toast or
            // a pane handles it.
            self.end_status_focus_outside(win_id);
        }
        if self.field_pointer_event(win_id, event) {
            // When: `field_pointer_event` owns a query-field gesture, neither the modal nor the terminal sees it.
            return Some(false);
        }
        if self.command_palette_handle_pointer_event(win_id, event) {
            // When: `command_palette_handle_pointer_event` consumes input, keep both window handlers from receiving it.
            return Some(matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: MouseButton::Left,
                    ..
                }
            ));
        }
        if let WindowEvent::MouseInput { state, button: MouseButton::Right, .. } = event {
            // When: MouseButton::Right reaches no palette, only the strip handles it; the rest of
            // right input is consumed unhandled, as before, and never reaches a pane.
            let _ = self.handle_right_mouse_input(win_id, *state);
            return Some(false);
        }
        None
    }
}

#[cfg(test)]
#[path = "window_event_pointer_tests.rs"]
mod window_event_pointer_tests;
