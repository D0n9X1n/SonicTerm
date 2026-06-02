//! sRGB→linear gamma conversion for theme colors.
//!
//! Regression: the wgpu surface is `Bgra8UnormSrgb`, which performs
//! linear→sRGB encoding on write. If we pass sRGB-encoded hex values to
//! the clear color / quad pipeline as if they were linear, gamma is applied
//! twice and dark backgrounds wash out. Gruvbox Dark Hard `#1d2021` (~RGB
//! 29,32,33) rendered as `~#6e6e6e` (~110,110,110) — a mid-gray — until
//! `hex_to_wgpu` / `hex_to_rgba` were updated to convert sRGB→linear.

use sonicterm_gpu::color::{hex_to_rgba, hex_to_wgpu, srgb_channel_to_linear};

fn approx(a: f64, b: f64, eps: f64) -> bool {
    (a - b).abs() < eps
}

#[test]
fn hex_to_wgpu_converts_srgb_to_linear() {
    // Gruvbox Dark Hard background: #1d2021. sRGB (29,32,33)/255 ≈
    // (0.1137, 0.1255, 0.1294). After sRGB→linear (each channel below the
    // 0.04045 knee uses c/12.92, above uses ((c+0.055)/1.055)^2.4) → roughly
    // (0.0131, 0.0152, 0.0159). The OLD (broken) behavior left these at
    // (~0.114, ~0.125, ~0.129) and produced a washed-out mid-gray on screen.
    let c = hex_to_wgpu("#1d2021");
    assert!(approx(c.r, 0.0131, 0.002), "r={} expected ~0.013, NOT ~0.114", c.r);
    assert!(approx(c.g, 0.0152, 0.002), "g={} expected ~0.015, NOT ~0.125", c.g);
    assert!(approx(c.b, 0.0159, 0.002), "b={} expected ~0.016, NOT ~0.129", c.b);
    assert!((c.a - 1.0).abs() < 1e-9, "alpha must remain 1.0, got {}", c.a);

    // sRGB white is linear white: identity at the endpoints.
    let w = hex_to_wgpu("#ffffff");
    assert!(approx(w.r, 1.0, 1e-9));
    assert!(approx(w.g, 1.0, 1e-9));
    assert!(approx(w.b, 1.0, 1e-9));

    // sRGB black is linear black.
    let k = hex_to_wgpu("#000000");
    assert!(approx(k.r, 0.0, 1e-9));
    assert!(approx(k.g, 0.0, 1e-9));
    assert!(approx(k.b, 0.0, 1e-9));

    // Mid-gray sRGB #808080 (0.502) is NOT 0.502 in linear — it's ~0.216.
    // This is the canonical "perceptual midpoint != linear midpoint" check.
    let m = hex_to_wgpu("#808080");
    assert!(approx(m.r, 0.2159, 0.005), "r={} expected ~0.216, NOT ~0.502", m.r);
    assert!(approx(m.g, 0.2159, 0.005));
    assert!(approx(m.b, 0.2159, 0.005));
}

#[test]
fn hex_to_rgba_converts_srgb_to_linear_keeps_alpha_straight() {
    // Same conversion as hex_to_wgpu, just packed into [f32; 4]. Alpha
    // must pass through unchanged (no gamma curve applies to alpha).
    let c = hex_to_rgba("#1d2021", 0.5);
    assert!(approx(c[0] as f64, 0.0131, 0.002), "r={}", c[0]);
    assert!(approx(c[1] as f64, 0.0152, 0.002), "g={}", c[1]);
    assert!(approx(c[2] as f64, 0.0159, 0.002), "b={}", c[2]);
    assert!((c[3] - 0.5).abs() < 1e-6, "alpha must pass through, got {}", c[3]);

    let m = hex_to_rgba("#808080", 1.0);
    assert!(approx(m[0] as f64, 0.2159, 0.005), "mid-gray must be ~0.216 linear, got {}", m[0]);
    assert!((m[3] - 1.0).abs() < 1e-6);
}

#[test]
fn srgb_transfer_knee_is_continuous() {
    // The sRGB EOTF is piecewise. At the knee c=0.04045 the linear
    // segment c/12.92 and the power segment ((c+0.055)/1.055)^2.4 must
    // produce nearly the same value.
    let knee = 0.04045_f64;
    let linear_segment = knee / 12.92;
    let lo = srgb_channel_to_linear(knee - 1e-6);
    let hi = srgb_channel_to_linear(knee + 1e-6);
    assert!((lo - linear_segment).abs() < 1e-6);
    assert!((hi - linear_segment).abs() < 1e-3);
}
