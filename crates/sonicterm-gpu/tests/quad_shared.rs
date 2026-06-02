//! Integration tests for sonicterm-shared quad.

use sonicterm_gpu::quad::*;

#[test]
fn px_to_ndc_full_screen_covers_whole_quad() {
    let q = px_to_ndc(0.0, 0.0, 100.0, 100.0, 100.0, 100.0);
    assert!((q[0] - -1.0).abs() < 1e-5);
    assert!((q[1] - -1.0).abs() < 1e-5);
    assert!((q[2] - 2.0).abs() < 1e-5);
    assert!((q[3] - 2.0).abs() < 1e-5);
}

#[test]
fn px_to_ndc_top_left_pixel() {
    let q = px_to_ndc(0.0, 0.0, 10.0, 10.0, 100.0, 100.0);
    // top-left pixel: x=-1, top of quad at y=1, height=0.2 → y_bottom = 0.8
    assert!((q[0] - -1.0).abs() < 1e-5);
    assert!((q[1] - 0.8).abs() < 1e-5);
}
