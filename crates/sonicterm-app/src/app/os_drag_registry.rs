//! Native tab-bar snapshot geometry and shared drop-target registry.

use super::*;

impl TabBarSnapshot {
    /// Returns `true` iff the screen point `(screen_x, screen_y)` is inside this
    /// window's outer rect (inclusive of left/top, exclusive of
    /// right/bottom — matches Win32 `RECT` semantics).
    pub fn window_contains(&self, screen_x: i32, screen_y: i32) -> bool {
        let (left, top, right, bottom) = self.window_rect;
        screen_x >= left && screen_x < right && screen_y >= top && screen_y < bottom
    }

    /// Returns `true` iff the screen point `(screen_x, screen_y)` is inside this
    /// window's tab bar rect.
    pub fn bar_contains(&self, screen_x: i32, screen_y: i32) -> bool {
        let (left, top, right, bottom) = self.bar_rect;
        screen_x >= left && screen_x < right && screen_y >= top && screen_y < bottom
    }

    /// Build a [`TabBarSnapshot`] from a computed `TabBarLayout` in
    /// window-local raster px plus the destination window's raster-px
    /// inner origin. All output rects are in screen-global raster px —
    /// the same coordinate system the OS reports drop cursors in.
    ///
    /// `inner_origin` is the window's `inner_position()` (top-left of
    /// the client area, screen-global raster px). `inner_size` is
    /// `inner_size()` in raster px.
    ///
    /// Used by the App's per-frame redraw path to publish the live tab
    /// bar geometry into the shared [`TabBarRegistry`] so platform OS-drag
    /// backends can hit-test drop cursors without re-entering the App.
    pub fn from_layout(
        window: Option<WindowId>,
        inner_origin: (i32, i32),
        inner_size: (u32, u32),
        layout: &sonicterm_ui::tabbar_view::TabBarLayout,
    ) -> Self {
        let (origin_x, origin_y) = inner_origin;
        let (inner_width, inner_height) = inner_size;
        let window_rect =
            (origin_x, origin_y, origin_x + inner_width as i32, origin_y + inner_height as i32);
        let bar = &layout.bar;
        let bar_rect = (
            origin_x + bar.x.round() as i32,
            origin_y + bar.y.round() as i32,
            origin_x + (bar.x + bar.w).round() as i32,
            origin_y + (bar.y + bar.h).round() as i32,
        );
        let mut tab_indices = Vec::with_capacity(layout.tabs.len());
        let mut tab_midpoints = Vec::with_capacity(layout.tabs.len());
        for tab in &layout.tabs {
            tab_indices.push(tab.idx);
            tab_midpoints.push(origin_x + (tab.bg_rect.x + tab.bg_rect.w * 0.5).ceil() as i32);
        }
        Self {
            window,
            window_rect,
            bar_rect,
            tab_midpoints,
            tab_indices,
            total_tabs: layout.total_tabs,
            overflow_append_from: layout.overflow.map(|control| origin_x + control.x.ceil() as i32),
        }
    }

    /// Resolve the same absolute visible-gap or overflow-append slot as TabBarLayout in screen coordinates.
    pub fn drop_slot(&self, screen_x: i32) -> usize {
        if self.overflow_append_from.is_some_and(|start| screen_x >= start) {
            // When: `screen_x` reaches overflow, preserve append-to-end independently of the visible segment.
            return self.total_tabs;
        }
        debug_assert_eq!(self.tab_midpoints.len(), self.tab_indices.len());
        for (&midpoint, &index) in self.tab_midpoints.iter().zip(&self.tab_indices) {
            if screen_x < midpoint {
                // When: `screen_x` precedes a visible midpoint, insert before its absolute `index`.
                return index;
            }
        }
        self.tab_indices.last().map_or(0, |index| (index + 1).min(self.total_tabs))
    }
}

impl TabBarRegistry {
    /// Construct an empty registry. The App owns one and shares
    /// `Arc<TabBarRegistry>` clones with backends through
    /// [`AppHandle::tab_bar_registry`].
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` iff no snapshots are published.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
