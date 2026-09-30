use super::*;
use crate::tabs::Tab;
use std::time::Instant;

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

/// A crowded strip keeps a readable segment containing the active tab instead of shrinking every title.
#[test]
fn overflow_retains_readable_active_segment() {
    let mut bar = TabBar::new();
    for index in 0..16 {
        bar.push(Tab::new(format!("terminal {index}")));
    }
    for active in [0, 8, 15] {
        bar.activate(active);
        let layout = TabBarLayout::compute(&bar, 600.0);
        let active_rect = layout.active_indicator_rect().expect("active tab stays visible");
        let minimum = TAB_BAR_HEIGHT * 2.0 + TAB_INNER_PAD * 2.0;
        assert!(
            active_rect.w >= minimum,
            "active tab width {} is below the readable width {minimum}",
            active_rect.w
        );
        assert!(layout.tabs.len() < bar.len());
        assert!(layout.tabs.iter().all(|tab| tab.bg_rect.w >= minimum));
        assert!(layout.tabs.iter().any(|tab| tab.idx == active));
    }
}

/// Sparse widgets keep absolute hit and drop indices while hidden insertion gaps remain absent.
#[test]
fn overflow_hit_and_insertion_indices_match_the_visible_segment() {
    let mut bar = TabBar::new();
    for index in 0..10 {
        bar.push(Tab::new(format!("tab {index}")));
    }
    for active in [0, 5, 9] {
        bar.activate(active);
        let layout = TabBarLayout::compute(&bar, 360.0);
        let control = layout.overflow.expect("crowded strip has an overflow control");
        assert_eq!(layout.total_tabs, 10);
        assert_eq!(
            layout.hit(control.x + control.w * 0.5, control.y + control.h * 0.5),
            Some(TabHit::Overflow)
        );
        for (position, widget) in layout.tabs.iter().enumerate() {
            let probe_x = widget.bg_rect.x + widget.bg_rect.w * 0.25;
            assert_eq!(layout.hit(probe_x, layout.bar.y + 1.0), Some(TabHit::Activate(widget.idx)));
            assert_eq!(layout.drop_slot(probe_x, 1.0), widget.idx);
            let line = layout.insertion_x(widget.idx).unwrap();
            if position > 0 {
                let previous = &layout.tabs[position - 1];
                assert!(
                    line >= previous.bg_rect.x + previous.bg_rect.w && line <= widget.bg_rect.x
                );
            }
        }
        let last = layout.tabs.last().unwrap();
        let end_slot = last.idx + 1;
        assert_eq!(layout.drop_slot(last.bg_rect.x + last.bg_rect.w, 1.0), end_slot);
        assert_eq!(layout.drop_slot(control.x, 1.0), bar.len());
        assert!(layout.insertion_x(end_slot).unwrap() >= last.bg_rect.x + last.bg_rect.w);
        for slot in 0..=bar.len() {
            if !layout.tabs.iter().any(|widget| widget.idx == slot)
                && slot != end_slot
                && slot != bar.len()
            {
                assert_eq!(layout.insertion_x(slot), None);
            }
        }
        let preview = TabBarLayout::compute_with_insertion_slot(&bar, 360.0, 40.0, Some(active));
        assert!(preview.tabs.iter().all(|widget| widget.bg_rect.x + widget.bg_rect.w <= control.x));
        assert_eq!(layout.clone().with_top_offset(23.0).overflow.unwrap().y, control.y + 23.0);
        assert_eq!(layout.with_visible(false).hit(control.x + 1.0, 1.0), None);
    }
}

