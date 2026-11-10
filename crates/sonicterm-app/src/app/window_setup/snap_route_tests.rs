//! Contracts for the snap layouts routing: which spot is the maximise button, what each message
//! does, the leave sequences that must never leave a hover stuck, and signed point packing.

use super::*;

/// A caption band at scale 1, 32 px tall: in an 800 px client area the buttons are minimise
/// 662..708, maximise 708..754 and close 754..800.
fn target() -> SnapTarget {
    SnapTarget::of(WindowBand::caption(), 32.0, 1.0).expect("a caption band has a target")
}

/// The maximise rect is exactly the App's maximise button: its centre and edges are on it; the
/// neighbouring buttons, the band below it and the terminal are not.
#[test]
fn only_the_maximise_button_is_the_snap_spot() {
    let target = target();
    assert!(target.on_maximize(800.0, 731.0, 16.0), "centre");
    assert!(target.on_maximize(800.0, 708.0, 0.0), "top-left corner");
    assert!(!target.on_maximize(800.0, 707.0, 16.0), "minimise");
    assert!(!target.on_maximize(800.0, 754.0, 16.0), "close");
    assert!(!target.on_maximize(800.0, 731.0, 32.0), "below the band");
    assert!(!target.on_maximize(800.0, 100.0, 16.0), "band left of the buttons");
    // A wider client area (a maximised window) moves the button with the right edge.
    assert!(target.on_maximize(1600.0, 1531.0, 16.0));
    // At scale 2 the buttons are 92 px wide: maximise is 1416..1508 of 1600.
    let scaled = SnapTarget::of(WindowBand::caption(), 64.0, 2.0).unwrap();
    assert!(scaled.on_maximize(1600.0, 1420.0, 60.0) && !scaled.on_maximize(1600.0, 1410.0, 60.0));
}

/// Bands without caption buttons have no target, so the subclass passes everything through:
/// a native titlebar, full screen and macOS's traffic lights.
#[test]
fn bands_without_caption_buttons_have_no_target() {
    assert!(SnapTarget::of(WindowBand::default(), 32.0, 1.0).is_none(), "native");
    assert!(SnapTarget::of(WindowBand::buttonless(), 32.0, 1.0).is_none(), "full screen");
    let lights = WindowBand::traffic_lights(14.0, 68.0);
    assert!(SnapTarget::of(lights, 28.0, 1.0).is_none(), "macOS");
}

/// Every non-client message about the maximise button routes to its action; the same messages
/// for another hit code, and unrelated messages, pass through untouched.
#[test]
fn non_client_messages_on_the_button_route_to_hover_press_and_swallow() {
    let idle = SnapTracking::default();
    let on = |message| route(message, true, Spot::Maximize, false, idle).0;
    let off = |message| route(message, false, Spot::Client, false, idle).0;
    assert_eq!(on(WM_NCHITTEST), SnapAction::HitTest);
    assert_eq!(off(WM_NCHITTEST), SnapAction::HitTest, "hit tests always ask the default first");
    assert_eq!(on(WM_NCMOUSEMOVE), SnapAction::Hover);
    assert_eq!(on(WM_NCLBUTTONDOWN), SnapAction::Press);
    assert_eq!(on(WM_NCLBUTTONDBLCLK), SnapAction::Press, "a fast second click is a press too");
    assert_eq!(on(WM_NCLBUTTONUP), SnapAction::Swallow, "the system never runs its own maximise");
    for message in [WM_NCMOUSEMOVE, WM_NCLBUTTONDOWN, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONUP] {
        assert_eq!(off(message), SnapAction::Pass, "another hit code: {message:#x}");
    }
    assert_eq!(on(0x0201), SnapAction::Pass, "a client press is winit's own");
    assert_eq!(off(WM_MOUSEMOVE), SnapAction::Pass, "a move with nothing lost");
    assert_eq!(off(WM_NCMOUSELEAVE), SnapAction::Pass, "a non-client leave with nothing lost");
}

