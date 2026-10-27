//! Chrome glyphs responsibilities of the renderer, preserving its single owner.

use super::*;

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabTitleGlyphDebug {
    pub raster_px: f32,
    pub rect: [f32; 4],
    pub px_size: [u32; 2],
}

/// Snapshot of one pane's layout in raster pixels, captured at the
/// end of each `render()` call. Used by [`GpuRenderer::pixel_to_cell`]
/// to (a) figure out which pane was clicked and (b) reconstruct
/// that pane's `snapped_cell_x` edge cache on-demand so the column
/// search uses the same device-pixel-snapped edges the renderer drew.
///
/// Raster coordinates match winit cursor input without a scale conversion, despite the `_logical` field names.
#[doc(hidden)]
#[derive(Debug, Clone, Copy)]
pub struct PaneLayoutSnapshot {
    /// Stable id of the pane this snapshot describes.
    pub id: u64,
    /// Raster-px left edge of the pane (== that pane's `padding_left`
    /// equivalent — the origin `build_snapped_cell_x` was passed).
    pub origin_x_logical: f32,
    /// Raster-px top edge of the pane (already adjusted for tab-bar /
    /// top inset).
    pub origin_y_logical: f32,
    /// Raster-px width of the pane's content rect.
    pub w_logical: f32,
    /// Raster-px height of the pane's content rect.
    pub h_logical: f32,
    /// Cell width in raster pixels for the pane.
    pub cell_w_logical: f32,
    /// Cell height in raster pixels for the pane.
    pub cell_h_logical: f32,
    /// Number of columns in the pane's grid at snapshot time.
    pub cols: u16,
    /// Number of rows in the pane's grid at snapshot time.
    pub rows: u16,
}

/// Shape and emit one tab's title spans as glyph instances.
///
/// Each `(text, colour, attrs)` span is laid out through
/// [`chrome_text::layout`] into the shared glyph atlas. Title and terminal
/// tiles remain distinct cache entries but draw through the same pass. The pen advances by
/// `avg_glyph_w` per character rather than by the shaper's advances, matching
/// the column arithmetic the caller already used to truncate and centre the
/// title.
///
/// Returns the outline quads of characters no face resolved; the caller draws them
/// with its chrome quads.
///
/// `debug`, when supplied, receives one record per emitted glyph for tests
/// asserting the atlas path was taken.
#[doc(hidden)]
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn emit_tab_title_glyphs(
    glyph_atlas: &mut GlyphAtlas,
    font_stack: &sonicterm_engine::FontStack,
    raster_px: f32,
    native_em_px: f32,
    wt_raster: &mut impl sonicterm_text::glyph_atlas::Rasterizer,
    spans: &[(&str, ChromeColor, ChromeAttrs)],
    baseline_y: f32,
    avg_glyph_w: f32,
    sw: f32,
    sh: f32,
    glyph_instances: &mut Vec<GlyphInstance>,
    mut debug: Option<&mut Vec<TabTitleGlyphDebug>>,
) -> Vec<QuadInstance> {
    // Each title span uses the native-size FontStack and shared atlas through chrome_text.
    let mut pen_x: f32 = 0.0;
    let mut missing_boxes = Vec::new();
    for (text, color, attrs) in spans {
        if text.is_empty() {
            // When: `text.is_empty()` — the title builder emits empty spans for
            // absent segments. Layout would yield no glyphs and no advance.
            continue;
        }
        let layout = chrome_text::layout_with_raster_variant(
            font_stack,
            wt_raster,
            glyph_atlas,
            text,
            *color,
            *attrs,
            raster_px,
            native_em_px,
            (pen_x, baseline_y),
            (sw, sh),
            None,
            GlyphRasterVariant::TabTitle,
        );
        let count_pre = glyph_instances.len();
        glyph_instances.extend(layout.glyphs.iter().copied());
        missing_boxes.extend(layout.missing_boxes);
        // Tab titles use `avg_glyph_w` columns × char count as the
        // logical layout stride (column-snapped), regardless of the
        // shaper's per-glyph advances. Preserves the existing
        // build_tab_title_spans column arithmetic that drives the
        // truncation / centering math upstream.
        let cols = text.chars().count() as f32;
        pen_x += cols * avg_glyph_w;
        if let Some(out) = debug.as_deref_mut() {
            for g in &glyph_instances[count_pre..] {
                // Tab-title debug records track only `raster_px` +
                // a rough px_size derived from the NDC quad height.
                let h = (-g.rect[3] * 0.5 * sh).abs();
                let w = (g.rect[2] * 0.5 * sw).abs();
                out.push(TabTitleGlyphDebug {
                    raster_px,
                    rect: [(g.rect[0] + 1.0) * 0.5 * sw, (1.0 - g.rect[1]) * 0.5 * sh, w, h],
                    px_size: [w as u32, h as u32],
                });
            }
        }
    }
    missing_boxes
}

/// Debug record emitted by [`emit_overlay_text_glyphs`] so tests can
/// assert the device-scaled atlas path was taken.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayTextGlyphDebug {
    pub raster_px: f32,
    pub font_size: f32,
    pub rect: [f32; 4],
    pub px_size: [u32; 2],
}

/// Emit one line of overlay text (palette query, rows, footer, search field)
/// as clipped glyph instances.
///
/// Positions glyphs from an explicit pixel `origin_x`/`baseline_y` so a caller
/// draws a multi-line overlay by calling once per line and advancing the
/// baseline itself. Glyphs falling outside `bounds` are dropped by the layout
/// clip, which is what keeps text inside a modal panel instead of painting
/// across the terminal behind it. Returns the outline quads of characters no face
/// resolved; the caller draws them with the overlay quads.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn emit_overlay_text_glyphs(
    glyph_atlas: &mut GlyphAtlas,
    font_stack: &sonicterm_engine::FontStack,
    font_size_px: f32,
    native_em_px: f32,
    wt_raster: &mut impl sonicterm_text::glyph_atlas::Rasterizer,
    text: &str,
    color: ChromeColor,
    attrs: ChromeAttrs,
    origin_x: f32,
    baseline_y: f32,
    bounds: [f32; 4], // [x, y, w, h] in raster px; glyphs outside are clipped
    sw: f32,
    sh: f32,
    glyph_instances: &mut Vec<GlyphInstance>,
    debug: Option<&mut Vec<OverlayTextGlyphDebug>>,
) -> Vec<QuadInstance> {
    if text.is_empty() {
        // When: `text.is_empty()` — an unset footer or empty query. Returning
        // leaves `glyph_instances` untouched, so line advance is unaffected.
        return Vec::new();
    }
    let [bx, by, bw, bh] = bounds;
    let layout = chrome_text::layout(
        font_stack,
        wt_raster,
        glyph_atlas,
        text,
        color,
        attrs,
        font_size_px,
        native_em_px,
        (origin_x, baseline_y),
        (sw, sh),
        Some(ChromeClip { x: bx, y: by, w: bw, h: bh }),
    );
    let count_pre = glyph_instances.len();
    glyph_instances.extend(layout.glyphs.iter().copied());
    if let Some(out) = debug {
        for g in &glyph_instances[count_pre..] {
            let h = (-g.rect[3] * 0.5 * sh).abs();
            let w = (g.rect[2] * 0.5 * sw).abs();
            out.push(OverlayTextGlyphDebug {
                raster_px: font_size_px,
                font_size: font_size_px,
                rect: [(g.rect[0] + 1.0) * 0.5 * sw, (1.0 - g.rect[1]) * 0.5 * sh, w, h],
                px_size: [w as u32, h as u32],
            });
        }
    }
    layout.missing_boxes
}
