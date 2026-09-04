use crate::geometry::PixelRect;

/// Legacy drawing-command compatibility trait.
///
/// No production backend implements this trait. It remains source-compatible for
/// external callers; new code should use `PaneRender` and `RenderInputs` with the
/// renderer's concrete frame API.
#[doc(hidden)]
pub trait Painter {
    /// Fill an axis-aligned rectangle with a solid linear-sRGB RGBA color —
    /// used for cursor blocks, tab chrome, underlines, selection tint, etc.
    fn draw_quad(&mut self, rect: PixelRect, color: [f32; 4]);
    /// Render `text` clipped to `rect` in the given foreground color, using the
    /// painter's currently-bound font + glyph atlas.
    fn draw_text(&mut self, rect: PixelRect, text: &str, color: [f32; 4]);
}
