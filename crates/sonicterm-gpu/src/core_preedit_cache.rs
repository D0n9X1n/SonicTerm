//! Preedit cache responsibilities of the renderer, preserving its single owner.

use super::*;

pub(super) struct PreeditGlyphCache {
    pub(super) text: String,
    pub(super) font_size: f32,
    pub(super) start_x: f32,
    pub(super) top_y: f32,
    pub(super) color_bits: u32,
    pub(super) atlas_stamp: GlyphContentStamp,
    pub(super) glyphs: Vec<GlyphInstance>,
    /// Outline quads of unresolved characters, replayed with the glyphs.
    pub(super) missing_boxes: Vec<QuadInstance>,
    /// The characters those outlines stand for, replayed into the frame's chrome readout.
    pub(super) missing_chrome_chars: Vec<char>,
}

impl PreeditGlyphCache {
    /// True when this cache entry exactly matches the requested preedit emit
    /// (text + placement + color + atlas generation).
    pub(super) fn matches(
        &self,
        text: &str,
        font_size: f32,
        start_x: f32,
        top_y: f32,
        color_bits: u32,
        atlas_stamp: GlyphContentStamp,
    ) -> bool {
        self.atlas_stamp == atlas_stamp
            && self.color_bits == color_bits
            && self.font_size.to_bits() == font_size.to_bits()
            && self.start_x.to_bits() == start_x.to_bits()
            && self.top_y.to_bits() == top_y.to_bits()
            && self.text == text
    }
}

#[cfg(test)]
#[path = "core_preedit_cache_tests.rs"]
mod core_preedit_cache_tests;