/// Narrow windows retain active/control hit zones without off-surface geometry across font and DPI scales.
#[test]
fn overflow_narrow_geometry_and_scale_keep_active_tab_reachable() {
    let mut bar = tab_bar(&["first", "middle", "last"]);
    for height in [36.0, 40.0, 60.0, 80.0] {
        for width in [0.0, 1.0, 24.0, 80.0, 160.0] {
            for active in 0..bar.len() {
                bar.activate(active);
                let layout = TabBarLayout::compute_with_height(&bar, width, height);
                let active = layout.active_indicator_rect().unwrap();
                let overflow = layout.overflow.unwrap();
                assert!(active.x >= 0.0 && active.x + active.w <= overflow.x);
                assert!(overflow.x >= 0.0 && overflow.x + overflow.w <= width);
                if width > 0.0 {
                    assert!(active.w > 0.0 && overflow.w > 0.0);
                }
            }
        }
    }
    assert!(TabBarLayout::compute(&TabBar::new(), 24.0).overflow.is_none());
    assert!(TabBarLayout::compute(&tab_bar(&["one"]), 24.0).overflow.is_none());
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

/// Scaled chrome can trigger overflow while preserving the active tab's title, color, and absolute index.
#[test]
fn compute_at_y_scales_tab_geometry_and_preserves_tab_state() {
    let mut bar = tab_bar(&["one", "two"]);
    bar.set_active_custom_color("#fabd2f");

    let layout = TabBarLayout::compute_at_y(&bar, 400.0, 80.0, 10.0);

    assert_eq!(layout.bar, Rect { x: 0.0, y: 10.0, w: 400.0, h: 80.0 });
    assert_eq!(layout.active, Some(1));
    assert_close(layout.tabs[0].bg_rect.x, 0.0);
    assert_close(layout.tabs[0].bg_rect.y, 14.0);
    assert_eq!(layout.tabs.len(), 1);
    assert_eq!(layout.tabs[0].idx, 1);
    assert_close(layout.tabs[0].bg_rect.w, 312.0);
    assert_close(layout.tabs[0].bg_rect.h, 72.0);
    assert_close(layout.tabs[0].title_rect.x, 20.0);
    assert_close(layout.tabs[0].title_rect.w, 272.0);
    assert_eq!(layout.tabs[0].title, "two");
    assert_eq!(layout.tabs[0].custom_color.as_deref(), Some("#fabd2f"));
    assert!(layout.tabs[0].active);
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
        layout.insertion_x(layout.total_tabs),
        Some(last.bg_rect.x + last.bg_rect.w + TAB_GAP * 0.5)
    );

    assert_eq!(layout.clone().with_visible(false).insertion_x(1), None);
    assert_eq!(TabBarLayout::compute(&TabBar::new(), 500.0).drop_slot(20.0, 20.0), 0);
    assert_eq!(TabBarLayout::compute(&TabBar::new(), 500.0).insertion_x(0), None);
}

