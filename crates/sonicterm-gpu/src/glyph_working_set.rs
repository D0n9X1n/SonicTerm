//! A fixture's glyph working set, measured through the renderer's own font stacks.
//!
//! The helper builds the renderer's body, tab-title and palette-footer stacks with
//! `renderer_font_stacks`, waits for fallback discovery on every source the renderer can draw for a
//! text, lays each source out through the same chrome layout path, and inserts the
//! grid's ASCII fast-path keys, all into a fixed 2048 atlas. Waiting means a cold measurement
//! holds the fallback faces' CJK and emoji tiles rather than the frame path's tofu. The result is
//! a conservative superset: over-inclusion can only raise the start size.
//!
//! A measurement is complete or rejected. A failed warm-up, a glyph the warm-up resolved that the
//! frame path still draws as notdef, a required tile the atlas did not place, and a resident key
//! without a face identity each reject it with a
//! [`WorkingSetError`](crate::glyph_working_set::WorkingSetError). A character no face covers, and
//! a resolved glyph whose face rasterizes nothing, are drawn by the renderer as the same tofu box,
//! so they are measured as it draws them and listed in
//! [`GlyphWorkingSet::unresolved_chars`](crate::glyph_working_set::GlyphWorkingSet::unresolved_chars)
//! and [`GlyphWorkingSet::raster_failed`](crate::glyph_working_set::GlyphWorkingSet::raster_failed).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use sonicterm_text::glyph_atlas::{FitOutcome, GlyphAtlas, GlyphInfo, ATLAS_DIM};
use sonicterm_text::start_size_inputs::{FailedFace, RasterFailure};
use sonicterm_types::{GlyphKey, GlyphRasterVariant};

use crate::chrome_text::{self, ChromeAttrs};
use crate::color::ChromeColor;
use crate::core::{palette_footer_font_size, renderer_font_stacks, RendererFontStacks};
use crate::frame_stats::CountingRasterizer;

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
    /// Characters no face covers: measured as the renderer draws them, as tofu, and listed here.
    pub unresolved_chars: Vec<char>,
    /// Resolved glyphs whose face rasterized nothing, each resolved to its face content, glyph and
    /// strike while its stack was alive: the renderer draws them as tofu too.
    pub raster_failed: Vec<RasterFailure>,
    /// Required glyphs whose tile is larger than the atlas can ever place, so they draw nothing.
    pub oversize_required: Vec<GlyphKey>,
}

/// The required tiles a measurement drew as tofu, listed rather than rejected.
#[derive(Debug, Default)]
struct Accounted {
    /// Characters no face covers.
    unresolved_chars: BTreeSet<char>,
    /// Resolved glyphs that rasterized to nothing, by key, each with its resolved face.
    raster_failed: HashMap<GlyphKey, RasterFailure>,
    /// Required glyphs cached as too large to place.
    oversize_required: HashSet<GlyphKey>,
    /// Each failed face's content identity, read once per face: hashing a large face file once per
    /// failed glyph would repeat the same work. `None` when its bytes could not be read.
    face_contents: HashMap<sonicterm_engine::FaceIdentity, Option<String>>,
}

/// Why a working-set measurement is incomplete, so it is rejected rather than classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkingSetError {
    /// The body font stack did not load.
    BodyStackUnavailable,
    /// The blocking warm-up could not shape a source in one style.
    WarmUp {
        /// The source's raster variant.
        variant: GlyphRasterVariant,
        /// Whether the style was bold.
        bold: bool,
        /// Whether the style was italic.
        italic: bool,
        /// The shaper's error.
        message: String,
    },
    /// The frame-path layout could not shape a source in one style.
    FrameShape {
        /// The source's raster variant.
        variant: GlyphRasterVariant,
        /// Whether the style was bold.
        bold: bool,
        /// Whether the style was italic.
        italic: bool,
    },
    /// The warm-up resolved `character` but the frame path still drew it as notdef: its fallback
    /// face was not published when the layout ran.
    FallbackPending {
        /// The source's raster variant.
        variant: GlyphRasterVariant,
        /// Whether the style was bold.
        bold: bool,
        /// Whether the style was italic.
        italic: bool,
        /// The cluster's lead character.
        character: char,
    },
    /// The atlas did not place a required tile and evicted nothing to explain it.
    NotPlaced {
        /// The tile's key.
        key: GlyphKey,
    },
    /// Resident keys that resolve to no face identity, so they cannot be compared with a renderer.
    UnresolvedIdentity {
        /// The keys.
        keys: Vec<GlyphKey>,
    },
}

