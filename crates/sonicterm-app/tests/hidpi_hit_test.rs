//! Regression test for haiku review on PR #76: hit-testing of the
//! tab bar and cross-window drag-merge must operate in LOGICAL pixel
//! coordinates so 2× Retina displays don't silently miss clicks.
//!
//! Before the fix:
//!   * cursor positions from winit's `PhysicalPosition` were passed
//!     directly into `TabBarLayout::hit`, whose `bar_rect.h` is the
//!     logical `TAB_BAR_HEIGHT = 32`. At scale_factor = 2.0 a logical
//!     y in (16, 32] mapped to physical y in (32, 64] → outside the
//!     bar, so the bottom half of every tab was a dead zone.
//!   * `WindowGeom::global_to_local` returned physical local coords,
//!     producing the same dead zone on every child window's tab bar
//!     during a cross-window drag-merge.
//!
//! After the fix the App layer divides cursor positions by the
//! window's scale factor before calling into `TabBarLayout`, and
//! `WindowGeom` carries the destination window's scale factor so
//! `global_to_local` returns logical coords end-to-end.

use sonicterm_app::tab_drag::{find_drop_target, global_to_local, WindowGeom};
use sonicterm_shared::tabbar_view::{TabBarLayout, TabHit, TAB_BAR_HEIGHT};
use sonicterm_shared::tabs::{Tab, TabBar};

fn bar_with(n: usize) -> TabBar {
    let mut b = TabBar::new();
    for i in 0..n {
        b.push(Tab::new(format!("t{i}")));
    }
    b
}

/// Same logical click hits the same tab regardless of HiDPI scale
/// factor — provided the caller normalizes the cursor through the
/// scale factor before calling `TabBarLayout::hit` (which the App
/// now does in CursorMoved / MouseInput).
#[test]
fn tab_click_hits_at_2x_after_logical_normalization() {
    let bar = bar_with(3);
    // Logical window width is identical across scale factors; the
    // physical surface differs, but the layout (and click target)
    // is laid out in logical units.
    let logical_w = 800.0;
    let layout = TabBarLayout::compute(&bar, logical_w);

    // A click landing at logical (50, 20) — inside the first tab's
    // bg rect and below the bar's vertical midpoint, but still
    // inside the logical `TAB_BAR_HEIGHT = 32`.
    let (logical_x, logical_y) = (50.0_f32, 20.0_f32);
    assert!(logical_y < TAB_BAR_HEIGHT, "test premise: y inside bar height");

    // Simulate a 1× display: physical == logical.
    let hit_1x = layout.hit(logical_x, logical_y);
    assert!(matches!(hit_1x, Some(TabHit::Activate(0))));

    // Simulate a 2× Retina display. The OS reports physical px
    // (100, 20), the App divides by scale_factor before calling
    // `hit`, recovering the original logical (50, 10).
    let sf = 2.0_f32;
    let physical = (logical_x * sf, logical_y * sf);
    let normalized = (physical.0 / sf, physical.1 / sf);
    let hit_2x = layout.hit(normalized.0, normalized.1);
    assert_eq!(hit_2x, hit_1x, "logical click hits same tab at any DPI");

    // And the buggy path (passing PHYSICAL directly) must MISS:
    // y = 20 (logical) → 40 (physical) is below the bar's logical
    // height of 32, so without normalization the hit-test returns
    // None / falls through. We assert this so re-introducing the
    // bug trips the test loudly.
    let buggy_hit = layout.hit(physical.0, physical.1);
    assert_ne!(buggy_hit, hit_1x, "without normalization the click misses at 2×");
}

/// `global_to_local` must divide by the destination window's scale
/// factor so the returned local coordinates line up with the
/// LOGICAL units `TabBarLayout` is computed in. Cross-window
/// drag-merge would otherwise miss tab bars on any HiDPI child
/// window.
#[test]
fn global_to_local_returns_logical_at_2x() {
    // A child window placed at physical (1000, 0) on a 2× display
    // of inner size 1600×1200 PHYSICAL px == 800×600 LOGICAL.
    let geom = WindowGeom { inner_origin: (1000, 0), inner_size: (1600, 1200), scale_factor: 2.0 };
    // Cursor at global physical (1100, 20) → 100 px right of the
    // window's inner origin, 20 px down → should map to logical
    // (50, 10) which is inside the 32-logical-px tab bar.
    let (lx, ly) = global_to_local(geom, (1100, 20)).expect("inside window");
    assert!((lx - 50.0).abs() < f32::EPSILON, "lx={lx}");
    assert!((ly - 10.0).abs() < f32::EPSILON, "ly={ly}");
    assert!(ly < TAB_BAR_HEIGHT, "logical y inside tab bar");
}

/// End-to-end: a click on the tab bar of a HiDPI child window during
/// cross-window drag is correctly resolved by `find_drop_target`
/// to that child's bar.
#[test]
fn find_drop_target_hits_2x_child_tab_bar() {
    let bar = bar_with(2);
    // Layout is logical (800 wide, 32 tall).
    let layout = TabBarLayout::compute(&bar, 800.0);
    let geom = WindowGeom {
        inner_origin: (1000, 0),
        inner_size: (1600, 1200), // 800×600 logical at 2×
        scale_factor: 2.0,
    };
    // Global physical (1100, 20) is logical (50, 10) inside the
    // child — squarely on tab 0.
    let target = find_drop_target((1100, 20), vec![("child", geom, layout)])
        .expect("drop target on child bar");
    assert_eq!(target.window, "child");
    assert_eq!(target.slot, 0);
}

/// Backwards-compat: at scale_factor 1.0 the new logic matches the
/// old physical-only behavior exactly (so the existing tab_drag
/// tests using `WindowGeom::new` keep passing).
#[test]
fn global_to_local_unchanged_at_1x() {
    let geom = WindowGeom::new((200, 100), (800, 600));
    assert_eq!(global_to_local(geom, (300, 110)), Some((100.0, 10.0)));
    assert_eq!(global_to_local(geom, (199, 200)), None);
}
