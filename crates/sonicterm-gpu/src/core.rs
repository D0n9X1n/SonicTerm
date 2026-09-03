//! GPU renderer for the terminal grid using wgpu 29.
//!
//! The legacy `glyphon` chrome path is
//! gone. Every chrome string (tab titles, palette, search, IME,
//! broadcast, drag chip, quick-select hints) flows through
//! [`crate::chrome_text::layout`] → the shared `GlyphAtlas` →
//! [`crate::wezterm_pipeline::WeztermPipeline`]. No second font system,
//! no second atlas, no second render pass.

use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use sonicterm_render_model::boundary::cfg::config::{
    BackdropKind, ScrollbarMode, SoftwareRenderMode,
};
use sonicterm_render_model::boundary::cfg::theme::{Color as ThemeColor, Theme};
use sonicterm_render_model::boundary::grid::grid::{
    bounded_grid_size, Cell, CellFlags, Color, Grid, UnderlineStyle,
};
use sonicterm_types::{GlyphRasterVariant, ResourceAmount, ResourceClass};
use wgpu::{
    CommandEncoderDescriptor, CompositeAlphaMode, DeviceDescriptor, Instance, InstanceDescriptor,
    LoadOp, Operations, PresentMode, RenderPassColorAttachment, RenderPassDescriptor,
    RequestAdapterOptions, SurfaceConfiguration, Texture, TextureDescriptor, TextureDimension,
    TextureFormat, TextureUsages, TextureView, TextureViewDescriptor,
};
use winit::{event_loop::ActiveEventLoop, window::Window};

use crate::chrome_text::{self, ChromeAttrs, ChromeClip};
use crate::color::{
    chrome_color_to_linear_rgba, dim_toward, hex_to_chrome_color, hex_to_premultiplied_rgba,
    hex_to_wgpu_with_alpha, ChromeColor,
};
use crate::cursor::{recolor_cursor_glyphs, InactivePaneCursor};
use sonicterm_render_model::boundary::ui::drag_chip::{DragChipOverlay, DragChipVisual};
use sonicterm_render_model::boundary::ui::tab_spans::tab_title_font_size;

const PANE_FOCUS_FLASH_DURATION: Duration = Duration::from_millis(360);
const PANE_FOCUS_FLASH_BUCKET: Duration = Duration::from_millis(16);

fn effective_scrollbar_bucket(
    mode: ScrollbarMode,
    scrollback_len: usize,
    viewport_rows: u16,
    alpha: f32,
) -> u16 {
    if matches!(mode, ScrollbarMode::Never)
        || scrollback_len == 0
        || viewport_rows == 0
        || alpha <= sonicterm_render_model::boundary::ui::scrollbar::ALPHA_EMIT_FLOOR
    {
        // When: no scrollbar pixels can be emitted, all equivalent states share bucket zero.
        return 0;
    }
    (alpha.clamp(0.0, 1.0) * f32::from(u16::MAX)).round() as u16
}

fn pane_scrollbar_identity<I>(mode: ScrollbarMode, panes: I) -> Vec<(u64, u16)>
where
    I: IntoIterator<Item = (u64, usize, u16, f32)>,
{
    let mut identity: Vec<_> = panes
        .into_iter()
        .map(|(pane_id, scrollback_len, viewport_rows, alpha)| {
            (pane_id, effective_scrollbar_bucket(mode, scrollback_len, viewport_rows, alpha))
        })
        .collect();
    identity.sort_unstable_by_key(|(pane_id, _)| *pane_id);
    identity
}

fn pane_focus_flash_sample(elapsed: Duration) -> Option<(u8, f32)> {
    if elapsed >= PANE_FOCUS_FLASH_DURATION {
        // When: `elapsed` reaches the bounded lifetime, no flash frame remains.
        return None;
    }
    let bucket = ((elapsed.as_millis() / PANE_FOCUS_FLASH_BUCKET.as_millis()) + 1)
        .min(u128::from(u8::MAX)) as u8;
    let t = elapsed.as_secs_f32() / PANE_FOCUS_FLASH_DURATION.as_secs_f32();
    Some((bucket, (1.0 - t).powi(2) * 0.12))
}

fn hovered_url_needs_accent(
    hovered: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
) -> bool {
    hovered.is_some_and(|h| h.active)
}

fn hovered_url_for_pane_row(
    hovered: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
    pane_id: u64,
    row: u16,
) -> Option<sonicterm_render_model::inputs::HoveredUrlCells> {
    let hovered = hovered.filter(|hovered| hovered.pane_id == pane_id)?;
    let span = hovered.span_for_row(row)?;
    sonicterm_render_model::inputs::HoveredUrlCells::new(pane_id, [span], hovered.active)
}

fn hovered_url_row_cache_key(
    key: u64,
    hovered: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
    row: u16,
) -> u64 {
    let Some(hovered) = hovered.filter(|hovered| hovered.active) else {
        // When: `hovered` is absent or inactive, glyph colors match the ordinary row cache entry.
        return key;
    };
    let Some(span) = hovered.span_for_row(row) else {
        // When: `hovered` has no `row` fragment, this row's glyph colors are unchanged.
        return key;
    };
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    0x55_524C_u64.hash(&mut hasher); // "URL" salt
    span.start_col.hash(&mut hasher);
    span.end_col.hash(&mut hasher);
    hasher.finish()
}

#[allow(clippy::too_many_arguments)]
fn hovered_url_span_rect(
    span: sonicterm_render_model::inputs::HoveredUrlSpan,
    cols: u16,
    rows: u16,
    origin_x: f32,
    origin_y: f32,
    cell_w: f32,
    cell_h: f32,
    snapped_cell_x: &[f32],
) -> Option<(f32, f32, f32, f32)> {
    if cols == 0 || span.row >= rows || span.start_col >= cols || span.end_col <= span.start_col {
        // When: `span` has no visible row or column coverage, emit no underline rectangle.
        return None;
    }
    let start_col = span.start_col as usize;
    let end_col = span.end_col.min(cols) as usize;
    let x = snapped_cell_x
        .get(start_col)
        .copied()
        .unwrap_or(origin_x + f32::from(span.start_col) * cell_w);
    let width = snapped_cell_x
        .get(end_col)
        .map(|right| right - x)
        .unwrap_or_else(|| f32::from(span.end_col.min(cols) - span.start_col) * cell_w);
    (width > 0.0).then_some((x, origin_y + f32::from(span.row) * cell_h, width, cell_h))
}

fn cursor_char_slice_at(text: &str, cursor: usize) -> Option<&str> {
    if text.is_empty() || cursor >= text.len() {
        // When: `cursor >= text.len()` — caret past the last char. No char to
        // slice, so the caller falls back to its placeholder.
        return None;
    }
    let mut c = cursor.min(text.len());
    while c > 0 && !text.is_char_boundary(c) {
        c -= 1;
    }
    let ch = text[c..].chars().next()?;
    Some(&text[c..c + ch.len_utf8()])
}

fn palette_cursor_char<'a>(
    query: &'a str,
    cursor: usize,
    placeholder: Option<&'a str>,
) -> Option<&'a str> {
    cursor_char_slice_at(query, cursor).or_else(|| {
        query
            .is_empty()
            .then(|| placeholder.and_then(|text| cursor_char_slice_at(text, 0)))
            .flatten()
    })
}

fn palette_footer_font_size(body_font_size: f32) -> f32 {
    (body_font_size - 1.0).max(1.0)
}

const PALETTE_FOOTER_INSET_X: f32 = 12.0;
const READ_ONLY_BADGE_ICON: &str = "";
const READ_ONLY_BADGE_LABEL: &str = "READONLY";
const SEARCH_BADGE_ICON: &str = "";
const NOTIFICATION_CLOSE_ICON: &str = "";
const READ_ONLY_BADGE_W: f32 = 250.0;
const READ_ONLY_BADGE_H: f32 = SEARCH_BAR_HEIGHT;
const READ_ONLY_BADGE_MARGIN: f32 = 12.0;
const READ_ONLY_BADGE_PAD_RIGHT: f32 = 15.0;
const READ_ONLY_BADGE_BASELINE_NUDGE_Y: f32 = -2.0;
const READ_ONLY_BADGE_RADIUS: f32 = 7.0;

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

fn estimate_badge_text_width(text: &str, font_size: f32) -> f32 {
    text.chars().map(|ch| if ch.is_ascii() { 0.58 } else { 1.0 }).sum::<f32>() * font_size
}

fn conservative_badge_text_width(fallback: f32, shaped: Option<f32>) -> f32 {
    shaped.filter(|width| width.is_finite() && *width >= 0.0).unwrap_or(fallback).max(fallback)
}

fn search_badge_content_width(
    icon: &str,
    label: &str,
    font_size: f32,
    gap: f32,
    font_stack: Option<&sonicterm_engine::FontStack>,
) -> f32 {
    let fallback = estimate_badge_text_width(icon, font_size)
        + gap
        + estimate_badge_text_width(label, font_size);
    let shaped = font_stack.and_then(|stack| {
        let icon_w = stack.measure_text_width(icon).ok()?;
        let label_w = stack.measure_text_width(label).ok()?;
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
fn preedit_has_visible_ink(preedit: &str) -> bool {
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
fn preedit_caret_advance(preedit: &str, caret_byte: usize, font_size: f32) -> f32 {
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
fn preedit_bg_rect(
    start_x: f32,
    top_y: f32,
    pre_w: f32,
    pad: f32,
    line_h: f32,
) -> (f32, f32, f32, f32) {
    (start_x, top_y, pre_w + pad, line_h)
}

/// Renderer initialization settings derived from config.
#[derive(Debug, Clone, Copy)]
pub struct RendererSettings<'a> {
    /// Font family to use for terminal text.
    pub font_family: &'a str,
    /// Packaged directories searched before platform-native font discovery.
    pub font_dirs: &'a [PathBuf],
    /// Font size in points.
    pub font_size: f32,
    /// Line-height multiplier.
    pub line_height_mult: f32,
    /// Regular-text coverage scale.
    pub font_weight_scale: f32,
    /// Window padding in logical pixels: left, right, top, bottom.
    pub padding: [f32; 4],
    /// Surface/backdrop settings.
    pub appearance: SurfaceAppearance,
    /// Stable renderer role used by memory and timing diagnostics.
    pub role: &'static str,
}

fn cursor_color_from_theme(theme: &Theme) -> [f32; 4] {
    hex_to_premultiplied_rgba(theme.colors.cursor.0.as_str(), 1.0)
}

fn cursor_text_color_from_theme(theme: &Theme) -> [f32; 4] {
    hex_to_premultiplied_rgba(theme.colors.cursor_text.0.as_str(), 1.0)
}

fn active_cursor_color(base: [f32; 4], _shape: CursorShape, _blink_alpha: f32) -> [f32; 4] {
    base
}

pub(crate) fn glyph_flags(is_color: bool, is_subpixel: bool) -> [f32; 4] {
    [if is_color { 1.0 } else { 0.0 }, if is_subpixel { 1.0 } else { 0.0 }, 0.0, 0.0]
}

fn effective_font_weight_scale(scale: f32) -> f32 {
    if scale.is_finite() && (0.5..=5.0).contains(&scale) {
        scale
    } else {
        // When: `scale` is NaN, infinite, or outside 0.5..=5.0. 1.0 is the
        // weight the font was drawn at, so a bad config still renders.
        1.0
    }
}

struct RendererFontStacks {
    body: Option<sonicterm_engine::FontStack>,
    tab_title: Option<sonicterm_engine::FontStack>,
    palette_footer: Option<sonicterm_engine::FontStack>,
}

fn renderer_font_stacks(
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
    let tab_title =
        body.as_ref().map(|stack| stack.with_font_size(f64::from(tab_title_font_size(body_size))));
    let palette_footer = body
        .as_ref()
        .map(|stack| stack.with_font_size(f64::from(palette_footer_font_size(body_size))));
    RendererFontStacks { body, tab_title, palette_footer }
}

fn software_block_glyph_target_rect(
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
) -> (f32, f32, f32, f32) {
    let left = (left - 0.5).ceil();
    let top = (top - 0.5).ceil();
    let right = (right - 0.5).ceil().max(left + 1.0);
    let bottom = (bottom - 0.5).ceil().max(top + 1.0);
    (left, top, right - left, bottom - top)
}

/// Apply HarfBuzz positioning while preserving the raster tile's size.
fn positioned_shaped_glyph_rect(
    natural: (f32, f32, f32, f32),
    x_offset: f32,
    y_offset: f32,
) -> (f32, f32, f32, f32) {
    (natural.0 + x_offset, natural.1 + y_offset, natural.2, natural.3)
}

/// Resolve a shaped glyph's horizontal offset from its cluster-local running pen.
fn shaped_cluster_x_offset(
    prior_col: &mut Option<u16>,
    pen_x: &mut f32,
    glyph: &sonicterm_text::shape::ShapedGlyph,
) -> f32 {
    if *prior_col != Some(glyph.lead_col) {
        // A new terminal cluster anchors its pen to the lead cell instead of carrying the
        // preceding cluster's accumulated advance.
        *prior_col = Some(glyph.lead_col);
        *pen_x = 0.0;
    }
    let offset = *pen_x + glyph.x_offset;
    *pen_x += glyph.x_advance;
    offset
}

/// Reserve vertical retained-damage margin for native glyph bearings and GPOS offsets.
fn terminal_vertical_ink_pad(cell_h: f32, metrics: Option<sonicterm_engine::CellMetricsPx>) -> f32 {
    metrics.map(|metrics| metrics.cell_h as f32).unwrap_or(cell_h).max(0.0).ceil()
}

/// Normalizes standalone Claude Code circle markers inside one cell without distortion.
pub(crate) fn fit_single_cell_status_marker(
    ch: char,
    cluster_cells: usize,
    is_wide: bool,
    has_extras: bool,
    natural: (f32, f32, f32, f32),
    cell: (f32, f32, f32, f32),
) -> (f32, f32, f32, f32) {
    // When: ch is not approved, cluster_cells is not one, is_wide is true, or
    // has_extras is true, preserve the font rasterizer's geometry exactly.
    if !matches!(ch, '\u{23fa}' | '\u{25ef}' | '\u{25cf}')
        || cluster_cells != 1
        || is_wide
        || has_extras
    {
        return natural;
    }

    let (_, _, glyph_w, glyph_h) = natural;
    let (cell_x, cell_y, cell_w, cell_h) = cell;
    // When: glyph_w, glyph_h, cell_w, or cell_h is non-positive, no meaningful
    // fit ratio exists, so preserve the natural rectangle.
    if glyph_w <= 0.0 || glyph_h <= 0.0 || cell_w <= 0.0 || cell_h <= 0.0 {
        return natural;
    }

    let scale = (cell_w / glyph_w).min(cell_h / glyph_h);
    let fitted_w = glyph_w * scale;
    let fitted_h = glyph_h * scale;
    (cell_x + (cell_w - fitted_w) * 0.5, cell_y + (cell_h - fitted_h) * 0.5, fitted_w, fitted_h)
}

/// Whether global or per-tab privilege requires the independent lock badge.
fn tab_requires_privilege_badge(process_privileged: bool, foreground_privileged: bool) -> bool {
    process_privileged || foreground_privileged
}

fn tab_bar_hash(tabs: &TabBar, now: Instant) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hash = DefaultHasher::new();
    tabs.active_index().hash(&mut hash);
    for tab in tabs.tabs() {
        tab.id.0.hash(&mut hash);
        tab.title.hash(&mut hash);
        tab.custom_color.hash(&mut hash);
        tab.foreground_privileged.hash(&mut hash);
        command_status_hash(&tab.command, now).hash(&mut hash);
    }
    hash.finish()
}

/// Resolve the title text colour for a single tab, honouring hover.
///
/// Default tabs use the active foreground when active or hovered. Custom
/// colours stay full-strength while active, hovered, or panel-focused and
/// otherwise recede to the standard unfocused alpha.
fn tab_title_color(
    custom_color: Option<&str>,
    active: bool,
    hovered: bool,
    active_panel_focused: bool,
    active_fg: ChromeColor,
    inactive_fg: ChromeColor,
) -> ChromeColor {
    match custom_color {
        None => {
            if active || hovered {
                active_fg
            } else {
                // When: neither `active` nor `hovered`, so nothing highlights
                // the tab and it recedes to the dimmer foreground.
                inactive_fg
            }
        }
        Some(hex) => {
            let color = hex_to_chrome_color(hex);
            // Full strength when the tab is highlighted (active/hovered) or
            // lives in the focused panel; otherwise recede with the rest of
            // the unfocused chrome.
            if active || hovered || active_panel_focused {
                color
            } else {
                // When: not `active`, `hovered`, or `active_panel_focused` — the
                // user's colour recedes with the rest of the unfocused chrome.
                scale_chrome_text_alpha(color, 0.55)
            }
        }
    }
}

const PRIVILEGE_BADGE_SIZE_PX: f32 = 18.0;
const PRIVILEGE_BADGE_GAP_PX: f32 = 6.0;
#[cfg(test)]
const PRIVILEGE_BADGE_QUAD_COUNT: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq)]
struct TabTitleBlockPlacement {
    badge_rect: Option<TabTitleRect>,
    text_x: f32,
    text_clip: TabTitleRect,
}

fn scaled_privilege_badge_metrics(scale: f32) -> (f32, f32) {
    let scale = scale.max(0.1);
    (PRIVILEGE_BADGE_SIZE_PX * scale, PRIVILEGE_BADGE_GAP_PX * scale)
}

fn tab_title_capacity(rect: TabTitleRect, avg_glyph_w: f32, privileged: bool, scale: f32) -> usize {
    let reserved = if privileged {
        let (badge, gap) = scaled_privilege_badge_metrics(scale);
        badge + gap
    } else {
        // When: `privileged` is false, preserve the full historical title width.
        0.0
    };
    (((rect.w - reserved).max(0.0) / avg_glyph_w.max(1.0)).floor() as usize)
        .max(usize::from(!privileged))
}

fn tab_title_display_text(title: &str, command_badge: Option<&str>, max_chars: usize) -> String {
    let Some(badge) = command_badge else {
        // When: `command_badge` is None, all character capacity belongs to the stored title.
        return truncate_title_body(title, max_chars);
    };
    let badge_chars = badge.chars().count() + 1;
    if badge_chars >= max_chars {
        // When: `badge_chars >= max_chars`, keep the leading status rather than partial title text.
        return truncate_title_body(badge, max_chars);
    }
    format!("{badge} {}", truncate_title_body(title, max_chars - badge_chars))
}

fn tab_title_block_placement(
    rect: TabTitleRect,
    measured_text_width: f32,
    privileged: bool,
    scale: f32,
) -> TabTitleBlockPlacement {
    if !privileged {
        // When: `!privileged`, center text in the unchanged historical title rectangle.
        let text_x = rect.x + ((rect.w - measured_text_width) * 0.5).max(0.0);
        return TabTitleBlockPlacement { badge_rect: None, text_x, text_clip: rect };
    }
    let (badge_size, gap) = scaled_privilege_badge_metrics(scale);
    let badge_size = badge_size.min(rect.h).min(rect.w.max(0.0));
    let total_width = (badge_size + gap + measured_text_width).min(rect.w.max(0.0));
    let block_x = rect.x + ((rect.w - total_width) * 0.5).max(0.0);
    let badge_y = rect.y + ((rect.h - badge_size) * 0.5).max(0.0);
    let badge_rect = TabTitleRect { x: block_x, y: badge_y, w: badge_size, h: badge_size };
    let text_x = (badge_rect.x + badge_rect.w + gap).min(rect.x + rect.w);
    let text_clip =
        TabTitleRect { x: text_x, y: rect.y, w: (rect.x + rect.w - text_x).max(0.0), h: rect.h };
    TabTitleBlockPlacement { badge_rect: Some(badge_rect), text_x, text_clip }
}

fn privilege_lock_color(danger: [f32; 4]) -> [f32; 4] {
    if danger[0] * 0.2126 + danger[1] * 0.7152 + danger[2] * 0.0722 > 0.179 {
        [0.0, 0.0, 0.0, danger[3]]
    } else {
        // When: danger luminance is at most 0.179, white lock geometry has stronger contrast.
        [danger[3], danger[3], danger[3], danger[3]]
    }
}

#[cfg(test)]
fn linear_contrast_ratio(a: [f32; 4], b: [f32; 4]) -> f32 {
    let luminance = |color: [f32; 4]| color[0] * 0.2126 + color[1] * 0.7152 + color[2] * 0.0722;
    let (brighter, darker) = {
        let a = luminance(a);
        let b = luminance(b);
        if a >= b {
            (a, b)
        } else {
            // When: `a < b`, use `b` as the brighter luminance in the contrast ratio.
            (b, a)
        }
    };
    (brighter + 0.05) / (darker + 0.05)
}

fn privilege_lock_rects(badge: TabTitleRect) -> [TabTitleRect; 4] {
    let unit = badge.w.min(badge.h) / 18.0;
    let x = badge.x;
    let y = badge.y;
    [
        TabTitleRect { x: x + 5.0 * unit, y: y + 3.0 * unit, w: 2.0 * unit, h: 6.0 * unit },
        TabTitleRect { x: x + 11.0 * unit, y: y + 3.0 * unit, w: 2.0 * unit, h: 6.0 * unit },
        TabTitleRect { x: x + 7.0 * unit, y: y + 3.0 * unit, w: 4.0 * unit, h: 2.0 * unit },
        TabTitleRect { x: x + 4.0 * unit, y: y + 8.0 * unit, w: 10.0 * unit, h: 7.0 * unit },
    ]
}

fn emit_privilege_badge_quads(
    quads: &mut Vec<QuadInstance>,
    badge: TabTitleRect,
    danger: [f32; 4],
    alpha: f32,
    surface: (f32, f32),
) {
    let alpha = alpha.clamp(0.0, 1.0);
    let scale = |mut color: [f32; 4]| {
        for channel in &mut color {
            *channel *= alpha;
        }
        color
    };
    let (sw, sh) = surface;
    let lock = scale(privilege_lock_color(danger));
    let danger = scale(danger);
    quads.push(QuadInstance::rounded(
        px_to_ndc(badge.x, badge.y, badge.w, badge.h, sw, sh),
        danger,
        [badge.w, badge.h],
        badge.w.min(badge.h) * 0.25,
    ));
    for part in privilege_lock_rects(badge) {
        quads.push(QuadInstance::sharp(px_to_ndc(part.x, part.y, part.w, part.h, sw, sh), lock));
    }
}

fn splitter_color_from_theme(theme: &Theme) -> [f32; 4] {
    let bg = theme.colors.background.color().unwrap_or_else(|| ThemeColor::rgb(0, 0, 0));
    let fg = theme.colors.foreground.color().unwrap_or_else(|| ThemeColor::rgb(255, 255, 255));
    bg.shift_toward(fg, 0.18).to_rgba_f32_linear(1.0)
}

/// Resolve a scrollbar tint from the theme foreground at `derived_alpha`.
/// Theme-customizable explicit scrollbar colors are intentionally not
/// supported: they would require updating ~50 `Palette { .. }` literals in
/// tests for no shipped benefit. Returns premultiplied linear RGBA.
fn scrollbar_tint(fg: &str, derived_alpha: f32) -> [f32; 4] {
    hex_to_premultiplied_rgba(fg, derived_alpha)
}

fn read_only_badge_rect(sw: f32, sh: f32, scale: f32, content_w: f32) -> (f32, f32, f32, f32) {
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

/// Classify a wgpu adapter as a software (CPU) rasterizer.
///
/// True when the adapter is a CPU device, or its name matches a known
/// software rasterizer (Microsoft WARP, Mesa llvmpipe, Google SwiftShader).
/// Used to drive the no-GPU degrade path. Pure fn over the
/// adapter info so it is unit-testable without a live GPU.
#[must_use]
pub fn detect_software_rendering(info: &wgpu::AdapterInfo) -> bool {
    software_rendering_from(&info.name, info.device_type)
}

/// Inner predicate over just the adapter name + device type, so it can be
/// unit-tested without building a full `wgpu::AdapterInfo` (which has no
/// `Default`).
#[must_use]
fn software_rendering_from(name: &str, device_type: wgpu::DeviceType) -> bool {
    if device_type == wgpu::DeviceType::Cpu {
        // When: `device_type` is `Cpu` — authoritative, so no name match is
        // needed. The string tests below cover rasterizers reporting otherwise.
        return true;
    }
    let name = name.to_ascii_lowercase();
    name.contains("microsoft basic render driver")
        || name.contains("llvmpipe")
        || name.contains("swiftshader")
        || name.contains("software adapter")
}

fn software_render_degrade_from(mode: SoftwareRenderMode, detected: bool) -> bool {
    match mode {
        SoftwareRenderMode::Auto => detected,
        SoftwareRenderMode::Force => true,
        SoftwareRenderMode::Off => false,
    }
}

/// Memory-allocation strategy selected for a wgpu device.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceMemoryPolicy {
    /// Favor lower allocator reserve on software adapters.
    MemoryUsage,
    /// Favor rendering performance on hardware adapters.
    Performance,
}

/// Select the device memory policy for an adapter classification.
///
/// Software adapters minimize allocator reserve; hardware adapters retain
/// wgpu's performance-oriented policy.
#[doc(hidden)]
#[must_use]
pub fn device_memory_policy_from(software_rendering: bool) -> DeviceMemoryPolicy {
    match software_rendering {
        true => DeviceMemoryPolicy::MemoryUsage,
        false => DeviceMemoryPolicy::Performance,
    }
}

/// Build the sole wgpu device descriptor used by the renderer.
///
/// Starting from wgpu defaults changes only `memory_hints`, keeping policy
/// selection independent from feature and limit negotiation.
#[doc(hidden)]
#[must_use]
pub fn device_descriptor_for(software_rendering: bool) -> DeviceDescriptor<'static> {
    let memory_hints = match device_memory_policy_from(software_rendering) {
        DeviceMemoryPolicy::MemoryUsage => wgpu::MemoryHints::MemoryUsage,
        DeviceMemoryPolicy::Performance => wgpu::MemoryHints::Performance,
    };
    DeviceDescriptor { memory_hints, ..DeviceDescriptor::default() }
}

/// Aggregate allocator usage without retaining allocation labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocatorSnapshot {
    /// Bytes occupied by live GPU allocations.
    pub allocated_bytes: u64,
    /// Bytes reserved by GPU memory blocks.
    pub reserved_bytes: u64,
    /// Number of live GPU allocations.
    pub allocations: u32,
    /// Number of reserved GPU memory blocks.
    pub blocks: u32,
    /// Size of the largest reserved GPU memory block.
    pub largest_block_bytes: u64,
}

/// Summarize a wgpu allocator report without reading allocation names.
#[doc(hidden)]
#[must_use]
pub fn allocator_snapshot_from(report: &wgpu::AllocatorReport) -> AllocatorSnapshot {
    AllocatorSnapshot {
        allocated_bytes: report.total_allocated_bytes,
        reserved_bytes: report.total_reserved_bytes,
        allocations: u32::try_from(report.allocations.len()).unwrap_or(u32::MAX),
        blocks: u32::try_from(report.blocks.len()).unwrap_or(u32::MAX),
        largest_block_bytes: report.blocks.iter().map(|block| block.size).max().unwrap_or(0),
    }
}

/// Preserve report unavailability while projecting an available report to scalar counters.
fn allocator_snapshot_from_report(
    report: Option<wgpu::AllocatorReport>,
) -> Option<AllocatorSnapshot> {
    report.as_ref().map(allocator_snapshot_from)
}

/// Emit a pane's scrollbar (track + thumb) into `quads_overlay` using the
/// shared geometry model. No-op when the pane has nothing to scroll, the
/// mode is `Never`, or `alpha` is at or below the emit floor.
/// Returns the number of quads emitted (for tests).
///
/// `alpha` in `[0.0, 1.0]` scales both track + thumb tint alphas; the
/// caller (app loop) feeds the lerped per-pane fade value from
/// `sonicterm_app::app::scrollbar_visibility::tick`.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn emit_pane_scrollbar(
    quads_overlay: &mut Vec<QuadInstance>,
    pane_rect: PaneRect,
    viewport_rows: u16,
    total_rows: u64,
    view_top: u64,
    mode: sonicterm_render_model::boundary::cfg::config::ScrollbarMode,
    theme: &Theme,
    sw: f32,
    sh: f32,
    alpha: f32,
    scale: f32,
) -> usize {
    if alpha <= sonicterm_render_model::boundary::ui::scrollbar::ALPHA_EMIT_FLOOR {
        // When: `alpha` is at the fade floor where the bar is invisible —
        // emitting costs two quads per pane per frame for unseeable pixels.
        return 0;
    }
    let alpha = alpha.clamp(0.0, 1.0);
    // Bar width in raster px. Authored at 8 logical px; scale with DPI so the
    // bar keeps a constant physical size across displays, min 1px.
    let scrollbar_width_px: f32 = (8.0 * scale).max(1.0);
    let geom_rect = sonicterm_render_model::boundary::ui::scrollbar::Rect::new(
        pane_rect.x,
        pane_rect.y,
        pane_rect.w,
        pane_rect.h,
    );
    let Some(geom) = sonicterm_render_model::boundary::ui::scrollbar::compute(
        viewport_rows,
        total_rows,
        view_top,
        geom_rect,
        mode,
        scrollbar_width_px,
    ) else {
        // When: `scrollbar::compute` yields None — nothing beyond the viewport
        // to scroll, or mode is `Never`. No track or thumb to place.
        return 0;
    };
    let fg_hex = theme.colors.foreground.0.as_str();
    let track_color = scrollbar_tint(fg_hex, 0.10 * alpha);
    let thumb_color = scrollbar_tint(fg_hex, 0.30 * alpha);
    quads_overlay.push(QuadInstance::sharp(
        px_to_ndc(
            geom.track_rect.x,
            geom.track_rect.y,
            geom.track_rect.w,
            geom.track_rect.h,
            sw,
            sh,
        ),
        track_color,
    ));
    quads_overlay.push(QuadInstance::sharp(
        px_to_ndc(
            geom.thumb_rect.x,
            geom.thumb_rect.y,
            geom.thumb_rect.w,
            geom.thumb_rect.h,
            sw,
            sh,
        ),
        thumb_color,
    ));
    2
}

fn splitter_rects_from_panes(pane_rects: &[(u64, PaneRect)], thickness: f32) -> Vec<SplitterRect> {
    let mut out = Vec::new();
    let thickness = thickness.max(0.0);
    let eps = 0.5_f32;

    for (i, (_, a)) in pane_rects.iter().enumerate() {
        for (_, b) in pane_rects.iter().skip(i + 1) {
            let vertical_overlap = a.y.max(b.y) < (a.y + a.h).min(b.y + b.h) - eps;
            if vertical_overlap && ((a.x + a.w) - b.x).abs() <= eps {
                let y = a.y.max(b.y);
                let h = (a.y + a.h).min(b.y + b.h) - y;
                out.push(SplitterRect {
                    axis: SplitAxis::Vertical,
                    rect: PaneRect::new(b.x - thickness * 0.5, y, thickness, h),
                });
            } else if vertical_overlap && ((b.x + b.w) - a.x).abs() <= eps {
                // When: `(b.x + b.w) - a.x` is the contact instead — the pair
                // walk is unordered, so the splitter straddles a's left edge.
                let y = a.y.max(b.y);
                let h = (a.y + a.h).min(b.y + b.h) - y;
                out.push(SplitterRect {
                    axis: SplitAxis::Vertical,
                    rect: PaneRect::new(a.x - thickness * 0.5, y, thickness, h),
                });
            }

            let horizontal_overlap = a.x.max(b.x) < (a.x + a.w).min(b.x + b.w) - eps;
            if horizontal_overlap && ((a.y + a.h) - b.y).abs() <= eps {
                let x = a.x.max(b.x);
                let w = (a.x + a.w).min(b.x + b.w) - x;
                out.push(SplitterRect {
                    axis: SplitAxis::Horizontal,
                    rect: PaneRect::new(x, b.y - thickness * 0.5, w, thickness),
                });
            } else if horizontal_overlap && ((b.y + b.h) - a.y).abs() <= eps {
                // When: `(b.y + b.h) - a.y` is the contact — mirrored pair
                // order, so the splitter straddles a's top edge.
                let x = a.x.max(b.x);
                let w = (a.x + a.w).min(b.x + b.w) - x;
                out.push(SplitterRect {
                    axis: SplitAxis::Horizontal,
                    rect: PaneRect::new(x, a.y - thickness * 0.5, w, thickness),
                });
            }
        }
    }

    out
}

use crate::{
    atlas_upload::{AtlasPixelEncoding, AtlasUpload, AtlasUploadStats},
    quad::{
        premultiply, px_to_ndc, scale_premultiplied_alpha, with_premultiplied_alpha, QuadInstance,
    },
    wezterm_pipeline::WeztermPipeline,
};
use sonicterm_render_model::boundary::cfg::config::CursorShape;
use sonicterm_render_model::boundary::ui::{
    command_palette::CommandPalette,
    copy_mode::{CopyModeState, QuickSelectState},
    cursor as ui_cursor,
    ime::ImeState,
    overlays::{
        command_palette_query_label, search_bar_label, search_query_caret_prefix,
        NotificationBubble, NotificationBubbleLayout, NotificationLevel, PaletteLayout,
        SearchBarLayout, PALETTE_BORDER, PALETTE_PANEL_RADIUS, PALETTE_QUERY_RADIUS,
        PALETTE_ROW_RADIUS, SEARCH_BAR_HEIGHT, SEARCH_BAR_ICON_GAP, SEARCH_BAR_PAD_LEFT,
        SEARCH_BAR_PAD_RIGHT,
    },
    pane::{Rect as PaneRect, SplitAxis, SplitterRect},
    search::SearchState,
    selection::Selection,
    tabbar_view::{
        tab_bar_height, Rect as TabTitleRect, TabBarLayout, ACTIVE_TOP_ACCENT_H,
        ACTIVE_TOP_ACCENT_INSET, TAB_BAR_HEIGHT, TAB_GAP, TAB_VERT_INSET,
    },
    tabs::{truncate_title_body, TabBar},
};
use sonicterm_render_model::geometry::{DamageRect, PixelRect};
use sonicterm_text::GlyphInstance;
use sonicterm_text::{
    glyph_atlas::GlyphAtlas,
    // `shape_run` + `ShapeCache` deleted in
    // T8 (the cosmic-text adapter is gone). `flush_shape_run` now drives
    // `shape_run_with_wezterm` directly; `ShapedGlyph::from_wezterm`
    // narrows wezterm's `GlyphInfo` into the renderer-facing record.
    // The legacy ASCII fast-path gate (`run_is_ascii_fast`) still
    // applies — it's purely cell-shape based and not tied to shaper
    // choice.
    //
    // `swash_rasterizer` is no longer
    // imported here — every chrome site and the grid path both route
    // through `sonicterm_engine::FontStack`. T10 deletes the
    // file outright.
    shape::{run_is_ascii_fast, RunStyle},
};

#[cfg(test)]
#[must_use]
#[allow(clippy::too_many_arguments)]
fn dirty_rows_damage_rect<I>(
    dirty_rows: I,
    pane_rect: PixelRect,
    origin_x: f32,
    origin_y: f32,
    cols: u16,
    cell_w: f32,
    cell_h: f32,
    surface_w: u32,
    surface_h: u32,
) -> Option<PixelRect>
where
    I: IntoIterator<Item = usize>,
{
    dirty_rows_damage_rect_with_ink_pad(
        dirty_rows, pane_rect, origin_x, origin_y, cols, cell_w, cell_h, 0.0, surface_w, surface_h,
    )
}

#[must_use]
#[allow(clippy::too_many_arguments)]
fn dirty_rows_damage_rect_with_ink_pad<I>(
    dirty_rows: I,
    pane_rect: PixelRect,
    origin_x: f32,
    origin_y: f32,
    cols: u16,
    cell_w: f32,
    cell_h: f32,
    vertical_ink_pad: f32,
    surface_w: u32,
    surface_h: u32,
) -> Option<PixelRect>
where
    I: IntoIterator<Item = usize>,
{
    if cols == 0 || cell_w <= 0.0 || cell_h <= 0.0 || surface_w == 0 || surface_h == 0 {
        // When: `cols` is 0, a cell metric is non-positive, or the surface is
        // degenerate — a rect from these would name pixels that do not exist.
        return None;
    }
    let bounds = PixelRect { x: 0, y: 0, w: surface_w, h: surface_h };
    let pane_bounds = pane_rect.intersect(bounds)?;
    let mut damage = DamageRect::empty();
    // Span from the floored left edge to the CEILED right edge, for the same
    // reason the row height below spans to the ceiled top of the next row.
    // `x` floors, which moves the strip left by the fractional part of
    // `origin_x`; a width taken from the cell count alone never gets that
    // fraction back, so the strip ends short of where the pane actually
    // reaches. A glyph that painted into that last column is then never
    // repainted when its cell is cleared, and survives as a stray mark on an
    // otherwise empty row.
    let left = origin_x.floor() as i32;
    let right = (origin_x + cols as f32 * cell_w).ceil() as i32;
    // Widen each row strip to the pane's own edges where the pane reaches
    // further than the cell grid. The difference is the padding band, and
    // glyph ink can land in it: a negative left side bearing at column 0
    // paints left of its cell. A strip that stops at the content edge never
    // repaints those columns, so such a pixel survives every later frame.
    let left = left.min(pane_bounds.x);
    let right = right.max(pane_bounds.x + pane_bounds.w as i32);
    let row_w = (right - left).max(1) as u32;
    for row in dirty_rows {
        let x = left;
        // Span each dirty row from its floored top edge to the CEILED top edge
        // of the NEXT row. With a fractional `cell_h` (common at fractional
        // DPI), a fixed `ceil(cell_h)` height starting at `floor(top)` can fall
        // one physical pixel short of where the next row begins, leaving the
        // boundary pixel un-repainted. A full-cell glyph/inverse block (e.g.
        // zsh's reverse-video PROMPT_EOL_MARK `%`) paints into that pixel, so
        // when the cell is later cleared but only this row is dirty, the bottom
        // 1px of the old block survives as a stray underline-like mark.
        // Covering through the next row's top edge closes the rounding seam.
        let ink_pad = vertical_ink_pad.max(0.0);
        let top = (origin_y + row as f32 * cell_h - ink_pad).floor() as i32;
        let next_top = (origin_y + (row as f32 + 1.0) * cell_h + ink_pad).ceil() as i32;
        let row_h = (next_top - top).max(1) as u32;
        damage.add_clipped(PixelRect { x, y: top, w: row_w, h: row_h }, pane_bounds);
    }
    damage.rect()
}

