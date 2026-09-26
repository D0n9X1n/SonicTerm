//! Local selection gestures bound to the pane that received the press.
//!
//! A local left-button drag records a [`SelectionAnchor`] at press time: the
//! press pane and tab, the tab bar's activation count, the screen, eviction, and
//! size identity, and the absolute cell, all read from one parser snapshot.
//! Motion resolves against that pane's rendered cell grid, so a pointer over
//! another pane, a gap, or outside every pane clamps into the press pane instead
//! of reusing a foreign row or column. A press whose snapshot is contended, or
//! whose cell the held grid no longer has, installs no gesture. Ownership is
//! checked before any layout lookup, so a closed pane or a tab switch cancels
//! the drag even when no frame has drawn since, and any real resize of the press
//! pane's grid cancels it at the next move.

use sonicterm_gpu::core::{build_snapped_cell_x, pixel_to_local_col, PaneLayoutSnapshot};
use sonicterm_grid::grid::Grid;
use sonicterm_ui::{
    selection::{SelectMode, Selection},
    tabs::TabId,
};

use super::{PointerCell, PointerGesture, PointerGestureOwner, WindowState};

/// Press-time identity of a local selection gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectionAnchor {
    /// Pane that received the press.
    pane_id: u64,
    /// Tab that owned the press pane.
    tab_id: TabId,
    /// The tab bar's activation count at the press; any later tab switch changes it.
    tab_activation: u64,
    /// Whether the press landed on the alternate screen.
    on_alt_screen: bool,
    /// The grid's screen epoch at the press.
    screen_epoch: u64,
    /// The grid's size generation at the press; any real resize changes it.
    size_generation: u64,
    /// Rows evicted from history when `cell` was last measured.
    evicted: u64,
    /// Absolute scrollback row and column of the pressed cell.
    cell: (u64, u16),
}

impl SelectionAnchor {
    /// Re-measure the anchored cell against `grid`, or `None` when its content is gone.
    ///
    /// A changed screen, screen epoch, or size generation, a backwards eviction
    /// count, and an evicted anchor row all cancel the gesture rather than guess
    /// a new cell. A resize can leave the pressed address valid while it names
    /// other output, so the size generation decides; addressability is a second guard.
    fn rebase(self, grid: &Grid) -> Option<Self> {
        if grid.is_alt() != self.on_alt_screen || grid.screen_epoch() != self.screen_epoch {
            // When: `is_alt` or `screen_epoch` changed, the anchored screen was replaced.
            return None;
        }
        if grid.size_generation() != self.size_generation {
            // When: `size_generation` moved, a resize remapped the pressed address.
            return None;
        }
        let evicted = grid.scrollback_evicted();
        // The alternate screen keeps no history, so its rows never shift.
        let delta = evicted.checked_sub(self.evicted)? * u64::from(!self.on_alt_screen);
        let row = self.cell.0.checked_sub(delta)?;
        if grid.row_at_abs(row).is_none() || self.cell.1 >= grid.cols {
            // When: `row_at_abs` finds no row or `cols` excludes the column, the cell is gone.
            return None;
        }
        Some(Self { evicted, cell: (row, self.cell.1), ..self })
    }
}

/// Anchor and initial selection for a press, from one parser snapshot.
///
/// `tab` is the owning tab and the tab bar's activation count at the press.
/// Returns `None` when `viewport_cell` is outside the held grid or resolves to
/// no retained row, as a layout drawn before a split resize can report.
fn capture_press(
    grid: &Grid,
    pane_id: u64,
    tab: (TabId, u64),
    view_top: u64,
    viewport_cell: (u16, u16),
    mode: SelectMode,
) -> Option<(SelectionAnchor, Selection)> {
    let (row, col) = viewport_cell;
    let abs_row = view_top + u64::from(row);
    if row >= grid.rows || col >= grid.cols || grid.row_at_abs(abs_row).is_none() {
        // When: `row`, `col`, or `abs_row` names a cell the held grid lacks, bind nothing.
        return None;
    }
    let anchor = SelectionAnchor {
        pane_id,
        tab_id: tab.0,
        tab_activation: tab.1,
        on_alt_screen: grid.is_alt(),
        screen_epoch: grid.screen_epoch(),
        size_generation: grid.size_generation(),
        evicted: grid.scrollback_evicted(),
        cell: (abs_row, col),
    };
    let selection = match mode {
        SelectMode::Word => Selection::word_at(grid, abs_row, col),
        SelectMode::Line => Selection::line_at(grid, abs_row),
        SelectMode::Cell => Selection::new(abs_row, col),
    };
    let selection = selection.with_content_state(
        pane_id,
        grid.content_seq(),
        grid.is_alt(),
        grid.scrollback_evicted(),
    );
    Some((anchor, selection))
}

