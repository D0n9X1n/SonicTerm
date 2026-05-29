use sonic_gpu::quad::{paint_caption_buttons, QuadInstance};

fn ndc_to_px(rect: [f32; 4], surface: (f32, f32)) -> (f32, f32, f32, f32) {
    let (sw, sh) = surface;
    let w = rect[2] * sw / 2.0;
    let h = rect[3] * sh / 2.0;
    let x = (rect[0] + 1.0) * sw / 2.0;
    let y = (1.0 - rect[1] - rect[3]) * sh / 2.0;
    (x, y, w, h)
}

#[test]
fn caption_buttons_emit_quads() {
    let surface = (1000.0, 700.0);
    let rects = [(862.0, 0.0, 46.0, 32.0), (908.0, 0.0, 46.0, 32.0), (954.0, 0.0, 46.0, 32.0)];
    let mut quads: Vec<QuadInstance> = Vec::new();

    paint_caption_buttons(&mut quads, &rects, surface, [0.1, 0.1, 0.1, 1.0], [0.9, 0.9, 0.9, 1.0]);

    assert!(quads.len() >= 13, "caption buttons should emit backgrounds plus geometric strokes");
    for (quad, expected) in quads.iter().take(3).zip(rects) {
        let got = ndc_to_px(quad.rect, surface);
        assert!((got.0 - expected.0).abs() < 0.5, "x mismatch: got {got:?}, expected {expected:?}");
        assert!((got.1 - expected.1).abs() < 0.5, "y mismatch: got {got:?}, expected {expected:?}");
        assert!((got.2 - expected.2).abs() < 0.5, "w mismatch: got {got:?}, expected {expected:?}");
        assert!((got.3 - expected.3).abs() < 0.5, "h mismatch: got {got:?}, expected {expected:?}");
        assert!(quad.color[3] > 0.0, "background alpha must be non-zero: {quad:?}");
    }
    for quad in quads.iter().skip(3) {
        assert!(quad.color[3] > 0.0, "stroke alpha must be non-zero: {quad:?}");
        assert_ne!(quad.color, quads[0].color, "stroke color must contrast with background");
    }
}
