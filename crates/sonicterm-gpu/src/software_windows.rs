//! Windows-only HWND/HDC bridge for an already-composed CPU frame.

#![cfg(target_os = "windows")]

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{
        GetDC, ReleaseDC, SetDIBitsToDevice, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        HDC, RGBQUAD,
    },
};
use winit::window::Window;

use crate::software_frame::{BgraFrame, SoftwareFrame};

/// Present a complete CPU frame through the Windows native bridge.
pub(crate) fn present_frame(frame: &SoftwareFrame, window: &Window) -> anyhow::Result<()> {
    let hwnd = hwnd_for_window(window)?;
    // SAFETY: hwnd belongs to the live window; BgraFrame binds validated extent to its initialized pixel storage.
    unsafe { blit_bgra_to_hwnd(hwnd, frame.bgra_frame()) }
}

fn hwnd_for_window(window: &Window) -> anyhow::Result<HWND> {
    let handle = window.window_handle()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Ok(HWND(h.hwnd.get() as *mut _)),
        _ => anyhow::bail!("window is not a Win32 HWND"),
    }
}

// SAFETY: hwnd must be live; frame keeps validated dimensions and initialized storage borrowed through the blit.
unsafe fn blit_bgra_to_hwnd(hwnd: HWND, frame: BgraFrame<'_>) -> anyhow::Result<()> {
    let hdc =
        // SAFETY: GetDC only reads hwnd, and reports failure as a null HDC rather than
        // by trapping, which the null check below rejects before any drawing use.
        unsafe { GetDC(Some(hwnd)) };
    if hdc.0.is_null() {
        anyhow::bail!("GetDC failed");
    }
    let result =
        // SAFETY: hdc came from the live hwnd; frame preserves its validated extent and storage through this call.
        unsafe { blit_bgra_to_hdc(hdc, frame) };
    let _ =
        // SAFETY: releases exactly the hdc GetDC returned, paired with the same hwnd, on
        // every path that reached the acquisition.
        unsafe { ReleaseDC(Some(hwnd), hdc) };
    result
}

// SAFETY: hdc must be valid; frame's immutable owner guarantees exactly width * height * 4 initialized BGRA bytes.
unsafe fn blit_bgra_to_hdc(hdc: HDC, frame: BgraFrame<'_>) -> anyhow::Result<()> {
    let width = frame.width();
    let height = frame.height();
    let pixels = frame.pixels();
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        bmiColors: [RGBQUAD::default(); 1],
    };
    let rows =
        // SAFETY: bmi describes height rows of width BGRA texels, the same extent the
        // caller guaranteed in pixels; biHeight is negated to read the slice top-down.
        unsafe {
        SetDIBitsToDevice(
            hdc,
            0,
            0,
            width,
            height,
            0,
            0,
            0,
            height,
            pixels.as_ptr() as *const std::ffi::c_void,
            &bmi,
            DIB_RGB_COLORS,
        )
    };
    if rows == 0 {
        anyhow::bail!("SetDIBitsToDevice failed");
    }
    Ok(())
}
