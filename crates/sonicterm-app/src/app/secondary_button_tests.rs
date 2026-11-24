use std::sync::atomic::Ordering;

use super::*;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use sonicterm_vt::vt::PointerModes;
use winit::dpi::PhysicalPosition;

/// Each report's bytes: SGR button 1 (middle) and 2 (right) with modifiers, `m` for a release,
/// 32 more while held; X10 adds 32 to every field and releases as button 3.
#[test]
fn secondary_reports_encode_like_xterm() {
    let at = (4, 9);
    let right = SecondaryButton::Right;
    let middle = SecondaryButton::Middle;
    let none = ModifiersState::empty();
    assert_eq!(
        secondary_report_bytes(
            sonicterm_vt::vt::MouseFormat::Sgr,
            SecondaryEvent::Press,
            right,
            none,
            at,
            (0, 0)
        ),
        b"\x1b[<2;10;5M"
    );
    assert_eq!(
        secondary_report_bytes(
            sonicterm_vt::vt::MouseFormat::Sgr,
            SecondaryEvent::Release,
            right,
            none,
            at,
            (0, 0)
        ),
        b"\x1b[<2;10;5m"
    );
    assert_eq!(
        secondary_report_bytes(
            sonicterm_vt::vt::MouseFormat::Sgr,
            SecondaryEvent::HeldMotion,
            right,
            none,
            at,
            (0, 0)
        ),
        b"\x1b[<34;10;5M"
    );
    assert_eq!(
        secondary_report_bytes(
            sonicterm_vt::vt::MouseFormat::Sgr,
            SecondaryEvent::Press,
            middle,
            ModifiersState::CONTROL,
            at,
            (0, 0)
        ),
        b"\x1b[<17;10;5M"
    );
    assert_eq!(
        secondary_report_bytes(
            sonicterm_vt::vt::MouseFormat::X10,
            SecondaryEvent::Press,
            right,
            none,
            at,
            (0, 0)
        ),
        [0x1b, b'[', b'M', 34, 42, 37]
    );
    assert_eq!(
        secondary_report_bytes(
            sonicterm_vt::vt::MouseFormat::X10,
            SecondaryEvent::Release,
            right,
            none,
            at,
            (0, 0)
        ),
        [0x1b, b'[', b'M', 35, 42, 37]
    );
}

/// A headless app with a main window and a torn-out child, each with one pane on a 10x20-pixel
/// cell grid whose program tracks the mouse with `tracking`, and the write ledger on.
fn tracking_app(tracking: MouseTracking) -> (App, Vec<(WindowId, u64)>) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let main_pane = app.__test_seed_tab("main");
    let main = app.registry.main_window_id.expect("main window");
    let child = app.__test_seed_child_window(&["child"]);
    let outer = sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 400.0);
    assert!(app.__test_set_main_pane_viewport(outer, 10.0, 20.0));
    assert!(app.__test_set_child_pane_viewport(child, outer, 10.0, 20.0));
    let child_pane = app.__test_child_active_pane(child).expect("child pane");
    let modes = PointerModes::new(tracking, sonicterm_vt::vt::MouseFormat::Sgr, false, false);
    for window in app.registry.windows.values() {
        for pane in window.panes.values() {
            // Ordering: pointer_input stores Relaxed; this thread is the handler's only reader.
            pane.pointer_input.store(modes.bits(), Ordering::Relaxed);
        }
    }
    app.__test_enable_pty_write_log();
    (app, vec![(main, main_pane), (child, child_pane)])
}

/// Put `window_id`'s pointer at pixel (`x_px`, `y_px`).
fn point(app: &mut App, window_id: WindowId, x_px: f64, y_px: f64) {
    app.registry.windows.get_mut(&window_id).unwrap().cursor_pos = (x_px, y_px);
}

/// A right press and release in either window reach that window's pane through the shared
/// pointer route, at the pointed cell, and nothing else is written.
#[test]
fn right_click_reaches_the_tracking_pane_in_either_window() {
    let (mut app, windows) = tracking_app(MouseTracking::Button);
    for (window_id, pane_id) in windows {
        point(&mut app, window_id, 35.0, 45.0);
        let press = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Right,
        };
        assert_eq!(app.route_shared_pointer(window_id, &press), Some(false));
        let release = WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Right,
        };
        assert_eq!(app.route_shared_pointer(window_id, &release), Some(false));
        assert_eq!(
            app.__test_drain_pty_writes(),
            vec![(pane_id, b"\x1b[<2;4;3M".to_vec()), (pane_id, b"\x1b[<2;4;3m".to_vec())]
        );
    }
}

/// With tracking off, in READONLY, or with Shift held, a right press is not reported and holds
/// nothing, so its release reports nothing either.
#[test]
fn right_click_stays_local_without_tracking_readonly_or_with_shift() {
    let (mut app, windows) = tracking_app(MouseTracking::Off);
    let (main, _) = windows[0];
    app.route_secondary_button(main, SecondaryButton::Right, ElementState::Pressed);
    app.route_secondary_button(main, SecondaryButton::Right, ElementState::Released);
    assert!(app.__test_drain_pty_writes().is_empty(), "tracking off");

    let (mut app, windows) = tracking_app(MouseTracking::Button);
    let (main, _) = windows[0];
    app.registry.windows.get_mut(&main).unwrap().copy_mode =
        Some(CopyModeState::read_only_at((0, 0)));
    app.route_secondary_button(main, SecondaryButton::Right, ElementState::Pressed);
    assert!(app.__test_drain_pty_writes().is_empty(), "READONLY");
    app.registry.windows.get_mut(&main).unwrap().copy_mode = None;
    app.registry.windows.get_mut(&main).unwrap().modifiers = ModifiersState::SHIFT;
    app.route_secondary_button(main, SecondaryButton::Right, ElementState::Pressed);
    assert!(app.__test_drain_pty_writes().is_empty(), "Shift");
    assert!(app.registry.windows[&main].secondary_press.is_none());
}

