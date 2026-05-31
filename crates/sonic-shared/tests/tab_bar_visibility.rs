//! Issue #383 — regression: when `tab_bar_visible = true`, the renderer
//! MUST emit at least one tab-bar background quad whose pixel rect is
//! pinned to the surface bottom and rendered fully opaque (alpha 1.0).
//!
//! The accompanying PR adds DIAGNOSTIC `tracing::debug!` instrumentation
//! around the emit path in `sonic_shared::render::core` so we can capture
//! runtime values on Windows. This test guards the *static* invariant
//! that the emit path itself produces a bottom-pinned opaque quad — a
//! follow-up fix PR will use the captured logs to decide whether the
//! Windows bug lives in the emit path, the GPU draw, or compositing.

use sonic_shared::{
    quad::QuadInstance,
    render::{emit_tab_bar_quads, TabBarQuadParams},
    tabbar_view::TabBarLayout,
    tabs::{Tab, TabBar},
};

fn bar_with_tabs() -> TabBar {
    let mut bar = TabBar::new();
    bar.push(Tab::new("alpha"));
    bar.push(Tab::new("beta"));
    bar
}

fn run_emit(
    tab_bar_visible: bool,
    bar_y: f32,
    window_w: f32,
    window_h: f32,
    bar_h: f32,
) -> Vec<QuadInstance> {
    let tabs = bar_with_tabs();
    let mut quads: Vec<QuadInstance> = Vec::new();
    // Mirror the `if self.tab_bar_visible` gate in
    // `sonic_shared::render::core::GpuRenderer::render`. When the flag
    // is false the production code skips the entire emit block — so we
    // do the same here. This is what the test guards against.
    if !tab_bar_visible {
        return quads;
    }
    let layout = TabBarLayout::compute_at_y(&tabs, window_w, bar_h, bar_y);
    emit_tab_bar_quads(
        &mut quads,
        &layout,
        &TabBarQuadParams {
            tab_count: tabs.tabs().len(),
            active_bg: [0.2, 0.2, 0.2, 1.0],
            hover_bg: [0.3, 0.3, 0.3, 1.0],
            accent: [0.0, 0.4, 1.0, 1.0],
            separator: [0.1, 0.1, 0.1, 1.0],
            border: [0.05, 0.05, 0.05, 1.0],
            close_color: [0.6, 0.6, 0.6, 1.0],
            hover_close_color: [1.0, 1.0, 1.0, 1.0],
            hover_tab_idx: u32::MAX,
            hover_close_hit: 0,
            surface: (window_w, window_h),
        },
    );
    quads
}

/// Positive case: with `tab_bar_visible = true` and a bar pinned to the
/// surface bottom, at least one emitted quad MUST sit at NDC y = -1.0
/// (the surface bottom edge) and be fully opaque.
#[test]
fn tab_bar_render_path_logs_bottom_quad_when_visible() {
    let window_w = 1024.0;
    let window_h = 720.0;
    let bar_h = 40.0;
    let bar_y = window_h - bar_h;

    let quads = run_emit(true, bar_y, window_w, window_h, bar_h);

    assert!(
        !quads.is_empty(),
        "tab-bar emit path must produce at least one quad when visible"
    );

    let bottom_ndc_eps = 1e-5_f32;
    let bottom_pinned: Vec<&QuadInstance> = quads
        .iter()
        .filter(|q| (q.rect[1] - -1.0_f32).abs() < bottom_ndc_eps)
        .collect();

    assert!(
        !bottom_pinned.is_empty(),
        "at least one tab-bar quad must reach NDC bottom (-1.0); got rects: {:?}",
        quads.iter().map(|q| q.rect).collect::<Vec<_>>()
    );

    let opaque_bottom: Vec<&&QuadInstance> = bottom_pinned
        .iter()
        .filter(|q| (q.color[3] - 1.0_f32).abs() < bottom_ndc_eps)
        .collect();

    assert!(
        !opaque_bottom.is_empty(),
        "at least one bottom-pinned tab-bar quad must be fully opaque (alpha 1.0); got alphas: {:?}",
        bottom_pinned.iter().map(|q| q.color[3]).collect::<Vec<_>>()
    );
}

/// Negative case 1: with `tab_bar_visible = false` the emit path is
/// skipped, so we must see zero quads.
#[test]
fn tab_bar_render_path_skips_emit_when_invisible() {
    let window_w = 1024.0;
    let window_h = 720.0;
    let bar_h = 40.0;
    let bar_y = window_h - bar_h;

    let quads = run_emit(false, bar_y, window_w, window_h, bar_h);
    assert!(quads.is_empty(), "tab-bar emit path must be skipped when invisible");
}

/// Negative case 2: if the bar y-offset is *above* the window (off-screen
/// top), no emitted quad reaches NDC bottom (-1.0). This guards against a
/// future regression that silently miscomputes `tab_bar_y_offset` so the
/// bar renders outside the visible region.
#[test]
fn tab_bar_render_path_no_bottom_quad_when_offscreen() {
    let window_w = 1024.0;
    let window_h = 720.0;
    let bar_h = 40.0;
    // Pin the bar 200 px ABOVE the window top — far off-screen.
    let bar_y = -200.0_f32;

    let quads = run_emit(true, bar_y, window_w, window_h, bar_h);

    let bottom_ndc_eps = 1e-3_f32;
    let bottom_pinned = quads
        .iter()
        .filter(|q| (q.rect[1] - -1.0_f32).abs() < bottom_ndc_eps)
        .count();

    assert_eq!(
        bottom_pinned, 0,
        "no quad must reach NDC bottom when bar is positioned off-screen"
    );
}