/// Decide a pane's per-frame damage rectangle, given whether the pane is
/// showing the alternate screen.
///
/// A hardware surface keeps the previous frame's pixels, so damage-limited
/// repainting only redraws the rows the grid marked dirty. That is correct
/// for a normal shell pane: a changed prompt line is a narrow edit and
/// leaving the surrounding rows untouched is exactly what we want. It is
/// WRONG for an alternate-screen app (vim/nvim/less/tmux). Those apps
/// scroll, split, and repaint regions such that a row which was NOT
/// re-emitted this frame can still be visually stale — the app moved
/// content out from under it. For an alt-screen pane we therefore repaint
/// the pane's whole clipped rectangle whenever ANY row is dirty, and
/// nothing when the pane is clean.
///
/// Returns:
/// * `None` for an alt-screen pane with no dirty rows (clean -> no repaint).
/// * the full pane rectangle clipped to the surface for a dirty alt-screen
///   pane — a complete pane repaint, never an unconditional full-window one.
/// * the narrow, glyph-padded dirty-row union ([`dirty_rows_damage_rect_with_ink_pad`])
///   for a normal-screen pane.
///
/// The alt-screen decision is independent of cell metrics, so sparse /
/// scattered dirty rows and fractional cell heights all resolve to the same
/// complete-pane repaint; the surface clip both bounds the rect to on-screen
/// pixels and rejects a fully off-surface pane.
#[cfg(test)]
#[must_use]
#[allow(clippy::too_many_arguments)]
fn pane_damage_rect<I>(
    is_alt: bool,
    dirty_rows: I,
    pane_rect: PixelRect,
    origin_x: f32,
    origin_y: f32,
    cols: u16,
    cell_w: f32,
    cell_h: f32,
    surface_w: u32,
    surface_h: u32,
) -> Option<PixelRect>
where
    I: IntoIterator<Item = usize>,
{
    pane_damage_rect_with_ink_pad(
        is_alt, dirty_rows, pane_rect, origin_x, origin_y, cols, cell_w, cell_h, 0.0, surface_w,
        surface_h,
    )
}

