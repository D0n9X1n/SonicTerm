//! Window setup shared by macOS and Linux, compiled for every target except
//! Windows: the theme background request has no window class to retarget,
//! and a DPI transition's target size gets no monitor work-area cap.

use super::*;

#[cfg(not(target_os = "windows"))]
#[doc(hidden)]
/// Accept the theme background request on platforms with no window class to
/// retarget, so window-creation sites stay identical across platforms.
pub fn install_native_window_background(_window: &Window, _bg_hex: &str) {}

#[cfg(not(target_os = "windows"))]
pub(super) fn destination_available_inner_size(
    _window: &Window,
    _old_scale: f64,
    _new_scale: f64,
    _minimum: winit::dpi::PhysicalSize<u32>,
) -> winit::dpi::PhysicalSize<u32> {
    winit::dpi::PhysicalSize::new(u32::MAX, u32::MAX)
}
