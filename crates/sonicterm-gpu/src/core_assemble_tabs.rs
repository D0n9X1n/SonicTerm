//! Assemble tabs in the existing painter order, with no device work.

use super::*;

impl GpuRenderer {
    /// Emit this phase into the borrowed frame scratch; the caller owns settlement and retries.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn assemble_tabs(
        &mut self,
        geometry: AssemblyGeometry<'_>,
        inputs: AssemblyUi<'_>,
        scratch: &mut frame_scratch::FrameScratch,
    ) -> crate::cursor::RecolorBounds {
        let AssemblyGeometry { surface_width_px: sw, surface_height_px: sh, .. } = geometry;
        let AssemblyUi { theme, tabs, process_privileged, hover_tab_idx, tab_now, .. } = inputs;
        let frame_scratch::FrameScratch { glyphs: glyph_instances, quads, .. } = scratch;
        // Title glyphs can reach above the padded band, so their ink is measured from here.
        let tab_glyph_start = glyph_instances.len();
        if self.tab_bar_visible {
            // When: `self.tab_bar_visible` — a hidden bar reserves no height,
            // so its strip, tab quads, and titles are all skipped.
            let insertion_slot = self.drag_chip.as_ref().and_then(|c| c.insertion_slot);
            let source_tab_idx = self.drag_chip.as_ref().and_then(|c| c.source_tab_idx);
            let source_alpha = self.drag_chip.as_ref().map(|c| c.source_alpha).unwrap_or(1.0);
            let layout = TabBarLayout::compute_with_insertion_slot(
                tabs,
                sw,
                self.tab_bar_logical_height(),
                insertion_slot,
            )
            .with_top_offset(self.tab_bar_y_offset());
            // Round 3 — premium browser-style chrome.
            // The structural colors come from `ui_tokens`, decoupled from
            // the terminal palette so every theme renders the same modern
            // tab bar. The theme.tab.* colors remain authoritative for
            // the title text (active vs inactive fg) so per-theme accents
            // still read through.
            let ui_palette = self.chrome_caches.palette.palette_for(theme);
            // `tok::BG_BASE` is a hardcoded near-black
            // (`#0B0E14`) that is indistinguishable from most dark
            // themes' `theme.background` — the tab bar drew correctly
            // (diagnostic confirmed 6 quads pinned at NDC
            // bottom with alpha 1.0) but the bar bg was the *same
            // pixel value* as the cell-grid bg, so it disappeared.
            // Switch to `ui_palette.bg_base` which is theme-derived
            // (`theme.background` shifted -8% lightness) so every
            // theme gets visible contrast automatically.
            let bar_bg = ui_palette.bg_base;
            // Theme-driven accent (was hardcoded ACCENT_BLUE — broke gruvbox/etc.).
            let accent_blue = ui_palette.accent;
            let separator = ui_palette.border_subtle;
            emit_tab_bar_quads(
                &mut *quads,
                &layout,
                &TabBarQuadParams {
                    accent: accent_blue,
                    separator,
                    border: bar_bg,
                    hover_tab_idx,
                    surface: (sw, sh),
                    active_panel_marker_alpha: if self.window_focused {
                        ACTIVE_PANEL_MARKER_ALPHA_FOCUSED
                    } else {
                        ACTIVE_PANEL_MARKER_ALPHA_UNFOCUSED
                    },
                },
            );
            for t in &layout.tabs {
                // If this tab is the source of
                // a live drag, overlay a translucent bar-bg quad to
                // dim it to roughly `source_alpha` perceived opacity.
                // The quad is painted AFTER the tab body + close icon
                // so it dims everything in the tab's footprint.
                if source_tab_idx == Some(t.idx) {
                    let dim = ((1.0 - source_alpha.clamp(0.0, 1.0)) * 0.45).clamp(0.0, 1.0);
                    let overlay = with_premultiplied_alpha(bar_bg, dim);
                    quads.push(QuadInstance {
                        rect: px_to_ndc(t.bg_rect.x, t.bg_rect.y, t.bg_rect.w, t.bg_rect.h, sw, sh),
                        color: overlay,
                        ..Default::default()
                    });
                }
            }

            // Tab titles are laid out per-tab so each run can be centered
            // by its measured glyph width instead of approximating with
            // column-padding spaces across one long synthetic string.
            let bar_h = self.tab_bar_logical_height();
            let bar_y = self.tab_bar_y_offset();
            let tab_raster_px = self.tab_title_font.raster_px();
            // Center the title using tab_raster_px in the same raster-pixel space as bar_h and bar_y.
            let title_top = bar_y + ((bar_h - tab_raster_px * 1.2) / 2.0).max(0.0);
            let tab_baseline_y = title_top + tab_raster_px * 0.95;
            let mut tab_rasterizer = self.tab_title_font.stack().cloned();
            // Kept titles are keyed by tab position; a closed tab's slot is dropped.
            let title_font_key = self.tab_title_font.key();
            let title_fallback_epoch = self.tab_title_font.fallback_epoch();
            self.chrome_caches.titles.retain_tabs(layout.tabs.len());
            for (position, t) in layout.tabs.iter().enumerate() {
                let Some(tab) = tabs.tabs().get(t.idx) else {
                    // When: `tabs.tabs().get(t.idx)` is None — the layout
                    // outlived a closed tab, so there is no title to draw.
                    continue;
                };
                let active = layout.active == Some(t.idx);
                let active_panel_focused = active && self.window_focused;
                let hovered = hover_tab_idx == t.idx as u32;
                // The content the tab was measured with, so the drawn text matches its width.
                let content = TabContent::of(tab, tab_now, active, process_privileged);
                let show_privilege_badge = content.privileged;
                let text_px = t.title_rect.w
                    - privilege_marker_reserve_px(show_privilege_badge, self.scale_factor);
                let badge_alpha = if source_tab_idx == Some(t.idx) { source_alpha } else { 1.0 };
                if let (Some(stack), Some(rasterizer)) =
                    (self.tab_title_font.stack(), tab_rasterizer.as_mut())
                {
                    let mut color = tab_title_color(
                        tab.custom_color.as_deref(),
                        active,
                        hovered,
                        active_panel_focused,
                        self.tab_active_fg,
                        self.tab_inactive_fg,
                    );
                    if source_tab_idx == Some(t.idx) {
                        color = scale_chrome_text_alpha(color, source_alpha);
                    }
                    let display = content.display_text();
                    let probe = crate::chrome_cache::TitleProbe {
                        text: &display,
                        font_key: title_font_key,
                        fallback_epoch: title_fallback_epoch,
                        raster_px: tab_raster_px,
                        text_px,
                    };
                    // A warm, unchanged title draws from its kept run with no shaping.
                    let draw = self.chrome_caches.titles.prepare(
                        position,
                        &probe,
                        stack,
                        self.chrome_reuse,
                    );
                    let title = self.chrome_caches.titles.drawn(&draw);
                    let placement = tab_title_block_placement(
                        t.title_rect,
                        title.width_px,
                        show_privilege_badge,
                        self.scale_factor,
                    );
                    if let Some(badge) = placement.badge_rect {
                        emit_privilege_badge_quads(
                            &mut *quads,
                            badge,
                            ui_palette.danger,
                            badge_alpha,
                            (sw, sh),
                        );
                    }
                    let title_clip = Some(ChromeClip {
                        x: placement.text_clip.x,
                        y: placement.text_clip.y,
                        w: placement.text_clip.w,
                        h: placement.text_clip.h,
                    });
                    let title_origin = (placement.text_x, tab_baseline_y);
                    let final_layout = crate::chrome_cache::layout_title(
                        stack,
                        rasterizer,
                        &mut self.glyph_atlas,
                        &title,
                        crate::chrome_cache::TitlePlacement {
                            color,
                            raster_px: tab_raster_px,
                            origin: title_origin,
                            screen: (sw, sh),
                            clip: title_clip,
                        },
                    );
                    glyph_instances.extend(final_layout.glyphs);
                    quads.extend(final_layout.missing_boxes);
                } else if show_privilege_badge {
                    // When: `show_privilege_badge` is true without a title font stack, paint the vector warning alone.
                    let placement =
                        tab_title_block_placement(t.title_rect, 0.0, true, self.scale_factor);
                    if let Some(badge) = placement.badge_rect {
                        emit_privilege_badge_quads(
                            &mut *quads,
                            badge,
                            ui_palette.danger,
                            badge_alpha,
                            (sw, sh),
                        );
                    }
                }
            }
        }
        crate::cursor::glyph_ink_bounds(&glyph_instances[tab_glyph_start..], sw, sh)
    }
}
