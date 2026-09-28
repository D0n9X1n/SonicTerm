//! Windows window setup: the window-class background brush that follows the
//! theme color, and the monitor work area that bounds a DPI transition's
//! target size.

use super::*;

#[cfg(target_os = "windows")]
static WINDOW_BG_BRUSHES: std::sync::OnceLock<std::sync::Mutex<HashMap<u32, isize>>> =
    std::sync::OnceLock::new();

#[cfg(target_os = "windows")]
fn native_background_brush(rgb: (u8, u8, u8)) -> Option<isize> {
    use windows::Win32::{Foundation::COLORREF, Graphics::Gdi::CreateSolidBrush};

    // COLORREF is 0x00BBGGRR. Brushes stay alive for the process lifetime:
    // window classes can retain their handles after this call returns, so
    // deleting a superseded theme brush would leave those classes dangling.
    let (r, g, b) = rgb;
    let color = u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16);
    let brushes = WINDOW_BG_BRUSHES.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut brushes = brushes.lock().ok()?;
    if let Some(brush) = brushes.get(&color) {
        // When: this `color` was already realized; reuse the cached handle so
        // repeat theme applications do not leak one GDI brush per call.
        return Some(*brush);
    }
    let brush =
        // SAFETY: `CreateSolidBrush` takes the COLORREF by value and has no
        // pointer or lifetime preconditions; failure is reported as a null handle.
        unsafe { CreateSolidBrush(COLORREF(color)) }.0 as isize;
    if brush == 0 {
        // When: GDI refused the allocation and `brush` is null; report absence so
        // callers keep the existing class brush instead of installing handle zero.
        return None;
    }
    brushes.insert(color, brush);
    Some(brush)
}

#[cfg(target_os = "windows")]
#[doc(hidden)]
/// Point the window's class background at a brush of the configured theme color.
///
/// Windows paints newly exposed client area with the class brush before the
/// swapchain presents, so leaving the default makes a resize flash white.
pub fn install_native_window_background(window: &Window, bg_hex: &str) {
    let Some(rgb) = parse_hex_rgb(bg_hex) else {
        // When: `bg_hex` is not a six-digit color, so there is nothing to realize;
        // keep whatever background the class already carries.
        return;
    };
    let Some(brush) = native_background_brush(rgb) else {
        // When: `native_background_brush` exhausted GDI, so installing its null
        // result would blank the class instead of theming it.
        return;
    };
    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        // When: `window_handle` reports no live handle, so no window class exists
        // to retarget and the paint would land nowhere.
        return;
    };
    let raw_window_handle::RawWindowHandle::Win32(h) = handle.as_raw() else {
        // When: `handle` is not the `Win32` variant, so this class-word write does
        // not apply to whatever backend produced it.
        return;
    };
    let hwnd = windows::Win32::Foundation::HWND(h.hwnd.get() as *mut _);
    // SAFETY: `hwnd` is derived from a handle the window just reported as live,
    // and `brush` outlives the class because the cache never frees its brushes.
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::SetClassLongPtrW(
            hwnd,
            windows::Win32::UI::WindowsAndMessaging::GCLP_HBRBACKGROUND,
            brush,
        );
    }
}

#[cfg(target_os = "windows")]
pub(super) fn destination_available_inner_size(
    window: &Window,
    old_scale: f64,
    new_scale: f64,
    minimum: winit::dpi::PhysicalSize<u32>,
) -> winit::dpi::PhysicalSize<u32> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };

    let outer = window.outer_size();
    let inner = window.inner_size();
    let decoration_scale = new_scale.max(0.1) / old_scale.max(0.1);
    let decoration_width =
        (f64::from(outer.width.saturating_sub(inner.width)) * decoration_scale).ceil() as u32;
    let decoration_height =
        (f64::from(outer.height.saturating_sub(inner.height)) * decoration_scale).ceil() as u32;
    let Ok(handle) = window.window_handle() else {
        // When: no native handle is available, preserve the minimum without inventing a monitor cap.
        return winit::dpi::PhysicalSize::new(u32::MAX, u32::MAX);
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        // When: the handle is not Win32, this Windows-only monitor query cannot classify it.
        return winit::dpi::PhysicalSize::new(u32::MAX, u32::MAX);
    };
    let hwnd = windows::Win32::Foundation::HWND(handle.hwnd.get() as *mut _);
    let monitor =
        // SAFETY: hwnd is the live winit window; the API returns an opaque monitor handle.
        unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut info =
        MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if !
        // SAFETY: info points to initialized writable storage with cbSize set as required.
        unsafe { GetMonitorInfoW(monitor, &mut info) }
        .as_bool()
    {
        // When: GetMonitorInfoW fails, preserve the minimum without applying an unproven cap.
        return winit::dpi::PhysicalSize::new(u32::MAX, u32::MAX);
    }
    let work_width = u32::try_from(info.rcWork.right.saturating_sub(info.rcWork.left)).unwrap_or(0);
    let work_height =
        u32::try_from(info.rcWork.bottom.saturating_sub(info.rcWork.top)).unwrap_or(0);
    winit::dpi::PhysicalSize::new(
        work_width.saturating_sub(decoration_width).max(minimum.width),
        work_height.saturating_sub(decoration_height).max(minimum.height),
    )
}
