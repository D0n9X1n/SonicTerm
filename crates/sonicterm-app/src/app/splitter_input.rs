//! Pane-divider hit-testing, hover cursors and drag resizing, kept side by
//! side for the main window and for child windows.

use winit::window::{CursorIcon, WindowId};

use super::child_window::resize_visible_panes_in_child;
use super::{mark_all_panes_dirty, App};

const SPLITTER_HIT_THICKNESS: f32 = 8.0;

impl App {
    fn main_pane_outer_rect(&self) -> Option<sonicterm_ui::pane::Rect> {
        let renderer = self.main_renderer()?;
        let (width, height) = renderer.logical_size();
        let top = (renderer.top_inset() - renderer.padding_top_px()).max(0.0);
        let bottom = renderer.bottom_inset();
        Some(sonicterm_ui::pane::Rect::new(
            0.0,
            top,
            width.max(0.0),
            (height - top - bottom).max(0.0),
        ))
    }

    pub(super) fn splitter_hit_at(
        &self,
        pixel_x: f32,
        pixel_y: f32,
    ) -> Option<sonicterm_ui::pane::SplitterHit> {
        let outer = self.main_pane_outer_rect()?;
        let tab_idx = self.main_tabs().map(|tab_bar| tab_bar.active_index()).unwrap_or(0);
        self.main_tab_states().and_then(|states| states.get(tab_idx)).and_then(|state| {
            state.tree.hit_splitter(outer, SPLITTER_HIT_THICKNESS, pixel_x, pixel_y)
        })
    }

    pub(super) fn set_splitter_cursor(&self, axis: sonicterm_ui::pane::SplitAxis) {
        if let Some(main_window) = self.main_window() {
            let icon = match axis {
                sonicterm_ui::pane::SplitAxis::Vertical => CursorIcon::ColResize,
                sonicterm_ui::pane::SplitAxis::Horizontal => CursorIcon::RowResize,
            };
            main_window.set_cursor(icon);
        }
    }

    pub(super) fn refresh_splitter_hover(&mut self, pixel_x: f32, pixel_y: f32) -> bool {
        if self.main().and_then(|window| window.splitter_drag.as_ref()).is_some() {
            // When: splitter_drag is Some, preserve its resize cursor and consume hover routing.
            return true;
        }
        let Some(hit) = self.splitter_hit_at(pixel_x, pixel_y) else {
            // When: splitter_hit_at returns None, clear any stale splitter hover cursor.
            let was_splitter = self
                .main_mut()
                .map(|window| window.splitter_hover.take().is_some())
                .unwrap_or(false);
            if was_splitter {
                if let Some(main_window) = self.main_window() {
                    main_window.set_cursor(CursorIcon::Default);
                }
            }
            return false;
        };
        if let Some(window) = self.main_mut() {
            window.hovered_url = None;
            window.hover_link = false;
            window.splitter_hover = Some(hit.axis);
        }
        self.set_splitter_cursor(hit.axis);
        true
    }

