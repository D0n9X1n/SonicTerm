//! Window backend trait. Minimal surface over winit / OS windows.
//! Implementers wrap `winit::window::Window`.
//!
//! Must be **object-safe**.

/// Minimal window backend abstraction.
pub trait WindowBackend: Send {
    /// Inner size in physical pixels (post-DPI scale).
    fn inner_size_px(&self) -> (u32, u32);

    /// Device pixel ratio (1.0 on non-HiDPI, 2.0 on Retina, etc.).
    fn scale_factor(&self) -> f64;

    /// Request a redraw on the next vsync. Coalesced by the backend —
    /// safe to call repeatedly. See LM-002 / LM-004.
    fn request_redraw(&self);

    /// Set the window title.
    fn set_title(&self, title: &str);
}
