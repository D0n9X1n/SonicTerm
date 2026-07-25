//! Wezterm-font powered font stack.
//!
//! Phase 3 entry point. Wraps `wezterm_font::FontConfiguration` so
//! `sonicterm-text` can query wezterm's font selection / fallback /
//! shaping algorithms instead of cosmic-text. The first concrete
//! consumer is the per-row shape cache; over time more sonicterm-text
//! helpers will route through this stack.
//!
//! The stack is intentionally minimal in this phase: we expose only
//! the methods the sonicterm-text shape cache needs to call. Adding
//! more sonicterm-font surface area as needed is a one-liner here.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Once;

use anyhow::Result;
use config::TextStyle;
use sonicterm_font::{
    rasterizer::{checked_glyph_rgba_len, MAX_RASTERIZED_GLYPH_DIMENSION},
    Direction, FontConfiguration, Presentation,
};
use sonicterm_text::glyph_atlas::{RasterTile, Rasterizer};
use sonicterm_types::glyph_key::GlyphKey;

/// Default primary font family. Matches `sonicterm_cfg::DEFAULT_FONT_FAMILY`
/// — the brand default the project ships with. Duplicated here (rather
/// than imported) because `sonicterm-engine` deliberately does not depend
/// on `sonicterm-cfg`; if a caller needs to override the family it should
/// invoke [`FontStack::try_new_with_family`].
pub const DEFAULT_FONT_FAMILY: &str = "Rec Mono St.Helens";

/// Synthesized fallback chain appended after the user's primary family.
/// Order matters: JetBrains Mono first (bundled by sonicterm-font itself,
/// always resolvable), then Symbols Nerd Font Mono for Powerline / Nerd
/// Font PUA glyphs the primary may lack, then Noto Color Emoji as the
/// last-resort color fallback.
const FALLBACK_FAMILIES: &[&str] =
    &["JetBrains Mono", "Symbols Nerd Font Mono", "Noto Color Emoji"];

/// Global `use_this_configuration` install guard. The wezterm `config`
/// crate keeps a process-wide `Configuration` slot read by
/// `FontConfiguration::new(None, ..)`; we install exactly one Config
/// derived from sonicterm preferences on the first FontStack
/// construction. Subsequent calls re-use it.
static INSTALL_ONCE: Once = Once::new();

/// Cell metrics in raster pixels, sourced from the active font stack.
///
/// G1a (wezterm-takeover): the renderer's coordinate system is raster
/// pixels end-to-end. [`FontStack::cell_metrics_raster_px`] emits this
/// renderer-friendly view without any `* scale_factor` math.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CellMetricsPx {
    /// Width of a single character cell, raster px.
    pub cell_w: f64,
    /// Height of a single character cell, raster px.
    pub cell_h: f64,
    /// Underline / strikethrough thickness, raster px.
    pub underline_h: f64,
    /// Descender (added to bottom y to find baseline; typically
    /// negative), raster px.
    pub descender: f64,
}

/// Holds a single wezterm `FontConfiguration` keyed to a logical DPI
/// + scale. Multiple sonicterm panes share one stack — sonicterm-font
///   itself caches per-font face state internally.
#[derive(Clone)]
pub struct FontStack {
    fc: Rc<FontConfiguration>,
    regular_weight_scale: f32,
    /// Memoized cell height in raster px, used to size outline growth.
    /// `0.0` means "not yet computed"; invalidated on scaling changes.
    cell_h_px: Cell<f64>,
}

impl FontStack {
    /// Construct a [`FontStack`] using the project's default font
    /// family ([`DEFAULT_FONT_FAMILY`] — "Rec Mono St.Helens") backed by
    /// the synthesized [`FALLBACK_FAMILIES`] chain. On first call this
    /// installs a process-wide wezterm `Config` so that
    /// `FontConfiguration::new(None, dpi)` selects sonicterm's primary
    /// family instead of sonicterm-font's bundled JetBrains Mono default.
    pub fn try_new(dpi: usize) -> Result<Self> {
        Self::try_new_full(DEFAULT_FONT_FAMILY, 14.0, dpi)
    }