    pub(super) fn apply_splitter_drag(&mut self, pixel_x: f32, pixel_y: f32) -> bool {
        let Some(drag) = self.main().and_then(|window| window.splitter_drag.clone()) else {
            // When: splitter_drag is None, this motion is not a splitter gesture.
            return false;
        };
        let Some(outer) = self.main_pane_outer_rect() else {
            // When: main_pane_outer_rect is None, splitter geometry cannot be updated.
            return false;
        };
        let delta_x = pixel_x - drag.last_pos.0;
        let delta_y = pixel_y - drag.last_pos.1;
        if delta_x == 0.0 && delta_y == 0.0 {
            // When: delta_x and delta_y are both zero, consume the gesture without resizing.
            return true;
        }

        let tab_idx = self.main_tabs().map(|tab_bar| tab_bar.active_index()).unwrap_or(0);
        let changed = self
            .main_tab_states_mut()
            .and_then(|states| states.get_mut(tab_idx))
            .map(|state| {
                state.tree.resize_splitter_by_delta(&drag.splitter, outer, delta_x, delta_y)
            })
            .unwrap_or(false);

        if changed {
            if let Some(((cell_w, cell_h), inset)) = self.main_renderer().map(|renderer| {
                (
                    renderer.cell_size(),
                    [
                        renderer.padding_left_px(),
                        renderer.padding_right_px(),
                        renderer.padding_top_px(),
                        renderer.padding_bottom_px(),
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

        if let Some(window) = self.main_mut() {
            if let Some(active) = window.splitter_drag.as_mut() {
                active.last_pos = (pixel_x, pixel_y);
            }
            if changed {
                mark_all_panes_dirty(&window.panes);
            }
        }
        self.set_splitter_cursor(drag.axis);
        if changed {
            if let Some(main_window) = self.main_window() {
                crate::app::frame_counters::request_native_redraw(main_window);
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
        let renderer = child.renderer.as_ref()?;
        let (width, height) = renderer.logical_size();
        let top = (renderer.top_inset() - renderer.padding_top_px()).max(0.0);
        let bottom = renderer.bottom_inset();
        Some(sonicterm_ui::pane::Rect::new(
            0.0,
            top,
            width.max(0.0),
            (height - top - bottom).max(0.0),
        ))
    }

    /// Hit-test a splitter divider in the child window `win_id`.
    pub(super) fn splitter_hit_at_in_child(
        &self,
        win_id: WindowId,
        pixel_x: f32,
        pixel_y: f32,
    ) -> Option<sonicterm_ui::pane::SplitterHit> {
        let outer = self.child_pane_outer_rect(win_id)?;
        let child = self.windows.get(&win_id)?;
        let tab_idx = child.tabs.active_index();
        child.tab_states.get(tab_idx).and_then(|state| {
            state.tree.hit_splitter(outer, CHILD_SPLITTER_HIT_THICKNESS, pixel_x, pixel_y)
        })
    }

    pub(super) fn set_child_splitter_cursor(
        &self,
        win_id: WindowId,
        axis: sonicterm_ui::pane::SplitAxis,
    ) {
        if let Some(child) = self.windows.get(&win_id) {
            if let Some(native_window) = child.window.as_ref() {
                let icon = match axis {
                    sonicterm_ui::pane::SplitAxis::Vertical => CursorIcon::ColResize,
                    sonicterm_ui::pane::SplitAxis::Horizontal => CursorIcon::RowResize,
                };
                native_window.set_cursor(icon);
            }
        }
    }

    pub(super) fn refresh_child_splitter_hover(
        &mut self,
        win_id: WindowId,
        pixel_x: f32,
        pixel_y: f32,
    ) -> bool {
        let hit = self.splitter_hit_at_in_child(win_id, pixel_x, pixel_y);
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
                if let Some(native_window) = child.window.as_ref() {
                    native_window.set_cursor(CursorIcon::Default);
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
        pixel_x: f32,
        pixel_y: f32,
    ) -> Option<sonicterm_ui::pane::SplitAxis> {
        self.splitter_hit_at_in_child(win_id, pixel_x, pixel_y).map(|hit| hit.axis)
    }

    /// Test-only: drive the child splitter hover refresh directly and report
    /// whether hover state or cursor shape changed, so the no-button hover path
    /// is reachable with headless viewport geometry.
    #[doc(hidden)]
    pub fn __test_refresh_child_splitter_hover(
        &mut self,
        win_id: WindowId,
        pixel_x: f32,
        pixel_y: f32,
    ) -> bool {
        self.refresh_child_splitter_hover(win_id, pixel_x, pixel_y)
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
        pixel_x: f32,
        pixel_y: f32,
    ) -> bool {
        let Some(drag) = self.windows.get(&win_id).and_then(|child| child.splitter_drag.clone())
        else {
            // When: no `splitter_drag` is recorded, so this cursor move is not
            // part of a divider drag and belongs to the ordinary hover path.
            return false;
        };
        let Some(outer) = self.child_pane_outer_rect(win_id) else {
            // When: `child_pane_outer_rect` is unavailable, so there is no
            // layout basis to convert the pointer delta into a split ratio.
            return false;
        };
        let delta_x = pixel_x - drag.last_pos.0;
        let delta_y = pixel_y - drag.last_pos.1;
        if delta_x == 0.0 && delta_y == 0.0 {
            // When: `delta_x` and `delta_y` are both zero, so the pointer has not moved
            // and the drag stays live without re-laying out the tree.
            return true;
        }
        let tab_idx = self.windows.get(&win_id).map(|child| child.tabs.active_index()).unwrap_or(0);
        let changed = self
            .windows
            .get_mut(&win_id)
            .and_then(|child| child.tab_states.get_mut(tab_idx))
            .map(|state| {
                state.tree.resize_splitter_by_delta(&drag.splitter, outer, delta_x, delta_y)
            })
            .unwrap_or(false);
        if changed {
            if let Some(child) = self.windows.get_mut(&win_id) {
                resize_visible_panes_in_child(child);
            }
        }
        if let Some(child) = self.windows.get_mut(&win_id) {
            if let Some(active) = child.splitter_drag.as_mut() {
                active.last_pos = (pixel_x, pixel_y);
            }
            if changed {
                mark_all_panes_dirty(&child.panes);
                child.request_window_redraw();
            }
        }
        self.set_child_splitter_cursor(win_id, drag.axis);
        true
    }
}

/// Splitter hit thickness in logical px (mirror of `SPLITTER_HIT_THICKNESS`).
const CHILD_SPLITTER_HIT_THICKNESS: f32 = 8.0;
