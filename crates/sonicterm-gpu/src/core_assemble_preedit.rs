//! Assemble preedit in the existing painter order, with no device work.

use super::*;

impl GpuRenderer {
    /// Emit this phase into the borrowed frame scratch; the caller owns settlement and retries.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn assemble_preedit(
        &mut self,
        geometry: AssemblyGeometry<'_>,
        inputs: AssemblyUi<'_>,
        scratch: &mut frame_scratch::FrameScratch,
        active_snapped_cell_x: &[f32],
        search_ime_anchor: Option<(f32, f32, f32)>,
        palette_layout: &Option<PaletteLayout>,
    ) {
        let AssemblyGeometry {
            grid,
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
        let AssemblyUi { ime, .. } = inputs;
        let frame_scratch::FrameScratch {
            overlay_glyphs: overlay_glyph_instances,
            overlay_quads: quads_overlay,
            ..
        } = scratch;
        // Inline IME preedit at the TERMINAL CURSOR (WezTerm-style): macOS does
        // NOT draw the in-flight composition for a terminal, so the app must.
        // When SEARCH is active the preedit is instead spliced into the search
        // label (see search_bar_label above) and rendered as part of that
        // string — so we skip the self-drawn overlay here to avoid drawing it
        // twice / overlapping the ` · N/M` suffix. This block only handles the
        // terminal-cursor case. Per-frame overlay (not row-cached); the
        // FrameKey hashes `i.preedit()` so composition changes re-render.
        let search_active = search_ime_anchor.is_some();
        let palette_active = palette_layout.is_some();
        if !search_active
            && !palette_active
            && ime.map(|i| preedit_has_visible_ink(i.preedit())).unwrap_or(false)
        {
            // When: `!search_active && !palette_active` and the preedit has ink
            // — the other two anchor their own caret, leaving the cursor case.
            if let (Some(i), Some(stack)) = (ime, self.font_stack.as_ref()) {
                // When: both `ime` and `font_stack` are Some — composing text
                // needs a shaper; without one no preedit glyphs are emitted.
                let text = i.preedit();
                // Body-matched, DPI-scaled font size (same as terminal text).
                let font_size = self.raster_px(self.font_size);
                let start_x = active_snapped_cell_x
                    .get(grid.cursor.col as usize)
                    .copied()
                    .unwrap_or(active_origin_x + f32::from(grid.cursor.col) * self.cell_w);
                let top_y = active_origin_y + f32::from(grid.cursor.row) * self.cell_h;
                let line_h = self.cell_h;
                // Per-char advance estimate mirrors the body/badge text path.
                let pre_w = estimate_badge_text_width(text, font_size).max(self.cell_w);
                let clip_to_pane = true;

                // (0) Opaque background behind the composing run.
                //
                // The inline preedit is drawn over whatever the app already
                // painted in these cells. When an app shows placeholder/hint
                // text at an empty input (e.g. Claude Code's `Try "edit …"`),
                // the first CJK char's in-flight pinyin would otherwise layer
                // on top of that hint and both become illegible. Lay down the
                // plain terminal `bg` first so the composing glyphs sit on a
                // clean surface — mirroring what the search-bar preedit path
                // already does. This is NOT a highlight color (preserving the
                // "no highlight background" preference); it only
                // masks the cells the preedit actually occupies. Pushed to
                // `quads_overlay`, which draws beneath `overlay_glyph_instances`
                // (see `draw_layers` order), so it never covers the glyphs.
                {
                    // Cover the full preedit footprint: from the cell start
                    // (start_x) across `pre_w` plus the small `text_pad` nudge
                    // the glyphs are shifted right by (emit_x = start_x +
                    // text_pad). Using the same width the glyphs use keeps the
                    // mask matched to the run without bleeding onto adjacent
                    // cells.
                    let pad = self.chrome_px(2.0);
                    let bg_rect = preedit_bg_rect(start_x, top_y, pre_w, pad, line_h);
                    if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                        bg_rect,
                        active_pane_x,
                        active_pane_y,
                        active_pane_w,
                        active_pane_h,
                    ) {
                        quads_overlay.push(QuadInstance {
                            rect: px_to_ndc(qx, qy, qw, qh, sw, sh),
                            color: self.bg_rgba,
                            ..Default::default()
                        });
                    }
                }

                // Keep `font_size_px == native_em_px`: line spacing in `cell_h` would shrink preedit below the body text size.
                let native_em = font_size;
                let mut wt = stack.clone();
                let text_pad = self.chrome_px(2.0);
                let baseline_y = top_y + (line_h + font_size * 0.8) * 0.5;
                let preedit_fg = self.search_fg;
                // Preedit UVs are reusable only on the same device, allocation and atlas contents.
                let emit_x = start_x + text_pad;
                let color_bits = (u32::from(preedit_fg.r) << 24)
                    | (u32::from(preedit_fg.g) << 16)
                    | (u32::from(preedit_fg.b) << 8)
                    | u32::from(preedit_fg.a);
                let atlas_stamp = self.glyph_atlas_stamp();
                let cache_hit = self.preedit_glyph_cache.as_ref().is_some_and(|c| {
                    c.matches(text, font_size, emit_x, baseline_y, color_bits, atlas_stamp)
                });
                if cache_hit {
                    // The qualified content stamp rejects UVs from a reset or replaced atlas.
                    let cached = self.preedit_glyph_cache.as_ref().unwrap();
                    overlay_glyph_instances.extend(cached.glyphs.iter().copied());
                    quads_overlay.extend(cached.missing_boxes.iter().copied());
                    for &missing in &cached.missing_chrome_chars {
                        // A hit draws the cached tofu without a layout, so it is noted here.
                        chrome_text::note_missing_chrome(missing);
                    }
                } else {
                    // When: `!cache_hit` — text, placement, colour, or atlas
                    // epoch changed, so the run is re-shaped and re-cached.
                    let before = overlay_glyph_instances.len();
                    // A nested scope captures the run's tofu for the cache; it is noted again
                    // into the frame's scope below.
                    let preedit_scope = chrome_text::MissingChromeScope::enter();
                    let preedit_boxes = emit_overlay_text_glyphs(
                        &mut self.glyph_atlas,
                        stack,
                        font_size,
                        native_em,
                        &mut wt,
                        text,
                        preedit_fg,
                        ChromeAttrs::default(),
                        emit_x,
                        baseline_y,
                        [start_x, top_y, pre_w, line_h],
                        sw,
                        sh,
                        &mut *overlay_glyph_instances,
                        None,
                    );
                    quads_overlay.extend(preedit_boxes.iter().copied());
                    let preedit_missing = preedit_scope.finish();
                    for &missing in &preedit_missing {
                        chrome_text::note_missing_chrome(missing);
                    }
                    // The frame-end stamp check rejects and clears this cache if emission recycled any UVs.
                    self.preedit_glyph_cache = Some(PreeditGlyphCache {
                        text: text.to_string(),
                        font_size,
                        start_x: emit_x,
                        top_y: baseline_y,
                        color_bits,
                        atlas_stamp: self.glyph_atlas_stamp(),
                        glyphs: overlay_glyph_instances[before..].to_vec(),
                        missing_boxes: preedit_boxes,
                        missing_chrome_chars: preedit_missing,
                    });
                }

                // (3) NO underline under the composing run.
                //
                // macOS routes ordinary typing through IME preedit whenever a
                // CJK/Pinyin input source is active (even plain Latin romaji),
                // so a per-keystroke composing underline reads as a stray
                // cursor-colored bar that flashes and "follows the cursor" as
                // you type (user-reported). The committed text is unaffected;
                // the in-flight glyphs above already show what is being
                // composed, so the underline added noise without information.
                // Drawn intentionally as nothing — keep the block for the
                // `clip_to_pane`/geometry context the glyph emit above uses.
                let _ = (clip_to_pane, pre_w);
            }
        }
    }
}