/// Client → button → client: the client leave over the button is swallowed (no CursorLeft, so
/// the hover cannot flicker), and the first real client move re-arms winit's tracking. A later
/// non-client leave then does nothing, so winit sees no stray leave.
#[test]
fn returning_to_the_client_area_rearms_and_ignores_the_late_non_client_leave() {
    let (action, tracking) =
        route(WM_MOUSELEAVE, false, Spot::Maximize, false, SnapTracking::default());
    assert_eq!((action, tracking.lost_leave), (SnapAction::SwallowLeave, true));
    let (action, tracking) = route(WM_MOUSEMOVE, false, Spot::Client, false, tracking);
    assert_eq!((action, tracking.lost_leave), (SnapAction::RearmClient, false));
    let (action, tracking) = route(WM_NCMOUSELEAVE, false, Spot::Client, false, tracking);
    assert_eq!((action, tracking.lost_leave), (SnapAction::Pass, false));
}

/// Button → anywhere else, non-client leave first: winit gets its leave whatever the geometry,
/// including the flyout or another window covering our client area, so the hover cannot stick;
/// the client move that follows passes through and re-enters winit normally.
#[test]
fn leaving_the_button_hands_winit_its_leave_whatever_is_under_the_cursor() {
    let lost = SnapTracking { lost_leave: true };
    for spot in [Spot::Outside, Spot::Client, Spot::Maximize] {
        let (action, tracking) = route(WM_NCMOUSELEAVE, false, spot, false, lost);
        assert_eq!((action, tracking.lost_leave), (SnapAction::ForwardLeave, false), "{spot:?}");
        let (action, _) = route(WM_MOUSEMOVE, false, Spot::Client, false, tracking);
        assert_eq!(action, SnapAction::Pass, "the next move re-enters winit itself");
    }
}

/// Outside → button → outside: a cursor that never crossed the client area still gets its leave.
#[test]
fn entering_and_leaving_through_the_button_alone_still_leaves() {
    let (_, tracking) = route(WM_MOUSELEAVE, false, Spot::Maximize, false, SnapTracking::default());
    let (action, tracking) = route(WM_NCMOUSELEAVE, false, Spot::Outside, false, tracking);
    assert_eq!((action, tracking), (SnapAction::ForwardLeave, SnapTracking::default()));
}

/// While a press holds the capture, winit already sees every move and tracks leaving itself, so a
/// leave is neither swallowed nor forwarded; when the capture ends the cursor's spot decides.
#[test]
fn a_held_press_defers_leave_handling_until_the_capture_ends() {
    let lost = SnapTracking { lost_leave: true };
    let (action, _) = route(WM_MOUSELEAVE, false, Spot::Maximize, true, SnapTracking::default());
    assert_eq!(action, SnapAction::Pass, "captured: winit's own leave");
    assert_eq!(route(WM_NCMOUSELEAVE, false, Spot::Outside, true, lost).0, SnapAction::Pass);
    let ended = |spot| route(WM_CAPTURECHANGED, false, spot, false, lost);
    assert_eq!(ended(Spot::Client), (SnapAction::RearmClient, SnapTracking::default()));
    assert_eq!(ended(Spot::Maximize), (SnapAction::ArmNonClient, lost));
    assert_eq!(ended(Spot::Outside), (SnapAction::ForwardLeave, SnapTracking::default()));
    let idle = route(WM_CAPTURECHANGED, false, Spot::Outside, false, SnapTracking::default());
    assert_eq!(idle.0, SnapAction::Pass, "nothing lost: nothing to do");
}

/// Points are signed 16-bit words, so a monitor left of or above the primary (negative screen
/// coordinates) round-trips, and packing matches how Windows builds a mouse message's lparam.
#[test]
fn points_round_trip_with_negative_coordinates() {
    for (x_px, y_px) in [(0, 0), (731, 16), (-269, 116), (-1, -1), (-32768, 32767)] {
        assert_eq!(point_of(lparam_of(x_px, y_px)), (x_px, y_px));
    }
    assert_eq!(lparam_of(731, 16), 731 | (16 << 16));
    assert_eq!(point_of(0xFFFF_FFFF_u32 as i32 as isize), (-1, -1), "sign-extended lparam");
    assert_eq!(point_of(0xFFFF_FFFF), (-1, -1), "zero-extended lparam");
}
