//! Pane-divider hit-testing, hover cursors and drag resizing, kept side by
//! side for the main window and for child windows.

use winit::window::{CursorIcon, WindowId};

use super::child_window::resize_visible_panes_in_child;
use super::{mark_all_panes_dirty, App};

const SPLITTER_HIT_THICKNESS: f32 = 8.0;

impl App {
    fn main_pane_outer_rect(&self) -> Option<sonicterm_ui::pane::Rect> {
        let r = self.main_renderer()?;
        let (w, h) = r.logical_size();
        let top = (r.top_inset() - r.padding_top_px()).max(0.0);
        let bottom = r.bottom_inset();
        Some(sonicterm_ui::pane::Rect::new(0.0, top, w.max(0.0), (h - top - bottom).max(0.0)))
    }

    pub(super) fn splitter_hit_at(
        &self,
        x: f32,
        y: f32,
    ) -> Option<sonicterm_ui::pane::SplitterHit> {
        let outer = self.main_pane_outer_rect()?;
        let tab_idx = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
        self.main_tab_states()
            .and_then(|states| states.get(tab_idx))
            .and_then(|state| state.tree.hit_splitter(outer, SPLITTER_HIT_THICKNESS, x, y))
    }

    pub(super) fn set_splitter_cursor(&self, axis: sonicterm_ui::pane::SplitAxis) {
        if let Some(w) = self.main_window() {
            let icon = match axis {
                sonicterm_ui::pane::SplitAxis::Vertical => CursorIcon::ColResize,
                sonicterm_ui::pane::SplitAxis::Horizontal => CursorIcon::RowResize,
            };
            w.set_cursor(icon);
        }
    }

    pub(super) fn refresh_splitter_hover(&mut self, x: f32, y: f32) -> bool {
        if self.main().and_then(|ws| ws.splitter_drag.as_ref()).is_some() {
            // When: splitter_drag is Some, preserve its resize cursor and consume hover routing.
            return true;
        }
        let Some(hit) = self.splitter_hit_at(x, y) else {
            // When: splitter_hit_at returns None, clear any stale splitter hover cursor.
            let was_splitter =
                self.main_mut().map(|ws| ws.splitter_hover.take().is_some()).unwrap_or(false);
            if was_splitter {
                if let Some(w) = self.main_window() {
                    w.set_cursor(CursorIcon::Default);
                }
            }
            return false;
        };
        if let Some(ws) = self.main_mut() {
            ws.hovered_url = None;
            ws.hover_link = false;
            ws.splitter_hover = Some(hit.axis);
        }
        self.set_splitter_cursor(hit.axis);
        true
    }

    pub(super) fn apply_splitter_drag(&mut self, x: f32, y: f32) -> bool {
        let Some(drag) = self.main().and_then(|ws| ws.splitter_drag.clone()) else {
            // When: splitter_drag is None, this motion is not a splitter gesture.
            return false;
        };
        let Some(outer) = self.main_pane_outer_rect() else {
            // When: main_pane_outer_rect is None, splitter geometry cannot be updated.
            return false;
        };
        let dx = x - drag.last_pos.0;
        let dy = y - drag.last_pos.1;
        if dx == 0.0 && dy == 0.0 {
            // When: dx and dy are both zero, consume the gesture without resizing.
            return true;
        }

        let tab_idx = self.main_tabs().map(|t| t.active_index()).unwrap_or(0);
        let changed = self
            .main_tab_states_mut()
            .and_then(|states| states.get_mut(tab_idx))
            .map(|state| state.tree.resize_splitter_by_delta(&drag.splitter, outer, dx, dy))
            .unwrap_or(false);

        if changed {
            if let Some(((cell_w, cell_h), inset)) = self.main_renderer().map(|r| {
                (
                    r.cell_size(),
                    [
                        r.padding_left_px(),
                        r.padding_right_px(),
                        r.padding_top_px(),
                        r.padding_bottom_px(),
                    ],
                )
            }) {
                let rects = self
                    .main_tab_states()
                    .and_then(|states| states.get(tab_idx))
                    .map(|state| state.tree.layout(outer))
                    .unwrap_or_default();
                if let Some(panes) = self.main_panes() {
                    crate::app::resize_panes_to_rects(panes, &rects, cell_w, cell_h, inset);
                }
            }
        }

        if let Some(ws) = self.main_mut() {
            if let Some(active) = ws.splitter_drag.as_mut() {
                active.last_pos = (x, y);
            }
            if changed {
                mark_all_panes_dirty(&ws.panes);
            }
        }
        self.set_splitter_cursor(drag.axis);
        if changed {
            if let Some(w) = self.main_window() {
                w.request_redraw();
            }
        }
        true
    }
}

impl App {
    // ── Child-window splitter (pane-divider) mouse drag ──
    // Mirrors the main-window splitter input. The pure tree ops
    // (`hit_splitter`, `resize_splitter_by_delta`, `layout`) are
    // window-agnostic; only the state lookups differ.

    /// Outer pane-layout rect for a child window (same basis the renderer
    /// + `compute_pane_rects_for` use).
    fn child_pane_outer_rect(&self, win_id: WindowId) -> Option<sonicterm_ui::pane::Rect> {
        let child = self.windows.get(&win_id)?;
        if let Some((outer, _, _)) = child.test_pane_viewport {
            // When: `test_pane_viewport` supplies `outer` directly, so headless
            // tests get pane geometry without a renderer.
            return Some(outer);
        }
        let r = child.renderer.as_ref()?;
        let (w, h) = r.logical_size();
        let top = (r.top_inset() - r.padding_top_px()).max(0.0);
        let bottom = r.bottom_inset();
        Some(sonicterm_ui::pane::Rect::new(0.0, top, w.max(0.0), (h - top - bottom).max(0.0)))
    }

