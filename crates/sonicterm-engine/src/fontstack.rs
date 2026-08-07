//! Renderer-facing adapter over SonicTerm's font discovery, shaping, and
//! rasterization stack.
//!
//! The adapter owns the configured-family provenance check and converts native
//! raster output into fixed-geometry atlas tiles. Weight scaling changes ink
//! coverage only; fallback, color, and explicitly bold glyphs remain untouched.

use std::cell::Cell;
use std::path::PathBuf;
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
/// The renderer's coordinate system is raster
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
    font_size_pt: f64,
    regular_weight_scale: f32,
    /// Memoized cell height in raster px, used to size outline growth.
    /// `0.0` means "not yet computed"; invalidated on scaling changes.
    cell_h_px: Cell<f64>,
}

impl FontStack {
    /// Construct a [`FontStack`] using the project's default font
    /// family (`DEFAULT_FONT_FAMILY` — "Rec Mono St.Helens") backed by
    /// the synthesized `FALLBACK_FAMILIES` chain. On first call this
    /// installs a process-wide wezterm `Config` so that
    /// `FontConfiguration::new(None, dpi)` selects sonicterm's primary
    /// family instead of sonicterm-font's bundled JetBrains Mono default.
    pub fn try_new(dpi: usize) -> Result<Self> {
        Self::try_new_full(DEFAULT_FONT_FAMILY, 14.0, dpi)
    }

