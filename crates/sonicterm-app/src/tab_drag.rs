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

use sonicterm_ui::tabbar_view::{TabBarLayout, TAB_BAR_HEIGHT, TEAR_OUT_THRESHOLD_PX};

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
    /// Cursor is over the SOURCE window's tab bar but at a different
    /// horizontal slot than the press. Release reorders the tab from
    /// `from` to `to` within the source `TabBar`. Indices are in the
    /// pre-reorder coordinate space (i.e. `to` is the destination slot
    /// in the original tab vector); `TabBar::reorder` handles the
    /// remove-then-insert shift.
    ReorderTab { from: usize, to: usize },
    /// Cursor is over another SonicTerm window's tab bar — release merges
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

/// Minimum Euclidean distance, in logical pixels, the cursor must
/// travel from the press point before a press-hold is treated as a
/// drag. Below this floor the chip is suppressed — otherwise every
/// click would flash a one-frame ghost. Matches Cocoa / GTK defaults.
pub const DRAG_START_THRESHOLD_PX: f32 = 5.0;

/// True when the live drag session has moved at least
/// [`DRAG_START_THRESHOLD_PX`] from its press point. Pure — the app
/// uses this each cursor-move to decide whether to publish a
/// `DragChipOverlay` to the renderer.
pub fn drag_moved_enough(session: &DragSession) -> bool {
    let dx = session.current_pos.0 - session.press_pos.0;
    let dy = session.current_pos.1 - session.press_pos.1;
    (dx * dx + dy * dy).sqrt() >= DRAG_START_THRESHOLD_PX
}

/// Pure builder for the renderer-facing drag-chip overlay.
///
/// Returns `None` until the cursor has moved past
/// [`DRAG_START_THRESHOLD_PX`] from the press position — the spec
/// requires no chip flash on small accidental wiggles.
///
/// When the cursor is still over the bar's Y range, the returned
/// overlay carries a `drop_line_x` matching the insertion slot under
/// the cursor and `scale = 1.0`. Once the cursor leaves the bar
/// vertically (tear-out armed), the drop line is cleared and `scale`
/// eases out to `1.02` to telegraph the tear gesture.
pub fn build_drag_chip_overlay(
    session: &DragSession,
    source_bar: &TabBarLayout,
    title: String,
) -> Option<sonicterm_ui::drag_chip::DragChipOverlay> {
    if !drag_moved_enough(session) {
        return None;
    }
    let (cx, cy) = session.current_pos;
    let over_bar = source_bar.point_over_bar(cx, cy);
    let drop_line_x = if over_bar {
        let slot = source_bar.drop_slot(cx, cy);
        source_bar.insertion_x(slot)
    } else {
        None
    };
    // Subtle scale ease — the renderer interpolates from the previous
    // frame, so we just publish the target value here. 1.0 in-bar,
    // 1.02 once the cursor leaves the bar.
    let scale = if over_bar { 1.0 } else { 1.02 };
    let chip_x = cx - 30.0;
    let chip_y = cy - 12.0;
    Some(sonicterm_ui::drag_chip::DragChipOverlay {
        top_left: (chip_x, chip_y),
        title,
        drop_line_x,
        drop_line_y: source_bar.bar_y_range(),
        scale,
        // Phase D (Epic #289) — drag visual feedback:
        //   D1: ghost_alpha 0.5 on the chip body
        //   D2: insertion_slot opens an 8 px gap in the destination
        //       bar at the drop slot when the cursor is over a bar
        //   D3: source_tab_idx flags the source tab for alpha-0.3
        //       painting so the dragged tab visibly "lifts off"
        source_tab_idx: Some(session.press_tab_index),
        source_alpha: 0.3,
        insertion_slot: if over_bar { Some(source_bar.drop_slot(cx, cy)) } else { None },
        ghost_alpha: 0.5,
    })
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
        // Within-bar reorder: compute the destination slot from the
        // cursor X and compare to the press tab index. `drop_slot`
        // returns a value in `[0, n]` (insertion-slot semantics). We
        // convert that to a tab-vec index in `[0, n-1]` and gate the
        // ReorderTab variant on it actually differing from the source
        // — otherwise this is the regular "drop on yourself" no-op
        // that browsers also treat as a cancel.
        //
        // A press-then-release with sub-threshold cursor movement is
        // a CLICK, not a drag — it must never reorder. Pre-fix, the
        // right half of any tab (which includes the title-to-`×` gap
        // on tab 0) had `drop_slot` returning the next tab's index,
        // so a stationary click silently swapped the two tabs and the
        // user perceived "nothing happened" because the active tab
        // simply migrated to the same on-screen position the other
        // tab vacated. See tests/click_without_drag_does_not_reorder.rs.
        let n = source_bar.tabwidgets().len();
        if n > 0 && drag_moved_enough(session) {
            let raw_slot = source_bar.drop_slot(cx, cy);
            // Clamp insertion-slot semantics: `raw_slot == n` means
            // "after the last tab", which is the last index.
            let to = raw_slot.min(n - 1);
            if to != session.press_tab_index {
                return DragAction::ReorderTab { from: session.press_tab_index, to };
            }
        }
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
    /// Top-left of the window's CONTENT area in screen-global physical
    /// pixels. Use `Window::inner_position()` for this — the tab bar
    /// is laid out relative to the inner (client) area, not the outer
    /// frame. Stays physical because cross-window math must operate
    /// in the same coordinate system the OS reports cursor positions
    /// in (i.e. physical px on the global desktop).
    pub inner_origin: (i32, i32),
    /// Inner size of the window in PHYSICAL pixels (width, height).
    pub inner_size: (u32, u32),
    /// HiDPI scale factor of this window. `global_to_local` divides
    /// the resulting local offset by this so callers receive LOGICAL
    /// pixel coordinates — which is the unit `TabBarLayout` is
    /// computed in. Default of `1.0` keeps pre-HiDPI tests valid.
    pub scale_factor: f32,
}

