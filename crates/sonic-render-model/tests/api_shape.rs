//! Integration test pinning the public API shape of `sonic-render-model`.

use sonic_render_model::*;

#[test]
fn render_inputs_default_constructs() {
    let r = RenderInputs::default();
    assert_eq!(r.tab_bar.active, 0);
    assert!(!r.overlays.palette_open);
}

struct NoopPainter;
impl Painter for NoopPainter {
    fn draw_quad(&mut self, _: PixelRect, _: [f32; 4]) {}
    fn draw_text(&mut self, _: PixelRect, _: &str, _: [f32; 4]) {}
}

#[test]
fn painter_trait_is_object_safe() {
    let _p: Box<dyn Painter> = Box::new(NoopPainter);
}
