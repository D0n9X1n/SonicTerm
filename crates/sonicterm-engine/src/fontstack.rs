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

use std::rc::Rc;
use std::sync::Once;

use anyhow::Result;
use sonicterm_font::{Direction, FontConfiguration, Presentation};
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
/// itself caches per-font face state internally.
#[derive(Clone)]
pub struct FontStack {
    fc: Rc<FontConfiguration>,
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

    /// Construct a [`FontStack`] with explicit primary family + point
    /// size + dpi. Use this when the caller knows the renderer's scale
    /// factor: pass `dpi = 72 * scale_factor` so sonicterm-font's
    /// `point_size * dpi / 72` yields raster-px-per-em equal to
    /// `point_size * scale_factor`. Default font size is 14 pt to
    /// match `sonicterm-cfg::FontConfig::default()`.
    pub fn try_new_full(primary_family: &str, font_size_pt: f64, dpi: usize) -> Result<Self> {
        install_default_config(primary_family, font_size_pt);
        let fc = FontConfiguration::new(
            Some(build_config(primary_family, font_size_pt, FALLBACK_FAMILIES)),
            dpi,
        )?;
        Ok(Self { fc: Rc::new(fc) })
    }

    pub fn change_scaling(&self, font_scale: f64, dpi: usize) -> (f64, usize) {
        self.fc.change_scaling(font_scale, dpi)
    }

    /// Shape a text run using SonicTerm's current WezTerm font stack policy.
    pub fn shape_text(&self, text: &str) -> Result<Vec<sonicterm_font::shaper::GlyphInfo>> {
        let font = self.fc.default_font()?;
        Ok(font.blocking_shape(
            text,
            Some(Presentation::Text),
            Direction::LeftToRight,
            None,
            None,
        )?)
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
        let font = self.fc.default_font().ok()?;
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
        let (coverage, is_color) = if rg.has_color {
            let mut bgra = Vec::with_capacity(rg.data.len());
            for px in rg.data.chunks_exact(4) {
                bgra.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
            (bgra, true)
        } else {
            let mask: Vec<u8> = rg.data.chunks_exact(4).map(|p| p[3]).collect();
            (mask, false)
        };
        Some(RasterTile {
            width: rg.width as u32,
            height: rg.height as u32,
            offset_x: rg.bearing_x.get() as i32,
            offset_y: -rg.bearing_y.get() as i32,
            advance: rg.width as f32,
            coverage,
            is_color,
        })
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
mod tests {
    use super::*;

    #[test]
    fn explicit_config_records_requested_font_size() {
        let cfg = build_config("Rec Mono St.Helens", 17.0, &["Symbols Nerd Font Mono"]);
        assert_eq!(cfg.font_size, 17.0);
        assert_eq!(cfg.font.font[0].family, "Rec Mono St.Helens");
        assert_eq!(cfg.font.font[1].family, "Symbols Nerd Font Mono");
    }
}
