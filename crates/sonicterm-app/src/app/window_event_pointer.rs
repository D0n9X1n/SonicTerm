//! Source-owned terminal pointer gesture and protocol routing.

use super::*;
use crate::app::input_dispatch::MotionProfile;

/// Encode one pane-local pointer report at zero-based cell (`row`, `col`) and pane pixel `pixel`
/// in `format`.
pub(in crate::app) fn pointer_report_bytes(
    format: MouseFormat,
    kind: PointerReportKind,
    modifiers: ModifiersState,
    row: u16,
    col: u16,
    pixel: (i32, i32),
) -> Vec<u8> {
    let button = match kind {
        PointerReportKind::LeftPress | PointerReportKind::LeftRelease => 0,
        PointerReportKind::HeldLeftMotion => 32,
        PointerReportKind::NoButtonMotion => 35,
    };
    crate::app::mouse_report::encode_mouse_report(
        format,
        crate::app::mouse_report::MouseReport {
            button,
            modifiers: crate::app::secondary_button::pointer_modifier_bits(modifiers),
            release: kind == PointerReportKind::LeftRelease,
            cell: (row, col),
            pixel,
        },
    )
}

/// Return an existing terminal press's destinations only for repeat events.
pub(in crate::app) fn terminal_repeat_targets(
    pressed_keys: &super::super::keyboard_protocol::PressedKeys,
    physical_key: PhysicalKey,
    repeat: bool,
) -> Option<super::super::keyboard_protocol::KeyRoutes> {
    repeat.then(|| pressed_keys.get(&physical_key).cloned()).flatten()
}

/// Choose and latch the owner of a left-button grid press with the press-time `tracking`, `format`
/// and screen (`is_alt`).
pub(in crate::app) fn begin_pointer_gesture(
    cell: PointerCell,
    tracking: MouseTracking,
    format: MouseFormat,
    is_alt: bool,
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
            PointerGestureOwner::Terminal { tracking, format, is_alt }
        }
    };
    Some(PointerGesture {
        owner,
        press_pane: cell.pane_id,
        last_cell: cell,
        anchor: None,
        prompt: None,
    })
}

impl WindowState {
    /// Begin one grid press from this window's live modifier state, latching the press-time
    /// `tracking`, `format` and screen (`is_alt`) its held motion is stamped with.
    pub(in crate::app) fn begin_pointer_press(
        &mut self,
        cell: PointerCell,
        tracking: MouseTracking,
        format: MouseFormat,
        is_alt: bool,
    ) -> Option<Vec<u8>> {
        let gesture = if self.copy_mode.as_ref().is_some_and(CopyModeState::is_read_only) {
            // READONLY selects local ownership only at the press; an earlier terminal hold keeps its route.
            Some(PointerGesture {
                owner: PointerGestureOwner::Local,
                press_pane: cell.pane_id,
                last_cell: cell,
                anchor: None,
                prompt: None,
            })
        } else {
            // When: copy_mode is not READONLY, retain the normal Shift and tracking ownership decision.
            begin_pointer_gesture(cell, tracking, format, is_alt, self.modifiers, false)
        };
        self.pointer_gesture = gesture;
        let PointerGestureOwner::Terminal { format, .. } = gesture?.owner else {
            // When: `gesture.owner` is Local, preserve the selection and emit no terminal report.
            return None;
        };
        self.selection = None;
        Some(pointer_report_bytes(
            format,
            PointerReportKind::LeftPress,
            self.modifiers,
            cell.row,
            cell.col,
            cell.pixel,
        ))
    }
}

