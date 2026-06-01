use sonicterm_shared::render::geometry::snap_to_device_pixels;

#[test]
fn integer_scale_is_noop() {
    let r = (10.3_f32, 20.7, 5.0, 5.0);
    assert_eq!(snap_to_device_pixels(r, 1.0), r);
    assert_eq!(snap_to_device_pixels(r, 2.0), r);
    assert_eq!(snap_to_device_pixels(r, 3.0), r);
}

#[test]
fn fractional_scale_snaps_all_edges_to_device_pixel() {
    let scale = 1.5_f32;
    let (x, y, w, h) = snap_to_device_pixels((10.3, 20.2, 7.4, 13.6), scale);
    // All four device-pixel edges must have zero fractional part.
    let eps = 1e-4;
    assert!((x * scale).fract().abs() < eps, "x_dev fractional: {}", (x * scale).fract());
    assert!((y * scale).fract().abs() < eps);
    assert!(((x + w) * scale).fract().abs() < eps);
    assert!(((y + h) * scale).fract().abs() < eps);
}

#[test]
fn snap_uses_edge_based_width_not_independent_w() {
    // (x=10.3, w=7.4) at scale=1.5:
    //   x_dev = round(15.45) = 15.0
    //   r_dev = round((17.7)*1.5) = round(26.55) = 27.0
    //   snapped_w = (27.0 - 15.0) / 1.5 = 8.0
    // If we naively snapped w independently:
    //   snapped_w_naive = round(7.4 * 1.5) / 1.5 = 11.0 / 1.5 = 7.333...
    // Edge-based snapping prevents row-width drift.
    let (_, _, w, _) = snap_to_device_pixels((10.3, 20.0, 7.4, 5.0), 1.5);
    let edge_based = ((10.3_f32 + 7.4) * 1.5).round() - (10.3_f32 * 1.5).round();
    let edge_based_logical = edge_based / 1.5;
    assert!((w - edge_based_logical).abs() < 1e-4, "w={} edge_based={}", w, edge_based_logical);
}

#[test]
fn cell_grid_simulation_at_125_percent() {
    // Real-world Windows scale=1.25, cell_w=8.4 (font-derived), col=10.
    let cell_w = 8.4_f32;
    let scale = 1.25_f32;
    let pad = 4.0_f32;
    let col = 10;
    let gx = pad + (col as f32) * cell_w; // = 4 + 84 = 88.0 logical
    let gy = 0.0_f32;
    let gw = cell_w;
    let gh = 16.0_f32;
    let (sx, _sy, sw_, _sh) = snap_to_device_pixels((gx, gy, gw, gh), scale);
    // All edges device-integer
    assert!(((sx) * scale).fract().abs() < 1e-4);
    assert!(((sx + sw_) * scale).fract().abs() < 1e-4);
}
