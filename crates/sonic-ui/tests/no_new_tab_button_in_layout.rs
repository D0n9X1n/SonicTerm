//! Issue #335 regression-guard: the `+` new-tab button was removed from
//! the tab bar. Cmd+T / Ctrl+Shift+T / the command palette remain the
//! only ways to spawn a new tab.
//!
//! These tests pin the absence of the button at three layers:
//!   1. `TabBarLayout` no longer carries a `new_tab` Rect.
//!   2. `TabHit` no longer has a `NewTab` variant.
//!   3. The layout reclaims the freed horizontal space so tabs are
//!      *wider* than they were when a 28-px gutter was reserved.

use sonic_ui::tabbar_view::{TabBarLayout, TabHit};
use sonic_ui::tabs::{Tab, TabBar};

fn bar_with(n: usize) -> TabBar {
    let mut b = TabBar::new();
    for i in 0..n {
        b.push(Tab::new(format!("tab{i}")));
    }
    b
}

#[test]
fn layout_has_no_new_tab_field() {
    // Pure compile-time guard: any field added back named `new_tab`
    // would cause this destructure to need updating. The exhaustive
    // pattern (with `..`) tolerates other fields but documents the
    // canonical set after #335.
    let bar = bar_with(1);
    let layout = TabBarLayout::compute(&bar, 800.0);
    let TabBarLayout { bar: _, tabs: _, active: _, visible: _ } = layout;
}

#[test]
fn tab_hit_variants_do_not_include_new_tab() {
    // Exhaustive match: if a `NewTab` variant is re-added, this match
    // stops being exhaustive and the test fails to compile.
    let h = TabHit::Activate(0);
    match h {
        TabHit::Activate(_) => {}
        TabHit::Close(_) => {}
    }
}

#[test]
fn clicking_anywhere_on_bar_never_returns_new_tab_like_hit() {
    let bar = bar_with(0);
    let layout = TabBarLayout::compute(&bar, 800.0);
    // Sweep across the bar — every probe must miss (no tabs, no +).
    for x in (0..800).step_by(20) {
        let cy = layout.bar.y + layout.bar.h / 2.0;
        assert_eq!(layout.hit(x as f32, cy), None, "stray hit at x={x}");
    }
}

#[test]
fn removing_plus_button_widens_tabs() {
    // Before #335 the layout reserved 28 px for the `+` button plus
    // a TAB_GAP (6 px) and BAR_LEFT_PAD (12 px) on the right side.
    // Total freed: at least the 28-px button itself.
    //
    // Concrete numeric check at a fixed window width: with 2 tabs at
    // 800 px wide, each tab must now be > 380 px (previously ~370).
    let bar = bar_with(2);
    let layout = TabBarLayout::compute(&bar, 800.0);
    let w0 = layout.tabs[0].bg.w;
    let w1 = layout.tabs[1].bg.w;
    #[cfg(target_os = "windows")]
    {
        // Windows reserves the native caption-button strip on the right edge,
        // so the reclaimed `+` space is bounded by that OS chrome gutter.
        assert!(w0 > 300.0, "tab0 should reclaim available client chrome space, got {w0}");
        assert!(w1 > 300.0, "tab1 should reclaim available client chrome space, got {w1}");
    }
    #[cfg(not(target_os = "windows"))]
    {
        assert!(w0 > 380.0, "tab0 should be wider after +-button removal, got {w0}");
        assert!(w1 > 380.0, "tab1 should be wider after +-button removal, got {w1}");
    }
}
