//! Right and middle mouse buttons reported to a pane whose program tracks the mouse, such as
//! herdr's right-click menu. A press belongs to one pane until its release, kept apart from the
//! left-button gesture, so neither button's release ever ends the other's press.

use super::input_dispatch::MotionProfile;
use super::*;
use sonicterm_vt::vt::MouseFormat;
use winit::event::{ElementState, MouseButton};

/// The xterm mouse-report modifier bits: Shift 4, Alt or Super 8, Control 16.
pub(crate) fn pointer_modifier_bits(modifiers: ModifiersState) -> u32 {
    u32::from(modifiers.shift_key()) * 4
        + u32::from(modifiers.alt_key() || modifiers.super_key()) * 8
        + u32::from(modifiers.control_key()) * 16
}

/// A mouse button other than the left one that a program may receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SecondaryButton {
    /// The middle button, report code 1.
    Middle,
    /// The right button, report code 2.
    Right,
}

impl SecondaryButton {
    /// The secondary button a winit button names; `None` for the left and any other button.
    pub(crate) fn from_winit(button: MouseButton) -> Option<Self> {
        match button {
            MouseButton::Middle => Some(Self::Middle),
            MouseButton::Right => Some(Self::Right),
            _ => None,
        }
    }

    /// The xterm button code this button reports.
    fn code(self) -> u32 {
        match self {
            Self::Middle => 1,
            Self::Right => 2,
        }
    }
}

/// A held right or middle press reported to `pane_id`, which owns its motion and release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SecondaryPress {
    pub(crate) button: SecondaryButton,
    pub(crate) pane_id: u64,
    /// The report format at the press; its motion and release use the same format.
    pub(crate) format: MouseFormat,
    /// The tracking mode at the press; held motion is reported only under 1002 and 1003.
    pub(crate) tracking: MouseTracking,
    /// Whether the alternate screen showed at the press; held motion is stamped with it.
    pub(crate) is_alt: bool,
    /// The last cell reported, as (row, column).
    pub(crate) cell: (u16, u16),
    /// The last pixel reported, from the press pane's cell-area origin.
    pub(crate) pixel: (i32, i32),
}

/// Which report a secondary button sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SecondaryEvent {
    Press,
    Release,
    HeldMotion,
}

/// Encode one secondary-button report at zero-based cell `cell` (row, column) and pane pixel
/// `pixel` in `format`.
pub(crate) fn secondary_report_bytes(
    format: MouseFormat,
    event: SecondaryEvent,
    button: SecondaryButton,
    modifiers: ModifiersState,
    cell: (u16, u16),
    pixel: (i32, i32),
) -> Vec<u8> {
    let report = super::mouse_report::MouseReport {
        button: match event {
            SecondaryEvent::Press | SecondaryEvent::Release => button.code(),
            SecondaryEvent::HeldMotion => 32 + button.code(),
        },
        modifiers: pointer_modifier_bits(modifiers),
        release: event == SecondaryEvent::Release,
        cell,
        pixel,
    };
    super::mouse_report::encode_mouse_report(format, report)
}

impl WindowState {
    /// Report motion while a secondary press is held: `None` when none is held (the caller goes
    /// on); otherwise the press owns the motion, and `Some(report)` carries the held-motion report
    /// under 1002 or 1003 with its press-time profile, or nothing under 1000. Motion over another
    /// pane keeps the last cell; `press_pixel` is the pointer measured from the press pane's origin.
    pub(in crate::app) fn secondary_motion(
        &mut self,
        cell: Option<PointerCell>,
        press_pixel: (i32, i32),
    ) -> Option<Option<(u64, MotionProfile, Vec<u8>)>> {
        let modifiers = self.modifiers;
        let press = self.secondary_press.as_mut()?;
        if let Some(cell) = cell.filter(|cell| cell.pane_id == press.pane_id) {
            press.cell = (cell.row, cell.col);
        }
        press.pixel = press_pixel;
        let held = matches!(press.tracking, MouseTracking::ButtonMotion | MouseTracking::AnyMotion);
        Some(held.then(|| {
            let bytes = secondary_report_bytes(
                press.format,
                SecondaryEvent::HeldMotion,
                press.button,
                modifiers,
                press.cell,
                press.pixel,
            );
            let profile = MotionProfile {
                tracking: press.tracking,
                format: press.format,
                is_alt: press.is_alt,
                held: true,
            };
            (press.pane_id, profile, bytes)
        }))
    }

