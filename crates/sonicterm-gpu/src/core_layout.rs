//! Layout responsibilities of the renderer, preserving its single owner.

use super::*;

impl GpuRenderer {
    /// Top inset reserved above the grid: OS titlebar band (when active)
    /// plus top window padding, returned in **raster px** so it lives in
    /// the same coordinate system as `config.width`/`config.height` and the
    /// rest of the renderer. The tab bar is always bottom-pinned,
    /// so its height is reserved via [`Self::bottom_inset`] instead of here.
    ///
    /// `titlebar_inset` and `padding_top` are stored in logical px (matching
    /// the config schema); both are scaled by [`Self::scale_factor`] before
    /// being summed so a 2x Retina display gets the right number of raster
    /// rows reserved for the OS titlebar band + user padding. Without the
    /// scale the grid was reporting one fewer row than the window could fit,
    /// leaving a dead strip below the last painted row that showed the
    /// surface clear color instead of vim's bg.
    ///
    /// A strip at the top sits between the titlebar band and the top padding, so its height is
    /// reserved here too.
    pub fn top_inset(&self) -> f32 {
        (self.titlebar_inset + self.padding_top) * self.scale_factor
            + self.status_line_placement().reserved_top
    }
}

impl GpuRenderer {
    /// Bottom inset reserved below the grid for the bottom-pinned tab bar,
    /// in **raster px** (same units as `config.height`). Returns 0 when
    /// the bar is hidden; the consumer still subtracts `padding_bottom *
    /// scale_factor` separately so window padding still applies when the
    /// bar is off.
    pub fn bottom_inset(&self) -> f32 {
        let tab_bar = if self.tab_bar_visible {
            self.tab_bar_logical_height()
        } else {
            // When: `!tab_bar_visible` — the bar reserves nothing. Window
            // padding is applied separately by the caller.
            0.0
        };
        tab_bar + self.status_line_placement().reserved_bottom
    }
}

impl GpuRenderer {
    /// The strip's band and the height it reserves on its edge, from the current geometry.
    pub(super) fn status_line_placement(&self) -> crate::status_strip::StripPlacement {
        let tab_bar = if self.tab_bar_visible {
            self.tab_bar_logical_height()
        } else {
            // When: `!tab_bar_visible`, a bottom strip sits on the window's bottom edge.
            0.0
        };
        crate::status_strip::strip_placement(
            self.status_line_position,
            self.config.height as f32,
            self.titlebar_inset * self.scale_factor,
            tab_bar,
            self.status_line_height(),
        )
    }
}

impl GpuRenderer {
    /// Y offset (in raster px) at which the tab bar layout should be
    /// anchored. The tab bar is always pinned to the bottom of the window.
    /// Callers pass this into [`TabBarLayout::with_top_offset`].
    pub fn tab_bar_y_offset(&self) -> f32 {
        let surf_h = self.config.height as f32;
        (surf_h - self.tab_bar_logical_height()).max(0.0)
    }
}

impl GpuRenderer {
    /// Raster-pixel height of the tab bar for the renderer's current font
    /// size. Derived from [`tab_bar_height`] (logical formula) and scaled
    /// to raster px to live in the same coordinate system as
    /// `config.width`/`config.height`. WezTerm fancy-mode parity: `font_size × 2 + 12` clamped.
    pub fn tab_bar_logical_height(&self) -> f32 {
        tab_bar_height(self.font_size) * self.scale_factor
    }
}

impl GpuRenderer {
    /// The titlebar inset alone (logical px) — the y-offset at which the
    /// tab bar strip itself begins, regardless of whether the bar is
    /// visible. Used by hit-testing / tab-bar layout to shift their
    /// rectangles down so clicks under the OS titlebar are not consumed
    /// as tab activations.
    pub fn titlebar_inset(&self) -> f32 {
        self.titlebar_inset
    }
}

