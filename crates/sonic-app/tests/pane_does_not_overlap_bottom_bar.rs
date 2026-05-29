use sonic_core::config::Config;
use sonic_ui::{pane::Rect, tabbar_view::tab_bar_height};

#[test]
fn pane_rect_reserves_bottom_pinned_tab_bar() {
    let mut config = Config::default();
    config.window.padding_left = 12.0;
    config.window.padding_right = 8.0;
    config.window.padding_top = 4.0;
    config.window.padding_bottom = 6.0;
    config.font.size = 15.0;

    let window_w = 1000.0;
    let window_h = 700.0;
    let tab_strip_h = tab_bar_height(config.font.size);
    let top = config.window.padding_top;
    let bottom_inset = tab_strip_h;
    let outer = Rect::new(
        config.window.padding_left,
        top,
        (window_w - config.window.padding_left - config.window.padding_right).max(0.0),
        (window_h - top - bottom_inset - config.window.padding_bottom).max(0.0),
    );

    let pane_bottom = outer.y + outer.h;
    let tab_strip_top = window_h - tab_strip_h;
    assert!(
        pane_bottom <= tab_strip_top,
        "pane bottom {pane_bottom} must not overlap bottom tab strip starting at {tab_strip_top}"
    );
}
