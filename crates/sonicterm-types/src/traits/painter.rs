//! Legacy painter compatibility seam.
//!
//! No production backend implements this trait; the app and GPU renderer use
//! renderer-specific frame APIs directly. These symbols remain source-compatible
//! for external callers while new code should use renderer-specific frame APIs.

/// Legacy type-erased painter contract retained for source compatibility.
#[doc(hidden)]
pub trait Painter: Send {
    /// Submit one frame's worth of draw commands. Returning `Err`
    /// signals the surface needs reconfiguration (e.g. wgpu
    /// `Suboptimal` — must drop the SurfaceTexture before reconfig per
    /// CLAUDE.md §4).
    fn paint_frame(&mut self, frame: &dyn FrameLike) -> Result<(), PaintError>;

    /// Resize the underlying surface. Called on window resize and DPI
    /// change.
    fn resize_surface(&mut self, width_px: u32, height_px: u32);
}

/// Legacy type-erased frame view retained for source compatibility.
#[doc(hidden)]
pub trait FrameLike {
    /// Logical grid width in cells.
    fn cols(&self) -> u32;
    /// Logical grid height in cells.
    fn rows(&self) -> u32;
}

/// Legacy painter failure values retained for source compatibility.
#[doc(hidden)]
#[derive(Debug)]
pub enum PaintError {
    /// Surface reported `Suboptimal` or `Outdated` — caller must
    /// reconfigure and retry.
    SurfaceLost,
    /// Out-of-memory on GPU resource allocation.
    OutOfMemory,
    /// Other fatal error with backend-specific message.
    Other(String),
}
