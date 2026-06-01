//! DWM backdrop application — Mica on Win11, acrylic fallback elsewhere.
//!
//! Wraps `window-vibrancy` so the rest of the Windows binary doesn't have
//! to depend on it directly. Both calls are best-effort: a failure simply
//! leaves the window with its default opaque background.

#![cfg(target_os = "windows")]

use raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
};
use sonicterm_core::config::BackdropKind;
use windows::Win32::{
    Foundation::HWND,
    Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE},
};

const DWMSBT_MAINWINDOW: u32 = 2;
const DWMSBT_TABBEDWINDOW: u32 = 4;

/// Apply the configured Windows compositor backdrop. Errors are swallowed —
/// neither is critical; the terminal renders fine on an opaque BG.
pub fn apply_backdrop(hwnd: HWND, backdrop: BackdropKind) {
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
    // SAFETY: `hwnd` is a live top-level window handle from winit, and the
    // attribute payload is a pointer to a valid `u32` for the duration of the
    // synchronous DWM call.
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
        // PANIC: safe — `make_raw_handle` is called only from `apply_backdrop`
        // (above) after winit has handed us a valid HWND for an existing
        // window. A null HWND would mean winit lied; that's a Win32 / winit
        // contract bug, not a recoverable runtime condition.
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
