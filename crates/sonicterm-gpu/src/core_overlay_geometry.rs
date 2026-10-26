//! Overlay geometry responsibilities of the renderer, preserving its single owner.

use super::*;

pub(crate) fn palette_footer_font_size(body_font_size: f32) -> f32 {
    (body_font_size - 1.0).max(1.0)
}

// Returns label baseline, subtitle top, and subtitle baseline in row-relative raster pixels.
pub(super) fn palette_detail_positions(
    row_height: f32,
    label_size: f32,
    detail_size: f32,
    gap: f32,
) -> (f32, f32, f32) {
    let top = ((row_height - label_size - gap - detail_size) * 0.5).max(0.0);
    let detail_top = (top + label_size + gap).min((row_height - detail_size).max(0.0));
    (top + label_size * 0.8, detail_top, detail_top + detail_size * 0.8)
}

pub(super) const PALETTE_FOOTER_INSET_X: f32 = 18.0;

pub(super) const READ_ONLY_BADGE_ICON: &str = "";

pub(super) const READ_ONLY_BADGE_LABEL: &str = "READONLY";

pub(super) const SEARCH_BADGE_ICON: &str = "";

pub(super) const NOTIFICATION_CLOSE_ICON: &str = "";

pub(super) const READ_ONLY_BADGE_W: f32 = 250.0;

pub(super) const READ_ONLY_BADGE_H: f32 = SEARCH_BAR_HEIGHT;

pub(super) const READ_ONLY_BADGE_MARGIN: f32 = 12.0;

pub(super) const READ_ONLY_BADGE_PAD_RIGHT: f32 = 15.0;

pub(super) const READ_ONLY_BADGE_BASELINE_NUDGE_Y: f32 = -2.0;

pub(super) const READ_ONLY_BADGE_RADIUS: f32 = 7.0;

/// Renderer compositor settings that affect surface configuration.
#[derive(Debug, Clone, Copy)]
pub struct SurfaceAppearance {
    /// System backdrop material requested by config.
    pub backdrop: BackdropKind,
    /// Theme background opacity.
    pub opacity: f32,
    /// Scrollbar visibility policy. `Auto` consumes per-pane opacity from the
    /// app, `Always` draws whenever scrollback exists, and `Never` suppresses it.
    pub scrollbar: sonicterm_render_model::boundary::cfg::config::ScrollbarMode,
    /// Padding between overlay panel chrome and inner content.
    pub panel_padding: f32,
    /// User override for the software-render degrade path.
    pub software_render_mode: SoftwareRenderMode,
}

pub(super) fn estimate_badge_text_width(text: &str, font_size: f32) -> f32 {
    text.chars().map(|ch| if ch.is_ascii() { 0.58 } else { 1.0 }).sum::<f32>() * font_size
}

pub(super) fn conservative_badge_text_width(fallback: f32, shaped: Option<f32>) -> f32 {
    shaped.filter(|width| width.is_finite() && *width >= 0.0).unwrap_or(fallback).max(fallback)
}

/// Content width of the search badge: icon, gap and label. `measure` returns a text's shaped
/// width, or `None` when it cannot be shaped; with no measure, or when either text fails, the
/// conservative estimate stands, and a shaped width below the estimate never shrinks the badge.
pub(super) fn search_badge_content_width(
    icon: &str,
    label: &str,
    font_size: f32,
    gap: f32,
    measure: Option<&mut dyn FnMut(&str) -> Option<f32>>,
) -> f32 {
    let fallback = estimate_badge_text_width(icon, font_size)
        + gap
        + estimate_badge_text_width(label, font_size);
    let shaped = measure.and_then(|measure| {
        let icon_w = measure(icon)?;
        let label_w = measure(label)?;
        Some(icon_w + gap + label_w)
    });
    conservative_badge_text_width(fallback, shaped)
}

/// True when an IME preedit string carries visible ink worth drawing an
/// inline composition overlay for.
///
/// The inline preedit overlay (composing glyphs + a one-cell-min underline
/// at the terminal cursor) must only paint when there is real composition
/// text. A preedit that is empty **or whitespace-only** has no glyph ink,
/// yet the underline quad is clamped to `max(self.cell_w)` — so drawing it
/// leaves a stray ~1-cell underscore mark at the cursor that lingers until
/// the next repaint. macOS can momentarily deliver a single-space marked
/// string during ordinary typing, which is exactly that case.
///
/// Real CJK / multi-key composition always carries non-whitespace ink, so
/// gating on this never suppresses a genuine composition overlay.
pub(super) fn preedit_has_visible_ink(preedit: &str) -> bool {
    preedit.chars().any(|c| !c.is_whitespace())
}

/// Horizontal advance (px) for the terminal cursor caret while an inline IME
/// composition is active, so the cursor block sits at the composition
/// insertion point WezTerm-style.
///
/// CRITICAL: this MUST be gated on the SAME predicate as the inline
/// preedit glyph overlay — [`preedit_has_visible_ink`]. macOS delivers a
/// whitespace-only marked string during ordinary typing (and on bare Enter)
/// whenever a CJK/Pinyin input source is active, even for plain Latin. The
/// glyph overlay correctly suppresses that case, but if the caret advance
/// does NOT, the cursor-colored block gets shoved right by the width of the
/// (invisible) whitespace and floats in empty prompt space with no glyph
/// under it — the stray "yellow line/block" users reported. Returning 0 for
/// no-visible-ink keeps the cursor at the grid's real column. Real CJK
/// composition always carries non-whitespace ink, so genuine compositions
/// still advance the caret. Pure so the gate is unit-testable without a GPU.
pub(super) fn preedit_caret_advance(preedit: &str, caret_byte: usize, font_size: f32) -> f32 {
    if !preedit_has_visible_ink(preedit) {
        // When: `preedit_has_visible_ink` is false — the whitespace-only marked
        // string macOS sends for Latin typing. Advancing would strand the block.
        return 0.0;
    }
    let mut cb = caret_byte.min(preedit.len());
    if !preedit.is_char_boundary(cb) {
        cb = preedit.len();
    }
    estimate_badge_text_width(&preedit[..cb], font_size)
}

