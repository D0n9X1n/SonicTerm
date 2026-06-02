use sonicterm_gpu::core::{emit_tab_bar_quads, TabBarQuadParams};
use sonicterm_gpu::quad::QuadInstance;
use sonicterm_ui::{
    tabbar_view::TabBarLayout,
    tabs::{Tab, TabBar},
};

fn bar_with_tabs() -> TabBar {
    let mut bar = TabBar::new();
    bar.push(Tab::new("alpha"));
    bar.push(Tab::new("beta"));
    bar
}

#[test]
fn tab_bar_quads_are_emitted_in_window_bottom_region_without_titlebar_gate() {
    let window_w = 800.0;
    let window_h = 600.0;
    let bar_h = 40.0;
    let bar_y = window_h - bar_h;
    let tabs = bar_with_tabs();
    let layout = TabBarLayout::compute_at_y(&tabs, window_w, bar_h, bar_y);
    let mut quads: Vec<QuadInstance> = Vec::new();

    emit_tab_bar_quads(
        &mut quads,
        &layout,
        &TabBarQuadParams {
            tab_count: tabs.tabs().len(),
            active_bg: [0.2, 0.2, 0.2, 1.0],
            hover_bg: [0.3, 0.3, 0.3, 1.0],
            accent: [0.0, 0.4, 1.0, 1.0],
            separator: [0.1, 0.1, 0.1, 1.0],
            border: [0.0, 0.0, 0.0, 1.0],
            close_color: [0.6, 0.6, 0.6, 1.0],
            hover_close_color: [1.0, 1.0, 1.0, 1.0],
            hover_tab_idx: u32::MAX,
            hover_close_hit: 0,
            surface: (window_w, window_h),
        },
    );

    assert!(!quads.is_empty(), "tab bar paint must not be gated on native titlebar mode");

    let expected_bottom_ndc = -1.0_f32;
    let expected_bar_top_ndc = 1.0 - 2.0 * (bar_y / window_h);
    let background = &quads[0];
    let bottom_ndc = background.rect[1];
    let top_ndc = background.rect[1] + background.rect[3];

    assert!((bottom_ndc - expected_bottom_ndc).abs() < 1e-5, "tab strip must reach client bottom");
    assert!(
        (top_ndc - expected_bar_top_ndc).abs() < 1e-5,
        "tab strip must stay inside client rect"
    );
}
