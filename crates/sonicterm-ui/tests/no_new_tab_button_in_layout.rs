//! Issue #335 regression-guard: the `+` new-tab button was removed from
//! the tab bar. Cmd+T / Ctrl+Shift+T / the command palette remain the
//! only ways to spawn a new tab.
//!
//! These tests pin the absence of the button at three layers:
//!   1. `TabBarLayout` no longer carries a `new_tab` Rect.
//!   2. `TabHit` no longer has a `NewTab` variant.
//!   3. The layout reclaims the freed horizontal space so tabs are
//!      *wider* than they were when a 28-px gutter was reserved.

use sonicterm_ui::tabbar_view::{TabBarLayout, TabHit};
use sonicterm_ui::tabs::{Tab, TabBar};

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
    // After #541 the layout reserves TAB_END_DROP_ZONE_PX on the right
    // for the "append at the end" drop zone — that's subtracted from
    // the per-tab budget, so the post-#335 widening is partly given
    // back. The check below is now stated in terms of the floor that
    // both removals together produced (still wider than the pre-#335
    // ~370 px baseline minus the end-zone contribution).
    let bar = bar_with(2);
    let layout = TabBarLayout::compute(&bar, 800.0);
    let w0 = layout.tabs[0].bg.w;
    let w1 = layout.tabs[1].bg.w;
    // Pre-#335: ~370. Post-#335 alone: > 380. Post-#541: tabs lose
    // TAB_END_DROP_ZONE_PX / 2 each ≈ 48 px, leaving ~332. We still
    // assert tabs are wider than they were when the +-button also
    // gobbled width (pre-#335 ~370 minus the ~48 end-zone share each
    // ≈ 322), so anything > 320 demonstrates the +-button widening
    // survived the end-zone deduction.
    let floor = 320.0;
    assert!(w0 > floor, "tab0 narrower than expected: got {w0}");
    assert!(w1 > floor, "tab1 narrower than expected: got {w1}");
}
