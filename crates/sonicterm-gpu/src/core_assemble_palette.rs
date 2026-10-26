//! Assemble palette in the existing painter order, with no device work.

use super::*;

impl GpuRenderer {
    /// Emit this phase into the borrowed frame scratch; the caller owns settlement and retries.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn assemble_palette(
        &mut self,
        geometry: AssemblyGeometry<'_>,
        inputs: AssemblyUi<'_>,
        scratch: &mut frame_scratch::FrameScratch,
        palette: Option<&mut CommandPalette>,
        field_environment: u64,
        field_candidates: &mut PresentedFields,
        field_selection_bg: [f32; 4],
        field_selection_fg: [f32; 4],
    ) -> Option<PaletteLayout> {
        let AssemblyGeometry { surface_width_px: sw, surface_height_px: sh, .. } = geometry;
        let AssemblyUi { theme, ime, .. } = inputs;
        let frame_scratch::FrameScratch {
            overlay_glyphs: overlay_glyph_instances,
            overlay_quads: quads_overlay,
            ..
        } = scratch;
        // -------- Command palette overlay ----------------------------------
        let palette_preedit = ime.map(|i| i.preedit()).unwrap_or("");
        let (palette_layout, palette_field_text) = if let Some(p) = palette {
            // The query text and caret come from the field display, so a literal bar glyph stays text.
            let field_text = FieldText::palette(p, palette_preedit);
            let layout = PaletteLayout::compute(p, sw, sh, self.panel_padding, self.scale_factor);
            (layout, field_text)
        } else {
            // When: `palette` is None — the command palette is closed, so no
            // layout, query, or caret exists for the overlay pass to draw.
            (None, None)
        };
        // Chrome colors are derived from the active theme so the palette
        // tracks the user's chosen palette instead of hardcoded
        // Tokyo Night literals (see UiPalette::from_theme).
        if let Some(layout) = &palette_layout {
            // When: `palette_layout` is Some — the palette is open, so its
            // panel, query row, and result rows all need chrome this frame.
            let palette_chrome = self.chrome_caches.palette.palette_for(theme);
            let accent_rgba = palette_chrome.accent;
            // Full-window scrim — sits below the modal so the underlying
            // terminal recedes visually.
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(
                    layout.scrim.x,
                    layout.scrim.y,
                    layout.scrim.w,
                    layout.scrim.h,
                    sw,
                    sh,
                ),
                color: palette_chrome.scrim,
                ..Default::default()
            });
            // Outer 1px border. Rounded radius 16 per spec — the border
            // sits 1px outside `bg`, so its radius equals the panel's
            // plus the border thickness.
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(
                    layout.border.x,
                    layout.border.y,
                    layout.border.w,
                    layout.border.h,
                    sw,
                    sh,
                ),
                color: palette_chrome.border_subtle,
                size_px: [layout.border.w, layout.border.h],
                radius_px: PALETTE_PANEL_RADIUS + PALETTE_BORDER,
                ..Default::default()
            });
            // Modal background. Rounded radius 16 per spec.
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(layout.bg.x, layout.bg.y, layout.bg.w, layout.bg.h, sw, sh),
                color: palette_chrome.bg_elevated,
                size_px: [layout.bg.w, layout.bg.h],
                radius_px: PALETTE_PANEL_RADIUS,
                ..Default::default()
            });
            // Query field background. Slightly smaller radius than the
            // panel reads as nested chrome.
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(
                    layout.query_row.x,
                    layout.query_row.y,
                    layout.query_row.w,
                    layout.query_row.h,
                    sw,
                    sh,
                ),
                color: palette_chrome.bg_base,
                size_px: [layout.query_row.w, layout.query_row.h],
                radius_px: PALETTE_QUERY_RADIUS,
                ..Default::default()
            });
            // Selected row highlight — theme accent at low alpha.
            if let Some(sel) = layout.selected_row {
                if let Some(row) = layout.rows.get(sel) {
                    quads_overlay.push(QuadInstance {
                        rect: px_to_ndc(row.rect.x, row.rect.y, row.rect.w, row.rect.h, sw, sh),
                        color: with_premultiplied_alpha(accent_rgba, 0.16),
                        size_px: [row.rect.w, row.rect.h],
                        radius_px: PALETTE_ROW_RADIUS,
                        ..Default::default()
                    });
                }
            }
            // Footer top border — 1px line at the top edge of the footer
            // rect. Kept sharp; a 1px hairline doesn't benefit from
            // SDF rounding.
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(layout.footer.x, layout.footer.y, layout.footer.w, 1.0, sw, sh),
                color: palette_chrome.border_subtle,
                ..Default::default()
            });
            // Shape the query row text. The renderer paints either the
            // placeholder (empty query) or the typed text + cursor.
            //
            // emit through the SonicTerm glyph atlas at device pixel
            // scale (mirrors `emit_tab_title_glyphs`) so the palette text
            // is crisp on HiDPI. The previous glyphon TextRenderer path
            // bypassed the DPI multiplier and rendered blurry on Windows.
            // An editable query paints its field display. An empty query paints the
            // placeholder and the colour picker paints its title; neither is selectable.
            let (paint_text, paint_caret, paint_selection) = match &palette_field_text {
                Some(text) if !text.label.is_empty() => {
                    (text.label.clone(), text.caret, text.selection.clone())
                }
                Some(_) => (layout.query_placeholder.clone().unwrap_or_default(), 0, None),
                None => (layout.query_label.clone(), layout.query_label.len(), None),
            };
            let title_text;
            let plan_text: &FieldText = if let Some(text) = palette_field_text.as_ref() {
                text
            } else {
                // When: `palette_field_text` is None — the colour picker title gets a
                // display-only record that places its end caret.
                title_text = FieldText {
                    kind: FieldKind::Palette,
                    label: paint_text.clone(),
                    content: 0..0,
                    caret: paint_text.len(),
                    selection: None,
                    composing: false,
                    variant: u8::MAX,
                };
                &title_text
            };
            let palette_font_size = self.raster_px(self.font_size);
            // Chrome text needs a wezterm FontStack; when one
            // isn't available (test fixtures), the palette quads still
            // render but no text is emitted. Wrap the entire chrome
            // emission in an `if let Some(...)` so the palette path
            // degrades gracefully instead of panicking.
            if let Some(stack) = self.font_stack.as_ref() {
                // When: `font_stack` is Some — palette quads are already
                // pushed; without a shaper the panel draws with no text.
                let palette_native_em = self.raster_px(self.font_size);
                let mut palette_rasterizer = stack.clone();
                // Query: vertically centre inside the query_row chrome.
                let query_origin_x = layout.query_row.x
                    + self.chrome_px(
                        sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_PAD_X,
                    );
                let query_baseline_y =
                    layout.query_row.y + (layout.query_row.h + palette_font_size * 0.8) * 0.5;
                let caret_h =
                    (palette_font_size * 1.15).min(layout.query_row.h - self.chrome_px(8.0));
                let caret_y = layout.query_row.y + (layout.query_row.h - caret_h) * 0.5;
                let query_pad_x = query_origin_x - layout.query_row.x;
                let query_clip = FieldRect {
                    x: query_origin_x,
                    y: layout.query_row.y,
                    w: (layout.query_row.w - query_pad_x * 2.0).max(0.0),
                    h: layout.query_row.h,
                };
                let query_row_rect = FieldRect {
                    x: layout.query_row.x,
                    y: layout.query_row.y,
                    w: layout.query_row.w,
                    h: layout.query_row.h,
                };
                let palette_placement = FieldPlacement {
                    kind: FieldKind::Palette,
                    clip: query_clip,
                    hit_area: query_row_rect,
                    caret_y,
                    caret_h,
                    caret_fallback_w: (self.cell_w * 0.70).max(4.0),
                    font_size_px: palette_font_size,
                    native_em_px: palette_native_em,
                };
                // One shaped run feeds caret, highlight, scroll, and the painted glyphs.
                let palette_run = chrome_text::ChromeShapedRun::shape(
                    stack,
                    &paint_text,
                    ChromeAttrs::default(),
                    palette_font_size,
                    palette_native_em,
                );
                if palette_run.is_none() {
                    // An unshaped query paints nothing, so every visible character is missing.
                    chrome_text::note_unshaped_chrome(&paint_text);
                }
                let palette_field = palette_run.as_ref().map(|run| {
                    plan_field(
                        palette_placement,
                        &FieldBoundaries::from_run(run),
                        plan_text,
                        paint_caret,
                        paint_selection,
                        field_environment,
                        self.presented_fields.palette.as_ref(),
                    )
                });
                if palette_field_text.is_some() {
                    // Only an editable query's presented geometry serves pointer and IME.
                    field_candidates.palette = palette_field;
                }
                let query_start = overlay_glyph_instances.len();
                // Tofu outlines of the query; drawn after the selection and caret quads.
                let mut query_tofu: Vec<QuadInstance> = Vec::new();
                if let (Some(run), Some(field)) = (palette_run.as_ref(), palette_field) {
                    // The query paints from the run its field geometry was measured on.
                    let query_layout = chrome_text::layout_prepared(
                        run,
                        &mut palette_rasterizer,
                        &mut self.glyph_atlas,
                        self.search_fg,
                        (field.text_x, query_baseline_y),
                        (sw, sh),
                        Some(ChromeClip {
                            x: query_clip.x,
                            y: query_clip.y,
                            w: query_clip.w,
                            h: query_clip.h,
                        }),
                        GlyphRasterVariant::Normal,
                    );
                    overlay_glyph_instances.extend(query_layout.glyphs);
                    query_tofu = query_layout.missing_boxes;
                }
                // Layout only culls whole glyphs; a scrolled glyph crossing the edge is trimmed here.
                clip_glyphs_to_rect(&mut *overlay_glyph_instances, query_start, query_clip, sw, sh);
                if let Some(field) = palette_field {
                    // The run shaped, so caret and highlight use its measured clusters.
                    let mut marks = Vec::new();
                    if let Some(highlight) = field.selection {
                        // A selected range paints under the glyphs with selection colors.
                        marks.push(crate::cursor::FieldMark {
                            rect: (highlight.x, highlight.y, highlight.w, highlight.h),
                            background: field_selection_bg,
                            foreground: field_selection_fg,
                        });
                    }
                    let caret = field.caret;
                    if caret.w > 0.0 && caret.h > 0.0 {
                        // A query row too small to show any caret clips it to zero area,
                        // and then no block is drawn.
                        marks.push(crate::cursor::FieldMark {
                            rect: (caret.x, caret.y, caret.w, caret.h),
                            background: self.cursor_color,
                            foreground: self.cursor_text_color,
                        });
                    }
                    crate::cursor::paint_field_marks(
                        &mut *quads_overlay,
                        &mut overlay_glyph_instances[query_start..],
                        std::mem::take(&mut query_tofu),
                        &marks,
                        sw,
                        sh,
                    );
                }
                // A query without a field geometry has no marks; its tofu, if any, still draws.
                quads_overlay.extend(query_tofu);

                // Rows: emit each visible row label as its own line so the
                // baseline aligns with the row's highlight quad.
                let bounds_bg = [layout.bg.x, layout.bg.y, layout.bg.w, layout.bg.h];
                for (i, label) in layout.row_labels.iter().enumerate() {
                    let Some(row) = layout.rows.get(i) else {
                        // When: `layout.rows.get(i)` is None — labels and rects
                        // are parallel vectors that can disagree in length.
                        continue;
                    };
                    let shortcut = layout.row_shortcuts.get(i).and_then(|hint| hint.as_deref());
                    let detail = layout.row_details.get(i).and_then(|text| text.as_deref());
                    let disabled = layout.row_disabled.get(i).copied().unwrap_or(false);
                    let mut label_color = self.search_fg;
                    if disabled {
                        label_color.a = 150;
                    }
                    let swatch = layout.row_swatches.get(i).and_then(|v| v.as_deref());
                    let shortcut_font_size = palette_font_size;
                    let shortcut_w = shortcut
                        .map(|hint| hint.chars().count() as f32 * shortcut_font_size * 0.62);
                    let mut origin_x = row.rect.x
                        + self.chrome_px(
                            sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_PAD_X,
                        );
                    if let Some(hex) = swatch {
                        let color = hex_to_premultiplied_rgba(hex, 1.0);
                        let line_h = self.chrome_px(2.0).max(1.0);
                        quads_overlay.push(QuadInstance::sharp(
                            px_to_ndc(row.rect.x, row.rect.y, row.rect.w, line_h, sw, sh),
                            color,
                        ));
                        let size = (row.rect.h * 0.55).max(8.0);
                        let swatch_x = origin_x;
                        let swatch_y = row.rect.y + (row.rect.h - size) * 0.5;
                        quads_overlay.push(QuadInstance::rounded(
                            px_to_ndc(swatch_x, swatch_y, size, size, sw, sh),
                            color,
                            [size, size],
                            size * 0.25,
                        ));
                        origin_x += size + self.chrome_px(8.0);
                    }
                    // Center the combined text block in raster pixels so both outer margins scale together.
                    let detail_font_size = self.raster_px(palette_footer_font_size(self.font_size));
                    let (label_baseline, detail_top, detail_baseline) = palette_detail_positions(
                        row.rect.h,
                        palette_font_size,
                        detail_font_size,
                        self.chrome_px(4.0),
                    );
                    let baseline_y = row.rect.y
                        + if detail.is_some() {
                            label_baseline
                        } else {
                            // When: detail is absent, retain the single-line row baseline.
                            (row.rect.h + palette_font_size * 0.8) * 0.5
                        };
                    let label_clip_h = if detail.is_some() {
                        detail_top.min(row.rect.h)
                    } else {
                        // When: detail is absent, the label owns the full row clip.
                        row.rect.h
                    };
                    let label_bounds_w = match shortcut_w {
                        Some(w) => (row.rect.w
                            - w
                            - self.chrome_px(sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_PAD_X) * 2.0
                            - self.chrome_px(sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_COLUMN_GAP))
                        .max(0.0),
                        None => row.rect.w,
                    };
                    quads_overlay.extend(emit_overlay_text_glyphs(
                        &mut self.glyph_atlas,
                        stack,
                        palette_font_size,
                        palette_native_em,
                        &mut palette_rasterizer,
                        label,
                        label_color,
                        ChromeAttrs::default(),
                        origin_x,
                        baseline_y,
                        [row.rect.x, row.rect.y, label_bounds_w, label_clip_h],
                        sw,
                        sh,
                        &mut *overlay_glyph_instances,
                        None,
                    ));
                    if let (Some(hint), Some(width)) = (shortcut, shortcut_w) {
                        let hint_origin_x = row.rect.x + row.rect.w
                            - self.chrome_px(
                                sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_PAD_X,
                            )
                            - width;
                        let mut hint_color = self.search_fg;
                        hint_color.a = if disabled { 120 } else { 165 };
                        quads_overlay.extend(emit_overlay_text_glyphs(
                            &mut self.glyph_atlas,
                            stack,
                            shortcut_font_size,
                            palette_native_em,
                            &mut palette_rasterizer,
                            hint,
                            hint_color,
                            ChromeAttrs { bold: false, italic: true },
                            hint_origin_x,
                            baseline_y,
                            [row.rect.x, row.rect.y, row.rect.w, row.rect.h],
                            sw,
                            sh,
                            &mut *overlay_glyph_instances,
                            None,
                        ));
                    }
                    if let (Some(detail), Some(detail_stack)) =
                        (detail, self.palette_footer_font_stack.as_ref())
                    {
                        // Details share the footer's smaller native strike and atlas identity.
                        let mut detail_rasterizer = detail_stack.clone();
                        let mut detail_color = self.search_fg;
                        detail_color.a = if disabled { 120 } else { 165 };
                        let detail_width = (row.rect.x + row.rect.w
                            - origin_x
                            - self.chrome_px(
                                sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_PAD_X,
                            ))
                        .max(0.0);
                        let detail_layout = chrome_text::layout_with_raster_variant(
                            detail_stack,
                            &mut detail_rasterizer,
                            &mut self.glyph_atlas,
                            detail,
                            detail_color,
                            ChromeAttrs::default(),
                            detail_font_size,
                            detail_font_size,
                            (origin_x, row.rect.y + detail_baseline),
                            (sw, sh),
                            Some(ChromeClip {
                                x: origin_x,
                                y: row.rect.y + detail_top,
                                w: detail_width,
                                h: (row.rect.h - detail_top).max(0.0),
                            }),
                            GlyphRasterVariant::PaletteFooter,
                        );
                        overlay_glyph_instances.extend(detail_layout.glyphs);
                        quads_overlay.extend(detail_layout.missing_boxes);
                    }
                }
                // Empty-state placeholder + hint.
                if let Some(ph) = &layout.empty_label {
                    let empty_x = layout.bg.x
                        + self.chrome_px(self.panel_padding)
                        + self.chrome_px(
                            sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_PAD_X,
                        );
                    let empty_y_top = layout.query_row.y
                        + layout.query_row.h
                        + self.chrome_px(self.panel_padding);
                    // No row rect here (empty state), so derive the scaled row
                    // height via chrome_px — same DPI basis as the row path. #palette
                    let empty_row_h = self.chrome_px(
                        sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_HEIGHT,
                    );
                    let empty_baseline_y =
                        empty_y_top + (empty_row_h + palette_font_size * 0.8) * 0.5;
                    quads_overlay.extend(emit_overlay_text_glyphs(
                        &mut self.glyph_atlas,
                        stack,
                        palette_font_size,
                        palette_native_em,
                        &mut palette_rasterizer,
                        ph,
                        self.search_fg,
                        ChromeAttrs::default(),
                        empty_x,
                        empty_baseline_y,
                        bounds_bg,
                        sw,
                        sh,
                        &mut *overlay_glyph_instances,
                        None,
                    ));
                    if let Some(hint) = &layout.empty_hint {
                        let hint_baseline_y = empty_baseline_y
                            + sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_HEIGHT
                            + sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_GAP;
                        quads_overlay.extend(emit_overlay_text_glyphs(
                            &mut self.glyph_atlas,
                            stack,
                            palette_font_size,
                            palette_native_em,
                            &mut palette_rasterizer,
                            hint,
                            self.search_fg,
                            ChromeAttrs::default(),
                            empty_x,
                            hint_baseline_y,
                            bounds_bg,
                            sw,
                            sh,
                            &mut *overlay_glyph_instances,
                            None,
                        ));
                    }
                }

                if let Some(footer_stack) = self.palette_footer_font_stack.as_ref() {
                    let footer_font_size = self.raster_px(palette_footer_font_size(self.font_size));
                    let footer_native_em = footer_font_size;
                    let mut footer_rasterizer = footer_stack.clone();
                    let footer_origin_x = layout.footer.x + self.chrome_px(PALETTE_FOOTER_INSET_X);
                    let footer_baseline_y =
                        layout.footer.y + (layout.footer.h + footer_font_size * 0.8) * 0.5;
                    let mut footer_color = self.search_fg;
                    footer_color.a = 165;
                    let footer_width =
                        (layout.footer.w - self.chrome_px(PALETTE_FOOTER_INSET_X) * 2.0).max(0.0);
                    let footer_layout = chrome_text::layout_with_raster_variant(
                        footer_stack,
                        &mut footer_rasterizer,
                        &mut self.glyph_atlas,
                        &layout.footer_label,
                        footer_color,
                        ChromeAttrs::default(),
                        footer_font_size,
                        footer_native_em,
                        (footer_origin_x, footer_baseline_y),
                        (sw, sh),
                        Some(ChromeClip {
                            x: footer_origin_x,
                            y: layout.footer.y,
                            w: footer_width,
                            h: layout.footer.h,
                        }),
                        GlyphRasterVariant::PaletteFooter,
                    );
                    overlay_glyph_instances.extend(footer_layout.glyphs);
                    quads_overlay.extend(footer_layout.missing_boxes);
                }
            }
        }
        palette_layout
    }
}
