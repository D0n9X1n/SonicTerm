//! Regression tests for issue #171: the amber/blue active-tab top
//! accent bar was painting in the empty gutter to the right of the
//! last tab (between the last tab and the `+` button) instead of
//! underlining the active tab itself.
//!
//! Root cause defense-in-depth: the renderer now reads the accent
//! rect from [`TabBarLayout::active_accent_rect`] rather than
//! computing it inline. These tests pin the helper's behaviour so a
//! future refactor cannot silently re-introduce the off-by-one.

use sonic_ui::tabbar_view::{TabBarLayout, ACTIVE_TOP_ACCENT_H, TAB_MIN_WIDTH};
use sonic_ui::tabs::{Tab, TabBar};

fn bar_with_active(n: usize, active: usize) -> TabBar {
    let mut b = TabBar::new();
    for i in 0..n {
        b.push(Tab::new(format!("#{} Administrator: cmd.exe", i + 1)));
    }
    b.activate(active);
    b
}

/// Helper: anchor invariant — the accent's rect MUST equal the active
/// tab's current post-layout bg rect (with only height reduced to 2px), NOT
/// `tabs[active + 1]` (the off-by-one in #171) and NOT the full strip width
/// (the overshoot in #257).
fn assert_accent_on_active(layout: &TabBarLayout, active_idx: usize) {
    let acc = layout.active_accent_rect().expect("active rect must exist");
    let t = &layout.tabs[active_idx];

    assert!((acc.x - t.bg.x).abs() < 0.01, "accent.x must equal active tab x");

    // And explicitly: NOT the off-by-one position the bug report
    // showed. When active_idx is the last tab we check against the
    // right edge of the bar; for inner tabs we check against the
    // next tab.
    let bug_x = if active_idx + 1 < layout.tabs.len() {
        layout.tabs[active_idx + 1].bg.x
    } else {
        layout.bar.w - 28.0
    };
    assert!(
        (acc.x - bug_x).abs() > 1.0,
        "accent.x ({}) is suspiciously close to the OFF-BY-ONE position ({}) \
         from issue #171",
        acc.x,
        bug_x,
    );

    // Y / H pinned to the active tab's bg.y and the 2px constant.
    assert!((acc.y - t.bg.y).abs() < 0.01, "accent.y must match bg.y");
    assert!((acc.h - ACTIVE_TOP_ACCENT_H).abs() < 0.01, "accent.h must be 2px");

    // Issue #257: width pinned to the active tab's actual width after
    // slack distribution, not the remaining tab-strip area.
    assert!((acc.w - t.bg.w).abs() < 0.01, "accent.w must equal active tab width");

    // And the accent must fully fall inside the active tab's bg rect.
    assert!(acc.x >= t.bg.x - 0.01);
    assert!(acc.x + acc.w <= t.bg.x + t.bg.w + 0.01);
}

#[test]
fn accent_on_only_tab_when_n_is_1() {
    let bar = bar_with_active(1, 0);
    let layout = TabBarLayout::compute(&bar, 1000.0);
    assert_accent_on_active(&layout, 0);
}

#[test]
fn accent_on_first_when_n_is_2() {
    let bar = bar_with_active(2, 0);
    let layout = TabBarLayout::compute(&bar, 1000.0);
    assert_accent_on_active(&layout, 0);
}

#[test]
fn accent_on_last_when_n_is_2() {
    // This is the exact scenario from issue #171: window 1000 px,
    // 2 tabs, active = tab 2 — the screenshot showed the accent
    // floating in the dead area between tab 2 and the `+` button.
    let bar = bar_with_active(2, 1);
    let layout = TabBarLayout::compute(&bar, 1000.0);
    assert_accent_on_active(&layout, 1);
}

#[test]
fn accent_on_first_when_n_is_5() {
    let bar = bar_with_active(5, 0);
    let layout = TabBarLayout::compute(&bar, 1600.0);
    assert_accent_on_active(&layout, 0);
}

#[test]
fn accent_on_last_when_n_is_5() {
    let bar = bar_with_active(5, 4);
    let layout = TabBarLayout::compute(&bar, 1600.0);
    assert_accent_on_active(&layout, 4);
}