impl WindowGeom {
    /// Convenience constructor for the common pre-HiDPI/test case
    /// where scale factor is 1.0.
    pub fn new(inner_origin: (i32, i32), inner_size: (u32, u32)) -> Self {
        Self { inner_origin, inner_size, scale_factor: 1.0 }
    }
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
/// destination window's local LOGICAL pixel coordinates, returning
/// `None` if the cursor is not inside the window's inner area at all.
///
/// The destination geometry stores its origin/size in physical px
/// (winit's natural unit) and its HiDPI scale factor. The returned
/// local position is divided by `scale_factor` so it lines up with
/// `TabBarLayout`, which is computed in logical px.
pub fn global_to_local(dest: WindowGeom, global: (i32, i32)) -> Option<(f32, f32)> {
    let (gx, gy) = global;
    let (ox, oy) = dest.inner_origin;
    let (w, h) = dest.inner_size;
    let lx = gx - ox;
    let ly = gy - oy;
    if lx < 0 || ly < 0 || lx as u32 >= w || ly as u32 >= h {
        return None;
    }
    let sf = dest.scale_factor.max(f32::EPSILON);
    Some((lx as f32 / sf, ly as f32 / sf))
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

/// Variant of [`find_drop_target`] for app-level candidate lists where a
/// window entry may not own a renderer yet. Phase B2 PR-A inserts a shadow
/// main-window entry into `App::windows` with `renderer: None`; drag-target
/// hit-testing must skip that placeholder rather than unwrapping it.
#[doc(hidden)]
pub fn find_drop_target_skipping_unrendered<W: Copy>(
    global_cursor: (i32, i32),
    candidates: impl IntoIterator<Item = (W, WindowGeom, Option<TabBarLayout>)>,
) -> Option<DropTarget<W>> {
    find_drop_target(
        global_cursor,
        candidates
            .into_iter()
            .filter_map(|(id, geom, layout)| layout.map(|layout| (id, geom, layout))),
    )
}

// Unit tests live in `tests/src_tab_drag.rs`.
