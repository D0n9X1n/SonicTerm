//! Mouse report encodings: one function per DECSET format, used by every pointer, wheel and
//! secondary-button report, the pane-local pixel position 1016 reports, and the rule that drops
//! an identical 1016 motion.

use sonicterm_ui::pane::Rect;
use sonicterm_vt::vt::MouseFormat;
use winit::window::WindowId;

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

    /// Send one motion report to `pane_id`, encoded in `format`. In 1016 a report identical to the
    /// last 1016 motion sent to that pane under the same tracking mode is dropped: its bytes carry
    /// the pixel, the held buttons and the modifiers, so any change among them is still sent.
    pub(in crate::app) fn write_pointer_motion(
        &mut self,
        pane_id: u64,
        format: MouseFormat,
        bytes: Vec<u8>,
    ) {
        let Some(pane) = self.registry.pane_mut(pane_id) else {
            // When: pane_id names no live pane, the motion has no destination.
            return;
        };
        let key = (format == MouseFormat::SgrPixels)
            .then(|| (pane.pointer_modes().tracking(), bytes.clone()));
        if key.is_some() && pane.last_pixel_motion == key {
            // When: key repeats the last 1016 motion under the same tracking mode, it says nothing new.
            return;
        }
        // A cell-format motion clears the key, so a 1016 report is compared only with 1016 reports.
        pane.last_pixel_motion = key;
        self.write_to_pane(pane_id, bytes, PtyInputSource::PointerMotion);
    }
}

#[cfg(test)]
#[path = "mouse_report_tests.rs"]
mod mouse_report_tests;
