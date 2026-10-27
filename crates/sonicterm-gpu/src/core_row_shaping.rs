//! Row shaping responsibilities of the renderer, preserving its single owner.

use super::*;

impl GpuRenderer {
    /// Shape one style run of `cells` into position-free row records: glyph records into
    /// `records.glyphs`, missing-glyph boxes into `records.tofu` and their codepoints into
    /// `records.missing_chars`. Projection onto the surface happens afterwards, once per row,
    /// on a hit and on a miss alike.
    ///
    /// Returns whether the run is complete. A run is incomplete when an attempted glyph was
    /// refused by the atlas (a `None` from `get_or_insert`, read before `drawable_or_tofu` folds
    /// it with the stable missing sentinel), when a shaped glyph with a real id rasterized nothing
    /// or is too large for the atlas (both also listed in `records.missing_chars`), when a block
    /// glyph drew nothing, when shaping failed, or when no shaper or rasterizer was available; such
    /// outcomes depend on atlas or font state, not on content, so the row they belong to is drawn
    /// but never cached. Intentional empty work is complete: an empty run, a run of wide
    /// continuations, whitespace and zero-area non-block tiles, and a character-fallback glyph the
    /// atlas caches as missing, which draws tofu.
    ///
    /// Non-ASCII clusters are shaped through the font stack. Each cluster's lead cell dispatches
    /// on [`sonicterm_block_glyph::BlockKey::from_char`]: on `Some`, the atlas holds a
    /// [`sonicterm_block_glyph::block_sprite`] tile keyed under the block-glyph sentinel
    /// (`GlyphKey { font_slot: 0xFF, glyph_id: <hashed SizedBlockKey>, .. }`), so the shaped
    /// path and the block-sprite path share the atlas without colliding; on `None`, the cluster
    /// takes the normal rasterize path. Box drawing, Powerline, Sextant, Octant and Braille all
    /// reach the renderer through this dispatch.
    // Hot inner-loop helper called per style run per row. Every argument is an exclusive `&mut`
    // borrow of a different renderer field or a per-run value; bundling them into one struct
    // would conflict with the surrounding loop's own borrows.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn build_shape_run(
        glyph_atlas: &mut GlyphAtlas,
        records: &mut sonicterm_text::row_glyph_cache::CachedRow,
        row: u16,
        style: RunStyle,
        // The run's cells borrowed from the grid, in strictly increasing column order.
        cells: &[(u16, &Cell)],
        theme: &Theme,
        fg_default: ChromeColor,
        cell_w: f32,
        cell_h: f32,
        top_inset: f32,
        snapped_cell_x: &[f32],
        // The sole shape entry point; `None` only in test fixtures without bundled fonts, where
        // non-ASCII runs emit nothing and are reported incomplete.
        font_stack: Option<&sonicterm_engine::FontStack>,
        // The rasterizer; `None` only in test fixtures, where no glyph is drawn and the run is
        // reported incomplete, while background, cursor and underline quads remain.
        mut wt_raster: Option<&mut sonicterm_engine::FontStack>,
        // This row's hover span, already gated to the drawing pane. An ACTIVE hover recolors the
        // glyphs inside it to `hovered_url_accent`; it is folded into the row's content key.
        hovered_url_cells: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
        hovered_url_accent: [f32; 4],
        software_presenter: bool,
    ) -> bool {
        use sonicterm_text::row_glyph_cache::{RowGlyphKind, RowTofu};
        if cells.is_empty() {
            // When: `cells.is_empty()` — the run carries no cells, so there is
            // nothing to shape and nothing missing.
            return true;
        }

        let style_span = tracing::enabled!(target: "render_timing", tracing::Level::DEBUG).then(|| {
            tracing::debug_span!(target: "render_timing", "font_style", bold = style.bold, italic = style.italic, row)
        });
        let _entered = style_span.as_ref().map(tracing::Span::enter);

        // A monochrome glyph's foreground in linear sRGB, swapped for the theme accent inside an
        // ACTIVE hover span; a plain hint leaves the colour and is marked by its underline only.
        let resolve_fg = |col: u16, base: ChromeColor| -> [f32; 4] {
            match hovered_url_cells {
                Some(h) if h.active && h.contains(row, col) => hovered_url_accent,
                _ => chrome_color_to_linear_rgba(base),
            }
        };
        // The tofu box of a cell `width` raster pixels wide, inset from the cell edges.
        let tofu_box = |lead_col: u16, width: f32, color: ChromeColor| {
            let inset = (cell_h * 0.12).max(1.0);
            RowTofu {
                lead_col,
                inset,
                width: width - inset * 2.0,
                height: cell_h - inset * 2.0,
                color: [color.r(), color.g(), color.b(), color.a()],
            }
        };
        let mut complete = true;

        // ASCII fast path: every cell is printable ASCII with no cluster extras and no ligature
        // trigger, so the shaper would map 1:1 anyway; the atlas is driven from each cell's key.
        // These codepoints never overlap the `BlockKey` ranges, so block dispatch is skipped.
        if run_is_ascii_fast(cells) {
            // When: `run_is_ascii_fast` — every cell is 0x20..=0x7E, which
            // cannot shape or need `BlockKey`, so each maps 1:1 to a tile.
            for (col, cell) in cells {
                let key = sonicterm_types::glyph_key::GlyphKey {
                    ch: cell.ch,
                    font_slot: 0,
                    weight_bold: style.bold,
                    italic: style.italic,
                    glyph_id: 0,
                    raster_variant: GlyphRasterVariant::Normal,
                };
                let Some(wt) = wt_raster.as_deref_mut() else {
                    // When: `wt_raster` is None — a test fixture with no FontStack; the glyph is
                    // skipped and the row is not cached.
                    complete = false;
                    continue;
                };
                let inserted = glyph_atlas.get_or_insert(key, &mut CountingRasterizer::new(wt));
                complete &= inserted.is_some();
                let Some(info) = drawable_or_tofu(inserted) else {
                    // When: `drawable_or_tofu` is None — the atlas refused the glyph or cached it
                    // as missing, so a printable cell draws the same outline box as the shaped path.
                    if !cell.ch.is_whitespace() {
                        // Blanks are intentionally tile-less and are not reported as missing.
                        records.tofu.push(tofu_box(*col, cell_w, cell_fg(cell, theme, fg_default)));
                        records.missing_chars.push(cell.ch);
                    }
                    continue;
                };
                if info.px_size[0] == 0 || info.px_size[1] == 0 {
                    // When: either axis of `info.px_size` is 0 — a zero-area
                    // tile, which is what a space rasterizes to.
                    continue;
                }
                let color = cell_fg(cell, theme, fg_default);
                let rgba = if info.is_color {
                    [1.0, 1.0, 1.0, 1.0]
                } else {
                    // When: `!info.is_color` — a monochrome mask, so the cell
                    // foreground and any hover recolor apply to it.
                    resolve_fg(*col, color)
                };
                if glyph_draw_is_degenerate(&info) {
                    // When: `glyph_draw_is_degenerate` — the tile has area but
                    // its UVs or metrics cannot produce a visible draw.
                    tracing::debug!(
                        target: "sonic::render::glyph",
                        ch = ?cell.ch,
                        is_color = info.is_color,
                        px_size = ?info.px_size,
                        uv = ?info.uv,
                        site = "ascii",
                        "skipped a degenerate glyph draw that would sample the atlas origin"
                    );
                    continue;
                }
                records.glyphs.push(tile_record(RowGlyphKind::Natural, *col, &info, rgba));
            }
            return complete;
        }

        // The non-ASCII path maps UTF-8 cluster offsets back to terminal columns before shaping.
        let Some(stack) = font_stack else {
            // When: `font_stack` is None — no shaper, so non-ASCII clusters
            // emit nothing and the row is not cached.
            return false;
        };
        // Only a run that reaches the shaper has its text and byte-to-column map built.
        let (text, cell_cols) = crate::row_runs::materialize_run_text(cells);
        if text.is_empty() {
            // When: `text.is_empty()` — every cell in the run was a wide
            // continuation, so the shaper has no bytes to work on.
            return true;
        }

        let shaped_text = crate::frame_stats::shape_request(|| {
            stack.shape_text_for_frame(&text, style.bold, style.italic)
        });
        let infos = match inject_shape_failure(shaped_text) {
            Ok(infos) => infos,
            Err(_) => {
                // When: `inject_shape_failure` passes on an `Err` — the face rejected the run or the
                // frame's shaping allowance ran out, so the run draws nothing and is not cached.
                return false;
            }
        };

        // Per-cell attributes (colour, WIDE flag, the codepoint for tofu diagnostics) are read
        // from the borrowed run by the shaped output's `lead_col`; a column the run lacks reads
        // as one default cell made per run, as the former owned lookup's fallback did.
        let default_cell = Cell::default();

        // Shaped output projects straight into the shaped-glyph record; cluster byte offsets map
        // back through `cell_cols`.
        let mut shaped = Vec::with_capacity(infos.len());
        let mut last_col: u16 = cell_cols.first().copied().unwrap_or(0);
        for info in infos {
            let cluster_byte = info.cluster as usize;
            let lead_col = cell_cols
                .get(cluster_byte)
                .copied()
                .or_else(|| (0..=cluster_byte).rev().find_map(|i| cell_cols.get(i).copied()))
                .unwrap_or(last_col);
            last_col = lead_col;
            let lead_ch =
                run_cell_at(cells, lead_col).map(|c| c.ch).or(info.only_char).unwrap_or(' ');
            let cluster_cells = (info.num_cells as u16).max(1);
            shaped.push(sonicterm_text::shape::ShapedGlyph {
                lead_col,
                cluster_cells,
                font_slot: u8::try_from(info.font_idx).unwrap_or(u8::MAX),
                glyph_id: info.glyph_pos,
                x_advance: info.x_advance.get() as f32,
                x_offset: info.x_offset.get() as f32,
                y_offset: info.y_offset.get() as f32,
                ch: lead_ch,
            });
        }

        #[cfg(debug_assertions)]
        debug_assert!(shaped_glyph_columns_are_monotonic(&shaped));

        let mut positioned_cluster_col = None;
        let mut positioned_cluster_pen_x = 0.0;
        for g in &shaped {
            let lead_cell: &Cell = run_cell_at(cells, g.lead_col).unwrap_or(&default_cell);
            let is_wide = lead_cell.flags.contains(CellFlags::WIDE);
            let cluster_cells = g.cluster_cells.max(1) as usize;
            let cells_to_span = if is_wide { 2 } else { cluster_cells };
            let cell_pixel_width = cell_w * cells_to_span as f32;

            // Block dispatch at the cluster lead cell: box drawing, block elements, Powerline,
            // Sextant, Octant and Braille draw from vendored geometry under the block-glyph
            // sentinel key, never from the font's own glyph.
            if let Some(block_key) = sonicterm_block_glyph::BlockKey::from_char(lead_cell.ch) {
                // When: `BlockKey::from_char` is Some — a box/block codepoint,
                // drawn from vendored geometry rather than the font's glyph.
                let cell_left_px = snapped_cell_x[g.lead_col as usize];
                let cell_top_px = top_inset + f32::from(row) * cell_h;
                let span = if is_wide { 2usize } else { cluster_cells };
                let end_col = ((g.lead_col as usize) + span).min(snapped_cell_x.len() - 1);
                let cell_right = snapped_cell_x[end_col];
                // The software presenter rasterizes to the exact integer destination so atlas
                // sampling stays one-to-one; that destination depends on the row's position,
                // which a cache lookup revalidates. The GPU keeps the fractional cell geometry.
                let (target_w, target_h) = if software_presenter {
                    let cell_bottom = top_inset + (f32::from(row) + 1.0) * cell_h;
                    let (_, _, width, height) = software_block_glyph_target_rect(
                        cell_left_px,
                        cell_top_px,
                        cell_right,
                        cell_bottom,
                    );
                    (width, height)
                } else {
                    // When: `!software_presenter` — the GPU path keeps the
                    // established fractional font-cell geometry unchanged.
                    (cell_w, cell_h)
                };
                let cell_w_i = target_w.round().max(1.0) as isize;
                let cell_h_i = target_h.round().max(1.0) as isize;
                // The outline stroke width comes from the font's underline thickness, as
                // WezTerm's sprite utilities take it; a hardcoded single pixel made box-drawing
                // strokes nearly invisible. With no font stack, a 1/16-cell heuristic is used.
                let underline_h_isize: isize = font_stack
                    .and_then(|s| s.cell_metrics_raster_px().ok())
                    .map(|m| m.underline_h.round().max(1.0) as isize)
                    .unwrap_or_else(|| ((cell_h / 16.0).round().max(1.0)) as isize);
                let size = sonicterm_block_glyph::glue::Size::new(cell_w_i, cell_h_i);
                let sized_key = sonicterm_block_glyph::SizedBlockKey { block: block_key, size };
                // Block tiles are size- and stroke-sensitive, so the atlas id hashes the sized key
                // and the stroke width; only collision resistance among the few hundred block
                // glyphs a frame touches is needed.
                let glyph_id_u32: u32 = {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    sized_key.hash(&mut h);
                    underline_h_isize.hash(&mut h);
                    let h64 = h.finish();
                    // Fold to u32 by xoring the halves so all 64 bits
                    // contribute to the atlas key.
                    ((h64 >> 32) as u32) ^ (h64 as u32)
                };
                // Block geometry ignores bold and italic, so those bits are collapsed.
                let key = sonicterm_types::glyph_key::GlyphKey {
                    ch: lead_cell.ch,
                    font_slot: 0xFF,
                    weight_bold: false,
                    italic: false,
                    glyph_id: glyph_id_u32,
                    raster_variant: GlyphRasterVariant::Normal,
                };
                // A thin `Rasterizer` around `block_sprite`, so the atlas computes the sprite only
                // on a miss; identity is captured by `key`.
                struct BlockSpriteRasterizer {
                    sized_key: sonicterm_block_glyph::SizedBlockKey,
                    underline_h: isize,
                }
                impl sonicterm_text::glyph_atlas::Rasterizer for BlockSpriteRasterizer {
                    fn rasterize(
                        &mut self,
                        _key: sonicterm_types::glyph_key::GlyphKey,
                    ) -> Option<sonicterm_text::glyph_atlas::RasterTile> {
                        // Cell metrics derive from the sized key; the underline height comes from
                        // the font, and anti-aliasing matches WezTerm's default.
                        let block_tile = sonicterm_block_glyph::block_sprite_with_cell_metrics(
                            self.sized_key,
                            self.underline_h,
                            true,
                        )
                        .ok()?;
                        // Coverage alpha becomes the monochrome mask while the block tile's raster geometry is preserved.
                        let alpha_mask: Vec<u8> =
                            block_tile.coverage.as_chunks::<4>().0.iter().map(|px| px[3]).collect();
                        Some(sonicterm_text::glyph_atlas::RasterTile {
                            width: block_tile.width,
                            height: block_tile.height,
                            offset_x: block_tile.offset_x,
                            offset_y: block_tile.offset_y,
                            advance: block_tile.advance,
                            coverage: alpha_mask,
                            // Block geometry is a mask for the cell foreground, not a self-coloured
                            // emoji, so icons inherit the SGR foreground.
                            is_color: false,
                            is_subpixel: false,
                        })
                    }
                }
                let mut block_raster =
                    BlockSpriteRasterizer { sized_key, underline_h: underline_h_isize };
                let Some(info) =
                    glyph_atlas.get_or_insert(key, &mut CountingRasterizer::new(&mut block_raster))
                else {
                    // When: `glyph_atlas.get_or_insert` is None — the block
                    // sprite could not be packed; the row is not cached.
                    complete = false;
                    continue;
                };
                if info.px_size[0] == 0 || info.px_size[1] == 0 {
                    // When: either axis of `info.px_size` is 0 — the sprite passed the atlas's
                    // placement limit here, so the block drew nothing and its row is not cached.
                    complete = false;
                    continue;
                }
                let color = cell_fg(lead_cell, theme, fg_default);
                let rgba = if info.is_color {
                    [1.0, 1.0, 1.0, 1.0]
                } else {
                    // When: `!info.is_color` — a monochrome mask, so the cell
                    // foreground and any hover recolor are applied to it.
                    resolve_fg(g.lead_col, color)
                };
                tracing::debug!(
                    target: "sonic::render::glyph",
                    ch = ?lead_cell.ch,
                    codepoint = format!("U+{:04X}", lead_cell.ch as u32),
                    code_u32 = lead_cell.ch as u32,
                    target_size = ?(target_w, target_h),
                    final_rgba = ?rgba,
                    is_color = info.is_color,
                    path = "block_sprite",
                    "glyph render emit (block-glyph)"
                );
                let mut record = tile_record(RowGlyphKind::Block, g.lead_col, &info, rgba);
                record.raster_offset = [0.0; 2];
                record.raster_size = [target_w, target_h];
                record.end_col = end_col as u16;
                records.glyphs.push(record);
                continue;
            }

            // ── Normal shape path (non-block cluster) ──
            let shape_x_offset = shaped_cluster_x_offset(
                &mut positioned_cluster_col,
                &mut positioned_cluster_pen_x,
                g,
            );
            let marker_fit = status_marker_fit_eligible(
                lead_cell.ch,
                cluster_cells,
                is_wide,
                lead_cell.extras().is_some(),
            );
            if g.glyph_id == 0 {
                // When: `g.glyph_id == 0` — the shaper found no glyph for this
                // cluster, so the char-fallback path runs instead.
                let ch = lead_cell.ch;
                if ch == '\0' || ch.is_whitespace() {
                    // When: `ch == '\0' || ch.is_whitespace()` — both are
                    // legitimately glyph-less and must not draw tofu.
                    continue;
                }
                // The rasterizer finds a face itself for slot 0 when glyph_id is 0.
                let key = sonicterm_types::glyph_key::GlyphKey {
                    ch,
                    font_slot: 0,
                    weight_bold: style.bold,
                    italic: style.italic,
                    glyph_id: 0,
                    raster_variant: GlyphRasterVariant::Normal,
                };
                let Some(wt) = wt_raster.as_deref_mut() else {
                    // When: `wt_raster` is None — a test fixture with no
                    // FontStack, so the fallback char cannot be rasterized.
                    complete = false;
                    continue;
                };
                let inserted = glyph_atlas.get_or_insert(key, &mut CountingRasterizer::new(wt));
                complete &= inserted.is_some();
                let Some(info) = drawable_or_tofu(inserted) else {
                    // When: `drawable_or_tofu` is None — true tofu: the atlas refused the glyph or
                    // cached it as missing, so an outline box is drawn instead.
                    records.tofu.push(tofu_box(
                        g.lead_col,
                        cell_pixel_width,
                        cell_fg(lead_cell, theme, fg_default),
                    ));
                    records.missing_chars.push(ch);
                    continue;
                };
                if info.px_size[0] == 0 || info.px_size[1] == 0 {
                    // When: either axis of `info.px_size` is 0 — the fallback
                    // face produced a zero-area tile, which has no pixels.
                    continue;
                }
                let color = cell_fg(lead_cell, theme, fg_default);
                let rgba = if info.is_color {
                    [1.0, 1.0, 1.0, 1.0]
                } else {
                    // When: `!info.is_color` — a monochrome fallback glyph, so
                    // it takes the cell foreground like ordinary text.
                    resolve_fg(g.lead_col, color)
                };
                if glyph_draw_is_degenerate(&info) {
                    // When: `glyph_draw_is_degenerate` — the tile has area but
                    // its UVs or metrics cannot produce a visible draw.
                    tracing::warn!(
                        target: "sonic::render::glyph",
                        ch = ?lead_cell.ch,
                        codepoint = format!("U+{:04X}", lead_cell.ch as u32),
                        is_color = info.is_color,
                        px_size = ?info.px_size,
                        uv = ?info.uv,
                        site = "shaped_run",
                        "skipped a degenerate glyph draw that would sample the atlas origin"
                    );
                    continue;
                }
                let mut record = tile_record(RowGlyphKind::Fallback, g.lead_col, &info, rgba);
                set_shaped_bits(
                    &mut record,
                    [shape_x_offset, g.y_offset],
                    marker_fit,
                    g,
                    is_wide,
                    lead_cell,
                );
                records.glyphs.push(record);
                continue;
            }

            let key = sonicterm_types::glyph_key::GlyphKey::shaped(
                g.ch,
                g.font_slot,
                g.glyph_id,
                style.bold,
                style.italic,
            );
            let Some(wt) = wt_raster.as_deref_mut() else {
                // When: `wt_raster` is None — a test fixture with no FontStack,
                // so the shaped glyph cannot be rasterized.
                complete = false;
                continue;
            };
            let Some(info) = glyph_atlas.get_or_insert(key, &mut CountingRasterizer::new(wt))
            else {
                // When: `glyph_atlas.get_or_insert` is None — the atlas refused this shaped
                // glyph: it draws nothing, so it is reported missing and the row is not cached.
                records.missing_chars.push(lead_cell.ch);
                complete = false;
                continue;
            };
            if info.missing || info.oversize {
                // When: a real glyph id rasterized nothing or its tile can never be placed, it draws
                // nothing although it should, so it is reported missing and the row is not cached.
                records.missing_chars.push(lead_cell.ch);
                complete = false;
                continue;
            }
            if info.px_size[0] == 0 || info.px_size[1] == 0 {
                // When: either axis of `info.px_size` is 0 — an intentionally empty glyph,
                // which has no pixels to blit.
                continue;
            }
            // Multi-cell ligature halves keep their natural overhang so paired
            // glyphs such as `=>` continue to fuse across adjacent cells.
            let color = cell_fg(lead_cell, theme, fg_default);
            let rgba = if info.is_color {
                [1.0, 1.0, 1.0, 1.0]
            } else {
                // When: `!info.is_color` — a monochrome mask, so the ligature
                // takes the cell foreground like ordinary text.
                resolve_fg(g.lead_col, color)
            };
            if glyph_draw_is_degenerate(&info) {
                // When: `glyph_draw_is_degenerate` — the tile has area but its
                // UVs or metrics cannot produce a visible draw.
                tracing::warn!(
                    target: "sonic::render::glyph",
                    ch = ?lead_cell.ch,
                    codepoint = format!("U+{:04X}", lead_cell.ch as u32),
                    is_color = info.is_color,
                    px_size = ?info.px_size,
                    uv = ?info.uv,
                    site = "ligature",
                    "skipped a degenerate glyph draw that would sample the atlas origin"
                );
                continue;
            }
            let mut record = tile_record(RowGlyphKind::Shaped, g.lead_col, &info, rgba);
            set_shaped_bits(
                &mut record,
                [shape_x_offset, g.y_offset],
                marker_fit,
                g,
                is_wide,
                lead_cell,
            );
            records.glyphs.push(record);
        }
        complete
    }
}

