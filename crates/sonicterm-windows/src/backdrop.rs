//! DWM backdrop application — Mica on Win11, acrylic fallback elsewhere.
//!
//! Wraps `window-vibrancy` so the rest of the Windows binary doesn't have
//! to depend on it directly. Both calls are best-effort: a failure simply
//! leaves the window with its default opaque background.

#![cfg(target_os = "windows")]

use raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
};
use sonicterm_cfg::config::BackdropKind;
use windows::Win32::{
    Foundation::HWND,
    Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE},
};

const DWMSBT_MAINWINDOW: u32 = 2;
const DWMSBT_TABBEDWINDOW: u32 = 4;

/// Apply the configured Windows compositor backdrop. Errors are swallowed —
/// neither is critical; the terminal renders fine on an opaque BG.
///
/// # Safety
///
/// `hwnd` must name a live top-level window for the duration of this call.
// SAFETY: callers must provide the live HWND required by the DWM and
// raw-window-handle operations below.
pub unsafe fn apply_backdrop(hwnd: HWND, backdrop: BackdropKind) {
    let result = match backdrop {
        BackdropKind::Opaque => Ok("opaque"),
        BackdropKind::Mica => apply_mica(hwnd),
        BackdropKind::Acrylic => apply_acrylic(hwnd),
        BackdropKind::Tabbed => apply_tabbed(hwnd),
    };
    match result {
        Ok(kind) => tracing::info!(backdrop = kind, "Windows backdrop applied"),
        Err(e) => tracing::warn!(?backdrop, error = %e, "Windows backdrop apply failed"),
    }
}

fn apply_mica(hwnd: HWND) -> Result<&'static str, String> {
    let raw = make_raw_handle(hwnd);
    let holder = HandleHolder(raw);
    window_vibrancy::apply_mica(&holder, Some(true)).map_err(|e| e.to_string())?;
    set_system_backdrop(hwnd, DWMSBT_MAINWINDOW).map_err(|e| e.to_string())?;
    Ok("mica")
}

fn apply_acrylic(hwnd: HWND) -> Result<&'static str, String> {
    let raw = make_raw_handle(hwnd);
    let holder = HandleHolder(raw);
    window_vibrancy::apply_acrylic(&holder, Some((18, 18, 18, 125))).map_err(|e| e.to_string())?;
    Ok("acrylic")
}

fn apply_tabbed(hwnd: HWND) -> Result<&'static str, String> {
    apply_mica(hwnd)?;
    set_system_backdrop(hwnd, DWMSBT_TABBEDWINDOW).map_err(|e| e.to_string())?;
    Ok("tabbed")
}

fn set_system_backdrop(hwnd: HWND, backdrop_type: u32) -> windows_core::Result<()> {
    // SAFETY: the unsafe `apply_backdrop` contract guarantees a live HWND, and
    // the attribute payload is a valid `u32` for this synchronous DWM call.
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &backdrop_type as *const u32 as *const _,
            std::mem::size_of_val(&backdrop_type) as u32,
        )
    }
}

fn make_raw_handle(hwnd: HWND) -> RawWindowHandle {
    let h = std::num::NonZeroIsize::new(hwnd.0 as isize)
        // PANIC: the unsafe caller contract guarantees a live, non-null HWND;
        // a null value violates that contract rather than representing a
        // recoverable backdrop failure.
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
        Ok(
            // SAFETY: the unsafe constructor contract guarantees the Win32 HWND
            // is live, and the borrowed handle cannot outlive `self`.
            unsafe { WindowHandle::borrow_raw(self.0) },
        )
    }
}
