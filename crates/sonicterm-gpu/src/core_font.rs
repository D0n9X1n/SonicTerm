//! Font responsibilities of the renderer, preserving its single owner.

use super::*;

pub(crate) struct RendererFontStacks {
    pub(crate) body: Option<sonicterm_engine::FontStack>,
    pub(crate) tab_title: Option<sonicterm_engine::FontStack>,
    pub(crate) palette_footer: Option<sonicterm_engine::FontStack>,
    pub(crate) status_line: Option<sonicterm_engine::FontStack>,
}

pub(crate) fn renderer_font_stacks(
    family: &str,
    body_size: f32,
    dpi: usize,
    weight_scale: f32,
    font_dirs: &[PathBuf],
) -> RendererFontStacks {
    let body = sonicterm_engine::FontStack::try_new_full_with_weight_and_font_dirs(
        family,
        f64::from(body_size),
        dpi,
        weight_scale,
        font_dirs,
    )
    .ok();
    renderer_font_views(body, body_size)
}

/// The renderer's four stacks from `body`: the tab-title, palette-footer and status line views
/// share its configuration at their own sizes for a `body_size` grid font.
pub(crate) fn renderer_font_views(
    body: Option<sonicterm_engine::FontStack>,
    body_size: f32,
) -> RendererFontStacks {
    let tab_title =
        body.as_ref().map(|stack| stack.with_font_size(f64::from(tab_title_font_size(body_size))));
    let palette_footer = body
        .as_ref()
        .map(|stack| stack.with_font_size(f64::from(palette_footer_font_size(body_size))));
    let status_line = body.as_ref().map(|stack| {
        stack.with_font_size(f64::from(crate::status_strip::status_line_font_size(
            body_size,
            crate::status_strip::default_font_scale(),
        )))
    });
    RendererFontStacks { body, tab_title, palette_footer, status_line }
}

impl GpuRenderer {
    /// the next `render()` call cannot short-circuit through the
    /// fast-path against a now-stale frame.
    pub fn set_font(&mut self, family: &str, size: f32, line_height_mult: f32, weight_scale: f32) {
        let weight_scale = effective_font_weight_scale(weight_scale);
        let dpi = (72.0 * self.scale_factor).round().max(1.0) as usize;
        let new_stacks = renderer_font_stacks(family, size, dpi, weight_scale, &self.font_dirs);
        self.adopt_font_stacks(family, size, line_height_mult, weight_scale, new_stacks);
    }
}

impl GpuRenderer {
    /// Test seam: adopt `stack` as the body font under the name `family`, with tab-title and
    /// footer views at their usual sizes, through the same path `set_font` takes. Lets a test
    /// drive a renderer with faces whose coverage it controls.
    #[doc(hidden)]
    pub fn __test_adopt_body_font_stack(
        &mut self,
        family: &str,
        stack: sonicterm_engine::FontStack,
    ) {
        let size = self.font_size;
        let stacks = renderer_font_views(Some(stack), size);
        self.adopt_font_stacks(family, size, self.line_height_mult, self.font_weight_scale, stacks);
    }
}