/// Would drawing this glyph sample the atlas outside its own tile?
///
/// The atlas caches an empty or failed rasterization as a sentinel with a
/// zero-area UV, `[0.0, 0.0, 0.0, 0.0]`, and its comment states the renderer
/// skips such a draw. Nothing did. `(0, 0)` is not "nowhere": it is the
/// atlas's top-left texel, which the shelf packer hands to the first glyph of
/// the session, so a zero-area sample reads that glyph's corner ink.
///
/// Monochrome block glyphs take the cell foreground, but a degenerate UV can
/// sample opaque coverage from the atlas origin and paint a stray
/// foreground-colored pixel.
///
/// Returns true when the instance must not be emitted.
#[must_use]
pub(super) fn glyph_draw_is_degenerate(info: &sonicterm_text::glyph_atlas::GlyphInfo) -> bool {
    info.px_size[0] == 0
        || info.px_size[1] == 0
        || info.uv[2] <= info.uv[0]
        || info.uv[3] <= info.uv[1]
}

/// Fraction a dim/faint (`SGR 2`) cell's foreground is blended toward its
/// background. `0.45` lands roughly at the ~55% perceived intensity that
/// xterm/VTE/WezTerm use for faint text — enough that editor inline
/// predictions / ghost text read as clearly fainter than committed text
/// without becoming unreadable. See.
pub(super) const DIM_BLEND: f32 = 0.45;