#[test]
fn accent_on_middle_when_n_is_5() {
    let bar = bar_with_active(5, 2);
    let layout = TabBarLayout::compute(&bar, 1600.0);
    assert_accent_on_active(&layout, 2);
}

#[test]
fn accent_is_none_on_empty_bar() {
    let bar = TabBar::new();
    let layout = TabBarLayout::compute(&bar, 1000.0);
    assert!(layout.active_accent_rect().is_none());
}

#[test]
fn accent_never_drifts_off_its_own_active_tab() {
    // Walk a range of window widths and tab counts that span the
    // narrow / typical / wide regimes. For each combo the accent
    // MUST stay anchored to its own active tab's `bg` rect — never
    // drift onto a neighbouring tab or into the right-edge empty
    // area of the bar.
    //
    // Note: under the TAB_MIN_WIDTH floor the bar may overflow on
    // the right when many tabs don't fit; that is OK and
    // intentional (issue #171 second bug). The accent just has to
    // ride on its own tab, wherever that tab ends up.
    for n in 1..=6 {
        for w in [600.0_f32, 800.0, 1000.0, 1280.0, 1600.0, 2000.0] {
            for active in 0..n {
                let bar = bar_with_active(n, active);
                let layout = TabBarLayout::compute(&bar, w);
                let acc = layout
                    .active_accent_rect()
                    .expect("active accent must exist when tabs are present");
                let t = &layout.tabs[active];
                assert!(
                    acc.x >= t.bg.x - 0.01 && acc.x + acc.w <= t.bg.x + t.bg.w + 0.01,
                    "n={n} w={w} active={active}: accent [{}..{}] is NOT inside \
                     tab.bg [{}..{}] — bug #171 regression",
                    acc.x,
                    acc.x + acc.w,
                    t.bg.x,
                    t.bg.x + t.bg.w,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Minimum-width regression (issue #171, second bug)
// ---------------------------------------------------------------------------

#[test]
fn tab_min_width_is_at_least_200_px() {
    // `>=` rather than `==` so a future bump (e.g. 220) still passes
    // this regression gate. `#[allow(clippy::assertions_on_constants)]`
    // because TAB_MIN_WIDTH is a `const`; the assert lives here so a
    // careless edit of the constant trips a clear, named test failure.
    #[allow(clippy::assertions_on_constants)]
    {
        assert!(
            TAB_MIN_WIDTH >= 200.0,
            "TAB_MIN_WIDTH must be >= 200 px so common shell titles fit; got {}",
            TAB_MIN_WIDTH,
        );
    }
}

#[test]
fn tabs_hold_min_width_when_room_allows() {
    // Common case: 2–4 tabs at a 1000-px window. The equal-share per-tab
    // width is comfortably ≥ TAB_MIN_WIDTH, so the preferred floor is
    // honored and titles like `Administrator: cmd.exe` stay readable.
    //
    // Under the *soft*-floor semantics (PR #184 follow-up), if too many
    // tabs would need to shrink below the floor to fit, the floor yields
    // and the bar shares space evenly — that overflow-avoidance behavior
    // is exercised separately by `tabbar_view::tab_widths_shrink_when_many_tabs`.
    //
    let window_width = 1000.0;
    for n in 2..=4 {
        let bar = bar_with_active(n, 0);
        let layout = TabBarLayout::compute(&bar, window_width);
        for t in &layout.tabs {
            assert!(
                t.bg.w >= TAB_MIN_WIDTH - 0.01,
                "n={n}: tab {} width {} fell below TAB_MIN_WIDTH={} \
                 (window has room — floor must hold)",
                t.index,
                t.bg.w,
                TAB_MIN_WIDTH,
            );
        }
    }
}

#[test]
fn two_tabs_at_1000px_each_get_min_width_or_more() {
    // Issue #171 reproducer: window 1000 px, 2 tabs. Each tab must
    // be at least 200 px wide so titles like `Administrator: cmd.exe`
    // stay readable instead of truncating to `#1 Administ…`.
    let bar = bar_with_active(2, 1);
    let layout = TabBarLayout::compute(&bar, 1000.0);
    for t in &layout.tabs {
        assert!(
            t.bg.w >= 200.0 - 0.01,
            "tab {} width {} is below the 200 px floor",
            t.index,
            t.bg.w,
        );
    }
}