impl GpuRenderer {
    /// Set the reserved OS titlebar band height (logical px). Called once
    /// from `app.rs` after creating each window so the renderer knows
    /// whether the macOS integrated-titlebar style is in effect.
    /// Invalidates the cached frame key so the next render() relays out.
    pub fn set_titlebar_inset(&mut self, inset: f32) {
        let clamped = inset.max(0.0);
        if (self.titlebar_inset - clamped).abs() < f32::EPSILON {
            // When: `titlebar_inset` is unchanged. This runs on every
            // window-state event, so clearing the key would relayout each time.
            return;
        }
        self.titlebar_inset = clamped;
        self.last_frame_key = None;
        self.last_pane_layout.clear();
    }
}

impl GpuRenderer {
    /// Show or hide the tab bar. Returns `true` if the visibility actually
    /// changed (so callers can decide whether to recompute grid dims).
    /// Invalidates the cached frame key so the next `render()` call rebuilds.
    pub fn set_tab_bar_visible(&mut self, visible: bool) -> bool {
        if self.tab_bar_visible == visible {
            // When: `tab_bar_visible == visible`. `false` tells the caller no
            // grid resize is needed for identical geometry.
            return false;
        }
        self.tab_bar_visible = visible;
        self.last_frame_key = None;
        self.last_pane_layout.clear();
        true
    }
}

impl GpuRenderer {
    /// Whether the tab bar is currently shown.
    pub fn tab_bar_visible(&self) -> bool {
        self.tab_bar_visible
    }
}

impl GpuRenderer {
    /// Update the requested LCD subpixel coverage order without rebuilding fonts or atlases.
    pub fn set_subpixel_aa_mode(&mut self, mode: SubpixelAaMode) -> bool {
        if self.subpixel_aa == mode {
            // When: `subpixel_aa == mode`, the retained frame already reflects this request.
            return false;
        }
        self.subpixel_aa = mode;
        self.last_frame_key = None;
        self.log_subpixel_aa_policy();
        self.request_window_redraw();
        true
    }
}

impl GpuRenderer {
    /// Requested LCD subpixel coverage order before platform/capability fallback.
    #[doc(hidden)]
    #[must_use]
    pub const fn subpixel_aa_mode(&self) -> SubpixelAaMode {
        self.subpixel_aa
    }
}

impl GpuRenderer {
    /// LCD mode after platform, target-opacity, and presenter capability fallback.
    #[doc(hidden)]
    #[must_use]
    pub fn effective_subpixel_aa_mode(&self) -> SubpixelAaMode {
        effective_subpixel_aa_mode(
            self.subpixel_aa,
            cfg!(windows),
            self.hardware_alpha_mode == CompositeAlphaMode::Opaque
                && self.bg_opacity >= 1.0 - f32::EPSILON,
            self.uses_windows_software_presenter(),
            self.device.features().contains(wgpu::Features::DUAL_SOURCE_BLENDING),
        )
    }
}

impl GpuRenderer {
    pub(super) fn log_subpixel_aa_policy(&self) {
        let opaque_target = self.hardware_alpha_mode == CompositeAlphaMode::Opaque
            && self.bg_opacity >= 1.0 - f32::EPSILON;
        let software_presenter = self.uses_windows_software_presenter();
        let dual_source_supported =
            self.device.features().contains(wgpu::Features::DUAL_SOURCE_BLENDING);
        let effective = effective_subpixel_aa_mode(
            self.subpixel_aa,
            cfg!(windows),
            opaque_target,
            software_presenter,
            dual_source_supported,
        );
        tracing::info!(
            target: "render_policy",
            renderer_role = self.render_timing_label,
            window_id = ?self.window.id(),
            requested = ?self.subpixel_aa,
            ?effective,
            windows_host = cfg!(windows),
            opaque_target,
            software_presenter,
            dual_source_supported,
            "renderer LCD subpixel policy"
        );
    }
}

impl GpuRenderer {
    /// Update scrollbar visibility policy from live config reload.
    ///
    /// `render()` folds this cached mode into the per-pane scrollbar emit
    /// path, so config changes must invalidate the frame key explicitly;
    /// otherwise an idle window could keep the previous scrollbar quads
    /// until some unrelated grid/theme/input change forced a redraw.
    pub fn set_scrollbar_mode(
        &mut self,
        mode: sonicterm_render_model::boundary::cfg::config::ScrollbarMode,
    ) -> bool {
        if self.scrollbar_mode == mode {
            // When: `scrollbar_mode == mode`. Every reload calls this, so the
            // early return keeps an unrelated edit from busting an idle frame.
            return false;
        }
        self.scrollbar_mode = mode;
        self.last_frame_key = None;
        true
    }
}

