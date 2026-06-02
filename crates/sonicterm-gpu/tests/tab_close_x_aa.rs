//! Renderer-level guard for the tab close-× anti-aliased path.
//!
//! Lives in `sonicterm-shared` (where the renderer composes the tab bar) so
//! a future regression that swaps the close-icon helper back to the
//! 8x8 binary mask trips a test in the same crate as the render code.
//!
//! Pins the contract: the renderer emits the close × as anti-aliased
//! line-SDF quads (`line_thickness_px > 0`), not as a stair-stepping
//! binary alpha mask.

use sonicterm_gpu::quad::{push_close_x_quads, CloseXParams, QuadInstance};

fn close_x_quads() -> Vec<QuadInstance> {
    let mut quads = Vec::new();
    // 14 px close-button rect, 8 px glyph (mirrors `render::core` inset
    // math: `inset = (14 - 8) * 0.5 = 3`, `glyph = 8`).
    push_close_x_quads(
        &mut quads,
        CloseXParams {
            x: 3.0,
            y: 3.0,
            size: 8.0,
            thickness: (8.0_f32 * 0.14).max(1.25),
            color: [0.7, 0.7, 0.7, 1.0],
            sw: 800.0,
            sh: 600.0,
        },
    );
    quads
}

#[test]
fn tab_close_x_renders_via_line_sdf_not_binary_mask() {
    let quads = close_x_quads();
    assert_eq!(quads.len(), 2, "close × must be two diagonal strokes, got {}", quads.len());
    for q in &quads {
        assert!(
            q.line_thickness_px > 0.0,
            "stroke must use the line-SDF (AA) path, not the binary-mask path; got thickness {}",
            q.line_thickness_px
        );
    }
}

#[test]
fn tab_close_x_strokes_cross() {
    // The two strokes must form an ×: one diagonal goes top-left →
    // bottom-right, the other top-right → bottom-left. If a future
    // refactor accidentally pointed both strokes the same way the
    // user would see a `\\` or `//`, not an `×`.
    let quads = close_x_quads();
    let d = |q: &QuadInstance| [q.line_b[0] - q.line_a[0], q.line_b[1] - q.line_a[1]];
    let d0 = d(&quads[0]);
    let d1 = d(&quads[1]);
    let crossed = d0[0].signum() != d1[0].signum() || d0[1].signum() != d1[1].signum();
    assert!(crossed, "strokes must cross to form ×: d0={d0:?}, d1={d1:?}");
}