    /// Construct a [`FontStack`] with `primary_family` as the baseline
    /// `[font] family` setting. The first call to this function (or
    /// [`Self::try_new`]) installs a wezterm `Config` globally; later
    /// calls re-use that install regardless of their `primary_family`
    /// argument (live family swaps are out-of-scope for this phase —
    /// see the wezterm-takeover spec § "Default font config").
    pub fn try_new_with_family(primary_family: &str, dpi: usize) -> Result<Self> {
        Self::try_new_full(primary_family, 14.0, dpi)
    }

    /// Construct a [`FontStack`] with explicit primary family, point size, and
    /// DPI using native regular-text coverage.
    pub fn try_new_full(primary_family: &str, font_size_pt: f64, dpi: usize) -> Result<Self> {
        Self::try_new_full_with_weight(primary_family, font_size_pt, dpi, 1.0)
    }

    /// Construct a [`FontStack`] with explicit primary family, point size, DPI,
    /// and regular-text coverage scale. Pass `dpi = 72 * scale_factor` so
    /// sonicterm-font's point-size conversion yields raster pixels.
    pub fn try_new_full_with_weight(
        primary_family: &str,
        font_size_pt: f64,
        dpi: usize,
        regular_weight_scale: f32,
    ) -> Result<Self> {
        install_default_config(primary_family, font_size_pt);
        let fc = FontConfiguration::new(
            Some(build_config(primary_family, font_size_pt, FALLBACK_FAMILIES)),
            dpi,
        )?;
        Ok(Self {
            fc: Rc::new(fc),
            regular_weight_scale: sanitize_weight_scale(regular_weight_scale),
            cell_h_px: Cell::new(0.0),
        })
    }

    pub fn change_scaling(&self, font_scale: f64, dpi: usize) -> (f64, usize) {
        // Cell height is derived from the rasterizer scale, so the memoized
        // value cannot survive a scaling change.
        self.cell_h_px.set(0.0);
        self.fc.change_scaling(font_scale, dpi)
    }

    /// Current logical font scale (independent of DPI). Callers changing
    /// only the rasterizer DPI on a scale-factor move should reuse this so
    /// the user's font-scale preference is preserved across the change.
    pub fn get_font_scale(&self) -> f64 {
        self.fc.get_font_scale()
    }

    /// Shape a regular text run using SonicTerm's current font stack policy.
    pub fn shape_text(&self, text: &str) -> Result<Vec<sonicterm_font::shaper::GlyphInfo>> {
        self.shape_text_with_style(text, false, false)
    }

    /// Shape a text run using the face selected for its bold/italic style.
    pub fn shape_text_with_style(
        &self,
        text: &str,
        bold: bool,
        italic: bool,
    ) -> Result<Vec<sonicterm_font::shaper::GlyphInfo>> {
        let font = self.font_for_style(bold, italic)?;
        font.blocking_shape(text, Some(Presentation::Text), Direction::LeftToRight, None, None)
    }

    fn font_for_style(&self, bold: bool, italic: bool) -> Result<Rc<sonicterm_font::LoadedFont>> {
        let mut style: TextStyle = self.fc.config().font.clone();
        if bold {
            style = style.make_bold();
        }
        if italic {
            style = style.make_italic();
        }
        self.fc.resolve_font(&style)
    }

    /// Measure a left-to-right text run in raster pixels using the same
    /// fallback-font shaping policy as the renderer.
    pub fn measure_text_width(&self, text: &str) -> Result<f32> {
        let glyphs = self.shape_text(text)?;
        Ok(glyphs.iter().map(|glyph| glyph.x_advance.get() as f32).sum())
    }

