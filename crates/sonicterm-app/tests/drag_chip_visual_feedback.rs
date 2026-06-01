//! Unit tests for the v0.6 "visual feedback while dragging a tab" feature.
//!
//! Covers two pure helpers introduced alongside the chip extension:
//!
//!   * `build_drag_chip_overlay` — only emits a chip once the cursor
//!     has moved at least `DRAG_START_THRESHOLD_PX` from the press
//!     point (5 px). Below that, the chip stays suppressed so a bare
//!     click never flashes a one-frame ghost.
//!
//!   * `TabBarLayout::insertion_x` — returns the X coordinate of the
//!     drop-line accent for a given insertion slot, so the rendered
//!     line lands exactly in the gap between two tabs (or just past
//!     the last tab for slot `n`).
//!
//! These are the two pieces of logic the new `DragChipOverlay` fields
//! (`drop_line_x`, `scale`) hinge on. The visual rendering is
//! exercised by the existing GUI smoke flow described in CLAUDE.md.

use sonicterm_app::tab_drag::{
    build_drag_chip_overlay, drag_moved_enough, DragSession, DRAG_START_THRESHOLD_PX,
};
use sonicterm_shared::tabbar_view::TabBarLayout;
use sonicterm_shared::tabs::{Tab, TabBar};

fn bar_with(n: usize) -> TabBar {
    let mut b = TabBar::new();
    for i in 0..n {
        b.push(Tab::new(format!("t{i}")));
    }
    b
}

fn layout(n: usize) -> TabBarLayout {
    TabBarLayout::compute(&bar_with(n), 800.0)
}

#[test]
fn drag_chip_appears_after_5px_movement_during_press() {
    let press = (200.0_f32, 10.0_f32);
    let mut s = DragSession::new(0, press);

    // Sub-threshold: a 3px wiggle must NOT publish a chip.
    s.current_pos = (press.0 + 3.0, press.1);
    assert!(
        !drag_moved_enough(&s),
        "3px movement below {DRAG_START_THRESHOLD_PX}px threshold should not arm chip"
    );
    let l = layout(3);
    assert!(
        build_drag_chip_overlay(&s, &l, "t0".into()).is_none(),
        "build_drag_chip_overlay must return None below threshold"
    );

    // Exactly the threshold (diagonal, well above 5px) arms the chip.
    s.current_pos = (press.0 + 5.0, press.1);
    assert!(drag_moved_enough(&s), "5px movement should arm chip");
    let chip = build_drag_chip_overlay(&s, &l, "t0".into())
        .expect("chip overlay must be Some at threshold");
    // Cursor still over the bar → drop-line populated, scale at rest.
    assert!(chip.drop_line_x.is_some(), "drop line should show while over bar");
    assert!((chip.scale - 1.0).abs() < 1e-3, "in-bar scale is 1.0");
}

#[test]
fn drop_line_position_matches_insertion_index() {
    // 4 tabs, fixed-width bar. Insertion slot `i` should fall in the
    // gap BETWEEN tab i-1 and tab i (or at the bar's edges for 0/n).
    let l = layout(4);
    let tabs = l.tabs.clone();
    assert_eq!(tabs.len(), 4);

    // Slot 0 — left of the first tab.
    let x0 = l.insertion_x(0).expect("slot 0 has an X");
    assert!(
        x0 < tabs[0].bg.x + 0.001,
        "slot 0 X ({x0}) must sit at or before first tab's left edge ({})",
        tabs[0].bg.x
    );

    // Slot i in the middle — between adjacent tabs' edges.
    for i in 1..tabs.len() {
        let prev_right = tabs[i - 1].bg.x + tabs[i - 1].bg.w;
        let next_left = tabs[i].bg.x;
        let xi = l.insertion_x(i).expect("middle slot has an X");
        assert!(
            xi >= prev_right - 0.001 && xi <= next_left + 0.001,
            "slot {i} X ({xi}) must sit in the gap [{prev_right}, {next_left}]"
        );
    }

    // Slot n — past the last tab.
    let last_right = tabs[3].bg.x + tabs[3].bg.w;
    let xn = l.insertion_x(tabs.len()).expect("end slot has an X");
    assert!(
        xn >= last_right - 0.001,
        "slot n X ({xn}) must sit at or after last tab's right edge ({last_right})"
    );

    // Empty / hidden layouts return None — no meaningful gap to draw.
    let empty = layout(0);
    assert_eq!(empty.insertion_x(0), None);
    assert_eq!(empty.insertion_x(1), None);

    // Out-of-range slot is clamped to len — same X as the end slot.
    assert_eq!(l.insertion_x(99), Some(xn));
}

#[test]
fn drop_line_cleared_when_cursor_leaves_bar_y_range() {
    let l = layout(3);
    let mut s = DragSession::new(1, (250.0, 10.0));

    // Far below the bar (tear-out armed).
    s.current_pos = (250.0, 200.0);
    let chip =
        build_drag_chip_overlay(&s, &l, "t1".into()).expect("chip must be Some after 190px move");
    assert!(chip.drop_line_x.is_none(), "drop line must clear once cursor leaves the bar Y range");
    assert!(
        chip.scale > 1.0,
        "scale should ease up to telegraph tear-out arm (got {})",
        chip.scale
    );
}
