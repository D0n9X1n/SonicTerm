//! Phase C2 (PR #295 follow-up): regression test for the bug Haiku
//! review caught — `TabBarRegistry` existed as a type but the
//! production render path never published into it, so every OLE
//! `IDropTarget::Drop` resolved to the placeholder `DroppedOnEmpty`
//! instead of the real `(WindowId, slot)`.
//!
//! These tests exercise the snapshot *construction* helper
//! ([`TabBarSnapshot::from_layout`]) that the production publish
//! path uses, plus the round-trip through the shared
//! [`TabBarRegistry`]. Driving the live winit App in a unit test is
//! not feasible (no display in CI / no real PTY children), but the
//! construction helper is the only piece between "renderer ran" and
//! "registry is populated" — once it produces a snapshot with the
//! right screen-coord geometry, the call site is a one-liner.

use sonic_app::app::os_drag::{TabBarRegistry, TabBarSnapshot};
use sonic_ui::tabbar_view::TabBarLayout;
use sonic_ui::tabs::{Tab, TabBar};

fn make_bar(n: usize) -> TabBar {
    let mut bar = TabBar::default();
    for i in 0..n {
        bar.push(Tab::new(format!("tab {i}")));
    }
    bar
}

#[test]
fn from_layout_publishes_snapshot_in_screen_coords() {
    // Logical window 1000 px wide; HiDPI 1.0; main window placed at
    // physical (200, 100).
    let bar = make_bar(3);
    let layout = TabBarLayout::compute_with_height(&bar, 1000.0, 30.0);
    let snap = TabBarSnapshot::from_layout(
        None, // main window convention
        (200, 100),
        (1000, 600),
        1.0,
        &layout,
    );

    // window_rect = origin + size in physical px.
    assert_eq!(snap.window_rect, (200, 100, 1200, 700));
    // bar_rect: layout.bar is at logical (0..1000, 0..30); translate by origin.
    assert_eq!(snap.bar_rect.0, 200);
    assert_eq!(snap.bar_rect.1, 100);
    assert_eq!(snap.bar_rect.3 - snap.bar_rect.1, 30);

    // Tabs landed in screen coords (left edge ≥ window left).
    assert_eq!(snap.tab_lefts.len(), 3);
    assert_eq!(snap.tab_rights.len(), 3);
    for (l, r) in snap.tab_lefts.iter().zip(snap.tab_rights.iter()) {
        assert!(*l >= 200, "tab left {l} must be ≥ window left 200");
        assert!(*r <= 1200, "tab right {r} must be ≤ window right 1200");
        assert!(*r > *l);
    }
}

#[test]
fn from_layout_honors_hidpi_scale_factor() {
    let bar = make_bar(2);
    let layout = TabBarLayout::compute_with_height(&bar, 500.0, 30.0);
    // Retina display: 2.0× scale.
    let snap = TabBarSnapshot::from_layout(None, (0, 0), (1000, 600), 2.0, &layout);
    // bar_rect height in physical px = 30 * 2 = 60.
    assert_eq!(snap.bar_rect.3 - snap.bar_rect.1, 60);
    // Tabs are in physical px now, so widths roughly doubled.
    let total_logical_w = layout.tabs.iter().map(|t| t.bg_rect.w).sum::<f32>();
    let total_physical_w: i32 =
        snap.tab_lefts.iter().zip(snap.tab_rights.iter()).map(|(l, r)| r - l).sum();
    // Allow ±2 px for rounding across two tabs.
    assert!(
        (total_physical_w - (total_logical_w * 2.0).round() as i32).abs() <= 2,
        "expected ~{} physical px, got {}",
        (total_logical_w * 2.0).round() as i32,
        total_physical_w
    );
}

#[test]
fn published_snapshot_round_trips_through_registry() {
    let reg = TabBarRegistry::new();
    let bar = make_bar(3);
    let layout = TabBarLayout::compute_with_height(&bar, 1000.0, 30.0);
    let snap = TabBarSnapshot::from_layout(None, (0, 0), (1000, 600), 1.0, &layout);
    let bar_y_mid = (snap.bar_rect.1 + snap.bar_rect.3) / 2;
    // Hit-test inside tab 0 (between its left and midpoint).
    let t0_l = snap.tab_lefts[0];
    let t0_r = snap.tab_rights[0];
    let t0_mid = (t0_l + t0_r) / 2;
    let probe_x = t0_l + (t0_mid - t0_l) / 2;
    reg.publish(snap);
    let resolved = reg.resolve_screen_pos(probe_x, bar_y_mid);
    assert_eq!(resolved, Some((None, 0)), "drop on tab 0 must resolve to (main, 0)");
}

#[test]
fn outside_bar_in_window_returns_none() {
    let reg = TabBarRegistry::new();
    let bar = make_bar(3);
    let layout = TabBarLayout::compute_with_height(&bar, 1000.0, 30.0);
    let snap = TabBarSnapshot::from_layout(None, (0, 0), (1000, 600), 1.0, &layout);
    // Probe well below the bar (y = 300, far past bar's 30px height).
    let probe = reg.clone_publish_and_resolve(snap, 500, 300);
    assert_eq!(probe, None);
}

// Tiny helper trait to keep the test concise.
trait RegistryTestExt {
    fn clone_publish_and_resolve(
        &self,
        snap: TabBarSnapshot,
        sx: i32,
        sy: i32,
    ) -> Option<(Option<winit::window::WindowId>, usize)>;
}

impl RegistryTestExt for TabBarRegistry {
    fn clone_publish_and_resolve(
        &self,
        snap: TabBarSnapshot,
        sx: i32,
        sy: i32,
    ) -> Option<(Option<winit::window::WindowId>, usize)> {
        self.publish(snap);
        self.resolve_screen_pos(sx, sy)
    }
}
