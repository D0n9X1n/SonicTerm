use super::*;

#[test]
fn accepted_move_without_target_preserves_source_instead_of_guessing_main() {
    // OLE's MOVE effect is not a destination identity and must never reuse the press-time tab index.
    let outcome = unresolved_drag_outcome(
        windows::Win32::Foundation::DRAGDROP_S_DROP,
        windows::Win32::System::Ole::DROPEFFECT_MOVE.0,
        || panic!("an accepted move must not query a tear-out position"),
    );
    assert_eq!(outcome, DragOutcome::Cancelled);
}

#[test]
fn cancellation_stays_local_and_empty_drop_keeps_screen_position() {
    // Cancel is not a tear-out; only a completed drop with no accepted target requests screen coordinates.
    assert_eq!(
        unresolved_drag_outcome(windows::Win32::Foundation::DRAGDROP_S_CANCEL, 0, || panic!(
            "cancelled drag"
        )),
        DragOutcome::Cancelled,
    );
    assert_eq!(
        unresolved_drag_outcome(windows::Win32::Foundation::DRAGDROP_S_DROP, 0, || (123, 456)),
        DragOutcome::DroppedOnEmpty { drop_screen_pos: (123, 456) },
    );
}

#[test]
fn registration_report_requires_all_native_lifetimes_without_failures() {
    // Neither fewer windows nor a matching count hiding one native failure can satisfy the runtime smoke.
    let report = DropRegistrationReport { registrations: 3, revocations: 3, live: 0, failures: 0 };
    assert_eq!(report.validate(sonicterm_app::app::RuntimeSmokeScenario::Default), Ok(()));
    for report in [
        DropRegistrationReport::default(),
        DropRegistrationReport { registrations: 2, revocations: 2, live: 0, failures: 0 },
        DropRegistrationReport { registrations: 3, revocations: 2, live: 1, failures: 0 },
        DropRegistrationReport { registrations: 3, revocations: 3, live: 0, failures: 1 },
    ] {
        assert!(report.validate(sonicterm_app::app::RuntimeSmokeScenario::Default).is_err());
    }
}

#[test]
fn registration_bookkeeping_follows_native_success_and_pins_window_custody() {
    // A failed native call may neither publish registration custody nor remove the Arc that keeps its HWND valid.
    let source = include_str!("tab_drag_os.rs");
    let registration = source.split("    fn register_window(").nth(1).unwrap();
    let registration = registration.split("    fn unregister_window(").next().unwrap();
    assert!(
        registration.find("register_for_window(hwnd, window_id)").unwrap()
            < registration.find("self.registered_windows.insert(").unwrap()
    );
    assert!(registration.contains("RegisteredWindow { window: window.clone()"));
    let release = source.split("    fn unregister_window(").nth(1).unwrap();
    assert!(
        release.find("unregister_for_window(hwnd)").unwrap()
            < release.find("self.registered_windows.remove(&window_id)").unwrap()
    );
    assert!(source.contains("impl Drop for WinOsTabDragBackend"));
}

/// The early frame-fault scenario still requires exact main-window registration and revocation.
#[test]
fn frame_fault_registration_report_requires_one_complete_lifetime() {
    use sonicterm_app::app::RuntimeSmokeScenario::FrameValidation;
    let report = DropRegistrationReport { registrations: 1, revocations: 1, live: 0, failures: 0 };
    assert_eq!(report.validate(FrameValidation), Ok(()));
    for report in [
        DropRegistrationReport::default(),
        DropRegistrationReport { registrations: 1, revocations: 0, live: 1, failures: 0 },
        DropRegistrationReport { registrations: 1, revocations: 1, live: 0, failures: 1 },
        DropRegistrationReport { registrations: 3, revocations: 3, live: 0, failures: 0 },
    ] {
        assert!(report.validate(FrameValidation).is_err());
    }
}
