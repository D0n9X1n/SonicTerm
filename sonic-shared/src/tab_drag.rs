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

use crate::tabbar_view::TabBarLayout;

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
    use crate::tabbar_view::TabBarLayout;
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
}
