/// Inset (in pixels) for an OS-integrated titlebar.
/// 0 on macOS (native titlebar), 32 on Windows (caption buttons drawn by us).
pub const fn integrated_titlebar_inset_px() -> u32 {
    #[cfg(target_os = "windows")]
    {
        32
    }
    #[cfg(not(target_os = "windows"))]
    {
        0
    }
}

/// Axis-aligned rectangle in window-pixel space (origin top-left, y grows down)
/// — the common geometry primitive shared between layout code and the painter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    /// Left edge in window pixels.
    pub x: i32,
    /// Top edge in window pixels.
    pub y: i32,
    /// Width in window pixels.
    pub w: u32,
    /// Height in window pixels.
    pub h: u32,
}
