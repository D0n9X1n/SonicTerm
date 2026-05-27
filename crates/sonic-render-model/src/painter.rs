use crate::geometry::PixelRect;

/// Generic drawing surface. GPU impl provided by sonic-shared (later sonic-gpu).
pub trait Painter {
    fn draw_quad(&mut self, rect: PixelRect, color: [f32; 4]);
    fn draw_text(&mut self, rect: PixelRect, text: &str, color: [f32; 4]);
}
