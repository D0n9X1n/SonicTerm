//! Cross-window tab drag-to-merge: pure helpers.
//!
//! When the user presses on a tab in window A and drags the cursor
//! away from A's bar, we want to detect "is the cursor currently over
//! window B's tab bar?" — if so, on mouse-up we MERGE the dragged tab
//! into B at the slot under the cursor instead of tearing it out into
//! a brand-new floating window.
//!
//! winit only delivers mouse events to the window that captured them
//! (the source window, since the press happened there). The captured
//! events keep arriving with the source window's local coordinates,
//! which can — and during a drag, typically do — go outside the
//! window's bounds. We turn those into screen-global coordinates using
//! the source window's outer position, then test each other window's
//! bar region in screen-global space.
//!
//! This module is intentionally winit-free: it only operates on
//! integer pixel rects so it can be unit-tested without spawning a
//! real event loop.
//
// FUTURE: cross-PROCESS drag (drag from one sonic process to another)
// will need OS-level drag-and-drop (NSPasteboard / OLE / Wayland data
// device). v1 is same-process only — we only look at our own
// `windows: HashMap<WindowId, ...>`.

use crate::tabbar_view::{TabBarLayout, TAB_BAR_HEIGHT, TEAR_OUT_THRESHOLD_PX};

/// What a tab drag will do on mouse-release, given the current cursor
/// position. Computed each frame from the `DragSession`, but only
/// executed when the button comes up — this is browser-standard
/// behavior: moving the cursor back onto the original bar cancels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DragAction<W> {
    /// Cursor is back over the source window's tab bar — release is a
    /// no-op (or, optionally, a within-bar reorder; we leave that to a
    /// dedicated future path).
    ReturnToOriginalBar,
    /// Cursor is over another Sonic window's tab bar — release merges
    /// the dragged tab into that window at the indicated slot.
    MergeIntoWindow(DropTarget<W>),
    /// Cursor is anywhere else (well below the source bar, or off any
    /// window entirely) — release tears the tab into a new floating
    /// window at the drop position (source-local coordinates).
    TearOutToNewWindow { drop_local: (f32, f32) },
}

/// State carried while the user is holding-and-dragging a tab.
#[derive(Debug, Clone, Copy)]
pub struct DragSession {
    /// Index of the tab in the SOURCE bar at the moment of press.
    pub press_tab_index: usize,
    /// Source-local cursor position at the moment of press.
    pub press_pos: (f32, f32),
    /// Most-recent source-local cursor position.
    pub current_pos: (f32, f32),
}

impl DragSession {
    pub fn new(press_tab_index: usize, press_pos: (f32, f32)) -> Self {
        Self { press_tab_index, press_pos, current_pos: press_pos }
    }
}

/// Pure helper: decide what `mouse-up` should do given the live
/// session, the optional foreign drop target, and the source bar.
///
/// Ordering: foreign target wins; else over-source-bar = cancel; else
/// past tear threshold = tear; else = cancel (hysteresis).
pub fn compute_action<W: Copy>(
    session: &DragSession,
    foreign_target: Option<DropTarget<W>>,
    source_bar: &TabBarLayout,
) -> DragAction<W> {
    if let Some(t) = foreign_target {
        return DragAction::MergeIntoWindow(t);
    }
    let (cx, cy) = session.current_pos;
    if source_bar.point_over_bar(cx, cy) {
        return DragAction::ReturnToOriginalBar;
    }
    if cy >= TAB_BAR_HEIGHT + TEAR_OUT_THRESHOLD_PX {
        return DragAction::TearOutToNewWindow { drop_local: (cx, cy) };
    }
    DragAction::ReturnToOriginalBar
}

/// Geometry of a candidate destination window for drop hit-testing.
#[derive(Debug, Clone, Copy)]
pub struct WindowGeom {
    /// Top-left of the window's CONTENT area in screen-global pixels.
    /// Use `Window::inner_position()` for this — the tab bar is laid
    /// out relative to the inner (client) area, not the outer frame.
    pub inner_origin: (i32, i32),
    /// Inner size of the window in physical pixels (width, height).
    pub inner_size: (u32, u32),
}