impl std::fmt::Display for WorkingSetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BodyStackUnavailable => formatter.write_str("the body font stack did not load"),
            Self::WarmUp { variant, bold, italic, message } => write!(
                formatter,
                "warm-up of {variant:?} (bold {bold}, italic {italic}) failed: {message}"
            ),
            Self::FrameShape { variant, bold, italic } => write!(
                formatter,
                "frame-path layout of {variant:?} (bold {bold}, italic {italic}) could not shape"
            ),
            Self::FallbackPending { variant, bold, italic, character } => write!(
                formatter,
                "{character:?} in {variant:?} (bold {bold}, italic {italic}) resolved in the \
                 warm-up but the frame path drew notdef"
            ),
            Self::NotPlaced { key } => write!(formatter, "{key:?} was not placed"),
            Self::UnresolvedIdentity { keys } => {
                write!(formatter, "resident keys without a face identity: {keys:?}")
            }
        }
    }
}

impl std::error::Error for WorkingSetError {}

/// The production warm-up: the stack's blocking styled shaping, which waits for fallback faces.
/// Returns the cluster byte offsets it drew with a real glyph.
fn blocking_warm_up(
    stack: &sonicterm_engine::FontStack,
    text: &str,
    bold: bool,
    italic: bool,
) -> anyhow::Result<HashSet<usize>> {
    let shaped = stack.shape_text_with_style(text, bold, italic)?;
    Ok(shaped
        .iter()
        .filter(|glyph| glyph.glyph_pos != 0)
        .map(|glyph| glyph.cluster as usize)
        .collect())
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
/// `dpi`, using `family` from the system and `font_dirs`. An incomplete measurement is an `Err`.
///
/// Sources, each in its own stack, size and raster variant:
/// - body (`Normal`): the texts, printable ASCII, `…`, the chrome symbols, and each non-ASCII
///   character in text and emoji presentation, in the four style faces, shaped and as ASCII
///   fast-path keys;
/// - tab titles (`TabTitle`, body + 1): printable ASCII, `…`, the chrome symbols, every program
///   icon a title can carry (`tab_title::PROGRAM_ICONS`) and the titles;
/// - palette footer (`PaletteFooter`, max(body − 1, 1)): printable ASCII, `…` and the chrome
///   symbols.
pub fn measure_glyph_working_set(
    texts: &[&str],
    chrome_texts: &[&str],
    family: &str,
    size: f32,
    dpi: usize,
    font_dirs: &[PathBuf],
) -> Result<GlyphWorkingSet, WorkingSetError> {
    measure_with_stacks(
        texts,
        chrome_texts,
        renderer_font_stacks(family, size, dpi, 1.0, font_dirs),
        size,
        dpi,
        blocking_warm_up,
    )
}

/// [`measure_glyph_working_set`] over the renderer stacks `stacks`, built for `size` and `dpi`,
/// warming each source up with `warm_up` before its frame-path layout. `warm_up` shapes a text in
/// a style and returns the cluster byte offsets it drew with a real glyph.
fn measure_with_stacks(
    texts: &[&str],
    chrome_texts: &[&str],
    stacks: RendererFontStacks,
    size: f32,
    dpi: usize,
    warm_up: impl Fn(&sonicterm_engine::FontStack, &str, bool, bool) -> anyhow::Result<HashSet<usize>>,
) -> Result<GlyphWorkingSet, WorkingSetError> {
    let body = stacks.body.ok_or(WorkingSetError::BodyStackUnavailable)?;
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
    // Every icon a tab title can carry, not only those the fixture's processes would select.
    tab_text.extend(sonicterm_render_model::boundary::ui::tab_title::PROGRAM_ICONS);
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
    let mut accounted = Accounted::default();
    for (stack, point_size, variant, text) in surfaces {
        variant_sizes.push((variant, point_size));
        let raster_px = point_size * px_per_pt;
        let mut raster = stack.clone();
        for (bold, italic) in STYLES {
            // Wait for fallback discovery before the frame-path layout: the stack's loaded face
            // for this style and size is shared, so the layout below shapes the faces discovery
            // published instead of notdef, and the atlas holds real tiles rather than tofu.
            // Clusters the warm-up drew with a real glyph; the frame path must agree on each.
            let resolved = warm_up(stack, text, bold, italic).map_err(|error| {
                WorkingSetError::WarmUp { variant, bold, italic, message: error.to_string() }
            })?;
            let attrs = ChromeAttrs { bold, italic };
            let run = chrome_text::ChromeShapedRun::shape(stack, text, attrs, raster_px, raster_px)
                .ok_or(WorkingSetError::FrameShape { variant, bold, italic })?;
            let _layout = chrome_text::layout_prepared(
                &run,
                &mut raster,
                &mut atlas,
                ChromeColor::WHITE,
                (0.0, raster_px),
                (65_536.0, 65_536.0),
                None,
                variant,
            );
            for (cluster, character, key) in run.tile_keys(variant) {
                let Some(key) = key else {
                    // When: key is None the glyph is a notdef blank, which draws no tile and needs none.
                    continue;
                };
                if key.glyph_id == 0 && resolved.contains(&cluster) {
                    // When: key.glyph_id is 0 yet resolved holds the cluster, the fallback face lagged.
                    return Err(WorkingSetError::FallbackPending {
                        variant,
                        bold,
                        italic,
                        character,
                    });
                }
                account_tile(&atlas, stack, key, &mut accounted)?;
            }
        }
    }
    // The grid's ASCII fast path keys cells by character, not by shaped glyph id.
    let mut body_raster = body.clone();
    for character in printable_ascii().chars() {
        for (bold, italic) in STYLES {
            let key = GlyphKey::new(character, bold, italic);
            // Counted like every insertion; outside a frame's counting scope this records nothing.
            let _info = atlas.get_or_insert(key, &mut CountingRasterizer::new(&mut body_raster));
            account_tile(&atlas, &body, key, &mut accounted)?;
        }
    }
    let tile_keys = atlas.resident_tile_keys();
    let (tile_identities, unresolved) = resident_tile_identities(&atlas, |variant| match variant {
        GlyphRasterVariant::Normal => Some(&body),
        GlyphRasterVariant::TabTitle => stacks.tab_title.as_ref(),
        GlyphRasterVariant::PaletteFooter => stacks.palette_footer.as_ref(),
    });
    // A tofu tile names no face, so only a real glyph without an identity is a failure.
    let unidentified: Vec<GlyphKey> = unresolved
        .into_iter()
        .filter(|key| atlas.get(*key).is_some_and(|info| !info.missing))
        .collect();
    if !unidentified.is_empty() {
        // When: unidentified is not empty, a real resident tile has no face identity to compare.
        return Err(WorkingSetError::UnresolvedIdentity { keys: unidentified });
    }
    Ok(GlyphWorkingSet {
        unresolved_chars: accounted.unresolved_chars.into_iter().collect(),
        raster_failed: accounted.raster_failed.into_values().collect(),
        oversize_required: accounted.oversize_required.into_iter().collect(),
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

/// Each of `codepoints` resolved, through the body stack the renderer builds for `family`, `size`
/// and `dpi`, to the identity of the real tile it draws: shaped alone (waiting for fallback), keyed
/// as the shaped path keys it, and rasterized into a scratch atlas. `None` when no face draws it
/// with a real glyph, or its tile is missing or too large to place.
#[must_use]
pub fn representative_identities(
    codepoints: &[char],
    family: &str,
    size: f32,
    dpi: usize,
    font_dirs: &[PathBuf],
) -> Vec<(char, Option<TileIdentity>)> {
    let Some(body) = renderer_font_stacks(family, size, dpi, 1.0, font_dirs).body else {
        // When: the body stack cannot be built, no codepoint resolves.
        return codepoints.iter().map(|codepoint| (*codepoint, None)).collect();
    };
    let mut atlas = GlyphAtlas::new(ATLAS_DIM, ATLAS_DIM);
    codepoints
        .iter()
        .map(|codepoint| (*codepoint, codepoint_identity(&body, &mut atlas, *codepoint)))
        .collect()
}

/// The identity of the real tile `stack` draws `codepoint` with, or `None`.
fn codepoint_identity(
    stack: &sonicterm_engine::FontStack,
    atlas: &mut GlyphAtlas,
    codepoint: char,
) -> Option<TileIdentity> {
    let shaped = stack.shape_text_with_style(&codepoint.to_string(), false, false).ok()?;
    let glyph = shaped.iter().find(|glyph| glyph.glyph_pos != 0)?;
    let key = GlyphKey::shaped(
        codepoint,
        u8::try_from(glyph.font_idx).ok()?,
        glyph.glyph_pos,
        false,
        false,
    );
    let mut raster = stack.clone();
    // Counted like every insertion; outside a frame's counting scope this records nothing.
    let info = atlas.get_or_insert(key, &mut CountingRasterizer::new(&mut raster))?;
    if info.missing || info.oversize {
        // When: info.missing or info.oversize, the tile drew nothing, so the codepoint has no real tile.
        return None;
    }
    tile_identity(stack, key, &info)
}

/// Every representative codepoint the renderer's resident tiles do not hold as a real raster, with
/// why: `no real glyph` (it resolved to no tile identity), `not resident`, or `no pixels`.
#[must_use]
pub fn representative_gaps(
    resolved: &[(char, Option<TileIdentity>)],
    resident: &HashMap<TileIdentity, [u32; 2]>,
) -> Vec<(char, &'static str)> {
    resolved
        .iter()
        .filter_map(|(codepoint, identity)| {
            let Some(identity) = identity else {
                // When: identity is None, the codepoint resolved to no real glyph, which is a gap.
                return Some((*codepoint, "no real glyph"));
            };
            match resident.get(identity) {
                None => Some((*codepoint, "not resident")),
                Some([width_px, height_px]) if *width_px == 0 || *height_px == 0 => {
                    Some((*codepoint, "no pixels"))
                }
                Some(_) => None,
            }
        })
        .collect()
}

/// Check that `key`, which a layout or the fast path required, is resident. A missing tile is the
/// renderer's tofu: a real glyph id that rasterized nothing is listed in `raster_failed`, a notdef
/// key in `unresolved_chars`, and a tile too large to place in `oversize_required`; each is an
/// incomplete required tile. A raster failure is resolved through `stack`, the stack that drew it,
/// so a reviewed exception can name its face. An absent tile is only allowed once the atlas evicted.
fn account_tile(
    atlas: &GlyphAtlas,
    stack: &sonicterm_engine::FontStack,
    key: GlyphKey,
    accounted: &mut Accounted,
) -> Result<(), WorkingSetError> {
    match atlas.get(key) {
        Some(info) if info.missing && key.glyph_id != 0 => {
            let failure = raster_failure(stack, key, &mut accounted.face_contents);
            accounted.raster_failed.insert(key, failure);
            Ok(())
        }
        Some(info) if info.missing => {
            accounted.unresolved_chars.insert(key.ch);
            Ok(())
        }
        Some(info) if info.oversize => {
            accounted.oversize_required.insert(key);
            Ok(())
        }
        Some(_) => Ok(()),
        // An evicting atlas reports `evicted` as its fit, so the lost tile cannot hide.
        None if atlas.evictions() > 0 => Ok(()),
        None => Err(WorkingSetError::NotPlaced { key }),
    }
}

/// `key`'s raster failure: its character, role and style, and the face content, index, glyph id and
/// strike `stack` resolves it to, with the face file's name for reading. It has no face when the key
/// resolves to none or the face's bytes cannot be read, so no exception can approve it.
fn raster_failure(
    stack: &sonicterm_engine::FontStack,
    key: GlyphKey,
    face_contents: &mut HashMap<sonicterm_engine::FaceIdentity, Option<String>>,
) -> RasterFailure {
    let face = stack.resolved_glyph_face(key).and_then(|resolved| {
        let content = face_contents
            .entry(resolved.face.clone())
            .or_insert_with(|| stack.resolved_face_content(key))
            .clone()?;
        Some(FailedFace {
            file: face_file_name(&resolved.face.source),
            content,
            face_index: resolved.face.face_index,
            glyph_id: resolved.glyph_id,
            strike_px_milli: resolved.strike_px_milli,
        })
    });
    RasterFailure {
        codepoint: key.ch,
        role: key.raster_variant,
        bold: key.weight_bold,
        italic: key.italic,
        face,
    }
}

/// A face source's file name, its last component on either host's path separators, for reading a
/// failure; a `builtin:` or `memory:` name has none and is kept whole. Matching uses the content.
fn face_file_name(source: &str) -> String {
    source.rsplit(['/', '\\']).next().unwrap_or(source).to_owned()
}

#[cfg(test)]
#[path = "glyph_working_set_tests.rs"]
mod glyph_working_set_tests;

/// The alpha census of one resident colour tile, read from the atlas's own pixels: how many of its
/// pixels are transparent (alpha 0), translucent (strictly between 0 and 255, an antialiased
/// edge) and opaque (255).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColourTileAlpha {
    /// The tile's atlas key.
    pub key: GlyphKey,
    /// Pixels with alpha 0.
    pub transparent: u64,
    /// Pixels with alpha strictly between 0 and 255.
    pub translucent: u64,
    /// Pixels with alpha 255.
    pub opaque: u64,
}

/// The alpha census of `character`'s largest resident colour tile in `atlas`, or `None` when the
/// atlas holds no colour tile for it. A coverage tile of the same character is not counted.
#[must_use]
pub fn colour_tile_alpha(atlas: &GlyphAtlas, character: char) -> Option<ColourTileAlpha> {
    let (width_px, height_px) = (atlas.width() as f32, atlas.height() as f32);
    atlas
        .resident_tile_keys()
        .into_iter()
        .filter(|key| key.ch == character)
        .filter_map(|key| atlas.get(key).filter(|info| info.is_color).map(|info| (key, info)))
        .max_by_key(|(_, info)| u64::from(info.px_size[0]) * u64::from(info.px_size[1]))
        .map(|(key, info)| {
            // UVs are exact divisions of the atlas size, so rounding recovers the tile's origin.
            let left = (info.uv[0] * width_px).round() as u32;
            let top = (info.uv[1] * height_px).round() as u32;
            let mut census = ColourTileAlpha { key, transparent: 0, translucent: 0, opaque: 0 };
            for pixel_y in top..top + info.px_size[1] {
                for pixel_x in left..left + info.px_size[0] {
                    match atlas.sample(pixel_x, pixel_y) {
                        0 => census.transparent += 1,
                        u8::MAX => census.opaque += 1,
                        _ => census.translucent += 1,
                    }
                }
            }
            census
        })
}
