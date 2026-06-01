//! Layout / metric helpers extracted from `render.rs` (issue #143).

use glyphon::{Buffer, FontSystem, Metrics, Shaping};
use sonicterm_text::terminal_font_attrs;

use crate::tabbar_view::TAB_BAR_HEIGHT;

/// Pure helper computing the top inset reserved above the grid for both
/// the OS titlebar band (when an integrated titlebar pushes the content
/// view under the native chrome) and the tab bar. Returns the titlebar
/// inset alone when the tab bar is hidden, so the grid recovers the row
/// the bar used to take. Exposed so tests can validate visibility wiring
/// without needing a live GPU context.
pub fn tab_bar_top_inset(visible: bool, padding: f32) -> f32 {
    tab_bar_top_inset_with_titlebar(visible, padding, 0.0)
}

/// Same as [`tab_bar_top_inset`] but adds a reserved titlebar band on top.
/// `titlebar_inset` is the height in logical pixels the OS reserves at the
/// top of the content view (e.g. macOS traffic-lights strip when
/// `with_fullsize_content_view(true)`). Pass 0 when the OS already keeps
/// our content below its chrome.
pub fn tab_bar_top_inset_with_titlebar(visible: bool, padding: f32, titlebar_inset: f32) -> f32 {
    let bar = if visible { TAB_BAR_HEIGHT + padding } else { padding };
    titlebar_inset + bar
}

/// Atlas dimension to allocate for a given DPI scale. On 2× screens we
/// roughly double-stack tiles, so a base 2048² atlas isn't enough room
/// for the same working set. We use `max(2048, base * ceil(scale))` to
/// keep the 1× footprint unchanged while reserving headroom on Retina.
pub fn atlas_dim_for_scale(scale_factor: f32) -> u32 {
    let base = crate::glyph_atlas::ATLAS_DIM;
    let s = scale_factor.max(1.0).ceil() as u32;
    base.saturating_mul(s).max(base)
}

pub(super) fn measure_cell(
    fs: &mut FontSystem,
    family: &str,
    size: f32,
    line_h: f32,
) -> (f32, f32) {
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
/// cell pitch. Sonic prior to this change used `size * line_height`,
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
