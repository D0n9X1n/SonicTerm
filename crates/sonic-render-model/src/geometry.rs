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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}
