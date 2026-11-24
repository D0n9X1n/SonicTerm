//! Mouse report encodings: one function per DECSET format, used by every pointer, wheel and
//! secondary-button report, the pane-local pixel position 1016 reports, and the queueing of
//! motion under the profile it was encoded in.

use sonicterm_ui::pane::Rect;
use sonicterm_vt::vt::MouseFormat;
use winit::window::WindowId;

use super::input_dispatch::MotionProfile;
use super::{App, PtyInputSource, WindowState};

/// One mouse event to encode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::app) struct MouseReport {
    /// The xterm button code with the motion bit (32) but without modifier bits: 0 left, 1 middle,
    /// 2 right, 35 no button, 64/65 wheel.
    pub(in crate::app) button: u32,
    /// The modifier bits: Shift 4, Alt or Super 8, Control 16.
    pub(in crate::app) modifiers: u32,
    /// The button was released; X10, 1005 and 1015 then report legacy button 3.
    pub(in crate::app) release: bool,
    /// The zero-based cell, as (row, column).
    pub(in crate::app) cell: (u16, u16),
    /// The zero-based physical pixel from the pane's cell-area origin, as (x, y); signed and
    /// unclamped, so a drag past the pane's edge reports its true offset.
    pub(in crate::app) pixel: (i32, i32),
}

/// The largest zero-based coordinate 1005 sends: `2014 + 33 = 0x7FF`, the last value UTF-8 writes
/// in two bytes, the range xterm and WezTerm decode.
const MAX_UTF8_COORDINATE_INDEX: u32 = 2014;

/// The bytes of `report` in `format`.
pub(in crate::app) fn encode_mouse_report(format: MouseFormat, report: MouseReport) -> Vec<u8> {
    let code = report.button + report.modifiers;
    // X10, 1005 and 1015 name no button in a release: every release is button 3.
    let legacy_code = match report.release {
        true => 3 + report.modifiers,
        false => code,
    };
    let (row, col) = (u32::from(report.cell.0), u32::from(report.cell.1));
    let terminator = if report.release { 'm' } else { 'M' };
    match format {
        MouseFormat::X10 => vec![
            0x1b,
            b'[',
            b'M',
            (legacy_code + 32).min(255) as u8,
            ((col + 1).min(223) + 32) as u8,
            ((row + 1).min(223) + 32) as u8,
        ],
        MouseFormat::Utf8 => {
            // No reported code reaches 96, so `Cb + 32` is one byte, the same as its UTF-8 form.
            let mut bytes = vec![0x1b, b'[', b'M', (legacy_code + 32).min(127) as u8];
            for index in [col, row] {
                let value = index.min(MAX_UTF8_COORDINATE_INDEX) + 33;
                let mut buffer = [0; 4];
                // Every value from 33 to 0x7FF is a scalar value, so the conversion always succeeds.
                if let Some(character) = char::from_u32(value) {
                    bytes.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
                }
            }
            bytes
        }
        MouseFormat::Urxvt => {
            format!("\x1b[{};{};{}M", legacy_code + 32, col + 1, row + 1).into_bytes()
        }
        MouseFormat::Sgr => {
            format!("\x1b[<{code};{};{}{terminator}", col + 1, row + 1).into_bytes()
        }
        MouseFormat::SgrPixels => {
            let (pixel_x, pixel_y) = report.pixel;
            format!("\x1b[<{code};{pixel_x};{pixel_y}{terminator}").into_bytes()
        }
    }
}

/// The pointer's position in `window` from the cell-area origin of `pane_id`, in whole physical
/// pixels: the renderer's laid-out origin, or without a renderer the pane rect `headless_rects`
/// gives. A pane no frame has laid out yet measures from the window's origin.
pub(in crate::app) fn pane_pixel_in(
    window: &WindowState,
    pane_id: u64,
    headless_rects: impl FnOnce() -> Vec<(u64, Rect)>,
) -> (i32, i32) {
    let origin = match window.renderer.as_ref() {
        Some(renderer) => renderer.pane_cell_origin(pane_id),
        None => headless_rects()
            .into_iter()
            .find(|(id, _)| *id == pane_id)
            .map(|(_, rect)| (rect.x, rect.y)),
    };
    // shortcut: a pane no frame laid out yet measures from the window origin; resolve it from the
    // collector's layout if a program needs exact pixels before a pane's first frame.
    let (origin_x, origin_y) = origin.unwrap_or((0.0, 0.0));
    (
        (window.cursor_pos.0 - f64::from(origin_x)).round() as i32,
        (window.cursor_pos.1 - f64::from(origin_y)).round() as i32,
    )
}

