//! Status strip responsibilities of the renderer, preserving its single owner.

use super::*;

impl GpuRenderer {
    /// Raster-px height of the status line strip; 0 while it is hidden. It
    /// follows the font size and scale factor, so zoom and DPI changes refit it.
    pub fn status_line_height(&self) -> f32 {
        if self.status_line_visible {
            crate::status_strip::status_line_height(self.font_size, self.scale_factor)
        } else {
            // When: `!status_line_visible`, the strip reserves no rows.
            0.0
        }
    }
}

impl GpuRenderer {
    /// Raster-px top edge of the strip: below the titlebar band at the top; at the bottom,
    /// directly above the tab bar, or on the bottom edge when the tab bar is hidden.
    pub fn status_line_top(&self) -> f32 {
        self.status_line_placement().top
    }
}

impl GpuRenderer {
    /// Set the strip text's share of the live terminal font size. Returns `true` when it changed:
    /// the strip's stack is rebuilt at the new size and the atlas reset, since a glyph key carries
    /// no size, so the caller redraws. The strip's height and the panes do not change.
    pub fn set_status_line_font_scale(&mut self, font_scale: f32) -> bool {
        if (self.status_line_font_scale - font_scale).abs() < f32::EPSILON {
            // When: `font_scale` equals `status_line_font_scale`, the strip keeps its stack and tiles.
            return false;
        }
        self.status_line_font_scale = font_scale;
        self.rebuild_status_line_stack();
        self.status_line_fit = None;
        self.status_line_cap_height = None;
        self.reset_glyph_atlas_in_place("status_line_font_scale");
        self.glyph_atlas_retry_without_eviction = false;
        self.row_glyph_cache.invalidate_all();
        self.last_frame_key = None;
        true
    }
}

impl GpuRenderer {
    /// Rebuild the strip's stack from the body stack at the renderer's scale; no body stack, no
    /// strip stack.
    pub(super) fn rebuild_status_line_stack(&mut self) {
        let size =
            crate::status_strip::status_line_font_size(self.font_size, self.status_line_font_scale);
        self.status_line_font_stack =
            self.font_stack.as_ref().map(|body| body.with_font_size(f64::from(size)));
    }
}

impl GpuRenderer {
    /// Move the strip to `position`. Returns `true` when a shown strip moved, so the caller
    /// refits panes to the new insets once; a hidden strip reserves nothing and moves nothing.
    pub fn set_status_line_position(
        &mut self,
        position: sonicterm_render_model::boundary::cfg::status_line::StatusPosition,
    ) -> bool {
        if self.status_line_position == position {
            // When: `status_line_position == position`, the strip stays where it is.
            return false;
        }
        self.status_line_position = position;
        if !self.status_line_visible {
            // When: `!status_line_visible`, no band or inset changes until it is shown.
            return false;
        }
        // The presented layout stays: the old band is on screen until a frame at the new edge
        // presents, and that frame's stage and settle replace it.
        self.status_line_fit = None;
        self.last_frame_key = None;
        self.last_pane_layout.clear();
        true
    }
}

impl GpuRenderer {
    /// Show or hide the strip. Returns `true` when visibility changed, so the
    /// caller refits panes to the new insets.
    pub fn set_status_line_visible(&mut self, visible: bool) -> bool {
        if self.status_line_visible == visible {
            // When: `status_line_visible == visible`, the geometry is unchanged.
            return false;
        }
        self.status_line_visible = visible;
        self.status_line_fit = None;
        self.status_line_layouts.clear();
        self.last_frame_key = None;
        self.last_pane_layout.clear();
        true
    }
}

impl GpuRenderer {
    /// Whether the strip is shown.
    pub fn status_line_visible(&self) -> bool {
        self.status_line_visible
    }
}

impl GpuRenderer {
    /// Replace the strip's segments. Returns `true` when its pixels change, so
    /// the caller requests a frame; that frame damages only the strip band.
    pub fn set_status_line_view(&mut self, view: StatusLineView) -> bool {
        if self.status_line_view == view {
            // When: the view is identical, there is nothing to redraw.
            return false;
        }
        // A pixel change reaches the frame key as `status_hash`, which damages only the band.
        let pixels_changed = self.status_line_view.pixel_hash != view.pixel_hash;
        self.status_line_view = view;
        pixels_changed
    }
}

impl GpuRenderer {
    /// Draw the keyboard focus ring on segment `focus`, doubled when its flag is set, or no ring
    /// for `None`. Returns `true` when the ring changed, so the caller requests a frame.
    pub fn set_status_line_focus(
        &mut self,
        focus: Option<(sonicterm_render_model::boundary::cfg::status_line::StatusSegmentId, bool)>,
    ) -> bool {
        if self.status_line_focus == focus {
            // When: `status_line_focus == focus`, the ring is unchanged and nothing redraws.
            return false;
        }
        self.status_line_focus = focus;
        self.last_frame_key = None;
        true
    }
}

impl GpuRenderer {
    /// The segments the strip may present.
    pub fn status_line_view(&self) -> &StatusLineView {
        &self.status_line_view
    }
}

impl GpuRenderer {
    /// The visible strip's band and the segments the last drawn frame presented; `None` while hidden.
    pub fn status_line_hit(&self) -> Option<crate::status_strip::StripHit> {
        // The band and segments are the committed unit, never the live geometry of a later resize.
        self.status_line_layouts.presented().filter(|_| self.status_line_visible).cloned()
    }
}

impl GpuRenderer {
    /// Whether raster-px `y` falls inside the visible strip band.
    pub fn status_line_contains(&self, y: f32) -> bool {
        self.status_line_visible
            && self.status_line_layouts.presented().is_some_and(|strip| strip.contains(y))
    }
}

impl GpuRenderer {
    /// The presented segment under raster-px `(x, y)`, from the last drawn frame.
    pub fn status_segment_at(
        &self,
        x: f32,
        y: f32,
    ) -> Option<&crate::status_strip::PresentedStatusSegment> {
        if !self.status_line_contains(y) {
            // When: `(x, y)` is outside the band, no segment is under it.
            return None;
        }
        let strip = self.status_line_layouts.presented()?;
        crate::status_strip::segment_at(&strip.segments, x)
    }
}
