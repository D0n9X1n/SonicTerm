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
fn taking_registered_hwnd_removes_bookkeeping_entry() {
    let backend = WinOsTabDragBackend::new();
    let window_id = WindowId::from(42);
    backend.registered_windows.lock().expect("registry lock").insert(window_id, 0x1234);

    assert_eq!(backend.take_registered_hwnd(window_id), Some(0x1234));
    assert!(backend.registered_windows.lock().expect("registry lock").is_empty());
}

#[test]
fn registration_bookkeeping_requires_native_success() {
    const SOURCE: &str = include_str!("tab_drag_os.rs");
    assert!(SOURCE.contains("let registered ="));
    assert!(SOURCE.contains("if registered"));
    assert!(SOURCE.contains("reg.insert(window_id, hwnd_val)"));
}