impl App {
    /// The pointer's pane-local pixel for `pane_id` in window `window_id`; (0, 0) for a closed
    /// window.
    pub(in crate::app) fn pane_pixel(&self, window_id: WindowId, pane_id: u64) -> (i32, i32) {
        let Some(window) = self.registry.windows.get(&window_id) else {
            // When: window_id names no live window, there is no pointer to measure.
            return (0, 0);
        };
        if Some(window_id) == self.registry.main_window_id {
            pane_pixel_in(window, pane_id, || self.compute_active_pane_rects())
        } else {
            // When: window_id is a child window, its own layout places the pane.
            pane_pixel_in(window, pane_id, || App::compute_pane_rects_for(window))
        }
    }

    /// The pointer's pixel from the origin of the pane a held left or secondary press belongs to,
    /// wherever the pointer is, so a drag out of the pane reports negative or past-edge pixels.
    pub(in crate::app) fn held_press_pixel(&self, window_id: WindowId) -> (i32, i32) {
        let press_pane = self.registry.windows.get(&window_id).and_then(|window| {
            window
                .secondary_press
                .map(|press| press.pane_id)
                .or(window.pointer_gesture.map(|gesture| gesture.press_pane))
        });
        press_pane.map_or((0, 0), |pane_id| self.pane_pixel(window_id, pane_id))
    }

    /// Queue one held-button motion report for `pane_id` under `profile`, the press-time tracking,
    /// format and screen it was encoded for. It is dropped when the pane's published modes no
    /// longer match: the program reads new motion under its new modes, and only the release keeps
    /// the press-time format. The writer checks the same profile again before admitting it.
    pub(in crate::app) fn write_pointer_motion(
        &mut self,
        pane_id: u64,
        profile: MotionProfile,
        bytes: Vec<u8>,
    ) {
        let Some(modes) = self.registry.pane_mut(pane_id).map(|pane| pane.pointer_modes()) else {
            // When: pane_id names no live pane, the motion has no destination.
            return;
        };
        if MotionProfile::of(modes, profile.held) != profile {
            // When: the published modes no longer match the profile, the program would misread it.
            return;
        }
        self.queue_pointer_motion(pane_id, profile, bytes);
    }

    /// Queue one motion report for `pane_id` under `profile`, the modes it was encoded with; the
    /// modes are never read again here, so a switch published after encoding cannot retag it. The
    /// writer admits it only while the parser's live modes still match the profile. In 1016 a
    /// report identical to the last one the writer admitted under the same profile is dropped.
    pub(in crate::app) fn queue_pointer_motion(
        &mut self,
        pane_id: u64,
        profile: MotionProfile,
        bytes: Vec<u8>,
    ) {
        #[cfg(test)]
        if let Some(hook) = MOTION_QUEUE_HOOK.take() {
            hook();
        }
        if !profile.reports_motion() {
            // When: the profile's tracking reports no such motion, the program would misread it.
            return;
        }
        self.log_test_write(pane_id, &bytes, PtyInputSource::PointerMotion);
        if let Some(pane) = self.registry.pane_mut(pane_id).filter(|pane| pane.pty.is_some()) {
            // The pane has a writer, so the report waits in its coalescing slot for admission.
            pane.pending_pointer_motion.offer(profile, &bytes);
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Run once where an encoded motion report reaches the queue, so a test can publish a mode
    /// change after the route read the modes it encoded under.
    pub(in crate::app) static MOTION_QUEUE_HOOK: std::cell::Cell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
#[path = "mouse_report_tests.rs"]
mod mouse_report_tests;