impl GpuRenderer {
    /// Install `new_stacks` for `family` at `size`: recompute cell metrics, swap the stacks (the
    /// body through the waker-attaching seam), and drop every cache built with the old faces.
    pub(super) fn adopt_font_stacks(
        &mut self,
        family: &str,
        size: f32,
        line_height_mult: f32,
        weight_scale: f32,
        new_stacks: RendererFontStacks,
    ) {
        let (new_cell_w, natural_cell_h) =
            match new_stacks.body.as_ref().and_then(|s| s.cell_metrics_raster_px().ok()) {
                Some(m) => (m.cell_w as f32, m.cell_h as f32),
                None => (self.raster_px(size * 0.6), self.raster_px(size * 1.2)),
            };
        let new_line_h = natural_cell_h * line_height_mult.max(0.0).max(0.01);
        let no_change = self.font_family == family
            && (self.font_size - size).abs() < f32::EPSILON
            && (self.line_height - new_line_h).abs() < f32::EPSILON
            && (self.font_weight_scale - weight_scale).abs() < f32::EPSILON
            && (self.cell_w - new_cell_w).abs() < f32::EPSILON
            && (self.cell_h - new_line_h).abs() < f32::EPSILON;
        if no_change {
            // When: `no_change` — family, size, weight, and both cell metrics
            // all match. The body below drops the atlas and both row caches.
            return;
        }
        self.font_family = family.to_string();
        self.font_size = size;
        self.line_height = new_line_h;
        self.font_weight_scale = weight_scale;
        self.line_height_mult = line_height_mult.max(0.0).max(0.01);
        // A new body stack has a new notice; it gets the same wake so its completions redraw.
        frame_fonts::install_body_stack(
            &mut self.font_stack,
            new_stacks.body,
            self.fallback_waker.as_ref(),
        );
        self.tab_title_font.set_font(family, size, weight_scale, new_stacks.tab_title);
        self.palette_footer_font_stack = new_stacks.palette_footer;
        self.status_line_font_stack = new_stacks.status_line;
        // The views are built at the default scale; the strip keeps the renderer's own.
        self.rebuild_status_line_stack();
        self.status_line_fit = None;
        self.status_line_cap_height = None;
        self.cell_w = new_cell_w;
        self.cell_h = new_line_h;
        self.reset_glyph_atlas_in_place("font_change");
        self.glyph_atlas_retry_without_eviction = false;
        // SwashRasterizer prebake gone. Atlas is now lazily
        // filled by the wezterm rasterizer on the next render.
        self.row_glyph_cache.invalidate_all();
        // Kept titles and chrome runs hold the old faces' glyph ids, whatever their keys say.
        self.chrome_caches.clear_runs();
        self.line_quad_cache.invalidate_all();
        self.last_frame_key = None;
        self.last_pane_layout.clear();
        // Field boundaries were measured with the previous font.
        self.presented_fields.clear();
        tracing::info!(
            "renderer.set_font: family={family} size={size} line_h={} cell={:.2}x{:.2}",
            self.line_height,
            self.cell_w,
            self.cell_h
        );
    }
}

impl GpuRenderer {
    /// Apply changed DPI by rebuilding glyph state and raster cell metrics; logical chrome sizes scale with it.
    pub fn set_scale_factor(&mut self, scale_factor: f32) {
        if !scale_factor_rebuild_required(self.scale_factor, scale_factor) {
            // When: `!scale_factor_rebuild_required` — the DPI is unchanged
            // within epsilon, and `rebuild_for_sf` re-rasterizes every glyph.
            return;
        }
        self.rebuild_for_sf(scale_factor);
    }
}

impl GpuRenderer {
    /// Force-rebuild atlas + GPU upload for the given DPI multiplier,
    /// regardless of whether the cached value matches. Used by the
    /// tear-out path where `GpuRenderer::new` may have latched the
    /// wrong scale (window not yet placed on a display, so the OS
    /// reports 1.0); once the OS places the new window on its real
    /// Retina display, we must re-rasterize glyphs at the correct
    /// physical em-size or the child window shows blurry tiles +
    /// atlas tofu instead of real text. See the bug report on
    /// torn-out windows rendering with wrong cell width and missing
    /// nerd-font glyphs.
    pub fn force_rebuild_for_scale(&mut self, sf: f32) {
        self.rebuild_for_sf(sf);
    }
}

impl GpuRenderer {
    /// Single helper that owns the rasterizer-px target derived
    /// from `font_size * DPI`. Every callsite (grid + chrome) routes
    /// a logical font size through here to obtain the raster-px
    /// em-size the font stack rasterizes at.
    #[inline]
    pub(super) fn raster_px(&self, font_size: f32) -> f32 {
        font_size * self.scale_factor
    }
}

impl GpuRenderer {
    /// How far below a body row's cell top its glyphs' baseline sits, in raster pixels: four fifths
    /// of the cell height. Every row's glyph quads are placed from this offset.
    #[doc(hidden)]
    #[must_use]
    pub fn baseline_y_in_cell(&self) -> f32 {
        self.cell_h * 0.8
    }
}

impl GpuRenderer {
    /// Scale a logical-px chrome constant into the renderer's physical/raster-px
    /// coordinate space. Chrome layout literals (badge/search-bar/palette sizes,
    /// paddings, radii, sub-cell thicknesses) are authored at scale-factor 1.0;
    /// route them through this so they track the display DPI like glyphs do.
    /// Window-anchored POSITIONS (edge margins, centering offsets) must NOT use
    /// this — they stay in window space. See.
    #[inline]
    pub(super) fn chrome_px(&self, logical: f32) -> f32 {
        logical * self.scale_factor
    }
}

