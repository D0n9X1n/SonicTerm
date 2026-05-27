//! Regression tests for issue #171: the amber/blue active-tab top
//! accent bar was painting in the empty gutter to the right of the
//! last tab (between the last tab and the `+` button) instead of
//! underlining the active tab itself.
//!
//! Root cause defense-in-depth: the renderer now reads the accent
//! rect from [`TabBarLayout::active_accent_rect`] rather than
//! computing it inline. These tests pin the helper's behaviour so a
//! future refactor cannot silently re-introduce the off-by-one.

use sonic_ui::tabbar_view::{
    TabBarLayout, ACTIVE_TOP_ACCENT_H, ACTIVE_TOP_ACCENT_INSET, TAB_MIN_WIDTH,
};
use sonic_ui::tabs::{Tab, TabBar};

fn bar_with_active(n: usize, active: usize) -> TabBar {
    let mut b = TabBar::new();
    for i in 0..n {
        b.push(Tab::new(format!("#{} Administrator: cmd.exe", i + 1)));
    }
    b.activate(active);
    b
}

/// Helper: anchor invariant — the accent's x MUST equal
/// `tabs[active].bg.x + ACTIVE_TOP_ACCENT_INSET`, NOT
/// `tabs[active + 1].bg.x + INSET` (the off-by-one in #171) and NOT
/// `new_tab.x + INSET` (the worst-case drift into the gutter).
fn assert_accent_on_active(layout: &TabBarLayout, active_idx: usize) {
    let acc = layout.active_accent_rect().expect("active rect must exist");
    let t = &layout.tabs[active_idx];

    assert!(
        (acc.x - (t.bg.x + ACTIVE_TOP_ACCENT_INSET)).abs() < 0.01,
        "accent.x ({}) must equal tabs[{}].bg.x + INSET ({}), got delta {}",
        acc.x,
        active_idx,
        t.bg.x + ACTIVE_TOP_ACCENT_INSET,
        acc.x - (t.bg.x + ACTIVE_TOP_ACCENT_INSET),
    );

    // And explicitly: NOT the off-by-one position the bug report
    // showed. When active_idx is the last tab we check against the
    // `+` button rect; for inner tabs we check against the next tab.
    let bug_x = if active_idx + 1 < layout.tabs.len() {
        layout.tabs[active_idx + 1].bg.x + ACTIVE_TOP_ACCENT_INSET
    } else {
        layout.new_tab.x + ACTIVE_TOP_ACCENT_INSET
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

    // Width pinned to `bg.w - 2 * inset`.
    let want_w = (t.bg.w - 2.0 * ACTIVE_TOP_ACCENT_INSET).max(0.0);
    assert!((acc.w - want_w).abs() < 0.01, "accent.w must be bg.w - 2*inset");

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
    // drift onto a neighbouring tab or into the `+` button gutter.
    //
    // Note: under the TAB_MIN_WIDTH floor the bar may overflow the
    // gutter on the right when many tabs don't fit; that is OK and
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
fn tabs_never_shrink_below_min_width_even_when_many() {
    // Many tabs at a moderate window width: under the old "advisory"
    // semantics each tab shrank below 100 px and titles ellipsized
    // to `#1 Administ…`. The bar must now hold the floor.
    for n in 2..=12 {
        let bar = bar_with_active(n, 0);
        let layout = TabBarLayout::compute(&bar, 1000.0);
        for t in &layout.tabs {
            assert!(
                t.bg.w >= TAB_MIN_WIDTH - 0.01,
                "n={n}: tab {} width {} fell below TAB_MIN_WIDTH={}",
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
