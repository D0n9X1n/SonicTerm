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
/// Width is the **maximum shaped advance over printable ASCII
/// codepoints `0x20..=0x7E`** — i.e. the same rule WezTerm uses in
/// `wezterm-font/src/ftwrap.rs::cell_metrics` (which iterates glyphs
/// 32..128 and takes `max(horiAdvance)`). Issue #623 stage 4: the
/// pre-fix path measured only `"M"`, which under-sizes the grid by
/// 3–5% for fonts where the widest ASCII glyph isn't M (e.g. some
/// patched Nerd Fonts where `_`, `w`, `@` exceed M's advance). That
/// in turn caused the icon↔label spacing tightness vs WezTerm
/// reported in #623 symptom #2.
///
/// Height is the caller-supplied `line_h` so the renderer can apply
/// the user's `line_height` multiplier on top of [`natural_line_h_px`].
///
/// Codepoints that the font can't shape (no glyph available, zero
/// advance) are skipped rather than crashing — `cell_w` is the max
/// over the present subset, falling back to `size * 0.6` if nothing
/// shapes (matches the previous "M not shapeable" fallback).
pub fn measure_cell(fs: &mut FontSystem, family: &str, size: f32, line_h: f32) -> (f32, f32) {
    let attrs = terminal_font_attrs(family);
    let fallback = size * 0.6;
    let mut max_w: f32 = 0.0;
    // Reuse one Buffer across the scan — shaping one codepoint at a
    // time is fine here (this runs once per font-size change, not per
    // frame) and keeps allocations low.
    let mut buf = Buffer::new(fs, Metrics::new(size, line_h));
    buf.set_size(fs, Some(1000.0), Some(1000.0));
    let mut tmp = [0u8; 4];
    // WezTerm scans 32..128; we use the printable subset 0x20..=0x7E
    // (DEL at 0x7F has zero advance in most fonts anyway). Inclusive
    // upper bound matches wezterm's `..128` exclusive range modulo DEL.
    for code in 0x20u32..=0x7Eu32 {
        let Some(ch) = char::from_u32(code) else { continue };
        let s = ch.encode_utf8(&mut tmp);
        buf.set_text(fs, s, &attrs, Shaping::Advanced, None);
        buf.shape_until_scroll(fs, false);
        let advance =
            buf.layout_runs().next().and_then(|r| r.glyphs.first().map(|g| g.w)).unwrap_or(0.0);
        if advance > max_w {
            max_w = advance;
        }
    }
    let w = if max_w > 0.0 { max_w } else { fallback };
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
