use super::*;

#[test]
fn taking_registered_hwnd_removes_bookkeeping_entry() {
    let backend = WinOsTabDragBackend::new();
    // SAFETY: WindowId is a transparent u64 handle; this test-only value is
    // never passed to winit or Win32.
    let window_id = unsafe { std::mem::transmute::<u64, WindowId>(42) };
    backend.registered_windows.lock().expect("registry lock").insert(window_id, 0x1234);

    assert_eq!(backend.take_registered_hwnd(window_id), Some(0x1234));
    assert!(backend.registered_windows.lock().expect("registry lock").is_empty());
}