/// Selection from `anchor` to the absolute `cursor` cell at `mode` granularity.
///
/// Cell drags carry an exact content fingerprint; word and line drags recompute
/// their span from the grid, as the press-time helpers do.
fn drag_selection(
    grid: &Grid,
    anchor: SelectionAnchor,
    cursor: (u64, u16),
    mode: SelectMode,
) -> Selection {
    let bind = |selection: Selection| {
        selection.with_content_state(
            anchor.pane_id,
            grid.content_seq(),
            grid.is_alt(),
            grid.scrollback_evicted(),
        )
    };
    match mode {
        SelectMode::Word => bind(Selection::word_drag(grid, anchor.cell, cursor)),
        SelectMode::Line => bind(Selection::line_drag(grid, anchor.cell.0, cursor.0)),
        SelectMode::Cell => bind(cell_span(anchor.cell, cursor)).with_content_fingerprint(grid),
    }
}

/// Cell-granularity span from `anchor` to `cursor`.
fn cell_span(anchor: (u64, u16), cursor: (u64, u16)) -> Selection {
    let mut selection = Selection::new(anchor.0, anchor.1);
    selection.extend(cursor.0, cursor.1);
    selection
}

/// Map a pointer to a text cell of the press pane, clamped to its addressable cells.
///
/// Columns use the renderer's own snapped edges and lookup, and rows the same
/// division the renderer uses, so a point inside the press pane resolves to the
/// cell the renderer draws there. A point over another pane, a gap, padding, or
/// outside every pane clamps to the pane's nearest text cell, so another pane's
/// row or column is never used.
fn press_pane_cell(layout: PaneLayoutSnapshot, x: f32, y: f32) -> (u16, u16) {
    let edges = build_snapped_cell_x(layout.origin_x_logical, layout.cell_w_logical, layout.cols);
    let left = edges.first().copied().unwrap_or(layout.origin_x_logical);
    let right = edges.last().copied().unwrap_or(left);
    let top = layout.origin_y_logical;
    let bottom = top + f32::from(layout.rows) * layout.cell_h_logical;
    // Half a raster pixel inside the far edges keeps the point in the last text cell.
    let x = x.max(left).min((right - 0.5).max(left));
    let y = y.max(top).min((bottom - 0.5).max(top));
    let last_col = layout.cols.saturating_sub(1);
    let col = pixel_to_local_col(x, &edges, layout.cols).unwrap_or(last_col).min(last_col);
    let row = ((y - top) / layout.cell_h_logical) as u16;
    (row.min(layout.rows.saturating_sub(1)), col)
}

impl WindowState {
    /// Admit one coherent local press before replacing its window's focus, selection, and gesture.
    pub(super) fn begin_local_selection(
        &mut self,
        pane_id: u64,
        viewport_cell: (u16, u16),
        click_count: u8,
    ) -> bool {
        let mode = match click_count {
            2 => SelectMode::Word,
            3 => SelectMode::Line,
            _ => SelectMode::Cell,
        };
        let Some((anchor, selection)) = self.capture_local_press(pane_id, viewport_cell, mode)
        else {
            // When: `capture_local_press` found no coherent in-grid press; bind no gesture.
            self.cancel_local_selection();
            return false;
        };
        let focus_change = self.begin_pointer_pane_focus_change(pane_id);
        self.select_mode = mode;
        self.select_anchor = anchor.cell;
        self.selection = Some(selection);
        let (row, col) = viewport_cell;
        self.pointer_gesture = Some(PointerGesture {
            owner: PointerGestureOwner::Local,
            press_pane: pane_id,
            last_cell: PointerCell { pane_id, row, col },
            anchor: Some(anchor),
        });
        if let Some(change) = focus_change {
            self.finish_pane_focus_change(change);
        }
        true
    }

    /// Anchor and initial selection for a press in `pane_id`, from one snapshot.
    ///
    /// `None` when `pane_id` is not in the active tab, its parser is contended,
    /// or the pressed cell is outside its held grid.
    fn capture_local_press(
        &self,
        pane_id: u64,
        viewport_cell: (u16, u16),
        mode: SelectMode,
    ) -> Option<(SelectionAnchor, Selection)> {
        let tab = (self.tabs.active()?.id, self.tabs.activation());
        let tab_state = self.tab_states.get(self.tabs.active_index())?;
        if !tab_state.tree.leaves().contains(&pane_id)
            || tab_state.tree.zoomed_pane_id().is_some_and(|zoomed| zoomed != pane_id)
        {
            // When: pane_id is absent or zoom hides it, no visible press may change focus or selection.
            return None;
        }
        let pane = self.panes.get(&pane_id)?;
        let parser = pane.parser.try_lock()?;
        let grid = parser.grid();
        let view_top = pane.resolved_view_top(grid);
        capture_press(grid, pane_id, tab, view_top, viewport_cell, mode)
    }