    /// End a held secondary press: its release report for its pane, or `None` when none is held.
    pub(crate) fn take_secondary_release(&mut self) -> Option<(u64, Vec<u8>)> {
        let press = self.secondary_press.take()?;
        let bytes = secondary_report_bytes(
            press.format,
            SecondaryEvent::Release,
            press.button,
            self.modifiers,
            press.cell,
            press.pixel,
        );
        Some((press.pane_id, bytes))
    }
}

impl App {
    /// Route a right or middle button event in `win_id` to the pane under the pointer. A press is
    /// reported only to a pane whose program tracks the mouse, outside READONLY and without Shift,
    /// and only while no other secondary press is held; a release ends only the press of the same
    /// button. Reports go to that one pane, never to broadcast receivers.
    pub(in crate::app) fn route_secondary_button(
        &mut self,
        win_id: WindowId,
        button: SecondaryButton,
        state: ElementState,
    ) {
        if state == ElementState::Released {
            // When: `state` is a release, it ends only a held press of the same button.
            let release = self
                .registry
                .windows
                .get_mut(&win_id)
                .filter(|window| window.secondary_press.is_some_and(|press| press.button == button))
                .and_then(WindowState::take_secondary_release);
            if let Some((pane_id, bytes)) = release {
                self.write_to_pane(pane_id, bytes, PtyInputSource::PointerButton);
            }
            return;
        }
        let Some(press) = self.secondary_press_at(win_id, button) else {
            // When: no pane under the pointer takes this press, it does nothing.
            return;
        };
        let modifiers =
            self.registry.windows.get(&win_id).map(|window| window.modifiers).unwrap_or_default();
        let bytes = secondary_report_bytes(
            press.format,
            SecondaryEvent::Press,
            button,
            modifiers,
            press.cell,
            press.pixel,
        );
        if let Some(window) = self.registry.windows.get_mut(&win_id) {
            window.secondary_press = Some(press);
        }
        self.write_to_pane(press.pane_id, bytes, PtyInputSource::PointerButton);
    }

    /// The press `button` would start in `win_id`, or `None` when it reaches no tracking pane.
    fn secondary_press_at(
        &self,
        win_id: WindowId,
        button: SecondaryButton,
    ) -> Option<SecondaryPress> {
        let window = self.registry.windows.get(&win_id)?;
        let read_only = window.copy_mode.as_ref().is_some_and(CopyModeState::is_read_only);
        if window.secondary_press.is_some()
            || window.mouse_down
            || read_only
            || window.modifiers.shift_key()
        {
            // When: a `secondary_press` or the left button (`mouse_down`) is held, `read_only` is
            // on, or Shift asks for local handling, no report starts: the first button owns.
            return None;
        }
        let (cursor_x, cursor_y) = (window.cursor_pos.0 as f32, window.cursor_pos.1 as f32);
        let (pane_id, row, col) = if self.registry.main_window_id == Some(win_id) {
            self.main_pane_cell_at(cursor_x, cursor_y)?
        } else {
            // When: `win_id` is a torn-out window, it lays out its own panes.
            super::child_window_pointer::child_pane_cell_at(window, cursor_x, cursor_y)?
        };
        let modes = window.panes.get(&pane_id)?.pointer_modes();
        let tracking = modes.tracking();
        (tracking != MouseTracking::Off).then_some(SecondaryPress {
            button,
            pane_id,
            format: modes.format(),
            tracking,
            is_alt: modes.is_alt(),
            cell: (row, col),
            pixel: self.pane_pixel(win_id, pane_id),
        })
    }
}

#[cfg(test)]
#[path = "secondary_button_tests.rs"]
mod secondary_button_tests;