impl GpuRenderer {
    /// Current scrollbar visibility policy. Test-only inspector for the
    /// live-reload path; production code pushes updates via
    /// [`Self::set_scrollbar_mode`].
    #[doc(hidden)]
    pub fn scrollbar_mode(&self) -> sonicterm_render_model::boundary::cfg::config::ScrollbarMode {
        self.scrollbar_mode
    }
}

impl GpuRenderer {
    /// Update overlay panel padding from live config reload.
    pub fn set_panel_padding(&mut self, padding: f32) -> bool {
        let padding = padding.max(0.0);
        if (self.panel_padding - padding).abs() < f32::EPSILON {
            // When: `(self.panel_padding - padding).abs() < f32::EPSILON` — a
            // reload carried the same padding; `false` skips the relayout.
            return false;
        }
        self.panel_padding = padding;
        self.last_frame_key = None;
        true
    }
}

impl GpuRenderer {
    /// Update the cursor shape. The frame key carries the shape, so the next
    /// render damages only the cursor row rather than the whole surface.
    pub fn set_cursor_shape(&mut self, shape: CursorShape) {
        self.cursor_shape = shape;
    }
}

impl GpuRenderer {
    /// Current cursor shape.
    pub fn cursor_shape(&self) -> CursorShape {
        self.cursor_shape
    }
}

impl GpuRenderer {
    /// Enable or disable the cursor blink. Resets the blink phase so
    /// the user always sees a full-brightness cursor immediately after
    /// flipping the setting (no random mid-cycle pop).
    pub fn set_cursor_blink(&mut self, blink: bool) {
        if self.cursor_blink == blink {
            // When: `self.cursor_blink == blink` — returning also preserves
            // `blink_epoch`, so a reload cannot restart the blink phase.
            return;
        }
        self.cursor_blink = blink;
        self.blink_epoch = Instant::now();
    }
}

impl GpuRenderer {
    /// Whether the cursor is currently configured to blink.
    pub fn cursor_blink(&self) -> bool {
        self.cursor_blink
    }
}

impl GpuRenderer {
    /// Suggested wall-clock interval between blink-only redraws. The
    /// app loop schedules a redraw at this cadence whenever the cursor
    /// is visible AND [`Self::cursor_blink`] is true; otherwise nothing
    /// new would render and the request would be wasted.
    pub fn blink_redraw_interval(&self) -> std::time::Duration {
        ui_cursor::redraw_interval()
    }
}

impl GpuRenderer {
    /// Wall-clock instant at which the next blink phase bucket begins,
    /// or `None` when blinking is disabled. The app loop should set
    /// `ControlFlow::WaitUntil(this)` so the renderer wakes up exactly
    /// at bucket boundaries instead of busy-looping `request_redraw()`
    /// after every frame (the project landmine flagged).
    pub fn next_blink_redraw_at(&self) -> Option<Instant> {
        // Blink-driven redraws are intentionally disabled in the idle
        // path. Re-shaping the grid 26×/sec just to fade the cursor
        // alpha melted the headless CPU bench at 17% — see the
        // `cursor_phase: 0` comment where `FrameKey` is built. The
        // cursor still re-evaluates its alpha on every real redraw
        // (PTY bytes, keys, mouse, resize, focus), which keeps it
        // visibly pulsing whenever the user is doing anything. Pure
        // idle leaves the cursor frozen at a fixed (always-visible)
        // alpha — strictly better than burning CPU on a backgrounded
        // window. The remaining fields (`cursor_blink`,
        // `window_focused`, `blink_epoch`) are kept so a future
        // event-driven re-enable (e.g. only blink for the first 5s
        // after a keypress) can pick the right starting bucket.
        let _ = (&self.cursor_blink, &self.window_focused, &self.blink_epoch);
        None
    }
}