/// The terminal's atlas result for a fallback character: `None` draws tofu, both when the atlas
/// refused the glyph and when it cached the glyph as missing; an empty glyph, and the zero-area
/// sentinel of a tile too large to place, is returned, so the caller skips it without a box.
#[must_use]
pub(super) fn drawable_or_tofu(
    info: Option<sonicterm_text::glyph_atlas::GlyphInfo>,
) -> Option<sonicterm_text::glyph_atlas::GlyphInfo> {
    info.filter(|info| !info.missing)
}

pub(super) fn cell_fg(cell: &Cell, theme: &Theme, default: ChromeColor) -> ChromeColor {
    // Resolve the foreground and the cell's effective background. INVERSE
    // swaps the two (foreground is painted in the bg color and vice versa),
    // so resolve both consistently and let DIM blend toward whichever
    // background the glyph is actually drawn over.
    let (fg, bg) = if cell.flags.contains(CellFlags::INVERSE) {
        let default_bg = hex_to_chrome_color(theme.colors.background.0.as_str());
        let default_fg = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        (color_to_chrome(cell.bg, theme, default_bg), color_to_chrome(cell.fg, theme, default_fg))
    } else {
        // When: no `INVERSE` flag — the ordinary case, where the cell's own
        // fg and bg are used as written.
        let default_bg = hex_to_chrome_color(theme.colors.background.0.as_str());
        (color_to_chrome(cell.fg, theme, default), color_to_chrome(cell.bg, theme, default_bg))
    };
    // Dim / faint (SGR 2): pull the foreground toward its background so
    // faint text is visibly de-emphasized instead of identical to normal
    // text. Stored but previously unread.
    if cell.flags.contains(CellFlags::DIM) {
        dim_toward(fg, bg, DIM_BLEND)
    } else {
        // When: no `DIM` flag — normal intensity, so the resolved foreground
        // is returned unblended.
        fg
    }
}

#[cfg(debug_assertions)]
pub(super) fn shaped_glyph_columns_are_monotonic(
    glyphs: &[sonicterm_text::shape::ShapedGlyph],
) -> bool {
    glyphs.windows(2).all(|pair| pair[0].lead_col <= pair[1].lead_col)
}

pub(super) fn color_to_chrome(color: Color, theme: &Theme, default: ChromeColor) -> ChromeColor {
    match color {
        Color::Default => default,
        Color::Rgb(r, g, b) => ChromeColor::rgb(r, g, b),
        Color::Indexed(i) => indexed(i, theme).unwrap_or(default),
    }
}

#[cfg(test)]
#[path = "core_row_shaping_tests.rs"]
mod core_row_shaping_tests;
