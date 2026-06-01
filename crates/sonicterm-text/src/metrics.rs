//! Font + atlas metric helpers (extracted from `sonicterm-shared::render::metrics`
//! in M7c). These depend on glyphon shaping and the atlas tile size, so
//! they live alongside the rest of the text layer rather than in the
//! renderer.

use cosmic_text::{Buffer, FontSystem, Metrics, Shaping};

use crate::glyph_atlas::ATLAS_DIM;
use crate::terminal_font_attrs;

/// Atlas dimension to allocate for a given DPI scale. On 2× screens we
/// roughly double-stack tiles, so a base 2048² atlas isn't enough room
/// for the same working set. We use `max(2048, base * ceil(scale))` to
/// keep the 1× footprint unchanged while reserving headroom on Retina.
pub fn atlas_dim_for_scale(scale_factor: f32) -> u32 {
    let base = ATLAS_DIM;
    let s = scale_factor.max(1.0).ceil() as u32;
    base.saturating_mul(s).max(base)
}

/// Measure one cell's pixel size (`(cell_w, cell_h)`) for `family` at
/// the given `size` (logical px) using the supplied `line_h`.
///
/// Width is taken from the shaped advance of `"M"`; height is the
/// caller-supplied `line_h` so the renderer can apply the user's
/// `line_height` multiplier on top of [`natural_line_h_px`].
pub fn measure_cell(fs: &mut FontSystem, family: &str, size: f32, line_h: f32) -> (f32, f32) {
    let mut buf = Buffer::new(fs, Metrics::new(size, line_h));
    buf.set_size(fs, Some(1000.0), Some(1000.0));
    buf.set_text(fs, "M", &terminal_font_attrs(family), Shaping::Advanced, None);
    buf.shape_until_scroll(fs, false);
    let w =
        buf.layout_runs().next().and_then(|r| r.glyphs.first().map(|g| g.w)).unwrap_or(size * 0.6);
    (w, line_h)
}

/// Compute the *natural* line height of `family` at `size` (logical px)
/// using the actual font's intrinsic vertical metrics — `ascent`,
/// `descent`, and `leading` (a.k.a. `line_gap`) — pulled from the
/// font's hhea/OS-2 tables via cosmic-text → skrifa.
///
/// This is the value WezTerm multiplies by `line_height` to derive its
/// cell pitch. SonicTerm prior to this change used `size * line_height`,
/// which silently dropped the font's intrinsic line gap and produced
/// cells that were ~88% of WezTerm's at otherwise-identical config
/// (font_size=14, line_height=1.1 on a typical monospace).
///
/// Falls back to `size` if the font can't be resolved (e.g. user
/// configured a family that isn't installed and we end up on a system
/// fallback that doesn't shape `"M"`).
pub fn natural_line_h_px(fs: &mut FontSystem, family: &str, size: f32) -> f32 {
    let mut buf = Buffer::new(fs, Metrics::new(size, size));
    buf.set_size(fs, Some(1000.0), Some(1000.0));
    buf.set_text(fs, "M", &terminal_font_attrs(family), Shaping::Advanced, None);
    buf.shape_until_scroll(fs, false);
    let Some(font_id) = buf.layout_runs().next().and_then(|r| r.glyphs.first().map(|g| g.font_id))
    else {
        return size;
    };
    // Default weight is fine — we only need vertical metrics, and these
    // are essentially weight-invariant for the families we care about.
    let Some(font) = fs.get_font(font_id, cosmic_text::fontdb::Weight::NORMAL) else {
        return size;
    };
    let m = font.metrics();
    let upem = f32::from(m.units_per_em).max(1.0);
    // skrifa's descent is typically negative (below baseline); leading
    // is the recommended gap between consecutive lines. Sum the
    // magnitudes — this matches the OpenType "ascent + |descent| +
    // line_gap" convention WezTerm uses.
    let natural_units = m.ascent + m.descent.abs() + m.leading;
    let natural_em = natural_units / upem;
    (natural_em * size).max(size)
}
