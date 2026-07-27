//! Win32 software-present support for no-GPU terminals.
//!
//! This module owns the retained BGRA surface + dirty-rectangle presentation
//! primitive used by the Windows software-render path. It deliberately lives in
//! the Windows binary crate because the present step is pure Win32/GDI and must
//! not leak into the cross-platform app or GPU crates.

#![allow(dead_code)]

use sonicterm_cfg::config::SoftwareRenderMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl DirtyRect {
    #[must_use]
    pub fn new(x: u32, y: u32, w: u32, h: u32) -> Option<Self> {
        if w == 0 || h == 0 {
            None
        } else {
            Some(Self { x, y, w, h })
        }
    }

    #[must_use]
    pub fn clipped(self, width: u32, height: u32) -> Option<Self> {
        let x2 = self.x.saturating_add(self.w).min(width);
        let y2 = self.y.saturating_add(self.h).min(height);
        if self.x >= x2 || self.y >= y2 {
            None
        } else {
            Some(Self { x: self.x, y: self.y, w: x2 - self.x, h: y2 - self.y })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsSoftwarePresenterPreference {
    /// Prefer the normal wgpu path unless adapter detection proves it is WARP.
    Auto,
    /// Use the Win32 retained-BGRA presenter immediately.
    Force,
    /// Never use the Win32 retained-BGRA presenter.
    Off,
}

impl WindowsSoftwarePresenterPreference {
    #[must_use]
    pub fn from_config(mode: SoftwareRenderMode) -> Self {
        match mode {
            SoftwareRenderMode::Auto => Self::Auto,
            SoftwareRenderMode::Force => Self::Force,
            SoftwareRenderMode::Off => Self::Off,
        }
    }

    /// Whether the software presenter applies, given runtime adapter detection.
    ///
    /// **No production caller.** The only one passed a hardcoded `false`, so
    /// under `Auto` — the default — it always answered "no" whatever the host
    /// had; it gated a log line and was removed rather than corrected, because
    /// `app/event_loop.rs` already logs `software-render degrade engaged` with
    /// both `detected` and `mode` at the moment those are real.
    ///
    /// A real caller cannot exist here yet for a structural reason:
    /// `detected_software_adapter` comes from the renderer, and the renderer is
    /// built inside `WindowsShell` — after this crate's startup code runs. The
    /// caller will arrive with [`SoftwareSurface`], which is also not yet
    /// constructed outside tests.
    ///
    /// Kept because the decision it encodes is verified against the app's copy
    /// (`should_degrade_for_software_render`) across the whole `(mode,
    /// detected)` domain, and that agreement is what stops a half-degraded
    /// renderer when the presenter is wired up.
    #[must_use]
    pub fn should_use(self, detected_software_adapter: bool) -> bool {
        match self {
            Self::Auto => detected_software_adapter,
            Self::Force => true,
            Self::Off => false,
        }
    }

    #[must_use]
    pub fn forces_opaque_window(self) -> bool {
        matches!(self, Self::Force)
    }
}

/// Retained BGRA8 top-down software surface.
///
/// Pixels are premultiplied BGRA, matching the DIB format passed to GDI.
pub struct SoftwareSurface {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    dirty: Vec<DirtyRect>,
}

impl SoftwareSurface {
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        Self::try_new(width, height).expect("software surface dimensions exceed safety limits")
    }

    #[must_use]
    pub fn try_new(width: u32, height: u32) -> Option<Self> {
        let width = width.max(1);
        let height = height.max(1);
        let len = pixel_len(width, height)?;
        Some(Self { width, height, pixels: vec![0; len], dirty: Vec::new() })
    }

    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    #[must_use]
    pub fn dirty_rects(&self) -> &[DirtyRect] {
        &self.dirty
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let _ = self.try_resize(width, height);
    }

    pub fn try_resize(&mut self, width: u32, height: u32) -> bool {
        let width = width.max(1);
        let height = height.max(1);
        if self.width == width && self.height == height {
            return true;
        }
        let Some(len) = pixel_len(width, height) else { return false };
        self.width = width;
        self.height = height;
        if len > self.pixels.capacity() || len < self.pixels.capacity() / 2 {
            self.pixels = vec![0; len];
        } else {
            self.pixels.resize(len, 0);
        }
        // Rects recorded before this call were clipped against the previous
        // dimensions, so after a shrink the list can hold a rect larger than
        // the surface it will be presented against — `present_dirty` passes
        // each rect to GDI as both source and destination geometry, against a
        // buffer that has just been reallocated to the new, smaller size.
        //
        // Discarding them loses nothing: the full-surface rect appended below
        // covers every pixel of the new surface, so any retained rect is a
        // subset of it once clipped.
        self.dirty.clear();
        self.mark_dirty(DirtyRect { x: 0, y: 0, w: width, h: height });
        true
    }

    pub fn mark_dirty(&mut self, rect: DirtyRect) {
        if let Some(rect) = rect.clipped(self.width, self.height) {
            self.dirty.push(rect);
        }
    }

    pub fn clear_dirty(&mut self) {
        self.dirty.clear();
    }

    pub fn fill_rect_bgra(&mut self, rect: DirtyRect, bgra: [u8; 4]) {
        let Some(rect) = rect.clipped(self.width, self.height) else { return };
        let stride = self.width as usize * 4;
        for y in rect.y..rect.y + rect.h {
            let row = y as usize * stride;
            for x in rect.x..rect.x + rect.w {
                let offset = row + x as usize * 4;
                self.pixels[offset..offset + 4].copy_from_slice(&bgra);
            }
        }
        self.dirty.push(rect);
    }

    #[cfg(target_os = "windows")]
    pub fn present_dirty(&mut self, hwnd: windows::Win32::Foundation::HWND) -> std::io::Result<()> {
        if self.dirty.is_empty() {
            return Ok(());
        }
        let hdc = unsafe { GetDC(hwnd.0) };
        if hdc.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let mut first_error = None;
        for rect in self.dirty.iter().copied() {
            let ok = unsafe {
                stretch_dibits_rect(hdc, self.pixels.as_ptr().cast(), self.width, self.height, rect)
            };
            if !ok && first_error.is_none() {
                first_error = Some(std::io::Error::last_os_error());
            }
        }
        unsafe {
            let _ = ReleaseDC(hwnd.0, hdc);
        }
        if let Some(err) = first_error {
            Err(err)
        } else {
            self.clear_dirty();
            Ok(())
        }
    }
}

fn pixel_len(width: u32, height: u32) -> Option<usize> {
    const MAX_DIMENSION: u32 = 16_384;
    const MAX_BYTES: u64 = 160 * 1024 * 1024;
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return None;
    }
    let bytes = u64::from(width).checked_mul(u64::from(height))?.checked_mul(4)?;
    if bytes > MAX_BYTES {
        return None;
    }
    usize::try_from(bytes).ok()
}

#[cfg(target_os = "windows")]
type Hdc = *mut std::ffi::c_void;

#[cfg(target_os = "windows")]
const BI_RGB: u32 = 0;

#[cfg(target_os = "windows")]
const DIB_RGB_COLORS: u32 = 0;

#[cfg(target_os = "windows")]
const SRCCOPY: u32 = 0x00CC0020;

#[cfg(target_os = "windows")]
const GDI_ERROR: i32 = -1;

#[cfg(target_os = "windows")]
#[repr(C)]
struct BitmapInfoHeader {
    bi_size: u32,
    bi_width: i32,
    bi_height: i32,
    bi_planes: u16,
    bi_bit_count: u16,
    bi_compression: u32,
    bi_size_image: u32,
    bi_x_pels_per_meter: i32,
    bi_y_pels_per_meter: i32,
    bi_clr_used: u32,
    bi_clr_important: u32,
}

#[cfg(target_os = "windows")]
#[repr(C)]
struct RgbQuad {
    rgb_blue: u8,
    rgb_green: u8,
    rgb_red: u8,
    rgb_reserved: u8,
}

#[cfg(target_os = "windows")]
#[repr(C)]
struct BitmapInfo {
    bmi_header: BitmapInfoHeader,
    bmi_colors: [RgbQuad; 1],
}

#[cfg(target_os = "windows")]
unsafe fn stretch_dibits_rect(
    hdc: Hdc,
    pixels: *const std::ffi::c_void,
    width: u32,
    height: u32,
    rect: DirtyRect,
) -> bool {
    let info = BitmapInfo {
        bmi_header: BitmapInfoHeader {
            bi_size: std::mem::size_of::<BitmapInfoHeader>() as u32,
            bi_width: width as i32,
            // Negative height makes this a top-down DIB, so row 0 is the top
            // row and dirty terminal rows map directly to GDI coordinates.
            bi_height: -(height as i32),
            bi_planes: 1,
            bi_bit_count: 32,
            bi_compression: BI_RGB,
            bi_size_image: width.saturating_mul(height).saturating_mul(4),
            bi_x_pels_per_meter: 0,
            bi_y_pels_per_meter: 0,
            bi_clr_used: 0,
            bi_clr_important: 0,
        },
        bmi_colors: [RgbQuad { rgb_blue: 0, rgb_green: 0, rgb_red: 0, rgb_reserved: 0 }],
    };
    let written = unsafe {
        StretchDIBits(
            hdc,
            rect.x as i32,
            rect.y as i32,
            rect.w as i32,
            rect.h as i32,
            rect.x as i32,
            rect.y as i32,
            rect.w as i32,
            rect.h as i32,
            pixels,
            &info as *const BitmapInfo,
            DIB_RGB_COLORS,
            SRCCOPY,
        )
    };
    written != 0 && written != GDI_ERROR
}

#[cfg(target_os = "windows")]
#[link(name = "user32")]
unsafe extern "system" {
    fn GetDC(hwnd: *mut std::ffi::c_void) -> Hdc;
    fn ReleaseDC(hwnd: *mut std::ffi::c_void, hdc: Hdc) -> i32;
}

#[cfg(target_os = "windows")]
#[link(name = "gdi32")]
unsafe extern "system" {
    fn StretchDIBits(
        hdc: Hdc,
        x_dest: i32,
        y_dest: i32,
        dest_width: i32,
        dest_height: i32,
        x_src: i32,
        y_src: i32,
        src_width: i32,
        src_height: i32,
        bits: *const std::ffi::c_void,
        bmi: *const BitmapInfo,
        color_use: u32,
        rop: u32,
    ) -> i32;
}

#[cfg(test)]
#[path = "software_presenter_tests.rs"]
mod software_presenter_tests;
