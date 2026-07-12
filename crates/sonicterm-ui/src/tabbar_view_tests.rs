use super::*;
use crate::tabs::Tab;

fn tab_bar(titles: &[&str]) -> TabBar {
    let mut bar = TabBar::new();
    for title in titles {
        bar.push(Tab::new(*title));
    }
    bar
}

fn assert_close(actual: f32, expected: f32) {
    assert!((actual - expected).abs() < 0.001, "expected {expected}, got {actual}");
}

#[test]
fn rectangles_and_widgets_use_half_open_hit_boundaries() {
    let rect = Rect { x: 10.0, y: 20.0, w: 30.0, h: 40.0 };
    assert!(rect.contains(10.0, 20.0));
    assert!(rect.contains(39.999, 59.999));
    assert!(!rect.contains(40.0, 20.0));
    assert!(!rect.contains(10.0, 60.0));

    let layout = TabBarLayout::compute(&tab_bar(&["one"]), 300.0);
    let widget = &layout.tabs[0];
    let inside = Point { x: widget.bg_rect.x, y: widget.bg_rect.y };
    let outside = Point { x: widget.bg_rect.x + widget.bg_rect.w, y: widget.bg_rect.y };
    assert_eq!(widget.hit(inside), Some(TabAction::Activate(0)));
    assert_eq!(widget.hover_at(Some(inside)), TabHover::Body);
    assert_eq!(widget.hit(outside), None);
    assert_eq!(widget.hover_at(Some(outside)), TabHover::None);
    assert_eq!(widget.hover_at(None), TabHover::None);
}

#[test]
fn layout_hit_uses_the_full_bar_height_but_excludes_gaps_and_hidden_bars() {
    let layout = TabBarLayout::compute(&tab_bar(&["one", "two"]), 400.0);
    let first = &layout.tabs[0];
    let first_x = first.bg_rect.x + first.bg_rect.w * 0.5;

    assert_eq!(layout.hit(first_x, layout.bar.y), Some(TabHit::Activate(0)));
    assert_eq!(layout.hit(first_x, layout.bar.y + layout.bar.h - 0.001), Some(TabHit::Activate(0)));

    let gap_x = first.bg_rect.x + first.bg_rect.w + TAB_GAP * 0.5;
    assert_eq!(layout.hit(gap_x, layout.bar.y + layout.bar.h * 0.5), None);
    assert_eq!(layout.hit(first_x, layout.bar.y + layout.bar.h), None);

    let hidden = layout.clone().with_visible(false);
    assert_eq!(hidden.hit(first_x, hidden.bar.y + 1.0), None);
    assert!(!hidden.point_over_bar(first_x, hidden.bar.y + 1.0));
}

#[test]
fn compute_at_y_scales_tab_geometry_and_preserves_tab_state() {
    let mut bar = tab_bar(&["one", "two"]);
    bar.set_active_custom_color("#fabd2f");

    let layout = TabBarLayout::compute_at_y(&bar, 400.0, 80.0, 10.0);

    assert_eq!(layout.bar, Rect { x: 0.0, y: 10.0, w: 400.0, h: 80.0 });
    assert_eq!(layout.active, Some(1));
    assert_close(layout.tabs[0].bg_rect.x, 0.0);
    assert_close(layout.tabs[0].bg_rect.y, 14.0);
    assert_close(layout.tabs[0].bg_rect.w, 100.0);
    assert_close(layout.tabs[0].bg_rect.h, 72.0);
    assert_close(layout.tabs[0].title_rect.x, 20.0);
    assert_close(layout.tabs[0].title_rect.w, 60.0);
    assert_close(layout.tabs[1].bg_rect.x, 108.0);
    assert_eq!(layout.tabs[1].title, "two");
    assert_eq!(layout.tabs[1].custom_color.as_deref(), Some("#fabd2f"));
    assert!(layout.tabs[1].active);
}

#[test]
fn insertion_preview_shifts_only_tabs_at_or_after_the_slot() {
    let bar = tab_bar(&["one", "two", "three"]);
    let base = TabBarLayout::compute_with_height(&bar, 500.0, 40.0);
    let preview = TabBarLayout::compute_with_insertion_slot(&bar, 500.0, 40.0, Some(1));

    assert_close(preview.tabs[0].bg_rect.x, base.tabs[0].bg_rect.x);
    for index in 1..3 {
        assert_close(
            preview.tabs[index].bg_rect.x,
            base.tabs[index].bg_rect.x + TabBarLayout::INSERTION_GAP_PX,
        );
        assert_close(
            preview.tabs[index].title_rect.x,
            base.tabs[index].title_rect.x + TabBarLayout::INSERTION_GAP_PX,
        );
        assert_eq!(preview.tabs[index].bg, preview.tabs[index].bg_rect);
        assert_eq!(preview.tabs[index].close, preview.tabs[index].close_x_rect);
    }

    let after_last = TabBarLayout::compute_with_insertion_slot(&bar, 500.0, 40.0, Some(bar.len()));
    for (actual, expected) in after_last.tabs.iter().zip(base.tabs.iter()) {
        assert_close(actual.bg_rect.x, expected.bg_rect.x);
    }
}