    /// Construct a [`FontStack`] with `primary_family` as the baseline
    /// `[font] family` setting. The first call to this function (or
    /// [`Self::try_new`]) installs a process-wide font configuration; later
    /// calls still pass an explicit per-stack configuration so live family
    /// changes build a stack for the requested family.
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
            font_size_pt,
            regular_weight_scale: sanitize_weight_scale(regular_weight_scale),
            cell_h_px: Cell::new(0.0),
        })
    }

    /// Construct from exact font directories and an explicit style.
    ///
    /// Hidden because this is a deterministic test seam, not user-facing font
    /// discovery. The caller supplies tracked font files/directories and can
    /// mark each family configured or fallback without consulting OS fonts.
    #[doc(hidden)]
    pub fn try_new_with_font_dirs_for_test(
        families: &[(&str, bool)],
        font_dirs: Vec<PathBuf>,
        font_size_pt: f64,
        dpi: usize,
        regular_weight_scale: f32,
    ) -> Result<Self> {
        let mut cfg = config::Config::default_config();
        cfg.font.font = families
            .iter()
            .map(|(family, is_fallback)| config::FontAttributes {
                family: (*family).to_string(),
                is_fallback: *is_fallback,
                ..config::FontAttributes::default()
            })
            .collect();
        cfg.font_size = font_size_pt;
        cfg.font_dirs = font_dirs;
        cfg.font_locator = config::FontLocatorSelection::ConfigDirsOnly;
        cfg.search_font_dirs_for_fallback = true;
        let fc = FontConfiguration::new(Some(config::ConfigHandle::new(cfg)), dpi)?;
        Ok(Self {
            fc: Rc::new(fc),
            font_size_pt,
            regular_weight_scale: sanitize_weight_scale(regular_weight_scale),
            cell_h_px: Cell::new(0.0),
        })
    }

    /// Create a native-size view that shares this stack's font configuration.
    ///
    /// Size stays part of sonicterm-font's loaded-face cache key, so bitmap
    /// strikes remain correct without duplicating font databases and fallback
    /// infrastructure for every chrome size in every window.
    #[must_use]
    pub fn with_font_size(&self, font_size_pt: f64) -> Self {
        Self {
            fc: Rc::clone(&self.fc),
            font_size_pt,
            regular_weight_scale: self.regular_weight_scale,
            cell_h_px: Cell::new(0.0),
        }
    }

    /// Whether two stacks share one font configuration and its databases.
    #[doc(hidden)]
    #[must_use]
    pub fn shares_configuration_with(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.fc, &other.fc)
    }

    /// Apply a logical font scale and raster DPI, returning the values accepted
    /// by the underlying font configuration.
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
        self.fc.resolve_font_at_size(&style, self.font_size_pt)
    }

    /// Measure a left-to-right text run in raster pixels using the same
    /// fallback-font shaping policy as the renderer.
    pub fn measure_text_width(&self, text: &str) -> Result<f32> {
        let glyphs = self.shape_text(text)?;
        Ok(glyphs.iter().map(|glyph| glyph.x_advance.get() as f32).sum())
    }

    /// Resolve a family to its exact handle index in this stack.
    ///
    /// Hidden test seam for provenance regression tests that must address a
    /// fallback handle directly even when the primary also covers the glyph.
    #[doc(hidden)]
    pub fn font_index_for_test(&self, family: &str) -> Result<usize> {
        let font = self.font_for_style(false, false)?;
        let attr = config::FontAttributes::new(family);
        font.clone_handles()
            .iter()
            .position(|handle| handle.matches_name(&attr))
            .ok_or_else(|| anyhow::anyhow!("tracked font fixture {family:?} did not resolve"))
    }

    /// Shape a glyph directly with a tracked family and return its glyph id.
    #[doc(hidden)]
    pub fn glyph_id_for_family_for_test(&self, family: &str, ch: char) -> Result<u32> {
        let style = TextStyle { font: vec![config::FontAttributes::new(family)], foreground: None };
        self.fc
            .resolve_font_at_size(&style, self.font_size_pt)?
            .blocking_shape(
                &ch.to_string(),
                Some(Presentation::Text),
                Direction::LeftToRight,
                None,
                None,
            )?
            .into_iter()
            .find(|glyph| glyph.glyph_pos != 0)
            .map(|glyph| glyph.glyph_pos)
            .ok_or_else(|| anyhow::anyhow!("tracked font fixture {family:?} lacks {ch:?}"))
    }

    /// Return cell metrics for the default font, projected into the
    /// renderer-facing [`CellMetricsPx`] (raster px). Wezterm's
    /// `FontMetrics` already lives in raster px, so this is a plain
    /// field extraction — no `* scale_factor` multiplier here, and
    /// none at the call site.
    ///
    /// Errors when sonicterm-font fails to load the default font (e.g.
    /// no installed fallback covers the configured family). Callers
    /// in the hot path should propagate; tests can `unwrap` once
    /// they've confirmed sonicterm-font picked something up.
    pub fn cell_metrics_raster_px(&self) -> Result<CellMetricsPx> {
        let m = self.fc.default_font_metrics_at_size(self.font_size_pt)?;
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
            // When: `glyph_id` is zero, shape `ch` so fallback resolution supplies both glyph and font slot.
            let s = key.ch.to_string();
            let infos = font
                .blocking_shape(&s, Some(Presentation::Text), Direction::LeftToRight, None, None)
                .ok()?;
            let first = infos.into_iter().find(|g| g.glyph_pos != 0)?;
            (first.font_idx, first.glyph_pos)
        };

        let rg = font.rasterize_glyph(glyph_pos, font_idx).ok()?;
        if rg.data.is_empty() || rg.width == 0 || rg.height == 0 {
            // When: empty raster data or dimensions cannot form a valid atlas tile.
            return None;
        }
        let expected_len = checked_glyph_rgba_len(rg.width, rg.height).ok()?;
        if rg.data.len() != expected_len {
            // When: `rg.data.len()` differs from `expected_len`, reject malformed coverage before conversion or upload.
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
            // When: `has_color` is false, derive monochrome or subpixel coverage from the raster channels.
            let has_subpixel_coverage =
                rg.data.chunks_exact(4).any(|px| px[0] != px[1] || px[1] != px[2]);
            if has_subpixel_coverage {
                let mut bgra = Vec::with_capacity(rg.data.len());
                for px in rg.data.chunks_exact(4) {
                    bgra.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                }
                (bgra, false, true)
            } else {
                // When: `has_subpixel_coverage` is false, one alpha mask replaces four redundant channel bytes.
                let mask: Vec<u8> = rg.data.chunks_exact(4).map(|p| p[3]).collect();
                (mask, false, false)
            }
        };
        let mut tile_w = rg.width;
        let mut tile_h = rg.height;
        let mut offset_x = rg.bearing_x.get() as i32;
        let mut offset_y = -rg.bearing_y.get() as i32;
        if weight_scale_applies(is_color, key.weight_bold, font.is_configured_family(font_idx)) {
            apply_regular_weight_scale(&mut coverage, self.regular_weight_scale, is_subpixel);
            // The coverage remap alone cannot thicken a stem whose core is
            // already fully opaque, which is the common case at HiDPI. Growing
            // the outline is what makes weight_scale visible there. The same
            // ceiling applies to thinning, so below 1.0 the outline shrinks.
            let cell_h = self.cell_h_px();
            let radius = embolden_radius_px(self.regular_weight_scale, cell_h);
            if let Some((grown, w, h, pad)) =
                embolden_coverage(&coverage, tile_w, tile_h, radius, is_subpixel)
            {
                coverage = grown;
                tile_w = w;
                tile_h = h;
                // Fixed-tile emboldening reports pad zero. Keep the adjustment
                // explicit so a future implementation that returns padding also
                // keeps its top-left corner aligned with the pen.
                offset_x -= pad as i32;
                offset_y -= pad as i32;
            }
            let thin = thin_radius_px(self.regular_weight_scale, cell_h);
            if let Some(eroded) = erode_coverage(&coverage, tile_w, tile_h, thin, is_subpixel) {
                // Erosion only removes ink, so dimensions and offsets hold.
                coverage = eroded;
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
            // When: positive `cached` metrics remain valid until `change_scaling` explicitly invalidates them.
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
        // When: `scale` is nonfinite or outside the supported range, identity avoids pathological coverage math.
        1.0
    }
}

