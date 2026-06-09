//! T13 (wezterm-takeover G3): wezterm-driven chrome text helper.
//!
//! Replaces the 11 `legacy-chrome TextRenderer` chrome sites in `core.rs`
//! (tab titles, search input, palette query/rows/footer, IME preedit,
//! broadcast banner, drag-chip title)
//! plus the `tab_spans.rs` the legacy chrome layer path. Every chrome string now flows
//! through the same `FontStack` raster path + `GlyphAtlas` + `TextPipeline`
//! path the terminal grid uses — no second font system, no second
//! atlas, no second render pass.
//!
//! ## Pipeline
//!
//! 1. `WtChromeRun::layout`  — sonicterm-font shapes the text run.
//! 2. `GlyphAtlas::get_or_insert` — caches the rasterized tile under a
//!    `GlyphKey { font_slot, glyph_id, bold, italic, ch }` so repeat
//!    chrome strings (every tab title rerender, every keystroke in the
//!    search box) re-use the same tile.
//! 3. `GlyphInstance` records are pushed into a caller-owned `Vec`;
//!    the caller hands the vec to the existing
//!    [`crate::text_pipeline::TextPipeline`] for the draw call.
//!
//! No new wgpu binding setup; no new render pass; no new shader. The
//! chrome ride-shares the terminal text pipeline (separate vec, same
//! pipeline).
//!
//! ## Font size scaling
//!
//! `FontStack` rasterizes at whatever size it was
//! configured for (the terminal font size). Chrome strings frequently
//! want a different size (search bar: `font_size * 0.85`; tab title:
//! `font_size + 1.0`; palette: `font_size`; etc.). We project the
//! atlas's native px tile into the requested `font_size_px` by scaling
//! `info.px_size` / `info.px_offset` / `advance` by
//! `font_size_px / native_em_px`. Glyph identity in the atlas is
//! preserved (same `GlyphKey` regardless of requested chrome size), so
//! a tab title at 13pt and a palette query at 12pt share atlas tiles
//! freely.

use sonicterm_engine::FontStack;
use sonicterm_text::glyph_atlas::{GlyphAtlas, Rasterizer};
use sonicterm_text::GlyphInstance;
use sonicterm_types::GlyphKey;
use std::collections::HashMap;
use unicode_width::UnicodeWidthChar;

use crate::color::{chrome_color_to_linear_rgba, ChromeColor};

/// Result of laying out a chrome text run into atlas glyph instances.
///
/// `glyphs` is ready to be appended to the caller's `Vec<GlyphInstance>`
/// before it is handed to [`crate::text_pipeline::TextPipeline::draw`].
/// `width_px` / `height_px` are the raster-px bounding box (origin
/// inclusive) of the laid-out text — useful for centering / right-align
/// callers that need to know where the run ended.
#[derive(Debug, Clone)]
pub struct ChromeTextLayout {
    /// One `GlyphInstance` per visible chrome glyph, in left-to-right
    /// order. Already in screen-px NDC via the supplied `(sw, sh)`.
    pub glyphs: Vec<GlyphInstance>,
    /// Total advance in raster px from the origin to the right edge of
    /// the last glyph. Zero when no glyphs were emitted (empty text,
    /// or every glyph fell outside the clip bounds).
    pub width_px: f32,
    /// Vertical extent in raster px (max glyph height encountered),
    /// useful for sizing a caller-drawn background quad.
    pub height_px: f32,
}

/// Optional clip rect for chrome runs that paint inside a modal
/// (palette, IME). Glyphs that fall entirely outside this rect are
/// skipped. Coordinates are raster px in the same frame as `origin`.
///
/// Pass `None` for chrome that paints anywhere in the window
/// (tab titles, drag chip).
#[derive(Debug, Clone, Copy)]
pub struct ChromeClip {
    /// Left edge, raster px.
    pub x: f32,
    /// Top edge, raster px.
    pub y: f32,
    /// Width, raster px.
    pub w: f32,
    /// Height, raster px.
    pub h: f32,
}

/// Single attribute bundle for a chrome run — the bits a font shaper
/// re-resolves the face for. Color is per-instance (passed separately)
/// so two runs that share `(bold, italic)` can still paint in different
/// colors without re-shaping.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChromeAttrs {
    /// True when the run should be shaped as bold.
    pub bold: bool,
    /// True when the run should be shaped as italic.
    pub italic: bool,
}

