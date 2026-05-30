//! Regression test for the tab "close ×" anti-aliased line-quad path.
//!
//! Before this PR the close button was rendered through the 8x8 binary
//! alpha mask [`sonic_gpu::quad::ICON_CLOSE_8`], which stair-stepped on
//! macOS Retina because each emitted cell was a sharp axis-aligned quad
//! with no edge AA — the diagonals were drawn as offset boxes.
//!
//! The new path emits **exactly two** [`QuadInstance`]s (one per stroke)
//! through the line-SDF branch of the WGSL shader: each quad carries a
//! non-zero `line_thickness_px` plus distinct `line_a` / `line_b`
//! endpoints, and the fragment stage smoothsteps a 1-pixel AA band.
//! These two properties are what the test pins.

use sonic_gpu::quad::{push_close_x_quads, CloseXParams};

#[test]
fn close_x_emits_two_line_sdf_quads() {
    let mut quads = Vec::new();
    push_close_x_quads(
        &mut quads,
        CloseXParams {
            x: 100.0,
            y: 50.0,
            size: 8.0,
            thickness: 1.5,
            color: [0.8, 0.8, 0.8, 1.0],
            sw: 1000.0,
            sh: 700.0,
        },
    );

    assert_eq!(
        quads.len(),
        2,
        "close × must emit exactly two line-SDF quads (one per diagonal stroke), got {}",
        quads.len()
    );

    for (i, q) in quads.iter().enumerate() {
        assert!(
            q.line_thickness_px > 0.0,
            "quad #{i} must take the line-SDF path (thickness > 0), got {}",
            q.line_thickness_px
        );
        assert!(q.color[3] > 0.0, "quad #{i} alpha must be non-zero");
        assert_eq!(
            q.radius_px, 0.0,
            "quad #{i} must NOT take the rounded-rect path; line-SDF only"
        );
        // Endpoints must be distinct — a zero-length segment would
        // render as a degenerate dot, not a stroke.
        assert_ne!(q.line_a, q.line_b, "quad #{i} endpoints must differ");
        // Bounding box size must be non-zero so fwidth() in the
        // fragment shader has gradient to smoothstep against.
        assert!(q.size_px[0] > 0.0 && q.size_px[1] > 0.0, "quad #{i} size_px must be > 0");
    }

    // The two strokes must run in opposite diagonal directions.
    // (Both x and y of the delta vectors flip sign between them.)
    let d0 = [quads[0].line_b[0] - quads[0].line_a[0], quads[0].line_b[1] - quads[0].line_a[1]];
    let d1 = [quads[1].line_b[0] - quads[1].line_a[0], quads[1].line_b[1] - quads[1].line_a[1]];
    assert!(
        d0[0].signum() != d1[0].signum() || d0[1].signum() != d1[1].signum(),
        "the two strokes must cross (one '\\' + one '/'), got d0={d0:?} d1={d1:?}"
    );
}

#[test]
fn close_x_thickness_floored_to_one_pixel() {
    // A 0.4 px request would otherwise vanish into sub-pixel territory
    // on a 1× display. The helper must clamp to >= 1.0 so the stroke
    // is always at least one full pixel wide.
    let mut quads = Vec::new();
    push_close_x_quads(
        &mut quads,
        CloseXParams {
            x: 0.0,
            y: 0.0,
            size: 8.0,
            thickness: 0.4,
            color: [1.0; 4],
            sw: 100.0,
            sh: 100.0,
        },
    );
    for q in &quads {
        assert!(q.line_thickness_px >= 1.0, "thickness clamp lost: {}", q.line_thickness_px);
    }
}
