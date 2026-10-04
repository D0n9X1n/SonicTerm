//! A fixture's glyph working set, measured through the renderer's own font stacks.
//!
//! The helper builds the renderer's body, tab-title and palette-footer stacks with
//! `renderer_font_stacks`, waits for fallback discovery on every source the renderer can draw for a
//! text, lays each source out through the same chrome layout path, and inserts the
//! grid's ASCII fast-path keys, all into a fixed 2048 atlas. Waiting means a cold measurement
//! holds the fallback faces' CJK and emoji tiles rather than the frame path's tofu. The result is
//! a conservative superset: over-inclusion can only raise the start size.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use sonicterm_text::glyph_atlas::{FitOutcome, GlyphAtlas, GlyphInfo, ATLAS_DIM};
use sonicterm_types::{GlyphKey, GlyphRasterVariant};

use crate::chrome_text::{self, ChromeAttrs};
use crate::color::ChromeColor;
use crate::core::{palette_footer_font_size, renderer_font_stacks, RendererFontStacks};

/// A resident tile's identity across font configurations: its resolved face, glyph and strike,
/// its raster variant and its presentation flags. Unlike a [`GlyphKey`], it carries no font slot,
/// whose numbering is local to the configuration that resolved it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TileIdentity {
    /// The face, glyph id and strike the tile was rasterized from.
    pub source: sonicterm_engine::ResolvedGlyphFace,
    /// The raster role whose strike drew the tile.
    pub raster_variant: GlyphRasterVariant,
    /// Whether the key asked for the bold face, which may be synthesized from the same file.
    pub bold: bool,
    /// Whether the key asked for the italic face, which may be synthesized from the same file.
    pub italic: bool,
    /// Whether the tile holds color artwork.
    pub is_color: bool,
    /// Whether the tile holds subpixel coverage.
    pub is_subpixel: bool,
}

/// The identity of the tile `key` holds as `info`, resolved through `stack`, the stack that
/// rasterized it. `None` when the key no longer resolves to a face.
#[must_use]
pub fn tile_identity(
    stack: &sonicterm_engine::FontStack,
    key: GlyphKey,
    info: &GlyphInfo,
) -> Option<TileIdentity> {
    Some(TileIdentity {
        source: stack.resolved_glyph_face(key)?,
        raster_variant: key.raster_variant,
        bold: key.weight_bold,
        italic: key.italic,
        is_color: info.is_color,
        is_subpixel: info.is_subpixel,
    })
}

/// Every resident tile of `atlas` by identity, with its raster width and height, resolving each
/// key through the stack `stack_for` returns for its raster variant. The second value lists the
/// resident keys that resolved to no identity.
#[must_use]
pub fn resident_tile_identities<'stack>(
    atlas: &GlyphAtlas,
    stack_for: impl Fn(GlyphRasterVariant) -> Option<&'stack sonicterm_engine::FontStack>,
) -> (HashMap<TileIdentity, [u32; 2]>, Vec<GlyphKey>) {
    let mut identities = HashMap::new();
    let mut unresolved = Vec::new();
    for key in atlas.resident_tile_keys() {
        let resolved = atlas.get(key).and_then(|info| {
            let identity = tile_identity(stack_for(key.raster_variant)?, key, &info)?;
            Some((identity, info.px_size))
        });
        match resolved {
            Some((identity, size)) => {
                identities.insert(identity, size);
            }
            None => unresolved.push(key),
        }
    }
    (identities, unresolved)
}

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
    /// Every resident tile's raster width and height in pixels, by key.
    pub tile_sizes: HashMap<GlyphKey, [u32; 2]>,
    /// Every resident tile's raster width and height in pixels, by identity across configurations.
    pub tile_identities: HashMap<TileIdentity, [u32; 2]>,
    /// The point size each raster variant was drawn at.
    pub variant_sizes: Vec<(GlyphRasterVariant, f32)>,
}

/// Printable ASCII, which the renderer can draw in every chrome surface.
fn printable_ascii() -> String {
    (' '..='~').collect()
}