/// Route motion for an already-latched left-button gesture. `press_pixel` is the pointer measured
/// from the press pane's origin wherever it is, so 1016 reports a drag past the pane's edge.
pub(in crate::app) fn route_pressed_pointer_motion(
    gesture: &mut PointerGesture,
    cell: Option<PointerCell>,
    press_pixel: (i32, i32),
    modifiers: ModifiersState,
) -> PointerMotionRoute {
    let PointerGestureOwner::Terminal { tracking, format, .. } = gesture.owner else {
        // When: local selection owns the gesture, current parser/modifier state cannot steal it.
        return PointerMotionRoute::Local;
    };
    if let Some(cell) = cell.filter(|cell| cell.pane_id == gesture.press_pane) {
        gesture.last_cell = cell;
    }
    gesture.last_cell.pixel = press_pixel;
    if tracking == MouseTracking::Button {
        // When: Button mode reports transitions only, suppress held motion.
        return PointerMotionRoute::None;
    }
    PointerMotionRoute::Report {
        pane_id: gesture.press_pane,
        format,
        row: gesture.last_cell.row,
        col: gesture.last_cell.col,
        pixel: gesture.last_cell.pixel,
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

/// The renderer metrics that place a pane's scrollbar: content padding (left, right, top,
/// bottom), the cell size the content never shrinks below, and the display scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::app) struct ScrollbarMetrics {
    /// Content padding in window pixels: left, right, top, bottom.
    pub(in crate::app) insets: [f32; 4],
    /// One cell's width and height in window pixels.
    pub(in crate::app) cell: (f32, f32),
    /// Display scale factor; the gutter width is logical pixels times this.
    pub(in crate::app) scale: f32,
}

impl ScrollbarMetrics {
    /// The metrics of a live renderer.
    pub(in crate::app) fn of(renderer: &sonicterm_gpu::core::GpuRenderer) -> Self {
        Self {
            insets: [
                renderer.padding_left_px(),
                renderer.padding_right_px(),
                renderer.padding_top_px(),
                renderer.padding_bottom_px(),
            ],
            cell: renderer.cell_size(),
            scale: renderer.scale_factor(),
        }
    }
}

/// Whether a no-button pointer at (`pixel_x`, `pixel_y`) belongs to the scrollbar of the pane laid
/// out at `pane` (its outer rect). With `metrics` (a live renderer) the gutter is measured inside
/// the padded content at the scaled width; without one (headless), on the outer rect at the
/// logical width. Main and child windows both route motion through this one calculation.
pub(in crate::app) fn pane_scrollbar_owns_pointer(
    mode: ScrollbarMode,
    pane: sonicterm_ui::pane::Rect,
    metrics: Option<ScrollbarMetrics>,
    pixel: (f32, f32),
    edge_active: bool,
    visible: bool,
) -> bool {
    let width_px = crate::app::scrollbar_input::SCROLLBAR_WIDTH_PX;
    let (content, gutter_width) = match metrics {
        Some(metrics) => (
            pointer_scrollbar_content_rect(pane, metrics.insets, metrics.cell),
            width_px * metrics.scale,
        ),
        None => (pane, width_px),
    };
    native_scrollbar_owns_pointer(
        mode,
        content,
        pixel.0,
        pixel.1,
        gutter_width,
        edge_active,
        visible,
    )
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
    format: MouseFormat,
    modifiers: ModifiersState,
    ui_consumed: bool,
) -> Option<PointerMotionRoute> {
    (!ui_consumed && tracking == MouseTracking::AnyMotion).then_some(PointerMotionRoute::Report {
        pane_id: cell.pane_id,
        format,
        row: cell.row,
        col: cell.col,
        pixel: cell.pixel,
        modifiers,
    })
}

impl PointerGesture {
    /// The profile a terminal-owned gesture's held motion is stamped with: its press-time
    /// tracking, format and screen, with the button held; `None` for a local gesture.
    pub(in crate::app) fn held_profile(&self) -> Option<MotionProfile> {
        let PointerGestureOwner::Terminal { tracking, format, is_alt } = self.owner else {
            // When: owner is Local, the gesture reports nothing to the terminal.
            return None;
        };
        Some(MotionProfile { tracking, format, is_alt, held: true })
    }
}

/// Consume a left-button gesture and return its terminal release route, if any.
pub(in crate::app) fn take_pointer_release(
    gesture: &mut Option<PointerGesture>,
    modifiers: ModifiersState,
) -> Option<PointerMotionRoute> {
    let gesture = gesture.take()?;
    let PointerGestureOwner::Terminal { format, .. } = gesture.owner else {
        // When: local selection owns the gesture, release stays entirely local.
        return None;
    };
    Some(PointerMotionRoute::Report {
        pane_id: gesture.press_pane,
        format,
        row: gesture.last_cell.row,
        col: gesture.last_cell.col,
        pixel: gesture.last_cell.pixel,
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
    let PointerMotionRoute::Report { pane_id, format, row, col, pixel, modifiers } = route else {
        // When: `route` is Local or None, ownership or mode suppressed the terminal report.
        return None;
    };
    Some((pane_id, pointer_report_bytes(format, kind, modifiers, row, col, pixel)))
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

/// Encode `count` mouse-wheel reports in `format` at zero-based cell `cell` (row, column) and pane
/// pixel `pixel`. Wheel buttons per xterm: 64 up, 65 down, a press with no release.
pub(in crate::app) fn wheel_report_bytes(
    format: MouseFormat,
    up: bool,
    cell: (u16, u16),
    pixel: (i32, i32),
    count: usize,
) -> Vec<u8> {
    let report = crate::app::mouse_report::MouseReport {
        button: if up { 64 } else { 65 },
        modifiers: 0,
        release: false,
        cell,
        pixel,
    };
    crate::app::mouse_report::encode_mouse_report(format, report).repeat(count)
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
        // A right or middle button that reached no palette never reaches the window handlers,
        // which handle only the left one: the strip takes a right press first, else a tracking pane.
        let secondary = match event {
            WindowEvent::MouseInput { state, button, .. } => {
                crate::app::secondary_button::SecondaryButton::from_winit(*button)
                    .map(|secondary| (secondary, *state, *button == MouseButton::Right))
            }
            _ => None,
        };
        if let Some((secondary, state, right)) = secondary {
            // When: the event is a `secondary` button, it is consumed here.
            if !(right && self.handle_right_mouse_input(win_id, state)) {
                self.route_secondary_button(win_id, secondary, state);
            }
            return Some(false);
        }
        let left_press = matches!(
            event,
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. }
        );
        if left_press
            && self
                .registry
                .windows
                .get(&win_id)
                .is_some_and(|window| window.secondary_press.is_some())
        {
            // When: a `left_press` comes while a right or middle press is held, the first button
            // keeps the pointer and the left press does nothing, in the main and child windows.
            return Some(false);
        }
        None
    }
}

#[cfg(test)]
#[path = "window_event_pointer_tests.rs"]
mod window_event_pointer_tests;
