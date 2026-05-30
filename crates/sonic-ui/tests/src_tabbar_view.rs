//! Tests for `caption_button_rects` (Windows-style caption strip layout).
//!
//! Migrated from inline `#[cfg(test)] mod caption_tests` in
//! `src/tabbar_view.rs`. Named `src_tabbar_view.rs` to distinguish from the
//! pre-existing `crates/sonic-shared/tests/tabbar_view.rs` (sibling crate).

use sonic_ui::tabbar_view::{
    caption_button_rects, TabBarLayout, CAPTION_BUTTON_HEIGHT, CAPTION_BUTTON_WIDTH,
};
use sonic_ui::tabs::{Tab, TabBar};

#[cfg(not(target_os = "windows"))]
use sonic_ui::tabbar_view::BAR_LEFT_PAD;

#[test]
fn caption_buttons_layout_right_to_left() {
    let [min, max, close] = caption_button_rects(1000, 1.0);
    assert!(min.x < max.x);
    assert!(max.x < close.x);
    assert_eq!(close.x + close.w, 1000.0);
    assert_eq!(min.w, CAPTION_BUTTON_WIDTH);
    assert_eq!(min.h, CAPTION_BUTTON_HEIGHT);
}

#[test]
fn caption_buttons_scale_with_dpi() {
    let [min, _, close] = caption_button_rects(2000, 2.0);
    assert_eq!(min.w, CAPTION_BUTTON_WIDTH * 2.0);
    assert_eq!(close.x + close.w, 2000.0);
}

#[test]
fn tabs_do_not_overlap_caption_buttons() {
    // Issue #335: the `+` new-tab button was removed. The remaining
    // constraint is that the last tab's right edge must not overlap
    // the caption-button strip on Windows.
    let mut bar = TabBar::new();
    bar.push(Tab::new("one"));
    let layout = TabBarLayout::compute(&bar, 1000.0);
    let [min, _, _] = caption_button_rects(1000, 1.0);
    let last = layout.tabs.last().unwrap();
    let last_right = last.bg.x + last.bg.w;
    #[cfg(target_os = "windows")]
    {
        assert!(
            last_right <= min.x,
            "last tab (right edge {last_right}) overlaps caption buttons (min.x = {})",
            min.x,
        );
    }
    #[cfg(not(target_os = "windows"))]
    {
        // On non-Windows the bar runs to within BAR_LEFT_PAD of the
        // right edge (a hair of breathing room).
        assert!(
            last_right <= 1000.0 - BAR_LEFT_PAD + 0.5,
            "last tab right edge {last_right} crossed right padding",
        );
        let _ = min;
    }
}