impl GpuRenderer {
    /// Update the cached keyboard-focus flag for the OS window.
    ///
    /// The text cursor is hidden while the window is unfocused. The frame key
    /// carries focus, so the next render damages only the cursor rows and the
    /// tab bar rather than the whole surface.
    pub fn set_window_focused(&mut self, focused: bool) {
        self.window_focused = focused;
    }
}

impl GpuRenderer {
    /// Whether the OS window currently has keyboard focus.
    pub fn window_focused(&self) -> bool {
        self.window_focused
    }
}

impl GpuRenderer {
    /// Set the window label that renderer-internal timing logs are tagged with.
    ///
    /// Affects diagnostics only; no frame state changes, so the frame key is
    /// deliberately left intact.
    pub fn set_render_timing_label(&mut self, label: &'static str) {
        self.render_timing_label = label;
    }
}

impl GpuRenderer {
    /// Start the short focus-confirmation flash on `pane_id`.
    ///
    /// The flash is bounded by `PANE_FOCUS_FLASH_DURATION` and animates
    /// through the frame key's quantised bucket, so this requests one redraw
    /// rather than starting a repeating timer.
    pub fn flash_pane_focus(&mut self, pane_id: u64) {
        self.pane_focus_flash = Some((pane_id, Instant::now()));
        self.last_frame_key = None;
        self.request_window_redraw();
    }
}

impl GpuRenderer {
    /// Accept the historical per-frame inactive-pane cursor list.
    /// Inactive panes no longer draw cursors, so any previously cached
    /// cursor records are cleared and new records are ignored.
    pub fn set_inactive_pane_cursors(&mut self, _cursors: Vec<InactivePaneCursor>) {
        if !self.inactive_pane_cursors.is_empty() {
            self.inactive_pane_cursors.clear();
            self.last_frame_key = None;
        }
    }
}

impl GpuRenderer {
    pub(super) fn pane_focus_flash_bucket(&mut self, now: Instant) -> u8 {
        let Some((_, started_at)) = self.pane_focus_flash else {
            // When: `self.pane_focus_flash` is None — no flash is running, and
            // 0 keeps the bucket out of the frame key so nothing animates.
            return 0;
        };
        let elapsed = now.saturating_duration_since(started_at);
        let Some((bucket, _)) = pane_focus_flash_sample(elapsed) else {
            // When: `pane_focus_flash_sample(elapsed)` returns `None`, the bounded
            // flash expired; clearing its state ends the redraw chain.
            self.pane_focus_flash = None;
            return 0;
        };
        bucket
    }
}

impl GpuRenderer {
    pub(super) fn pane_focus_flash_alpha(&self, now: Instant) -> Option<(u64, f32)> {
        let (pane_id, started_at) = self.pane_focus_flash?;
        let elapsed = now.saturating_duration_since(started_at);
        pane_focus_flash_sample(elapsed).map(|(_, alpha)| (pane_id, alpha))
    }
}

impl GpuRenderer {
    /// Current physical surface width in pixels.
    pub fn width(&self) -> u32 {
        self.config.width
    }
}

impl GpuRenderer {
    /// Current physical surface height in pixels.
    pub fn height(&self) -> u32 {
        self.config.height
    }
}

impl GpuRenderer {
    /// Left padding (logical px). Kept for backward compatibility with
    /// callers that pre-date per-side padding; new code should prefer
    /// the per-side accessors below.
    pub fn padding(&self) -> f32 {
        self.padding_left
    }
}

impl GpuRenderer {
    /// Left padding in logical pixels.
    pub fn padding_left(&self) -> f32 {
        self.padding_left
    }
}

impl GpuRenderer {
    /// Right padding in logical pixels.
    pub fn padding_right(&self) -> f32 {
        self.padding_right
    }
}

impl GpuRenderer {
    /// Top padding in logical pixels (above any tab bar / titlebar inset).
    pub fn padding_top(&self) -> f32 {
        self.padding_top
    }
}