/// The drop slot a cross-window drag will land at on mouse-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DropTarget<W> {
    /// Identifier of the destination window (winit `WindowId` in
    /// production; arbitrary key in tests).
    pub window: W,
    /// Insertion index in the destination bar, in `[0, len]`.
    pub slot: usize,
}

/// Convert a cursor position reported by the source window's
/// `CursorMoved` event into screen-global pixel coordinates.
pub fn local_to_global(source_inner_origin: (i32, i32), local: (f64, f64)) -> (i32, i32) {
    (source_inner_origin.0 + local.0.round() as i32, source_inner_origin.1 + local.1.round() as i32)
}

/// Translate a screen-global cursor position into the given
/// destination window's local pixel coordinates, returning `None` if
/// the cursor is not inside the window's inner area at all.
pub fn global_to_local(dest: WindowGeom, global: (i32, i32)) -> Option<(f32, f32)> {
    let (gx, gy) = global;
    let (ox, oy) = dest.inner_origin;
    let (w, h) = dest.inner_size;
    let lx = gx - ox;
    let ly = gy - oy;
    if lx < 0 || ly < 0 || lx as u32 >= w || ly as u32 >= h {
        return None;
    }
    Some((lx as f32, ly as f32))
}

/// Iterate candidate destination windows and return the first one whose
/// tab bar contains the global cursor position. Caller is responsible
/// for excluding the source window from `candidates` (a tab can't be
/// dropped back on its own bar by this path; that's just a reorder).
///
/// `candidates`: iterator of `(window_id, geom, layout)` triples.
pub fn find_drop_target<W: Copy>(
    global_cursor: (i32, i32),
    candidates: impl IntoIterator<Item = (W, WindowGeom, TabBarLayout)>,
) -> Option<DropTarget<W>> {
    for (id, geom, layout) in candidates {
        let Some((lx, ly)) = global_to_local(geom, global_cursor) else { continue };
        if layout.point_over_bar(lx, ly) {
            let slot = layout.drop_slot(lx, ly);
            return Some(DropTarget { window: id, slot });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabbar_view::{TabBarLayout, TAB_BAR_HEIGHT};
    use crate::tabs::{Tab, TabBar};

    fn synth_bar(n: usize) -> TabBar {
        let mut b = TabBar::new();
        for i in 0..n {
            b.push(Tab::new(format!("t{i}")));
        }
        b
    }

    #[test]
    fn local_to_global_offsets_correctly() {
        assert_eq!(local_to_global((100, 50), (10.0, 20.0)), (110, 70));
        // Cursor can go negative (dragged off the left edge).
        assert_eq!(local_to_global((100, 50), (-5.0, 200.0)), (95, 250));
    }

    #[test]
    fn global_to_local_rejects_outside() {
        let g = WindowGeom { inner_origin: (200, 100), inner_size: (800, 600) };
        assert_eq!(global_to_local(g, (199, 200)), None);
        assert_eq!(global_to_local(g, (1000, 200)), None);
        assert_eq!(global_to_local(g, (300, 99)), None);
        assert_eq!(global_to_local(g, (300, 700)), None);
        assert_eq!(global_to_local(g, (200, 100)), Some((0.0, 0.0)));
        assert_eq!(global_to_local(g, (999, 699)), Some((799.0, 599.0)));
    }

    #[test]
    fn drop_target_picks_window_under_cursor() {
        let bar_a = synth_bar(3);
        let layout_a = TabBarLayout::compute(&bar_a, 800.0);
        let geom_a = WindowGeom { inner_origin: (0, 0), inner_size: (800, 600) };

        let bar_b = synth_bar(2);
        let layout_b = TabBarLayout::compute(&bar_b, 800.0);
        let geom_b = WindowGeom { inner_origin: (1000, 0), inner_size: (800, 600) };

        // Cursor at global (1100, 10) → inside window B's bar.
        let candidates = vec![("a", geom_a, layout_a), ("b", geom_b, layout_b)];
        let t = find_drop_target((1100, 10), candidates).expect("hits b");
        assert_eq!(t.window, "b");
    }

    #[test]
    fn drop_target_none_when_no_window_underneath() {
        let bar = synth_bar(2);
        let layout = TabBarLayout::compute(&bar, 800.0);
        let geom = WindowGeom { inner_origin: (0, 0), inner_size: (800, 600) };
        assert!(find_drop_target((2000, 2000), vec![("a", geom, layout)]).is_none());
    }

    #[test]
    fn drop_target_none_when_cursor_below_bar_in_window() {
        let bar = synth_bar(2);
        let layout = TabBarLayout::compute(&bar, 800.0);
        let geom = WindowGeom { inner_origin: (0, 0), inner_size: (800, 600) };
        // Inside the window but well below the 32px bar.
        assert!(find_drop_target((100, 400), vec![("a", geom, layout)]).is_none());
    }

    #[test]
    fn drop_slot_at_end_of_bar() {
        let bar = synth_bar(2);
        let layout = TabBarLayout::compute(&bar, 800.0);
        let geom = WindowGeom { inner_origin: (0, 0), inner_size: (800, 600) };
        // Far-right end of the bar (past last tab midpoint, before +
        // button) → slot 2 (== len).
        let t = find_drop_target((700, 10), vec![("a", geom, layout)]).expect("over bar");
        assert_eq!(t.slot, 2);
    }

    // --- DragSession / compute_action ---

    fn src_layout() -> TabBarLayout {
        TabBarLayout::compute(&synth_bar(3), 800.0)
    }

    #[test]
    fn action_returns_to_original_bar_when_cursor_over_source() {
        let mut s = DragSession::new(1, (100.0, 10.0));
        s.current_pos = (120.0, 5.0);
        let a: DragAction<&str> = compute_action(&s, None, &src_layout());
        assert_eq!(a, DragAction::ReturnToOriginalBar);
    }

    #[test]
    fn action_returns_to_bar_when_just_below_bar_within_hysteresis() {
        let mut s = DragSession::new(1, (100.0, 10.0));
        s.current_pos = (120.0, TAB_BAR_HEIGHT + 5.0);
        let a: DragAction<&str> = compute_action(&s, None, &src_layout());
        assert_eq!(a, DragAction::ReturnToOriginalBar);
    }

    #[test]
    fn action_tears_out_when_well_below_bar() {
        let mut s = DragSession::new(1, (100.0, 10.0));
        s.current_pos = (120.0, TAB_BAR_HEIGHT + TEAR_OUT_THRESHOLD_PX + 1.0);
        let a: DragAction<&str> = compute_action(&s, None, &src_layout());
        assert!(matches!(a, DragAction::TearOutToNewWindow { .. }));
    }

    #[test]
    fn action_merges_when_foreign_target_set_even_if_cursor_far_below() {
        let mut s = DragSession::new(1, (100.0, 10.0));
        s.current_pos = (500.0, 999.0);
        let target = DropTarget { window: "b", slot: 2 };
        let a = compute_action(&s, Some(target), &src_layout());
        assert_eq!(a, DragAction::MergeIntoWindow(target));
    }

    #[test]
    fn action_drag_below_then_back_over_bar_cancels() {
        let mut s = DragSession::new(1, (100.0, 10.0));
        s.current_pos = (120.0, TAB_BAR_HEIGHT + TEAR_OUT_THRESHOLD_PX + 50.0);
        assert!(matches!(
            compute_action::<&str>(&s, None, &src_layout()),
            DragAction::TearOutToNewWindow { .. }
        ));
        s.current_pos = (140.0, 5.0);
        let a: DragAction<&str> = compute_action(&s, None, &src_layout());
        assert_eq!(a, DragAction::ReturnToOriginalBar);
    }
}