impl GpuRenderer {
    pub(super) fn rebuild_for_sf(&mut self, sf: f32) {
        let sf = sf.max(0.1);
        self.scale_factor = sf;
        // Reset glyph contents for the new DPI; atlas tiles are rasterized lazily on demand.
        self.reset_glyph_atlas_in_place("dpi_change");
        self.glyph_atlas_retry_without_eviction = false;
        // Update each stack's DPI before rerasterization; failed metric lookup preserves the prior cell measurement.
        let fs_dpi = (72.0 * sf).round() as usize;
        // Body, tab-title, footer and status line stacks rescale in that order; the tab-title font also
        // records the scale, so the next width measurement shapes every title again.
        if let Some(stack) = self.font_stack.as_ref() {
            stack.change_scaling(stack.get_font_scale(), fs_dpi);
        }
        self.tab_title_font.set_scale_factor(sf, fs_dpi);
        if let Some(stack) = self.palette_footer_font_stack.as_ref() {
            stack.change_scaling(stack.get_font_scale(), fs_dpi);
        }
        if let Some(stack) = self.status_line_font_stack.as_ref() {
            stack.change_scaling(stack.get_font_scale(), fs_dpi);
        }
        if let Some(stack) = self.font_stack.as_ref() {
            if let Ok(m) = stack.cell_metrics_raster_px() {
                self.cell_w = m.cell_w as f32;
                let natural = m.cell_h as f32;
                self.cell_h = natural * self.line_height_mult.max(0.01);
                self.line_height = self.cell_h;
            }
        }
        self.row_glyph_cache.invalidate_all();
        self.line_quad_cache.invalidate_all();
        // Every stack rescaled in place, so kept titles and runs hold old-size glyphs.
        self.chrome_caches.clear_runs();
        // The GPU-side AtlasUpload owns a texture sized to the old atlas
        // dimensions and a bind group pointing at it. After replacing the
        // CPU `GlyphAtlas` with one of a different size, the next
        // `glyph_upload.sync(...)` would either write out-of-bounds or
        // sample tiles at stale UVs. Rebuild the upload so its texture +
        // bind group match the new atlas dimensions exactly.
        self.rebuild_glyph_upload_if_needed();
        self.last_frame_key = None;
        self.last_pane_layout.clear();
        // Field geometry was measured at the previous DPI.
        self.presented_fields.clear();
        self.request_window_redraw();
        tracing::info!(
            "renderer.rebuild_for_sf: sf={sf} atlas={}x{} raster_px={}",
            self.glyph_atlas.width(),
            self.glyph_atlas.height(),
            self.raster_px(self.font_size),
        );
    }
}

impl GpuRenderer {
    /// Apply a new color theme without reconstructing the renderer.
    /// Recomputes every cached wgpu / glyphon color derived from the
    /// theme so the next frame reflects the swap.
    pub fn set_theme(&mut self, theme: &Theme) {
        self.set_theme_with_opacity(theme, self.bg_opacity);
    }
}

impl GpuRenderer {
    /// Apply a new color theme and terminal background opacity.
    pub fn set_theme_with_opacity(&mut self, theme: &Theme, opacity: f32) {
        self.bg_opacity = opacity.clamp(0.0, 1.0);
        self.bg = hex_to_wgpu_with_alpha(theme.colors.background.0.as_str(), self.bg_opacity);
        self.fg_default = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        self.cursor_color = cursor_color_from_theme(theme);
        self.bg_rgba = hex_to_premultiplied_rgba(theme.colors.background.0.as_str(), 1.0);
        self.cursor_text_color = cursor_text_color_from_theme(theme);
        self.selection_color = hex_to_premultiplied_rgba(theme.colors.selection_bg.0.as_str(), 0.5);
        self.tab_bar_bg = hex_to_premultiplied_rgba(theme.colors.tab.bar_bg.0.as_str(), 1.0);
        self.tab_active_bg = hex_to_premultiplied_rgba(theme.colors.tab.active_bg.0.as_str(), 1.0);
        self.tab_inactive_bg =
            hex_to_premultiplied_rgba(theme.colors.tab.inactive_bg.0.as_str(), 1.0);
        self.tab_active_fg = hex_to_chrome_color(theme.colors.tab.active_fg.0.as_str());
        self.tab_inactive_fg = hex_to_chrome_color(theme.colors.tab.inactive_fg.0.as_str());
        self.tab_separator =
            hex_to_premultiplied_rgba(theme.colors.tab.inactive_fg.0.as_str(), 0.45);
        self.hyperlink_underline = hex_to_premultiplied_rgba(theme.colors.cursor.0.as_str(), 0.9);
        self.splitter_color = splitter_color_from_theme(theme);
        let tint_alpha = match theme.appearance {
            sonicterm_render_model::boundary::cfg::theme::Appearance::Dark => {
                // When: `Appearance::Dark` — dark needs more accent before
                // the hyperlink tint reads as tinted at all.
                0.14
            }
            sonicterm_render_model::boundary::cfg::theme::Appearance::Light => {
                // When: `Appearance::Light` — 0.14 reads as a highlighter
                // stripe over the text rather than a hint beneath it.
                0.10
            }
        };
        self.hyperlink_tint = hex_to_premultiplied_rgba(theme.colors.cursor.0.as_str(), tint_alpha);
        self.search_highlight =
            hex_to_premultiplied_rgba(theme.colors.bright.yellow.0.as_str(), 0.35);
        self.search_fg = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        self.search_bg = hex_to_premultiplied_rgba(theme.colors.tab.bar_bg.0.as_str(), 0.95);
        // Refresh the kept UI palette now, so the next frame with this theme derives nothing.
        let _palette = self.chrome_caches.palette.palette_for(theme);
        self.last_frame_key = None;
        self.style_rev = self.style_rev.wrapping_add(1);
        self.row_glyph_cache.invalidate_all();
        self.line_quad_cache.invalidate_all();
        self.log_subpixel_aa_policy();
        tracing::info!("renderer.set_theme: {}", theme.name);
    }
}

