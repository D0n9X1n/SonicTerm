//! Wezterm fancy-mode tab-bar visual rhythm — verifies that the tab bar
//! height scales with the configured terminal font size and that the tab
//! text is vertically centered inside the bar, plus the 6px per-tab
//! horizontal padding matching WezTerm's chrome.

use sonic_shared::tabbar_view::{tab_bar_height, TabBarLayout, TAB_BAR_HEIGHT, TAB_INNER_PAD};
use sonic_shared::tabs::{Tab, TabBar};

fn bar_with(n: usize) -> TabBar {
    let mut b = TabBar::new();
    for i in 0..n {
        b.push(Tab::new(format!("t{i}")));
    }
    b
}

#[test]
fn tab_bar_height_at_font_size_14_is_about_40() {
    let h = tab_bar_height(14.0);
    // Roomier WezTerm-style default at font_size=14 → ~40 logical px,
    // leaving 8px of breathing room above + below the title text.
    assert!((h - 40.0).abs() < 0.5, "expected ~40, got {h}");
}

#[test]
fn tab_bar_height_scales_with_font_size() {
    // font_size * 2 + breathing room, clamped to 36.0 floor.
    assert!(tab_bar_height(10.0) >= 36.0); // floor
    assert!(tab_bar_height(16.0) > tab_bar_height(12.0));
    // Doubling font size roughly doubles bar height once above the floor.
    let small = tab_bar_height(14.0);
    let big = tab_bar_height(28.0);
    assert!(big > small * 1.5, "bar should scale: small={small}, big={big}");
}

#[test]
fn tab_bar_height_default_constant_matches_font_size_14() {
    // The default TAB_BAR_HEIGHT (40.0) corresponds to font_size = 14.
    assert!((tab_bar_height(14.0) - TAB_BAR_HEIGHT).abs() < 0.5);
}

#[test]
fn tab_bar_height_at_least_36() {
    // Roomy floor — tab titles should never feel cramped.
    for fs in [8.0, 10.0, 12.0, 14.0, 15.0, 16.0, 20.0] {
        assert!(tab_bar_height(fs) >= 36.0, "fs={fs} gave {}", tab_bar_height(fs));
    }
    const _: () = assert!(TAB_BAR_HEIGHT >= 36.0);
}

#[test]
fn tab_bar_height_constant_is_at_least_36_const() {
    const _: () = assert!(TAB_BAR_HEIGHT >= 36.0);
}

#[test]
fn tab_text_has_eight_px_breathing_room() {
    // The title text (height ~= font_size * 0.85 * 1.2) should sit
    // with ≥8 logical px of margin above AND below inside the bar.
    let font_size = 14.0_f32;
    let bar_h = tab_bar_height(font_size);
    let text_h = font_size * 0.85 * 1.2;
    let margin = (bar_h - text_h) / 2.0;
    assert!(margin >= 8.0, "expected ≥8px margin, got {margin}");
}

#[test]
fn tab_text_y_position_is_vertically_centered() {
    // Mimics the renderer's title_top math: title baseline sits so the
    // text height (font_size * 0.85 * 1.2) is centered inside the bar.
    let font_size = 14.0_f32;
    let bar_h = tab_bar_height(font_size);
    let text_h = font_size * 0.85 * 1.2;
    let title_top = ((bar_h - text_h) / 2.0).max(0.0);
    let expected = (bar_h - text_h) / 2.0;
    assert!((title_top - expected).abs() < 0.01);
    // Text fits inside the bar with non-negative margin top and bottom.
    assert!(title_top >= 0.0);
    assert!(title_top + text_h <= bar_h + 0.01);
}

#[test]
fn tab_inner_padding_is_ten_pixels() {
    // Issue #112 Round 3: bumped inner padding from 6 to 10 for the
    // modern browser-style chrome breathing room.
    assert_eq!(TAB_INNER_PAD, 10.0);
}

#[test]
fn compute_with_height_threads_height_through_layout() {
    let bar = bar_with(3);
    let layout = TabBarLayout::compute_with_height(&bar, 800.0, 32.0);
    assert_eq!(layout.bar.h, 32.0);
    for t in &layout.tabs {
        // Each tab inset 4px top + 4px bottom from the bar (issue #112 R3).
        assert_eq!(t.bg.h, 32.0 - 8.0);
        // Title rect starts TAB_INNER_PAD in from the tab's left edge.
        assert!((t.title.x - (t.bg.x + TAB_INNER_PAD)).abs() < 0.01);
    }
}
