//! User-reported: with only one tab, the × close button was invisible
//! — the renderer hover-gated it (and the layout's close rect was the
//! "right edge of the only tab" so the user had nowhere to aim).
//! Spec: the close × must be present and hit-testable for every tab
//! including the only tab, so Cmd+W and click both have a discoverable
//! target.
//!
//! This file pins the LAYOUT invariant: `TabBarLayout::compute` MUST
//! emit a non-zero `close_x_rect` for n=1 (and every n). The render-
//! time visibility change lives in `sonic-shared/src/render/core.rs`
//! and is exercised by the §13 GUI smoke; together they guarantee the
//! affordance is always reachable.

use sonic_ui::tabbar_view::{TabBarLayout, TabHit};
use sonic_ui::tabs::{Tab, TabBar};

fn bar_with(n: usize) -> TabBar {
    let mut b = TabBar::new();
    for i in 0..n {
        b.push(Tab::new(format!("tab{i}")));
    }
    b.activate(0);
    b
}

#[test]
fn single_tab_layout_has_visible_close_rect() {
    let bar = bar_with(1);
    let layout = TabBarLayout::compute(&bar, 1000.0);
    assert_eq!(layout.tabs.len(), 1);
    let t = &layout.tabs[0];
    assert!(
        t.close_x_rect.w > 0.0 && t.close_x_rect.h > 0.0,
        "n=1 close_x_rect must be a real rect (got w={}, h={})",
        t.close_x_rect.w,
        t.close_x_rect.h
    );
    // The close rect must be inside the tab's bg.
    assert!(
        t.close_x_rect.x >= t.bg_rect.x
            && t.close_x_rect.x + t.close_x_rect.w <= t.bg_rect.x + t.bg_rect.w + 0.5,
        "close × must be inside the only tab's bg"
    );
}

#[test]
fn single_tab_close_rect_is_hit_testable() {
    let bar = bar_with(1);
    let layout = TabBarLayout::compute(&bar, 1000.0);
    let t = &layout.tabs[0];
    let cx = t.close_x_rect.x + t.close_x_rect.w * 0.5;
    let cy = t.close_x_rect.y + t.close_x_rect.h * 0.5;
    // Hit-test at the center of the close rect; must yield Close(0).
    let hit = layout.hit(cx, cy);
    assert!(
        matches!(hit, Some(TabHit::Close(0))),
        "click at center of the only tab's × must yield Close(0), got {hit:?}"
    );
}

#[test]
fn close_rect_is_visible_for_every_n_not_just_multi_tab() {
    // Regression-guard against any future "hide × when n==1" guard.
    for n in [1usize, 2, 3, 5, 8] {
        let bar = bar_with(n);
        let layout = TabBarLayout::compute(&bar, 1200.0);
        assert_eq!(layout.tabs.len(), n);
        for (i, t) in layout.tabs.iter().enumerate() {
            assert!(
                t.close_x_rect.w > 0.0 && t.close_x_rect.h > 0.0,
                "tab {i}/{n}: close_x_rect must be non-zero (got {:?})",
                t.close_x_rect
            );
        }
    }
}
