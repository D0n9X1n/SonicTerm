use crate::geometry::PixelRect;

/// Backend-agnostic drawing command surface for simple rectangle and text emitters.
/// No production backend currently implements this trait; `GpuRenderer::render`
/// consumes `PaneRender` and `RenderInputs` directly.
pub trait Painter {
    /// Fill an axis-aligned rectangle with a solid linear-sRGB RGBA color —
    /// used for cursor blocks, tab chrome, underlines, selection tint, etc.
    fn draw_quad(&mut self, rect: PixelRect, color: [f32; 4]);
    /// Render `text` clipped to `rect` in the given foreground color, using the
    /// painter's currently-bound font + glyph atlas.
    fn draw_text(&mut self, rect: PixelRect, text: &str, color: [f32; 4]);
}
