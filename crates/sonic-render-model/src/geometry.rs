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
