//! The wheel rules main and child windows share: how far a wheel event moves, and whether it
//! becomes mouse reports, cursor keys, or local scrollback for the pane under the pointer. Each
//! window's handler only finds the pane and cell, then carries the plan out.

use sonicterm_vt::vt::MouseTracking;
use winit::event::MouseScrollDelta;

use super::window_event::{wheel_report_bytes, wheel_route, WheelRoute};

/// Lines one wheel notch moves, matching the stock GTK and Cocoa feel.
const LINES_PER_TICK: f32 = 3.0;

/// What one wheel event does to the pane under the pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WheelPlan {
    /// The motion rounded to no lines.
    None,
    /// Bytes for the pane's PTY: mouse reports or cursor keys.
    Input(Vec<u8>),
    /// Move the pane's local scrollback by this many lines; negative goes back into history.
    Scroll(i32),
}

/// The pane state a wheel plan reads: its published pointer modes and whether READONLY holds.
#[derive(Debug, Clone, Copy)]
pub(super) struct WheelTarget {
    /// The pane's mouse tracking mode.
    pub(super) tracking: MouseTracking,
    /// The pane shows its alternate screen.
    pub(super) alt_screen: bool,
    /// Reports use SGR encoding rather than X10.
    pub(super) sgr: bool,
    /// Cursor keys use application mode (`ESC O`) rather than `ESC [`.
    pub(super) application_cursor: bool,
    /// The pane's window is in READONLY, so the wheel may not send input.
    pub(super) readonly: bool,
    /// The cell under the pointer, 0-based (row, column); reports fall back to the first cell.
    pub(super) cell: Option<(u16, u16)>,
}

/// Lines `delta` moves at a cell height of `cell_height_px`, rounded away from zero so a small
/// trackpad nudge still moves one line. Negative lines scroll back into history, the opposite
/// of winit's sign.
pub(super) fn wheel_lines(delta: MouseScrollDelta, cell_height_px: f32) -> i32 {
    let lines = match delta {
        MouseScrollDelta::LineDelta(_, vertical_ticks) => -vertical_ticks * LINES_PER_TICK,
        MouseScrollDelta::PixelDelta(position) => -(position.y as f32) / cell_height_px,
    };
    if lines >= 0.0 {
        lines.ceil() as i32
    } else {
        // When: `lines` is negative, rounding goes away from zero too, so an upward nudge moves.
        lines.floor() as i32
    }
}

/// Plan one wheel event of `lines` for `target`. READONLY keeps the wheel local even when the
/// pane asked for reports or shows an alternate screen.
pub(super) fn plan_wheel(lines: i32, target: WheelTarget) -> WheelPlan {
    if lines == 0 {
        // When: `lines` is zero, the motion moves nothing.
        return WheelPlan::None;
    }
    let route = if target.readonly {
        // READONLY: no new wheel gesture may produce reports or keys.
        WheelRoute::LocalScrollback
    } else {
        // When: not `readonly`, the pane's tracking mode and screen choose the route.
        wheel_route(target.tracking, target.alt_screen)
    };
    let up = lines < 0;
    let count = lines.unsigned_abs() as usize;
    match route {
        WheelRoute::MouseReport => {
            let (row, col) = target.cell.unwrap_or((0, 0));
            let (col1, row1) = (u32::from(col) + 1, u32::from(row) + 1);
            WheelPlan::Input(wheel_report_bytes(target.sgr, up, col1, row1, count))
        }
        WheelRoute::CursorKeys => {
            let sequence: &[u8] = match (target.application_cursor, up) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1bOB",
                (false, true) => b"\x1b[A",
                (false, false) => b"\x1b[B",
            };
            WheelPlan::Input(sequence.repeat(count))
        }
        WheelRoute::LocalScrollback => WheelPlan::Scroll(lines),
    }
}

#[cfg(test)]
#[path = "wheel_plan_tests.rs"]
mod wheel_plan_tests;