/// Glyph outline growth per unit of `weight_scale` above 1.0, expressed as a
/// fraction of the cell height. Tuned so `weight_scale = 2.0` adds roughly
/// half a pixel of radius at a 13pt Retina cell.
const EMBOLDEN_RADIUS_PER_CELL_H: f64 = 0.02;

/// Raster-space growth ceiling while the output stays inside the original tile.
///
/// Larger bitmap dilations flatten counters and saturate the tile edge after
/// crop-back. The ceiling is deliberately independent of the glyph's existing
/// margins: flat-sided glyphs often touch one bitmap edge, and consulting spare
/// margin would disable growth for those while still growing curved glyphs.
const MAX_EMBOLDEN_RADIUS_PX: f64 = 1.0;

/// Radius, in raster px, that regular text should grow at `scale`. Zero at or
/// below `1.0`, where [`thin_radius_px`] takes over instead.
fn embolden_radius_px(scale: f32, cell_h: f64) -> f64 {
    if scale <= 1.0 || !cell_h.is_finite() || cell_h <= 0.0 {
        // When: `scale` does not request growth or `cell_h` is unusable, disable emboldening instead of guessing a radius.
        return 0.0;
    }
    f64::from(scale - 1.0) * cell_h * EMBOLDEN_RADIUS_PER_CELL_H
}

/// Outline shrink per unit of `weight_scale` below 1.0, as a fraction of cell
/// height. Slightly larger per unit than [`EMBOLDEN_RADIUS_PER_CELL_H`]
/// because thinning only has 0.5 of range to work with (0.5..1.0) against
/// emboldening's 4.0. Kept low enough that a small thin such as `0.9` stays a
/// nudge and leaves stem cores opaque rather than washing the glyph out.
const THIN_RADIUS_PER_CELL_H: f64 = 0.012;

