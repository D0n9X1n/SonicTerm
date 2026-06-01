//! Painter / FrameSink trait. The renderer-agnostic seam the app loop
//! hands a frame to. Implementers: `sonicterm-gpu` (wgpu).
//!
//! Must be **object-safe**.

/// Opaque frame payload — concrete `FrameModel` lives in
/// `sonicterm-render-model`. Declared as a type-erased associated type
/// to keep this trait dep-free of render-model.
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

/// Type-erased frame view. Concrete `FrameModel` in
/// `sonicterm-render-model` implements this.
pub trait FrameLike {
    /// Logical grid width in cells.
    fn cols(&self) -> u32;
    /// Logical grid height in cells.
    fn rows(&self) -> u32;
}

/// Reasons painting can fail.
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
