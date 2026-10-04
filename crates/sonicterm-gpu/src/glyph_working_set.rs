//! A fixture's glyph working set, measured through the renderer's own font stacks.
//!
//! The helper builds the renderer's body, tab-title and palette-footer stacks with
//! `renderer_font_stacks`, lays every source the renderer can draw for a text out through the
//! same chrome layout path, and inserts the grid's ASCII fast-path keys, all into a fixed 2048
//! atlas. The result is a conservative superset: over-inclusion can only raise the start size.

use std::collections::HashSet;
use std::path::PathBuf;

use sonicterm_text::glyph_atlas::{FitOutcome, GlyphAtlas, ATLAS_DIM};
use sonicterm_types::{GlyphKey, GlyphRasterVariant};

use crate::chrome_text::{self, ChromeAttrs};
use crate::color::ChromeColor;
use crate::core::{palette_footer_font_size, renderer_font_stacks};

/// The working set the renderer could hold for a text at one DPI.
#[derive(Debug, Clone)]
pub struct GlyphWorkingSet {
    /// Smallest start size the resident tiles would have fitted.
    pub fit_outcome: FitOutcome,
    /// Largest tile width and largest tile height.
    pub max_tile_dims: [u32; 2],
    /// Area of every resident tile at its tile size.
    pub packed_pixels: u64,
    /// Every resident tile's key.
    pub tile_keys: HashSet<GlyphKey>,
    /// The point size each raster variant was drawn at.
    pub variant_sizes: Vec<(GlyphRasterVariant, f32)>,
}

/// Printable ASCII, which the renderer can draw in every chrome surface.
fn printable_ascii() -> String {
    (' '..='~').collect()
}

/// Every style face a cell or chrome run can request.
const STYLES: [(bool, bool); 4] = [(false, false), (true, false), (false, true), (true, true)];

/// Measure the working set of `texts` (body) and `chrome_texts` (tab titles) at `size` points and
/// `dpi`, using `family` from the system and `font_dirs`. `None` when the body stack cannot load.
///
/// Sources, each in its own stack, size and raster variant:
/// - body (`Normal`): the texts, printable ASCII, `…`, and each non-ASCII character in text and
///   emoji presentation, in the four style faces, shaped and as ASCII fast-path keys;
/// - tab titles (`TabTitle`, body + 1): printable ASCII, `…` and the titles;
/// - palette footer (`PaletteFooter`, max(body − 1, 1)): printable ASCII and `…`.
#[must_use]
pub fn measure_glyph_working_set(
    texts: &[&str],
    chrome_texts: &[&str],
    family: &str,
    size: f32,
    dpi: usize,
    font_dirs: &[PathBuf],
) -> Option<GlyphWorkingSet> {
    let stacks = renderer_font_stacks(family, size, dpi, 1.0, font_dirs);
    let body = stacks.body?;
    let tab_size = sonicterm_render_model::boundary::ui::tab_spans::tab_title_font_size(size);
    let footer_size = palette_footer_font_size(size);
    let mut body_text: String = texts.concat();
    body_text.push_str(&printable_ascii());
    body_text.push('…');
    for character in texts.iter().flat_map(|text| text.chars()).filter(|ch| !ch.is_ascii()) {
        // Both presentations: a cell can carry either selector after an emoji-capable char.
        body_text.extend([character, '\u{FE0E}', character, '\u{FE0F}']);
    }
    let mut tab_text = printable_ascii();
    tab_text.push('…');
    tab_text.push_str(&chrome_texts.concat());
    let mut footer_text = printable_ascii();
    footer_text.push('…');

    // A fixed maximum atlas never grows, so the replay sees every tile the sources produce.
    let mut atlas = GlyphAtlas::new(ATLAS_DIM, ATLAS_DIM);
    let px_per_pt = dpi as f32 / 72.0;
    let mut surfaces = vec![(&body, size, GlyphRasterVariant::Normal, body_text.as_str())];
    if let Some(stack) = stacks.tab_title.as_ref() {
        surfaces.push((stack, tab_size, GlyphRasterVariant::TabTitle, tab_text.as_str()));
    }
    if let Some(stack) = stacks.palette_footer.as_ref() {
        surfaces.push((
            stack,
            footer_size,
            GlyphRasterVariant::PaletteFooter,
            footer_text.as_str(),
        ));
    }
    let mut variant_sizes = Vec::new();
    for (stack, point_size, variant, text) in surfaces {
        variant_sizes.push((variant, point_size));
        let raster_px = point_size * px_per_pt;
        let mut raster = stack.clone();
        for (bold, italic) in STYLES {
            let _layout = chrome_text::layout_with_raster_variant(
                stack,
                &mut raster,
                &mut atlas,
                text,
                ChromeColor::WHITE,
                ChromeAttrs { bold, italic },
                raster_px,
                raster_px,
                (0.0, raster_px),
                (65_536.0, 65_536.0),
                None,
                variant,
            );
        }
    }
    // The grid's ASCII fast path keys cells by character, not by shaped glyph id.
    let mut body_raster = body.clone();
    for character in printable_ascii().chars() {
        for (bold, italic) in STYLES {
            let _info =
                atlas.get_or_insert(GlyphKey::new(character, bold, italic), &mut body_raster);
        }
    }
    Some(GlyphWorkingSet {
        fit_outcome: atlas.fit_outcome(),
        max_tile_dims: atlas.max_tile_dims(),
        packed_pixels: atlas.packed_pixels(),
        tile_keys: atlas.resident_tile_keys(),
        variant_sizes,
    })
}

#[cfg(test)]
#[path = "glyph_working_set_tests.rs"]
mod glyph_working_set_tests;