/// Radius, in raster px, that regular text should shrink at `scale`. Zero at
/// or above `1.0`. Like emboldening, this exists because the coverage remap
/// cannot move a pixel that is already fully opaque — at HiDPI a stem core is
/// solid, so gamma alone leaves the stem exactly as wide as it started.
fn thin_radius_px(scale: f32, cell_h: f64) -> f64 {
    if scale >= 1.0 || !cell_h.is_finite() || cell_h <= 0.0 {
        // When: `scale` does not request thinning or `cell_h` is unusable, disable erosion instead of guessing a radius.
        return 0.0;
    }
    f64::from(1.0 - scale) * cell_h * THIN_RADIUS_PER_CELL_H
}

fn checked_coverage_len(width: usize, height: usize, channels: usize) -> Option<usize> {
    if width == 0
        || height == 0
        || width > MAX_RASTERIZED_GLYPH_DIMENSION
        || height > MAX_RASTERIZED_GLYPH_DIMENSION
    {
        // When: `width` or `height` is zero or exceeds the raster limit, no legal allocation exists.
        return None;
    }
    width.checked_mul(height)?.checked_mul(channels)
}

/// Shrink `coverage` inward by `radius` px in place. Unlike growth, erosion
/// never needs padding — the glyph only loses ink — so tile dimensions and
/// offsets are unchanged and the caller can swap the buffer straight in.
///
/// Returns `None` when there is nothing to do.
fn erode_coverage(
    coverage: &[u8],
    width: usize,
    height: usize,
    radius: f64,
    is_subpixel: bool,
) -> Option<Vec<u8>> {
    if radius <= 0.0 || width == 0 || height == 0 {
        // When: `radius` or tile dimensions are nonpositive, erosion has no valid work to perform.
        return None;
    }
    let channels = if is_subpixel { 4 } else { 1 };
    let byte_len = checked_coverage_len(width, height, channels)?;
    if coverage.len() != byte_len {
        // When: `coverage.len()` differs from `byte_len`, morphology would index outside the supplied tile.
        return None;
    }
    let pixel_len = width.checked_mul(height)?;
    let mut out = vec![0u8; byte_len];
    for ch in 0..channels {
        let mut plane = vec![0u8; pixel_len];
        for i in 0..pixel_len {
            plane[i] = coverage[i * channels + ch];
        }
        let mut tmp = vec![0u8; pixel_len];
        morph_axis(&plane, &mut tmp, width, height, width, radius, true);
        let mut transposed = vec![0u8; pixel_len];
        for y in 0..height {
            for x in 0..width {
                transposed[x * height + y] = tmp[y * width + x];
            }
        }
        let mut tcol = vec![0u8; pixel_len];
        morph_axis(&transposed, &mut tcol, height, width, height, radius, true);
        for y in 0..height {
            for x in 0..width {
                out[(y * width + x) * channels + ch] = tcol[x * height + y];
            }
        }
    }
    if is_subpixel {
        for px in out.chunks_exact_mut(4) {
            px[3] = px[0].max(px[1]).max(px[2]);
        }
    }
    Some(out)
}

