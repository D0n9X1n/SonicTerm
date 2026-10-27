//! Assemble search in the existing painter order, with no device work.

use super::*;

impl GpuRenderer {
    /// Emit this phase into the borrowed frame scratch; the caller owns settlement and retries.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn assemble_search(
        &mut self,
        geometry: AssemblyGeometry<'_>,
        inputs: AssemblyUi<'_>,
        scratch: &mut frame_scratch::FrameScratch,
        active_snapped_cell_x: &[f32],
    ) -> AssemblyFields {
        let AssemblyGeometry {
            grid,
            plan,
            active_origin_x,
            active_origin_y,
            active_pane_x,
            active_pane_y,
            active_pane_width_px: active_pane_w,
            active_pane_height_px: active_pane_h,
            surface_width_px: sw,
            surface_height_px: sh,
            ..
        } = geometry;
        let AssemblyUi { theme, search, ime, read_only_mode, .. } = inputs;
        let frame_scratch::FrameScratch {
            glyphs: glyph_instances,
            overlay_glyphs: overlay_glyph_instances,
            quads,
            overlay_quads: quads_overlay,
            row_spans,
            ..
        } = scratch;
        // -------- Search highlights + badge --------------------------------
        if let Some(s) = search {
            // When: `search` is Some — a search session is live, so its match
            // highlights and status badge belong on this frame.
            let cur_idx = s.current;
            let view_top_abs = plan.active_view_top_abs;
            let match_bg = hex_to_premultiplied_rgba(theme.colors.ansi.yellow.0.as_str(), 1.0);
            let match_fg = hex_to_premultiplied_rgba(theme.colors.background.0.as_str(), 1.0);
            let current_bg = hex_to_premultiplied_rgba(theme.colors.bright.green.0.as_str(), 1.0);
            let current_fg = match_fg;
            // Only walk matches whose row intersects the viewport. `matches`
            // is row-sorted, so this is a binary-search-bounded slice — per
            // frame cost is O(visible matches), not O(total matches), which
            // otherwise grows with scrollback depth. `start`
            // keeps `i` aligned with the full slice for the cur_idx compare.
            let (vis_start, vis_end) = s.visible_match_range(view_top_abs, grid.rows);
            for (i, m) in
                s.matches[vis_start..vis_end].iter().enumerate().map(|(j, m)| (vis_start + j, m))
            {
                if u64::from(m.row) < view_top_abs || m.col_end <= m.col_start {
                    // When: the match row is above the viewport, or the span is
                    // empty — neither yields a highlight quad with area.
                    continue;
                }
                let visible_row = u64::from(m.row) - view_top_abs;
                if visible_row >= u64::from(grid.rows) {
                    // When: `visible_row >= grid.rows` — the binary-searched
                    // slice can include a row just past the last visible one.
                    continue;
                }
                // derive x/w from the active-pane snapped-edge
                // cache so match highlights share device-pixel edges
                // with adjacent glyph cells at fractional DPI.
                let cache_end =
                    (m.col_end as usize).min(active_snapped_cell_x.len().saturating_sub(1));
                let cs = (m.col_start as usize).min(cache_end);
                let x = active_snapped_cell_x
                    .get(cs)
                    .copied()
                    .unwrap_or(active_origin_x + f32::from(m.col_start) * self.cell_w);
                let y = active_origin_y + (visible_row as f32) * self.cell_h;
                let w = active_snapped_cell_x
                    .get(cache_end)
                    .map(|r| r - x)
                    .unwrap_or_else(|| f32::from(m.col_end - m.col_start) * self.cell_w);
                let (bg_color, fg_color) = if Some(i) == cur_idx {
                    (current_bg, current_fg)
                } else {
                    // When: `Some(i) != cur_idx` — one of the other matches,
                    // which stays yellow so the current one reads as selected.
                    (match_bg, match_fg)
                };
                // Clip the match highlight to the active pane — a long
                // match that runs past the pane's
                // last column would otherwise paint into the neighbour.
                if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                    (x, y, w, self.cell_h),
                    active_pane_x,
                    active_pane_y,
                    active_pane_w,
                    active_pane_h,
                ) {
                    quads.push(QuadInstance {
                        rect: px_to_ndc(qx, qy, qw, qh, sw, sh),
                        color: bg_color,
                        ..Default::default()
                    });
                    // The tab titles were appended after the rows; they lie outside every
                    // recorded row span, so this scan still examines each of them.
                    let RecolorOutcome { visited, .. } = recolor_cursor_glyphs_in(
                        &mut *glyph_instances,
                        row_spans,
                        qx,
                        qy,
                        qw,
                        qh,
                        sw,
                        sh,
                        fg_color,
                    );
                    crate::frame_stats::note_recolor_glyphs_visited(|| visited);
                }
            }
        }

        // -------- Bottom-right search bar (state-only overlay) -------------
        // This is the lightweight "N/M" badge that lives in the corner,
        // distinct from the legacy full-width status bar above. It shows
        // whenever search state exists, so the user has a persistent
        // affordance while typing.
        let read_only_badge = read_only_mode.then(|| {
            // Content width = icon + gap + "READONLY", in the badge's own
            // (DPI-scaled) font, so the badge hugs its text.
            let badge_font = self.raster_px((self.font_size + 2.0).max(1.0));
            let content_w = estimate_badge_text_width(READ_ONLY_BADGE_ICON, badge_font)
                + self.chrome_px(SEARCH_BAR_ICON_GAP)
                + estimate_badge_text_width(READ_ONLY_BADGE_LABEL, badge_font);
            read_only_badge_rect(sw, sh, self.scale_factor, content_w)
        });
        // Field geometry planned this frame; it replaces the presented record only
        // after a `Presented` outcome, so retries and failures never bless it.
        let field_environment = self.field_environment();
        let mut field_candidates = PresentedFields::default();
        let field_selection_bg =
            hex_to_premultiplied_rgba(theme.colors.selection_bg.0.as_str(), 1.0);
        let field_selection_fg =
            hex_to_premultiplied_rgba(theme.colors.selection_fg.0.as_str(), 1.0);
        let search_font_size = self.raster_px(self.font_size.max(1.0));
        // Search preedit is inserted at the query caret for display only; matches still use committed text.
        let search_preedit: &str =
            search.and(ime).map(|i| i.preedit()).filter(|s| !s.is_empty()).unwrap_or("");
        let search_label = search.map(|s| search_bar_label(s, search_preedit));
        // The overlay's icon and label are looked up in the chrome-run cache: a warm overlay with
        // an unchanged label measures and draws them without shaping.
        let search_run_key = crate::chrome_cache::ChromeRunKey::new(
            crate::chrome_cache::ChromeStack::Body,
            ChromeAttrs::default(),
            search_font_size,
            search_font_size,
        );
        let chrome_reuse = self.chrome_reuse;
        let search_gap_px = self.chrome_px(SEARCH_BAR_ICON_GAP);
        let search_content_width = search_label.as_ref().map(|label| {
            let runs = &mut self.chrome_caches.runs;
            let mut measure = self.font_stack.as_ref().map(|stack| {
                move |text: &str| {
                    let handle = runs.prepare(stack, text, search_run_key, chrome_reuse);
                    runs.view(&handle).map(|view| view.raw_width_px())
                }
            });
            search_badge_content_width(
                SEARCH_BADGE_ICON,
                label,
                search_font_size,
                search_gap_px,
                measure.as_mut().map(|measure| measure as &mut dyn FnMut(&str) -> Option<f32>),
            )
        });
        let search_bar_layout = search_content_width.map(|content_w| {
            if read_only_badge.is_some() {
                SearchBarLayout::compute_at_row(sw, sh, content_w, 1, self.scale_factor)
            } else {
                // When: `read_only_badge` is None — no badge occupies row 0, so
                // the search bar takes the default top row.
                SearchBarLayout::compute(sw, sh, content_w, self.scale_factor)
            }
        });
        // When search is active the inline IME preedit must anchor to the
        // current search-box caret, not the terminal cursor.
        // Populated inside the search-render block below; read by the inline
        // preedit block further down. (cx = caret_left, by = box_top, bh =
        // box_height)
        let mut search_ime_anchor: Option<(f32, f32, f32)> = None;
        if let (Some(_), Some(layout)) = (search_label.as_ref(), search_bar_layout) {
            let search_badge_bg =
                hex_to_premultiplied_rgba(theme.colors.ansi.yellow.0.as_str(), 1.0);
            let search_badge_fg = hex_to_chrome_color(theme.colors.background.0.as_str());
            quads_overlay.push(QuadInstance::rounded(
                px_to_ndc(
                    layout.border.x,
                    layout.border.y,
                    layout.border.w,
                    layout.border.h,
                    sw,
                    sh,
                ),
                search_badge_bg,
                [layout.border.w, layout.border.h],
                self.chrome_px(READ_ONLY_BADGE_RADIUS),
            ));
            // Search-badge overlay text → chrome_text into the
            // overlay glyph instance vec (sits above quad_overlay).
            if let (Some(stack), Some(search_state)) = (self.font_stack.as_ref(), search) {
                let mut wt = stack.clone();
                let icon_measure = self.chrome_caches.runs.prepare(
                    stack,
                    SEARCH_BADGE_ICON,
                    search_run_key,
                    chrome_reuse,
                );
                let icon_w = conservative_badge_text_width(
                    estimate_badge_text_width(SEARCH_BADGE_ICON, search_font_size),
                    self.chrome_caches.runs.view(&icon_measure).map(|view| view.raw_width_px()),
                );
                let icon_x = layout.border.x + self.chrome_px(SEARCH_BAR_PAD_LEFT);
                let text_x = icon_x + icon_w + self.chrome_px(SEARCH_BAR_ICON_GAP);
                let visible_w = (layout.border.x + layout.border.w
                    - self.chrome_px(SEARCH_BAR_PAD_RIGHT)
                    - text_x)
                    .max(0.0);
                let caret_h =
                    (search_font_size * 1.15).min((layout.border.h - self.chrome_px(8.0)).max(4.0));
                let caret_y = layout.border.y + (layout.border.h - caret_h) * 0.5;
                // Only the query between the prompt and the counter is selectable.
                let search_text = FieldText::search(search_state, search_preedit);
                let search_clip =
                    FieldRect { x: text_x, y: layout.border.y, w: visible_w, h: layout.border.h };
                let search_placement = FieldPlacement {
                    kind: FieldKind::Search,
                    clip: search_clip,
                    hit_area: search_clip,
                    caret_y,
                    caret_h,
                    caret_fallback_w: (self.cell_w * 0.70).max(4.0),
                    font_size_px: search_font_size,
                    native_em_px: search_font_size,
                };
                // One run, kept or shaped now, feeds caret, highlight, scroll, and the glyphs.
                let label_handle = self.chrome_caches.runs.prepare(
                    stack,
                    &search_text.label,
                    search_run_key,
                    chrome_reuse,
                );
                // The icon draws only on a drawable surface, as `chrome_text::layout` decides.
                let icon_handle = (sw > 0.0 && sh > 0.0).then(|| {
                    self.chrome_caches.runs.prepare(
                        stack,
                        SEARCH_BADGE_ICON,
                        search_run_key,
                        chrome_reuse,
                    )
                });
                let search_run = self.chrome_caches.runs.view(&label_handle);
                if search_run.is_none() {
                    // An unshaped label paints nothing, so every visible character is missing.
                    chrome_text::note_unshaped_chrome(&search_text.label);
                }
                let search_field = search_run.map(|run| {
                    plan_field(
                        search_placement,
                        &FieldBoundaries::from_view(run),
                        &search_text,
                        search_text.caret,
                        search_text.selection.clone(),
                        field_environment,
                        self.presented_fields.search.as_ref(),
                    )
                });
                field_candidates.search = search_field;
                let baseline = layout.border.y + (layout.border.h + search_font_size * 0.8) * 0.5;
                let icon_clip = Some(ChromeClip {
                    x: layout.border.x,
                    y: layout.border.y,
                    w: layout.border.w,
                    h: layout.border.h,
                });
                let icon_view =
                    icon_handle.as_ref().and_then(|handle| self.chrome_caches.runs.view(handle));
                let icon_layout = match icon_view {
                    // The icon run is kept or shaped, so it draws without shaping again.
                    Some(view) => chrome_text::layout_view(
                        view,
                        &mut wt,
                        &mut self.glyph_atlas,
                        search_badge_fg,
                        (icon_x, baseline),
                        (sw, sh),
                        icon_clip,
                        GlyphRasterVariant::Normal,
                    ),
                    // With no surface or no run, the layout decides as it always did.
                    None => chrome_text::layout(
                        stack,
                        &mut wt,
                        &mut self.glyph_atlas,
                        SEARCH_BADGE_ICON,
                        search_badge_fg,
                        ChromeAttrs::default(),
                        search_font_size,
                        search_font_size,
                        (icon_x, baseline),
                        (sw, sh),
                        icon_clip,
                    ),
                };
                overlay_glyph_instances.extend(icon_layout.glyphs);
                quads_overlay.extend(icon_layout.missing_boxes);
                let label_start = overlay_glyph_instances.len();
                // Tofu outlines of the label; drawn after the selection and caret quads.
                let mut field_tofu: Vec<QuadInstance> = Vec::new();
                if let (Some(run), Some(field)) = (search_run, search_field) {
                    // The label paints from the run its field geometry was measured on.
                    let chrome_layout = chrome_text::layout_view(
                        run,
                        &mut wt,
                        &mut self.glyph_atlas,
                        search_badge_fg,
                        (field.text_x, baseline),
                        (sw, sh),
                        Some(ChromeClip {
                            x: text_x,
                            y: layout.border.y,
                            w: visible_w,
                            h: layout.border.h,
                        }),
                        GlyphRasterVariant::Normal,
                    );
                    overlay_glyph_instances.extend(chrome_layout.glyphs);
                    field_tofu = chrome_layout.missing_boxes;
                }
                // Layout only culls whole glyphs; scrolled glyphs crossing the edge are trimmed here.
                clip_glyphs_to_rect(
                    &mut *overlay_glyph_instances,
                    label_start,
                    search_clip,
                    sw,
                    sh,
                );

                if let Some(field) = search_field {
                    // The label shaped, so caret and highlight come from its measured clusters.
                    search_ime_anchor = Some((field.caret.x, layout.border.y, layout.border.h));
                    let mut marks = Vec::new();
                    if let Some(highlight) = field.selection {
                        // A selected range uses the theme selection pair, which keeps it
                        // legible on the yellow badge and distinct from the inverted caret.
                        marks.push(crate::cursor::FieldMark {
                            rect: (highlight.x, highlight.y, highlight.w, highlight.h),
                            background: field_selection_bg,
                            foreground: field_selection_fg,
                        });
                    }
                    // The badge is already cursor-yellow, so invert locally: a
                    // theme-background block with the covered glyph recolored to
                    // badge yellow. The block overlays existing text and contributes
                    // no advance, matching terminal and command-palette cursors.
                    let caret = field.caret;
                    if caret.w > 0.0 && caret.h > 0.0 {
                        // A field too small to show any caret clips it to zero area,
                        // and then no block is drawn.
                        marks.push(crate::cursor::FieldMark {
                            rect: (caret.x, caret.y, caret.w, caret.h),
                            background: chrome_color_to_linear_rgba(search_badge_fg),
                            foreground: search_badge_bg,
                        });
                    }
                    crate::cursor::paint_field_marks(
                        &mut *quads_overlay,
                        &mut overlay_glyph_instances[label_start..],
                        std::mem::take(&mut field_tofu),
                        &marks,
                        sw,
                        sh,
                    );
                }
                // A label without a field geometry has no marks; its tofu, if any, still draws.
                quads_overlay.extend(field_tofu);
            }
        }

        if let Some((badge_x, badge_y, badge_w, badge_h)) = read_only_badge {
            // When: `read_only_badge` is Some — copy mode is read-only, so the
            // badge announces that typing will not reach the shell.
            let badge_bg = hex_to_premultiplied_rgba(theme.colors.bright.green.0.as_str(), 1.0);
            quads_overlay.push(QuadInstance::rounded(
                px_to_ndc(badge_x, badge_y, badge_w, badge_h, sw, sh),
                badge_bg,
                [badge_w, badge_h],
                self.chrome_px(READ_ONLY_BADGE_RADIUS),
            ));
            if let Some(stack) = self.font_stack.as_ref() {
                // When: `font_stack` is Some — the badge quad is already
                // pushed; without a shaper it draws as a bare rounded rect.
                let native_em = stack
                    .cell_metrics_raster_px()
                    .ok()
                    .map(|m| m.cell_h as f32)
                    .unwrap_or(self.cell_h);
                let mut wt = stack.clone();
                let font_size = self.raster_px((self.font_size + 2.0).max(1.0));
                let text_color = hex_to_chrome_color(theme.colors.background.0.as_str());
                let baseline = badge_y
                    + (badge_h + font_size * 0.8) * 0.5
                    + self.chrome_px(READ_ONLY_BADGE_BASELINE_NUDGE_Y);
                // Pre-scale chrome paddings into locals BEFORE the
                // `&mut self.glyph_atlas` borrow below, so we don't borrow
                // `*self` immutably (chrome_px) while it's mutably borrowed.
                let badge_pad_left = self.chrome_px(SEARCH_BAR_PAD_LEFT);
                let badge_pad_right = self.chrome_px(READ_ONLY_BADGE_PAD_RIGHT);
                let icon_layout = chrome_text::layout(
                    stack,
                    &mut wt,
                    &mut self.glyph_atlas,
                    READ_ONLY_BADGE_ICON,
                    text_color,
                    ChromeAttrs { bold: true, italic: false },
                    font_size,
                    native_em,
                    (badge_x + badge_pad_left, baseline),
                    (sw, sh),
                    Some(ChromeClip { x: badge_x, y: badge_y, w: badge_w, h: badge_h }),
                );
                overlay_glyph_instances.extend(icon_layout.glyphs);
                quads_overlay.extend(icon_layout.missing_boxes);
                // Place the label immediately after the lock icon (no big
                // right-aligned gap): icon_x + icon width + the icon gap.
                let label_x = badge_x
                    + badge_pad_left
                    + icon_layout.width_px
                    + self.chrome_px(SEARCH_BAR_ICON_GAP);
                let _ = badge_pad_right;
                quads_overlay.extend(emit_overlay_text_glyphs(
                    &mut self.glyph_atlas,
                    stack,
                    font_size,
                    native_em,
                    &mut wt,
                    READ_ONLY_BADGE_LABEL,
                    text_color,
                    ChromeAttrs { bold: true, italic: false },
                    label_x,
                    baseline,
                    [badge_x, badge_y, badge_w, badge_h],
                    sw,
                    sh,
                    &mut *overlay_glyph_instances,
                    None,
                ));
                quads_overlay.extend(emit_overlay_text_glyphs(
                    &mut self.glyph_atlas,
                    stack,
                    font_size,
                    native_em,
                    &mut wt,
                    READ_ONLY_BADGE_LABEL,
                    text_color,
                    ChromeAttrs { bold: true, italic: false },
                    label_x + 1.0,
                    baseline,
                    [badge_x, badge_y, badge_w, badge_h],
                    sw,
                    sh,
                    &mut *overlay_glyph_instances,
                    None,
                ));
            }
        }
        AssemblyFields {
            read_only_badge,
            search_bar_layout,
            field_environment,
            field_candidates,
            field_selection_bg,
            field_selection_fg,
            search_ime_anchor,
        }
    }
}
