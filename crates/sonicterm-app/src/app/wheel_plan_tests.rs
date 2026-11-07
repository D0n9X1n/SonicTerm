//! Pins the shared wheel rules: rounding, each route, READONLY, and report coordinates.

use sonicterm_vt::vt::MouseTracking;
use winit::dpi::PhysicalPosition;
use winit::event::MouseScrollDelta;

use super::{plan_wheel, wheel_lines, WheelPlan, WheelTarget};

/// A pane with no tracking on the primary screen, pointing at row 4, column 9.
fn primary() -> WheelTarget {
    WheelTarget {
        tracking: MouseTracking::Off,
        alt_screen: false,
        sgr: false,
        application_cursor: false,
        readonly: false,
        cell: Some((4, 9)),
    }
}

/// A notch moves three lines; a tiny pixel nudge either way still moves one, away from zero.
#[test]
fn wheel_motion_rounds_away_from_zero() {
    assert_eq!(wheel_lines(MouseScrollDelta::LineDelta(0.0, 1.0), 16.0), -3);
    assert_eq!(wheel_lines(MouseScrollDelta::LineDelta(0.0, -1.0), 16.0), 3);
    let nudge_up = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 0.5));
    let nudge_down = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -0.5));
    assert_eq!(wheel_lines(nudge_up, 16.0), -1);
    assert_eq!(wheel_lines(nudge_down, 16.0), 1);
    assert_eq!(wheel_lines(MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 0.0)), 16.0), 0);
}

/// An untracked primary screen scrolls locally; zero lines does nothing.
#[test]
fn an_untracked_primary_screen_scrolls_locally() {
    assert_eq!(plan_wheel(-3, primary()), WheelPlan::Scroll(-3));
    assert_eq!(plan_wheel(0, primary()), WheelPlan::None);
}

/// An untracked alternate screen gets one arrow per line, in the pane's cursor-key mode.
#[test]
fn an_untracked_alternate_screen_gets_arrows() {
    let alt = WheelTarget { alt_screen: true, ..primary() };
    assert_eq!(plan_wheel(-2, alt), WheelPlan::Input(b"\x1b[A\x1b[A".to_vec()));
    let application = WheelTarget { application_cursor: true, ..alt };
    assert_eq!(plan_wheel(1, application), WheelPlan::Input(b"\x1bOB".to_vec()));
}

/// Tracking sends one report per line at the hovered cell, 1-based, in SGR or X10.
#[test]
fn tracking_reports_carry_the_hovered_cell() {
    let sgr = WheelTarget { tracking: MouseTracking::Button, sgr: true, ..primary() };
    assert_eq!(plan_wheel(-1, sgr), WheelPlan::Input(b"\x1b[<64;10;5M".to_vec()));
    let x10 = WheelTarget { sgr: false, ..sgr };
    assert_eq!(plan_wheel(2, x10), WheelPlan::Input([0x1b, b'[', b'M', 97, 42, 37].repeat(2)));
    let nowhere = WheelTarget { cell: None, ..sgr };
    assert_eq!(plan_wheel(-1, nowhere), WheelPlan::Input(b"\x1b[<64;1;1M".to_vec()));
}

/// READONLY keeps the wheel local even when the pane asked for reports or shows an alternate
/// screen.
#[test]
fn readonly_keeps_the_wheel_local() {
    let tracked = WheelTarget { tracking: MouseTracking::Button, readonly: true, ..primary() };
    assert_eq!(plan_wheel(-3, tracked), WheelPlan::Scroll(-3));
    let alt = WheelTarget { alt_screen: true, readonly: true, ..primary() };
    assert_eq!(plan_wheel(3, alt), WheelPlan::Scroll(3));
}