impl GpuRenderer {
    /// Bottom padding in logical pixels.
    pub fn padding_bottom(&self) -> f32 {
        self.padding_bottom
    }
}

impl GpuRenderer {
    /// Left padding scaled to **raster px**, i.e. the same coordinate
    /// system as `config.width`/`config.height` and the rest of the
    /// renderer. Prefer this over [`Self::padding_left`] when
    /// building geometry that will be handed back to the renderer (e.g.
    /// the per-pane rect in `compute_pane_rects_for`). Mixing the
    /// logical-px accessor with raster surface dims off-by-ones the row
    /// count and leaves a dead strip below the last painted row.
    pub fn padding_left_px(&self) -> f32 {
        self.padding_left * self.scale_factor
    }
}

impl GpuRenderer {
    /// Right padding scaled to raster px. See [`Self::padding_left_px`].
    pub fn padding_right_px(&self) -> f32 {
        self.padding_right * self.scale_factor
    }
}

impl GpuRenderer {
    /// Top padding scaled to raster px. See [`Self::padding_left_px`].
    /// `top_inset()` includes titlebar padding; `pane_grid_origin()` also
    /// includes pane position and bottom-alignment slack after layout.
    pub fn padding_top_px(&self) -> f32 {
        self.padding_top * self.scale_factor
    }
}

impl GpuRenderer {
    /// Bottom padding scaled to raster px. See [`Self::padding_left_px`].
    pub fn padding_bottom_px(&self) -> f32 {
        self.padding_bottom * self.scale_factor
    }
}

impl GpuRenderer {
    /// Current planned text origin for a pane, absent before layout or after a geometry change.
    pub fn pane_grid_origin(&self, pane_id: u64) -> Option<[f32; 2]> {
        self.last_pane_layout
            .iter()
            .find(|pane| pane.id == pane_id)
            .map(|pane| [pane.origin_x_logical, pane.origin_y_logical])
    }
}

impl GpuRenderer {
    /// Test-only: the configured surface size `(width, height)` in physical pixels, which a
    /// `Resized` at the same size leaves `Unchanged`.
    #[doc(hidden)]
    #[must_use]
    pub fn surface_size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }
}

impl GpuRenderer {
    /// Rendered layout of a pane from the most recent frame, absent before layout.
    #[doc(hidden)]
    pub fn pane_layout(&self, pane_id: u64) -> Option<PaneLayoutSnapshot> {
        self.last_pane_layout.iter().find(|pane| pane.id == pane_id).copied()
    }
}

impl GpuRenderer {
    /// Test-only raster-pixel pane origins from the last render; production code must not depend on this diagnostic.
    #[doc(hidden)]
    pub fn last_emitted_origins(&self) -> Vec<(u64, [f32; 2])> {
        self.last_emit_origins.clone()
    }
}

impl GpuRenderer {
    /// Update all four padding values at once (used by the live config
    /// reload path so editing `sonicterm.toml` takes effect without restart).
    /// Invalidates the cached frame so the next render relays out.
    pub fn set_padding(&mut self, padding: [f32; 4]) {
        let [l, r, t, b] = padding;
        if (self.padding_left - l).abs() < f32::EPSILON
            && (self.padding_right - r).abs() < f32::EPSILON
            && (self.padding_top - t).abs() < f32::EPSILON
            && (self.padding_bottom - b).abs() < f32::EPSILON
        {
            // When: all four `.abs() < f32::EPSILON` — a reload carried the
            // same padding, so relayout and a frame-key clear are wasted.
            return;
        }
        self.padding_left = l;
        self.padding_right = r;
        self.padding_top = t;
        self.padding_bottom = b;
        self.last_frame_key = None;
        self.last_pane_layout.clear();
    }
}

impl GpuRenderer {
    /// Render surface width and height in raster pixels, despite the compatibility method name.
    pub fn logical_size(&self) -> (f32, f32) {
        (self.config.width as f32, self.config.height as f32)
    }
}