/// One separable morphology pass with fractional radius. `radius` is split
/// into an integer core, taken at full strength, and a fractional outer ring
/// that is blended in proportionally so the change is smooth rather than
/// snapping a whole pixel at a time.
///
/// `erode` selects the operator: max-filter (grow) when false, min-filter
/// (shrink) when true. The two differ at the boundary — a max-filter ignores
/// out-of-bounds samples, while a min-filter must treat them as empty so the
/// glyph erodes inward from its own edge.
fn morph_axis(
    src: &[u8],
    dst: &mut [u8],
    len: usize,
    count: usize,
    stride: usize,
    radius: f64,
    erode: bool,
) {
    let whole = radius.floor() as usize;
    let frac = radius - radius.floor();
    for line in 0..count {
        let base = line * stride;
        for i in 0..len {
            let lo = i.saturating_sub(whole);
            let hi = (i + whole).min(len - 1);
            if erode {
                // A window that overhangs the tile edge sees empty space
                // there, so the glyph erodes inward from its own rim. Only
                // genuinely out-of-bounds samples count as empty — treating
                // in-bounds neighbours as empty would erode the whole glyph
                // rather than its edge.
                let mut best = if i < whole || i + whole >= len { 0u8 } else { u8::MAX };
                for j in lo..=hi {
                    best = best.min(src[base + j]);
                }
                if frac > 0.0 && best > 0 {
                    // Outer ring one step beyond the integer core, on both
                    // sides. Out-of-bounds reads as empty.
                    let left = if i > whole {
                        src[base + i - whole - 1]
                    } else {
                        // When: `i` has no sample beyond the left core, erosion sees empty space at the tile edge.
                        0
                    };
                    let right = if i + whole + 1 < len {
                        src[base + i + whole + 1]
                    } else {
                        // When: the right outer sample exceeds `len`, erosion sees empty space at the tile edge.
                        0
                    };
                    let ring = left.min(right);
                    if ring < best {
                        // Pull toward the ring minimum in proportion to frac.
                        let blended = f64::from(best) - (f64::from(best) - f64::from(ring)) * frac;
                        best = blended.round().clamp(0.0, 255.0) as u8;
                    }
                }
                dst[base + i] = best;
            } else {
                // When: `erode` is false, use a max filter to grow coverage without empty edge samples.
                let mut best = 0u8;
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
}

/// Grow `coverage` outward by `radius` raster pixels without changing the
/// returned tile dimensions or origin.
///
/// The operation pads scratch space so max-filter samples can cross the old
/// bitmap edge, then crops back to the original bounds. Radius is capped by
/// [`MAX_EMBOLDEN_RADIUS_PX`] rather than by spare bitmap margin: a flat-sided
/// glyph commonly touches an edge, and margin-dependent growth would make
/// weight behavior vary by glyph shape.
///
/// Returns `None` when there is nothing to do, the declared final dimensions
/// exceed the atlas limit, the input buffer does not match them exactly, or
/// checked scratch arithmetic overflows. The bounded scratch padding may exceed
/// the final atlas dimensions because it is cropped before return.
fn embolden_coverage(
    coverage: &[u8],
    width: usize,
    height: usize,
    radius: f64,
    is_subpixel: bool,
) -> Option<(Vec<u8>, usize, usize, usize)> {
    if radius <= 0.0 || width == 0 || height == 0 {
        // When: `radius` or tile dimensions are nonpositive, growth has no valid work to perform.
        return None;
    }
    let channels = if is_subpixel { 4 } else { 1 };
    let byte_len = checked_coverage_len(width, height, channels)?;
    if coverage.len() != byte_len {
        // When: `coverage.len()` differs from `byte_len`, dilation would index outside the supplied tile.
        return None;
    }
    // A shape-independent ceiling prevents high-weight crop saturation without
    // making flat glyphs (which commonly touch one bitmap edge) grow less than
    // curved glyphs. Fractional values below the ceiling still blend smoothly.
    let radius = radius.min(MAX_EMBOLDEN_RADIUS_PX);
    let pad = radius.ceil() as usize;
    let doubled_pad = pad.checked_mul(2)?;
    let new_w = width.checked_add(doubled_pad)?;
    let new_h = height.checked_add(doubled_pad)?;
    let scratch_pixels = new_w.checked_mul(new_h)?;
    let scratch_bytes = scratch_pixels.checked_mul(channels)?;
    let mut padded = vec![0u8; scratch_bytes];
    for y in 0..height {
        let src = y * width * channels;
        let dst = ((y + pad) * new_w + pad) * channels;
        padded[dst..dst + width * channels].copy_from_slice(&coverage[src..src + width * channels]);
    }

    // Dilate each channel independently. Interleaved BGRA is handled by
    // deinterleaving into a scratch plane, since the separable passes need a
    // contiguous stride per axis. Every byte is written below, so the buffer
    // starts zeroed rather than copied.
    let mut out = vec![0u8; scratch_bytes];
    for ch in 0..channels {
        let mut plane = vec![0u8; scratch_pixels];
        for i in 0..scratch_pixels {
            plane[i] = padded[i * channels + ch];
        }
        let mut tmp = vec![0u8; scratch_pixels];
        // Horizontal: new_h lines of new_w samples, stride new_w.
        morph_axis(&plane, &mut tmp, new_w, new_h, new_w, radius, false);
        // Vertical: transpose, reuse the same row-wise pass, transpose back.
        let mut transposed = vec![0u8; scratch_pixels];
        for y in 0..new_h {
            for x in 0..new_w {
                transposed[x * new_h + y] = tmp[y * new_w + x];
            }
        }
        let mut tcol = vec![0u8; scratch_pixels];
        morph_axis(&transposed, &mut tcol, new_h, new_w, new_h, radius, false);
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

    // Crop back to the original bounds. The padding exists so the dilation has
    // somewhere to expand into; keeping it would grow the tile, and a tile that
    // grows makes a *weight* control read as a *size* control. Every glyph then
    // changes size when the user asks for more ink, and glyphs from different
    // fonts change by different amounts — which is what makes two adjacent
    // markers drift apart.
    //
    // Ink that lands outside the original bounds is discarded. That is the
    // right trade: a rasterized glyph carries margin around its outline, so
    // there is room to thicken into, and where there is not, losing a fraction
    // of a pixel at the edge is less visible than every glyph resizing.
    let mut cropped = vec![0u8; byte_len];
    for y in 0..height {
        let src = ((y + pad) * new_w + pad) * channels;
        let dst = y * width * channels;
        cropped[dst..dst + width * channels].copy_from_slice(&out[src..src + width * channels]);
    }
    // `pad` is reported as zero: the caller shifts the tile offset by it, and
    // the tile no longer moved.
    Some((cropped, width, height, 0))
}

fn scale_coverage(coverage: u8, scale: f32) -> u8 {
    if coverage == 0 || coverage == u8::MAX || (scale - 1.0).abs() < f32::EPSILON {
        // When: `coverage` is an endpoint or `scale` is identity, gamma remapping cannot change the byte.
        return coverage;
    }
    let normalized = f32::from(coverage) / 255.0;
    let exponent = 1.0 / scale;
    (normalized.powf(exponent) * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Whether `weight_scale` may act on this glyph.
///
/// The setting scales the native weight of *the configured font*, so it must
/// reach that font's glyphs and no others. Three exclusions, each for its own
/// reason:
///
/// * **colour glyphs** — emoji carry their own artwork; remapping coverage on
///   them alters the picture rather than its weight.
/// * **SGR bold** — the terminal already resolved a bold face for those, and
///   scaling on top of it would compound two weight changes.
/// * **fallback glyphs** — these come from a font the user did not configure,
///   drawn at a weight its own designer chose. Reweighting them applies the
///   user's intent for one family to a different one, and the mismatch shows
///   whenever the two sit adjacent: a fallback glyph grows or thins while its
///   neighbour from the configured family does not move with it.
///
/// `is_configured_family` is asked of the loaded font rather than inferred
/// from the handle index. Resolution pushes a handle only when a family
/// actually matches, so a configured family that fails to load is absent
/// entirely and the first fallback inherits index 0 — an index test would then
/// reweight a font the user never named, in the one case where the difference
/// matters most.
///
/// Split out of `rasterize` so it can be tested. The gate governs both the
/// coverage remap and the outline growth that follows it, and a test that
/// reached only the helpers underneath would pass against a build whose gate
/// was gone.
fn weight_scale_applies(is_color: bool, weight_bold: bool, is_configured_family: bool) -> bool {
    !is_color && !weight_bold && is_configured_family
}

fn apply_regular_weight_scale(coverage: &mut [u8], scale: f32, is_subpixel: bool) {
    let scale = sanitize_weight_scale(scale);
    if (scale - 1.0).abs() < f32::EPSILON {
        // When: sanitized `scale` is identity, leave every coverage byte and subpixel alpha untouched.
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
        // When: `is_subpixel` is false, each byte is a complete scalar coverage sample.
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