    /// Hit-test a splitter divider in the child window `win_id`.
    pub(super) fn splitter_hit_at_in_child(
        &self,
        win_id: WindowId,
        x: f32,
        y: f32,
    ) -> Option<sonicterm_ui::pane::SplitterHit> {
        let outer = self.child_pane_outer_rect(win_id)?;
        let child = self.windows.get(&win_id)?;
        let tab_idx = child.tabs.active_index();
        child
            .tab_states
            .get(tab_idx)
            .and_then(|state| state.tree.hit_splitter(outer, CHILD_SPLITTER_HIT_THICKNESS, x, y))
    }

    pub(super) fn set_child_splitter_cursor(
        &self,
        win_id: WindowId,
        axis: sonicterm_ui::pane::SplitAxis,
    ) {
        if let Some(child) = self.windows.get(&win_id) {
            if let Some(w) = child.window.as_ref() {
                let icon = match axis {
                    sonicterm_ui::pane::SplitAxis::Vertical => CursorIcon::ColResize,
                    sonicterm_ui::pane::SplitAxis::Horizontal => CursorIcon::RowResize,
                };
                w.set_cursor(icon);
            }
        }
    }

    pub(super) fn refresh_child_splitter_hover(
        &mut self,
        win_id: WindowId,
        x: f32,
        y: f32,
    ) -> bool {
        let hit = self.splitter_hit_at_in_child(win_id, x, y);
        let axis = hit.map(|hit| hit.axis);
        let changed =
            self.windows.get(&win_id).map(|child| child.splitter_hover != axis).unwrap_or(false);
        if let Some(child) = self.windows.get_mut(&win_id) {
            child.splitter_hover = axis;
        }
        if let Some(axis) = axis {
            self.set_child_splitter_cursor(win_id, axis);
        } else if changed {
            // When: `changed` and no axis — the pointer just left a divider, so
            // the resize cursor must be handed back to the default.
            if let Some(child) = self.windows.get(&win_id) {
                if let Some(w) = child.window.as_ref() {
                    w.set_cursor(CursorIcon::Default);
                }
            }
        }
        changed || axis.is_some()
    }

    /// Test-only: prove the child splitter hit-test is reachable with headless
    /// viewport geometry.
    #[doc(hidden)]
    pub fn __test_child_splitter_hit_axis(
        &self,
        win_id: WindowId,
        x: f32,
        y: f32,
    ) -> Option<sonicterm_ui::pane::SplitAxis> {
        self.splitter_hit_at_in_child(win_id, x, y).map(|hit| hit.axis)
    }

    /// Test-only: drive the child splitter hover refresh directly and report
    /// whether hover state or cursor shape changed, so the no-button hover path
    /// is reachable with headless viewport geometry.
    #[doc(hidden)]
    pub fn __test_refresh_child_splitter_hover(
        &mut self,
        win_id: WindowId,
        x: f32,
        y: f32,
    ) -> bool {
        self.refresh_child_splitter_hover(win_id, x, y)
    }

    /// Test-only: read child splitter-hover state.
    #[doc(hidden)]
    pub fn __test_child_splitter_hover(
        &self,
        win_id: WindowId,
    ) -> Option<sonicterm_ui::pane::SplitAxis> {
        self.windows.get(&win_id).and_then(|child| child.splitter_hover)
    }

    /// Apply an in-flight splitter drag in the child window `win_id`.
    pub(super) fn apply_splitter_drag_in_child(
        &mut self,
        win_id: WindowId,
        x: f32,
        y: f32,
    ) -> bool {
        let Some(drag) = self.windows.get(&win_id).and_then(|c| c.splitter_drag.clone()) else {
            // When: no `splitter_drag` is recorded, so this cursor move is not
            // part of a divider drag and belongs to the ordinary hover path.
            return false;
        };
        let Some(outer) = self.child_pane_outer_rect(win_id) else {
            // When: `child_pane_outer_rect` is unavailable, so there is no
            // layout basis to convert the pointer delta into a split ratio.
            return false;
        };
        let dx = x - drag.last_pos.0;
        let dy = y - drag.last_pos.1;
        if dx == 0.0 && dy == 0.0 {
            // When: `dx` and `dy` are both zero, so the pointer has not moved
            // and the drag stays live without re-laying out the tree.
            return true;
        }
        let tab_idx = self.windows.get(&win_id).map(|c| c.tabs.active_index()).unwrap_or(0);
        let changed = self
            .windows
            .get_mut(&win_id)
            .and_then(|c| c.tab_states.get_mut(tab_idx))
            .map(|state| state.tree.resize_splitter_by_delta(&drag.splitter, outer, dx, dy))
            .unwrap_or(false);
        if changed {
            if let Some(child) = self.windows.get_mut(&win_id) {
                resize_visible_panes_in_child(child);
            }
        }
        if let Some(child) = self.windows.get_mut(&win_id) {
            if let Some(active) = child.splitter_drag.as_mut() {
                active.last_pos = (x, y);
            }
            if changed {
                mark_all_panes_dirty(&child.panes);
                child.request_redraw();
            }
        }
        self.set_child_splitter_cursor(win_id, drag.axis);
        true
    }
}

/// Splitter hit thickness in logical px (mirror of `SPLITTER_HIT_THICKNESS`).
const CHILD_SPLITTER_HIT_THICKNESS: f32 = 8.0;