impl GpuRenderer {
    /// Grid `(cols, rows)` from raster surface and cell dimensions; logical padding is scaled before subtraction.
    pub fn cells(&self) -> (u16, u16) {
        let surf_w = self.config.width as f32;
        let surf_h = self.config.height as f32;
        let sf = self.scale_factor;
        let inner_w = (surf_w - self.padding_left * sf - self.padding_right * sf).max(self.cell_w);
        let inner_h = (surf_h - self.top_inset() - self.bottom_inset() - self.padding_bottom * sf)
            .max(self.cell_h);
        let cols = (inner_w / self.cell_w).floor() as u64;
        let rows = (inner_h / self.cell_h).floor() as u64;
        bounded_grid_size(cols, rows)
    }
}

impl GpuRenderer {
    /// Cell width and height in raster pixels, matching rendered pane content rectangles.
    pub fn cell_size(&self) -> (f32, f32) {
        (self.cell_w, self.cell_h)
    }
}

impl GpuRenderer {
    /// Ask the native window to redraw, counting the request when this renderer counts. Every
    /// renderer-owned request goes through here; scheduling is exactly `request_redraw`.
    pub(crate) fn request_window_redraw(&self) {
        if let Some(sink) = &self.frame_sink {
            // this renderer counts, its own native request is recorded.
            sink.note_native_request();
        }
        self.window.request_redraw();
    }
}

impl GpuRenderer {
    /// Clear retained frame identity in memory, forcing the next real frame to assemble and draw in full.
    pub fn invalidate_retained_frame(&mut self) {
        self.last_frame_key = None;
    }
}

impl GpuRenderer {
    /// Install the callback that wakes the app after this renderer's device
    /// changes state. Renderers that share a device keep the first waker.
    pub fn set_device_state_waker(&self, waker: DeviceStateWaker) -> bool {
        self.device_errors.set_waker(waker)
    }
}

impl GpuRenderer {
    /// True when wgpu fell back to a CPU/software rasterizer for this window.
    /// The app uses this to degrade frame pacing and per-frame animation in
    /// the no-GPU case.
    pub fn is_software_rendering(&self) -> bool {
        self.software_rendering
    }
}

impl GpuRenderer {
    /// Whether the no-GPU degrade path is active for this window.
    ///
    /// Distinct from [`Self::is_software_rendering`]: that reports what the
    /// adapter is, this reports the resolved policy after
    /// `[appearance].software_render_mode` is applied, so `Force` degrades on
    /// real hardware and `Off` declines to degrade on a CPU rasterizer.
    pub fn is_software_render_degraded(&self) -> bool {
        self.software_render_degrade
    }
}

impl GpuRenderer {
    /// Translate physical-pixel `(px, py)` (as winit reports) into a
    /// `(row, col)` cell address inside the grid, or `None` if the point
    /// falls outside the grid (in the tab bar, padding, etc.).
    ///
    /// Winit physical pixels match renderer raster pixels; no DPI division is needed.
    ///
    /// pane-aware. After the first `render` call, this resolves
    /// the click against the per-pane layout captured in
    /// `last_pane_layout` and uses that pane's reconstructed
    /// `snapped_cell_x` cache to pick a column. This matters at
    /// fractional DPI (1.25/1.5/1.75) where naive `(x / cell_w).floor()`
    /// disagrees with the device-pixel-snapped edges the renderer
    /// actually drew on — off-by-one column near the right side of wide
    /// grids — and at split layouts where the right pane's column 0 is
    /// not at `padding_left`.
    ///
    /// Before the first render the layout snapshot is empty and we fall
    /// back to the legacy single-grid arithmetic; callers should not
    /// hit-test before rendering, but tests / early input events
    /// previously did and the legacy behaviour is preserved for them.
    pub fn pixel_to_cell(&self, px: f32, py: f32) -> Option<(u16, u16)> {
        self.pixel_to_pane_cell(px, py).map(|(_, row, col)| (row, col))
    }
}

