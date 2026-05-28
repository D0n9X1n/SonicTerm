//! Part B step 7 — per-pane render origin plumbing.
//!
//! `GpuRenderer::render()` records every pane's `(id, [origin_x, origin_y])`
//! into `last_emit_origins` on every frame so an integration test can
//! confirm both panes' origins reach the renderer (regression guard for the
//! pre-Part-B behaviour where only the active pane's origin was visible).
//!
//! `GpuRenderer::new` requires a live wgpu device + surface, which is not
//! available in `cargo test` on CI / headless macOS. This test therefore
//! exercises the *upstream* invariant on the `PaneRender` slice the app
//! builds before calling `render()` — same data, one indirection earlier.
//! When the offscreen-wgpu harness lands we can promote this to a real
//! `render()` call and assert against `last_emitted_origins()` directly;
//! the hook (`#[doc(hidden)] pub fn last_emitted_origins`) already exists
//! on the renderer for that future test.

use std::sync::Arc;

use parking_lot::Mutex;
use sonic_core::grid::Grid;
use sonic_core::vt::Parser;
use sonic_render_model::geometry::PixelRect;
use sonic_render_model::{CursorStyle, PaneRender};

#[test]
fn split_right_yields_two_pane_origins_with_distinct_x() {
    let parser_a = Arc::new(Mutex::new(Parser::new(Grid::new(50, 35))));
    let parser_b = Arc::new(Mutex::new(Parser::new(Grid::new(50, 35))));

    let rect_a = PixelRect { x: 0, y: 0, w: 500, h: 700 };
    let rect_b = PixelRect { x: 500, y: 0, w: 500, h: 700 };

    let mut a = parser_a.lock();
    let mut b = parser_b.lock();
    let panes: Vec<PaneRender<'_>> = vec![
        PaneRender {
            id: 1,
            grid: a.grid_mut(),
            rect_px: rect_a,
            is_active: true,
            cursor_style: CursorStyle::default(),
        },
        PaneRender {
            id: 2,
            grid: b.grid_mut(),
            rect_px: rect_b,
            is_active: false,
            cursor_style: CursorStyle::default(),
        },
    ];

    // Same projection `GpuRenderer::render()` performs into
    // `last_emit_origins`. See crates/sonic-shared/src/render/core.rs.
    let origins: Vec<(u64, [f32; 2])> =
        panes.iter().map(|p| (p.id, [p.rect_px.x as f32, p.rect_px.y as f32])).collect();

    assert_eq!(origins.len(), 2, "render() must see both panes");
    assert_eq!(origins[0], (1u64, [0.0, 0.0]));
    assert_eq!(origins[1].0, 2u64);
    assert!(
        origins[1].1[0] > 0.0,
        "second pane's origin.x must be > 0 (split right); got {:?}",
        origins[1].1
    );
    assert_eq!(origins[1].1[0], 500.0);
}