/// Layout one chrome text run into the supplied atlas + glyph vec.
///
/// Arguments:
///
/// - `font_stack`: SonicTerm's WezTerm-compatible font stack.
/// - `wt_raster`: rasterizer that backs the atlas. Same instance the
///   grid uses; chrome strings warm tiles for free.
/// - `atlas`: shared glyph atlas. Chrome and grid coexist in one atlas.
/// - `text`: the text to lay out. UTF-8; handled per codepoint (no
///   ligature shaping across span boundaries — call once per styled
///   span).
/// - `color`: foreground for monochrome glyphs. Color-glyph runs
///   (emoji) ignore this and paint from the strike's own colors.
/// - `attrs`: bold / italic — only used to derive the atlas key for
///   now; wezterm's font selection happens through the loaded face.
/// - `font_size_px`: requested chrome glyph size in raster px. The
///   atlas tile is rasterized at the loaded font's native em and
///   scaled down/up here so chrome strings at different sizes share
///   tiles.
/// - `native_em_px`: the loaded font's native em size in raster px.
///   Pass `cell_metrics_raster_px().cell_h` (the same value the
///   grid path uses to project atlas tiles); chrome tiles end up
///   pixel-identical to the corresponding grid glyph.
/// - `origin`: `(x, baseline_y)` of the run in raster px. The
///   baseline matches the row that grid text would paint on.
/// - `screen`: `(sw, sh)` raster-px dimensions of the surface, used
///   to project rects into NDC.
/// - `clip`: optional bounding rect that culls glyphs outside the
///   modal (palette, IME). Pass `None` for tab titles / drag chip.
///
/// Returns a [`ChromeTextLayout`] whose `glyphs` are ready for
/// `text_pipeline.draw(...)`.
#[allow(clippy::too_many_arguments)]
pub fn layout(
    font_stack: &FontStack,
    wt_raster: &mut impl Rasterizer,
    atlas: &mut GlyphAtlas,
    text: &str,
    color: ChromeColor,
    attrs: ChromeAttrs,
    font_size_px: f32,
    native_em_px: f32,
    origin: (f32, f32),
    screen: (f32, f32),
    clip: Option<ChromeClip>,
) -> ChromeTextLayout {
    let mut out = ChromeTextLayout { glyphs: Vec::new(), width_px: 0.0, height_px: 0.0 };
    if text.is_empty() {
        return out;
    }
    let (sw, sh) = screen;
    if sw <= 0.0 || sh <= 0.0 {
        return out;
    }

    // wezterm shapes `text` against the loaded font. We need a
    // `cell_cols` array mapping each *byte* index of the run to a
    // column number — the shaper reports cluster offsets in bytes, and
    // shape_run_with_wezterm maps them back to columns via this table.
    //
    // For chrome we use a "1 codepoint = 1 cell" mapping: every byte
    // inside a codepoint maps to that codepoint's column index, so
    // multi-byte UTF-8 still resolves to the same cluster column. The
    // column itself is a counting index — chrome strings don't carry
    // wide-cell information in this path.
    let mut cell_cols: Vec<u16> = Vec::with_capacity(text.len());
    let mut col: u16 = 0;
    for ch in text.chars() {
        let byte_len = ch.len_utf8();
        for _ in 0..byte_len {
            cell_cols.push(col);
        }
        col = col.saturating_add(1);
    }

    // Build a column → first-codepoint table so we can synthesize a
    // `GlyphKey` for glyphs the shaper reports as `glyph_id == 0`
    // (notdef / blank space): the key must carry the cluster's char so
    // the rasterizer can fall back to a charmap lookup at insert time.
    let mut col_to_char: HashMap<u16, char> = HashMap::with_capacity(col as usize);
    {
        let mut c: u16 = 0;
        for ch in text.chars() {
            col_to_char.insert(c, ch);
            c = c.saturating_add(1);
        }
    }

    let shaped = match font_stack.shape_text(text) {
        Ok(v) => v,
        Err(_) => return out,
    };

    let scale = if native_em_px > 0.0 { font_size_px / native_em_px } else { 1.0 };
    let rgba = chrome_color_to_linear_rgba(color);

    // Layout walker: track the running x-advance independently of the
    // shaper's `lead_col` so wezterm's reported advances drive the
    // spacing (matches the grid path; preserves ligature widths).
    let mut pen_x = origin.0;
    let baseline_y = origin.1;
    let mut max_y_extent: f32 = 0.0;
    let mut last_pen_x = origin.0;

    let mut last_col: u16 = cell_cols.first().copied().unwrap_or(0);
    for g in shaped {
        let cluster_byte = g.cluster as usize;
        let lead_col = cell_cols
            .get(cluster_byte)
            .copied()
            .or_else(|| (0..=cluster_byte).rev().find_map(|i| cell_cols.get(i).copied()))
            .unwrap_or(last_col);
        last_col = lead_col;
        // Pick the cluster's lead char from our col→char table; default
        // to space if the shaper landed past the input (shouldn't
        // happen for well-formed runs but be defensive — glyph id 0
        // for an unknown char becomes a tofu blank in the atlas
        // rather than a panic).
        let lead_ch = col_to_char.get(&lead_col).copied().unwrap_or(' ');

        // Build the atlas key. Mirrors the grid path
        // (`flush_shape_run`): when the shaper produced a real glyph
        // id, key by `(font_slot, glyph_id)` so identity is shape-
        // accurate; otherwise key by `(char, slot=0)` and let the
        // rasterizer do a charmap lookup.
        let glyph_pos = g.glyph_pos;
        let font_idx = u8::try_from(g.font_idx).unwrap_or(u8::MAX);
        let key = if glyph_pos != 0 {
            GlyphKey::shaped(lead_ch, font_idx, glyph_pos, attrs.bold, attrs.italic)
        } else {
            // Skip pure blanks (space / control chars) — they have no
            // pixels and don't need an atlas slot.
            if lead_ch == '\0' || lead_ch.is_whitespace() {
                // Advance the pen for whitespace using wezterm's
                // reported `x_advance` (scaled). Spaces still need to
                // contribute width so the next glyph lands at the
                // right column.
                let adv = (g.x_advance.get() as f32) * scale;
                pen_x += if adv > 0.0 {
                    adv
                } else {
                    // Fall back to the unicode-width estimate when
                    // wezterm reported zero (some shapers do this
                    // for ASCII spaces).
                    let w = UnicodeWidthChar::width(lead_ch).unwrap_or(0) as f32;
                    w * font_size_px * 0.5
                };
                last_pen_x = pen_x;
                continue;
            }
            GlyphKey::with_slot(lead_ch, 0, attrs.bold, attrs.italic)
        };

        // Chrome doesn't carry per-cell flags beyond `(bold, italic)` —
        // they're already baked into the `key` above. No synthetic
        // `Cell` construction is needed here (the grid path does that
        // for richer flag handling; chrome stays minimal).

        let info = match atlas.get_or_insert(key, wt_raster) {
            Some(i) => i,
            None => continue,
        };
        if info.px_size[0] == 0 || info.px_size[1] == 0 {
            // No-pixel glyph (e.g. ascii space hit via the shaped
            // path). Still need to advance the pen by the shaper's
            // x_advance so the next glyph lands correctly.
            let adv = (g.x_advance.get() as f32) * scale;
            pen_x += adv;
            last_pen_x = pen_x;
            continue;
        }

        // Project atlas-native tile to requested chrome size.
        let gw = info.px_size[0] as f32 * scale;
        let gh = info.px_size[1] as f32 * scale;
        let off_x = info.px_offset[0] as f32 * scale;
        let off_y = info.px_offset[1] as f32 * scale;
        let advance = (g.x_advance.get() as f32) * scale;

        // Wezterm-font reports `y_offset` positive-down (matches the
        // grid path). Apply it on top of the baseline.
        let extra_y = (g.y_offset.get() as f32) * scale;
        let gx = pen_x + off_x;
        let gy = baseline_y + off_y + extra_y;

        // Clip cull: reject glyphs entirely outside the supplied rect.
        if let Some(c) = clip {
            if gx + gw < c.x || gx > c.x + c.w || gy + gh < c.y || gy > c.y + c.h {
                pen_x += advance;
                last_pen_x = pen_x;
                continue;
            }
        }

        let rect = px_to_ndc(gx, gy, gw, gh, sw, sh);
        let inst_color = if info.is_color { [1.0, 1.0, 1.0, 1.0] } else { rgba };
        // Apply the requested alpha. `chrome_color_to_linear_rgba`
        // returns `a = 1.0`, so callers that want dimmed chrome (the
        // drag chip ghost path via `scale_chrome_text_alpha`) need
        // their reduced `color.a` honoured. Multiply through so the
        // pipeline blend gets the premultiplied value it expects.
        let alpha = color.a() as f32 / 255.0;
        let inst_color =
            [inst_color[0] * alpha, inst_color[1] * alpha, inst_color[2] * alpha, alpha];

        out.glyphs.push(GlyphInstance {
            rect,
            uv: info.uv,
            color: inst_color,
            flags: [if info.is_color { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
        });

        if gh > max_y_extent {
            max_y_extent = gh;
        }

        pen_x += advance;
        last_pen_x = pen_x;
    }

    out.width_px = (last_pen_x - origin.0).max(0.0);
    out.height_px = max_y_extent;
    out
}

/// Convert a `(x, y, w, h)` rect in raster pixels (origin top-left,
/// y-down) into the NDC quad `[x0, y0, w_ndc, h_ndc]` the
/// [`crate::text_pipeline::TextPipeline`] WGSL expects. Must match
/// `quad::px_to_ndc` byte-for-byte — the text shader interprets
/// `rect.y` as the BOTTOM corner of the quad in NDC (smaller NDC y) and
/// `rect.w` as a positive upward extent, because its UV mix uses
/// `uv.w` (the texture's larger v, i.e. the BOTTOM of the bitmap) at
/// `c.y = 0` and `uv.y` (the texture's smaller v, i.e. the TOP of the
/// bitmap) at `c.y = 1` — so the corner with the smaller v sample MUST
/// be the smaller-NDC-y corner. A prior implementation here returned
/// `y0 = top_NDC` and `h_ndc < 0`, which made `c.y = 0` land at the
/// visual TOP of the quad while sampling the BOTTOM of the bitmap —
/// vertically mirroring every chrome glyph (Bug 3 of the
/// post-wezterm-takeover smoke).
#[inline]
fn px_to_ndc(px_x: f32, px_y: f32, px_w: f32, px_h: f32, sw: f32, sh: f32) -> [f32; 4] {
    let nx = (px_x / sw) * 2.0 - 1.0;
    let ny = 1.0 - (px_y / sh) * 2.0 - (px_h / sh) * 2.0;
    let nw = (px_w / sw) * 2.0;
    let nh = (px_h / sh) * 2.0;
    [nx, ny, nw, nh]
}