#[must_use]
#[allow(clippy::too_many_arguments)]
fn pane_damage_rect_with_ink_pad<I>(
    is_alt: bool,
    dirty_rows: I,
    pane_rect: PixelRect,
    origin_x: f32,
    origin_y: f32,
    cols: u16,
    cell_w: f32,
    cell_h: f32,
    vertical_ink_pad: f32,
    surface_w: u32,
    surface_h: u32,
) -> Option<PixelRect>
where
    I: IntoIterator<Item = usize>,
{
    if is_alt {
        // When: `is_alt` — the app scrolls and moves content, so a row it did
        // not re-emit can still be stale. Repaint the pane, not the row union.
        let has_dirty = dirty_rows.into_iter().next().is_some();
        return if has_dirty {
            pane_rect.intersect(PixelRect { x: 0, y: 0, w: surface_w, h: surface_h })
        } else {
            // When: `has_dirty` is false — the app marked nothing, so the
            // surface still holds pixels it considers current.
            None
        };
    }
    dirty_rows_damage_rect_with_ink_pad(
        dirty_rows,
        pane_rect,
        origin_x,
        origin_y,
        cols,
        cell_w,
        cell_h,
        vertical_ink_pad,
        surface_w,
        surface_h,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenderMode {
    Full,
    Noop,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RenderSignals {
    pub first_frame: bool,
    pub resize: bool,
    pub dpi_or_scale_change: bool,
    pub font_or_atlas_rebuild: bool,
    pub theme_or_config_reload: bool,
    pub surface_reconfigure: bool,
    pub occlusion_restore: bool,
    pub viewport_scroll: bool,
    pub selection_change: bool,
    pub tab_switch: bool,
    pub pane_topology_change: bool,
    pub scrollbar_change: bool,
    pub overlay_active_or_toggled: bool,
    pub degrade_state_changed: bool,
    pub dirty_damage: Option<PixelRect>,
}

#[must_use]
pub(crate) fn decide_render_mode(degrade: bool, signals: RenderSignals) -> RenderMode {
    if !degrade {
        // When: `!degrade` — a real GPU presents. Frame-skipping exists to
        // spare a CPU rasterizer; on hardware it costs more than it saves.
        return RenderMode::Full;
    }
    let force_full = signals.first_frame
        || signals.resize
        || signals.dpi_or_scale_change
        || signals.font_or_atlas_rebuild
        || signals.theme_or_config_reload
        || signals.surface_reconfigure
        || signals.occlusion_restore
        || signals.viewport_scroll
        || signals.selection_change
        || signals.tab_switch
        || signals.pane_topology_change
        || signals.scrollbar_change
        || signals.overlay_active_or_toggled
        || signals.degrade_state_changed;
    if force_full || signals.dirty_damage.is_some() {
        RenderMode::Full
    } else {
        // When: neither `force_full` nor `dirty_damage` — nothing visible
        // differs, so the CPU rasterizer skips the pass entirely.
        RenderMode::Noop
    }
}

#[must_use]
fn atlas_evicted_during_frame(frame_epoch: u64, atlas: &GlyphAtlas) -> bool {
    atlas.evictions() != frame_epoch
}

fn atlas_texture_rebuild_required(current: (u32, u32), next: (u32, u32)) -> bool {
    current != next
}

fn scale_factor_rebuild_required(current: f32, next: f32) -> bool {
    (current - next.max(0.1)).abs() >= f32::EPSILON
}

const PLACEHOLDER_ATLAS_DIM: u32 = 1;

#[must_use]
fn atlas_payload_bytes(width: u32, height: u32) -> u64 {
    u64::from(width).saturating_mul(u64::from(height)).saturating_mul(4)
}

#[must_use]
fn desired_gpu_atlas_dimensions(software_presenter: bool, atlas: &GlyphAtlas) -> (u32, u32) {
    if software_presenter {
        (PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM)
    } else {
        // When: `!software_presenter` — the GPU samples the atlas texture, so
        // it must match the CPU atlas or the UVs address the wrong tiles.
        (atlas.width(), atlas.height())
    }
}

#[must_use]
fn image_atlas_promotion_required(atlas: &GlyphAtlas, has_inline_media: bool) -> bool {
    has_inline_media
        && (atlas.width(), atlas.height()) == (PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM)
}

/// Frames a window must draw with no renderable inline media before its image
/// atlas is released.
///
/// At 60fps this is about four seconds. Long enough that scrolling an image
/// out of view and back does not free and reallocate 16 MiB — which would also
/// force every visible image to re-decode — and short enough that a window
/// that has genuinely finished with images does not hold the allocation for
/// the rest of its life.
const IMAGE_ATLAS_IDLE_FRAMES: u32 = 240;

/// Whether an idle window should release its full-size image atlas.
///
/// Promotion is otherwise one-way: `reset_in_place` clears the map and
/// repacker but never touches the pixel buffer or the dimensions, so a window
/// that displays one inline image keeps the allocation until it closes.
#[must_use]
fn image_atlas_demotion_ready(
    atlas: &GlyphAtlas,
    has_inline_media: bool,
    frames_without_inline_media: u32,
) -> bool {
    !has_inline_media
        && (atlas.width(), atlas.height()) != (PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM)
        && frames_without_inline_media >= IMAGE_ATLAS_IDLE_FRAMES
}

/// Whether clearing the image atlas would actually do anything.
///
/// The frame-assembly caller resets whenever inline media "changed", but that
/// signal is `true` on any frame whose predecessor's key was absent — which is
/// every frame following the many state changes that clear it. On a window
/// that has never shown an image the media hash cannot change, so the absent
/// key accounts for every reset, once per rendered frame.
///
/// An untouched placeholder atlas holds no entries and no packing state, so
/// resetting it changes nothing while still rebuilding the packer and bumping
/// the atlas identity — which invalidates every cache keyed to it. A promoted
/// atlas carries that state even while its entry map is momentarily empty and
/// must still be reset, or the packer would keep handing out coordinates from
/// a layout the caller believes it discarded.
#[must_use]
fn image_atlas_reset_warranted(atlas: &GlyphAtlas) -> bool {
    !atlas.is_empty()
        || (atlas.width(), atlas.height()) != (PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM)
}

fn full_surface_rect(width: u32, height: u32) -> PixelRect {
    PixelRect { x: 0, y: 0, w: width.max(1), h: height.max(1) }
}

pub(crate) const MAX_SURFACE_DIMENSION: u32 = 16_384;
const MAX_SURFACE_BYTES: u64 = 160 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ValidatedSurfaceSize {
    pub width: u32,
    pub height: u32,
    pub bytes: usize,
}

#[must_use]
pub(crate) fn validated_surface_size(
    width: u32,
    height: u32,
    device_max_dimension: u32,
) -> Option<ValidatedSurfaceSize> {
    let width = width.max(1);
    let height = height.max(1);
    let max_dimension = device_max_dimension.clamp(1, MAX_SURFACE_DIMENSION);
    if width > max_dimension || height > max_dimension {
        // When: an axis exceeds `max_dimension` — configuring the surface
        // anyway is a driver-level failure, so the caller rejects the size.
        return None;
    }
    let bytes = u64::from(width).checked_mul(u64::from(height))?.checked_mul(4)?;
    if bytes > MAX_SURFACE_BYTES {
        // When: `bytes > MAX_SURFACE_BYTES` — both axes legal, their product
        // not. The ceiling bounds one surface against the process budget.
        return None;
    }
    Some(ValidatedSurfaceSize { width, height, bytes: usize::try_from(bytes).ok()? })
}

fn search_text_scroll(prefix_width: f32, cursor_width: f32, visible_width: f32) -> f32 {
    (prefix_width + cursor_width - visible_width).max(0.0)
}

fn create_frame_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    format: TextureFormat,
) -> (Texture, TextureView) {
    let texture = device.create_texture(&TextureDescriptor {
        label: Some("sonic-retained-frame"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format,
        usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&TextureViewDescriptor::default());
    (texture, view)
}

/// Opacity of the active tab's accent bar while the window holds keyboard
/// focus.
pub const ACTIVE_PANEL_MARKER_ALPHA_FOCUSED: f32 = 1.0;

/// Opacity of the active tab's accent bar while the window is unfocused.
///
/// Low enough to read as "not the focused window" at a glance, high enough
/// that the active tab is still identifiable without focusing the window to
/// ask.
pub const ACTIVE_PANEL_MARKER_ALPHA_UNFOCUSED: f32 = 0.4;

/// Style and sizing inputs for tab-bar quad emission.
pub struct TabBarQuadParams {
    /// Number of tabs in the bar.
    pub tab_count: usize,
    /// Active tab accent color.
    pub accent: [f32; 4],
    /// Inactive tab separator color.
    pub separator: [f32; 4],
    /// Bar background and bottom border color.
    pub border: [f32; 4],
    /// Hovered tab index, or `u32::MAX` when no tab is hovered.
    pub hover_tab_idx: u32,
    /// Surface dimensions in the same units as the layout rects.
    pub surface: (f32, f32),
    /// Opacity of the active tab's accent bar, `0.0`–`1.0`.
    ///
    /// Not a visibility flag. The accent answers *which tab is active*, which
    /// is true of the window whether or not it holds keyboard focus, so an
    /// unfocused window dims it rather than dropping it — otherwise the
    /// window stops saying anything about its own state and has to be
    /// focused to find out.
    pub active_panel_marker_alpha: f32,
}

/// Paint the tab-bar background and tab chrome quads into `quads`.
pub fn emit_tab_bar_quads(
    quads: &mut Vec<QuadInstance>,
    layout: &TabBarLayout,
    params: &TabBarQuadParams,
) {
    let (sw, sh) = params.surface;
    quads.push(QuadInstance {
        rect: px_to_ndc(layout.bar.x, layout.bar.y, layout.bar.w, layout.bar.h, sw, sh),
        color: params.border,
        ..Default::default()
    });
    quads.push(QuadInstance {
        rect: px_to_ndc(layout.bar.x, layout.bar.y + layout.bar.h - 1.0, layout.bar.w, 1.0, sw, sh),
        color: params.border,
        ..Default::default()
    });
    for t in &layout.tabs {
        let is_active = layout.active == Some(t.idx);
        let marker_alpha = params.active_panel_marker_alpha.clamp(0.0, 1.0);
        if is_active && marker_alpha > 0.0 {
            let scale = (t.bg_rect.h / (TAB_BAR_HEIGHT - 2.0 * TAB_VERT_INSET)).max(0.1);
            let inset = ACTIVE_TOP_ACCENT_INSET * scale;
            let acc = sonicterm_render_model::boundary::ui::tabbar_view::Rect {
                x: t.bg_rect.x + inset,
                y: t.bg_rect.y + 1.0 * scale,
                w: (t.bg_rect.w - inset * 2.0).max(0.0),
                h: ACTIVE_TOP_ACCENT_H * scale,
            };
            let base = t
                .custom_color
                .as_deref()
                .map(|hex| hex_to_premultiplied_rgba(hex, 1.0))
                .unwrap_or(params.accent);
            let color = scale_premultiplied_alpha(base, marker_alpha);
            quads.push(QuadInstance {
                rect: px_to_ndc(acc.x, acc.y, acc.w, acc.h, sw, sh),
                color,
                ..Default::default()
            });
        }
        if t.idx + 1 < params.tab_count {
            // Geometric scale = bar.h / default-logical-bar-h. Mirrors
            // the per-bar-height scale `TabBarLayout::compute_at_y`
            // uses to grow TAB_GAP / padding with bar height — keeps
            // separators centered in each adjacent-tab gap.
            let scale = (layout.bar.h / 40.0).max(0.1);
            let sep_w = 1.0_f32 * scale;
            let sep_h = (layout.bar.h - 16.0 * scale).max(1.0);
            let sep_y = layout.bar.y + (layout.bar.h - sep_h) * 0.5;
            let gap_mid = t.bg_rect.x + t.bg_rect.w + (TAB_GAP * scale - sep_w) * 0.5;
            quads.push(QuadInstance {
                rect: px_to_ndc(gap_mid, sep_y, sep_w, sep_h, sw, sh),
                color: params.separator,
                ..Default::default()
            });
        }
    }
}

// (Per-row cache + grid SpanDesc removed in the B3 cutover — the GPU
// atlas does an O(1) lookup per cell, so the bookkeeping is wasted
// work. Walking 80×40 ≈ 3 200 cells per frame stays well under a
// millisecond on the renderer thread.)

// GpuRenderer holds several wgpu / cosmic-text resources (`instance`,
// `font_system`, etc.) that exist purely to keep their owned allocations
// alive for the lifetime of the renderer — they're never read after
// construction. `#[allow(dead_code)]` documents that intent at the struct
// level; removing it would force per-field `_` prefixing which obscures
// what each handle is.
#[allow(dead_code)]
#[derive(Clone)]
pub struct GpuSharedContext {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

/// Top-level GPU-backed terminal renderer. Owns the wgpu surface, the
/// text + quad pipelines, the glyph atlas, font/shape caches, and all
/// per-frame layout / cursor / overlay state. One per OS window.
pub struct GpuRenderer {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    /// True when wgpu selected a CPU/software rasterizer (WARP, llvmpipe,
    /// SwiftShader) — see [`detect_software_rendering`]. The app reads this
    /// via [`GpuRenderer::is_software_rendering`] to degrade the frame cap and
    /// per-frame animation.
    software_rendering: bool,
    /// Resolved no-GPU degrade state: adapter detection combined with
    /// `[appearance].software_render_mode`.
    software_render_degrade: bool,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: SurfaceConfiguration,
    hardware_present_mode: PresentMode,
    hardware_alpha_mode: CompositeAlphaMode,
    window: Arc<Window>,

    /// WezTerm-style final presentation pipeline. It consumes every glyph and
    /// geometry primitive for a frame and emits one indexed draw stream.
    present_pipeline: WeztermPipeline,
    frame_texture: Texture,
    frame_view: TextureView,
    frame_blitter: wgpu::util::TextureBlitter,

    // B3 GPU text path for terminal and chrome glyphs.
    glyph_atlas: GlyphAtlas,
    glyph_upload: AtlasUpload,
    /// Monotonic identity of the current glyph-atlas allocation. Unlike the
    /// atlas-local eviction count, this survives replacement and prevents an
    /// old epoch-0 UV cache from matching a new epoch-0 atlas.
    glyph_atlas_generation: u64,
    // Inline media is isolated so large images cannot evict glyph tiles
    // referenced by the row cache.
    image_atlas: GlyphAtlas,
    image_upload: AtlasUpload,
    retained_inline_media_bytes: usize,
    /// Consecutive frames this window has drawn with no renderable inline
    /// media. Promotion of the image atlas is one-way without this: a window
    /// that shows a single image holds the full-size allocation for its whole
    /// life, even after the image scrolls into history. Demoting on the first
    /// idle frame would instead free and reallocate 16 MiB every time an image
    /// scrolled off and back, and re-decode it each time, so the count gates
    /// demotion behind a sustained absence.
    frames_without_inline_media: u32,
    /// True after an eviction-triggered compaction. The rebuilt atlas has
    /// eviction disabled until one frame presents successfully, bounding
    /// retries when the visible glyph working set exceeds atlas capacity.
    glyph_atlas_retry_without_eviction: bool,

    font_family: String,
    font_dirs: Vec<PathBuf>,
    font_size: f32,
    line_height: f32,
    font_weight_scale: f32,
    /// Multiplier applied to the font's natural cell height to derive the
    /// rendered line height (`cell_h = natural_cell_h * line_height_mult`).
    /// Stored so a DPI/scale-factor change can recompute `cell_h` from the
    /// freshly-rasterized natural height — see `rebuild_for_sf`. Without it,
    /// the rebuild had to back this factor out of the stale `line_height`,
    /// which algebraically cancelled and pinned `cell_h` to the old DPI.
    line_height_mult: f32,
    /// DPI multiplier (e.g. 2.0 on Retina). Post-G1a (wezterm-takeover)
    /// the renderer is raster-px end-to-end, so draw and hit-test sites
    /// no longer multiply/divide by this; its sole job is sizing the
    /// glyph rasterizer target. Stored, plumbed to `SwashRasterizer`,
    /// never used at the draw boundary.
    scale_factor: f32,
    /// Cell width in raster pixels (one terminal column). Sourced from
    /// `FontStack::cell_metrics_raster_px()` so sonicterm-font metrics
    /// drop in without a unit conversion.
    pub cell_w: f32,
    /// Cell height in raster pixels (one terminal row).
    pub cell_h: f32,
    padding_left: f32,
    padding_right: f32,
    padding_top: f32,
    padding_bottom: f32,
    bg: wgpu::Color,
    bg_opacity: f32,
    /// Scrollbar visibility policy from config. Read on
    /// every frame in the per-pane scrollbar emit loop.
    scrollbar_mode: sonicterm_render_model::boundary::cfg::config::ScrollbarMode,
    /// Padding between overlay panel chrome and inner content.
    panel_padding: f32,
    fg_default: ChromeColor,
    cursor_color: [f32; 4],
    /// Theme cursor-text color as straight RGBA. Used to recolor glyphs
    /// under block cursors and cursor-like overlay carets without deriving
    /// text color from the background.
    cursor_text_color: [f32; 4],
    /// Theme background as straight RGBA. Used by overlays that intentionally
    /// mask text with the terminal background.
    bg_rgba: [f32; 4],
    /// Visual style of the text cursor (block / bar / underline).
    /// Live-updated from config; see [`Self::set_cursor_shape`].
    cursor_shape: CursorShape,
    /// Whether the text cursor blinks. When `false` the cursor renders
    /// at solid alpha and the FrameKey ignores the phase bucket.
    cursor_blink: bool,
    /// Anchor for the blink phase. Reset on every config change so the
    /// user sees the cursor at full brightness immediately after they
    /// toggle the setting (rather than wherever the cycle happened to
    /// be at the time).
    blink_epoch: Instant,
    /// Whether the OS window currently holds keyboard focus. The text
    /// cursor is hidden while the window is inactive. Defaults to
    /// `true` so a freshly created renderer draws the cursor on the
    /// very first frame, before winit has a chance to deliver
    /// `Focused(true)`.
    window_focused: bool,
    /// Cursor positions inside inactive panes (panes that share the
    /// window with the active pane but don't currently own keyboard
    /// focus). Kept as a compatibility sink for the app-side plumbing;
    /// inactive pane cursors are no longer drawn.
    inactive_pane_cursors: Vec<InactivePaneCursor>,
    /// Short-lived focus confirmation animation for the pane that just
    /// became active. Cleared automatically after
    /// [`PANE_FOCUS_FLASH_DURATION`].
    pane_focus_flash: Option<(u64, Instant)>,
    selection_color: [f32; 4],
    tab_bar_bg: [f32; 4],
    tab_active_bg: [f32; 4],
    tab_inactive_bg: [f32; 4],
    tab_active_fg: ChromeColor,
    tab_inactive_fg: ChromeColor,
    /// Deprecated user override for the removed tab close button. Kept
    /// only so older configs round-trip without changing the renderer
    /// settings surface.
    tab_close_override: Option<[f32; 4]>,
    /// Last reported cursor position in LOGICAL pixels, or `None` when
    /// the cursor is outside the window. Drives tab hover state.
    hover_cursor: Option<(f32, f32)>,
    /// Color for the wezterm-style vertical bar drawn between adjacent
    /// inactive tabs. A dim variant of the inactive-fg works in every
    /// theme; we precompute it here so the per-frame render path stays
    /// allocation-free.
    tab_separator: [f32; 4],
    hyperlink_underline: [f32; 4],
    splitter_color: [f32; 4],
    hyperlink_tint: [f32; 4],
    search_highlight: [f32; 4],
    search_fg: ChromeColor,
    search_bg: [f32; 4],
    // The 11 `*_buffer: legacy chrome buffer`
    // fields that lived here (search, quick_select, palette_{query,rows,
    // footer}, ime, broadcast,
    // drag_chip) are gone. Every chrome string is now shaped on demand
    // inside `render()` via `chrome_text::layout(...)`; the resulting
    // glyph instances feed either `glyph_instances` (pre-overlay
    // chrome — tab titles, search status bar) or
    // `overlay_glyph_instances` (modal chrome — palette,
    // IME preedit, drag-chip title). No per-renderer glyphon buffer
    // state survives.
    /// Cached drag-chip rect from the last `render()` call (in logical
    /// pixels). `None` when no chip was drawn. Test-only diagnostic
    /// surfaced through [`Self::last_drag_chip_visual`].
    drag_chip_visual: Option<DragChipVisual>,
    /// Last rendered frame key — when the next frame would produce an
    /// identical key, render() short-circuits before any GPU work.
    last_frame_key: Option<FrameKey>,
    /// Memoized inline IME preedit overlay glyphs. The preedit
    /// is re-shaped from scratch every frame otherwise; while a composition is
    /// unchanged across frames (paused, or PTY-burst redraws while composing)
    /// this reuses the emitted glyphs. Keyed on the text + placement + color +
    /// the atlas allocation generation + eviction epoch, so neither
    /// replacement nor rectangle recycling can leave its UVs stale.
    preedit_glyph_cache: Option<PreeditGlyphCache>,
    /// Cumulative count of frames skipped via the FrameKey fast-path.
    /// Exposed via tracing::trace for `RUST_LOG=trace` hit-rate dashboards.
    skipped_frames: u64,
    /// Frames that reached a native presentation boundary successfully.
    successful_frame_count: u64,
    #[cfg(target_os = "windows")]
    software_frame: Option<crate::software_windows::WindowsSoftwareFrame>,
    /// Window label used in renderer-internal timing logs.
    render_timing_label: &'static str,
    /// Whether the tab bar is currently shown. Toggled at runtime by the
    /// View → Toggle Tab Bar menu action; when `false`, [`Self::top_inset`]
    /// returns 0 and the tab bar draw block in [`Self::render`] is skipped.
    tab_bar_visible: bool,
    /// Reserved height (logical px) above the tab bar for the OS native
    /// titlebar. Kept at zero while SonicTerm uses the normal OS titlebar with a
    /// bottom-pinned tab bar.
    titlebar_inset: f32,
    /// Characters from the most recent `render()` call that the
    /// rasterizer could not produce a tile for (i.e. would draw as a
    /// tofu outline). Whitespace is excluded. Test-only diagnostic
    /// surfaced through [`Self::last_missing_tofu`]; production code
    /// must not depend on it.
    last_missing_chars: Vec<char>,
    // The per-style-run `ShapeCache` was
    // deleted with the cosmic-text path in T8 (`shape.rs` is now a
    // thin sonicterm-font adapter). Per-row caching survives at the
    // higher-level `row_glyph_cache` layer below — that's the cache
    // that actually short-circuits the steady-state interactive
    // shell. Re-shaping a style run via sonicterm-font on a row-cache
    // miss is cheap relative to the bitmap rasterize + atlas insert
    // it precedes.
    /// Sonicterm-font driven shaper. Owns
    /// the cell metrics (`cell_metrics_raster_px()`), the resolved
    /// font fallback chain, and the `blocking_shape` entry point that
    /// `flush_shape_run` calls through `shape_run_with_wezterm`. The
    /// renderer keeps the `Option<...>` shape so test fixtures (no
    /// bundled fonts on disk) can still construct a `GpuRenderer`
    /// even though the grid path is degraded.
    pub(crate) font_stack: Option<sonicterm_engine::FontStack>,
    /// Native-size stack for tab titles (`body + 1`).
    tab_title_font_stack: Option<sonicterm_engine::FontStack>,
    /// Native-size stack for the command-palette footer (`body - 1`).
    palette_footer_font_stack: Option<sonicterm_engine::FontStack>,
    /// Per-row glyph cache. Stores the shaped
    /// `GlyphInstance`s, underline coalescing, and missing-tofu list
    /// for each visible row, keyed by absolute row index + a content
    /// hash. A row whose contents / style / selection-overlap haven't
    /// changed splices its cached output straight into the frame and
    /// skips the entire `flush_shape_run` walk.
    row_glyph_cache: sonicterm_text::row_glyph_cache::RowGlyphCache,
    /// Per-row cache for background/underline/hyperlink-tint quads
    /// Mirrors `row_glyph_cache` but for the
    /// `QuadInstance`s emitted by `emit_cell_bg_quads_clipped` — on a
    /// hit we splice the cached `Vec<QuadInstance>` straight into the
    /// frame's quad vector and skip the per-cell run-length-encode.
    line_quad_cache: crate::row_quad_cache::LineQuadCache,
    /// Per-pane origins recorded on the most recent `render()` call.
    /// `(pane_id, [origin_x_px, origin_y_px])` for every pane in the
    /// frame's pane slice. Test-only diagnostic surfaced through
    /// [`Self::last_emitted_origins`]; production code must not rely
    /// on it. Part B step 7 hook for the per-pane render integration
    /// test.
    last_emit_origins: Vec<(u64, [f32; 2])>,
    /// Per-pane logical-px layout snapshot recorded on the most recent
    /// `render()` call, in raster pixels (winit reports physical-px;
    /// post-G1a the renderer is raster-px end-to-end so no boundary
    /// conversion happens). Drives the pane-aware hit-test in
    /// [`Self::pixel_to_cell`] so clicks land on the correct
    /// pane and column even when the per-column edge cache
    /// (`snapped_cell_x`) has jitter at fractional DPI scales. Empty
    /// before the first render — callers must handle the fallback path.
    last_pane_layout: Vec<PaneLayoutSnapshot>,
    /// Monotonic counter bumped on theme / default-fg / default-bg
    /// changes. Folded into every `row_hash` so palette swaps
    /// invalidate cached colours without iterating the cache.
    style_rev: u64,
    /// Active drag-chip overlay: translucent rect drawn at the cursor
    /// while a tab is held. Cleared on release.
    drag_chip: Option<DragChipOverlay>,
    /// Optional async font fallback loader.
    /// When set, every transient `SwashRasterizer` built inside
    /// `render()` / `set_font` / `rebuild_for_scale` has the loader
    /// attached so misses on CJK / emoji / nerd-font codepoints fire a
    /// background `request_load` and, on completion, the loader's
    /// notifier fires `UserEvent::ClearShapeCache` on the winit
    /// `EventLoopProxy` plumbed in by `sonicterm-app`. Stays `None` in
    /// tests / examples that construct `GpuRenderer` without an event
    /// loop proxy (the existing tofu fallback path keeps working).
    // `async_fallback::AsyncFallbackLoader` is deleted with
    // the rest of the swash/cosmic-text family. sonicterm-font handles
    // CJK/emoji/Nerd-font fallback synchronously through its own
    // resolved fallback chain (vendor-* features), so no async hook
    // is plumbed here. The field stays as a placeholder so the
    // surrounding `Option<...>` pattern + `set_async_loader` /
    // `async_loader` getter API survive future plumbing without a
    // cross-crate breaking change.
    async_loader: Option<()>,
}

/// A compact fingerprint of every input that can affect the rendered
/// frame. If two consecutive frames produce an equal key the second one
/// is a no-op for the user, so the renderer skips text shaping, quad
/// rebuild and GPU submission entirely.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FrameKey {
    grid_revision: u64,
    /// Per-pane grid revisions. Part B step 5: split panes each own a Grid,
    /// so a write to an inactive pane (e.g. background `tail -f`) must
    /// invalidate the cached frame even though `grid_revision` (active pane)
    /// is unchanged.
    pane_revs: Vec<(u64, u64, Option<u64>)>,
    /// Sorted per-pane opacity buckets for scrollbar pixels that can be emitted.
    pane_scrollbar_alpha: Vec<(u64, u16)>,
    selection: Option<Selection>,
    copy_mode: Option<CopyModeState>,
    quick_select_hint_count: u32,
    cursor_visible: bool,
    tab: u64,
    pane: u64,
    search_hash: u64,
    palette_hash: u64,
    ime_hash: u64,
    notification_hash: u64,
    width: u32,
    height: u32,
    tab_hash: u64,
    pane_rect_hash: u64,
    viewport_top_abs: Option<u64>,
    /// Cursor shape variant index — different shapes paint different
    /// pixels even for the same grid + same blink phase, so this MUST
    /// participate in the key.
    cursor_shape: u8,
    /// Whether the cursor is blinking. Folded into the key so flipping
    /// the setting invalidates the cached frame immediately.
    cursor_blink: bool,
    /// Quantised blink phase. `0` when blinking is disabled (see
    /// [`crate::cursor::phase_bucket`]).
    cursor_phase: u8,
    /// Whether the window has keyboard focus — toggles active cursor
    /// visibility.
    window_focused: bool,
    /// Quantized pane-focus flash phase. Folded into the key so the
    /// bounded flash can animate without reviving the old infinite
    /// heartbeat redraw loop.
    pane_focus_flash_bucket: u8,
    /// Index of the tab the cursor is currently over, or `u32::MAX`
    /// when the cursor is not over any tab. Moving between tabs must
    /// invalidate the cached frame for hover chrome.
    hover_tab: u32,
    /// Deprecated close-button hover bit. Always zero now that close
    /// buttons are no longer drawn; kept to avoid reshaping FrameKey.
    hover_close: u8,
    /// Deprecated close-button override bit. Kept so older config reload
    /// paths can still invalidate safely.
    close_override: u8,
    broadcast_receivers_hash: u64,
    inline_media_hash: u64,
    /// Cmd-hovered URL cell range. Folded into the key so moving the
    /// hover onto / off a URL (or to a different URL span) invalidates
    /// the cached frame and re-shapes with / without the accent recolor.
    hovered_url_cells: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
    process_privileged: bool,
}

/// Memoized inline IME preedit overlay glyphs. Reused across
/// frames when the composition text and its placement are unchanged so a
/// paused or streaming-while-composing preedit isn't re-shaped every frame.
/// `atlas_epoch` combines a renderer-owned allocation generation with the
/// atlas's cumulative eviction count. Replacement changes the generation;
/// rectangle recycling changes the eviction count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GlyphAtlasEpoch {
    generation: u64,
    evictions: u64,
}

struct PreeditGlyphCache {
    text: String,
    font_size: f32,
    start_x: f32,
    top_y: f32,
    color_bits: u32,
    atlas_epoch: GlyphAtlasEpoch,
    glyphs: Vec<GlyphInstance>,
}

impl PreeditGlyphCache {
    /// True when this cache entry exactly matches the requested preedit emit
    /// (text + placement + color + atlas generation).
    fn matches(
        &self,
        text: &str,
        font_size: f32,
        start_x: f32,
        top_y: f32,
        color_bits: u32,
        atlas_epoch: GlyphAtlasEpoch,
    ) -> bool {
        self.atlas_epoch == atlas_epoch
            && self.color_bits == color_bits
            && self.font_size.to_bits() == font_size.to_bits()
            && self.start_x.to_bits() == start_x.to_bits()
            && self.top_y.to_bits() == top_y.to_bits()
            && self.text == text
    }
}

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
/// Post-G1a (wezterm-takeover): all coordinates are in raster pixels,
/// the same unit winit reports for cursor input — no boundary
/// conversion takes place inside `pixel_to_cell`.
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
    /// Cell width in raster pixels for the pane (currently identical
    /// across panes but kept per-pane for forward-compat with per-pane
    /// fonts).
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
/// `debug`, when supplied, receives one record per emitted glyph for tests
/// asserting the atlas path was taken.
#[doc(hidden)]
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
) {
    // Chrome_text-driven port of the tab-title emit loop. Each
    // span is shaped through sonicterm-font and rasterized through the
    // supplied native-size FontStack into the shared atlas. The legacy SwashRasterizer +
    // cosmic-text `shape_run` path is gone (T10 deletes the
    // helpers entirely; T14 has already migrated this site off them).
    let mut pen_x: f32 = 0.0;
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

/// Emit overlay text (palette query / rows / footer, etc.) as
/// chrome_text-rendered glyph instances. Mirrors
/// [`emit_tab_title_glyphs`] but takes an explicit pixel `origin_x`
/// and `baseline_y` plus a clipping rect, so the caller can position
/// multi-line overlays (one call per line, advancing `baseline_y` by
/// `line_stride` each time).
///
/// Post-G1a (wezterm-takeover) and post-T14: every input is raster
/// px, the emitted instance rects are raster-px-derived NDC, and the
/// chrome path lives entirely in [`chrome_text::layout`] — no
/// SwashRasterizer, no cosmic-text shaper.
///
/// Glyphs whose rect falls entirely outside `bounds` are skipped so
/// the renderer doesn't paint outside the palette modal.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
fn measure_overlay_text_width(
    glyph_atlas: &mut GlyphAtlas,
    font_stack: &sonicterm_engine::FontStack,
    font_size_px: f32,
    native_em_px: f32,
    wt_raster: &mut impl sonicterm_text::glyph_atlas::Rasterizer,
    text: &str,
    color: ChromeColor,
) -> f32 {
    if text.is_empty() {
        // When: `text.is_empty()` — layout would still touch the atlas and
        // rasterizer to produce a zero-width answer.
        return 0.0;
    }
    chrome_text::layout(
        font_stack,
        wt_raster,
        glyph_atlas,
        text,
        color,
        ChromeAttrs::default(),
        font_size_px,
        native_em_px,
        (0.0, 0.0),
        (1.0, 1.0),
        None,
    )
    .width_px
}

/// Emit one line of overlay text (palette query, rows, footer, search field)
/// as clipped glyph instances.
///
/// Positions glyphs from an explicit pixel `origin_x`/`baseline_y` so a caller
/// draws a multi-line overlay by calling once per line and advancing the
/// baseline itself. Glyphs falling outside `bounds` are dropped by the layout
/// clip, which is what keeps text inside a modal panel instead of painting
/// across the terminal behind it.
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
) {
    if text.is_empty() {
        // When: `text.is_empty()` — an unset footer or empty query. Returning
        // leaves `glyph_instances` untouched, so line advance is unaffected.
        return;
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
}

/// Renderers constructed but not yet dropped, across the whole process.
///
/// A renderer's own `retained_amounts()` cannot answer "did the last one go
/// away": it reports the instance being asked, and every instance reports the
/// same atlas capacity. Reading it in a loop compares a constant to itself and
/// holds whether or not anything leaked. This counter is the quantity a leak
/// actually moves — it rises on construction, falls in `Drop`, and returns to
/// its starting value only if every renderer built was also released.
static LIVE_RENDERERS: AtomicUsize = AtomicUsize::new(0);

/// How many `GpuRenderer`s are alive right now.
///
/// Intended for churn and lifecycle checks: take a reading, create and drop
/// renderers, and compare. A surviving renderer leaves this above where it
/// started.
// Ordering: `LIVE_RENDERERS.load(Ordering::Acquire)`, pairing with the
// `Ordering::AcqRel` RMWs in `new_async` and `Drop`. No payload is published.
pub fn live_renderer_count() -> usize {
    LIVE_RENDERERS.load(Ordering::Acquire)
}

/// CPU-side storage a renderer holds, split by owning class.
///
/// Deliberately not a single total. The three parts have different lifetimes
/// and different remedies: atlases grow with the glyph and image set and are
/// evictable, while a software frame is sized by the window and is not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RendererRetention {
    /// Rasterized glyph pixels mirrored on the CPU, and resident entries.
    pub glyph_atlas: ResourceAmount,
    /// Decoded inline-image pixels mirrored on the CPU, and resident entries.
    pub image_atlas: ResourceAmount,
    /// Windows software presentation buffer. Zero elsewhere.
    pub software_frame: ResourceAmount,
}

impl RendererRetention {
    /// Class-tagged parts, for checking that every part is accounted for.
    ///
    /// **Nothing charges these.** `sonicterm-gpu` declares no dependency on
    /// `sonicterm-resource`, so this crate cannot reserve against a governor
    /// at all, and the renderer's memory reaches the app as a report rather
    /// than a ledger entry. The `ResourceClass` tags exist so a part cannot be
    /// added to this struct without deciding what it is.
    ///
    /// Wiring this to charging is not a small change, and the reason is here
    /// rather than in the caller that would attempt it: `image_atlas` maps to
    /// `InlineMediaRetained`, which a pane's decoded media already uses. They
    /// are different resident allocations — `capacity()` of one contiguous
    /// atlas buffer versus summed `len()` across separately-owned per-image
    /// `Vec`s — so charging both under one class would make the class mean two
    /// things and leave a reader unable to tell which allocation to act on.
    #[must_use]
    pub fn seam_classes(&self) -> [(ResourceClass, ResourceAmount); 3] {
        [
            (ResourceClass::GlyphAtlas, self.glyph_atlas),
            (ResourceClass::InlineMediaRetained, self.image_atlas),
            (ResourceClass::SoftwareFrame, self.software_frame),
        ]
    }

    /// Sum of every part.
    #[must_use]
    pub fn total(&self) -> ResourceAmount {
        [self.glyph_atlas, self.image_atlas, self.software_frame].into_iter().fold(
            ResourceAmount::default(),
            |acc, part| ResourceAmount {
                bytes: acc.bytes.saturating_add(part.bytes),
                items: acc.items.saturating_add(part.items),
            },
        )
    }
}

impl GpuRenderer {
    /// Build a renderer bound to `window`. Creates the wgpu surface +
    /// device + pipelines, the cosmic-text font system, the glyph atlas,
    /// and seeds the initial cell metrics from `theme`'s configured
    /// font family / size / line height.
    pub fn new(
        window: Arc<Window>,
        event_loop: &ActiveEventLoop,
        theme: &Theme,
        settings: RendererSettings<'_>,
    ) -> Result<Self> {
        pollster::block_on(Self::new_async(window, event_loop, theme, settings, None))
    }

    /// Build a renderer that shares an existing wgpu instance, adapter, device,
    /// and queue with another window.
    ///
    /// Every window after the first takes this path: one device serves all of
    /// them, so opening a window neither re-enumerates adapters nor allocates a
    /// second device. The surface, pipelines, and atlases are still per-window.
    pub fn new_with_shared_context(
        window: Arc<Window>,
        event_loop: &ActiveEventLoop,
        theme: &Theme,
        settings: RendererSettings<'_>,
        shared: GpuSharedContext,
    ) -> Result<Self> {
        pollster::block_on(Self::new_async(window, event_loop, theme, settings, Some(shared)))
    }

    /// Clone the handles a sibling window needs to share this renderer's GPU
    /// context, for passing to [`Self::new_with_shared_context`].
    ///
    /// The clones are wgpu reference-counted handles to one underlying device,
    /// not copies of it.
    pub fn shared_context(&self) -> GpuSharedContext {
        GpuSharedContext {
            instance: self.instance.clone(),
            adapter: self.adapter.clone(),
            device: self.device.clone(),
            queue: self.queue.clone(),
        }
    }

    // Ordering: `LIVE_RENDERERS.fetch_add(1, Ordering::AcqRel)`, pairing with
    // the `Ordering::AcqRel` decrement in `Drop`. Publishes no payload.
    async fn new_async(
        window: Arc<Window>,
        event_loop: &ActiveEventLoop,
        theme: &Theme,
        settings: RendererSettings<'_>,
        shared: Option<GpuSharedContext>,
    ) -> Result<Self> {
        let RendererSettings {
            font_family,
            font_dirs,
            font_size,
            line_height_mult,
            font_weight_scale,
            padding,
            appearance,
            role,
        } = settings;
        let font_weight_scale = effective_font_weight_scale(font_weight_scale);
        let [padding_left, padding_right, padding_top, padding_bottom] = padding;
        let size = window.inner_size();
        // G1a: read the OS DPI multiplier; stored verbatim into the
        // field below and only re-used by the rasterizer-target helper.
        let sf = window.scale_factor() as f32;
        let instance = shared.as_ref().map(|s| s.instance.clone()).unwrap_or_else(|| {
            Instance::new(InstanceDescriptor::new_with_display_handle_from_env(Box::new(
                event_loop.owned_display_handle(),
            )))
        });
        let surface = instance.create_surface(window.clone()).context("create surface")?;
        let (adapter, device, queue, software_rendering) = if let Some(shared) = shared {
            let info = shared.adapter.get_info();
            let software_rendering = detect_software_rendering(&info);
            let device_memory_policy = device_memory_policy_from(software_rendering);
            tracing::info!(
                backend = ?info.backend,
                name = %info.name,
                driver = %info.driver,
                device_type = ?info.device_type,
                software_rendering,
                device_memory_policy = ?device_memory_policy,
                "wgpu adapter reused"
            );
            (shared.adapter, shared.device, shared.queue, software_rendering)
        } else {
            // When: `shared` is None — this is the first window, so it
            // enumerates adapters and opens the device later windows reuse.
            let adapter = instance
                .request_adapter(&RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: Some(&surface),
                    force_fallback_adapter: false,
                    apply_limit_buckets: false,
                })
                .await
                .map_err(|e| anyhow!("no suitable GPU adapter: {e}"))?;
            let info = adapter.get_info();
            let software_rendering = detect_software_rendering(&info);
            let device_memory_policy = device_memory_policy_from(software_rendering);
            tracing::info!(
                backend = ?info.backend,
                name = %info.name,
                driver = %info.driver,
                device_type = ?info.device_type,
                software_rendering,
                device_memory_policy = ?device_memory_policy,
                "wgpu adapter selected"
            );
            if software_rendering {
                tracing::warn!(
                    adapter = %info.name,
                    "No hardware GPU — wgpu fell back to a software rasterizer (CPU). \
                     Rendering will be degraded to stay responsive (lower frame cap, \
                     no fade animation). Common cause: RDP / VM without GPU passthrough. \
                     See [appearance].software_render_mode."
                );
            }
            if matches!(info.backend, wgpu::Backend::Gl) {
                tracing::warn!(
                    adapter = %info.name,
                    "GPU backend is GLES — rendering may differ from native D3D12/Metal. \
                     Glyph sharpness, Powerline anchoring, and HiDPI snap may behave \
                     unexpectedly. Common cause: running over RDP without GPU passthrough."
                );
            }
            let (device, queue) = adapter
                .request_device(&device_descriptor_for(software_rendering))
                .await
                .context("request device")?;
            (adapter, device, queue, software_rendering)
        };

        let format = TextureFormat::Bgra8UnormSrgb;
        let max_surface_dimension =
            device.limits().max_texture_dimension_2d.min(MAX_SURFACE_DIMENSION);
        let validated_size =
            validated_surface_size(size.width, size.height, max_surface_dimension).ok_or_else(
                || {
                    anyhow!(
                        "window surface {}x{} exceeds renderer limits (max dimension {}, max BGRA bytes {})",
                        size.width,
                        size.height,
                        max_surface_dimension,
                        MAX_SURFACE_BYTES
                    )
                },
            )?;
        tracing::debug!(
            target: "memory",
            requested_width = size.width,
            requested_height = size.height,
            width = validated_size.width,
            height = validated_size.height,
            bgra_bytes = validated_size.bytes,
            software_rendering,
            "renderer initial surface allocation accepted"
        );
        // Prefer Mailbox when the backend exposes it: Mailbox drops in-flight
        // superseded frames so a fast-typing user always sees the newest
        // keystroke without waiting a full vblank. Fall back to Fifo on
        // backends that don't advertise Mailbox (Fifo is universally supported
        // and remains the spec-mandated default).
        let surface_caps = surface.get_capabilities(&adapter);
        let hardware_present_mode = if surface_caps.present_modes.contains(&PresentMode::Mailbox) {
            PresentMode::Mailbox
        } else {
            PresentMode::Fifo
        };
        let hardware_alpha_mode = if appearance.backdrop == BackdropKind::Opaque {
            CompositeAlphaMode::Opaque
        } else {
            CompositeAlphaMode::PreMultiplied
        };
        let software_render_degrade =
            software_render_degrade_from(appearance.software_render_mode, software_rendering);
        let present_mode =
            if software_render_degrade { PresentMode::Fifo } else { hardware_present_mode };
        let alpha_mode =
            if software_render_degrade { CompositeAlphaMode::Opaque } else { hardware_alpha_mode };
        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: validated_size.width,
            height: validated_size.height,
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: if software_render_degrade { 1 } else { 2 },
        };
        surface.configure(&device, &config);

        // B3 GPU text path. Allocate independent glyph and inline-image
        // atlases up front so media pressure cannot recycle text UVs.
        // No more SwashRasterizer
        // prebake — chrome and grid share the glyph atlas, populated
        // on demand by the wezterm rasterizer on every miss.
        let present_pipeline = WeztermPipeline::new(&device, format, 4096);
        let (frame_texture, frame_view) =
            create_frame_texture(&device, config.width, config.height, format);
        let frame_blitter = wgpu::util::TextureBlitter::new(&device, format);
        let glyph_atlas = GlyphAtlas::default_size();
        let image_atlas = GlyphAtlas::new(PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM);
        let software_presenter = cfg!(target_os = "windows") && software_render_degrade;
        let glyph_gpu_dimensions = desired_gpu_atlas_dimensions(software_presenter, &glyph_atlas);
        let image_gpu_dimensions = desired_gpu_atlas_dimensions(software_presenter, &image_atlas);
        let glyph_upload = AtlasUpload::new_sized(
            &device,
            glyph_gpu_dimensions.0,
            glyph_gpu_dimensions.1,
            present_pipeline.texture_bind_group_layout(),
        );
        let image_upload = AtlasUpload::new_sized(
            &device,
            image_gpu_dimensions.0,
            image_gpu_dimensions.1,
            present_pipeline.texture_bind_group_layout(),
        );
        tracing::debug!(
            target: "memory",
            renderer_role = role,
            window_id = ?window.id(),
            software_presenter,
            glyph_cpu_width = glyph_atlas.width(),
            glyph_cpu_height = glyph_atlas.height(),
            glyph_cpu_payload_bytes = atlas_payload_bytes(glyph_atlas.width(), glyph_atlas.height()),
            glyph_gpu_width = glyph_gpu_dimensions.0,
            glyph_gpu_height = glyph_gpu_dimensions.1,
            glyph_gpu_payload_bytes = glyph_upload.payload_bytes(),
            image_cpu_width = image_atlas.width(),
            image_cpu_height = image_atlas.height(),
            image_cpu_payload_bytes = atlas_payload_bytes(image_atlas.width(), image_atlas.height()),
            image_gpu_width = image_gpu_dimensions.0,
            image_gpu_height = image_gpu_dimensions.1,
            image_gpu_payload_bytes = image_upload.payload_bytes(),
            glyph_resident = glyph_atlas.len(),
            image_resident = image_atlas.len(),
            retained_inline_media_bytes = 0,
            payload_estimate = true,
            "renderer atlas payload initialized"
        );

        // G1a (T2) + T13: cell metrics come from sonicterm-font in raster
        // px directly. The cosmic-text `measure_cell` fallback is gone
        // — when the FontStack fails to load (test fixtures without
        // bundled fonts) we fall back to a font-size-derived guess
        // (`font_size * 0.6, font_size * 1.2`) that's close enough to
        // keep test fixtures rendering at a sensible aspect ratio.
        // FontStack DPI: sonicterm-font computes px_per_em = point_size *
        // dpi / 72. Pass `dpi = 72 * scale_factor` so the raster cell
        // metrics match the renderer's raster-px coordinate system.
        // Font size in points equals sonicterm's logical font_size.
        let fs_dpi = (72.0 * sf).round() as usize;
        let font_stacks =
            renderer_font_stacks(font_family, font_size, fs_dpi, font_weight_scale, font_dirs);
        let (cell_w, natural_cell_h) =
            match font_stacks.body.as_ref().and_then(|s| s.cell_metrics_raster_px().ok()) {
                Some(m) => (m.cell_w as f32, m.cell_h as f32),
                None => (font_size * 0.6 * sf, font_size * 1.2 * sf),
            };
        let line_height = natural_cell_h * line_height_mult.max(0.0).max(0.01);
        let cell_h = line_height;

        let bg = hex_to_wgpu_with_alpha(theme.colors.background.0.as_str(), appearance.opacity);
        let bg_rgba = hex_to_premultiplied_rgba(theme.colors.background.0.as_str(), 1.0);
        let fg_default = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        let cursor_color = cursor_color_from_theme(theme);
        let cursor_text_color = cursor_text_color_from_theme(theme);
        let selection_color = hex_to_premultiplied_rgba(theme.colors.selection_bg.0.as_str(), 0.5);
        let tab_bar_bg = hex_to_premultiplied_rgba(theme.colors.tab.bar_bg.0.as_str(), 1.0);
        let tab_active_bg = hex_to_premultiplied_rgba(theme.colors.tab.active_bg.0.as_str(), 1.0);
        let tab_inactive_bg =
            hex_to_premultiplied_rgba(theme.colors.tab.inactive_bg.0.as_str(), 1.0);
        let tab_active_fg = hex_to_chrome_color(theme.colors.tab.active_fg.0.as_str());
        let tab_inactive_fg = hex_to_chrome_color(theme.colors.tab.inactive_fg.0.as_str());
        let tab_separator =
            hex_to_premultiplied_rgba(theme.colors.tab.inactive_fg.0.as_str(), 0.45);
        // Hyperlink visuals: theme-aware. Use the theme's cursor color as the
        // accent (every bundled theme designates it). Underline reads as
        // deliberate at high opacity; the tint behind the run is subtle.
        let hyperlink_underline = hex_to_premultiplied_rgba(theme.colors.cursor.0.as_str(), 0.9);
        let splitter_color = splitter_color_from_theme(theme);
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
        let hyperlink_tint = hex_to_premultiplied_rgba(theme.colors.cursor.0.as_str(), tint_alpha);
        let search_highlight =
            hex_to_premultiplied_rgba(theme.colors.bright.yellow.0.as_str(), 0.35);
        let search_fg = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        let search_bg = hex_to_premultiplied_rgba(theme.colors.tab.bar_bg.0.as_str(), 0.95);
        // Cosmic-text Buffer / Metrics allocations deleted.
        // Chrome strings are shape+raster'd on demand inside `render()`
        // through `chrome_text::layout(...)`; there is no persistent
        // per-overlay text buffer to size at construction.

        // Counted here rather than earlier in `new`: every `?` above this
        // point returns without producing a renderer, so incrementing sooner
        // would charge for instances that never existed and never drop.
        LIVE_RENDERERS.fetch_add(1, Ordering::AcqRel);

        Ok(Self {
            instance,
            adapter,
            software_rendering,
            software_render_degrade,
            device,
            queue,
            surface,
            config,
            hardware_present_mode,
            hardware_alpha_mode,
            window,
            present_pipeline,
            frame_texture,
            frame_view,
            frame_blitter,
            glyph_atlas,
            glyph_upload,
            glyph_atlas_generation: 0,
            image_atlas,
            image_upload,
            retained_inline_media_bytes: 0,
            frames_without_inline_media: 0,
            glyph_atlas_retry_without_eviction: false,
            font_family: font_family.to_string(),
            font_dirs: font_dirs.to_vec(),
            font_size,
            line_height,
            font_weight_scale,
            line_height_mult: line_height_mult.max(0.0).max(0.01),
            scale_factor: sf,
            cell_w,
            cell_h,
            padding_left,
            padding_right,
            padding_top,
            padding_bottom,
            bg,
            bg_opacity: appearance.opacity.clamp(0.0, 1.0),
            scrollbar_mode: appearance.scrollbar,
            panel_padding: appearance.panel_padding.max(0.0),
            fg_default,
            cursor_color,
            cursor_text_color,
            bg_rgba,
            cursor_shape: CursorShape::default(),
            cursor_blink: true,
            blink_epoch: Instant::now(),
            window_focused: true,
            inactive_pane_cursors: Vec::new(),
            pane_focus_flash: None,
            selection_color,
            tab_bar_bg,
            tab_active_bg,
            tab_inactive_bg,
            tab_active_fg,
            tab_inactive_fg,
            tab_close_override: None,
            hover_cursor: None,
            tab_separator,
            hyperlink_underline,
            splitter_color,
            hyperlink_tint,
            search_highlight,
            search_fg,
            search_bg,
            drag_chip_visual: None,
            last_frame_key: None,
            preedit_glyph_cache: None,
            skipped_frames: 0,
            successful_frame_count: 0,
            #[cfg(target_os = "windows")]
            software_frame: None,
            render_timing_label: role,
            tab_bar_visible: true,
            titlebar_inset: 0.0,
            last_missing_chars: Vec::new(),
            // `shape_cache` field deleted with the cosmic-text path.
            font_stack: font_stacks.body,
            tab_title_font_stack: font_stacks.tab_title,
            palette_footer_font_stack: font_stacks.palette_footer,
            row_glyph_cache: sonicterm_text::row_glyph_cache::RowGlyphCache::new(),
            line_quad_cache: crate::row_quad_cache::LineQuadCache::new(),
            last_emit_origins: Vec::new(),
            last_pane_layout: Vec::new(),
            style_rev: 0,
            drag_chip: None,
            async_loader: None,
        })
    }

    /// Checked resize used by window-event paths that must react to rejection.
    #[must_use]
    pub fn try_resize(&mut self, width: u32, height: u32) -> bool {
        let max_dimension =
            self.device.limits().max_texture_dimension_2d.min(MAX_SURFACE_DIMENSION);
        let Some(size) = validated_surface_size(width, height, max_dimension) else {
            // When: `validated_surface_size` returns None. The old surface
            // stays configured — refusing is recoverable, resizing is not.
            tracing::error!(
                target: "memory",
                requested_width = width,
                requested_height = height,
                current_width = self.config.width,
                current_height = self.config.height,
                max_dimension,
                max_bgra_bytes = MAX_SURFACE_BYTES,
                "renderer rejected unsafe surface resize"
            );
            return false;
        };
        if self.config.width == size.width && self.config.height == size.height {
            // When: the validated size equals the configured one — common on
            // scale events. Reconfiguring would drop both caches for nothing.
            return true;
        }
        tracing::debug!(
            target: "memory",
            window = self.render_timing_label,
            old_width = self.config.width,
            old_height = self.config.height,
            requested_width = width,
            requested_height = height,
            width = size.width,
            height = size.height,
            bgra_bytes = size.bytes,
            software_rendering = self.software_rendering,
            "renderer surface resize accepted"
        );
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
        let (frame_texture, frame_view) = create_frame_texture(
            &self.device,
            self.config.width,
            self.config.height,
            self.config.format,
        );
        self.frame_texture = frame_texture;
        self.frame_view = frame_view;
        // Geometry change → force the next frame to actually render.
        self.last_frame_key = None;
        // Cell layout and absolute-row positioning both change with
        // the surface size; cached glyph instances would land at the
        // wrong NDC coordinates.
        self.row_glyph_cache.invalidate_all();
        self.line_quad_cache.invalidate_all();
        // Post-glyphon there is no persistent text buffer to
        // resize — chrome strings are re-shaped through
        // `chrome_text::layout` on every frame, picking up the new
        // surface dims via the per-call `(sw, sh)` parameter. The
        // legacy `*_buffer.set_size(...)` block that lived here is
        // gone with the glyphon plumbing.
        true
    }

    /// Top inset reserved above the grid: OS titlebar band (when active)
    /// plus top window padding, returned in **raster px** so it lives in
    /// the same coordinate system as `config.width`/`config.height` and the
    /// rest of the renderer post-G1a. The tab bar is always bottom-pinned,
    /// so its height is reserved via [`Self::bottom_inset`] instead of here.
    ///
    /// `titlebar_inset` and `padding_top` are stored in logical px (matching
    /// the config schema); both are scaled by [`Self::scale_factor`] before
    /// being summed so a 2x Retina display gets the right number of raster
    /// rows reserved for the OS titlebar band + user padding. Without the
    /// scale the grid was reporting one fewer row than the window could fit,
    /// leaving a dead strip below the last painted row that showed the
    /// surface clear color instead of vim's bg.
    pub fn top_inset(&self) -> f32 {
        (self.titlebar_inset + self.padding_top) * self.scale_factor
    }

    /// Bottom inset reserved below the grid for the bottom-pinned tab bar,
    /// in **raster px** (same units as `config.height`). Returns 0 when
    /// the bar is hidden; the consumer still subtracts `padding_bottom *
    /// scale_factor` separately so window padding still applies when the
    /// bar is off.
    pub fn bottom_inset(&self) -> f32 {
        if self.tab_bar_visible {
            self.tab_bar_logical_height()
        } else {
            // When: `!tab_bar_visible` — the bar reserves nothing. Window
            // padding is applied separately by the caller.
            0.0
        }
    }

    /// Y offset (in raster px) at which the tab bar layout should be
    /// anchored. The tab bar is always pinned to the bottom of the window.
    /// Callers pass this into [`TabBarLayout::with_top_offset`].
    pub fn tab_bar_y_offset(&self) -> f32 {
        let surf_h = self.config.height as f32;
        (surf_h - self.tab_bar_logical_height()).max(0.0)
    }

    /// Raster-pixel height of the tab bar for the renderer's current font
    /// size. Derived from [`tab_bar_height`] (logical formula) and scaled
    /// to raster px to live in the same coordinate system as
    /// `config.width`/`config.height` and the rest of the renderer
    /// post-G1a. WezTerm fancy-mode parity: `font_size × 2 + 12` clamped.
    pub fn tab_bar_logical_height(&self) -> f32 {
        tab_bar_height(self.font_size) * self.scale_factor
    }

    /// The titlebar inset alone (logical px) — the y-offset at which the
    /// tab bar strip itself begins, regardless of whether the bar is
    /// visible. Used by hit-testing / tab-bar layout to shift their
    /// rectangles down so clicks under the OS titlebar are not consumed
    /// as tab activations.
    pub fn titlebar_inset(&self) -> f32 {
        self.titlebar_inset
    }

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
    }

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
        true
    }

    /// Whether the tab bar is currently shown.
    pub fn tab_bar_visible(&self) -> bool {
        self.tab_bar_visible
    }

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

    /// Current scrollbar visibility policy. Test-only inspector for the
    /// live-reload path; production code pushes updates via
    /// [`Self::set_scrollbar_mode`].
    #[doc(hidden)]
    pub fn scrollbar_mode(&self) -> sonicterm_render_model::boundary::cfg::config::ScrollbarMode {
        self.scrollbar_mode
    }

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

    /// Update the cursor shape. Invalidates the cached frame so the
    /// next render redraws with the new geometry.
    pub fn set_cursor_shape(&mut self, shape: CursorShape) {
        if self.cursor_shape == shape {
            // When: `self.cursor_shape == shape` — every config reload calls
            // this, so clearing `last_frame_key` would redraw for no change.
            return;
        }
        self.cursor_shape = shape;
        self.last_frame_key = None;
    }

    /// Current cursor shape.
    pub fn cursor_shape(&self) -> CursorShape {
        self.cursor_shape
    }

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
        self.last_frame_key = None;
    }

    /// Whether the cursor is currently configured to blink.
    pub fn cursor_blink(&self) -> bool {
        self.cursor_blink
    }

    /// Suggested wall-clock interval between blink-only redraws. The
    /// app loop schedules a redraw at this cadence whenever the cursor
    /// is visible AND [`Self::cursor_blink`] is true; otherwise nothing
    /// new would render and the request would be wasted.
    pub fn blink_redraw_interval(&self) -> std::time::Duration {
        ui_cursor::redraw_interval()
    }

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

    /// Update the cached "is the OS window focused" flag. Hides the
    /// text cursor when `false`. Bumps the FrameKey via
    /// `Self::last_frame_key` so the next render is not skipped by
    /// the cache.
    /// Host-side storage this renderer holds, split by the class that owns it.
    ///
    /// Every figure here already existed and was unreachable from outside the
    /// crate: `GlyphAtlas::retained_amount` and
    /// `WindowsSoftwareFrame::retained_bytes` were both written, tested, and
    /// called by nothing. What was missing was a way for the owner of the
    /// governor to read them, which is what this provides.
    ///
    /// **CPU-side only.** GPU textures and buffers are not included: their
    /// memory belongs to the driver, `wgpu` exposes no size accounting for
    /// them, and a figure invented here would be a guess presented as a
    /// measurement. The atlases are the CPU mirrors that back those textures,
    /// so they track the same content without claiming to measure VRAM.
    #[must_use]
    pub fn retained_amounts(&self) -> RendererRetention {
        RendererRetention {
            glyph_atlas: self.glyph_atlas.retained_amount(),
            image_atlas: self.image_atlas.retained_amount(),
            software_frame: self.software_frame_retained_amount(),
        }
    }

    #[cfg(windows)]
    fn software_frame_retained_amount(&self) -> ResourceAmount {
        self.software_frame.as_ref().map_or_else(ResourceAmount::default, |frame| ResourceAmount {
            bytes: frame.retained_bytes(),
            items: usize::from(frame.retained_bytes() > 0),
        })
    }

    /// Non-Windows builds have no software presentation path, so this is
    /// always zero rather than absent — a caller charging it should not need
    /// a platform branch to do so.
    #[cfg(not(windows))]
    fn software_frame_retained_amount(&self) -> ResourceAmount {
        ResourceAmount::default()
    }

    /// Update the cached keyboard-focus flag for the OS window.
    ///
    /// The text cursor is hidden while the window is unfocused, so a change
    /// here alters what the next frame paints and invalidates the frame key.
    pub fn set_window_focused(&mut self, focused: bool) {
        if self.window_focused == focused {
            // When: `window_focused == focused` — winit re-delivers focus
            // events, and clearing the key would redraw for no visible change.
            return;
        }
        self.window_focused = focused;
        self.last_frame_key = None;
    }

    /// Whether the OS window currently has keyboard focus.
    pub fn window_focused(&self) -> bool {
        self.window_focused
    }

    /// Set the window label that renderer-internal timing logs are tagged with.
    ///
    /// Affects diagnostics only; no frame state changes, so the frame key is
    /// deliberately left intact.
    pub fn set_render_timing_label(&mut self, label: &'static str) {
        self.render_timing_label = label;
    }

    /// Start the short focus-confirmation flash on `pane_id`.
    ///
    /// The flash is bounded by `PANE_FOCUS_FLASH_DURATION` and animates
    /// through the frame key's quantised bucket, so this requests one redraw
    /// rather than starting a repeating timer.
    pub fn flash_pane_focus(&mut self, pane_id: u64) {
        self.pane_focus_flash = Some((pane_id, Instant::now()));
        self.last_frame_key = None;
        self.window.request_redraw();
    }

    /// Accept the historical per-frame inactive-pane cursor list.
    /// Inactive panes no longer draw cursors, so any previously cached
    /// cursor records are cleared and new records are ignored.
    pub fn set_inactive_pane_cursors(&mut self, _cursors: Vec<InactivePaneCursor>) {
        if !self.inactive_pane_cursors.is_empty() {
            self.inactive_pane_cursors.clear();
            self.last_frame_key = None;
        }
    }

    fn pane_focus_flash_bucket(&mut self, now: Instant) -> u8 {
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

    fn pane_focus_flash_alpha(&self, now: Instant) -> Option<(u64, f32)> {
        let (pane_id, started_at) = self.pane_focus_flash?;
        let elapsed = now.saturating_duration_since(started_at);
        pane_focus_flash_sample(elapsed).map(|(_, alpha)| (pane_id, alpha))
    }

    /// Return the pane targeted by the live focus flash, for integration diagnostics.
    #[doc(hidden)]
    pub fn __test_pane_focus_flash_target(&self) -> Option<u64> {
        self.pane_focus_flash.map(|(pane_id, _)| pane_id)
    }

    /// Current physical surface width in pixels.
    pub fn width(&self) -> u32 {
        self.config.width
    }

    /// Current physical surface height in pixels.
    pub fn height(&self) -> u32 {
        self.config.height
    }

    /// Left padding (logical px). Kept for backward compatibility with
    /// callers that pre-date per-side padding; new code should prefer
    /// the per-side accessors below.
    pub fn padding(&self) -> f32 {
        self.padding_left
    }

    /// Left padding in logical pixels.
    pub fn padding_left(&self) -> f32 {
        self.padding_left
    }
    /// Right padding in logical pixels.
    pub fn padding_right(&self) -> f32 {
        self.padding_right
    }
    /// Top padding in logical pixels (above any tab bar / titlebar inset).
    pub fn padding_top(&self) -> f32 {
        self.padding_top
    }
    /// Bottom padding in logical pixels.
    pub fn padding_bottom(&self) -> f32 {
        self.padding_bottom
    }

    /// Left padding scaled to **raster px**, i.e. the same coordinate
    /// system as `config.width`/`config.height` and the rest of the
    /// renderer post-G1a. Prefer this over [`Self::padding_left`] when
    /// building geometry that will be handed back to the renderer (e.g.
    /// the per-pane rect in `compute_pane_rects_for`). Mixing the
    /// logical-px accessor with raster surface dims off-by-ones the row
    /// count and leaves a dead strip below the last painted row.
    pub fn padding_left_px(&self) -> f32 {
        self.padding_left * self.scale_factor
    }
    /// Right padding scaled to raster px. See [`Self::padding_left_px`].
    pub fn padding_right_px(&self) -> f32 {
        self.padding_right * self.scale_factor
    }
    /// Top padding scaled to raster px. See [`Self::padding_left_px`].
    /// Note: [`Self::top_inset`] already returns raster px (it bakes in
    /// the titlebar inset + this value); callers that want the full
    /// "y-origin of the grid" should use `top_inset()`, not this raw
    /// padding alone.
    pub fn padding_top_px(&self) -> f32 {
        self.padding_top * self.scale_factor
    }
    /// Bottom padding scaled to raster px. See [`Self::padding_left_px`].
    pub fn padding_bottom_px(&self) -> f32 {
        self.padding_bottom * self.scale_factor
    }
    /// Per-pane origins recorded by the most recent `render()` call, as
    /// `(pane_id, [origin_x_px, origin_y_px])`. Test-only hook for the
    /// Part B step 7 per-pane render integration test. Production code
    /// must not depend on this.
    #[doc(hidden)]
    pub fn last_emitted_origins(&self) -> Vec<(u64, [f32; 2])> {
        self.last_emit_origins.clone()
    }

    /// Translate a scrollback-absolute row into the row index visible in the
    /// current viewport. Returns `None` when the row lies above or below the
    /// rendered viewport.
    #[doc(hidden)]
    pub fn viewport_relative_row(
        absolute_row: usize,
        view_top_abs: u64,
        visible_rows: u16,
    ) -> Option<u16> {
        let visible_row = absolute_row as i128 - i128::from(view_top_abs);
        (0..i128::from(visible_rows)).contains(&visible_row).then_some(visible_row as u16)
    }

    /// Resolve the viewport top used by the renderer after clamping explicit
    /// scrollback requests to the live bottom.
    #[doc(hidden)]
    pub fn resolved_view_top_abs(grid: &Grid, viewport_top_abs: Option<u64>) -> u64 {
        let live_top_abs = grid.scrollback_len() as u64;
        viewport_top_abs.map(|v| v.min(live_top_abs)).unwrap_or(live_top_abs)
    }

    /// Legacy-Grid variant kept for sonicterm-app call sites that still
    /// hold an `Arc<Mutex<Parser>>` and want to ask viewport questions
    /// of the parser's grid. Identical algorithm to the GridFacade
    /// version; both will collapse to one helper once sonicterm-app
    /// stops carrying the legacy parser.
    #[doc(hidden)]
    pub fn resolved_view_top_abs_legacy(
        grid: &sonicterm_render_model::boundary::grid::grid::Grid,
        viewport_top_abs: Option<u64>,
    ) -> u64 {
        let live_top_abs = grid.scrollback_len() as u64;
        viewport_top_abs.map(|v| v.min(live_top_abs)).unwrap_or(live_top_abs)
    }

    /// Adjust a viewport after copy-mode movement so the scrollback-absolute
    /// copy-mode cursor remains visible.
    #[doc(hidden)]
    pub fn copy_mode_view_top_after_move(
        copy_mode: &CopyModeState,
        grid: &Grid,
        viewport_top_abs: Option<u64>,
    ) -> Option<u64> {
        let view_top_abs = Self::resolved_view_top_abs(grid, viewport_top_abs);
        let cursor_row = copy_mode.cursor.1 as u64;
        let viewport_height = u64::from(grid.rows);
        if cursor_row < view_top_abs {
            Some(cursor_row)
        } else if cursor_row >= view_top_abs.saturating_add(viewport_height) {
            // When: `cursor_row >= view_top_abs + viewport_height` — cursor
            // below the viewport; scroll so it lands on the last row.
            Some(cursor_row.saturating_add(1).saturating_sub(viewport_height))
        } else {
            // When: `cursor_row` is already inside the viewport — return the
            // caller's `viewport_top_abs` so an explicit scroll survives.
            viewport_top_abs
        }
    }

    /// Legacy-Grid variant. See `resolved_view_top_abs_legacy`.
    #[doc(hidden)]
    pub fn copy_mode_view_top_after_move_legacy(
        copy_mode: &CopyModeState,
        grid: &sonicterm_render_model::boundary::grid::grid::Grid,
        viewport_top_abs: Option<u64>,
    ) -> Option<u64> {
        let view_top_abs = Self::resolved_view_top_abs_legacy(grid, viewport_top_abs);
        let cursor_row = copy_mode.cursor.1 as u64;
        let viewport_height = u64::from(grid.rows);
        if cursor_row < view_top_abs {
            Some(cursor_row)
        } else if cursor_row >= view_top_abs.saturating_add(viewport_height) {
            // When: `cursor_row >= view_top_abs + viewport_height` — cursor
            // below the viewport; scroll so it lands on the last row.
            Some(cursor_row.saturating_add(1).saturating_sub(viewport_height))
        } else {
            // When: `cursor_row` is already visible — return the caller's
            // `viewport_top_abs` so an explicit scroll position survives.
            viewport_top_abs
        }
    }

    /// Emit copy-mode selection and cursor quads using scrollback-absolute
    /// copy-mode coordinates translated into viewport-relative rows.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn emit_copy_mode_quads(
        copy_mode: &CopyModeState,
        grid: &Grid,
        view_top_abs: u64,
        origin_x: f32,
        origin_y: f32,
        cell_w: f32,
        cell_h: f32,
        sw: f32,
        sh: f32,
        selection_color: [f32; 4],
        cursor_color: [f32; 4],
        quads: &mut Vec<QuadInstance>,
        snapped_cell_x: &[f32],
    ) -> Option<(f32, f32)> {
        // derive selection-row x/w and copy-cursor cx from the
        // shared snapped-edge cache so copy-mode overlays share
        // device-pixel edges with adjacent glyph cells at fractional
        // DPI. Empty-cache fallback preserves the raw arithmetic for
        // callers (debug/test helpers) that don't carry a real cache;
        // integer scales make the two identical via the identity fast
        // path in `snap_to_device_pixels`.
        let raw_fallback = snapped_cell_x.is_empty();
        if let Some((start, end)) = copy_mode.selected_range() {
            // When: `copy_mode.selected_range()` is Some — copy mode has an
            // anchored selection. The cursor quad below is emitted regardless.
            for row_abs in start.1..=end.1 {
                let Some(visible_row) =
                    Self::viewport_relative_row(row_abs, view_top_abs, grid.rows)
                else {
                    // When: `viewport_relative_row` is None — a selection may
                    // span scrollback, so off-screen rows are skipped.
                    continue;
                };
                let col_a = if row_abs == start.1 {
                    start.0
                } else {
                    // When: `row_abs != start.1` — an interior or final row,
                    // which starts at column 0, not the selection anchor.
                    0
                }
                .min(grid.cols as usize);
                let col_b = if row_abs == end.1 {
                    end.0.min(grid.cols.saturating_sub(1) as usize)
                } else {
                    // When: `row_abs != end.1` — an interior row of a
                    // multi-row selection, which runs to the right edge.
                    grid.cols.saturating_sub(1) as usize
                };
                if col_b < col_a {
                    // When: `col_b < col_a` — an empty span on this row, which
                    // would still push a zero-width quad into the buffer.
                    continue;
                }
                let end_exclusive = col_b + 1;
                let (x, w) = if raw_fallback {
                    (origin_x + col_a as f32 * cell_w, (end_exclusive - col_a) as f32 * cell_w)
                } else {
                    // When: `!raw_fallback` — a real snapped-edge cache, so the
                    // quad takes device-pixel edges shared with glyph cells.
                    let cache_end = end_exclusive.min(snapped_cell_x.len() - 1);
                    let lo = snapped_cell_x[col_a];
                    let hi = snapped_cell_x[cache_end];
                    (lo, hi - lo)
                };
                let y = origin_y + f32::from(visible_row) * cell_h;
                quads.push(QuadInstance {
                    rect: px_to_ndc(x, y, w, cell_h, sw, sh),
                    color: selection_color,
                    ..Default::default()
                });
            }
        }

        if copy_mode.is_read_only() {
            // When: `copy_mode.is_read_only()` — a read-only view has no
            // editable cursor to draw, so only the selection quads above stand.
            return None;
        }

        let visible_row = Self::viewport_relative_row(copy_mode.cursor.1, view_top_abs, grid.rows)?;
        let copy_col = copy_mode.cursor.0.min(grid.cols.saturating_sub(1) as usize);
        let (cx, cw) = if raw_fallback {
            (origin_x + copy_col as f32 * cell_w, cell_w)
        } else {
            // When: `!raw_fallback` — a real snapped-edge cache, so the cursor
            // takes the same edges as the glyph cell beneath it.
            let lo = snapped_cell_x[copy_col];
            let hi = snapped_cell_x[(copy_col + 1).min(snapped_cell_x.len() - 1)];
            (lo, hi - lo)
        };
        let cy = origin_y + f32::from(visible_row) * cell_h;
        quads.push(QuadInstance {
            rect: px_to_ndc(cx, cy, cw, cell_h, sw, sh),
            color: cursor_color,
            ..Default::default()
        });
        Some((cx, cy))
    }

    /// Fix 1 test hook: number of panes the most recent
    /// `render()` call received in its slice. The integration test
    /// asserts this equals the active tab's pane count so a regression
    /// to a single-element slice (the original bug) is caught
    /// mechanically. Production code must not depend on this.
    #[doc(hidden)]
    pub fn last_panes_received(&self) -> usize {
        self.last_emit_origins.len()
    }

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
    }

    /// Raster-pixel size of the render surface. Post-G1a (wezterm-takeover)
    /// the pane layout, padding, top inset, and cell metrics are all
    /// raster px too, so this is just `(config.width, config.height)`
    /// cast to `f32`. Name kept for back-compat with callers that
    /// were once unit-mixing.
    pub fn logical_size(&self) -> (f32, f32) {
        (self.config.width as f32, self.config.height as f32)
    }

    /// Snapshot of every codepoint the previous `render()` call could
    /// not produce a glyph tile for (i.e. that drew a tofu outline).
    /// Whitespace is filtered out — those are intentionally blank.
    ///
    /// Test-only diagnostic. Production code MUST NOT depend on this
    /// surface — it exists so the renderer-capability matrix can
    /// assert "no character class regressed" without sniffing pixels
    /// off the swapchain. Doc-hidden to keep it out of the public
    /// rustdoc; still `pub` so integration tests under `tests/` can
    /// reach it.
    #[doc(hidden)]
    pub fn last_missing_tofu(&self) -> &[char] {
        &self.last_missing_chars
    }

    /// Current grid dimensions in `(cols, rows)`. G1a: surface dims +
    /// cell_w / cell_h all share the raster-px coordinate system, so
    /// this is plain integer division — no DPI reconciliation step.
    ///
    /// Padding is stored in **logical px** (matching the config schema),
    /// so each side is scaled by [`Self::scale_factor`] before being
    /// subtracted from the raster-px surface dims. `top_inset()` and
    /// `bottom_inset()` already return raster px, so they're subtracted
    /// raw. Without the per-side scale the row count was off by ~1 on 2x
    /// Retina, which left a dead strip below the last painted row.
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

    /// Logical cell metrics (width, height) in CSS pixels. Pair with a
    /// `sonicterm_render_model::boundary::ui::pane::Rect` from `PaneTree::layout` to compute how many
    /// cells fit in that rect: `cols = (rect.w / cell_w).floor()`,
    /// similarly rows.
    ///
    /// Returned values are positive (the renderer asserts a positive glyph
    /// advance at font load).
    pub fn cell_size(&self) -> (f32, f32) {
        (self.cell_w, self.cell_h)
    }

    /// Number of frames that completed a native presentation successfully.
    ///
    /// Skipped, occluded, outdated, lost, and failed frames do not advance it.
    #[must_use]
    pub fn successful_frame_count(&self) -> u64 {
        self.successful_frame_count
    }

    /// Current font family in effect. Test-only inspector for the
    /// live-reload path; production code reads font fields directly.
    #[doc(hidden)]
    pub fn font_family(&self) -> &str {
        &self.font_family
    }

    /// Current font size in px.
    #[doc(hidden)]
    pub fn font_size(&self) -> f32 {
        self.font_size
    }

    /// Measure overlay text in raster pixels with the renderer's active font
    /// stack, falling back conservatively when shaping is unavailable.
    pub fn measure_overlay_text_width(&self, text: &str, font_size: f32) -> f32 {
        let estimate = estimate_badge_text_width(text, font_size);
        conservative_badge_text_width(
            estimate,
            self.font_stack.as_ref().and_then(|stack| stack.measure_text_width(text).ok()),
        )
    }

    /// Return current allocator totals when the selected backend exposes them.
    ///
    /// `None` means allocator reporting is unavailable; it does not represent
    /// an allocator with zero usage.
    #[must_use]
    pub fn allocator_snapshot(&self) -> Option<AllocatorSnapshot> {
        allocator_snapshot_from_report(self.device.generate_allocator_report())
    }

    /// True when wgpu fell back to a CPU/software rasterizer for this window.
    /// The app uses this to degrade frame pacing and per-frame animation in
    /// the no-GPU case.
    pub fn is_software_rendering(&self) -> bool {
        self.software_rendering
    }

    /// Whether the no-GPU degrade path is active for this window.
    ///
    /// Distinct from [`Self::is_software_rendering`]: that reports what the
    /// adapter is, this reports the resolved policy after
    /// `[appearance].software_render_mode` is applied, so `Force` degrades on
    /// real hardware and `Off` declines to degrade on a CPU rasterizer.
    pub fn is_software_render_degraded(&self) -> bool {
        self.software_render_degrade
    }

    /// Read one BGRA pixel out of the Windows software presentation buffer.
    ///
    /// Test-only inspector for the software path: it is the only way to assert
    /// what that path actually wrote without a GPU readback. Returns `None`
    /// when no software frame is allocated or the coordinates fall outside it.
    #[cfg(target_os = "windows")]
    #[doc(hidden)]
    pub fn __test_software_frame_pixel_bgra(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        self.software_frame.as_ref()?.pixel_bgra_at(x, y)
    }

    /// Update the resolved no-GPU degrade state after a config reload.
    /// A transition invalidates the retained frame and reconfigures the
    /// surface with the software-render present tweaks.
    pub fn set_software_render_degrade(&mut self, degrade: bool) {
        if self.software_render_degrade == degrade {
            // When: `software_render_degrade == degrade`. The body below
            // reconfigures the surface and may drop both atlases.
            return;
        }
        let used_software_presenter = self.uses_windows_software_presenter();
        self.software_render_degrade = degrade;
        if degrade {
            // Fifo, opaque compositing, and one frame of latency trade
            // smoothness for the lowest CPU cost per presented frame.
            self.config.present_mode = PresentMode::Fifo;
            self.config.alpha_mode = CompositeAlphaMode::Opaque;
            self.config.desired_maximum_frame_latency = 1;
        } else {
            // When: leaving degrade. The modes captured at construction are
            // restored, so a backend without Mailbox does not acquire it here.
            self.config.present_mode = self.hardware_present_mode;
            self.config.alpha_mode = self.hardware_alpha_mode;
            self.config.desired_maximum_frame_latency = 2;
            #[cfg(target_os = "windows")]
            {
                self.software_frame = None;
            }
        }
        self.surface.configure(&self.device, &self.config);
        let uses_software_presenter = self.uses_windows_software_presenter();
        if used_software_presenter != uses_software_presenter {
            // The software and GPU presenters size their atlas textures
            // differently, so cached UVs do not survive the transition.
            self.row_glyph_cache.invalidate_all();
            self.line_quad_cache.invalidate_all();
            if !uses_software_presenter {
                // GPU textures must grow from the placeholder dimensions the
                // software path left behind.
                self.reset_glyph_atlas_in_place("software_to_gpu");
                self.reset_image_atlas();
                self.glyph_atlas_retry_without_eviction = false;
            }
            self.rebuild_glyph_upload_if_needed();
            self.rebuild_image_upload_if_needed();
            tracing::debug!(
                target: "memory",
                renderer_role = self.render_timing_label,
                window_id = ?self.window.id(),
                software_presenter = uses_software_presenter,
                glyph_gpu_width = self.glyph_upload.width(),
                glyph_gpu_height = self.glyph_upload.height(),
                glyph_gpu_payload_bytes = self.glyph_upload.payload_bytes(),
                image_gpu_width = self.image_upload.width(),
                image_gpu_height = self.image_upload.height(),
                image_gpu_payload_bytes = self.image_upload.payload_bytes(),
                glyph_resident = self.glyph_atlas.len(),
                image_resident = self.image_atlas.len(),
                payload_estimate = true,
                "renderer software/GPU atlas transition"
            );
        }
        self.last_frame_key = None;
        self.window.request_redraw();
    }

    fn uses_windows_software_presenter(&self) -> bool {
        cfg!(target_os = "windows") && self.software_render_degrade
    }

    /// Current OS display scale factor (physical px per logical px). Exposed so
    /// the app layer can scale window-event geometry (e.g. the search-bar IME
    /// caret rect) to match the renderer's physical-px coordinate space.
    pub fn scale_factor(&self) -> f32 {
        self.scale_factor
    }

    /// Number of glyph tiles currently resident in the rasterizer atlas.
    /// Test-only; the atlas is cleared and rebuilt by [`Self::set_font`].
    #[doc(hidden)]
    pub fn glyph_atlas_len(&self) -> usize {
        self.glyph_atlas.len()
    }

    /// Apply a new font family / size / line-height multiplier without
    /// reconstructing the renderer.
    ///
    /// The shelf-packed glyph atlas is cleared because existing tiles
    /// are sized for the old metrics — reusing them would render at the
    /// wrong pixel scale. The frame-key cache is also invalidated so
    /// Set (or clear) the translucent drag-chip overlay drawn on top
    /// of the frame. Called by the app on every CursorMoved during a
    /// held-tab drag, and with `None` on release.
    pub fn set_drag_chip(&mut self, chip: Option<DragChipOverlay>) {
        self.drag_chip = chip;
        // Bust the frame-key cache so a new chip position is actually
        // drawn — otherwise the no-change fast path would short-circuit.
        self.last_frame_key = None;
    }

    /// Active drag chip overlay (if any). Read-only accessor used by
    /// tests and the app event loop to inspect the live chip state.
    pub fn drag_chip(&self) -> Option<&DragChipOverlay> {
        self.drag_chip.as_ref()
    }

    /// Diagnostic — visual rect of the most recently rendered drag
    /// chip, or `None` if no chip was drawn. Test-only.
    #[doc(hidden)]
    pub fn last_drag_chip_visual(&self) -> Option<DragChipVisual> {
        self.drag_chip_visual
    }

    /// Update the renderer's view of where the cursor is, in LOGICAL
    /// pixels (origin top-left). Drives WezTerm fancy-mode close-button
    /// hover behaviour — when the cursor is over a tab, the dim × is
    /// shown; when it's over the × itself the glyph brightens to
    /// `tab_active_fg`. Pass `None` when the cursor leaves the window.
    ///
    /// Returns `true` when the change could affect tab-bar rendering
    /// (the previous or new cursor position falls inside the tab-bar
    /// row, or the cursor left while previously over the bar). The
    /// app uses this signal to request a redraw — without it a bare
    /// hover-only move never triggers `render()` and the muted ×
    /// stays stale until the next event nudges the loop.
    pub fn set_hover_cursor(&mut self, pos: Option<(f32, f32)>) -> bool {
        if self.hover_cursor == pos {
            // When: `hover_cursor == pos`. This runs on every `CursorMoved`, so
            // clearing the key here would defeat the frame cache during a drag.
            return false;
        }
        let prev = self.hover_cursor;
        self.hover_cursor = pos;
        self.last_frame_key = None;
        self.hover_change_touches_tab_bar(prev, pos)
    }

    /// True when either the old or new logical cursor position falls
    /// inside the tab-bar band. Used by `set_hover_cursor` to decide
    /// whether a pure mouse-move warrants a redraw request.
    fn hover_change_touches_tab_bar(
        &self,
        prev: Option<(f32, f32)>,
        next: Option<(f32, f32)>,
    ) -> bool {
        if !self.tab_bar_visible {
            // When: `!tab_bar_visible` — no bar on screen, so no position can
            // be over one and no move changes tab chrome.
            return false;
        }
        let inset = self.tab_bar_y_offset();
        let bar_h = self.tab_bar_logical_height();
        let in_bar = |p: Option<(f32, f32)>| -> bool {
            match p {
                // Only the y axis matters — the bar spans the window's width.
                Some((_, y)) => y >= inset && y <= inset + bar_h,
                // Pointer outside the window. The caller ORs the previous
                // position, so leaving the bar still reports a change.
                None => false,
            }
        };
        in_bar(prev) || in_bar(next)
    }

    fn reset_glyph_atlas_after_eviction(&mut self, frame_epoch: u64) {
        let current_epoch = self.glyph_atlas.evictions();
        let resident = self.glyph_atlas.len();
        let hits = self.glyph_atlas.hits();
        let misses = self.glyph_atlas.misses();
        let width = self.glyph_atlas.width();
        let height = self.glyph_atlas.height();
        tracing::warn!(
            target: "sonic::glyph_atlas",
            frame_epoch,
            current_epoch,
            resident,
            hits,
            misses,
            width,
            height,
            "glyph atlas evicted during frame assembly; rebuilding before presentation"
        );

        self.reset_glyph_atlas_in_place("eviction_compaction");
        self.glyph_atlas.set_eviction_enabled(false);
        self.row_glyph_cache.invalidate_all();
        self.glyph_atlas_retry_without_eviction = true;
        self.last_frame_key = None;
        self.window.request_redraw();
    }

    fn glyph_atlas_epoch(&self) -> GlyphAtlasEpoch {
        GlyphAtlasEpoch {
            generation: self.glyph_atlas_generation,
            evictions: self.glyph_atlas.evictions(),
        }
    }

    fn mark_glyph_atlas_replaced(&mut self) {
        self.glyph_atlas_generation = self.glyph_atlas_generation.wrapping_add(1);
        self.preedit_glyph_cache = None;
    }

    fn reset_glyph_atlas_in_place(&mut self, reason: &'static str) {
        let width = self.glyph_atlas.width();
        let height = self.glyph_atlas.height();
        self.glyph_atlas.reset_in_place();
        self.mark_glyph_atlas_replaced();
        tracing::debug!(
            target: "memory",
            renderer_role = self.render_timing_label,
            window_id = ?self.window.id(),
            software_presenter = self.uses_windows_software_presenter(),
            atlas = "glyph",
            reason,
            width,
            height,
            cpu_payload_bytes = atlas_payload_bytes(width, height),
            gpu_width = self.glyph_upload.width(),
            gpu_height = self.glyph_upload.height(),
            gpu_payload_bytes = self.glyph_upload.payload_bytes(),
            resident = self.glyph_atlas.len(),
            retained_inline_media_bytes = self.retained_inline_media_bytes,
            retained_pixel_allocation = true,
            payload_estimate = true,
            "renderer atlas reset in place"
        );
    }

    fn reset_image_atlas(&mut self) {
        let width = self.image_atlas.width();
        let height = self.image_atlas.height();
        self.image_atlas.reset_in_place();
        tracing::debug!(
            target: "memory",
            renderer_role = self.render_timing_label,
            window_id = ?self.window.id(),
            software_presenter = self.uses_windows_software_presenter(),
            atlas = "image",
            width,
            height,
            cpu_payload_bytes = atlas_payload_bytes(width, height),
            gpu_width = self.image_upload.width(),
            gpu_height = self.image_upload.height(),
            gpu_payload_bytes = self.image_upload.payload_bytes(),
            resident = self.image_atlas.len(),
            retained_inline_media_bytes = self.retained_inline_media_bytes,
            retained_pixel_allocation = true,
            payload_estimate = true,
            "renderer atlas reset in place"
        );
    }

    /// Release a full-size image atlas once the window has drawn without any
    /// renderable inline media for [`IMAGE_ATLAS_IDLE_FRAMES`].
    ///
    /// Without this, promotion is permanent: a window that displays a single
    /// image keeps 16 MiB of CPU pixels — and, on the GPU path, a matching
    /// texture — until it closes, however long ago the image scrolled away.
    /// Across several windows that is the largest retained term in the
    /// process.
    ///
    /// Nothing the user can see changes. The atlas is rebuilt on demand the
    /// next time an image becomes visible, which is the same work the first
    /// promotion does.
    fn demote_image_atlas_if_idle(&mut self, has_inline_media: bool) {
        if has_inline_media {
            // When: `has_inline_media` — the atlas is in use. The idle run
            // resets to zero; demotion needs a sustained absence, not a net one.
            self.frames_without_inline_media = 0;
            return;
        }
        self.frames_without_inline_media = self.frames_without_inline_media.saturating_add(1);
        if !image_atlas_demotion_ready(
            &self.image_atlas,
            has_inline_media,
            self.frames_without_inline_media,
        ) {
            // When: `image_atlas_demotion_ready` is false — still placeholder-
            // sized, or the idle run is short. Early demotion thrashes 16 MiB.
            return;
        }

        let released_width = self.image_atlas.width();
        let released_height = self.image_atlas.height();
        self.image_atlas = GlyphAtlas::new(PLACEHOLDER_ATLAS_DIM, PLACEHOLDER_ATLAS_DIM);
        if !self.uses_windows_software_presenter() {
            // A GPU texture mirrors the atlas and must shrink with it.
            self.rebuild_image_upload_if_needed();
        }
        self.frames_without_inline_media = 0;
        tracing::debug!(
            target: "memory",
            renderer_role = self.render_timing_label,
            window_id = ?self.window.id(),
            software_presenter = self.uses_windows_software_presenter(),
            atlas = "image",
            released_width,
            released_height,
            released_cpu_bytes = atlas_payload_bytes(released_width, released_height),
            gpu_width = self.image_upload.width(),
            gpu_height = self.image_upload.height(),
            idle_frames = IMAGE_ATLAS_IDLE_FRAMES,
            "image atlas released after sustained absence of inline media"
        );
    }

    fn promote_image_atlas_if_needed(
        &mut self,
        has_inline_media: bool,
        retained_inline_media_bytes: usize,
    ) -> bool {
        if !image_atlas_promotion_required(&self.image_atlas, has_inline_media) {
            // When: `!image_atlas_promotion_required` — no media to draw, or
            // already promoted. `false` keeps the caller's cached UVs valid.
            return false;
        }
        self.image_atlas = GlyphAtlas::default_size();
        if !self.uses_windows_software_presenter() {
            // The GPU texture must grow to match the promoted atlas before
            // anything samples it.
            self.rebuild_image_upload_if_needed();
        }
        tracing::debug!(
            target: "memory",
            renderer_role = self.render_timing_label,
            window_id = ?self.window.id(),
            software_presenter = self.uses_windows_software_presenter(),
            atlas = "image",
            width = self.image_atlas.width(),
            height = self.image_atlas.height(),
            cpu_payload_bytes = atlas_payload_bytes(self.image_atlas.width(), self.image_atlas.height()),
            gpu_width = self.image_upload.width(),
            gpu_height = self.image_upload.height(),
            gpu_payload_bytes = self.image_upload.payload_bytes(),
            resident = self.image_atlas.len(),
            retained_inline_media_bytes,
            payload_estimate = true,
            "inline image atlas promoted"
        );
        true
    }

    /// Deprecated close-button color override. The button is no longer
    /// drawn, but accepting the setting keeps older configs harmless.
    pub fn set_tab_close_override(&mut self, color: Option<&str>) -> bool {
        let parsed = color.map(|c| hex_to_premultiplied_rgba(c, 1.0));
        if self.tab_close_override != parsed {
            self.tab_close_override = parsed;
            self.last_frame_key = None;
            true
        } else {
            // When: `self.tab_close_override == parsed` — the override is
            // unchanged, so `false` tells the caller nothing needs redrawing.
            false
        }
    }

    /// the next `render()` call cannot short-circuit through the
    /// fast-path against a now-stale frame.
    pub fn set_font(&mut self, family: &str, size: f32, line_height_mult: f32, weight_scale: f32) {
        let weight_scale = effective_font_weight_scale(weight_scale);
        let dpi = (72.0 * self.scale_factor).round().max(1.0) as usize;
        let new_stacks = renderer_font_stacks(family, size, dpi, weight_scale, &self.font_dirs);
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
        self.font_stack = new_stacks.body;
        self.tab_title_font_stack = new_stacks.tab_title;
        self.palette_footer_font_stack = new_stacks.palette_footer;
        self.cell_w = new_cell_w;
        self.cell_h = new_line_h;
        self.reset_glyph_atlas_in_place("font_change");
        self.glyph_atlas_retry_without_eviction = false;
        // SwashRasterizer prebake gone. Atlas is now lazily
        // filled by the wezterm rasterizer on the next render.
        self.row_glyph_cache.invalidate_all();
        self.line_quad_cache.invalidate_all();
        self.last_frame_key = None;
        tracing::info!(
            "renderer.set_font: family={family} size={size} line_h={} cell={:.2}x{:.2}",
            self.line_height,
            self.cell_w,
            self.cell_h
        );
    }

    /// Apply a new DPI scale factor without reconstructing the renderer.
    ///
    /// G1a: this used to drive a logical-vs-physical projection at draw
    /// time too. Post-takeover it only governs the rasterizer target
    /// inside `Self::raster_px`, so cell metrics are recomputed from
    /// `FontStack::cell_metrics_raster_px` whenever the rasterizer
    /// target changes — there is no longer a "logical cell pitch
    /// independent of DPI" because the renderer's coordinate system
    /// IS raster pixels.
    pub fn set_scale_factor(&mut self, scale_factor: f32) {
        if !scale_factor_rebuild_required(self.scale_factor, scale_factor) {
            // When: `!scale_factor_rebuild_required` — the DPI is unchanged
            // within epsilon, and `rebuild_for_sf` re-rasterizes every glyph.
            return;
        }
        self.rebuild_for_sf(scale_factor);
    }

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

    /// G1a: single helper that owns the rasterizer-px target derived
    /// from `font_size * DPI`. Every callsite (grid + chrome) routes
    /// a logical font size through here to obtain the raster-px
    /// em-size the [`SwashRasterizer`] expects.
    #[inline]
    fn raster_px(&self, font_size: f32) -> f32 {
        font_size * self.scale_factor
    }

    /// Scale a logical-px chrome constant into the renderer's physical/raster-px
    /// coordinate space. Chrome layout literals (badge/search-bar/palette sizes,
    /// paddings, radii, sub-cell thicknesses) are authored at scale-factor 1.0;
    /// route them through this so they track the display DPI like glyphs do.
    /// Window-anchored POSITIONS (edge margins, centering offsets) must NOT use
    /// this — they stay in window space. See.
    #[inline]
    fn chrome_px(&self, logical: f32) -> f32 {
        logical * self.scale_factor
    }

    fn rebuild_for_sf(&mut self, sf: f32) {
        let sf = sf.max(0.1);
        self.scale_factor = sf;
        // Post-glyphon the atlas is sized once at default
        // and grows on demand; no DPI-derived resize and no
        // SwashRasterizer prebake. The wezterm rasterizer fills the
        // atlas lazily on first encounter with each glyph.
        self.reset_glyph_atlas_in_place("dpi_change");
        self.glyph_atlas_retry_without_eviction = false;
        // G1a: cell metrics are raster px end-to-end, so re-pull them
        // from sonicterm-font when the rasterizer target moves. Falls
        // back to the prior measurement if the font stack rejects the
        // load (e.g. test fixtures without bundled fonts).
        // DPI fix: the atlas + caches above are cleared, but glyphs would
        // otherwise re-rasterize through stacks still holding the prior DPI.
        let fs_dpi = (72.0 * sf).round() as usize;
        for stack in [
            self.font_stack.as_ref(),
            self.tab_title_font_stack.as_ref(),
            self.palette_footer_font_stack.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
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
        // The GPU-side AtlasUpload owns a texture sized to the old atlas
        // dimensions and a bind group pointing at it. After replacing the
        // CPU `GlyphAtlas` with one of a different size, the next
        // `glyph_upload.sync(...)` would either write out-of-bounds or
        // sample tiles at stale UVs. Rebuild the upload so its texture +
        // bind group match the new atlas dimensions exactly.
        self.rebuild_glyph_upload_if_needed();
        self.last_frame_key = None;
        if let Some(w) = Some(&self.window) {
            w.request_redraw();
        }
        tracing::info!(
            "renderer.rebuild_for_sf: sf={sf} atlas={}x{} raster_px={}",
            self.glyph_atlas.width(),
            self.glyph_atlas.height(),
            self.raster_px(self.font_size),
        );
    }

    fn rebuild_glyph_upload_if_needed(&mut self) {
        let current = (self.glyph_upload.width(), self.glyph_upload.height());
        let next =
            desired_gpu_atlas_dimensions(self.uses_windows_software_presenter(), &self.glyph_atlas);
        if atlas_texture_rebuild_required(current, next) {
            self.glyph_upload = AtlasUpload::new_sized(
                &self.device,
                next.0,
                next.1,
                self.present_pipeline.texture_bind_group_layout(),
            );
        }
    }

    fn rebuild_image_upload_if_needed(&mut self) {
        let current = (self.image_upload.width(), self.image_upload.height());
        let next =
            desired_gpu_atlas_dimensions(self.uses_windows_software_presenter(), &self.image_atlas);
        if atlas_texture_rebuild_required(current, next) {
            self.image_upload = AtlasUpload::new_sized(
                &self.device,
                next.0,
                next.1,
                self.present_pipeline.texture_bind_group_layout(),
            );
        }
    }

    fn log_atlas_upload_stats(
        &self,
        atlas: &'static str,
        stats: AtlasUploadStats,
        retained_inline_media_bytes: usize,
    ) {
        if stats.dirty_rects == 0 {
            // When: `stats.dirty_rects == 0` — no atlas region changed, so the
            // upload was a no-op and logging it would bury the real ones.
            return;
        }
        tracing::debug!(
            target: "memory",
            renderer_role = self.render_timing_label,
            window_id = ?self.window.id(),
            software_presenter = self.uses_windows_software_presenter(),
            atlas,
            dirty_rects = stats.dirty_rects,
            upload_calls = stats.upload_calls,
            uploaded_bytes = stats.uploaded_bytes,
            retained_inline_media_bytes,
            glyph_resident = self.glyph_atlas.len(),
            image_resident = self.image_atlas.len(),
            "renderer atlas upload synchronized"
        );
    }

    /// Apply a new color theme without reconstructing the renderer.
    /// Recomputes every cached wgpu / glyphon color derived from the
    /// theme so the next frame reflects the swap.
    pub fn set_theme(&mut self, theme: &Theme) {
        self.set_theme_with_opacity(theme, self.bg_opacity);
    }

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
        self.last_frame_key = None;
        self.style_rev = self.style_rev.wrapping_add(1);
        self.row_glyph_cache.invalidate_all();
        self.line_quad_cache.invalidate_all();
        tracing::info!("renderer.set_theme: {}", theme.name);
    }

    /// Drop every shape/row/line cache and bump `style_rev` so the next
    /// frame re-shapes from scratch. Called from the winit event loop
    /// in response to `UserEvent::ClearShapeCache` — itself fired by
    /// the `sonicterm_text::async_fallback::AsyncFallbackLoader`
    /// notifier when a CJK/emoji family finishes loading off the hot
    /// startup path.
    ///
    /// Without this method, freshly loaded fallback faces would not
    /// take effect until something else invalidated the caches
    /// (theme change, font reload, etc.) — the user would keep
    /// seeing tofu boxes for an arbitrary amount of time after the
    /// font finished loading.
    ///
    /// The per-style-run `ShapeCache`
    /// was deleted in T8; the only surviving caches the async loader
    /// notifier needs to invalidate are the per-row + per-line
    /// quad caches plus the style_rev bump.
    pub fn clear_shape_cache(&mut self) {
        self.row_glyph_cache.invalidate_all();
        self.line_quad_cache.invalidate_all();
        self.style_rev = self.style_rev.wrapping_add(1);
        self.last_frame_key = None;
        tracing::info!(
            "renderer.clear_shape_cache (async fallback notifier) style_rev={}",
            self.style_rev
        );
    }

    /// Test/diagnostic peek at the renderer's monotonic style
    /// revision. The counter is opaque; tests only care that it
    /// *changes* on theme / `clear_shape_cache` calls.
    #[doc(hidden)]
    #[must_use]
    pub fn style_rev(&self) -> u64 {
        self.style_rev
    }

    /// Attach point for the legacy async font fallback loader.
    /// Stub today — sonicterm-font handles fallback synchronously via its
    /// built-in vendor chain, so the loader is a no-op `()`. Kept as
    /// `Option<()>` so the cross-crate API (`sonicterm-app` calls
    /// `set_async_loader(...)` on renderer construction) survives;
    /// the legacy `SwashRasterizer::set_async_loader` plumb is gone.
    pub fn set_async_loader(&mut self, _loader: ()) {
        self.async_loader = Some(());
    }

    /// Borrow the attached async loader, if any. Test/diagnostic only —
    /// used by `async_font_loader_attached_in_prod` to assert the
    /// production wiring actually plumbed the loader through.
    #[doc(hidden)]
    #[must_use]
    pub fn async_loader(&self) -> Option<&()> {
        self.async_loader.as_ref()
    }

    /// Translate physical-pixel `(px, py)` (as winit reports) into a
    /// `(row, col)` cell address inside the grid, or `None` if the point
    /// falls outside the grid (in the tab bar, padding, etc.).
    ///
    /// G1a: the renderer is raster px end-to-end, so winit's physical
    /// px IS our cell-grid coordinate system — no boundary divide.
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

    /// Translate physical pixels into the exact rendered pane and cell.
    ///
    /// The pane identity and cell coordinates come from one layout snapshot, so
    /// split-pane clicks cannot mix app geometry with device-pixel-snapped edges.
    pub fn pixel_to_pane_cell(&self, px: f32, py: f32) -> Option<(u64, u16, u16)> {
        // G1a: winit physical px == renderer raster px. Use raw.
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
            let x = px - self.padding_left * sf;
            let y = py - self.top_inset();
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
            return Some((0, row.min(u16::MAX as i32) as u16, col.min(u16::MAX as i32) as u16));
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
        let edges = build_snapped_cell_x(pane.origin_x_logical, pane.cell_w_logical, pane.cols);
        let col = pixel_to_local_col(px, &edges, pane.cols)?;
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

    // `render` threads borrowed app state plus one copyable process flag through
    // wgpu submission. A parameter struct would still need separate
    // borrow fields (no win over positional args) or force the App layer
    // to construct an interior-mutable wrapper around its own state —
    // both worse than the current shape. Keep the suppression beside this
    // borrow-shape rationale.
    /// Render one frame: terminal grid + cursor + selection + overlays
    /// (tab bar, search, command palette, IME preedit). Submits to the
    /// wgpu queue and presents the surface. See the parameter comments
    /// above for the lifetime / borrow rationale.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        panes: &mut [sonicterm_render_model::PaneRender<'_>],
        theme: &Theme,
        cursor_visible: bool,
        selection: Option<&Selection>,
        copy_mode: Option<&CopyModeState>,
        tabs: &TabBar,
        process_privileged: bool,
        search: Option<&SearchState>,
        palette: Option<&mut CommandPalette>,
        ime: Option<&ImeState>,
        viewport_top_abs: Option<u64>,
        notification: Option<&NotificationBubble>,
        // Cmd-hovered auto-detected URL cell range (viewport coords),
        // or `None` when no URL is hovered while the open-URL modifier
        // is held. When set on the active pane, the URL's glyphs are
        // recolored with the theme accent (companion to the existing
        // hover underline). Same lifetime/gating as the underline.
        hovered_url_cells: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
    ) -> Result<()> {
        // Part B step 2: signature now takes &mut [PaneRender]. Behavior is
        // unchanged inside the body — we extract the active pane's grid into
        // the local `grid` binding and derive `pane_rects` / `active_pane`
        // from the slice. The mechanical re-anchor of the 62
        // `padding_left`/`top_inset()` sites to per-pane origins is tracked
        // separately. If all panes failed to lock (empty slice), skip the
        // frame — callers are expected to filter dropped locks before calling.
        if panes.is_empty() {
            // When: `panes.is_empty()` — every pane's lock was dropped by the
            // caller, so there is no grid to read and the frame is skipped.
            return Ok(());
        }
        let mut gpu_timing = tracing::enabled!(target: "render_timing", tracing::Level::DEBUG)
            .then(|| {
                let now = Instant::now();
                (now, now, Vec::<(&'static str, f32)>::with_capacity(12))
            });
        macro_rules! gpu_lap {
            ($name:literal) => {
                if let Some((_, last, parts)) = gpu_timing.as_mut() {
                    let now = Instant::now();
                    parts
                        .push(($name, now.saturating_duration_since(*last).as_secs_f32() * 1000.0));
                    *last = now;
                }
            };
        }
        let now = Instant::now();
        // Part B step 7: record per-pane origins for the integration test
        // hook. Populated unconditionally on every render() call so the
        // test can assert that all panes' origins reach the renderer with
        // the expected x/y in physical pixels.
        let content_inset_l = self.padding_left_px();
        let content_inset_r = self.padding_right_px();
        let content_inset_t = self.padding_top_px();
        let content_inset_b = self.padding_bottom_px();
        let content_rect = |p: &sonicterm_render_model::PaneRender<'_>| {
            let x = p.rect_px.x as f32 + content_inset_l;
            let y = p.rect_px.y as f32 + content_inset_t;
            let w = (p.rect_px.w as f32 - content_inset_l - content_inset_r).max(self.cell_w);
            let h = (p.rect_px.h as f32 - content_inset_t - content_inset_b).max(self.cell_h);
            (x, y, w, h)
        };
        self.last_emit_origins = panes
            .iter()
            .map(|p| {
                let (x, y, _, _) = content_rect(p);
                (p.id, [x, y])
            })
            .collect();
        // per-pane raster-px layout snapshot for the pane-aware
        // hit-test in `pixel_to_cell`. PaneRender::rect_px is raster
        // px (winit physical-px is the same coordinate system post-G1a),
        // so the snapshot reads directly from `rect_px` with no scale
        // projection.
        let cell_w_log = self.cell_w;
        let cell_h_log = self.cell_h;
        self.last_pane_layout = panes
            .iter()
            .map(|p| {
                let (x, y, w, h) = content_rect(p);
                PaneLayoutSnapshot {
                    id: p.id,
                    origin_x_logical: x,
                    origin_y_logical: y,
                    w_logical: w,
                    h_logical: h,
                    cell_w_logical: cell_w_log,
                    cell_h_logical: cell_h_log,
                    cols: p.grid.cols,
                    rows: p.grid.rows,
                }
            })
            .collect();
        let active_idx = panes.iter().position(|p| p.is_active).unwrap_or(0);
        let active_pane: u64 = panes[active_idx].id;
        // Derive the legacy `pane_rects` vector from the slice so downstream
        // code (cache key, focus-ring quad, etc.) continues to work
        // unchanged. PaneRender::rect_px is already in physical px adjusted
        // for top_inset — same units as the old PaneRect.
        let pane_rects: Vec<(u64, PaneRect)> = panes
            .iter()
            .map(|p| {
                (
                    p.id,
                    PaneRect {
                        x: p.rect_px.x as f32,
                        y: p.rect_px.y as f32,
                        w: p.rect_px.w as f32,
                        h: p.rect_px.h as f32,
                    },
                )
            })
            .collect();
        let pane_rects = pane_rects.as_slice();
        let broadcast_receiver_ids: Vec<u64> =
            panes.iter().filter(|p| p.is_broadcast_receiver).map(|p| p.id).collect();
        // Collect immutable per-pane views for ALL panes so the cell-emission
        // body below can iterate per-pane. The grid is borrowed shared
        // (`&Grid`) — every read in the loop (`scrollback_len`, `dirty_rows`,
        // `row_at_abs`, `rows`, `cursor`, `prompts`) is immutable, so neither
        // `&mut Grid` nor raw pointers are needed. Taking `&mut Grid` per pane
        // would overlap borrows across panes sharing one grid.
        struct PaneView<'g> {
            grid: &'g Grid,
            pane_id: u64,
            origin_x: f32,
            origin_y: f32,
            // Pane rect width/height in pixels — the source of truth for
            // pane geometry. Do NOT recompute as `grid.cols * cell_w`
            // for clipping bounds: when the pane has just been resized
            // but the grid hasn't yet been resynced (resize is debounced
            // through the PTY) the derived value is smaller than the
            // real pane rect and overlay quads at the trailing edge get
            // clipped away, allowing terminal content to bleed through.
            rect_w: f32,
            rect_h: f32,
            /// The pane's FULL rect, padding included, in pixels.
            ///
            /// Distinct from `origin_*`/`rect_*` above, which are the content
            /// rect and drive cell layout. Damage must use this one: a glyph
            /// with a negative left side bearing at column 0 paints left of
            /// its cell and into the padding band, and a damage rect built
            /// from the content rect never covers those columns again. The
            /// pixel then survives every later frame, including a full
            /// alt-screen pane repaint.
            full_rect: PixelRect,
            is_active: bool,
            viewport_top_abs: Option<u64>,
            scrollbar_alpha: f32,
            inline_images: &'g [sonicterm_render_model::InlineImage],
        }
        let pane_views: Vec<PaneView<'_>> = panes
            .iter()
            .map(|p| PaneView {
                grid: &*p.grid,
                pane_id: p.id,
                origin_x: content_rect(p).0,
                origin_y: content_rect(p).1,
                rect_w: content_rect(p).2,
                rect_h: content_rect(p).3,
                full_rect: p.rect_px,
                is_active: p.is_active,
                viewport_top_abs: p.viewport_top_abs,
                scrollbar_alpha: p.scrollbar_alpha,
                inline_images: &p.inline_images,
            })
            .collect();
        // Pre-compute pane revisions for FrameKey from the safe borrows.
        let pane_revs_vec: Vec<(u64, u64, Option<u64>)> = pane_views
            .iter()
            .map(|pv| (pv.pane_id, pv.grid.revision(), pv.viewport_top_abs))
            .collect();
        let pane_scrollbar_alpha = pane_scrollbar_identity(
            self.scrollbar_mode,
            pane_views.iter().map(|pane| {
                (pane.pane_id, pane.grid.scrollback_len(), pane.grid.rows, pane.scrollbar_alpha)
            }),
        );
        let retained_inline_media_bytes = pane_views
            .iter()
            .flat_map(|view| view.inline_images)
            .fold(0usize, |total, image| total.saturating_add(image.bgra.len()));
        self.retained_inline_media_bytes = retained_inline_media_bytes;
        let inline_media_hash = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            for pv in &pane_views {
                pv.pane_id.hash(&mut h);
                pv.inline_images.len().hash(&mut h);
                for img in pv.inline_images {
                    img.id.hash(&mut h);
                    img.row.hash(&mut h);
                    img.col.hash(&mut h);
                    img.width.hash(&mut h);
                    img.height.hash(&mut h);
                }
            }
            h.finish()
        };
        // Active pane's origin. Selection / cursor / overlays anchor to
        // this — they apply only to the focused pane (Part B step 4 /
        // Fix 3). Lifting these out as plain `f32` makes the overlay
        // sites below borrow-free.
        let active_view_idx = pane_views.iter().position(|p| p.is_active).unwrap_or(0);
        let active_origin_x: f32 = pane_views[active_view_idx].origin_x;
        let active_origin_y: f32 = pane_views[active_view_idx].origin_y;
        // Active pane rect (px) — used to clip every overlay quad anchored
        // to the active pane (selection, cursor, hyperlink hover, search
        // matches, IME preedit) so a quad that would otherwise extend past
        // the pane edge never bleeds into a neighbouring split pane.
        // See (selection clipping) — same overflow class for the
        // other overlay families is handled here.
        let active_pane_x: f32 = active_origin_x;
        let active_pane_y: f32 = active_origin_y;
        // Use the pane's own rect_px width/height (the source of truth
        // for pane geometry) rather than `grid.cols * cell_w`. After a
        // pane resize the grid resync is debounced through the PTY;
        // during that window the derived extent is *smaller* than the
        // real pane rect, which would clip overlays inside the trailing
        // edge and allow terminal content to bleed through.
        let active_pane_w: f32 = pane_views[active_view_idx].rect_w;
        let active_pane_h: f32 = pane_views[active_view_idx].rect_h;
        // Active grid borrow — shared, used by overlays that read the
        // active pane's cursor/scrollback/prompts. Disjoint from the
        // per-pane loop (which uses its own per-iteration borrow).
        let grid: &Grid = pane_views[active_view_idx].grid;

        // Advance the atlas frame counter so LRU eviction can
        // distinguish glyphs touched this frame from cold ones. Cheap
        // (one integer increment) and unconditional — even on a fully
        // cached frame the bump is harmless and keeps the counter in
        // step with wall-clock frames for diagnostic dumps.
        self.glyph_atlas.tick_frame();
        let atlas_epoch_at_frame_start = self.glyph_atlas.evictions();
        // Build a fingerprint of every input that can affect the rendered
        // pixels. If it matches the last frame, nothing on screen would
        // change — skip text shaping, quad rebuild and GPU submit.
        let search_hash = search
            .map(|s| {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                s.query.hash(&mut h);
                s.cursor().hash(&mut h);
                s.matches.len().hash(&mut h);
                s.current.hash(&mut h);
                h.finish()
            })
            .unwrap_or(0);
        // Per-component dirty flag for the command palette so that a
        // keystroke into the query box (which changes neither the grid
        // revision nor the active tab) still invalidates the cached frame.
        let palette_hash: u64 = palette
            .as_deref()
            .filter(|p| p.is_open())
            .map(|p| {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                // The open-bit is implicit in the filter above; mark with
                // a salt so closed→empty-query opens differ from a stale
                // hash.
                0xC0DE_FA17_u64.hash(&mut h);
                p.query().hash(&mut h);
                p.cursor().hash(&mut h);
                p.selected().hash(&mut h);
                p.len().hash(&mut h);
                p.scroll_offset().hash(&mut h);
                h.finish()
            })
            .unwrap_or(0);
        // Likewise for IME preedit — composition changes don't bump grid
        // revision until commit.
        let ime_hash: u64 = ime
            .map(|i| {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                i.preedit().hash(&mut h);
                i.is_composing().hash(&mut h);
                // Fold the composition caret too: the terminal cursor block now
                // tracks `i.cursor()` (the in-flight caret byte), so a caret move
                // WITHIN unchanged preedit text must still invalidate the frame —
                // otherwise the cursor would stick at the old caret position. #B14
                i.cursor().hash(&mut h);
                h.finish()
            })
            .unwrap_or(0);
        let notification_hash: u64 = notification
            .map(|n| {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                n.level.hash(&mut h);
                n.message.hash(&mut h);
                h.finish()
            })
            .unwrap_or(0);
        // Include every tab's title, order, activity, color, command status, and
        // foreground privilege so inactive-tab changes cannot leave stale chrome.
        let tab_hash = tab_bar_hash(tabs, now);
        let broadcast_receivers_hash: u64 = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            broadcast_receiver_ids.hash(&mut h);
            h.finish()
        };
        // Hash pane rects so split geometry changes invalidate the frame
        // even when the active pane id is unchanged.
        let pane_rect_hash: u64 = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            for (id, r) in pane_rects {
                id.hash(&mut h);
                (r.x.to_bits(), r.y.to_bits(), r.w.to_bits(), r.h.to_bits()).hash(&mut h);
            }
            h.finish()
        };
        let blink_elapsed = self.blink_epoch.elapsed();
        let blink_alpha = ui_cursor::blink_alpha(blink_elapsed, self.cursor_blink);
        // `phase_bucket` is intentionally NOT folded into the FrameKey
        // (see the `cursor_phase: 0` comment below). The alpha is
        // still computed every render so a real redraw event picks up
        // the current blink pulse.
        let _ = ui_cursor::phase_bucket(blink_elapsed, self.cursor_blink);
        // Compute hover state against the tab bar layout. Done before
        // the FrameKey is built so the cache invalidates as the cursor
        // moves between tabs.
        let hover_tab_idx = {
            let mut idx: u32 = u32::MAX;
            if self.tab_bar_visible {
                // When: `self.tab_bar_visible` — a hidden bar has no widgets to
                // hit-test, and `u32::MAX` already means "no tab hovered".
                if let Some((cx, cy)) = self.hover_cursor {
                    // When: `self.hover_cursor` is Some — the pointer is inside
                    // the window; `None` means it left and nothing is hovered.
                    let sw_log = self.config.width as f32;
                    let layout = TabBarLayout::compute_with_height(
                        tabs,
                        sw_log,
                        self.tab_bar_logical_height(),
                    )
                    .with_top_offset(self.tab_bar_y_offset());
                    for t in layout.tabwidgets() {
                        match t.hover_at(Some(
                            sonicterm_render_model::boundary::ui::tabbar_view::Point {
                                x: cx,
                                y: cy,
                            },
                        )) {
                            sonicterm_render_model::boundary::ui::tabbar_view::TabHover::None => {
                                // When: `TabHover::None` — the pointer misses
                                // this widget; later tabs are still tested.
                            }
                            sonicterm_render_model::boundary::ui::tabbar_view::TabHover::Body => {
                                // When: `TabHover::Body` — the pointer is over
                                // the tab itself. Tabs cannot overlap, so stop.
                                idx = t.idx as u32;
                                break;
                            }
                            sonicterm_render_model::boundary::ui::tabbar_view::TabHover::Close => {
                                // When: `TabHover::Close` — close buttons are
                                // no longer drawn, so this hover is ignored.
                            }
                        }
                    }
                }
            }
            idx
        };
        let quick_select_hint_count = copy_mode
            .and_then(|state| state.quick_select.as_ref())
            .map_or(0, |quick| quick.hints.len() as u32);
        let read_only_mode = copy_mode.is_some_and(CopyModeState::is_read_only);
        let pane_focus_flash_bucket = self.pane_focus_flash_bucket(now);
        let overlay_active = search.is_some()
            || palette.as_deref().is_some_and(CommandPalette::is_open)
            || notification.is_some()
            || ime.is_some_and(|i| i.is_composing() || !i.preedit().is_empty())
            || self.drag_chip.is_some()
            || self.pane_focus_flash.is_some();
        let scrollbar_changed = self
            .last_frame_key
            .as_ref()
            .is_none_or(|prev| prev.pane_scrollbar_alpha != pane_scrollbar_alpha);
        let overlay_or_chrome_changed = self.last_frame_key.as_ref().is_none_or(|prev| {
            scrollbar_changed
                || prev.selection != selection.copied()
                || prev.copy_mode != copy_mode.cloned()
                || prev.quick_select_hint_count != quick_select_hint_count
                || prev.cursor_visible != cursor_visible
                || prev.tab != tabs.active().map(|t| t.id.0).unwrap_or(0)
                || prev.pane != active_pane
                || prev.search_hash != search_hash
                || prev.palette_hash != palette_hash
                || prev.ime_hash != ime_hash
                || prev.notification_hash != notification_hash
                || prev.width != self.config.width
                || prev.height != self.config.height
                || prev.tab_hash != tab_hash
                || prev.pane_rect_hash != pane_rect_hash
                || prev.viewport_top_abs != viewport_top_abs
                || prev.cursor_shape != self.cursor_shape as u8
                || prev.cursor_blink != self.cursor_blink
                || prev.window_focused != self.window_focused
                || prev.pane_focus_flash_bucket != pane_focus_flash_bucket
                || prev.hover_tab != hover_tab_idx
                || prev.close_override != u8::from(self.tab_close_override.is_some())
                || prev.broadcast_receivers_hash != broadcast_receivers_hash
                || prev.inline_media_hash != inline_media_hash
                || prev.hovered_url_cells != hovered_url_cells
                || prev.process_privileged != process_privileged
        });
        let mut damage = DamageRect::empty();
        let surface_rect = full_surface_rect(self.config.width, self.config.height);
        let vertical_ink_pad = terminal_vertical_ink_pad(
            self.cell_h,
            self.font_stack.as_ref().and_then(|stack| stack.cell_metrics_raster_px().ok()),
        );
        if overlay_or_chrome_changed {
            damage.add_clipped(surface_rect, surface_rect);
        } else {
            // When: `!overlay_or_chrome_changed` — only grid content can have
            // changed, so damage narrows to the panes that reported it.
            for pv in &pane_views {
                // The pane's full rect, not the content rect. Glyph ink can
                // land in the padding band — a negative left side bearing at
                // column 0 reaches left of its cell — and a damage rect that
                // stops at the content edge never repaints those columns
                // again, so such a pixel survives every later frame.
                //
                // The dirty-row geometry below still uses the content origin,
                // because that is where the cell grid actually starts. Only
                // the bounding rectangle widens.
                let pane_rect = pv.full_rect;
                if let Some(rect) = pane_damage_rect_with_ink_pad(
                    pv.grid.is_alt(),
                    pv.grid.dirty_rows(),
                    pane_rect,
                    pv.origin_x,
                    pv.origin_y,
                    pv.grid.cols,
                    self.cell_w,
                    self.cell_h,
                    vertical_ink_pad,
                    self.config.width,
                    self.config.height,
                ) {
                    damage.add_clipped(rect, surface_rect);
                }
            }
        }
        let dirty_damage = damage.rect();
        let damaged_rows: usize = pane_views.iter().map(|pv| pv.grid.dirty_rows().count()).sum();

        let key = FrameKey {
            grid_revision: grid.revision(),
            pane_revs: pane_revs_vec,
            pane_scrollbar_alpha,
            selection: selection.copied(),
            copy_mode: copy_mode.cloned(),
            quick_select_hint_count,
            cursor_visible,
            tab: tabs.active().map(|t| t.id.0).unwrap_or(0),
            pane: active_pane,
            search_hash,
            palette_hash,
            ime_hash,
            notification_hash,
            width: self.config.width,
            height: self.config.height,
            tab_hash,
            pane_rect_hash,
            viewport_top_abs,
            cursor_shape: self.cursor_shape as u8,
            cursor_blink: self.cursor_blink,
            // NOTE: `cursor_phase` is deliberately NOT folded into the
            // FrameKey. Including it cracked the cache on every blink
            // bucket boundary, forcing a full grid re-shape ~26×/sec
            // and wedging idle CPU. The
            // cursor still re-evaluates its alpha on every real
            // render; between real renders the cursor sits at
            // whatever alpha it last drew at — a frozen but
            // always-visible cursor is better than a CPU-melting
            // blinking one.
            cursor_phase: 0,
            window_focused: self.window_focused,
            pane_focus_flash_bucket,
            hover_tab: hover_tab_idx,
            hover_close: 0,
            close_override: u8::from(self.tab_close_override.is_some()),
            broadcast_receivers_hash,
            inline_media_hash,
            hovered_url_cells,
            process_privileged,
        };
        let pane_revs_len = key.pane_revs.len();
        if Some(&key) == self.last_frame_key.as_ref() {
            // When: `key` equals `last_frame_key` — every input that can affect
            // the image is unchanged, so the presented frame is still correct.
            self.skipped_frames = self.skipped_frames.wrapping_add(1);
            tracing::trace!(skipped = self.skipped_frames, "renderer: skipped unchanged frame");
            #[cfg(target_os = "windows")]
            if self.software_render_degrade {
                if let Some(frame) = self.software_frame.as_ref() {
                    frame.present(&self.window)?;
                }
            }
            if pane_focus_flash_bucket != 0 {
                self.window.request_redraw();
            }
            // Blink redraws are now scheduled in the app event loop via
            // `next_blink_redraw_at()` + `ControlFlow::WaitUntil(..)`,
            // so we deliberately do NOT call `request_redraw()` here.
            // The earlier heartbeat reintroduced the project landmine
            // around feedback loops: two ticks in the same phase bucket
            // would re-arm at 0ms and peg the redraw queue.
            return Ok(());
        }
        let prev_key = self.last_frame_key.as_ref();
        let inline_media_changed =
            prev_key.is_none_or(|prev| prev.inline_media_hash != inline_media_hash);
        let render_mode = decide_render_mode(
            self.software_render_degrade,
            RenderSignals {
                first_frame: prev_key.is_none(),
                resize: prev_key.is_some_and(|prev| {
                    prev.width != self.config.width || prev.height != self.config.height
                }),
                dpi_or_scale_change: false,
                font_or_atlas_rebuild: false,
                theme_or_config_reload: false,
                surface_reconfigure: false,
                occlusion_restore: false,
                viewport_scroll: prev_key
                    .is_some_and(|prev| prev.viewport_top_abs != viewport_top_abs),
                selection_change: prev_key.is_some_and(|prev| prev.selection != selection.copied()),
                tab_switch: prev_key.is_some_and(|prev| {
                    prev.tab != tabs.active().map(|t| t.id.0).unwrap_or(0)
                        || prev.tab_hash != tab_hash
                }),
                pane_topology_change: prev_key.is_some_and(|prev| {
                    prev.pane != active_pane
                        || prev.pane_rect_hash != pane_rect_hash
                        || prev.pane_revs.len() != pane_revs_len
                }),
                scrollbar_change: scrollbar_changed,
                overlay_active_or_toggled: overlay_active || overlay_or_chrome_changed,
                degrade_state_changed: false,
                dirty_damage,
            },
        );
        if matches!(render_mode, RenderMode::Noop) {
            // When: `matches!(render_mode, RenderMode::Noop)` — nothing visible
            // changed. The key is stored so the next frame compares against it.
            self.last_frame_key = Some(key);
            return Ok(());
        }
        let emit_full_rows = matches!(render_mode, RenderMode::Full);
        gpu_lap!("frame_key");
        // Note: do NOT cache key here. If prepare()/get_current_texture()
        // fails on a transient surface state we'd cache a key for a frame
        // that never actually got drawn, and the next redraw could
        // early-exit silently. Cache only AFTER successful submit+present.

        // -------- B3 cutover: walk the grid once, emit one glyph
        // instance per visible cell, route every miss through the
        // swash rasterizer + atlas. No per-row cache, no rich-text
        // buffer, no glyphon shape pass for the terminal grid.
        let fg_default = self.fg_default;
        // Underline runs collected per pane. We record
        // (origin_x, origin_y, pane_cols, row, col_a, col_b) where
        // origin_{x,y} is the PANE's origin (pad / top_inset) and
        // `pane_cols` is the originating pane's column count, captured at
        // insert time, and each entry carries its own `origin_{x,y}`.
        // Both are needed because the emit loop draws underlines from every
        // pane: without the origin an inactive pane's underlines land under
        // the active pane's coordinates, and without `pane_cols` the
        // per-origin snapped-edge cache is sized from the active pane, so a
        // wider inactive pane has its underlines clamped and truncated.
        let mut underlines: Vec<(
            f32,
            f32,
            u16,
            u16,
            sonicterm_text::row_glyph_cache::UnderlineRun,
        )> = Vec::new();
        let mut glyph_instances: Vec<GlyphInstance> =
            Vec::with_capacity(grid.cols as usize * grid.rows as usize);
        // Overlay glyph instances — palette text + (future) other modals.
        // Kept separate so they can be drawn AFTER `quad_overlay` paints
        // the modal backdrop, otherwise they'd be hidden by their own
        // background. (— palette text was previously routed through
        // glyphon's TextRenderer which bypassed the device-scale atlas
        // path used by `emit_tab_title_glyphs`, hence the HiDPI blur.)
        let mut overlay_glyph_instances: Vec<GlyphInstance> = Vec::new();
        // Missing-glyph "tofu" outlines collected during the cell walk.
        // Drawn via the quad pipeline after the text instances.
        let mut missing_tofu: Vec<(f32, f32, f32, f32, ChromeColor)> = Vec::new();
        // Mirror of missing_tofu, recording just the codepoint so tests
        // can assert "no class regressed" without depending on pixel
        // layout. Cleared every frame; published into `self.last_missing_chars`
        // before render() returns.
        let mut missing_chars_this_frame: Vec<char> = Vec::new();
        // G1a: surface dims, cell pitch, padding, top_inset, font_size
        // all live in raster px now, so `px_to_ndc` gets the raw surface
        // dims — the pre-unit mismatch can no longer arise.
        let sw = self.config.width as f32;
        let sh = self.config.height as f32;
        // Note: window-level `pad` / `top_inset` no longer cached here;
        // each pane uses its own origin via PaneView (Part B step 3).
        let cell_w = self.cell_w;
        let cell_h = self.cell_h;
        // Baseline offset inside the cell box. swash returns
        // placement.top relative to the baseline; we want screen-y
        // relative to the cell top. Using ≈80% of cell height matches
        // a reasonable ascent for monospace fonts at the configured
        // line-height; finer baseline control would require querying
        // font metrics.
        let baseline_y_in_cell = cell_h * 0.8;
        let software_presenter = self.uses_windows_software_presenter();

        let raster_px = self.raster_px(self.font_size);
        {
            // Post-glyphon the grid path is wezterm-only.
            // FontStack is the sole rasterizer; on test fixtures
            // without bundled fonts (FontStack returns None) the grid
            // walk skips per-glyph emission and only paints quads.
            let mut wt_raster = self.font_stack.clone();
            // The async fallback loader was wired into the
            // legacy SwashRasterizer. The wezterm path doesn't expose
            // an equivalent hook; missing glyphs are handled by
            // sonicterm-font's built-in fallback chain (NotoColorEmoji,
            // PingFangSC, etc. via the vendored features). We drop the
            // loader plumb here. If future work re-introduces an async
            // hook on FontStack rasterization, it would attach in this
            // same scope.
            let _ = self.async_loader;
            // Theme accent for the Cmd-hovered URL recolor. `UiPalette::accent`
            // is a linear-sRGB `[f32;4]` (alpha 1.0), the same space the
            // per-glyph `color` field carries, so it drops in with no
            // conversion. PERF: `UiPalette::from_theme` does ~20 hex parses +
            // sRGB→linear `powf` conversions; computing it unconditionally
            // every frame added measurable render latency to plain output
            // repaints (e.g. `ls -al`). It's only consumed when a URL is
            // ACTIVE-hovered (modifier held) and glyphs recolor to accent;
            // plain hover draws only a yellow underline, so compute lazily —
            // `[0.0;4]` otherwise. #perf
            let hovered_url_accent: [f32; 4] = if hovered_url_needs_accent(hovered_url_cells) {
                sonicterm_render_model::boundary::ui::ui_tokens::UiPalette::from_theme(theme).accent
            } else {
                // When: `!hovered_url_needs_accent` — plain hover draws only
                // the underline, so the accent is never sampled.
                [0.0, 0.0, 0.0, 0.0]
            };
            // Part B step 3: iterate every pane. Each iteration rebinds
            // `grid` to that pane's Grid (via the raw pointer collected
            // into pane_views above), uses the pane's own origin instead
            // of the window-level padding/inset, and threads its own
            // pane_id into the row_glyph_cache so split panes don't
            // collide on absolute-row keys (prereq).
            // Size the row glyph cache ONCE for the whole frame using the
            // total visible rows across all panes — NOT per-pane inside the
            // loop. Resizing to a single pane's `grid.rows` on every iteration
            // changed the cap each time in an unequal-height split and cleared
            // the entire cache per pane per frame, forcing all rows to
            // re-shape every keystroke. Mirrors the quad
            // cache's total-visible-rows sizing below.
            let total_glyph_rows: u16 = pane_views.iter().map(|pv| pv.grid.rows).sum();
            self.row_glyph_cache.resize(total_glyph_rows.max(1));
            for pv in &pane_views {
                let grid: &Grid = pv.grid;
                let pane_id: sonicterm_text::row_glyph_cache::PaneId = pv.pane_id;
                let pad = pv.origin_x;
                let top_inset = pv.origin_y;
                // Resolve which absolute row sits at the top of the rendered
                // viewport. When the user hasn't scrolled (or hasn't scrolled
                // past the visible bottom), this is the live-buffer top, i.e.
                // `scrollback_len()`. Otherwise it's the explicit absolute
                // index requested by the scroll action (e.g. a prompt row).
                let view_top_abs = Self::resolved_view_top_abs(grid, pv.viewport_top_abs);
                // Drop cache entries for every row the VT thread mutated
                // since the last frame. `grid.dirty_rows()` already covers
                // theme/font/resize/scroll/focus/selection changes via the
                // invalidation hooks; renderer-side state changes
                // (font/theme/scale/resize) already cleared the cache
                // wholesale above. Translating dirty row indices to
                // absolute rows uses the current view top — the same key
                // we'll look up by below.
                for r in grid.dirty_rows() {
                    self.row_glyph_cache.invalidate_row_abs(pane_id, view_top_abs + r as u64);
                }
                // Normalise selection once outside the loop so we hash a
                // canonical bbox per row. Rows are scrollback-ABSOLUTE; the
                // per-row membership test inside `row_hash_cells` compares
                // them against each row's `view_top_abs + r`.
                let sel_bbox: Option<(u64, u16, u64, u16)> = selection.map(|s| {
                    let (a, b) = s.normalized();
                    (a.0, a.1, b.0, b.1)
                });
                // per-cell device-pixel snapping rounds each cell's left
                // edge independently. At fractional DPI (1.25/1.5/1.75) that
                // produces a 14/15/14/15 device-pixel alternation in cell
                // pitch, which shows as 1-px gaps between adjacent Powerline
                // chevrons. Precompute snapped column edges once per pane so
                // every glyph-emit path in `flush_shape_run` derives `cx` and
                // the per-cell width from the SAME snapped edges — adjacent
                // cells then share an edge by construction. Integer-scale
                // fast path in `snap_to_device_pixels` makes this a no-op at
                // scale 1.0/2.0 (mac dHash snapshots stay green).
                let snapped_cell_x: Vec<f32> = build_snapped_cell_x(pad, cell_w, grid.cols);
                // Hover recolor applies only to the pane named by the hit-test.
                // Filtering by identity prevents a split at the same row/columns
                // from inheriting another pane's target accent.
                let pane_hovered_url =
                    hovered_url_cells.filter(|hovered| hovered.pane_id == pv.pane_id);
                for r in 0..grid.rows {
                    if !emit_full_rows && !grid.dirty_rows().any(|dirty| dirty == r as usize) {
                        // When: `!emit_full_rows` and `r` is absent from
                        // `dirty_rows` — the row's pixels are already correct.
                        continue;
                    }
                    let row_abs = view_top_abs + r as u64;
                    let Some(row) = grid.row_at_abs(row_abs) else {
                        // When: `grid.row_at_abs(row_abs)` is None — that
                        // absolute row is outside the scrollback still held.
                        continue;
                    };
                    // ------ Cache lookup ------
                    // Rows containing Box-Drawing / Block-Element
                    // codepoints cache normally: those glyphs now route
                    // through the same WezTerm block_sprite atlas path as
                    // text glyphs, so no side-channel geometry replay is
                    // required.
                    // G1a: cell_w / cell_h now ARE raster px, so the
                    // legacy DPI hash input is redundant (a constant
                    // after takeover). Pass 1.0 to keep the cache key
                    // shape; T3 will drop the param from `row_hash`
                    // itself.
                    let key = sonicterm_text::row_glyph_cache::row_hash_cells(
                        view_top_abs,
                        r as usize,
                        row.iter(),
                        self.style_rev,
                        cell_w,
                        cell_h,
                        1.0,
                        pad,
                        top_inset,
                        sw,
                        sh,
                        sel_bbox,
                    );
                    // Fold only this row's active hover fragment into its cache
                    // key. Peer fragments and hint-only underlines remain outside
                    // the row cache, so unrelated rows keep replaying.
                    let row_hovered_url = hovered_url_for_pane_row(pane_hovered_url, pv.pane_id, r);
                    let key = hovered_url_row_cache_key(key, row_hovered_url, r);
                    let atlas_epoch = self.glyph_atlas.evictions();
                    if let Some(cached) =
                        self.row_glyph_cache.get(pane_id, row_abs, key, atlas_epoch)
                    {
                        // When: `row_glyph_cache.get` is Some — the row hash and
                        // atlas epoch both match, so shaped glyphs are reusable.
                        glyph_instances.extend_from_slice(&cached.glyphs);
                        for run in &cached.underlines {
                            underlines.push((pad, top_inset, grid.cols, r, *run));
                        }
                        for t in &cached.tofu {
                            // TofuColor is [u8;4] in the cache (no
                            // cross-crate ChromeColor dep). Convert
                            // back to ChromeColor for the frame's
                            // local emit vec.
                            let (x, y, w, h, c) = *t;
                            missing_tofu.push((x, y, w, h, ChromeColor::from(c)));
                        }
                        missing_chars_this_frame.extend_from_slice(&cached.missing_chars);
                        continue;
                    }
                    // ------ Miss: shape into row-local buffers, then
                    // splice into the frame buffers AND insert into the
                    // cache. Keeping the per-row work in local Vecs is
                    // what lets us cache without scanning the frame
                    // buffers after the fact. ------
                    let glyph_base = glyph_instances.len();
                    let tofu_base = missing_tofu.len();
                    let miss_base = missing_chars_this_frame.len();
                    let mut row_underlines: Vec<sonicterm_text::row_glyph_cache::UnderlineRun> =
                        Vec::new();
                    let mut ul_start: Option<(u16, UnderlineStyle, Color)> = None;
                    let mut last_visible_col: u16 = 0;
                    // First pass: per-cell underline coalescing (unchanged
                    // — underlines are a cell-level decoration, independent
                    // of shaping).
                    for (col, cell) in row.iter().enumerate() {
                        if cell.flags.contains(CellFlags::WIDE_CONT) {
                            // When: `WIDE_CONT` — the trailing half of a wide
                            // glyph, whose decoration belongs to its lead cell.
                            continue;
                        }
                        last_visible_col = col as u16;
                        if let Some((style, color)) = underline_key(cell) {
                            match ul_start {
                                Some((_, active_style, active_color))
                                    if active_style == style && active_color == color =>
                                {
                                    // When: the guard holds — same style and
                                    // colour, so the open run simply continues.
                                }
                                Some((s, active_style, active_color)) => {
                                    let end = (col as u16).saturating_sub(1);
                                    let run = sonicterm_text::row_glyph_cache::UnderlineRun {
                                        start_col: s,
                                        end_col: end,
                                        style: active_style,
                                        color: active_color,
                                    };
                                    row_underlines.push(run);
                                    underlines.push((pad, top_inset, grid.cols, r, run));
                                    ul_start = Some((col as u16, style, color));
                                }
                                None => {
                                    ul_start = Some((col as u16, style, color));
                                }
                            }
                        } else if let Some((s, style, color)) = ul_start.take() {
                            // When: `ul_start.take()` is Some — this cell has no
                            // underline, so the open run ends and is emitted.
                            let end = (col as u16).saturating_sub(1);
                            let run = sonicterm_text::row_glyph_cache::UnderlineRun {
                                start_col: s,
                                end_col: end,
                                style,
                                color,
                            };
                            row_underlines.push(run);
                            underlines.push((pad, top_inset, grid.cols, r, run));
                        }
                    }
                    if let Some((s, style, color)) = ul_start.take() {
                        let run = sonicterm_text::row_glyph_cache::UnderlineRun {
                            start_col: s,
                            end_col: last_visible_col,
                            style,
                            color,
                        };
                        row_underlines.push(run);
                        underlines.push((pad, top_inset, grid.cols, r, run));
                    }

                    // Second pass: group cells into style runs and shape
                    // each run through cosmic-text. The shaper composes
                    // ZWJ sequences and ligatures into single glyphs when
                    // the font supports them; otherwise it produces 1:1
                    // output identical to the old char-based path.
                    let mut run_cells: Vec<(u16, Cell)> = Vec::new();
                    let mut run_style: Option<RunStyle> = None;
                    let mut run_first_col: u16 = 0;
                    for (col, cell) in row.iter().enumerate() {
                        if cell.flags.contains(CellFlags::WIDE_CONT) {
                            // When: `WIDE_CONT` — the trailing half of a wide
                            // glyph, already shaped from its lead cell.
                            continue;
                        }
                        let style = RunStyle::from_cell(cell);
                        match run_style {
                            None => {
                                run_style = Some(style);
                                run_first_col = col as u16;
                                run_cells.push((col as u16, cell.clone()));
                            }
                            Some(s) if s == style => {
                                run_cells.push((col as u16, cell.clone()));
                            }
                            Some(s) => {
                                Self::flush_shape_run(
                                    &mut self.glyph_atlas,
                                    &self.font_family,
                                    raster_px,
                                    &mut glyph_instances,
                                    &mut missing_tofu,
                                    &mut missing_chars_this_frame,
                                    r,
                                    run_first_col,
                                    s,
                                    &run_cells,
                                    theme,
                                    fg_default,
                                    cell_w,
                                    cell_h,
                                    top_inset,
                                    pad,
                                    sw,
                                    sh,
                                    baseline_y_in_cell,
                                    &snapped_cell_x,
                                    self.font_stack.as_ref(),
                                    wt_raster.as_mut(),
                                    row_hovered_url,
                                    hovered_url_accent,
                                    software_presenter,
                                );
                                run_cells.clear();
                                run_style = Some(style);
                                run_first_col = col as u16;
                                run_cells.push((col as u16, cell.clone()));
                            }
                        }
                    }
                    if let Some(s) = run_style {
                        Self::flush_shape_run(
                            &mut self.glyph_atlas,
                            &self.font_family,
                            raster_px,
                            &mut glyph_instances,
                            &mut missing_tofu,
                            &mut missing_chars_this_frame,
                            r,
                            run_first_col,
                            s,
                            &run_cells,
                            theme,
                            fg_default,
                            cell_w,
                            cell_h,
                            top_inset,
                            pad,
                            sw,
                            sh,
                            baseline_y_in_cell,
                            &snapped_cell_x,
                            self.font_stack.as_ref(),
                            wt_raster.as_mut(),
                            pane_hovered_url,
                            hovered_url_accent,
                            software_presenter,
                        );
                    }
                    // Capture this row's contributions and insert into
                    // the cache so subsequent unchanged frames replay
                    // without shaping.
                    let row_glyphs = glyph_instances[glyph_base..].to_vec();
                    // Convert ChromeColor → TofuColor for cache storage.
                    let row_tofu: Vec<(f32, f32, f32, f32, [u8; 4])> = missing_tofu[tofu_base..]
                        .iter()
                        .map(|(x, y, w, h, c)| (*x, *y, *w, *h, [c.r(), c.g(), c.b(), c.a()]))
                        .collect();
                    let row_missing = missing_chars_this_frame[miss_base..].to_vec();
                    self.row_glyph_cache.insert(
                        pane_id,
                        row_abs,
                        key,
                        self.glyph_atlas.evictions(),
                        sonicterm_text::row_glyph_cache::CachedRow {
                            glyphs: row_glyphs,
                            underlines: row_underlines,
                            tofu: row_tofu,
                            missing_chars: row_missing,
                        },
                    );
                }
            } // end per-pane loop
        }

        let mut quads: Vec<QuadInstance> = Vec::new();
        // Overlay quads — drawn AFTER terminal text + main quads so that
        // palette / search-input / IME backgrounds visually cover the
        // terminal content underneath. Emitted into the same vector as the
        // main quads, terminal glyphs bleed through overlay dialogs.
        let mut quads_overlay: Vec<QuadInstance> = Vec::new();

        let inline_image_placements: Vec<InlineImagePlacement<'_>> = pane_views
            .iter()
            .flat_map(|pv| {
                pv.inline_images.iter().map(move |image| (image, pv.origin_x, pv.origin_y))
            })
            .enumerate()
            .map(|(painter_order, (image, origin_x, origin_y))| InlineImagePlacement {
                image,
                origin_x,
                origin_y,
                painter_order,
            })
            .collect();
        let has_renderable_inline_media = inline_image_placements.iter().any(|placement| {
            let image = placement.image;
            if image.width == 0 || image.height == 0 || image.bgra.is_empty() {
                // When: any dimension is 0 or `bgra` is empty — a failed or
                // pending decode, which has no pixels to place.
                return false;
            }
            let x = placement.origin_x + image.col as f32 * cell_w;
            let y = placement.origin_y + image.row as f32 * cell_h;
            x < sw && y < sh && x + image.width as f32 > 0.0 && y + image.height as f32 > 0.0
        });
        self.demote_image_atlas_if_idle(has_renderable_inline_media);
        let image_atlas_promoted = self.promote_image_atlas_if_needed(
            has_renderable_inline_media,
            retained_inline_media_bytes,
        );
        if inline_media_changed
            && !image_atlas_promoted
            && image_atlas_reset_warranted(&self.image_atlas)
        {
            self.reset_image_atlas();
        }
        let mut image_glyph_instances = Vec::new();
        let skipped_inline_images = emit_inline_image_instances(
            &mut self.image_atlas,
            &mut image_glyph_instances,
            &inline_image_placements,
            cell_w,
            cell_h,
            sw,
            sh,
        );
        if inline_media_changed && skipped_inline_images > 0 {
            tracing::warn!(
                target: "sonic::glyph_atlas",
                skipped = skipped_inline_images,
                resident = self.image_atlas.len(),
                width = self.image_atlas.width(),
                height = self.image_atlas.height(),
                "inline image atlas full; skipped older images without evicting text glyphs"
            );
        }

        // build the active pane's shared device-pixel-snapped
        // column-edge cache once per frame, hoisted above every overlay
        // path. Every overlay anchored to the active pane (selection,
        // cursor, copy-mode, quick-select, hyperlink, search-highlight,
        // underline-decoration, IME preedit) reads its x edges from
        // this cache so it stays edge-aligned with adjacent glyph cells
        // at fractional DPI. Integer scales (1.0/2.0) are an identity
        // fast path inside `snap_to_device_pixels`, so mac dHash
        // baselines stay green by construction. Per diagnosis,
        // per-pane bg fill builds its OWN cache (see the per-pane bg
        // loop below) — it MUST NOT share the active pane's cache.
        let active_snapped_cell_x: Vec<f32> =
            build_snapped_cell_x(active_origin_x, self.cell_w, grid.cols);

        // Per-cell ANSI background colors. Must be pushed FIRST so that
        // selection / cursor / overlay quads draw on top — otherwise an
        // ANSI-colored cell would obscure the selection highlight. The
        // helper run-length coalesces adjacent same-bg cells into a single
        // wide quad (an 80-col `\033[41m` fill becomes 1 quad, not 80).
        // Cells whose bg resolves to the theme default are skipped: the
        // surface `LoadOp::Clear(self.bg)` already covers that area.
        // Part B step 3: emit bg quads for EVERY pane using each pane's
        // own origin, not just the active pane.
        //
        // P2: per-row LineQuadCache. Background quads are a
        // hot QuadInstance source in dense-cell workloads. Each row's
        // emission is keyed on (pane_id,
        // abs_row, content+geom+style+selection hash); on a hit we
        // `extend_from_slice` the cached slice and skip the per-cell
        // run-length-encode walk in `emit_cell_bg_quads_for_row`.
        let sel_bbox_for_quads: Option<(u64, u16, u64, u16)> = selection.map(|s| {
            let (a, b) = s.normalized();
            (a.0, a.1, b.0, b.1)
        });
        let total_visible_rows: u16 = pane_views.iter().map(|pv| pv.grid.rows).sum();
        self.line_quad_cache.resize(total_visible_rows.max(1));
        for pv in &pane_views {
            let pv_grid: &Grid = pv.grid;
            let pane_id: crate::row_quad_cache::PaneId = pv.pane_id;
            let pane_rect = PaneRect { x: pv.origin_x, y: pv.origin_y, w: pv.rect_w, h: pv.rect_h };
            let view_top_abs_bg = Self::resolved_view_top_abs(pv_grid, pv.viewport_top_abs);
            // Mirror RowGlyphCache's dirty-row invalidation: drop entries
            // for every row the VT thread mutated since the last frame.
            for r in pv_grid.dirty_rows() {
                self.line_quad_cache.invalidate_row_abs(pane_id, view_top_abs_bg + r as u64);
            }
            let pad_bg = pane_rect.x;
            let top_inset_bg = pane_rect.y;
            let max_cols =
                ((pane_rect.w / cell_w).floor() as i32).clamp(0, i32::from(pv_grid.cols)) as u16;
            let max_rows =
                ((pane_rect.h / cell_h).floor() as i32).clamp(0, i32::from(pv_grid.rows)) as u16;
            if max_cols == 0 || max_rows == 0 {
                // When: `max_cols == 0 || max_rows == 0` — the pane rect is
                // thinner than one cell, so no background quad would fit.
                continue;
            }
            // per-pane snapped-edge cache for bg-fill runs. Per
            // diagnosis Recommendation, per-pane bg must NOT reuse the
            // active pane's cache because each split-pane has its own
            // pad and the snapped column edges differ.
            let snapped_cell_x_bg = build_snapped_cell_x(pad_bg, cell_w, pv_grid.cols);
            for r in 0..max_rows {
                if !emit_full_rows && !pv_grid.dirty_rows().any(|dirty| dirty == r as usize) {
                    // When: `!emit_full_rows` and `r` is absent from
                    // `dirty_rows` — this row's background is already correct.
                    continue;
                }
                let row_abs = view_top_abs_bg + r as u64;
                let Some(row_cells) = pv_grid.row_at_abs(row_abs) else {
                    // When: `pv_grid.row_at_abs(row_abs)` is None — the row is
                    // outside the scrollback this pane still retains.
                    continue;
                };
                // G1a: pass 1.0 for the legacy DPI hash input
                // (cell_w/cell_h ARE raster px now). T3 will drop
                // the param from `row_quad_hash` itself.
                let key = crate::row_quad_cache::row_quad_hash_cells(
                    view_top_abs_bg,
                    r as usize,
                    row_cells.iter(),
                    self.style_rev,
                    cell_w,
                    cell_h,
                    pad_bg,
                    top_inset_bg,
                    pane_rect.w,
                    pane_rect.h,
                    sel_bbox_for_quads,
                );
                if let Some(cached) = self.line_quad_cache.get(pane_id, row_abs, key) {
                    // When: `line_quad_cache.get` is Some — the row's contents,
                    // style, and selection overlap are all unchanged.
                    quads.extend_from_slice(&cached.quads);
                    continue;
                }
                let base = quads.len();
                emit_cell_bg_quads_for_row(
                    pv_grid,
                    view_top_abs_bg,
                    theme,
                    pad_bg,
                    top_inset_bg,
                    cell_w,
                    cell_h,
                    sw,
                    sh,
                    max_cols,
                    r,
                    &mut quads,
                    &snapped_cell_x_bg,
                );
                let row_quads = quads[base..].to_vec();
                self.line_quad_cache.insert(
                    pane_id,
                    row_abs,
                    key,
                    crate::row_quad_cache::CachedRowQuads { quads: row_quads },
                );
            }
        }

        if let Some((flash_pane_id, flash_alpha)) = self.pane_focus_flash_alpha(now) {
            if let Some(pv) = pane_views.iter().find(|pv| pv.pane_id == flash_pane_id) {
                let flash_rgb = [
                    (self.bg_rgba[0] + 0.07).min(1.0),
                    (self.bg_rgba[1] + 0.07).min(1.0),
                    (self.bg_rgba[2] + 0.07).min(1.0),
                ];
                let color = premultiply([flash_rgb[0], flash_rgb[1], flash_rgb[2], flash_alpha]);
                quads.push(QuadInstance {
                    rect: px_to_ndc(pv.origin_x, pv.origin_y, pv.rect_w, pv.rect_h, sw, sh),
                    color,
                    ..Default::default()
                });
            }
        }

        // Per-pane scrollbar emit. Runs after row backgrounds and before
        // selection, cursor, and modal overlays. Auto opacity comes from the
        // app state machine; geometry remains shared with hit-testing.
        for pv in &pane_views {
            let pane_rect = PaneRect { x: pv.origin_x, y: pv.origin_y, w: pv.rect_w, h: pv.rect_h };
            let pv_grid: &Grid = pv.grid;
            let viewport_rows = pv_grid.rows;
            let total_rows = pv_grid.scrollback_len() as u64 + viewport_rows as u64;
            let view_top = Self::resolved_view_top_abs(pv_grid, pv.viewport_top_abs);
            emit_pane_scrollbar(
                &mut quads_overlay,
                pane_rect,
                viewport_rows,
                total_rows,
                view_top,
                self.scrollbar_mode,
                theme,
                sw,
                sh,
                pv.scrollbar_alpha,
                self.scale_factor,
            );
        }

        if let Some(sel) = selection {
            if !sel.is_empty() {
                // Selection highlights are anchored to the active pane's
                // origin. They MUST be clipped to that pane's rect — otherwise
                // a selection that extends past the pane's last visible column
                // (e.g. the user drags across the split into the neighbouring
                // pane) would emit a quad that visually bleeds into the
                // neighbouring pane's grid area. Regression-guard for the
                // bug where dragging in a split-right layout painted the
                // selection across both panes.
                let pane_x = active_origin_x;
                let pane_y = active_origin_y;
                // Pane rect_px is the source of truth — see note above.
                let pane_w = active_pane_w;
                let pane_h = active_pane_h;
                // Selection rows are scrollback-ABSOLUTE; resolve the active
                // pane's view top so `selection_quad_rects` can map them back
                // to viewport rows (so the highlight follows the TEXT when
                // scrolled).
                let sel_view_top_abs = Self::resolved_view_top_abs(grid, viewport_top_abs);
                for rect in selection_quad_rects(
                    sel,
                    sel_view_top_abs,
                    grid.rows,
                    grid.cols,
                    active_origin_x,
                    active_origin_y,
                    self.cell_w,
                    self.cell_h,
                    &active_snapped_cell_x,
                )
                .into_iter()
                .filter_map(|r| clip_rect_to_pane(r, pane_x, pane_y, pane_w, pane_h))
                {
                    quads.push(QuadInstance {
                        rect: px_to_ndc(rect.0, rect.1, rect.2, rect.3, sw, sh),
                        color: self.selection_color,
                        ..Default::default()
                    });
                }
            }
        }

        if let Some(copy_mode) = copy_mode {
            if let Some(quick_select) = copy_mode.quick_select.as_ref() {
                self.prepare_quick_select_overlay(
                    quick_select,
                    active_origin_x,
                    active_origin_y,
                    grid.scrollback_len(),
                    grid.rows as usize,
                    theme,
                    sw,
                    sh,
                    &mut quads_overlay,
                    &active_snapped_cell_x,
                );
            }
            let view_top_abs = Self::resolved_view_top_abs(grid, viewport_top_abs);
            if let Some((cx, cy)) = Self::emit_copy_mode_quads(
                copy_mode,
                grid,
                view_top_abs,
                active_origin_x,
                active_origin_y,
                self.cell_w,
                self.cell_h,
                sw,
                sh,
                self.selection_color,
                self.cursor_color,
                &mut quads,
                &active_snapped_cell_x,
            ) {
                recolor_cursor_glyphs(
                    &mut glyph_instances,
                    cx,
                    cy,
                    self.cell_w,
                    self.cell_h,
                    sw,
                    sh,
                    self.cursor_text_color,
                );
            }
        }
        if cursor_visible && self.window_focused && !read_only_mode {
            // Hide the cursor when the viewport is scrolled away from the
            // live region — its absolute row is `scrollback_len + cursor.row`,
            // which sits below the bottom of a scrolled-back view.
            let live_top = grid.scrollback_len() as u64;
            let view_top = viewport_top_abs.map(|v| v.min(live_top)).unwrap_or(live_top);
            if view_top == live_top {
                // read both cursor cell left edge AND width from the
                // shared snapped-edge cache so the cursor (block / bar /
                // underline) lines up with its glyph cell at fractional DPI.
                let row = grid.row(grid.cursor.row);
                let mut cur_col = grid.cursor.col as usize;
                let mut cursor_span = 1usize;
                if let Some(cell) = row.get(cur_col) {
                    if cell.flags.contains(CellFlags::WIDE_CONT) && cur_col > 0 {
                        cur_col -= 1;
                        cursor_span = 2;
                    } else if cell.flags.contains(CellFlags::WIDE) {
                        // When: `WIDE` — the cursor is on the lead half, so the
                        // block spans two columns from where it already is.
                        cursor_span = 2;
                    }
                }
                let cur_col_clamped = cur_col.min(active_snapped_cell_x.len().saturating_sub(2));
                let end_col = (cur_col_clamped + cursor_span)
                    .min(active_snapped_cell_x.len().saturating_sub(1));
                let mut cx = active_snapped_cell_x
                    .get(cur_col_clamped)
                    .copied()
                    .unwrap_or(active_origin_x + f32::from(grid.cursor.col) * self.cell_w);
                let cw = active_snapped_cell_x
                    .get(end_col)
                    .map(|r| r - cx)
                    .unwrap_or(self.cell_w * cursor_span as f32);
                let cy = active_origin_y + f32::from(grid.cursor.row) * self.cell_h;
                // #B14: when an inline IME composition is active at the terminal
                // cursor (search NOT focused — that case anchors to the search
                // box instead), the OS does not draw it, so we do (preedit block
                // further down). Advance the cursor mark to the composition caret
                // (end of the in-flight run, or the IME-reported caret byte) so it
                // sits at the insertion point WezTerm-style, instead of frozen on
                // the first composing glyph.
                if search.is_none() {
                    if let Some(i) = ime {
                        let text = i.preedit();
                        // gate on visible ink (NOT just non-empty) and
                        // reuse the shared pure helper, so a whitespace-only
                        // macOS marked string never shoves the cursor block
                        // into empty space with no glyph under it.
                        let caret_byte = i.cursor().map(|(_, e)| e).unwrap_or(text.len());
                        let font_size = self.raster_px(self.font_size);
                        cx += preedit_caret_advance(text, caret_byte, font_size);
                    }
                }
                // Keep every cursor shape at its exact theme color. Alpha
                // fading over the terminal background makes Gruvbox yellow
                // read as olive/green during real redraws.
                let color = active_cursor_color(self.cursor_color, self.cursor_shape, blink_alpha);
                // Wezterm cursor shapes:
                //   Block     → full-cell quad, glyph re-rendered in bg
                //   Bar       → 2px vertical bar pinned to the left edge
                //   Underline → 2px horizontal bar pinned to the bottom
                // We pick a ~2px sub-cell thickness rather than something
                // proportional to cell_h so the bar stays crisp on both
                // small and large font sizes (no half-pixel sub-stem).
                // 2 logical px scaled to physical px so the bar/underline
                // keep a constant physical thickness across DPIs (min 1px).
                let subshape_px: f32 = (2.0 * self.scale_factor).round().max(1.0);
                match self.cursor_shape {
                    CursorShape::Block => {
                        if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                            (cx, cy, cw, self.cell_h),
                            active_pane_x,
                            active_pane_y,
                            active_pane_w,
                            active_pane_h,
                        ) {
                            quads.push(QuadInstance {
                                rect: px_to_ndc(qx, qy, qw, qh, sw, sh),
                                color,
                                ..Default::default()
                            });
                        }
                        recolor_cursor_glyphs(
                            &mut glyph_instances,
                            cx,
                            cy,
                            cw,
                            self.cell_h,
                            sw,
                            sh,
                            self.cursor_text_color,
                        );
                    }
                    CursorShape::Bar => {
                        if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                            (cx, cy, subshape_px, self.cell_h),
                            active_pane_x,
                            active_pane_y,
                            active_pane_w,
                            active_pane_h,
                        ) {
                            quads.push(QuadInstance {
                                rect: px_to_ndc(qx, qy, qw, qh, sw, sh),
                                color,
                                ..Default::default()
                            });
                        }
                    }
                    CursorShape::Underline => {
                        if let Some((qx, qy, qw, qh)) = clip_rect_to_pane(
                            (cx, cy + self.cell_h - subshape_px, cw, subshape_px),
                            active_pane_x,
                            active_pane_y,
                            active_pane_w,
                            active_pane_h,
                        ) {
                            quads.push(QuadInstance {
                                rect: px_to_ndc(qx, qy, qw, qh, sw, sh),
                                color,
                                ..Default::default()
                            });
                        }
                    }
                }
            }
        }

        // OSC 8 hyperlinks are semantic, not visual. Do not tint/underline
        // every hyperlink cell permanently: prompts such as Oh My Posh wrap
        // the path segment in a `file:` hyperlink, and a permanent overlay
        // changes the segment's configured truecolor background compared with
        // Windows Terminal. Link affordance is drawn below only for the
        // currently hovered URL span.

        gpu_lap!("grid_walk");
        // Underline quads — drawn last so they appear on top of the text.
        // SGR 4:n style and SGR 58 colour are stored per-cell and coalesced
        // above, matching WezTerm/xterm underline semantics instead of the
        // old single-colour single-line approximation.
        let underline_thickness = (self.cell_h * 0.08).max(1.0);
        // underlines are collected from every pane (each entry
        // carries its own `origin_x` == pane pad), so memoize a snapped
        // cache per distinct pane pad. Most frames have ≤ 2 panes, so
        // the linear-scan map is cheaper than a HashMap.
        // Step-4 revise (option (a)): each entry also carries the
        // ORIGINATING pane's column count. Previously this loop sized
        // the cache from `grid.cols` (== ACTIVE pane), which clamped
        // and truncated underlines on wider INACTIVE panes. Key the
        // cache by (pad_bits, pane_cols) and size it accordingly.
        let mut underline_caches: Vec<(u32, u16, Vec<f32>)> = Vec::new();
        for (origin_x, origin_y, pane_cols, row, run) in &underlines {
            let pad_bits = origin_x.to_bits();
            let cache = if let Some((_, _, c)) =
                underline_caches.iter().find(|(b, pc, _)| *b == pad_bits && *pc == *pane_cols)
            {
                c
            } else {
                // When: the `find` returned None — no cache exists yet for this
                // origin and column count, so one is built and memoized.
                let c = build_snapped_cell_x(*origin_x, self.cell_w, *pane_cols);
                underline_caches.push((pad_bits, *pane_cols, c));
                &underline_caches.last().unwrap().2
            };
            let end_exclusive = (run.end_col as usize).saturating_add(1);
            let cache_end = end_exclusive.min(cache.len().saturating_sub(1));
            let col_a_usize = (run.start_col as usize).min(cache_end);
            let x = cache
                .get(col_a_usize)
                .copied()
                .unwrap_or(*origin_x + f32::from(run.start_col) * self.cell_w);
            let w = cache
                .get(cache_end)
                .map(|r| r - x)
                .unwrap_or_else(|| f32::from(run.end_col - run.start_col + 1) * self.cell_w);
            let y = *origin_y + f32::from(*row) * self.cell_h;
            let underline_color =
                chrome_color_to_linear_rgba(color_to_chrome(run.color, theme, self.fg_default));
            push_underline_quads(
                &mut quads,
                run.style,
                x,
                y,
                w,
                self.cell_h,
                underline_thickness,
                sw,
                sh,
                underline_color,
            );
        }

        // Hover target underline. The pane id travels with the hit so an
        // inactive split can reveal its own path without painting another
        // pane's fragments at the same rows and columns.
        if let Some(h) = hovered_url_cells {
            // When: `hovered_url_cells` contains `h`, render every canonical fragment for its owning pane.
            if let Some(hovered_view) = pane_views.iter().find(|view| view.pane_id == h.pane_id) {
                // When: `pane_views` finds `h.pane_id`, project all fragments through that pane's geometry.
                let hov_accent = if h.active {
                    sonicterm_render_model::boundary::ui::ui_tokens::UiPalette::from_theme(theme)
                        .accent
                } else {
                    // When: `h.active` is false, render the non-clickable hover hint in the theme's yellow rather than the action accent.
                    hex_to_premultiplied_rgba(theme.colors.ansi.yellow.0.as_str(), 0.9)
                };
                let hovered_grid_cols = hovered_view.grid.cols;
                let hcache =
                    build_snapped_cell_x(hovered_view.origin_x, self.cell_w, hovered_grid_cols);
                for span in h.spans() {
                    let Some((x, y, width, height)) = hovered_url_span_rect(
                        *span,
                        hovered_grid_cols,
                        hovered_view.grid.rows,
                        hovered_view.origin_x,
                        hovered_view.origin_y,
                        self.cell_w,
                        self.cell_h,
                        &hcache,
                    ) else {
                        // When: `span` has no visible coverage in `hovered_view`, emit no stale geometry.
                        continue;
                    };
                    push_underline_quads(
                        &mut quads,
                        UnderlineStyle::Single,
                        x,
                        y,
                        width,
                        height,
                        underline_thickness,
                        sw,
                        sh,
                        hov_accent,
                    );
                }
            }
        }

        // -------- Missing-glyph tofu fallback ------------------------------
        // For cells whose rasterizer returned no tile (and char isn't
        // whitespace), draw a thin outlined rectangle so the gap is
        // visible. Helps catch font-fallback misses (emoji etc.).
        for (x, y, w, h, col) in &missing_tofu {
            let rgba = with_premultiplied_alpha(chrome_color_to_linear_rgba(*col), 0.55);
            let t = 1.0_f32; // border thickness
                             // Top
            quads.push(QuadInstance {
                rect: px_to_ndc(*x, *y, *w, t, sw, sh),
                color: rgba,
                ..Default::default()
            });
            // Bottom
            quads.push(QuadInstance {
                rect: px_to_ndc(*x, *y + *h - t, *w, t, sw, sh),
                color: rgba,
                ..Default::default()
            });
            // Left
            quads.push(QuadInstance {
                rect: px_to_ndc(*x, *y, t, *h, sw, sh),
                color: rgba,
                ..Default::default()
            });
            // Right
            quads.push(QuadInstance {
                rect: px_to_ndc(*x + *w - t, *y, t, *h, sw, sh),
                color: rgba,
                ..Default::default()
            });
        }

        // -------- Pane splitters + broadcast safety chrome ------------------
        // Splitters are 1px interior seams at the shared OUTER pane boundary.
        // They are not pane borders: no window perimeter is drawn, and the
        // seam sits outside the per-pane cell padding that is applied inside
        // each pane rect by the layout caller.
        if pane_rects.len() > 1 {
            for splitter in splitter_rects_from_panes(pane_rects, 1.0) {
                quads.push(QuadInstance {
                    rect: px_to_ndc(
                        splitter.rect.x,
                        splitter.rect.y,
                        splitter.rect.w,
                        splitter.rect.h,
                        sw,
                        sh,
                    ),
                    color: self.splitter_color,
                    ..Default::default()
                });
            }
        }

        // Broadcast receivers keep unmistakable red safety chrome so users do
        // not accidentally leave mirrored input enabled. This is intentionally
        // independent of the subtle split-pane seam styling above.
        if !broadcast_receiver_ids.is_empty() {
            // When: `!broadcast_receiver_ids.is_empty()` — at least one pane
            // mirrors input, which needs its red safety border and strip.
            let warning = hex_to_premultiplied_rgba(theme.colors.bright.red.0.as_str(), 1.0);
            for (id, r) in pane_rects {
                if !broadcast_receiver_ids.contains(id) {
                    // When: this pane's id is absent from the receiver set — it
                    // does not mirror input, so it takes no warning chrome.
                    continue;
                }
                let t = 2.0_f32;
                quads.push(QuadInstance {
                    rect: px_to_ndc(r.x, r.y, r.w, t, sw, sh),
                    color: warning,
                    ..Default::default()
                });
                quads.push(QuadInstance {
                    rect: px_to_ndc(r.x, r.y + r.h - t, r.w, t, sw, sh),
                    color: warning,
                    ..Default::default()
                });
                quads.push(QuadInstance {
                    rect: px_to_ndc(r.x, r.y, t, r.h, sw, sh),
                    color: warning,
                    ..Default::default()
                });
                quads.push(QuadInstance {
                    rect: px_to_ndc(r.x + r.w - t, r.y, t, r.h, sw, sh),
                    color: warning,
                    ..Default::default()
                });
                let strip_h = (self.font_size * 1.45).max(20.0).min(r.h.max(0.0));
                let strip = with_premultiplied_alpha(warning, 0.92);
                quads_overlay.push(QuadInstance {
                    rect: px_to_ndc(r.x + t, r.y + t, (r.w - t * 2.0).max(0.0), strip_h, sw, sh),
                    color: strip,
                    ..Default::default()
                });
            }
        }
        // -------- Tab bar ---------------------------------------------------
        // The insertion gap below opens 8 px at the current drop slot when a
        // drag is active over this bar.
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
            let ui_palette =
                sonicterm_render_model::boundary::ui::ui_tokens::UiPalette::from_theme(theme);
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
                &mut quads,
                &layout,
                &TabBarQuadParams {
                    tab_count: tabs.tabs().len(),
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
            let tab_font_size = tab_title_font_size(self.font_size);
            let avg_glyph_w = (self.cell_w * (tab_font_size / self.font_size)).max(1.0);
            let bar_h = self.tab_bar_logical_height();
            let bar_y = self.tab_bar_y_offset();
            let tab_raster_px = self.raster_px(tab_font_size);
            // bar_h, bar_y are raster px (post-G1a). Use raster-px font
            // height (tab_raster_px) for the vertical centering math
            // so the title sits in the middle of the bar instead of
            // tracking the un-scaled logical font_size at 1x while the
            // bar lives at 2x.
            let title_top = bar_y + ((bar_h - tab_raster_px * 1.2) / 2.0).max(0.0);
            let tab_baseline_y = title_top + tab_raster_px * 0.95;
            let native_em = tab_raster_px;
            let mut tab_rasterizer = self.tab_title_font_stack.clone();
            for t in &layout.tabs {
                let Some(tab) = tabs.tabs().get(t.idx) else {
                    // When: `tabs.tabs().get(t.idx)` is None — the layout
                    // outlived a closed tab, so there is no title to draw.
                    continue;
                };
                let active = layout.active == Some(t.idx);
                let active_panel_focused = active && self.window_focused;
                let hovered = hover_tab_idx == t.idx as u32;
                let show_privilege_badge =
                    tab_requires_privilege_badge(process_privileged, tab.foreground_privileged);
                let max_chars = tab_title_capacity(
                    t.title_rect,
                    avg_glyph_w,
                    show_privilege_badge,
                    self.scale_factor,
                );
                let title = tab_title_display_text(
                    &tab.title,
                    tab.command.clone().badge(now, active),
                    max_chars,
                );
                let badge_alpha = if source_tab_idx == Some(t.idx) { source_alpha } else { 1.0 };
                if let (Some(stack), Some(rasterizer)) =
                    (self.tab_title_font_stack.as_ref(), tab_rasterizer.as_mut())
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
                    let measure = chrome_text::layout_with_raster_variant(
                        stack,
                        rasterizer,
                        &mut self.glyph_atlas,
                        &title,
                        color,
                        ChromeAttrs::default(),
                        tab_raster_px,
                        native_em,
                        (0.0, tab_baseline_y),
                        (sw, sh),
                        None,
                        GlyphRasterVariant::TabTitle,
                    );
                    let placement = tab_title_block_placement(
                        t.title_rect,
                        measure.width_px,
                        show_privilege_badge,
                        self.scale_factor,
                    );
                    if let Some(badge) = placement.badge_rect {
                        emit_privilege_badge_quads(
                            &mut quads,
                            badge,
                            ui_palette.danger,
                            badge_alpha,
                            (sw, sh),
                        );
                    }
                    let final_layout = chrome_text::layout_with_raster_variant(
                        stack,
                        rasterizer,
                        &mut self.glyph_atlas,
                        &title,
                        color,
                        ChromeAttrs::default(),
                        tab_raster_px,
                        native_em,
                        (placement.text_x, tab_baseline_y),
                        (sw, sh),
                        Some(ChromeClip {
                            x: placement.text_clip.x,
                            y: placement.text_clip.y,
                            w: placement.text_clip.w,
                            h: placement.text_clip.h,
                        }),
                        GlyphRasterVariant::TabTitle,
                    );
                    glyph_instances.extend(final_layout.glyphs);
                } else if show_privilege_badge {
                    // When: `show_privilege_badge` is true without a title font stack, paint the vector warning alone.
                    let placement =
                        tab_title_block_placement(t.title_rect, 0.0, true, self.scale_factor);
                    if let Some(badge) = placement.badge_rect {
                        emit_privilege_badge_quads(
                            &mut quads,
                            badge,
                            ui_palette.danger,
                            badge_alpha,
                            (sw, sh),
                        );
                    }
                }
            }
        }
        // -------- Search highlights + badge --------------------------------
        if let Some(s) = search {
            // When: `search` is Some — a search session is live, so its match
            // highlights and status badge belong on this frame.
            let cur_idx = s.current;
            let view_top_abs = Self::resolved_view_top_abs(grid, viewport_top_abs);
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
                    recolor_cursor_glyphs(&mut glyph_instances, qx, qy, qw, qh, sw, sh, fg_color);
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
        let search_font_size = self.raster_px(self.font_size.max(1.0));
        // When search is active and the IME has a non-empty composing run, splice
        // the preedit into the label at the query caret so the whole bar renders
        // as one continuous string: the box grows to fit it and the ` · N/M`
        // counter flows to the right of the composition instead of being
        // overlapped. Display-only — the preedit does not drive matching (only
        // committed text does). (#B14)
        let search_preedit: &str =
            search.and(ime).map(|i| i.preedit()).filter(|s| !s.is_empty()).unwrap_or("");
        let search_label = search.map(|s| search_bar_label(s, search_preedit));
        let search_content_width = search_label.as_ref().map(|label| {
            search_badge_content_width(
                SEARCH_BADGE_ICON,
                label,
                search_font_size,
                self.chrome_px(SEARCH_BAR_ICON_GAP),
                self.font_stack.as_ref(),
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
        if let (Some(label), Some(layout)) = (search_label.as_ref(), search_bar_layout) {
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
                let icon_w = conservative_badge_text_width(
                    estimate_badge_text_width(SEARCH_BADGE_ICON, search_font_size),
                    stack.measure_text_width(SEARCH_BADGE_ICON).ok(),
                );
                let icon_x = layout.border.x + self.chrome_px(SEARCH_BAR_PAD_LEFT);
                let text_x = icon_x + icon_w + self.chrome_px(SEARCH_BAR_ICON_GAP);
                let visible_w = (layout.border.x + layout.border.w
                    - self.chrome_px(SEARCH_BAR_PAD_RIGHT)
                    - text_x)
                    .max(0.0);
                let caret_prefix = search_query_caret_prefix(search_state, search_preedit);
                let prefix_w = measure_overlay_text_width(
                    &mut self.glyph_atlas,
                    stack,
                    search_font_size,
                    search_font_size,
                    &mut wt,
                    &caret_prefix,
                    search_badge_fg,
                );
                let caret_w = cursor_char_slice_at(&search_state.query, search_state.cursor())
                    .map(|ch| {
                        measure_overlay_text_width(
                            &mut self.glyph_atlas,
                            stack,
                            search_font_size,
                            search_font_size,
                            &mut wt,
                            ch,
                            search_badge_fg,
                        )
                        .max(4.0)
                    })
                    .unwrap_or_else(|| (self.cell_w * 0.70).max(4.0));
                // Scroll only enough to keep the entire block cursor visible.
                // When the caret moves left, the committed suffix may be clipped
                // on the right, but the insertion point remains inside the field.
                let scroll_x = search_text_scroll(prefix_w, caret_w, visible_w);
                let caret_max_x = text_x + (visible_w - caret_w).max(0.0);
                let caret_x = (text_x - scroll_x + prefix_w).clamp(text_x, caret_max_x);
                search_ime_anchor = Some((caret_x, layout.border.y, layout.border.h));
                let baseline = layout.border.y + (layout.border.h + search_font_size * 0.8) * 0.5;
                let icon_layout = chrome_text::layout(
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
                    Some(ChromeClip {
                        x: layout.border.x,
                        y: layout.border.y,
                        w: layout.border.w,
                        h: layout.border.h,
                    }),
                );
                overlay_glyph_instances.extend(icon_layout.glyphs);
                let chrome_layout = chrome_text::layout(
                    stack,
                    &mut wt,
                    &mut self.glyph_atlas,
                    label,
                    search_badge_fg,
                    ChromeAttrs::default(),
                    search_font_size,
                    search_font_size,
                    (text_x - scroll_x, baseline),
                    (sw, sh),
                    Some(ChromeClip {
                        x: text_x,
                        y: layout.border.y,
                        w: visible_w,
                        h: layout.border.h,
                    }),
                );
                overlay_glyph_instances.extend(chrome_layout.glyphs);

                // The badge is already cursor-yellow, so invert locally: a
                // theme-background block with the covered glyph recolored to
                // badge yellow. The block overlays existing text and contributes
                // no advance, matching terminal and command-palette cursors.
                let caret_h =
                    (search_font_size * 1.15).min((layout.border.h - self.chrome_px(8.0)).max(4.0));
                let caret_y = layout.border.y + (layout.border.h - caret_h) * 0.5;
                quads_overlay.push(QuadInstance {
                    rect: px_to_ndc(caret_x, caret_y, caret_w, caret_h, sw, sh),
                    color: chrome_color_to_linear_rgba(search_badge_fg),
                    ..Default::default()
                });
                recolor_cursor_glyphs(
                    &mut overlay_glyph_instances,
                    caret_x,
                    caret_y,
                    caret_w,
                    caret_h,
                    sw,
                    sh,
                    search_badge_bg,
                );
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
                // Place the label immediately after the lock icon (no big
                // right-aligned gap): icon_x + icon width + the icon gap.
                let label_x = badge_x
                    + badge_pad_left
                    + icon_layout.width_px
                    + self.chrome_px(SEARCH_BAR_ICON_GAP);
                let _ = badge_pad_right;
                emit_overlay_text_glyphs(
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
                    &mut overlay_glyph_instances,
                    None,
                );
                emit_overlay_text_glyphs(
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
                    &mut overlay_glyph_instances,
                    None,
                );
            }
        }

        if let Some(bubble) = notification {
            let notification_font_size = self.raster_px(self.font_size.max(1.0));
            let content_w = estimate_badge_text_width(&bubble.message, notification_font_size);
            let row = u8::from(read_only_badge.is_some()) + u8::from(search_bar_layout.is_some());
            let layout =
                NotificationBubbleLayout::compute(sw, sh, content_w, row, self.scale_factor);
            let bg_hex = match bubble.level {
                NotificationLevel::Info => theme.colors.bright.green.0.as_str(),
                NotificationLevel::Warning => theme.colors.ansi.yellow.0.as_str(),
                NotificationLevel::Error => theme.colors.bright.red.0.as_str(),
            };
            let bubble_bg = hex_to_premultiplied_rgba(bg_hex, 1.0);
            let bubble_fg = hex_to_chrome_color(theme.colors.background.0.as_str());
            quads_overlay.push(QuadInstance::rounded(
                px_to_ndc(
                    layout.border.x,
                    layout.border.y,
                    layout.border.w,
                    layout.border.h,
                    sw,
                    sh,
                ),
                bubble_bg,
                [layout.border.w, layout.border.h],
                self.chrome_px(READ_ONLY_BADGE_RADIUS),
            ));
            if let Some(stack) = self.font_stack.as_ref() {
                let mut wt = stack.clone();
                let text_x = layout.border.x + self.chrome_px(SEARCH_BAR_PAD_LEFT);
                let text_clip_w = (layout.close.x - text_x).max(0.0);
                let baseline =
                    layout.border.y + (layout.border.h + notification_font_size * 0.8) * 0.5;
                emit_overlay_text_glyphs(
                    &mut self.glyph_atlas,
                    stack,
                    notification_font_size,
                    notification_font_size,
                    &mut wt,
                    &bubble.message,
                    bubble_fg,
                    ChromeAttrs::default(),
                    text_x,
                    baseline,
                    [text_x, layout.border.y, text_clip_w, layout.border.h],
                    sw,
                    sh,
                    &mut overlay_glyph_instances,
                    None,
                );
                let close_w =
                    estimate_badge_text_width(NOTIFICATION_CLOSE_ICON, notification_font_size);
                let close_x = layout.close.x + (layout.close.w - close_w) * 0.5;
                emit_overlay_text_glyphs(
                    &mut self.glyph_atlas,
                    stack,
                    notification_font_size,
                    notification_font_size,
                    &mut wt,
                    NOTIFICATION_CLOSE_ICON,
                    bubble_fg,
                    ChromeAttrs::default(),
                    close_x,
                    baseline,
                    [layout.close.x, layout.close.y, layout.close.w, layout.close.h],
                    sw,
                    sh,
                    &mut overlay_glyph_instances,
                    None,
                );
            }
        }

        // -------- Command palette overlay ----------------------------------
        let palette_preedit = ime.map(|i| i.preedit()).unwrap_or("");
        let (palette_layout, palette_query_text, palette_caret_char) = if let Some(p) = palette {
            let query_text = if palette_preedit.is_empty() {
                None
            } else {
                // When: `!palette_preedit.is_empty()` — an IME composition is
                // live, so the label interleaves it with the typed query.
                Some(command_palette_query_label(p, palette_preedit))
            };
            let layout = PaletteLayout::compute(p, sw, sh, self.panel_padding, self.scale_factor);
            let caret_char = palette_cursor_char(
                p.query(),
                p.cursor(),
                layout.as_ref().and_then(|layout| layout.query_placeholder.as_deref()),
            )
            .map(str::to_string);
            (layout, query_text, caret_char)
        } else {
            // When: `palette` is None — the command palette is closed, so no
            // layout, query, or caret exists for the overlay pass to draw.
            (None, None, None)
        };
        // Chrome colors are derived from the active theme so the palette
        // tracks the user's chosen palette instead of hardcoded
        // Tokyo Night literals (see UiPalette::from_theme).
        if let Some(layout) = &palette_layout {
            // When: `palette_layout` is Some — the palette is open, so its
            // panel, query row, and result rows all need chrome this frame.
            let palette_chrome =
                sonicterm_render_model::boundary::ui::ui_tokens::UiPalette::from_theme(theme);
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
            let query_text = if let Some(text) = &palette_query_text {
                text.replace('▏', "")
            } else if let Some(ph) = &layout.query_placeholder {
                // When: no composed text but `layout.query_placeholder` is Some
                // — an empty query shows its placeholder hint instead.
                ph.clone()
            } else {
                // When: `palette_query_text` and `layout.query_placeholder` are
                // both None — the typed query stands alone.
                layout.query_label.replace('▏', "")
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
                emit_overlay_text_glyphs(
                    &mut self.glyph_atlas,
                    stack,
                    palette_font_size,
                    palette_native_em,
                    &mut palette_rasterizer,
                    &query_text,
                    self.search_fg,
                    ChromeAttrs::default(),
                    query_origin_x,
                    query_baseline_y,
                    [
                        layout.query_row.x,
                        layout.query_row.y,
                        layout.query_row.w,
                        layout.query_row.h,
                    ],
                    sw,
                    sh,
                    &mut overlay_glyph_instances,
                    None,
                );
                let caret_prefix = if let Some(text) = &palette_query_text {
                    text.split('▏').next().unwrap_or("")
                } else {
                    // When: `palette_query_text` is None — no composition, so
                    // the caret prefix comes from the plain query label.
                    layout.query_label.split('▏').next().unwrap_or("")
                };
                let caret_x = query_origin_x
                    + measure_overlay_text_width(
                        &mut self.glyph_atlas,
                        stack,
                        palette_font_size,
                        palette_native_em,
                        &mut palette_rasterizer,
                        caret_prefix,
                        self.search_fg,
                    );
                let caret_w = palette_caret_char
                    .as_deref()
                    .map(|ch| {
                        measure_overlay_text_width(
                            &mut self.glyph_atlas,
                            stack,
                            palette_font_size,
                            palette_native_em,
                            &mut palette_rasterizer,
                            ch,
                            self.search_fg,
                        )
                        .max(4.0)
                    })
                    .unwrap_or_else(|| (self.cell_w * 0.70).max(4.0));
                let caret_h =
                    (palette_font_size * 1.15).min(layout.query_row.h - self.chrome_px(8.0));
                let caret_y = layout.query_row.y + (layout.query_row.h - caret_h) * 0.5;
                quads_overlay.push(QuadInstance {
                    rect: px_to_ndc(caret_x, caret_y, caret_w, caret_h, sw, sh),
                    color: self.cursor_color,
                    ..Default::default()
                });
                recolor_cursor_glyphs(
                    &mut overlay_glyph_instances,
                    caret_x,
                    caret_y,
                    caret_w,
                    caret_h,
                    sw,
                    sh,
                    self.cursor_text_color,
                );

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
                    // Vertically centre the label in the row. Use the row's
                    // ACTUAL (DPI-scaled) height `row.rect.h`, NOT the unscaled
                    // PALETTE_ROW_HEIGHT constant — mixing a logical-px height
                    // with the scaled `row.rect.y` / `palette_font_size` pushed
                    // the baseline off-centre at fractional DPI (the query row
                    // already centres correctly via `query_row.h`). #palette
                    let baseline_y = row.rect.y + (row.rect.h + palette_font_size * 0.8) * 0.5;
                    let label_bounds_w = match shortcut_w {
                        Some(w) => (row.rect.w
                            - w
                            - self.chrome_px(sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_PAD_X) * 2.0
                            - self.chrome_px(sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_COLUMN_GAP))
                        .max(0.0),
                        None => row.rect.w,
                    };
                    emit_overlay_text_glyphs(
                        &mut self.glyph_atlas,
                        stack,
                        palette_font_size,
                        palette_native_em,
                        &mut palette_rasterizer,
                        label,
                        self.search_fg,
                        ChromeAttrs::default(),
                        origin_x,
                        baseline_y,
                        [row.rect.x, row.rect.y, label_bounds_w, row.rect.h],
                        sw,
                        sh,
                        &mut overlay_glyph_instances,
                        None,
                    );
                    if let (Some(hint), Some(width)) = (shortcut, shortcut_w) {
                        let hint_origin_x = row.rect.x + row.rect.w
                            - self.chrome_px(
                                sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_PAD_X,
                            )
                            - width;
                        let mut hint_color = self.search_fg;
                        hint_color.a = 180;
                        emit_overlay_text_glyphs(
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
                            &mut overlay_glyph_instances,
                            None,
                        );
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
                    emit_overlay_text_glyphs(
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
                        &mut overlay_glyph_instances,
                        None,
                    );
                    if let Some(hint) = &layout.empty_hint {
                        let hint_baseline_y = empty_baseline_y
                            + sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_HEIGHT
                            + sonicterm_render_model::boundary::ui::overlays::PALETTE_ROW_GAP;
                        emit_overlay_text_glyphs(
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
                            &mut overlay_glyph_instances,
                            None,
                        );
                    }
                }

                if let Some(footer_stack) = self.palette_footer_font_stack.as_ref() {
                    let footer_font_size = self.raster_px(palette_footer_font_size(self.font_size));
                    let footer_native_em = footer_font_size;
                    let mut footer_rasterizer = footer_stack.clone();
                    let footer_origin_x = layout.footer.x + self.chrome_px(PALETTE_FOOTER_INSET_X);
                    let footer_baseline_y =
                        layout.footer.y + (layout.footer.h + footer_font_size * 0.8) * 0.5;
                    let footer_layout = chrome_text::layout_with_raster_variant(
                        footer_stack,
                        &mut footer_rasterizer,
                        &mut self.glyph_atlas,
                        &layout.footer_label,
                        self.search_fg,
                        ChromeAttrs::default(),
                        footer_font_size,
                        footer_native_em,
                        (footer_origin_x, footer_baseline_y),
                        (sw, sh),
                        Some(ChromeClip {
                            x: layout.footer.x,
                            y: layout.footer.y,
                            w: layout.footer.w,
                            h: layout.footer.h,
                        }),
                        GlyphRasterVariant::PaletteFooter,
                    );
                    overlay_glyph_instances.extend(footer_layout.glyphs);
                }
            }
        }

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

                // (1) Composing text glyphs — vertically centered in the
                // line, nudged a hair right so it doesn't kiss the cell edge.
                // The opaque terminal-bg mask emitted in (0) sits behind these
                // glyphs (plain bg, not a highlight color), so the in-flight
                // run is legible even over app placeholder/hint text.
                // native_em MUST equal font_size here. chrome_text scales each
                // glyph tile by `font_size_px / native_em_px`; using cell_h
                // (which includes line spacing, so cell_h > raster_px(font))
                // made `scale < 1` and rendered the preedit visibly SMALLER
                // than body text. Pass raster_px(font_size) for both so
                // scale == 1 and the composing text matches the body size. #B14
                let native_em = font_size;
                let mut wt = stack.clone();
                let text_pad = self.chrome_px(2.0);
                let baseline_y = top_y + (line_h + font_size * 0.8) * 0.5;
                let preedit_fg = self.search_fg;
                // reuse the memoized preedit glyphs when text +
                // placement + color + atlas generation all match, so a paused
                // or streaming-while-composing preedit isn't re-shaped each
                // frame. Any atlas eviction bumps the epoch and forces a
                // rebuild, so cached atlas UVs can't go stale.
                let emit_x = start_x + text_pad;
                let color_bits = (u32::from(preedit_fg.r) << 24)
                    | (u32::from(preedit_fg.g) << 16)
                    | (u32::from(preedit_fg.b) << 8)
                    | u32::from(preedit_fg.a);
                let atlas_epoch = self.glyph_atlas_epoch();
                let cache_hit = self.preedit_glyph_cache.as_ref().is_some_and(|c| {
                    c.matches(text, font_size, emit_x, baseline_y, color_bits, atlas_epoch)
                });
                if cache_hit {
                    // SAFETY of UVs: epoch match means no eviction since build.
                    let cached = self.preedit_glyph_cache.as_ref().unwrap();
                    overlay_glyph_instances.extend(cached.glyphs.iter().copied());
                } else {
                    // When: `!cache_hit` — text, placement, colour, or atlas
                    // epoch changed, so the run is re-shaped and re-cached.
                    let before = overlay_glyph_instances.len();
                    emit_overlay_text_glyphs(
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
                        &mut overlay_glyph_instances,
                        None,
                    );
                    // Cache the freshly emitted glyphs. Re-read the epoch: the
                    // emit itself may have evicted to make room, in which case
                    // these glyphs are valid against the NEW epoch.
                    self.preedit_glyph_cache = Some(PreeditGlyphCache {
                        text: text.to_string(),
                        font_size,
                        start_x: emit_x,
                        top_y: baseline_y,
                        color_bits,
                        atlas_epoch: self.glyph_atlas_epoch(),
                        glyphs: overlay_glyph_instances[before..].to_vec(),
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

        // Drag-chip overlay: translucent ~120×24 quad that follows the
        // cursor while a tab is held. Drawn AFTER ime/search so it
        // sits on top of everything.
        let broadcast_label_rects: Vec<PaneRect> = pane_rects
            .iter()
            .filter(|(id, _)| broadcast_receiver_ids.contains(id))
            .map(|(_, r)| *r)
            .collect();
        if !broadcast_label_rects.is_empty() {
            // Broadcast warning label → chrome_text, one call per
            // pane rect (each rect gets its own ⚠ BROADCAST string).
            if let Some(stack) = self.font_stack.as_ref() {
                let native_em = stack
                    .cell_metrics_raster_px()
                    .ok()
                    .map(|m| m.cell_h as f32)
                    .unwrap_or(self.cell_h);
                let mut wt = stack.clone();
                let warn_color = hex_to_chrome_color(theme.colors.bright.yellow.0.as_str());
                for rect in broadcast_label_rects.iter() {
                    emit_overlay_text_glyphs(
                        &mut self.glyph_atlas,
                        stack,
                        self.font_size * 0.85,
                        native_em,
                        &mut wt,
                        "⚠ BROADCAST",
                        warn_color,
                        ChromeAttrs::default(),
                        rect.x + 10.0,
                        rect.y + 4.0 + self.font_size * 0.85 * 0.8,
                        [rect.x, rect.y, rect.w, (self.font_size * 1.45).max(20.0)],
                        sw,
                        sh,
                        &mut overlay_glyph_instances,
                        None,
                    );
                }
            }
        }

        if let Some(chip) = self.drag_chip.clone() {
            const CHIP_W: f32 = 120.0;
            const CHIP_H: f32 = 24.0;
            // Two independent multipliers compose here.
            // `chip.scale` is the tear-out ANIMATION ease (1.0 in-bar, 1.02
            // on tear); `dpi` is the display scale factor. The chip's logical
            // size + decorations must scale by DPI so it keeps a constant
            // physical size across displays. `top_left` is already in physical
            // px (cursor-relative, from the app layer) so it is NOT scaled —
            // only the size and the size-derived centering offset are.
            let dpi = self.scale_factor;
            let scale = chip.scale.clamp(0.5, 2.0) * dpi;
            let w = CHIP_W * scale;
            let h = CHIP_H * scale;
            // Re-center the scaled chip so growth is centered around
            // the original anchor point (cursor-relative offset is
            // preserved by the caller in `top_left`).
            let cx = chip.top_left.0 + CHIP_W * 0.5 * dpi;
            let cy = chip.top_left.1 + CHIP_H * 0.5 * dpi;
            let x0 = cx - w * 0.5;
            let y0 = cy - h * 0.5;

            // Soft drop shadow: stack two dimmer quads with growing
            // offset to fake an 8px blur without a fragment shader.
            // Offsets are logical px → scale by DPI.
            for (off, alpha) in [(2.0_f32, 0.18_f32), (4.0_f32, 0.10_f32), (8.0_f32, 0.05_f32)] {
                let off = off * dpi;
                quads_overlay.push(QuadInstance {
                    rect: px_to_ndc(x0 + off, y0 + off, w, h, sw, sh),
                    color: [0.0, 0.0, 0.0, alpha],
                    ..Default::default()
                });
            }

            // Drop-line indicator (in-bar reorder cue). Drawn BEFORE
            // the chip so the chip floats on top if they overlap.
            if let Some(lx) = chip.drop_line_x {
                let (ly0, ly1) = chip.drop_line_y;
                let lh = (ly1 - ly0).max(2.0 * dpi);
                // Drop-line accent — theme-driven (was hardcoded ACCENT_BLUE).
                let line_color = with_premultiplied_alpha(
                    sonicterm_render_model::boundary::ui::ui_tokens::UiPalette::from_theme(theme)
                        .accent,
                    0.95,
                );
                // 3px line centered on lx; both the half-width offset and the
                // width are logical px scaled by DPI.
                quads_overlay.push(QuadInstance {
                    rect: px_to_ndc(lx - 1.5 * dpi, ly0, 3.0 * dpi, lh, sw, sh),
                    color: line_color,
                    ..Default::default()
                });
            }

            // Ghost body: alpha controlled by
            // `chip.ghost_alpha` (spec 0.5). The historical chip
            // rendered at 0.7; the spec ghost is more
            // translucent so the bar underneath stays legible.
            let chip_color = with_premultiplied_alpha(self.tab_active_bg, chip.ghost_alpha);
            quads_overlay.push(QuadInstance {
                rect: px_to_ndc(x0, y0, w, h, sw, sh),
                color: chip_color,
                ..Default::default()
            });

            // Drag-chip title text → chrome_text.
            //
            // Scale the
            // text color alpha by `chip.ghost_alpha` (spec 0.5) so
            // the GHOST TITLE matches the ghost body translucency.
            if !chip.title.is_empty() {
                let ghost_fg =
                    scale_chrome_text_alpha(self.tab_active_fg, chip.ghost_alpha.clamp(0.0, 1.0));
                if let Some(stack) = self.font_stack.as_ref() {
                    let native_em = stack
                        .cell_metrics_raster_px()
                        .ok()
                        .map(|m| m.cell_h as f32)
                        .unwrap_or(self.cell_h);
                    let mut wt = stack.clone();
                    // Match the legacy TextArea geometry: left = x0 + 6,
                    // top = y0 + (h - font_size*0.85*1.2) * 0.5, clip to
                    // chip body inset 4px. Font size goes through raster_px and
                    // the insets through dpi so the ghost title scales with the
                    // chip on HiDPI displays.
                    let chip_font_size = self.raster_px(self.font_size * 0.85);
                    let top = y0 + ((h - chip_font_size * 1.2).max(0.0)) * 0.5;
                    let baseline_y = top + chip_font_size * 0.8;
                    let layout = chrome_text::layout(
                        stack,
                        &mut wt,
                        &mut self.glyph_atlas,
                        &chip.title,
                        ghost_fg,
                        ChromeAttrs::default(),
                        chip_font_size,
                        native_em,
                        (x0 + 6.0 * dpi, baseline_y),
                        (sw, sh),
                        Some(ChromeClip { x: x0 + 4.0 * dpi, y: y0, w: w - 8.0 * dpi, h }),
                    );
                    overlay_glyph_instances.extend(layout.glyphs);
                }
            }
            self.drag_chip_visual = Some(DragChipVisual { top_left: (x0, y0), size: (w, h) });
        } else {
            // When: `self.drag_chip` is None — no tab is being dragged, so the
            // recorded visual is cleared rather than left stale for tests.
            self.drag_chip_visual = None;
        }

        // Glyphon `Resolution` / `TextArea` / `TextBounds` /
        // `text_renderer.prepare` are gone. Every chrome string already
        // landed in `glyph_instances` (pre-overlay: search status bar,
        // tab titles) or `overlay_glyph_instances` (modal chrome:
        // palette, IME preedit, broadcast banner, drag-
        // chip title, quick-select hints) via `chrome_text::layout`
        // earlier in this function. The atlas upload + per-pass draw
        // calls below carry those instances to the GPU.

        // Quick-select hint overlay → chrome_text into the overlay
        // glyph instance vec. Each hint is anchored at its (row, col)
        // cell origin so the hint character sits exactly inside the
        // chosen cell.
        if quick_select_hint_count > 0 {
            // Reconstruct the hint string the legacy
            // `prepare_quick_select_overlay` routed through
            // `self.quick_select_buffer`. The hint set is sparse so
            // emitting per-hint via chrome_text avoids materializing
            // the full padded string.
            if let Some(qs) = copy_mode.and_then(|cm| cm.quick_select.as_ref()) {
                let bg_color = hex_to_chrome_color(theme.colors.background.0.as_str());
                if let Some(stack) = self.font_stack.as_ref() {
                    let native_em = stack
                        .cell_metrics_raster_px()
                        .ok()
                        .map(|m| m.cell_h as f32)
                        .unwrap_or(self.cell_h);
                    let mut wt = stack.clone();
                    for hint in &qs.hints {
                        let x = active_origin_x + hint.col_start as f32 * self.cell_w;
                        let y = active_origin_y + hint.row as f32 * self.cell_h;
                        let s = hint.hint.to_string();
                        let l = chrome_text::layout(
                            stack,
                            &mut wt,
                            &mut self.glyph_atlas,
                            &s,
                            bg_color,
                            ChromeAttrs::default(),
                            self.font_size,
                            native_em,
                            (x, y + self.font_size * 0.8),
                            (sw, sh),
                            None,
                        );
                        overlay_glyph_instances.extend(l.glyphs);
                    }
                }
            }
        }

        gpu_lap!("overlays");

        if atlas_evicted_during_frame(atlas_epoch_at_frame_start, &self.glyph_atlas) {
            // When: `atlas_evicted_during_frame` — a tile was recycled mid-
            // assembly, so glyphs emitted earlier hold UVs into freed space.
            self.reset_glyph_atlas_after_eviction(atlas_epoch_at_frame_start);
            return Ok(());
        }

        #[cfg(debug_assertions)]
        {
            crate::quad::debug_assert_premultiplied_quads("base", &quads);
            crate::quad::debug_assert_premultiplied_quads("overlay", &quads_overlay);
        }

        #[cfg(target_os = "windows")]
        if self.software_render_degrade {
            // When: `software_render_degrade` on Windows — frames reach the
            // window through the CPU blitter, not the swapchain.
            let bg_clear = [self.bg.r as f32, self.bg.g as f32, self.bg.b as f32, self.bg.a as f32];
            if self.software_frame.is_none() {
                // First degraded frame, or the buffer was released when the
                // path last turned off.
                self.software_frame = Some(crate::software_windows::WindowsSoftwareFrame::new(
                    self.config.width,
                    self.config.height,
                    bg_clear,
                )?);
            }
            let frame = self.software_frame.as_mut().expect("software frame initialized");
            frame.prepare(self.config.width, self.config.height, bg_clear)?;
            frame.draw_layers(
                &self.glyph_atlas,
                &self.image_atlas,
                &quads,
                &image_glyph_instances,
                &glyph_instances,
                &quads_overlay,
                &overlay_glyph_instances,
            );
            self.glyph_atlas.clear_dirty_rects();
            self.image_atlas.clear_dirty_rects();
            frame.present(&self.window)?;
            gpu_lap!("software_present");
            self.finish_successful_frame(
                key,
                missing_chars_this_frame,
                panes,
                render_mode,
                damaged_rows,
                gpu_timing,
            );
            return Ok(());
        }

        // B3: push any new glyph tiles to the GPU texture before any
        // draw call samples it. Must come AFTER the grid walk above
        // (which is what populated the dirty rects) and BEFORE the
        // WezTerm presentation draw call in the render pass below.
        let image_upload_stats = self.image_upload.sync(
            &self.queue,
            &mut self.image_atlas,
            AtlasPixelEncoding::PremultipliedSrgb,
        );
        let glyph_upload_stats = self.glyph_upload.sync(
            &self.queue,
            &mut self.glyph_atlas,
            AtlasPixelEncoding::Coverage,
        );
        self.log_atlas_upload_stats("image", image_upload_stats, retained_inline_media_bytes);
        self.log_atlas_upload_stats("glyph", glyph_upload_stats, retained_inline_media_bytes);
        gpu_lap!("glyph_upload");

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                // When: `Timeout` or `Occluded` — no texture was handed back,
                // so there is nothing to draw into this frame.

                // Invariant: any render() that returns without a successful
                // present must force the next render() onto the full-redraw
                // path. Otherwise an unchanged FrameKey hits the fast path at
                // the top of render() and skips the present again, leaving a
                // freshly (re)configured swapchain texture blank until the
                // next output changes the key.
                self.last_frame_key = None;
                self.window.request_redraw();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                // When: `Outdated` — the swapchain no longer matches the window
                // (a resize landed between configure and acquire).
                self.last_frame_key = None;
                self.surface.configure(&self.device, &self.config);
                self.last_frame_key = None;
                self.window.request_redraw();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                // When: `Suboptimal` — the swapchain still works but no longer
                // matches the surface, so it is reconfigured before reuse.

                // wgpu 29: Surface::configure panics if a SurfaceTexture is
                // still alive. Drop the frame BEFORE reconfiguring.
                drop(frame);
                self.last_frame_key = None;
                self.surface.configure(&self.device, &self.config);
                self.last_frame_key = None;
                self.window.request_redraw();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                // When: `Lost` — the surface itself is gone (display change,
                // driver reset), so it is recreated rather than reconfigured.
                self.last_frame_key = None;
                self.surface = self.instance.create_surface(self.window.clone())?;
                self.surface.configure(&self.device, &self.config);
                self.last_frame_key = None;
                self.window.request_redraw();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                // When: `Validation` — a driver-level error the renderer cannot
                // recover from by reconfiguring, so it propagates.
                return Err(anyhow!("surface validation error"));
            }
        };
        gpu_lap!("surface_acquire");
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder =
            self.device.create_command_encoder(&CommandEncoderDescriptor { label: Some("sonic") });
        let first_retained_frame = self.last_frame_key.is_none();
        let software_full_repaint =
            self.software_render_degrade && matches!(render_mode, RenderMode::Full);
        let damage_rect = if first_retained_frame || software_full_repaint {
            surface_rect
        } else {
            // When: neither `first_retained_frame` nor `software_full_repaint`
            // — the retained texture is valid, so only damage is redrawn.
            damage.rect().unwrap_or(surface_rect)
        };
        let bg_clear = [self.bg.r as f32, self.bg.g as f32, self.bg.b as f32, self.bg.a as f32];
        let mut retained_quads = Vec::with_capacity(quads.len() + 1);
        retained_quads.push(QuadInstance::sharp(
            px_to_ndc(
                damage_rect.x as f32,
                damage_rect.y as f32,
                damage_rect.w as f32,
                damage_rect.h as f32,
                sw,
                sh,
            ),
            bg_clear,
        ));
        retained_quads.extend_from_slice(&quads);
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("sonic-retained-pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &self.frame_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: if first_retained_frame {
                            LoadOp::Clear(self.bg)
                        } else {
                            // When: `!first_retained_frame` — the texture holds
                            // the last frame, and clearing would discard it.
                            LoadOp::Load
                        },
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_scissor_rect(
                damage_rect.x.max(0) as u32,
                damage_rect.y.max(0) as u32,
                damage_rect.w.max(1),
                damage_rect.h.max(1),
            );
            // WezTerm-style final presentation: every glyph and colored
            // geometry primitive flows through one vertex/shader/indexed-draw
            // path. The ordering preserves the previous painter stack:
            // base quads -> inline images -> base glyphs -> overlay quads
            // -> overlay glyphs.
            self.present_pipeline.draw_frame(
                &self.device,
                &self.queue,
                &mut pass,
                self.image_upload.color_bind_group(),
                self.glyph_upload.coverage_bind_group(),
                sw,
                sh,
                &retained_quads,
                &image_glyph_instances,
                &glyph_instances,
                &quads_overlay,
                &overlay_glyph_instances,
            );
        }
        self.frame_blitter.copy(&self.device, &mut encoder, &self.frame_view, &view);
        gpu_lap!("render_pass");
        self.queue.submit(Some(encoder.finish()));
        gpu_lap!("queue_submit");
        self.queue.present(frame);
        gpu_lap!("present");
        self.finish_successful_frame(
            key,
            missing_chars_this_frame,
            panes,
            render_mode,
            damaged_rows,
            gpu_timing,
        );
        Ok(())
    }

    fn finish_successful_frame(
        &mut self,
        key: FrameKey,
        missing_chars_this_frame: Vec<char>,
        panes: &mut [sonicterm_render_model::PaneRender<'_>],
        render_mode: RenderMode,
        damaged_rows: usize,
        gpu_timing: Option<(Instant, Instant, Vec<(&'static str, f32)>)>,
    ) {
        self.successful_frame_count = self.successful_frame_count.saturating_add(1);
        if std::mem::take(&mut self.glyph_atlas_retry_without_eviction) {
            self.glyph_atlas.set_eviction_enabled(true);
            self.row_glyph_cache.invalidate_all();
            self.preedit_glyph_cache = None;
            tracing::warn!(
                target: "sonic::glyph_atlas",
                resident = self.glyph_atlas.len(),
                misses = self.glyph_atlas.misses(),
                "glyph atlas compaction retry presented with eviction disabled"
            );
        }
        self.last_missing_chars = missing_chars_this_frame;
        self.last_frame_key = Some(key);
        if self.pane_focus_flash.is_some() {
            self.window.request_redraw();
        }
        for p in panes.iter_mut() {
            p.grid.clear_dirty();
        }
        if let Some((start, last, mut parts)) = gpu_timing {
            let now = Instant::now();
            parts.push(("cleanup", now.saturating_duration_since(last).as_secs_f32() * 1000.0));
            let total_ms = now.saturating_duration_since(start).as_secs_f32() * 1000.0;
            let mode = match render_mode {
                RenderMode::Full => "full",
                RenderMode::Noop => "noop",
            };
            let mut line = format!(
                "[gpu_render_timing] window={} mode={mode} damaged_rows={damaged_rows} total={total_ms:.2}ms",
                self.render_timing_label
            );
            for (name, ms) in parts {
                line.push_str(&format!(" {name}={ms:.2}ms"));
            }
            tracing::debug!(target: "render_timing", %line);
        }
    }

    /// This function only emits the quick-select hint background
    /// quads now. The legacy `quick_select_buffer` text path is gone;
    /// the per-hint text is laid out via `chrome_text::layout` later
    /// in `render()` so it shares the wezterm atlas with the rest of
    /// the chrome.
    #[allow(clippy::too_many_arguments)]
    fn prepare_quick_select_overlay(
        &mut self,
        quick_select: &QuickSelectState,
        origin_x: f32,
        origin_y: f32,
        scrollback_len: usize,
        visible_rows: usize,
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
            let Some(visible_row) = hint.row.checked_sub(scrollback_len) else {
                // When: `hint.row.checked_sub(scrollback_len)` is None — the
                // hint sits above the viewport, in scrolled-off history.
                continue;
            };
            if visible_row >= visible_rows {
                // When: `visible_row >= visible_rows` — below the viewport, so
                // its background quad would land outside the pane.
                continue;
            }
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

    /// Shape a single style-run worth of cells and append the
    /// resulting glyph instances + missing-glyph tofus to the frame's
    /// queues. Factored out of the per-row loop so the loop body stays
    /// readable; otherwise it would inline ~80 lines of placement +
    /// fallback handling four times (run start, mid-row flush, end of
    /// row, etc.).
    ///
    /// Non-ASCII clusters drive through
    /// `shape_run_with_wezterm` only — the cosmic-text path plus the
    /// legacy wezterm-cluster-width overlay are gone. Each cluster
    /// lead cell dispatches on
    /// [`sonicterm_block_glyph::BlockKey::from_char`]: on `Some`, the
    /// atlas pulls a [`sonicterm_block_glyph::block_sprite`] tile
    /// keyed under the block-glyph sentinel
    /// (`GlyphKey { font_slot: 0xFF, glyph_id: <hashed SizedBlockKey>,
    /// .. }`) so the wezterm shape path and the block-sprite path
    /// share the atlas without colliding; on `None`, the cluster
    /// follows the normal sonicterm-font rasterize path. Box drawing,
    /// Powerline, Sextant, Octant, and Braille all reach the renderer
    /// through this dispatch — there is no fallback to the swash-
    /// rasterized font glyph for codepoints `BlockKey` recognizes.
    // Hot inner-loop helper called per shaped run per row. Every
    // argument is an exclusive `&mut` borrow of a *different* field of
    // `GpuRenderer` (atlas, rasterizer, instance buffers, missing-glyph
    // trackers) — bundling them into a struct would force a single
    // `&mut Ctx` that conflicts with the surrounding loop's own
    // borrows. Suppression stays with this explanatory comment.
    #[allow(clippy::too_many_arguments)]
    fn flush_shape_run(
        glyph_atlas: &mut GlyphAtlas,
        _font_family: &str,
        _font_size: f32,
        glyph_instances: &mut Vec<GlyphInstance>,
        missing_tofu: &mut Vec<(f32, f32, f32, f32, ChromeColor)>,
        missing_chars_this_frame: &mut Vec<char>,
        row: u16,
        _run_first_col: u16,
        style: RunStyle,
        cells: &[(u16, Cell)],
        theme: &Theme,
        fg_default: ChromeColor,
        cell_w: f32,
        cell_h: f32,
        top_inset: f32,
        _pad: f32,
        sw: f32,
        sh: f32,
        baseline_y_in_cell: f32,
        snapped_cell_x: &[f32],
        // `font_stack` is now the sole
        // shape entry point — when None, the non-ASCII branch can
        // emit nothing (test fixtures without bundled fonts hit
        // this; the ASCII branch still drives through `wt_raster`
        // if it's been wired). The Option shape is kept so
        // `GpuRenderer::new` can continue to construct a partly-
        // degraded renderer in tests.
        font_stack: Option<&sonicterm_engine::FontStack>,
        // Sonicterm-font is now the sole
        // atlas insertion path. The legacy `rasterizer: &mut
        // SwashRasterizer` parameter is gone (T10 deletes the type
        // entirely). When `wt_raster` is None (test fixtures without
        // a FontStack), the function emits no glyphs — the renderer
        // still paints quads (bg, cursor, underlines) so the frame is
        // visually coherent.
        mut wt_raster: Option<&mut sonicterm_engine::FontStack>,
        // Cmd-hovered URL cell range for this pane (viewport coords),
        // already gated to the active pane by the caller. When a cell's
        // (row, col) falls inside this span the glyph's foreground is
        // overridden with `hovered_url_accent`. `None` = no recolor.
        hovered_url_cells: Option<sonicterm_render_model::inputs::HoveredUrlCells>,
        // Theme accent in linear-sRGB `[f32;4]` (alpha 1.0) used for the
        // recolor above. Same color space the per-glyph `color` field
        // already carries, so it is assigned with no conversion.
        hovered_url_accent: [f32; 4],
        software_presenter: bool,
    ) {
        if cells.is_empty() {
            // When: `cells.is_empty()` — the run carries no cells, so there is
            // nothing to shape and no glyph to emit.
            return;
        }

        // Resolve a monochrome glyph's foreground to linear-sRGB rgba,
        // swapping in the theme accent when this cell sits inside the
        // Cmd-hovered URL span. `row` is fixed for the whole run; only
        // `col` varies per glyph. Used by every non-color emit path
        // below so the recolor is applied uniformly (ASCII fast path,
        // char-fallback, and the main shaped path). Color glyphs
        // (`info.is_color`) bypass this and keep their own tile color.
        let resolve_fg = |col: u16, base: ChromeColor| -> [f32; 4] {
            match hovered_url_cells {
                // Only the ACTIVE (modifier-held) hover recolors glyphs to the
                // accent. A plain-hover hint leaves the text color alone and is
                // marked by the yellow underline only. #URL-hint
                Some(h) if h.active && h.contains(row, col) => hovered_url_accent,
                _ => chrome_color_to_linear_rgba(base),
            }
        };

        // ASCII fast path: every cell is printable-ASCII (0x20..=0x7E)
        // with no cluster extras and no ligature trigger, so the shaper
        // would emit a 1:1 mapping anyway. Skip the shape call entirely
        // and drive the glyph atlas straight from each cell's GlyphKey.
        //
        // ASCII codepoints (0x20..=0x7E) never overlap the
        // `BlockKey::from_char` ranges (≥ U+2500) and never carry a
        // Powerline / NF PUA codepoint, so the BlockKey dispatch is
        // safely skipped here.
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
                // Sonicterm-font owns the atlas. No swash
                // fallback — when `wt_raster` is None (test fixture
                // without a FontStack) the glyph is silently skipped
                // so the renderer still paints quads.
                let Some(wt) = wt_raster.as_deref_mut() else {
                    // When: `wt_raster` is None — a test fixture with no
                    // FontStack; quads still paint, glyphs are skipped.
                    continue;
                };
                let info_opt = glyph_atlas.get_or_insert(key, wt);
                let Some(info) = info_opt else {
                    // When: `info_opt` is None — the rasterizer produced no
                    // tile, so the cell would draw tofu.
                    if !cell.ch.is_whitespace() {
                        // Blanks are intentionally tile-less and are not
                        // reported as missing.
                        missing_chars_this_frame.push(cell.ch);
                    }
                    continue;
                };
                if info.px_size[0] == 0 || info.px_size[1] == 0 {
                    // When: either axis of `info.px_size` is 0 — a zero-area
                    // tile, which is what a space rasterizes to.
                    continue;
                }
                let cx = snapped_cell_x[*col as usize];
                let cy = top_inset + f32::from(row) * cell_h;
                // G1a: atlas px == draw px == raster px, so the prior
                // atlas-to-logical projection collapses to the identity.
                let inv_s = 1.0_f32;
                let gx = cx + info.px_offset[0] as f32 * inv_s;
                let gy = cy + baseline_y_in_cell + info.px_offset[1] as f32 * inv_s;
                let gw = info.px_size[0] as f32 * inv_s;
                let gh = info.px_size[1] as f32 * inv_s;
                // The legacy `apply_symbol_fit_v2` +
                // `block_element_rect` overlay tracks the SwashRasterizer
                // path; sonicterm-font handles cell fit natively. ASCII
                // glyphs are always `Natural` (identity) so dropping
                // the overlay is a no-op for the steady-state hot path.
                let (gx, gy, gw, gh) =
                    sonicterm_render_model::geometry::snap_to_device_pixels((gx, gy, gw, gh), 1.0);
                let color = cell_fg(cell, theme, fg_default);
                let rgba = if info.is_color {
                    [1.0, 1.0, 1.0, 1.0]
                } else {
                    // When: `!info.is_color` — a monochrome mask, so the cell
                    // foreground and any hover recolor apply to it.
                    resolve_fg(*col, color)
                };
                trace_white_glyph(cell.ch, rgba, (gx, gy, gw, gh), "ascii");
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
                glyph_instances.push(GlyphInstance {
                    rect: px_to_ndc(gx, gy, gw, gh, sw, sh),
                    uv: info.uv,
                    color: rgba,
                    flags: glyph_flags(info.is_color, info.is_subpixel),
                });
            }
            return;
        }

        // ── Non-ASCII / mixed run ── T9: drive sonicterm-font directly.
        //
        // Build the text + byte-to-col map for `shape_run_with_wezterm`.
        // Identical to the legacy cluster-width overlay's input
        // assembly so wezterm sees the same input bytes.
        let Some(stack) = font_stack else {
            // When: `font_stack` is None — no shaper, so non-ASCII clusters
            // emit nothing. Test-only path; production always carries a stack.
            return;
        };
        let mut text = String::with_capacity(cells.len() * 2);
        let mut cell_cols: Vec<u16> = Vec::with_capacity(cells.len() * 2);
        for (col, cell) in cells {
            let start = text.len();
            text.push(cell.ch);
            if let Some(extras) = cell.extras() {
                for ch in extras.chars() {
                    text.push(ch);
                }
            }
            let appended = text.len() - start;
            for _ in 0..appended {
                cell_cols.push(*col);
            }
        }
        if text.is_empty() {
            // When: `text.is_empty()` — every cell in the run was a wide
            // continuation, so the shaper has no bytes to work on.
            return;
        }

        let infos = match stack.shape_text_with_style(&text, style.bold, style.italic) {
            Ok(v) => v,
            Err(_) => {
                // When: `shape_text_with_style` returns `Err` — the face
                // rejected the run, so no glyph ids exist to place.
                return;
            }
        };

        // Build a lookup from col → cell so we can recover per-cell
        // attributes (color, WIDE flag, the actual codepoint for tofu
        // diagnostics) from the shaped output's `lead_col`.
        let mut cell_by_col: std::collections::HashMap<u16, Cell> =
            std::collections::HashMap::with_capacity(cells.len());
        for (col, c) in cells {
            cell_by_col.insert(*col, c.clone());
        }

        // We consume WezTerm's GlyphInfo directly here and project it into the
        // Sonic glyph record the rest of the renderer already uses. No
        // WtShapedGlyph wrapper: cluster byte offsets map straight back through
        // `cell_cols`.
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
                cell_by_col.get(&lead_col).map(|c| c.ch).or(info.only_char).unwrap_or(' ');
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
            let lead_cell = cell_by_col.get(&g.lead_col).cloned().unwrap_or_default();
            let is_wide = lead_cell.flags.contains(CellFlags::WIDE);
            let cluster_cells = g.cluster_cells.max(1) as usize;
            let cells_to_span = if is_wide { 2 } else { cluster_cells };
            let cell_pixel_width = cell_w * cells_to_span as f32;

            // ── T9: BlockKey dispatch at the cluster lead cell ──
            //
            // Box-drawing (U+2500..=U+259F), Powerline (U+E0A0..=U+E0D7),
            // Sextant (U+1FB00..), Octant, and Braille (U+2800..) all
            // recognize via `BlockKey::from_char`. When the lead cell
            // resolves, the vendored wezterm geometry produces the
            // glyph; the atlas keys it under
            // `(font_slot = 0xFF, glyph_id = hashed SizedBlockKey)` so
            // it never collides with a wezterm-shaped glyph
            // (`FallbackIdx` truncated to u8 cannot reach 0xFF in
            // practice — wezterm chains a handful of fallbacks, never
            // 255). The shaper-reported `glyph_id` is intentionally
            // ignored for this branch — wezterm itself draws block
            // glyphs through the same `customglyph::block_sprite` we
            // vendored, so taking the font glyph would produce the
            // wrong rendering (or tofu, if the chosen face lacks the
            // codepoint).
            if let Some(block_key) = sonicterm_block_glyph::BlockKey::from_char(lead_cell.ch) {
                // When: `BlockKey::from_char` is Some — a box/block codepoint,
                // drawn from vendored geometry rather than the font's glyph.
                let cx = snapped_cell_x[g.lead_col as usize];
                let cy = top_inset + f32::from(row) * cell_h;
                let span = if is_wide { 2usize } else { cluster_cells };
                let end_col = ((g.lead_col as usize) + span).min(snapped_cell_x.len() - 1);
                let cell_right = snapped_cell_x[end_col];
                let (gx, gy, gw, gh) = if software_presenter {
                    let cell_bottom = top_inset + (f32::from(row) + 1.0) * cell_h;
                    software_block_glyph_target_rect(cx, cy, cell_right, cell_bottom)
                } else {
                    // When: `!software_presenter` — the GPU path keeps the
                    // established fractional font-cell geometry unchanged.
                    (cx, cy, cell_right - cx, cell_h)
                };
                // Hardware keeps the established font-cell geometry unchanged.
                // The software presenter rasterizes to the exact integer
                // destination so glyph-atlas sampling stays one-to-one.
                let cell_w_i =
                    if software_presenter { gw } else { cell_w }.round().max(1.0) as isize;
                let cell_h_i =
                    if software_presenter { gh } else { cell_h }.round().max(1.0) as isize;
                // Bug 4 / wezterm-takeover: stroke width for the
                // `PolyStyle::Outline` box-drawing path comes from the
                // font's actual `underline_thickness`, mirroring
                // wezterm-gui's `utilsprites.rs:29` (`metrics
                // .underline_thickness.get().round().max(1.) as isize`).
                // A hardcoded 1 was producing nearly-invisible 1-device-px
                // strokes that looked like tofu rectangles at every font
                // size — the user-reported "U+2500 renders as a single
                // tofu box" symptom. Use the font metric when we have
                // it; fall back to a 1/16-cell-height heuristic for the
                // test fixture path with no FontStack.
                let underline_h_isize: isize = font_stack
                    .and_then(|s| s.cell_metrics_raster_px().ok())
                    .map(|m| m.underline_h.round().max(1.0) as isize)
                    .unwrap_or_else(|| ((cell_h / 16.0).round().max(1.0)) as isize);
                let size = sonicterm_block_glyph::glue::Size::new(cell_w_i, cell_h_i);
                let sized_key = sonicterm_block_glyph::SizedBlockKey { block: block_key, size };
                // BlockKey identity collapses to a u32 via the std
                // `DefaultHasher`. Block glyphs are size-sensitive
                // (the same key at a different cell pitch produces a
                // different bitmap) so the hash inputs include the
                // packed cell dims as well as the variant. We don't
                // need cryptographic strength — only collision
                // resistance among the ~hundred block glyphs the
                // renderer touches per frame; `DefaultHasher` is
                // overkill but free.
                //
                // Bug 4 fix: `underline_h_isize` participates in the
                // hash too — the same SizedBlockKey at the same cell
                // size renders with a different stroke width when the
                // font's underline_thickness changes (e.g. live font
                // family swap), so the cached tile would be stale
                // without this bit of the key.
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
                // Block glyphs ignore bold/italic — the geometry is the
                // same regardless of cell style, so collapse those bits
                // to keep the cache footprint minimal.
                let key = sonicterm_types::glyph_key::GlyphKey {
                    ch: lead_cell.ch,
                    font_slot: 0xFF,
                    weight_bold: false,
                    italic: false,
                    glyph_id: glyph_id_u32,
                    raster_variant: GlyphRasterVariant::Normal,
                };
                // Wrap `block_sprite` in a thin `Rasterizer` so the
                // atlas only computes the sprite on a cache miss.
                // Identity is captured by `key` above; the rasterizer
                // ignores its `GlyphKey` argument and returns the
                // tile derived from `sized_key`.
                struct BlockSpriteRasterizer {
                    sized_key: sonicterm_block_glyph::SizedBlockKey,
                    underline_h: isize,
                }
                impl sonicterm_text::glyph_atlas::Rasterizer for BlockSpriteRasterizer {
                    fn rasterize(
                        &mut self,
                        _key: sonicterm_types::glyph_key::GlyphKey,
                    ) -> Option<sonicterm_text::glyph_atlas::RasterTile> {
                        // Synthesize the BlockCellMetrics input that
                        // `block_sprite` expects. Customglyph reads
                        // `cell_size`, `underline_height`, and (only
                        // under the `PolyWithCustomMetrics` arm)
                        // descender / descender_row / descender_plus_two
                        // / strike_row. Cell metrics are derived from
                        // the SizedBlockKey's `size`. The underline
                        // height arrives from the font (Bug 4 fix —
                        // hardcoded 1 made Outline strokes invisible).
                        // anti_alias=true — matches the wezterm-gui
                        // default behavior (`config.anti_alias = true`).
                        // We don't surface a config knob: per spec
                        // "where wezterm and sonicterm disagree,
                        // wezterm wins" + the upstream default is AA.
                        let block_tile = sonicterm_block_glyph::block_sprite_with_cell_metrics(
                            self.sized_key,
                            self.underline_h,
                            true,
                        )
                        .ok()?;
                        // T7 Option A: field-for-field copy
                        // `BlockRasterTile` → `RasterTile`. Same
                        // semantics; T10 may collapse the
                        // duplicate by re-exporting `RasterTile`
                        // directly from `sonicterm-text` once that
                        // crate compiles again.
                        let alpha_mask: Vec<u8> =
                            block_tile.coverage.as_chunks::<4>().0.iter().map(|px| px[3]).collect();
                        Some(sonicterm_text::glyph_atlas::RasterTile {
                            width: block_tile.width,
                            height: block_tile.height,
                            offset_x: block_tile.offset_x,
                            offset_y: block_tile.offset_y,
                            advance: block_tile.advance,
                            coverage: alpha_mask,
                            // WezTerm customglyph geometry is a mask for the
                            // cell foreground, not a self-colored emoji. Treat
                            // it as monochrome coverage so brand/icons like
                            // claude's red block logo inherit SGR fg.
                            is_color: false,
                            is_subpixel: false,
                        })
                    }
                }
                let mut block_raster =
                    BlockSpriteRasterizer { sized_key, underline_h: underline_h_isize };
                let Some(info) = glyph_atlas.get_or_insert(key, &mut block_raster) else {
                    // When: `glyph_atlas.get_or_insert` is None — the block
                    // sprite could not be rasterized or packed.
                    continue;
                };
                if info.px_size[0] == 0 || info.px_size[1] == 0 {
                    // When: either axis of `info.px_size` is 0 — a zero-area
                    // sprite, which has no pixels to blit.
                    continue;
                }
                // `block_sprite` was generated from this exact target size.
                // Keeping the destination identical avoids software resampling;
                // the hardware branch retains its established fractional rect.
                let color = cell_fg(&lead_cell, theme, fg_default);
                // Block glyphs are converted to monochrome masks so they
                // honour the cell foreground and Cmd-hover URL recolor like text.
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
                    final_rect = ?(gx, gy, gw, gh),
                    final_rgba = ?rgba,
                    is_color = info.is_color,
                    path = "block_sprite",
                    "glyph render emit (block-glyph)"
                );
                glyph_instances.push(GlyphInstance {
                    rect: px_to_ndc(gx, gy, gw, gh, sw, sh),
                    uv: info.uv,
                    color: rgba,
                    flags: glyph_flags(info.is_color, info.is_subpixel),
                });
                continue;
            }

            // ── Normal wezterm-shape path (non-block cluster) ──
            //
            let shape_x_offset = shaped_cluster_x_offset(
                &mut positioned_cluster_col,
                &mut positioned_cluster_pen_x,
                g,
            );
            // Post-glyphon the char-fallback path is wezterm-
            // only. FontStack is the sole rasterizer; missing chars
            // emit tofu via `Rasterizer::rasterize` returning
            // None (when sonicterm-font's fallback chain has nothing).
            if g.glyph_id == 0 {
                // When: `g.glyph_id == 0` — the shaper found no glyph for this
                // cluster, so the char-fallback path runs instead.
                let ch = lead_cell.ch;
                if ch == '\0' || ch.is_whitespace() {
                    // When: `ch == '\0' || ch.is_whitespace()` — both are
                    // legitimately glyph-less and must not draw tofu.
                    continue;
                }
                // Drop the `resolve_slot` swash walk. wezterm
                // handles fallback internally — pass `font_slot = 0`
                // and let `FontStack::rasterize` find a face
                // (it shapes the single char against the loaded font
                // when glyph_id == 0).
                let slot: u8 = 0;
                let key = sonicterm_types::glyph_key::GlyphKey {
                    ch,
                    font_slot: slot,
                    weight_bold: style.bold,
                    italic: style.italic,
                    glyph_id: 0,
                    raster_variant: GlyphRasterVariant::Normal,
                };
                let Some(wt) = wt_raster.as_deref_mut() else {
                    // When: `wt_raster` is None — a test fixture with no
                    // FontStack, so the fallback char cannot be rasterized.
                    continue;
                };
                let info_opt = glyph_atlas.get_or_insert(key, wt);
                let Some(info) = info_opt else {
                    // When: `info_opt` is None — true tofu; the fallback chain
                    // rejected the char, so an outline box is drawn instead.

                    let cx = snapped_cell_x[g.lead_col as usize];
                    let cy = top_inset + f32::from(row) * cell_h;
                    let inset = (cell_h * 0.12).max(1.0);
                    missing_tofu.push((
                        cx + inset,
                        cy + inset,
                        cell_pixel_width - inset * 2.0,
                        cell_h - inset * 2.0,
                        cell_fg(&lead_cell, theme, fg_default),
                    ));
                    missing_chars_this_frame.push(ch);
                    continue;
                };
                if info.px_size[0] == 0 || info.px_size[1] == 0 {
                    // When: either axis of `info.px_size` is 0 — the fallback
                    // face produced a zero-area tile, which has no pixels.
                    continue;
                }
                let cx = snapped_cell_x[g.lead_col as usize];
                let cy = top_inset + f32::from(row) * cell_h;
                let inv_s = 1.0_f32;
                let gx = cx + info.px_offset[0] as f32 * inv_s;
                let gy = cy + baseline_y_in_cell + info.px_offset[1] as f32 * inv_s;
                let gw = info.px_size[0] as f32 * inv_s;
                let gh = info.px_size[1] as f32 * inv_s;
                let (gx, gy, gw, gh) =
                    positioned_shaped_glyph_rect((gx, gy, gw, gh), shape_x_offset, g.y_offset);
                let cell_right =
                    snapped_cell_x.get(g.lead_col as usize + 1).copied().unwrap_or(cx + cell_w);
                let (gx, gy, gw, gh) = fit_single_cell_status_marker(
                    ch,
                    cluster_cells,
                    is_wide,
                    lead_cell.extras().is_some(),
                    (gx, gy, gw, gh),
                    (cx, cy, cell_right - cx, cell_h),
                );
                // All other glyphs keep their natural raster geometry. In
                // particular, multi-cell ligature halves may exceed one cell.
                let color = cell_fg(&lead_cell, theme, fg_default);
                let rgba = if info.is_color {
                    [1.0, 1.0, 1.0, 1.0]
                } else {
                    // When: `!info.is_color` — a monochrome fallback glyph, so
                    // it takes the cell foreground like ordinary text.
                    resolve_fg(g.lead_col, color)
                };
                let (gx, gy, gw, gh) =
                    sonicterm_render_model::geometry::snap_to_device_pixels((gx, gy, gw, gh), 1.0);
                trace_white_glyph(lead_cell.ch, rgba, (gx, gy, gw, gh), "shaped_run");
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
                glyph_instances.push(GlyphInstance {
                    rect: px_to_ndc(gx, gy, gw, gh, sw, sh),
                    uv: info.uv,
                    color: rgba,
                    flags: glyph_flags(info.is_color, info.is_subpixel),
                });
                continue;
            }

            let key = sonicterm_types::glyph_key::GlyphKey::shaped(
                g.ch,
                g.font_slot,
                g.glyph_id,
                style.bold,
                style.italic,
            );
            // Sonicterm-font is the sole rasterizer; the
            // legacy `swash_rasterizer::classify_symbol` / SymbolFit
            // family routes through the SwashRasterizer which is gone.
            // sonicterm-font sizes glyphs natively, so the IconCellFit
            // resample helper isn't needed either. Atlas keys remain
            // identical (font_slot, glyph_id) so cached tiles survive.
            let Some(wt) = wt_raster.as_deref_mut() else {
                // When: `wt_raster` is None — a test fixture with no FontStack,
                // so the ligature glyph cannot be rasterized.
                continue;
            };
            let Some(info) = glyph_atlas.get_or_insert(key, wt) else {
                // When: `glyph_atlas.get_or_insert` is None — the face produced
                // no tile for this shaped glyph id.
                continue;
            };
            if info.px_size[0] == 0 || info.px_size[1] == 0 {
                // When: either axis of `info.px_size` is 0 — a zero-area tile,
                // which has no pixels to blit.
                continue;
            }
            let cx = snapped_cell_x[g.lead_col as usize];
            let cy = top_inset + f32::from(row) * cell_h;
            let inv_s = 1.0_f32;
            let gx = cx + info.px_offset[0] as f32 * inv_s;
            let gy = cy + baseline_y_in_cell + info.px_offset[1] as f32 * inv_s;
            let gw = info.px_size[0] as f32 * inv_s;
            let gh = info.px_size[1] as f32 * inv_s;
            let (gx, gy, gw, gh) =
                positioned_shaped_glyph_rect((gx, gy, gw, gh), shape_x_offset, g.y_offset);
            let cell_right =
                snapped_cell_x.get(g.lead_col as usize + 1).copied().unwrap_or(cx + cell_w);
            let (gx, gy, gw, gh) = fit_single_cell_status_marker(
                lead_cell.ch,
                cluster_cells,
                is_wide,
                lead_cell.extras().is_some(),
                (gx, gy, gw, gh),
                (cx, cy, cell_right - cx, cell_h),
            );
            // Multi-cell ligature halves keep their natural overhang so paired
            // glyphs such as `=>` continue to fuse across adjacent cells.
            let color = cell_fg(&lead_cell, theme, fg_default);
            let rgba = if info.is_color {
                [1.0, 1.0, 1.0, 1.0]
            } else {
                // When: `!info.is_color` — a monochrome mask, so the ligature
                // takes the cell foreground like ordinary text.
                resolve_fg(g.lead_col, color)
            };
            let (gx, gy, gw, gh) =
                sonicterm_render_model::geometry::snap_to_device_pixels((gx, gy, gw, gh), 1.0);
            trace_white_glyph(lead_cell.ch, rgba, (gx, gy, gw, gh), "ligature");
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
            glyph_instances.push(GlyphInstance {
                rect: px_to_ndc(gx, gy, gw, gh, sw, sh),
                uv: info.uv,
                color: rgba,
                flags: glyph_flags(info.is_color, info.is_subpixel),
            });
        }
    }
}

// Lifecycle: `GpuRenderer` releases its `LIVE_RENDERERS` slot here — the sole
// decrement, paired with the increment in `new_async`.
impl Drop for GpuRenderer {
    // Ordering: `LIVE_RENDERERS.fetch_sub(1, Ordering::AcqRel)`, pairing with
    // the `Ordering::AcqRel` increment in `new_async`. Publishes no payload.
    fn drop(&mut self) {
        // Paired with the increment in `new`. Together they make the live
        // count return to its starting value across balanced open/close
        // churn, and stay above it when a renderer survives.
        LIVE_RENDERERS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Report a glyph about to be drawn in pure white.
///
/// `[1.0, 1.0, 1.0, 1.0]` is emitted on exactly one branch — the colour-glyph
/// path, which deliberately skips the per-cell foreground. Stray pure-white
/// pixels have been reported against a theme whose text is `(190, 183, 150)`,
/// so a glyph carrying this colour is the only draw that could produce them.
///
/// Logging every one makes the question answerable from a capture: if the
/// pixels appear and this fired, the codepoint and rect name the glyph; if
/// they appear and this never fired, no glyph draw is responsible and the
/// whole glyph path is excluded.
fn trace_white_glyph(ch: char, rgba: [f32; 4], rect: (f32, f32, f32, f32), site: &'static str) {
    if rgba != [1.0, 1.0, 1.0, 1.0] {
        // When: `rgba` is not pure white — the glyph cannot be the source of
        // the stray white pixels this probe exists to identify.
        return;
    }
    tracing::warn!(
        target: "sonic::render::glyph",
        ch = ?ch,
        codepoint = format!("U+{:04X}", ch as u32),
        rect = ?rect,
        site,
        "emitting a glyph in pure white"
    );
}

/// Would drawing this glyph sample the atlas outside its own tile?
///
/// The atlas caches an empty or failed rasterization as a sentinel with a
/// zero-area UV, `[0.0, 0.0, 0.0, 0.0]`, and its comment states the renderer
/// skips such a draw. Nothing did. `(0, 0)` is not "nowhere": it is the
/// atlas's top-left texel, which the shelf packer hands to the first glyph of
/// the session, so a zero-area sample reads that glyph's corner ink.
///
/// A monochrome sentinel would at least take the cell's foreground. A block
/// glyph's does not — block tiles carry `is_color: true`, which makes the
/// renderer paint the texture's own colour, so an opaque corner texel arrives
/// as pure white whatever the theme says.
///
/// Returns true when the instance must not be emitted.
#[must_use]
fn glyph_draw_is_degenerate(info: &sonicterm_text::glyph_atlas::GlyphInfo) -> bool {
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
const DIM_BLEND: f32 = 0.45;

fn cell_fg(cell: &Cell, theme: &Theme, default: ChromeColor) -> ChromeColor {
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
fn shaped_glyph_columns_are_monotonic(glyphs: &[sonicterm_text::shape::ShapedGlyph]) -> bool {
    glyphs.windows(2).all(|pair| pair[0].lead_col <= pair[1].lead_col)
}

fn color_to_chrome(color: Color, theme: &Theme, default: ChromeColor) -> ChromeColor {
    match color {
        Color::Default => default,
        Color::Rgb(r, g, b) => ChromeColor::rgb(r, g, b),
        Color::Indexed(i) => indexed(i, theme).unwrap_or(default),
    }
}

#[allow(clippy::too_many_arguments)]
#[derive(Clone, Copy)]
struct InlineImagePlacement<'a> {
    image: &'a sonicterm_render_model::InlineImage,
    origin_x: f32,
    origin_y: f32,
    painter_order: usize,
}

fn emit_inline_image_instances(
    image_atlas: &mut GlyphAtlas,
    out: &mut Vec<GlyphInstance>,
    placements: &[InlineImagePlacement<'_>],
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
) -> usize {
    let mut skipped = 0usize;
    let mut allocation_order: Vec<&InlineImagePlacement<'_>> = placements.iter().collect();
    allocation_order.sort_unstable_by_key(|placement| std::cmp::Reverse(placement.image.id));
    let mut emitted = Vec::with_capacity(placements.len());
    // Prefer the globally newest retained images when the bounded atlas
    // cannot hold the entire history, regardless of which pane owns them.
    for placement in allocation_order {
        let image = placement.image;
        if image.width == 0 || image.height == 0 || image.bgra.is_empty() {
            // When: a dimension is 0 or `bgra` is empty — a failed or pending
            // decode, which has no pixels to pack into the atlas.
            continue;
        }
        let x = placement.origin_x + image.col as f32 * cell_w;
        let y = placement.origin_y + image.row as f32 * cell_h;
        if x >= sw || y >= sh || x + image.width as f32 <= 0.0 || y + image.height as f32 <= 0.0 {
            // When: `x >= sw || y >= sh` or the rect ends at or before the
            // origin — wholly off-surface, so no draw could sample it.
            continue;
        }
        let key = sonicterm_types::glyph_key::GlyphKey {
            ch: '\u{fffc}',
            font_slot: 0xFE,
            weight_bold: false,
            italic: false,
            glyph_id: fold_u64_to_u32(image.id),
            raster_variant: GlyphRasterVariant::Normal,
        };
        let Some(info) =
            image_atlas.get_or_insert_lazy_without_eviction(key, image.width, image.height, || {
                sonicterm_text::glyph_atlas::RasterTile {
                    width: image.width,
                    height: image.height,
                    offset_x: 0,
                    offset_y: 0,
                    advance: image.width as f32,
                    coverage: image.bgra.as_ref().to_vec(),
                    is_color: true,
                    is_subpixel: false,
                }
            })
        else {
            // When: `get_or_insert_lazy_without_eviction` is None — the image
            // does not fit the bounded atlas; older ones are dropped first.
            skipped += 1;
            continue;
        };
        emitted.push((
            placement.painter_order,
            GlyphInstance {
                rect: px_to_ndc(x, y, info.px_size[0] as f32, info.px_size[1] as f32, sw, sh),
                uv: info.uv,
                color: [1.0, 1.0, 1.0, 1.0],
                flags: [1.0, 0.0, 1.0, 0.0],
            },
        ));
    }
    emitted.sort_unstable_by_key(|(painter_order, _)| *painter_order);
    out.extend(emitted.into_iter().map(|(_, instance)| instance));
    skipped
}

fn fold_u64_to_u32(value: u64) -> u32 {
    ((value >> 32) as u32) ^ (value as u32)
}

fn underline_key(cell: &Cell) -> Option<(UnderlineStyle, Color)> {
    (cell.flags.contains(CellFlags::UNDERLINE) && !cell.ch.is_whitespace())
        .then(|| (cell.underline_style(), cell.underline_color().unwrap_or(cell.fg)))
}

#[allow(clippy::too_many_arguments)]
fn push_underline_quads(
    out: &mut Vec<QuadInstance>,
    style: UnderlineStyle,
    x: f32,
    y: f32,
    w: f32,
    cell_h: f32,
    thickness: f32,
    sw: f32,
    sh: f32,
    color: [f32; 4],
) {
    if w <= 0.0 {
        // When: `w <= 0.0` — an empty or inverted span, which would emit a
        // quad with no area.
        return;
    }
    let bottom_y = y + cell_h - thickness;
    match style {
        UnderlineStyle::Single => {
            out.push(QuadInstance::sharp(px_to_ndc(x, bottom_y, w, thickness, sw, sh), color));
        }
        UnderlineStyle::Double => {
            let gap = thickness.max(1.0);
            let y1 = (bottom_y - gap - thickness).max(y);
            out.push(QuadInstance::sharp(px_to_ndc(x, y1, w, thickness, sw, sh), color));
            out.push(QuadInstance::sharp(px_to_ndc(x, bottom_y, w, thickness, sw, sh), color));
        }
        UnderlineStyle::Dotted => {
            let dot = (thickness * 1.6).max(1.0);
            let step = dot * 2.0;
            let mut dx = 0.0;
            while dx < w {
                let size = dot.min(w - dx);
                out.push(QuadInstance::rounded(
                    px_to_ndc(x + dx, bottom_y, size, dot, sw, sh),
                    color,
                    [size, dot],
                    dot * 0.5,
                ));
                dx += step;
            }
        }
        UnderlineStyle::Dashed => {
            let dash = (thickness * 4.0).max(4.0);
            let gap = (thickness * 2.0).max(2.0);
            let mut dx = 0.0;
            while dx < w {
                let len = dash.min(w - dx);
                out.push(QuadInstance::sharp(
                    px_to_ndc(x + dx, bottom_y, len, thickness, sw, sh),
                    color,
                ));
                dx += dash + gap;
            }
        }
        UnderlineStyle::Curly => {
            let amp = (thickness * 1.4).max(1.0);
            let step = (thickness * 4.0).max(4.0);
            let mid_y = y + cell_h - thickness - amp;
            let mut sx = x;
            let mut up = true;
            while sx < x + w {
                let ex = (sx + step).min(x + w);
                // When: up flips once per segment, so each curl stroke starts where the previous ended and the row reads as one wave, not dashes.
                let sy = if up { mid_y + amp } else { mid_y - amp };
                // When: up drives the end point to the opposite side of mid_y, giving this segment the inverse slope of its neighbour.
                let ey = if up { mid_y - amp } else { mid_y + amp };
                push_line_segment_px(out, sx, sy, ex, ey, thickness, sw, sh, color);
                sx = ex;
                up = !up;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_line_segment_px(
    out: &mut Vec<QuadInstance>,
    ax: f32,
    ay: f32,
    bx: f32,
    by: f32,
    thickness: f32,
    sw: f32,
    sh: f32,
    color: [f32; 4],
) {
    let pad = thickness * 0.5 + 1.0;
    let x0 = ax.min(bx) - pad;
    let y0 = ay.min(by) - pad;
    let x1 = ax.max(bx) + pad;
    let y1 = ay.max(by) + pad;
    let w = (x1 - x0).max(1.0);
    let h = (y1 - y0).max(1.0);
    let cx = x0 + w * 0.5;
    let cy = y0 + h * 0.5;
    out.push(QuadInstance::line(
        px_to_ndc(x0, y0, w, h, sw, sh),
        color,
        [w, h],
        [ax - cx, ay - cy],
        [bx - cx, by - cy],
        thickness,
    ));
}

/// Resolve a cell's background to a linear-space `[r,g,b,a]` suitable for the
/// quad pipeline, OR `None` if the cell should fall through to the surface
/// clear color (`theme.colors.background`).
///
/// Returning `None` for the default-bg case lets the per-row emit loop skip
/// pushing a no-op quad over every blank cell — the `LoadOp::Clear(self.bg)`
/// already covers that area.
///
/// Note on color space: the wgpu surface is `Bgra8UnormSrgb`, so the quad
/// fragment shader's output is sRGB-encoded on write. Inputs MUST therefore
/// be in linear-light space, otherwise gamma is applied twice and the result
/// looks washed out (same trap documented in `color.rs::hex_to_premultiplied_rgba`). The
/// sRGB→linear LUT here is bit-exact with the one feeding `hex_to_premultiplied_rgba`, so
/// `Color::Indexed(1)` (ANSI red) ends up identical to the theme's `ansi.red`
/// rendered through the LoadOp clear path.
#[doc(hidden)]
pub fn cell_bg_rgba(cell: &Cell, theme: &Theme) -> Option<[f32; 4]> {
    let color = if cell.flags.contains(CellFlags::INVERSE) {
        let default_fg = hex_to_chrome_color(theme.colors.foreground.0.as_str());
        color_to_chrome(cell.fg, theme, default_fg)
    } else {
        // When: INVERSE is clear, so the cell keeps its own bg and the glyph keeps fg; swapping them here too would cancel out reverse-video runs.
        match cell.bg {
            Color::Default => {
                // When: Color::Default defers to the surface LoadOp::Clear that already covers this cell, so blank regions cost zero quad instances.
                return None;
            }
            bg => color_to_chrome(bg, theme, ChromeColor::rgb(0, 0, 0)),
        }
    };
    let lut = super::color::srgb_u8_to_linear_lut();
    Some([lut[color.r() as usize], lut[color.g() as usize], lut[color.b() as usize], 1.0])
}

/// Walk the visible rows of `grid`, emit one `QuadInstance` per maximal run
/// of horizontally-adjacent cells that share the same non-default background
/// color. Cells whose `bg` resolves to the theme default are skipped — the
/// surface `LoadOp::Clear(theme.background)` already covers them, so emitting
/// a quad there would be wasted bandwidth.
///
/// Run-length coalescing is essential: a single `\033[41m` color-fill of an
/// 80-column row would otherwise produce 80 quads where 1 suffices. The
/// renderer can hit tens of thousands of background cells per frame during
/// e.g. `htop` or `vim` syntax highlighting; per-cell quads would blow the
/// instance buffer and tank fill-rate.
///
/// `WIDE_CONT` cells (the right half of a wide CJK cell) inherit the lead
/// cell's bg via the parser, so they participate in the same run naturally.
///
/// The emitted quads are sharp-edged (no SDF) and pushed onto `out` in row-
/// major order. Caller is responsible for placing this BEFORE selection /
/// cursor / overlay quads in the draw vector so those still paint on top.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)] // mirrors flush_shape_run / collect_hyperlink_runs siblings — all geometry must be threaded in explicitly to keep this a free function (testable without a full GpuRenderer)
pub fn emit_cell_bg_quads(
    grid: &Grid,
    view_top_abs: u64,
    theme: &Theme,
    pad: f32,
    top_inset: f32,
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
    out: &mut Vec<QuadInstance>,
) {
    emit_cell_bg_quads_clipped(
        grid,
        view_top_abs,
        theme,
        PaneRect {
            x: pad,
            y: top_inset,
            w: f32::from(grid.cols) * cell_w,
            h: f32::from(grid.rows) * cell_h,
        },
        cell_w,
        cell_h,
        sw,
        sh,
        out,
    );
}

/// Like [`emit_cell_bg_quads`] but clips runs to a pane sub-rect. This is
/// the production split-pane path: a pane whose grid is wider than its
/// current tile must never emit quads into its neighbour's rectangle.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn emit_cell_bg_quads_clipped(
    grid: &Grid,
    view_top_abs: u64,
    theme: &Theme,
    pane_rect: PaneRect,
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
    out: &mut Vec<QuadInstance>,
) {
    let pad = pane_rect.x;
    let top_inset = pane_rect.y;
    let max_cols = ((pane_rect.w / cell_w).floor() as i32).clamp(0, i32::from(grid.cols)) as u16;
    let max_rows = ((pane_rect.h / cell_h).floor() as i32).clamp(0, i32::from(grid.rows)) as u16;
    if max_cols == 0 || max_rows == 0 {
        // When: max_cols or max_rows floors to zero, the pane tile cannot hold one whole cell; emitting anyway would paint bg into the neighbour pane.
        return;
    }
    // build this pane's snapped-edge cache once. Per the
    // diagnosis, per-pane bg builds its own cache (not the active
    // pane's) so split-pane bg edges stay aligned with that pane's
    // glyph cells. G1a: `build_snapped_cell_x` no longer takes a
    // scale parameter — inputs are raster px already, so snapping
    // is fixed to scale = 1.0 internally.
    let snapped_cell_x = build_snapped_cell_x(pad, cell_w, grid.cols);
    for r in 0..max_rows {
        emit_cell_bg_quads_for_row(
            grid,
            view_top_abs,
            theme,
            pad,
            top_inset,
            cell_w,
            cell_h,
            sw,
            sh,
            max_cols,
            r,
            out,
            &snapped_cell_x,
        );
    }
}

/// shared device-pixel-snapped column-edge cache. Returns
/// `cols + 1` entries where slot `c` is the snapped left edge of cell
/// `c`, and slot `c + span` is its right edge. Every overlay/glyph
/// path that derives a horizontal rect from a column index must read
/// from this cache so adjacent overlays share an exact device-pixel
/// edge with the glyph cells they cover.
///
/// G1a (wezterm-takeover): inputs are raster pixels, so "snapping to
/// device pixels" reduces to integer-pixel rounding — the helper now
/// passes scale = 1.0 to [`snap_to_device_pixels`] (raster px IS the
/// device-pixel grid) instead of threading the renderer's DPI scale
/// through the call. Behaviour at integer DPIs is identical to the
/// pre-G1a `cell_w * scale` arithmetic; at fractional DPIs the new
/// path matches what the renderer actually paints (a single integer
/// raster-pixel-aligned grid) rather than the prior logical-px
/// half-pixel cache.
#[doc(hidden)]
#[must_use]
pub fn build_snapped_cell_x(origin_x: f32, cell_w: f32, cols: u16) -> Vec<f32> {
    (0..=cols)
        .map(|col| {
            sonicterm_render_model::geometry::snap_to_device_pixels(
                (origin_x + (col as f32) * cell_w, 0.0, 0.0, 0.0),
                1.0,
            )
            .0
        })
        .collect()
}

/// Pure column-from-pixel lookup that mirrors the renderer's
/// device-pixel-snapped edge cache. `edges` is the output of
/// `build_snapped_cell_x` for the pane in question (length `cols + 1`).
/// Returns `Some(col)` for any `px` in `[edges[0], edges[cols])` using
/// half-open buckets `edges[col] <= px < edges[col+1]` — boundary px
/// resolve to the RHS cell, matching the renderer's draw bias.
///
/// Returns `None` if `px` is left of `edges[0]` or `>= edges[cols]`
/// (caller already gated negatives via the pane resolution step, but
/// this is defensive). Returns `None` if `edges` is malformed
/// (`len < 2`) — that only happens for a 0-col pane, which has no
/// addressable cell to begin with.
#[doc(hidden)]
#[must_use]
pub fn pixel_to_local_col(px: f32, edges: &[f32], cols: u16) -> Option<u16> {
    if cols == 0 || edges.len() < 2 {
        // When: cols is zero or edges is malformed (len < 2), the pane has no addressable cell, so no pixel can resolve to a column.
        return None;
    }
    if px < edges[0] {
        // When: px sits left of edges[0], it lands in the window padding rather than the grid, so no column owns it.
        return None;
    }
    if px >= edges[cols as usize] {
        // When: px reaches edges[cols], it is past the grid's right edge in trailing padding; the half-open buckets end at that exact value.
        return None;
    }
    // Linear scan: half-open buckets edges[i] <= px < edges[i+1].
    // Cell counts are bounded (<= a few hundred) so a scan beats the
    // branch overhead of binary search at typical widths. For very wide
    // grids (cols >> 200) this could switch to `partition_point` — the
    // input is monotone non-decreasing by construction.
    for i in 0..cols as usize {
        if px < edges[i + 1] {
            // When: px falls under edges[i + 1], bucket i contains it; a boundary px resolves to the right-hand cell, matching the renderer's draw bias.
            return Some(i as u16);
        }
    }
    // Unreachable given the `>= edges[cols]` guard above, but keep the
    // total function obvious.
    None
}

/// Emit background quads for a single visible row. Extracted so the
/// `LineQuadCache` miss path (P2) can call it for one row
/// at a time and capture the resulting quads into the cache.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn emit_cell_bg_quads_for_row(
    grid: &Grid,
    view_top_abs: u64,
    theme: &Theme,
    pad: f32,
    top_inset: f32,
    cell_w: f32,
    cell_h: f32,
    sw: f32,
    sh: f32,
    max_cols: u16,
    r: u16,
    out: &mut Vec<QuadInstance>,
    snapped_cell_x: &[f32],
) {
    {
        let row_abs = view_top_abs + r as u64;
        let Some(row) = grid.row_at_abs(row_abs) else {
            // When: row_at_abs finds no row for row_abs, that absolute line has aged out of scrollback, so this viewport row has no cells to shade.
            return;
        };
        // Run-length encode adjacent same-bg cells into one quad.
        let mut run_start: Option<u16> = None;
        let mut run_color: Option<[f32; 4]> = None;
        let mut col: u16 = 0;
        // derive x/w from the shared snapped-edge cache so bg
        // runs share device-pixel edges with adjacent glyph cells at
        // fractional DPI. Falls back to raw arithmetic if the cache is
        // empty (defensive — production always passes the full cache).
        let raw_fallback = snapped_cell_x.is_empty();
        let flush =
            |start: u16, end_exclusive: u16, color: [f32; 4], out: &mut Vec<QuadInstance>| {
                let clipped_end = end_exclusive.min(max_cols);
                if clipped_end <= start {
                    // When: max_cols pulls clipped_end back to or before start, the run lies outside the pane tile and would push a zero-width quad.
                    return;
                }
                let (x, w) = if raw_fallback {
                    (pad + f32::from(start) * cell_w, f32::from(clipped_end - start) * cell_w)
                } else {
                    // When: raw_fallback is off, the run takes its edges from the shared snapped cache so bg meets the glyph cells exactly at fractional DPI.
                    let lo = snapped_cell_x[start as usize];
                    let hi = snapped_cell_x[clipped_end as usize];
                    (lo, hi - lo)
                };
                let y = top_inset + f32::from(r) * cell_h;
                out.push(QuadInstance::sharp(px_to_ndc(x, y, w, cell_h, sw, sh), color));
            };
        for cell in row.iter().take(max_cols as usize) {
            let bg = cell_bg_rgba(cell, theme);
            match (run_color, bg) {
                (Some(prev), Some(cur)) if prev == cur => {
                    // When: cur repeats prev, so the cell joins the open run and an 80-column fill stays one quad instead of eighty.
                    // extend run
                }
                (Some(prev), _) => {
                    // PANIC: safe — `run_color` and `run_start` are written
                    // together (search this fn for `run_start = ` to see they
                    // are always assigned in the same statement-pair). Matching
                    // `run_color == Some(_)` therefore proves `run_start ==
                    // Some(_)`. Hot per-frame path: no Result conversion.
                    let start = run_start.expect("run_start set when run_color is");
                    flush(start, col, prev, out);
                    run_start = bg.map(|_| col);
                    run_color = bg;
                }
                (None, Some(_)) => {
                    run_start = Some(col);
                    run_color = bg;
                }
                (None, None) => {
                    // When: neither run_color nor bg is set, the cell is default-bg with no run open; LoadOp::Clear already covers it.
                }
            }
            col = col.saturating_add(1);
        }
        if let (Some(start), Some(color)) = (run_start, run_color) {
            flush(start, col, color, out);
        }
    }
}

fn indexed(i: u8, theme: &Theme) -> Option<ChromeColor> {
    let p = &theme.colors;
    let pick = |h: &str| hex_to_chrome_color(h);
    match i {
        0 => Some(pick(p.ansi.black.0.as_str())),
        1 => Some(pick(p.ansi.red.0.as_str())),
        2 => Some(pick(p.ansi.green.0.as_str())),
        3 => Some(pick(p.ansi.yellow.0.as_str())),
        4 => Some(pick(p.ansi.blue.0.as_str())),
        5 => Some(pick(p.ansi.magenta.0.as_str())),
        6 => Some(pick(p.ansi.cyan.0.as_str())),
        7 => Some(pick(p.ansi.white.0.as_str())),
        8 => Some(pick(p.bright.black.0.as_str())),
        9 => Some(pick(p.bright.red.0.as_str())),
        10 => Some(pick(p.bright.green.0.as_str())),
        11 => Some(pick(p.bright.yellow.0.as_str())),
        12 => Some(pick(p.bright.blue.0.as_str())),
        13 => Some(pick(p.bright.magenta.0.as_str())),
        14 => Some(pick(p.bright.cyan.0.as_str())),
        15 => Some(pick(p.bright.white.0.as_str())),
        16..=231 => {
            let v = i - 16;
            let r = v / 36;
            let g = (v / 6) % 6;
            let b = v % 6;
            // When: c is nonzero, the xterm 6x6x6 cube spaces levels at 55 + 40c rather than evenly, so 256-color ramps match other terminals.
            let to8bit = |c: u8| if c == 0 { 0 } else { c * 40 + 55 };
            Some(ChromeColor::rgb(to8bit(r), to8bit(g), to8bit(b)))
        }
        232..=255 => {
            let g = (i - 232) * 10 + 8;
            Some(ChromeColor::rgb(g, g, g))
        }
    }
}

#[cfg(test)]
#[path = "core_tests.rs"]
mod core_tests;
// `hex_to_glyphon` and
// `scale_glyphon_alpha` have moved into `crate::color` under the
// renamed `hex_to_chrome_color` / `scale_chrome_text_alpha` names and
// now consume `ChromeColor` instead of `legacy chrome color`.
// Re-export them at the legacy path so callers that imported
// `sonicterm_gpu::core::scale_glyphon_alpha` can switch to the new
// identifier (see `crates/sonicterm-app/tests/drag_visual_feedback.rs`
// for the port). The legacy names are gone from this file entirely;
// any caller that lingers on them will fail to compile (intentional —
// it's the must-pass #4 grep gate's job to catch survivors).
pub use crate::color::scale_chrome_text_alpha;

// `terminal_font_attrs` re-export removed. It returned
// `legacy chrome attrs` which carried per-span family/weight; the
// chrome-text path replaces it with `ChromeAttrs { bold, italic }`
// constructed per-span at the call site. Downstream callers
// (`sonicterm-ui::tab_spans`) build `(text, ChromeColor, ChromeAttrs)`
// span tuples directly. The grid/chrome shape calls reach the loaded
// wezterm font via `FontStack::default_font()` — there is no per-span
// font attribute layer in this path.

/// Walk the grid and collect runs of contiguous cells that share a hyperlink
/// id, per row. Wide-cell continuations don't break a run (they inherit the
/// lead cell's hyperlink). Returns `(row, col_start, col_end_inclusive)`.
#[doc(hidden)]
pub fn collect_hyperlink_runs(grid: &Grid) -> Vec<(u16, u16, u16)> {
    let mut runs = Vec::new();
    for r in 0..grid.rows {
        let row = grid.row(r);
        let mut start: Option<u16> = None;
        let mut current: Option<sonicterm_render_model::boundary::grid::hyperlink::HyperlinkId> =
            None;
        let mut last_col: u16 = 0;
        for (col, cell) in row.iter().enumerate() {
            if cell.flags.contains(CellFlags::WIDE_CONT) {
                // When: WIDE_CONT marks the trailing half of a wide cell, which inherits the lead cell's hyperlink, so it extends the run instead of breaking it.
                if start.is_some() {
                    last_col = col as u16;
                }
                continue;
            }
            match (cell.hyperlink(), current) {
                (Some(hid), Some(cur)) if hid == cur => {
                    last_col = col as u16;
                }
                (Some(hid), _) => {
                    if let (Some(s), Some(_)) = (start, current) {
                        runs.push((r, s, last_col));
                    }
                    start = Some(col as u16);
                    current = Some(hid);
                    last_col = col as u16;
                }
                (None, Some(_)) => {
                    if let Some(s) = start.take() {
                        runs.push((r, s, last_col));
                    }
                    current = None;
                }
                (None, None) => {
                    // When: the cell carries no hyperlink and current is unset, there is no run to open or close, so the walk just advances.
                }
            }
        }
        if let (Some(s), Some(_)) = (start, current) {
            runs.push((r, s, last_col));
        }
    }
    runs
}

// `load_bundled_fonts` (cosmic-text bundle loader) is gone.
// Bundled fonts ship via sonicterm-font's `vendor-jetbrains`,
// `vendor-noto-emoji`, `vendor-nerd-font-symbols` features (see
// `sonicterm-text/Cargo.toml`), so the FontStack discovers them
// automatically without an explicit per-file disk load.

/// Stable fingerprint for command badges, including wall-clock buckets that
/// change when badge visibility can transition without a tab model mutation.
#[doc(hidden)]
pub fn command_status_hash(
    status: &sonicterm_render_model::boundary::ui::tabs::CommandStatus,
    now: Instant,
) -> u64 {
    match status {
        sonicterm_render_model::boundary::ui::tabs::CommandStatus::Idle => 0,
        sonicterm_render_model::boundary::ui::tabs::CommandStatus::Running(started_at) => {
            let elapsed_secs = now.duration_since(*started_at).as_secs().min(5);
            let badge_visible = u64::from(now.duration_since(*started_at).as_secs() > 5);
            1 | (elapsed_secs << 32) | (badge_visible << 40)
        }
        sonicterm_render_model::boundary::ui::tabs::CommandStatus::Done { exit, until } => {
            let is_past_expiry = u64::from(now >= *until);
            2 | (u64::from(exit.unwrap_or(255)) << 8) | (is_past_expiry << 32)
        }
    }
}

/// Compute the per-row selection quad rects (in physical pixels) that the
/// renderer would emit for `sel` against a grid of `rows` × `cols`, anchored
/// at `(origin_x, origin_y)` with `cell_w × cell_h` cells.
///
/// Pure helper, no clipping applied — pair with [`clip_rect_to_pane`] before
/// pushing to the GPU. Exposed so integration tests can verify the
/// pre-clip / post-clip relationship without standing up a real surface.
///
/// Each returned tuple is `(x, y, w, h)` in physical pixels.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub fn selection_quad_rects(
    sel: &sonicterm_render_model::boundary::ui::selection::Selection,
    view_top_abs: u64,
    rows: u16,
    cols: u16,
    origin_x: f32,
    origin_y: f32,
    cell_w: f32,
    cell_h: f32,
    snapped_cell_x: &[f32],
) -> Vec<(f32, f32, f32, f32)> {
    if sel.is_empty() {
        // When: sel covers no cells, returning an empty vec drops the previous frame's highlight rather than leaving a stale rect on screen.
        return Vec::new();
    }
    let (a, b) = sel.normalized();
    let mut out = Vec::with_capacity(usize::from(rows));
    // derive each row's x/w from the shared snapped-edge cache so
    // selection rects share device-pixel edges with adjacent glyph
    // cells at fractional DPI. Empty-cache fallback preserves the old
    // raw-arithmetic behavior for callers (debug/test helpers) that
    // don't carry a real cache; integer scales make the two identical.
    let raw_fallback = snapped_cell_x.is_empty();
    // Selection rows are scrollback-ABSOLUTE. Only the absolute rows that
    // intersect the viewport produce quads, so bound the walk to
    // `[max(a.0, view_top_abs) ..= min(b.0, view_top_abs + rows - 1)]`.
    // This keeps per-frame cost O(viewport rows) even when the selection
    // spans a huge multi-screen region of scrollback. The first/last-row
    // column tests still compare against the true `a.0`/`b.0` (which may
    // sit off-screen), so partial first/last rows render correctly.
    if rows == 0 {
        // When: rows is zero the viewport has no line to highlight, and `rows as u64 - 1` below would underflow into a bogus bottom bound.
        return out;
    }
    let view_bottom_abs = view_top_abs + (rows as u64 - 1);
    let first_abs = a.0.max(view_top_abs);
    let last_abs = b.0.min(view_bottom_abs);
    if first_abs > last_abs {
        // When: first_abs passes last_abs, no absolute selection row intersects the viewport, so a selection deep in scrollback costs no per-row walk.
        return out; // selection entirely above or below the viewport
    }
    for abs_r in first_abs..=last_abs {
        let vr = (abs_r - view_top_abs) as u16;
        // When: abs_r is past a.0 the row starts mid-selection, so col_a falls back to 0 and the highlight spans from the left edge.
        let col_a = if abs_r == a.0 { a.1 } else { 0 };
        // Note: do NOT clamp `col_b` to `cols - 1` here. The selection may
        // legitimately reach the grid's last column, and the per-pane clip
        // below trims any pixel overhang. Clamping pre-clip would silently
        // shrink the selection on the last row when the user dragged past
        // the rightmost cell — which is precisely the path that hides
        // bugs like the split-pane bleed-through.

        // When: abs_r sits before b.0 the row ends mid-selection, so col_b runs to the last column and the highlight reads as continuous.
        let col_b = if abs_r == b.0 { b.1 } else { cols.saturating_sub(1) };
        if col_b < col_a {
            // When: col_b lands left of col_a the row holds no selected span, and `end_exclusive - col_a` would wrap on u16.
            continue;
        }
        let end_exclusive = col_b.saturating_add(1);
        let (x, w) = if raw_fallback {
            (origin_x + f32::from(col_a) * cell_w, f32::from(end_exclusive - col_a) * cell_w)
        } else {
            // When: raw_fallback is off, the rect takes its edges from the shared snapped cache so selection meets the glyph cells exactly at fractional DPI.

            // Clamp the right edge to the cache bounds (`cols + 1`); a
            // selection that touches col `cols - 1` reads `snapped[cols]`.
            let cache_end = end_exclusive.min((snapped_cell_x.len() - 1) as u16);
            if cache_end <= col_a {
                // When: the cache clamp pulls cache_end back to or before col_a, the row's span falls outside the cached edges and would be zero-width.
                continue;
            }
            let lo = snapped_cell_x[col_a as usize];
            let hi = snapped_cell_x[cache_end as usize];
            (lo, hi - lo)
        };
        let y = origin_y + f32::from(vr) * cell_h;
        out.push((x, y, w, cell_h));
    }
    out
}

/// Clip a quad rect (in physical pixels) to the active pane's bounding box.
/// Returns `None` if the rect is entirely outside the pane.
///
/// Selection / cursor / overlay quads are anchored to the active pane's
/// origin and can extend past its right or bottom edge when the user drags
/// beyond the pane (or the cursor temporarily sits outside the grid bounds
/// due to a resize race). Pushing the unclipped quad would paint into the
/// neighbouring pane in a split layout — see the regression test for
/// the split-right drag-select bug.
#[doc(hidden)]
pub fn clip_rect_to_pane(
    rect: (f32, f32, f32, f32),
    pane_x: f32,
    pane_y: f32,
    pane_w: f32,
    pane_h: f32,
) -> Option<(f32, f32, f32, f32)> {
    let (x, y, w, h) = rect;
    let clipped_x = x.max(pane_x);
    let clipped_right = (x + w).min(pane_x + pane_w);
    let clipped_y = y.max(pane_y);
    let clipped_bottom = (y + h).min(pane_y + pane_h);
    let clipped_w = clipped_right - clipped_x;
    let clipped_h = clipped_bottom - clipped_y;
    if clipped_w > 0.0 && clipped_h > 0.0 {
        Some((clipped_x, clipped_y, clipped_w, clipped_h))
    } else {
        // When: clipped_w or clipped_h collapses to zero, the rect lies wholly outside the pane; returning nothing keeps it out of the neighbour's tile.
        None
    }
}