    /// Extend the local selection toward a pointer at physical pixels `x`, `y`.
    ///
    /// Ownership is checked before any layout lookup, so a gesture whose pane
    /// or tab has gone is cancelled even when no frame has drawn since. Motion
    /// then resolves against the press pane's rendered cell grid, clamping a
    /// pointer over another pane, a gap, or outside every pane into it.
    /// Returns whether the selection changed.
    pub(super) fn extend_local_selection(&mut self, x: f32, y: f32) -> bool {
        self.extend_local_selection_with(x, y, |window, pane_id| {
            window.renderer.as_ref()?.pane_layout(pane_id)
        })
    }

    /// Same as `extend_local_selection`, with the press pane's layout from `layout_of`.
    fn extend_local_selection_with(
        &mut self,
        x: f32,
        y: f32,
        layout_of: impl Fn(&Self, u64) -> Option<PaneLayoutSnapshot>,
    ) -> bool {
        let Some(anchor) = self.local_selection_anchor() else {
            // When: no `anchor` is recorded, only a coherent local press may start a drag.
            return false;
        };
        if !self.anchor_topology_holds(anchor) {
            // When: `anchor_topology_holds` fails, the press pane or its tab is gone or inactive.
            self.cancel_local_selection();
            return false;
        }
        let Some(layout) = layout_of(self, anchor.pane_id) else {
            // When: the press pane has no `layout` yet, skip this move until a frame records one.
            return false;
        };
        self.extend_local_selection_to_cell(press_pane_cell(layout, x, y))
    }

    /// Extend the local selection to `viewport_cell` of the press pane.
    ///
    /// The cell is clamped to the held grid, whose size can differ from a layout
    /// drawn before a resize. Cancels the gesture, keeping the current selection,
    /// when the press pane or its tab was removed, another tab was activated
    /// since the press, the screen changed, the grid was resized, or the anchored
    /// cell was evicted. Never retargets the active pane.
    pub(super) fn extend_local_selection_to_cell(&mut self, viewport_cell: (u16, u16)) -> bool {
        let Some(anchor) = self.local_selection_anchor() else {
            // When: no `anchor` is recorded, there is no press pane to extend.
            return false;
        };
        if !self.anchor_topology_holds(anchor) {
            // When: `anchor_topology_holds` fails, the press pane or its tab is gone or inactive.
            self.cancel_local_selection();
            return false;
        }
        let Some(pane) = self.panes.get(&anchor.pane_id) else {
            // When: the press `pane` is missing, the topology check already cancelled.
            return false;
        };
        let Some(parser) = pane.parser.try_lock() else {
            // When: `try_lock` finds the parser busy, skip this move and keep the gesture.
            return false;
        };
        let grid = parser.grid();
        let Some(rebased) = anchor.rebase(grid) else {
            // When: `rebase` finds the anchored content gone, cancel rather than guess a cell.
            drop(parser);
            self.cancel_local_selection();
            return false;
        };
        let row = viewport_cell.0.min(grid.rows.saturating_sub(1));
        let col = viewport_cell.1.min(grid.cols.saturating_sub(1));
        let cursor = (pane.resolved_view_top(grid) + u64::from(row), col);
        let replacement = drag_selection(grid, rebased, cursor, self.select_mode);
        drop(parser);
        let replaces = match (self.select_mode, self.selection.as_ref()) {
            (_, None) => false,
            (SelectMode::Cell, Some(current)) => !current.anchored,
            (SelectMode::Word | SelectMode::Line, Some(_)) => true,
        };
        if !replaces {
            // When: `replaces` is false, the selection was cleared or an anchored one holds.
            return false;
        }
        self.selection = Some(replacement);
        self.select_anchor = rebased.cell;
        if let Some(gesture) = self.pointer_gesture.as_mut() {
            gesture.anchor = Some(rebased);
        }
        true
    }

    /// The recorded anchor of a local gesture, if any.
    fn local_selection_anchor(&self) -> Option<SelectionAnchor> {
        self.pointer_gesture
            .filter(|gesture| gesture.owner == PointerGestureOwner::Local)
            .and_then(|gesture| gesture.anchor)
    }

    /// Whether the press pane is still a leaf of its press tab, with no tab switch since.
    fn anchor_topology_holds(&self, anchor: SelectionAnchor) -> bool {
        let active_tab = self.tabs.active().map(|tab| tab.id);
        let tab = self.tab_states.get(self.tabs.active_index());
        active_tab == Some(anchor.tab_id)
            && self.tabs.activation() == anchor.tab_activation
            && self.panes.contains_key(&anchor.pane_id)
            && tab.is_some_and(|tab| tab.tree.leaves().contains(&anchor.pane_id))
    }

    /// Drop a local gesture so later motion cannot extend; the selection is kept.
    fn cancel_local_selection(&mut self) {
        let local =
            self.pointer_gesture.is_some_and(|gesture| gesture.owner == PointerGestureOwner::Local);
        if local {
            self.pointer_gesture = None;
        }
    }
}

#[cfg(test)]
#[path = "selection_gesture_tests.rs"]
mod selection_gesture_tests;
