//! Tabs responsibilities of the renderer, preserving its single owner.

use super::*;

impl GpuRenderer {
    /// Apply a new font family / size / line-height multiplier without
    /// reconstructing the renderer.
    ///
    /// The shelf-packed glyph atlas is cleared because existing tiles
    /// are sized for the old metrics — reusing them would render at the
    /// wrong pixel scale. The frame-key cache is also invalidated so
    /// Set (or clear) the translucent drag-chip overlay drawn on top
    /// of the frame. Called by the app on every CursorMoved during a
    /// held-tab drag, and with `None` on release.
    pub fn set_drag_chip(&mut self, chip: Option<DragChipOverlay>) {
        self.drag_chip = chip;
        // Bust the frame-key cache so a new chip position is actually
        // drawn — otherwise the no-change fast path would short-circuit.
        self.last_frame_key = None;
    }
}

impl GpuRenderer {
    /// Active drag chip overlay (if any). Read-only accessor used by
    /// tests and the app event loop to inspect the live chip state.
    pub fn drag_chip(&self) -> Option<&DragChipOverlay> {
        self.drag_chip.as_ref()
    }
}

impl GpuRenderer {
    /// Diagnostic — visual rect of the most recently rendered drag
    /// chip, or `None` if no chip was drawn. Test-only.
    #[doc(hidden)]
    pub fn last_drag_chip_visual(&self) -> Option<DragChipVisual> {
        self.drag_chip_visual
    }
}

impl GpuRenderer {
    /// Record where the pointer is, in the pixel space pointer events use
    /// (origin top-left), or `None` when it leaves the window. `tabs` is this
    /// window's tab bar, the one the next frame draws.
    ///
    /// Returns `true` only when the hovered tab changes, because the hovered
    /// tab is the only hover fact a frame draws and it is part of the frame
    /// key. A move within one tab, across empty bar space or over the terminal
    /// area returns `false` and leaves the frame key alone.
    pub fn set_hover_cursor(&mut self, pos: Option<(f32, f32)>, tabs: &TabBar) -> bool {
        if self.hover_cursor == pos {
            // When: `hover_cursor == pos`, the pointer did not move, so the hovered tab cannot change.
            return false;
        }
        let previous = self.hover_cursor;
        self.hover_cursor = pos;
        self.hovered_tab_index(tabs, previous) != self.hovered_tab_index(tabs, pos)
    }
}

impl GpuRenderer {
    /// Where this window's tab bar sits for the hover hit test.
    pub(super) fn tab_bar_hover_geometry(&self) -> TabBarHoverGeometry {
        TabBarHoverGeometry {
            width_px: self.config.width as f32,
            bar_height_px: self.tab_bar_logical_height(),
            top_offset_px: self.tab_bar_y_offset(),
            visible: self.tab_bar_visible,
        }
    }
}

impl GpuRenderer {
    /// The index of the tab under `cursor` in this window's bar, or
    /// `u32::MAX` for none. `render` and `set_hover_cursor` both resolve the
    /// hovered tab here, so the tab drawn hovered and the tab that decided a
    /// redraw are always the same.
    pub(super) fn hovered_tab_index(&self, tabs: &TabBar, cursor: Option<(f32, f32)>) -> u32 {
        hovered_tab_at(tabs, self.tab_bar_hover_geometry(), cursor)
    }
}

impl GpuRenderer {
    /// The vertical band the visible tab bar occupies, `(top, bottom)`, in the
    /// pixel space pointer positions use, or `None` while the bar is hidden.
    /// Each window tests its own recorded pointer against it to decide whether
    /// its tab widths hold.
    #[must_use]
    pub fn tab_bar_band(&self) -> Option<(f32, f32)> {
        let top = self.tab_bar_y_offset();
        self.tab_bar_visible.then_some((top, top + self.tab_bar_logical_height()))
    }
}

impl GpuRenderer {
    /// Measure changed tab titles with the tab font and store their widths on
    /// `tabs`, where drawing, hover, hit-testing, drag and tear-out slots,
    /// native drag snapshots and the overflow selector all read them.
    ///
    /// Call it right before [`Self::render`] with the same tabs. Only a tab
    /// whose title, badge, privilege marker, font or scale changed is shaped.
    /// While `hold` is set, a changed title, badge or marker is measured but
    /// laid out later, so no tab moves under the pointer; a font, scale or
    /// width-limit change lays the bar out at once. It measures on the CPU,
    /// through the tab-title font that `set_font` and the scale rebuild update.
    pub fn measure_tab_widths(
        &self,
        fonts: &FrameFonts,
        tabs: &mut TabBar,
        process_privileged: bool,
        hold: bool,
        now: Instant,
    ) -> ContentWidthRefresh {
        let _collect = crate::frame_stats::CollectGuard::enter(self.frame_sink.as_ref());
        self.debug_assert_prepared(fonts);
        self.tab_title_font.measure(tabs, process_privileged, hold, now)
    }
}

impl GpuRenderer {
    /// Deprecated close-button color override. The button is no longer
    /// drawn, but accepting the setting keeps older configs harmless.
    pub fn set_tab_close_override(&mut self, color: Option<&str>) -> bool {
        let parsed = color.map(|c| hex_to_premultiplied_rgba(c, 1.0));
        if self.tab_close_override != parsed {
            self.tab_close_override = parsed;
            self.last_frame_key = None;
            true
        } else {
            // When: `self.tab_close_override == parsed` — the override is
            // unchanged, so `false` tells the caller nothing needs redrawing.
            false
        }
    }
}

#[cfg(test)]
#[path = "core_tabs_tests.rs"]
mod core_tabs_tests;