/// Opaque-background rect for the inline IME preedit.
///
/// Returns `(x, y, w, h)` in renderer pixels for the mask that is laid
/// down behind the composing run so it stays legible over whatever the
/// app already painted in those cells (placeholder/hint text). It must:
/// * start at `start_x` — the cursor cell's left edge — so the mask's left
///   edge aligns with where the glyphs begin (they are nudged right by
///   `pad`, so the mask starting at `start_x` fully contains them);
/// * span `pre_w + pad` — the same width used to emit the glyphs plus the
///   right-nudge — so the mask covers the whole run and no wider, never
///   bleeding onto adjacent cells;
/// * be exactly one line tall.
///
/// Pure so the geometry is unit-testable without a GPU context.
pub(super) fn preedit_bg_rect(
    start_x: f32,
    top_y: f32,
    pre_w: f32,
    pad: f32,
    line_h: f32,
) -> (f32, f32, f32, f32) {
    (start_x, top_y, pre_w + pad, line_h)
}

pub(super) fn splitter_color_from_theme(theme: &Theme) -> [f32; 4] {
    let bg = theme.colors.background.color().unwrap_or_else(|| ThemeColor::rgb(0, 0, 0));
    let fg = theme.colors.foreground.color().unwrap_or_else(|| ThemeColor::rgb(255, 255, 255));
    bg.shift_toward(fg, 0.18).to_rgba_f32_linear(1.0)
}

/// Resolve a scrollbar tint from the theme foreground at `derived_alpha`.
/// Theme-customizable explicit scrollbar colors are intentionally not
/// supported: they would require updating ~50 `Palette { .. }` literals in
/// tests for no shipped benefit. Returns premultiplied linear RGBA.
pub(super) fn scrollbar_tint(fg: &str, derived_alpha: f32) -> [f32; 4] {
    hex_to_premultiplied_rgba(fg, derived_alpha)
}

pub(super) fn read_only_badge_rect(
    sw: f32,
    sh: f32,
    scale: f32,
    content_w: f32,
) -> (f32, f32, f32, f32) {
    // Badge width hugs its content (icon + "READONLY") instead of a fixed
    // constant, so it never looks over-long. `content_w` is already in raster
    // px (estimated from the DPI-scaled badge font); add scaled paddings. The
    // edge MARGIN is a window-anchored position and stays in window space.
    let s = scale.max(0.01);
    let pad = (SEARCH_BAR_PAD_LEFT + SEARCH_BAR_PAD_RIGHT) * s;
    let w = (content_w + pad)
        .max(READ_ONLY_BADGE_W * 0.4 * s) // small floor so it never collapses
        .min((sw - READ_ONLY_BADGE_MARGIN * 2.0).max(40.0));
    let h = (READ_ONLY_BADGE_H * s).min((sh - READ_ONLY_BADGE_MARGIN * 2.0).max(20.0));
    let x = (sw - w - READ_ONLY_BADGE_MARGIN).max(0.0);
    let y = READ_ONLY_BADGE_MARGIN.min((sh - h).max(0.0));
    (x, y, w, h)
}

impl GpuRenderer {
    /// This function only emits the quick-select hint background
    /// quads now. The legacy `quick_select_buffer` text path is gone;
    /// the per-hint text is laid out via `chrome_text::layout` later
    /// in `render()` so it shares the wezterm atlas with the rest of
    /// the chrome.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn prepare_quick_select_overlay(
        &mut self,
        quick_select: &QuickSelectState,
        origin_x: f32,
        origin_y: f32,
        view_top_abs: u64,
        visible_rows: usize,
        scrollback_evicted: u64,
        _theme: &Theme,
        sw: f32,
        sh: f32,
        quads_overlay: &mut Vec<QuadInstance>,
        snapped_cell_x: &[f32],
    ) {
        // derive each hint cell's x/w from the shared snapped-edge
        // cache so quick-select hint backgrounds share device-pixel
        // edges with adjacent glyph cells at fractional DPI.
        let raw_fallback = snapped_cell_x.is_empty();
        for hint in &quick_select.hints {
            let Some(visible_row) = quick_select_hint_display_row(
                quick_select,
                hint,
                scrollback_evicted,
                view_top_abs,
                visible_rows,
            ) else {
                // When: quick_select_hint_display_row finds the hint evicted or off the displayed rows, it draws nothing.
                continue;
            };
            let (x, w) = if raw_fallback {
                (origin_x + hint.col_start as f32 * self.cell_w, self.cell_w)
            } else {
                // When: `!raw_fallback` — a real snapped-edge cache, so hint
                // backgrounds share device-pixel edges with glyph cells.
                let col = (hint.col_start).min(snapped_cell_x.len().saturating_sub(2));
                let lo = snapped_cell_x[col];
                let hi = snapped_cell_x[col + 1];
                (lo, hi - lo)
            };
            let y = origin_y + visible_row as f32 * self.cell_h;
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(x, y, w, self.cell_h, sw, sh),
                color: self.cursor_color,
                ..Default::default()
            });
        }
    }
}

#[cfg(test)]
#[path = "core_overlay_geometry_tests.rs"]
mod core_overlay_geometry_tests;