/// Symbols the chrome itself draws beside ASCII: the palette footer's and detail rows' `·`
/// separators, its `↑↓` and `↵` key hints, the tab-colour row's `—`, and the tab badges' `✓` and
/// `✗`. Every strike measures them, so a locale's footer cannot hold a key the helper missed.
const CHROME_SYMBOLS: &str = "·↑↓↵—✓✗";

/// Every style face a cell or chrome run can request.
const STYLES: [(bool, bool); 4] = [(false, false), (true, false), (false, true), (true, true)];

/// Measure the working set of `texts` (body) and `chrome_texts` (tab titles) at `size` points and
/// `dpi`, using `family` from the system and `font_dirs`. `None` when the body stack cannot load.
///
/// Sources, each in its own stack, size and raster variant:
/// - body (`Normal`): the texts, printable ASCII, `…`, the chrome symbols, and each non-ASCII
///   character in text and emoji presentation, in the four style faces, shaped and as ASCII
///   fast-path keys;
/// - tab titles (`TabTitle`, body + 1): printable ASCII, `…`, the chrome symbols and the titles;
/// - palette footer (`PaletteFooter`, max(body − 1, 1)): printable ASCII, `…` and the chrome
///   symbols.
#[must_use]
pub fn measure_glyph_working_set(
    texts: &[&str],
    chrome_texts: &[&str],
    family: &str,
    size: f32,
    dpi: usize,
    font_dirs: &[PathBuf],
) -> Option<GlyphWorkingSet> {
    measure_with_stacks(
        texts,
        chrome_texts,
        renderer_font_stacks(family, size, dpi, 1.0, font_dirs),
        size,
        dpi,
    )
}

/// [`measure_glyph_working_set`] over the renderer stacks `stacks`, built for `size` and `dpi`.
fn measure_with_stacks(
    texts: &[&str],
    chrome_texts: &[&str],
    stacks: RendererFontStacks,
    size: f32,
    dpi: usize,
) -> Option<GlyphWorkingSet> {
    let body = stacks.body?;
    let tab_size = sonicterm_render_model::boundary::ui::tab_spans::tab_title_font_size(size);
    let footer_size = palette_footer_font_size(size);
    let mut body_text: String = texts.concat();
    body_text.push_str(&printable_ascii());
    body_text.push('…');
    body_text.push_str(CHROME_SYMBOLS);
    for character in texts.iter().flat_map(|text| text.chars()).filter(|ch| !ch.is_ascii()) {
        // Both presentations: a cell can carry either selector after an emoji-capable char.
        body_text.extend([character, '\u{FE0E}', character, '\u{FE0F}']);
    }
    let mut tab_text = printable_ascii();
    tab_text.push('…');
    tab_text.push_str(CHROME_SYMBOLS);
    tab_text.push_str(&chrome_texts.concat());
    let mut footer_text = printable_ascii();
    footer_text.push('…');
    footer_text.push_str(CHROME_SYMBOLS);

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
            // Wait for fallback discovery before the frame-path layout: the stack's loaded face
            // for this style and size is shared, so the layout below shapes the faces discovery
            // published instead of notdef, and the atlas holds real tiles rather than tofu. A
            // failed warm-up leaves the layout to report what the frame path would draw.
            let _warmed = stack.shape_text_with_style(text, bold, italic);
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
    let tile_keys = atlas.resident_tile_keys();
    let (tile_identities, _unresolved) =
        resident_tile_identities(&atlas, |variant| match variant {
            GlyphRasterVariant::Normal => Some(&body),
            GlyphRasterVariant::TabTitle => stacks.tab_title.as_ref(),
            GlyphRasterVariant::PaletteFooter => stacks.palette_footer.as_ref(),
        });
    Some(GlyphWorkingSet {
        tile_identities,
        fit_outcome: atlas.fit_outcome(),
        max_tile_dims: atlas.max_tile_dims(),
        packed_pixels: atlas.packed_pixels(),
        tile_sizes: tile_keys
            .iter()
            .filter_map(|key| atlas.get(*key).map(|info| (*key, info.px_size)))
            .collect(),
        tile_keys,
        variant_sizes,
    })
}

#[cfg(test)]
#[path = "glyph_working_set_tests.rs"]
mod glyph_working_set_tests;