/// While a right press is held, a middle press is ignored, the middle release and a left-button
/// release end nothing, and only the right release ends it.
#[test]
fn the_first_secondary_press_owns_its_release() {
    let (mut app, windows) = tracking_app(MouseTracking::Button);
    let (main, pane_id) = windows[0];
    point(&mut app, main, 35.0, 45.0);
    app.route_secondary_button(main, SecondaryButton::Right, ElementState::Pressed);
    app.route_secondary_button(main, SecondaryButton::Middle, ElementState::Pressed);
    app.route_secondary_button(main, SecondaryButton::Middle, ElementState::Released);
    assert_eq!(app.__test_drain_pty_writes(), vec![(pane_id, b"\x1b[<2;4;3M".to_vec())]);
    assert!(app.registry.windows[&main].pointer_gesture.is_none(), "the left gesture is separate");
    assert!(app.registry.windows[&main].secondary_press.is_some(), "still held");
    app.route_secondary_button(main, SecondaryButton::Right, ElementState::Released);
    assert_eq!(app.__test_drain_pty_writes(), vec![(pane_id, b"\x1b[<2;4;3m".to_vec())]);
}

/// Motion with a right press held reports `32 + 2` at the new cell under 1002 and 1003, and
/// nothing under 1000; the press owns the motion either way.
#[test]
fn held_right_motion_follows_the_tracking_mode() {
    for (tracking, reported) in [
        (MouseTracking::Button, false),
        (MouseTracking::ButtonMotion, true),
        (MouseTracking::AnyMotion, true),
    ] {
        let (mut app, windows) = tracking_app(tracking);
        for (window_id, pane_id) in windows {
            point(&mut app, window_id, 35.0, 45.0);
            app.route_secondary_button(window_id, SecondaryButton::Right, ElementState::Pressed);
            let _ = app.__test_drain_pty_writes();
            if app.registry.main_window_id == Some(window_id) {
                app.handle_main_cursor_moved(PhysicalPosition::new(55.0, 85.0));
            } else {
                app.handle_child_cursor_moved(
                    window_id,
                    PhysicalPosition::new(55.0, 85.0),
                    &Config::default(),
                );
            }
            let expected =
                if reported { vec![(pane_id, b"\x1b[<34;6;5M".to_vec())] } else { Vec::new() };
            assert_eq!(app.__test_drain_pty_writes(), expected, "{tracking:?}");
            app.route_secondary_button(window_id, SecondaryButton::Right, ElementState::Released);
            assert_eq!(app.__test_drain_pty_writes(), vec![(pane_id, b"\x1b[<2;6;5m".to_vec())]);
        }
    }
}

/// Losing focus while a right press is held sends its release, since the native button-up may
/// never arrive.
#[test]
fn focus_loss_releases_a_held_right_press() {
    let (mut app, windows) = tracking_app(MouseTracking::Button);
    let (main, pane_id) = windows[0];
    point(&mut app, main, 35.0, 45.0);
    app.route_secondary_button(main, SecondaryButton::Right, ElementState::Pressed);
    let _ = app.__test_drain_pty_writes();
    app.handle_window_focus_changed(main, false);
    let writes = app.__test_drain_pty_writes();
    assert!(writes.contains(&(pane_id, b"\x1b[<2;4;3m".to_vec())), "{writes:?}");
    assert!(app.registry.windows[&main].secondary_press.is_none());
}

/// The first button owns the pointer in either window: with a right press held a left press is
/// consumed before either window handler and starts no gesture, and with the left button down a
/// right press reports nothing and holds nothing.
#[test]
fn a_held_button_refuses_the_other_in_either_window() {
    let (mut app, windows) = tracking_app(MouseTracking::Button);
    let button = |state, button| WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state,
        button,
    };
    for (window_id, pane_id) in windows {
        point(&mut app, window_id, 35.0, 45.0);
        let right_press = button(ElementState::Pressed, MouseButton::Right);
        assert_eq!(app.route_shared_pointer(window_id, &right_press), Some(false));
        let left_press = button(ElementState::Pressed, MouseButton::Left);
        assert_eq!(app.route_shared_pointer(window_id, &left_press), Some(false), "left consumed");
        assert!(!app.registry.windows[&window_id].mouse_down, "no left gesture starts");
        let right_release = button(ElementState::Released, MouseButton::Right);
        assert_eq!(app.route_shared_pointer(window_id, &right_release), Some(false));
        assert_eq!(
            app.__test_drain_pty_writes(),
            vec![(pane_id, b"\x1b[<2;4;3M".to_vec()), (pane_id, b"\x1b[<2;4;3m".to_vec())]
        );

        app.registry.windows.get_mut(&window_id).unwrap().mouse_down = true;
        assert_eq!(app.route_shared_pointer(window_id, &right_press), Some(false));
        assert!(app.registry.windows[&window_id].secondary_press.is_none(), "left owns");
        assert!(app.__test_drain_pty_writes().is_empty());
        app.registry.windows.get_mut(&window_id).unwrap().mouse_down = false;
    }
}