#[test]
fn active_indicator_and_accent_follow_the_active_widget() {
    let layout = TabBarLayout::compute(&tab_bar(&["one", "two"]), 400.0);
    let active = layout.tabs[1].bg_rect;

    assert_eq!(layout.active_indicator_rect(), Some(active));
    assert_eq!(
        layout.active_accent_rect(),
        Some(Rect {
            x: active.x + ACTIVE_TOP_ACCENT_INSET,
            y: active.y + 1.0,
            w: active.w - ACTIVE_TOP_ACCENT_INSET * 2.0,
            h: ACTIVE_TOP_ACCENT_H,
        })
    );

    let mut stale = layout;
    stale.active = Some(99);
    assert_eq!(stale.active_indicator_rect(), None);
    assert_eq!(stale.active_accent_rect(), None);
}

#[test]
fn top_offsets_shift_every_hit_tested_rectangle_and_clamp_negative_values() {
    let base = TabBarLayout::compute_at_y(&tab_bar(&["one"]), 300.0, 40.0, 5.0);
    let unchanged = base.clone().with_top_offset(-20.0);
    assert_eq!(unchanged.bar.y, base.bar.y);
    assert_eq!(unchanged.tabs[0].bg_rect.y, base.tabs[0].bg_rect.y);

    let shifted = base.with_top_offset(12.0);
    assert_close(shifted.bar.y, 17.0);
    assert_close(shifted.tabs[0].bg_rect.y, 19.0);
    assert_close(shifted.tabs[0].title_rect.y, 19.0);
    assert_eq!(shifted.tabs[0].bg, shifted.tabs[0].bg_rect);
    assert_eq!(shifted.bar_y_range(), (17.0, 57.0));
    assert!(shifted.point_over_bar(1.0, 17.0));
}

#[test]
fn drop_slots_switch_at_midpoints_and_insertion_positions_clamp() {
    let layout = TabBarLayout::compute(&tab_bar(&["one", "two", "three"]), 500.0);
    let first_mid = layout.tabs[0].bg_rect.x + layout.tabs[0].bg_rect.w * 0.5;
    let last = &layout.tabs[2];
    let last_mid = last.bg_rect.x + last.bg_rect.w * 0.5;

    assert_eq!(layout.drop_slot(first_mid - 0.001, 0.0), 0);
    assert_eq!(layout.drop_slot(first_mid, 0.0), 1);
    assert_eq!(layout.drop_slot(last_mid, 0.0), 3);

    assert_eq!(layout.insertion_x(0), Some(layout.tabs[0].bg_rect.x - TAB_GAP * 0.5));
    let middle =
        (layout.tabs[0].bg_rect.x + layout.tabs[0].bg_rect.w + layout.tabs[1].bg_rect.x) * 0.5;
    assert_eq!(layout.insertion_x(1), Some(middle));
    assert_eq!(
        layout.insertion_x(usize::MAX),
        Some(last.bg_rect.x + last.bg_rect.w + TAB_GAP * 0.5)
    );

    assert_eq!(layout.clone().with_visible(false).insertion_x(1), None);
    assert_eq!(TabBarLayout::compute(&TabBar::new(), 500.0).drop_slot(20.0, 20.0), 0);
    assert_eq!(TabBarLayout::compute(&TabBar::new(), 500.0).insertion_x(0), None);
}

#[test]
fn tear_out_and_inset_helpers_cover_their_threshold_branches() {
    assert_eq!(detect_tear_out(3, (12.0, 79.999)), None);
    assert_eq!(
        detect_tear_out(3, (12.0, TAB_BAR_HEIGHT + TEAR_OUT_THRESHOLD_PX)),
        Some(TearOut {
            tab_index: 3,
            drop_position: (12.0, TAB_BAR_HEIGHT + TEAR_OUT_THRESHOLD_PX),
        })
    );

    assert_eq!(tab_bar_height(10.0), 36.0);
    assert_eq!(tab_bar_height(15.0), 42.0);
    assert_eq!(tab_bar_top_inset(false, 3.0), 3.0);
    assert_eq!(tab_bar_top_inset(true, 3.0), TAB_BAR_HEIGHT + 3.0);
    assert_eq!(tab_bar_top_inset_with_titlebar(false, 3.0, 24.0), 27.0);
    assert_eq!(tab_bar_top_inset_with_titlebar(true, 3.0, 24.0), 67.0);
}