impl GpuRenderer {
    /// Translate physical pixels into the exact rendered pane and cell.
    ///
    /// The pane identity and cell coordinates come from one layout snapshot, so
    /// split-pane clicks cannot mix app geometry with device-pixel-snapped edges.
    pub fn pixel_to_pane_cell(&self, px: f32, py: f32) -> Option<(u64, u16, u16)> {
        // Winit physical pixels already match the renderer's raster coordinates.
        // When the tab bar is pinned to the bottom of the window, clicks
        // inside the bar strip must NOT resolve to a phantom grid cell —
        // otherwise selection drags initiated in the bar would extend
        // the underlying grid selection. Reject anything below the
        // grid's content area. Padding is logical-px stored, so scale.
        let surf_h = self.config.height as f32;
        let sf = self.scale_factor;
        let content_bottom = surf_h - self.bottom_inset() - self.padding_bottom * sf;
        if py >= content_bottom {
            // When: `py >= content_bottom` — the point is in the tab-bar strip
            // or bottom padding; a phantom cell would extend grid selection.
            return None;
        }
        if py < self.top_inset() {
            // When: `py < self.top_inset()` — above the grid, in the titlebar
            // band or top padding, so no row corresponds to it.
            return None;
        }
        if self.last_pane_layout.is_empty() {
            // When: `last_pane_layout.is_empty()` — no render has run yet, so
            // legacy single-grid arithmetic (padding + cell_w) is used.
            let (cols, rows) = self.cells();
            let geometry = sonicterm_render_model::pane_content_geometry(
                PixelRect { x: 0, y: 0, w: self.config.width, h: self.config.height },
                [
                    self.padding_left_px(),
                    self.padding_right_px(),
                    self.top_inset(),
                    self.bottom_inset() + self.padding_bottom_px(),
                ],
                self.cell_h,
                rows,
            );
            let x = px - geometry.grid.x;
            let y = py - geometry.grid.y;
            if x < 0.0 || y < 0.0 {
                // When: `x < 0.0 || y < 0.0` — left of or above the grid
                // origin, which floors to a negative cell index.
                return None;
            }
            let col = (x / self.cell_w).floor() as i32;
            let row = (y / self.cell_h).floor() as i32;
            if col < 0 || row < 0 {
                // When: `col < 0 || row < 0` — a fractional origin can still
                // floor below zero after the non-negative check above.
                return None;
            }
            return (row < i32::from(rows) && col < i32::from(cols))
                .then_some((0, row as u16, col as u16));
        }
        // Pane resolution: find the pane whose raster-px rect contains
        // (px, py). Split panes have different origins, so this MUST
        // happen before the column search.
        let pane = self.last_pane_layout.iter().find(|p| {
            px >= p.origin_x_logical
                && px < p.origin_x_logical + p.w_logical
                && py >= p.origin_y_logical
                && py < p.origin_y_logical + p.h_logical
        })?;
        let local_x = px - pane.origin_x_logical;
        let local_y = py - pane.origin_y_logical;
        if local_x < 0.0 || local_y < 0.0 {
            // When: `local_x < 0.0 || local_y < 0.0` — float error at a pane
            // edge can push the point just outside the rect that matched.
            return None;
        }
        // Column: linear scan over the pane's snapped_cell_x edges so we
        // pick the bucket the renderer actually drew. Half-open
        // `edge[col] <= px < edge[col+1]`; boundaries resolve to the
        // RHS cell, which matches `partition_point`'s contract.
        // Binary search over edges computed on demand: no edge vector per lookup.
        let col = snapped_col_at(px, pane.origin_x_logical, pane.cell_w_logical, pane.cols)?;
        // Row: cell_h has no per-cell snapping cache today, so the
        // straight division is correct. Clamp to the pane's grid.
        let row_f = local_y / pane.cell_h_logical;
        if row_f < 0.0 {
            // When: `row_f < 0.0` — a non-positive `cell_h_logical` in the
            // snapshot would invert the division.
            return None;
        }
        let row = row_f.floor() as i32;
        if row < 0 || row >= pane.rows as i32 {
            // When: `row < 0 || row >= pane.rows` — the point is inside the
            // pane rect but below its last text row, in trailing padding.
            return None;
        }
        Some((pane.id, row as u16, col))
    }
}

#[cfg(test)]
#[path = "core_layout_tests.rs"]
mod core_layout_tests;