#[test]
fn tear_out_and_inset_helpers_cover_their_threshold_branches() {
    // The detector uses the provided bar bounds while inset helpers retain their layout contracts.
    let layout = TabBarLayout::compute(&tab_bar(&["one"]), 400.0);
    assert_eq!(detect_tear_out(3, (12.0, 79.999), &layout), None);
    assert_eq!(
        detect_tear_out(3, (12.0, TAB_BAR_HEIGHT + TEAR_OUT_THRESHOLD_PX), &layout),
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

/// A bar whose tabs carry the content widths the renderer would have stored for them.
fn measured_bar(tabs: &[(&str, f32)]) -> TabBar {
    let mut bar = TabBar::new();
    for (title, _) in tabs {
        bar.push(Tab::new(*title));
    }
    bar.refresh_content_widths(Instant::now(), false, 1, false, |content| {
        tabs.iter().find(|(title, _)| *title == content.title).map(|(_, width_px)| *width_px)
    });
    bar
}

fn assert_widths(layout: &TabBarLayout, expected: &[f32]) {
    let actual: Vec<f32> = layout.tabs.iter().map(|tab| tab.bg_rect.w).collect();
    assert_eq!(actual.len(), expected.len(), "tab widths {actual:?}, expected {expected:?}");
    for (actual_w, expected_w) in actual.iter().zip(expected) {
        assert_close(*actual_w, *expected_w);
    }
}

#[test]
fn a_short_title_gets_a_narrower_tab_than_a_long_one() {
    // Tabs size to their measured titles, and a click, a drop, the insertion line and the
    // drag preview all follow the drawn widths rather than an even share.
    let bar = measured_bar(&[("zsh", 30.0), ("cargo build --release", 190.0)]);
    let layout = TabBarLayout::compute_at_y_with_max(&bar, 1200.0, 40.0, 0.0, TAB_MAX_WIDTH);

    assert_widths(&layout, &[100.0, 210.0]);
    assert_close(layout.tabs[1].bg_rect.x, 104.0);
    assert_close(layout.tabs[1].title_rect.w, 190.0);
    assert_eq!(layout.hit(50.0, 20.0), Some(TabHit::Activate(0)));
    assert_eq!(layout.hit(200.0, 20.0), Some(TabHit::Activate(1)));
    assert_eq!(layout.drop_slot(49.0, 20.0), 0);
    assert_eq!(layout.drop_slot(51.0, 20.0), 1);
    assert_eq!(layout.drop_slot(208.0, 20.0), 1);
    assert_eq!(layout.drop_slot(210.0, 20.0), 2);
    assert_eq!(layout.insertion_x(1), Some(102.0));
    let preview = TabBarLayout::compute_with_insertion_slot(&bar, 1200.0, 40.0, Some(1));
    assert_close(preview.tabs[1].bg_rect.x, 104.0 + TabBarLayout::INSERTION_GAP_PX);
    assert_close(preview.tabs[1].bg_rect.w, 210.0);

    let lone = measured_bar(&[("zsh", 30.0)]);
    let lone_layout = TabBarLayout::compute_at_y_with_max(&lone, 1200.0, 40.0, 0.0, TAB_MAX_WIDTH);
    assert_widths(&lone_layout, &[100.0]);
}

#[test]
fn a_long_title_under_the_maximum_shows_whole_when_the_strip_has_room() {
    // With room to spare each tab takes its preferred width, so seven short titles stay
    // narrow and the long title keeps its whole measured width.
    let bar = measured_bar(&[
        ("zsh", 30.0),
        ("vim", 30.0),
        ("git", 30.0),
        ("top", 30.0),
        ("ssh", 30.0),
        ("man", 30.0),
        ("tig", 30.0),
        ("cargo test --workspace", 200.0),
    ]);
    let layout = TabBarLayout::compute_at_y_with_max(&bar, 1200.0, 40.0, 0.0, TAB_MAX_WIDTH);

    assert!(layout.overflow.is_none());
    assert_widths(&layout, &[100.0, 100.0, 100.0, 100.0, 100.0, 100.0, 100.0, 220.0]);
    assert_close(layout.tabs[7].title_rect.w, 200.0);
}

#[test]
fn a_title_wider_than_the_maximum_is_capped_beside_a_narrower_neighbour() {
    // `tab_max_width` caps one tab: a very long title stops at the maximum while a short
    // neighbour keeps its own narrower width.
    let bar = measured_bar(&[("zsh", 30.0), ("tail -f /var/log/system.log", 400.0)]);
    let layout = TabBarLayout::compute_at_y_with_max(&bar, 1200.0, 40.0, 0.0, TAB_MAX_WIDTH);

    assert_widths(&layout, &[100.0, 240.0]);
    assert_close(layout.tabs[1].title_rect.w, 220.0);
}

#[test]
fn a_crowded_strip_shrinks_the_widest_tabs_first_and_keeps_short_titles_whole() {
    // When the preferred widths overflow the strip, only the widest tabs shrink, to one
    // common cap, so short titles stay whole and the strip fills exactly.
    let bar =
        measured_bar(&[("zsh", 30.0), ("htop -d 10", 160.0), ("cargo build --release", 220.0)]);
    let layout = TabBarLayout::compute_at_y_with_max(&bar, 596.0, 40.0, 0.0, TAB_MAX_WIDTH);

    assert!(layout.overflow.is_none());
    assert_widths(&layout, &[100.0, 180.0, 212.0]);
    let last = layout.tabs.last().expect("three tabs");
    assert_close(last.bg_rect.x + last.bg_rect.w, 596.0 - TAB_END_DROP_ZONE_PX);

    let shared =
        measured_bar(&[("zsh", 30.0), ("htop -d 10", 400.0), ("cargo build --release", 400.0)]);
    let shared_layout =
        TabBarLayout::compute_at_y_with_max(&shared, 596.0, 40.0, 0.0, TAB_MAX_WIDTH);
    assert_widths(&shared_layout, &[100.0, 196.0, 196.0]);
}

#[test]
fn the_overflow_threshold_and_the_lone_tab_rule_do_not_depend_on_titles() {
    // Overflow starts exactly where the readable minimum stops fitting, and a lone tab in a
    // very narrow window takes the whole bar, whatever the measured titles are.
    let titles = [
        ("zsh", 30.0),
        ("vim", 30.0),
        ("git", 30.0),
        ("top", 30.0),
        ("cargo build --release", 400.0),
    ];
    let bar = measured_bar(&titles);
    let fits = TabBarLayout::compute_at_y_with_max(&bar, 612.0, 40.0, 0.0, TAB_MAX_WIDTH);
    assert!(fits.overflow.is_none());
    assert_widths(&fits, &[100.0; 5]);

    let crowded = TabBarLayout::compute_at_y_with_max(&bar, 611.0, 40.0, 0.0, TAB_MAX_WIDTH);
    assert!(crowded.overflow.is_some());
    assert!(crowded.tabs.len() < titles.len());
    assert!(crowded.tabs.iter().all(|tab| tab.bg_rect.w >= 100.0));

    let lone = measured_bar(&[("cargo build --release", 400.0)]);
    let narrow = TabBarLayout::compute_at_y_with_max(&lone, 150.0, 40.0, 0.0, TAB_MAX_WIDTH);
    assert!(narrow.overflow.is_none());
    assert_widths(&narrow, &[150.0]);
}

#[test]
fn unmeasured_tabs_keep_the_even_share_capped_at_the_maximum() {
    // Before the renderer measures a tab it prefers the maximum, so a bar laid out before
    // the first frame shares the strip evenly, capped at the maximum.
    let pair = tab_bar(&["one", "two"]);
    let roomy = TabBarLayout::compute_at_y_with_max(&pair, 1200.0, 40.0, 0.0, TAB_MAX_WIDTH);
    assert_widths(&roomy, &[240.0, 240.0]);

    let trio = tab_bar(&["one", "two", "three"]);
    let shared = TabBarLayout::compute_at_y_with_max(&trio, 600.0, 40.0, 0.0, TAB_MAX_WIDTH);
    let share = (600.0 - TAB_END_DROP_ZONE_PX - 2.0 * TAB_GAP) / 3.0;
    assert_widths(&shared, &[share, share, share]);
}

#[test]
fn a_new_tab_max_width_lays_out_the_stored_widths_again() {
    // A `tab_max_width` reload re-lays the bar out from the stored widths, the readable
    // minimum wins over a smaller maximum, and raster widths are not scaled again.
    let bar = measured_bar(&[("zsh", 30.0), ("tail -f /var/log/system.log", 400.0)]);
    let roomy = TabBarLayout::compute_at_y_with_max(&bar, 1200.0, 40.0, 0.0, 300.0);
    assert_widths(&roomy, &[100.0, 300.0]);
    let tight = TabBarLayout::compute_at_y_with_max(&bar, 1200.0, 40.0, 0.0, 150.0);
    assert_widths(&tight, &[100.0, 150.0]);
    let below_minimum = TabBarLayout::compute_at_y_with_max(&bar, 1200.0, 40.0, 0.0, 50.0);
    assert_widths(&below_minimum, &[100.0, 100.0]);

    let retina = measured_bar(&[("zsh", 60.0), ("tail -f /var/log/system.log", 800.0)]);
    let scaled = TabBarLayout::compute_at_y_with_max(&retina, 2400.0, 80.0, 0.0, 300.0);
    assert_widths(&scaled, &[200.0, 600.0]);
}