impl GpuRenderer {
    /// Prepare this frame's fonts before width measurement and frame-key planning: when the body
    /// stack's fallback notice published a newer generation, invalidate every cached placeholder.
    pub fn begin_frame_fonts(&mut self) -> FrameFonts {
        let _collect = crate::frame_stats::CollectGuard::enter(self.frame_sink.as_ref());
        let current = self.font_stack.as_ref().map_or((0, 0), |stack| {
            let notice = stack.fallback_notice();
            (notice.id(), notice.generation())
        });
        frame_fonts::prepare_and_owe(
            &mut self.applied_fonts,
            &mut self.unattributed_apply,
            current,
            frame_fonts::FontApplyTargets {
                row_glyph_cache: &mut self.row_glyph_cache,
                line_quad_cache: &mut self.line_quad_cache,
                style_rev: &mut self.style_rev,
                last_frame_key: &mut self.last_frame_key,
                glyph_atlas: &mut self.glyph_atlas,
                preedit_glyph_cache: &mut self.preedit_glyph_cache,
                fallback_epoch: self.tab_title_font.fallback_epoch_mut(),
                chrome_runs: &mut self.chrome_caches.runs,
            },
        )
    }
}

impl GpuRenderer {
    /// Install the App's wake for fallback completions and attach it to the current body stack's
    /// notice. An owed completion is delivered once now; a claim already posted is left alone.
    pub fn set_font_fallback_waker(&mut self, waker: FontFallbackWaker) {
        self.fallback_waker = Some(waker);
        self.attach_fallback_waker();
    }
}

impl GpuRenderer {
    /// Attach the stored wake to the body stack's notice. The tab-title and footer stacks are
    /// clones of the body configuration and share its notice, so one attachment covers all three.
    pub(super) fn attach_fallback_waker(&self) {
        frame_fonts::attach_fallback_waker(self.font_stack.as_ref(), self.fallback_waker.as_ref());
    }
}

impl GpuRenderer {
    /// Test seam: hold this renderer's fallback worker inside `hook` while it holds the
    /// pending-handle lock, so a test controls when a found face can merge and publish. The
    /// tab-title and footer stacks share the body configuration, so one hook covers all three.
    #[doc(hidden)]
    pub fn __test_set_fallback_append_hook(&self, hook: std::sync::Arc<dyn Fn() + Send + Sync>) {
        if let Some(stack) = self.font_stack.as_ref() {
            stack.set_fallback_append_hook_for_test(hook);
        }
    }
}

impl GpuRenderer {
    /// The id of the fallback notice this renderer's body stack publishes to, if it has a stack.
    #[must_use]
    pub fn font_fallback_notice_id(&self) -> Option<u64> {
        self.font_stack.as_ref().map(|stack| stack.fallback_notice().id())
    }
}

