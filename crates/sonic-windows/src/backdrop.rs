//! DWM backdrop application — Mica on Win11, acrylic fallback elsewhere.
//!
//! Wraps `window-vibrancy` so the rest of the Windows binary doesn't have
//! to depend on it directly. Both calls are best-effort: a failure simply
//! leaves the window with its default opaque background.

#![cfg(target_os = "windows")]

use raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
};
use windows::Win32::Foundation::HWND;

/// Apply Mica (Win11) or fall back to acrylic. Errors are swallowed —
/// neither is critical; the terminal renders fine on an opaque BG.
pub fn apply_backdrop(hwnd: HWND) {
    let raw = make_raw_handle(hwnd);
    let holder = HandleHolder(raw);
    // Try Mica first — succeeds on Win11 22H2+.
    if window_vibrancy::apply_mica(&holder, Some(true)).is_ok() {
        return;
    }
    // Fall back to acrylic for Win10 / older Win11 builds.
    let _ = window_vibrancy::apply_acrylic(&holder, Some((18, 18, 18, 125)));
}

fn make_raw_handle(hwnd: HWND) -> RawWindowHandle {
    let h = std::num::NonZeroIsize::new(hwnd.0 as isize)
        .expect("HWND is non-null when applying backdrop");
    let handle = Win32WindowHandle::new(h);
    // hinstance is optional for window-vibrancy's purposes.
    RawWindowHandle::Win32(handle)
}

/// Adapter so a bare [`RawWindowHandle`] satisfies the
/// [`HasWindowHandle`] bound required by `window-vibrancy` 0.5 (which
/// moved from raw-window-handle 0.5's free `RawWindowHandle` to 0.6's
/// trait-bound API).
struct HandleHolder(RawWindowHandle);

impl HasWindowHandle for HandleHolder {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // SAFETY: `self.0` is a Win32 HWND that remains valid for the
        // duration of `apply_backdrop`'s synchronous DWM calls — the
        // caller passes a live HWND from the on_window_ready hook and
        // we return the borrow only for the lifetime of `&self`.
        Ok(unsafe { WindowHandle::borrow_raw(self.0) })
    }
}
