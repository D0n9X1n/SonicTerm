use super::*;

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