impl GpuRenderer {
    /// The distinct codepoints this window's presented terminal rows draw as tofu that no font
    /// SonicTerm can find covers. Records whose slot no longer shows their row, or whose fonts
    /// were replaced, are dropped first; the unresolved set is read only when its revision moved.
    pub fn presented_missing_glyphs(
        &mut self,
        ignore_private_use: bool,
    ) -> crate::missing_glyphs::MissingGlyphs {
        let Some(stack) = self.font_stack.as_ref() else {
            // When: font_stack is None, the renderer shapes no terminal text.
            return Default::default();
        };
        let cache = &self.row_glyph_cache;
        crate::missing_glyphs::prune_missing(
            &mut self.presented_missing,
            |pane_id, slot| cache.committed_slot(pane_id, slot),
            self.applied_fonts,
        );
        if self.presented_missing.is_empty() {
            // When: presented_missing is empty, no presented row drew tofu.
            return Default::default();
        }
        let notice = stack.fallback_notice();
        let revision = notice.unresolved_revision();
        let fresh = self
            .unresolved_snapshot
            .as_ref()
            .is_some_and(|(id, snapshot)| *id == notice.id() && snapshot.revision == revision);
        if !fresh {
            self.unresolved_snapshot = Some((notice.id(), notice.unresolved_snapshot()));
        }
        let unresolved = &self.unresolved_snapshot.as_ref().expect("refreshed above").1;
        crate::missing_glyphs::count_missing(
            self.presented_missing.values(),
            unresolved,
            ignore_private_use,
        )
    }
}

impl GpuRenderer {
    /// Handle a delivered fallback wake for `notice_id`: acknowledge it when it is this
    /// renderer's current notice, and return whether a frame must apply a new face generation and
    /// whether only the unresolved set moved. An event for an older notice touches nothing.
    pub fn acknowledge_font_fallback(
        &mut self,
        notice_id: u64,
    ) -> crate::missing_glyphs::FallbackWake {
        let faces_due = frame_fonts::acknowledge_fallback_wake(
            self.font_stack.as_ref(),
            self.applied_fonts,
            notice_id,
        );
        let unresolved_changed = frame_fonts::unresolved_moved(
            self.font_stack.as_ref(),
            notice_id,
            &mut self.acknowledged_unresolved,
        );
        crate::missing_glyphs::FallbackWake { faces_due, unresolved_changed }
    }
}

impl GpuRenderer {
    /// A frame's measurement and drawing take the token their preparation returned; neither reads
    /// the generation, so they act on exactly what `begin_frame_fonts` applied.
    pub(super) fn debug_assert_prepared(&self, fonts: &FrameFonts) {
        debug_assert_eq!(
            self.applied_fonts,
            Some((fonts.notice_id(), fonts.generation())),
            "a frame must take the token of this renderer's latest begin_frame_fonts"
        );
    }
}

impl GpuRenderer {
    /// Test seam: keep (`true`, the default) or drop the frame scratch and the kept chrome titles
    /// and runs between frames, so a test can compare reuse against cold assembly.
    #[doc(hidden)]
    pub fn __set_frame_reuse(&mut self, reuse: bool) {
        self.frame_scratch.set_reuse(reuse);
        self.chrome_reuse = reuse;
        if !reuse {
            // Reuse is turned off, so kept titles and runs are dropped at once.
            self.chrome_caches.clear_runs();
        }
    }
}

impl GpuRenderer {
    /// Test seam: how many times the UI palette was derived after construction.
    #[doc(hidden)]
    #[must_use]
    pub fn __palette_computes(&self) -> u64 {
        self.chrome_caches.palette.computes()
    }
}

impl GpuRenderer {
    /// Invalidate row glyphs, line quads, kept chrome titles and runs, and the frame key, bumping
    /// `style_rev` so the next frame reshapes text.
    pub fn clear_shape_cache(&mut self) {
        self.row_glyph_cache.invalidate_all();
        self.line_quad_cache.invalidate_all();
        self.chrome_caches.clear_runs();
        self.style_rev = self.style_rev.wrapping_add(1);
        self.last_frame_key = None;
        tracing::info!(
            "renderer.clear_shape_cache (async fallback notifier) style_rev={}",
            self.style_rev
        );
    }
}

impl GpuRenderer {
    /// Test/diagnostic peek at the renderer's monotonic style
    /// revision. The counter is opaque; tests only care that it
    /// *changes* on theme / `clear_shape_cache` calls.
    #[doc(hidden)]
    #[must_use]
    pub fn style_rev(&self) -> u64 {
        self.style_rev
    }
}

impl GpuRenderer {
    /// Record a compatibility attachment without starting or changing font fallback work.
    pub fn set_async_loader(&mut self, _loader: ()) {
        self.async_loader = Some(());
    }
}

impl GpuRenderer {
    /// Observe the compatibility attachment; `None` until the setter records `Some(())`.
    #[doc(hidden)]
    #[must_use]
    pub fn async_loader(&self) -> Option<&()> {
        self.async_loader.as_ref()
    }
}

#[cfg(test)]
#[path = "core_font_tests.rs"]
mod core_font_tests;