    /// Return cell metrics for the default font, projected into the
    /// renderer-facing [`CellMetricsPx`] (raster px). G1a: wezterm's
    /// `FontMetrics` already lives in raster px, so this is a plain
    /// field extraction — no `* scale_factor` multiplier here, and
    /// none at the call site.
    ///
    /// Errors when sonicterm-font fails to load the default font (e.g.
    /// no installed fallback covers the configured family). Callers
    /// in the hot path should propagate; tests can `unwrap` once
    /// they've confirmed sonicterm-font picked something up.
    pub fn cell_metrics_raster_px(&self) -> Result<CellMetricsPx> {
        let m = self.fc.default_font_metrics()?;
        Ok(CellMetricsPx {
            cell_w: m.cell_width.get(),
            cell_h: m.cell_height.get(),
            underline_h: m.underline_thickness.get(),
            descender: m.descender.get(),
        })
    }
}

impl Rasterizer for FontStack {
    fn rasterize(&mut self, key: GlyphKey) -> Option<RasterTile> {
        let font = self.font_for_style(key.weight_bold, key.italic).ok()?;
        let (font_idx, glyph_pos) = if key.glyph_id != 0 {
            (key.font_slot as usize, key.glyph_id)
        } else {
            let s = key.ch.to_string();
            let infos = font
                .blocking_shape(&s, Some(Presentation::Text), Direction::LeftToRight, None, None)
                .ok()?;
            let first = infos.into_iter().find(|g| g.glyph_pos != 0)?;
            (first.font_idx, first.glyph_pos)
        };

        let rg = font.rasterize_glyph(glyph_pos, font_idx).ok()?;
        if rg.data.is_empty() || rg.width == 0 || rg.height == 0 {
            return None;
        }
        let expected_len = checked_glyph_rgba_len(rg.width, rg.height).ok()?;
        if rg.data.len() != expected_len {
            log::warn!(
                "font rasterizer returned invalid {}x{} glyph buffer: {} bytes, expected {}",
                rg.width,
                rg.height,
                rg.data.len(),
                expected_len
            );
            return None;
        }
        let (mut coverage, is_color, is_subpixel) = if rg.has_color {
            let mut bgra = Vec::with_capacity(rg.data.len());
            for px in rg.data.chunks_exact(4) {
                bgra.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
            (bgra, true, false)
        } else {
            let has_subpixel_coverage =
                rg.data.chunks_exact(4).any(|px| px[0] != px[1] || px[1] != px[2]);
            if has_subpixel_coverage {
                let mut bgra = Vec::with_capacity(rg.data.len());
                for px in rg.data.chunks_exact(4) {
                    bgra.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                }
                (bgra, false, true)
            } else {
                let mask: Vec<u8> = rg.data.chunks_exact(4).map(|p| p[3]).collect();
                (mask, false, false)
            }
        };
        let mut tile_w = rg.width;
        let mut tile_h = rg.height;
        let mut offset_x = rg.bearing_x.get() as i32;
        let mut offset_y = -rg.bearing_y.get() as i32;
        if !is_color && !key.weight_bold {
            apply_regular_weight_scale(&mut coverage, self.regular_weight_scale, is_subpixel);
            // The coverage remap alone cannot thicken a stem whose core is
            // already fully opaque, which is the common case at HiDPI. Growing
            // the outline is what makes weight_scale visible there.
            let radius = embolden_radius_px(self.regular_weight_scale, self.cell_h_px());
            if let Some((grown, w, h, pad)) =
                embolden_coverage(&coverage, tile_w, tile_h, radius, is_subpixel)
            {
                coverage = grown;
                tile_w = w;
                tile_h = h;
                // The tile grew by `pad` on every side, so its top-left corner
                // now sits that much further up and to the left of the pen.
                offset_x -= pad as i32;
                offset_y -= pad as i32;
            }
        }
        Some(RasterTile {
            width: tile_w as u32,
            height: tile_h as u32,
            offset_x,
            offset_y,
            // Advance stays keyed to the original bitmap. Emboldening adds ink
            // around the glyph but must not shift the cell grid.
            advance: rg.width as f32,
            coverage,
            is_color,
            is_subpixel,
        })
    }
}

impl FontStack {
    /// Cell height in raster px, memoized. Returns `0.0` when metrics cannot
    /// be resolved, which disables outline growth rather than guessing a size.
    fn cell_h_px(&self) -> f64 {
        let cached = self.cell_h_px.get();
        if cached > 0.0 {
            return cached;
        }
        let resolved = self.cell_metrics_raster_px().map(|m| m.cell_h).unwrap_or(0.0);
        self.cell_h_px.set(resolved);
        resolved
    }
}

fn sanitize_weight_scale(scale: f32) -> f32 {
    if scale.is_finite() && (0.5..=5.0).contains(&scale) {
        scale
    } else {
        1.0
    }
}

/// Glyph outline growth per unit of `weight_scale` above 1.0, expressed as a
/// fraction of the cell height. Tuned so `weight_scale = 2.0` adds roughly
/// half a pixel of radius at a 13pt Retina cell and `5.0` stays under the
/// point where adjacent stems merge into a blob.
const EMBOLDEN_RADIUS_PER_CELL_H: f64 = 0.02;

/// Radius, in raster px, that regular text should grow at `scale`. Zero for
/// `scale <= 1.0` — thinning is handled by the coverage remap, which can
/// lighten partial pixels but cannot erode a solid stem.
fn embolden_radius_px(scale: f32, cell_h: f64) -> f64 {
    if scale <= 1.0 || !cell_h.is_finite() || cell_h <= 0.0 {
        return 0.0;
    }
    f64::from(scale - 1.0) * cell_h * EMBOLDEN_RADIUS_PER_CELL_H
}

/// One separable max-filter pass with fractional radius. `radius` is split
/// into an integer core, taken at full strength, and a fractional outer ring
/// that is blended in proportionally so growth is smooth rather than snapping
/// a whole pixel at a time.
fn dilate_axis(src: &[u8], dst: &mut [u8], len: usize, count: usize, stride: usize, radius: f64) {
    let whole = radius.floor() as usize;
    let frac = radius - radius.floor();
    for line in 0..count {
        let base = line * stride;
        for i in 0..len {
            let mut best = 0u8;
            let lo = i.saturating_sub(whole);
            let hi = (i + whole).min(len - 1);
            for j in lo..=hi {
                best = best.max(src[base + j]);
            }
            if frac > 0.0 {
                let mut ring = 0u8;
                if i > whole {
                    ring = ring.max(src[base + i - whole - 1]);
                }
                if i + whole + 1 < len {
                    ring = ring.max(src[base + i + whole + 1]);
                }
                let blended = f64::from(ring) * frac;
                best = best.max(blended.round().clamp(0.0, 255.0) as u8);
            }
            dst[base + i] = best;
        }
    }
}

/// Grow `coverage` outward by `radius` px, returning the padded buffer and its
/// new dimensions. The glyph is padded on every side first so the added ink has
/// somewhere to land instead of being clipped at the old bitmap edge.
///
/// Returns `None` when there is nothing to do or the padded tile would exceed
/// the atlas dimension limit.
fn embolden_coverage(
    coverage: &[u8],
    width: usize,
    height: usize,
    radius: f64,
    is_subpixel: bool,
) -> Option<(Vec<u8>, usize, usize, usize)> {
    if radius <= 0.0 || width == 0 || height == 0 {
        return None;
    }
    let pad = radius.ceil() as usize;
    let new_w = width + pad * 2;
    let new_h = height + pad * 2;
    if new_w > MAX_RASTERIZED_GLYPH_DIMENSION || new_h > MAX_RASTERIZED_GLYPH_DIMENSION {
        return None;
    }
    let channels = if is_subpixel { 4 } else { 1 };
    let mut padded = vec![0u8; new_w * new_h * channels];
    for y in 0..height {
        let src = y * width * channels;
        let dst = ((y + pad) * new_w + pad) * channels;
        padded[dst..dst + width * channels]
            .copy_from_slice(&coverage[src..src + width * channels]);
    }

    // Dilate each channel independently. Interleaved BGRA is handled by
    // deinterleaving into a scratch plane, since the separable passes need a
    // contiguous stride per axis. Every byte is written below, so the buffer
    // starts zeroed rather than copied.
    let mut out = vec![0u8; new_w * new_h * channels];
    for ch in 0..channels {
        let mut plane = vec![0u8; new_w * new_h];
        for i in 0..new_w * new_h {
            plane[i] = padded[i * channels + ch];
        }
        let mut tmp = vec![0u8; new_w * new_h];
        // Horizontal: new_h lines of new_w samples, stride new_w.
        dilate_axis(&plane, &mut tmp, new_w, new_h, new_w, radius);
        // Vertical: transpose, reuse the same row-wise pass, transpose back.
        let mut transposed = vec![0u8; new_w * new_h];
        for y in 0..new_h {
            for x in 0..new_w {
                transposed[x * new_h + y] = tmp[y * new_w + x];
            }
        }
        let mut tcol = vec![0u8; new_w * new_h];
        dilate_axis(&transposed, &mut tcol, new_h, new_w, new_h, radius);
        for y in 0..new_h {
            for x in 0..new_w {
                out[(y * new_w + x) * channels + ch] = tcol[x * new_h + y];
            }
        }
    }

    if is_subpixel {
        // Alpha is the envelope of the dilated RGB coverage.
        for px in out.chunks_exact_mut(4) {
            px[3] = px[0].max(px[1]).max(px[2]);
        }
    }
    Some((out, new_w, new_h, pad))
}

fn scale_coverage(coverage: u8, scale: f32) -> u8 {
    if coverage == 0 || coverage == u8::MAX || (scale - 1.0).abs() < f32::EPSILON {
        return coverage;
    }
    let normalized = f32::from(coverage) / 255.0;
    let exponent = 1.0 / scale;
    (normalized.powf(exponent) * 255.0).round().clamp(0.0, 255.0) as u8
}

fn apply_regular_weight_scale(coverage: &mut [u8], scale: f32, is_subpixel: bool) {
    let scale = sanitize_weight_scale(scale);
    if (scale - 1.0).abs() < f32::EPSILON {
        return;
    }
    if is_subpixel {
        for pixel in coverage.chunks_exact_mut(4) {
            pixel[0] = scale_coverage(pixel[0], scale);
            pixel[1] = scale_coverage(pixel[1], scale);
            pixel[2] = scale_coverage(pixel[2], scale);
            pixel[3] = pixel[0].max(pixel[1]).max(pixel[2]);
        }
    } else {
        for value in coverage {
            *value = scale_coverage(*value, scale);
        }
    }
}

/// Install the sonicterm-derived wezterm `Config` into the process-wide
/// `Configuration` slot exactly once. Idempotent — subsequent invocations
/// (even with a different `primary_family`) are no-ops; reconfiguring the
/// font at runtime is tracked separately and would need a `change_scaling`
/// / `config_changed` round-trip through every live `FontConfiguration`.
fn install_default_config(primary_family: &str, font_size_pt: f64) {
    INSTALL_ONCE.call_once(|| {
        sonicterm_font::use_sonic_font_configuration(
            primary_family,
            font_size_pt,
            FALLBACK_FAMILIES,
        );
    });
}

fn build_config(
    primary_family: &str,
    font_size_pt: f64,
    fallback_families: &[&str],
) -> config::ConfigHandle {
    let mut cfg = config::Config::default_config();
    let mut font_attrs = Vec::with_capacity(1 + fallback_families.len());
    font_attrs.push(config::FontAttributes::new(primary_family));
    for fam in fallback_families {
        font_attrs.push(config::FontAttributes::new_fallback(fam));
    }
    cfg.font = config::TextStyle { font: font_attrs, foreground: None };
    cfg.font_size = font_size_pt;
    config::ConfigHandle::new(cfg)
}

#[cfg(test)]
#[path = "fontstack_tests.rs"]
mod fontstack_tests;
