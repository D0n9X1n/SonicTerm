//! DWM backdrop application — Mica on Win11, acrylic fallback elsewhere.
//!
//! Wraps `window-vibrancy` so the rest of the Windows binary doesn't have
//! to depend on it directly. Both calls are best-effort: a failure simply
//! leaves the window with its default opaque background.

#![cfg(target_os = "windows")]

use raw_window_handle::{RawWindowHandle, Win32WindowHandle};
use windows::Win32::Foundation::HWND;

/// Apply Mica (Win11) or fall back to acrylic. Errors are swallowed —
/// neither is critical; the terminal renders fine on an opaque BG.
pub fn apply_backdrop(hwnd: HWND) {
    let raw = make_raw_handle(hwnd);
    // Try Mica first — succeeds on Win11 22H2+.
    if window_vibrancy::apply_mica(raw, Some(true)).is_ok() {
        return;
    }
    // Fall back to acrylic for Win10 / older Win11 builds.
    let _ = window_vibrancy::apply_acrylic(raw, Some((18, 18, 18, 125)));
}

fn make_raw_handle(hwnd: HWND) -> RawWindowHandle {
    let h = std::num::NonZeroIsize::new(hwnd.0 as isize)
        .expect("HWND is non-null when applying backdrop");
    let mut handle = Win32WindowHandle::new(h);
    // hinstance is optional for window-vibrancy's purposes.
    let _ = &mut handle;
    RawWindowHandle::Win32(handle)
}
