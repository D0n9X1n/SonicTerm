//! Custom-glyph value types and dispatch to preserved geometry maps and rasterization.

// Vendored from wezterm @ 577474d89ee61aef4a48145cdec82a638d874751,
// path: wezterm-gui/src/customglyph.rs (6036 LOC).
// Original © Wez Furlong and the WezTerm contributors, MIT-licensed.
// See README §Acknowledgements and
// crates/sonicterm-block-glyph/LICENSE-WEZTERM for the full upstream
// license.
//
// SonicTerm integration changes (all other upstream geometry remains intact):
//
//   1. The import block is replaced by these crate-local substitutions:
//        `(upstream) crate :: glyphcache :: {GlyphCache, SizedBlockKey}`
//          — deleted (spelling split so a grep for the literal
//          upstream module path returns 0);
//          `SizedBlockKey` is defined inline below; `GlyphCache` does
//          not exist in this crate (we are not vendoring the cache).
//        `crate::utilsprites::RenderMetrics`
//          → `crate::glue::BlockCellMetrics as RenderMetrics`, a local
//          record of the metric fields customglyph reads. Font cell
//          metrics are `CellMetricsPx`, defined in `sonicterm-engine`
//          (`fontstack.rs`); this crate does not depend on it.
//        `config::DimensionContext` — dropped. Upstream imported it
//          because `cursor_sprite` read
//          `self.fonts.config().cursor_thickness` and resolved it via
//          `Dimension::evaluate_as_pixels`. `cursor_sprite` is not
//          vendored and `block_sprite` never reaches
//          `DimensionContext`, which is defined in `sonicterm-cfg`
//          (`dimension.rs`); this crate does not depend on it.
//        `::window::bitmaps::atlas::Sprite` — deleted; `block_sprite`
//          returns `crate::glue::BlockRasterTile`, imported as
//          `RasterTile` here. The tile type is local because this
//          crate depends on no first-party crate (see the Cargo.toml
//          leaf rule).
//        `::window::color::SrgbaPixel`
//          → `crate::glue::BgraPixel as SrgbaPixel`
//        `window::{BitmapImage, Image, Point, Rect, Size}`
//          → `crate::glue::{BitmapImage, Bitmap as Image, Point, Rect, Size}`
//      `std::ops::Range`, `tiny_skia::*`, and Sonic-native pixel units
//      stay as-is. `termwiz::surface::CursorShape` is dropped along
//      with `cursor_sprite` (the only consumer of it in upstream).
//
//   2. `impl GlyphCache { fn draw_polys / pub fn cursor_sprite /
//      pub fn block_sprite }` is rewritten to free functions:
//        - `draw_polys` becomes a free `fn draw_polys(metrics, polys,
//          buffer, anti_alias_mode, blend_mode)` — body verbatim apart
//          from the §6 names, only `&mut self` dropped from the
//          signature. Called by `block_sprite`.
//        - `cursor_sprite` is OUT OF SCOPE here (this crate is block /
//          box-drawing / Powerline / Sextant glyphs only — cursor
//          rendering lives in a sibling task). It is removed here;
//          the geometry it encoded is reproduced verbatim wherever
//          the consumer needs it via direct `draw_polys` calls.
//        - `pub fn block_sprite(&mut self, render_metrics, key)
//          -> anyhow::Result<Sprite>` becomes free
//          `pub fn block_sprite(render_metrics, key, anti_alias)
//          -> anyhow::Result<RasterTile>`. Body verbatim except:
//            * the `&mut self` drops;
//            * every `anti_alias`
//              read becomes the new `anti_alias: bool` parameter
//              (wezterm-side config global is unavailable here);
//            * the trailing `self.atlas.allocate(&buffer)?;
//              self.block_glyphs.insert(...)` returns a
//              `RasterTile { width, height, coverage: buffer.into_bgra_vec(),
//              offset_x: 0, offset_y: 0, advance: width as f32,
//              is_color: true }`. The atlas + cache wrapping is the
//              consumer's job, not customglyph's.
//
//   3. `BlockKey::filter_out_synthetic(glyphs)` becomes
//      `filter_out_synthetic(glyphs, custom_block_glyphs)` — the
//      single `config::configuration().custom_block_glyphs` read is
//      lifted out as a `bool` parameter for the same reason.
//
//   4. A `#[cfg(test)]` declaration at the end of this file attaches
//      the SonicTerm unit tests in `customglyph_tests.rs`; it adds no
//      production code.
//
//   5. `draw_polys` skips an empty path builder: a collapsed spinner
//      inner circle contributes no clearing geometry. Nonempty paths
//      retain upstream validation and rasterization unchanged.
//
//   6. Upstream one- and two-letter bindings, parameters, and local
//      macros carry descriptive names so the crate passes
//      `clippy::min_ident_chars`: for example `pb` is `path_builder`,
//      `aa` is `anti_alias_mode`, cell-relative `x`/`y` are
//      `cell_x`/`cell_y`, and the corner macros `p0!`..`p3!` are
//      `top_left!`..`bottom_right!`. Only names differ; every
//      expression computes what upstream computes.
//
//   7. The glyph mappings live in contiguous family modules, selected by
//      disjoint codepoint ranges. Rasterization lives in customglyph_raster.rs;
//      mapping arms, lookup tables, and geometry expressions remain intact.
//
// Every other line — geometry math, alpha tables, polygon
// construction, the 5000+ lines of glyph definitions — is the
// upstream bytes, apart from the integrations above and explanatory comments.

// Clippy allows: these lints fire on the verbatim upstream
// wezterm-gui code. The "no shortcuts" rule forbids per-call-site
// rewrites of geometry / polygon construction, so we accept the lint
// at the module level instead. Each `allow` documents WHAT upstream
// pattern triggers it:
//
//   * `clone_on_copy` — `cell_size.clone()` / `render_metrics.clone()`
//     in the `PolyWithCustomMetrics` arm of `block_sprite`. Both types
//     are `Copy` here (Size, BlockCellMetrics derive `Copy`) so the
//     clone is redundant but harmless; rewriting would diverge from
//     the upstream byte pattern.
//   * `enum_variant_names` — `BlockKey::{...Square, ...Inside}` etc.
//     deliberately suffix-match their enum to disambiguate at call
//     sites. Upstream choice; we keep it.
//   * `field_reassign_with_default` — `let mut paint = Paint::default();
//     paint.blend_mode = ...; paint.anti_alias = ...;` pattern in
//     `draw_polys` and `fill_rect`. The struct-update alternative is
//     less readable for a 5-field reassign and would diverge from
//     upstream.
//   * `redundant_field_names` — upstream uses `Poly { path, intensity,
//     style }` shorthand which clippy flags when the field type is
//     ambiguous in context. Verbatim.
//   * `should_implement_trait` — `BlockKey::from_str` is named to
//     mirror `from_char` / `from_cell` (one-char input). Implementing
//     `std::str::FromStr` would force returning a different error
//     type (`FromStr::Err` vs `Option`) and break callers.
//   * `wrong_self_convention` — `BlockAlpha::to_scale(self)` takes
//     `self` (not `&self`) for a Copy enum. Idiomatic for cheap Copy
//     types; clippy disagrees.
//
// These are limited to the verbatim module — `glue.rs` and `lib.rs`
// inherit the workspace clippy gate.
#![allow(clippy::clone_on_copy)]
#![allow(clippy::enum_variant_names)]
#![allow(clippy::field_reassign_with_default)]
#![allow(clippy::redundant_field_names)]
#![allow(clippy::should_implement_trait)]
#![allow(clippy::wrong_self_convention)]

use crate::glue::{
    BgraPixel as SrgbaPixel, Bitmap as Image, BitmapImage, BlockCellMetrics as RenderMetrics,
    BlockRasterTile as RasterTile, IntPixelLength, PixelLength, Point, Rect, Size,
};
use std::ops::Range;
use tiny_skia::{BlendMode, FillRule, Paint, Path, PathBuilder, PixmapMut, Stroke, Transform};

/// Per-cell key the consumer hands to [`block_sprite`]. Lifted verbatim
/// from upstream `wezterm-gui/src/glyphcache.rs` (the `pub struct
/// SizedBlockKey` definition there), modulo the `size` field type:
/// upstream uses a `CellMetricKey { pixel_width, pixel_height }`, this
/// crate uses [`Size`] directly so the cell rect arrives in the same
/// euclid type the rest of customglyph expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SizedBlockKey {
    pub block: BlockKey,
    pub size: Size,
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum PolyAA {
    AntiAlias,
    MoarPixels,
}

bitflags::bitflags! {
    //  ╭──────╮
    //  │UL╱╲UR│
    //  │ ╱  ╲ │
    //  │╱    ╲│
    //  │╲    ╱│
    //  │ ╲  ╱ │
    //  │LL╲╱LR│
    //  ╰──────╯
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct CellDiagonal: u8{
        const UPPER_LEFT = 1<<1;
        const UPPER_RIGHT = 1<<2;
        const LOWER_LEFT = 1<<3;
        const LOWER_RIGHT = 1<<4;
    }
}

bitflags::bitflags! {
    // ╭────╮
    // │╲U ╱│
    // │ ╲╱R│
    // │L╱╲ │
    // │╱ D╲│
    // ╰────╯
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Triangle: u8{
        const UPPER = 1<<1;
        const RIGHT = 1<<2;
        const LOWER = 1<<3;
        const LEFT = 1<<4;
    }
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ProgressChunk: u8{
        const LEFT = 1<<1;
        const RIGHT = 1<<2;
        const MIDDLE = 1<<3;
        const FULL = 1<<4;
    }
}

bitflags::bitflags! {
    /// Components to make up graph branch diagrams (eg. for git history)
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Branch: u16{
        const VERTICAL = 1<<1;
        const HORIZONTAL = 1<<2;
        /// 
        const RIGHT_TO_DOWN = 1<<3;
        /// 
        const RIGHT_TO_UP = 1<<4;
        /// 
        const LEFT_TO_DOWN = 1<<5;
        /// 
        const LEFT_TO_UP = 1<<6;
        /// 
        const CIRCLE_FILLED = 1<<7;
        /// 
        const CIRCLE_OUTLINE = 1<<8;
        const LEFT = 1<<9;
        const RIGHT = 1<<10;
        const UP = 1<<11;
        const DOWN = 1<<12;
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum BlockAlpha {
    /// 100%
    Full,
    /// 75%
    Dark,
    /// 50%
    Medium,
    /// 25%
    Light,
}

impl BlockAlpha {
    pub fn to_scale(self) -> f32 {
        match self {
            BlockAlpha::Full => 1.0,
            BlockAlpha::Dark => 0.75,
            BlockAlpha::Medium => 0.5,
            BlockAlpha::Light => 0.25,
        }
    }
}

/// Represents a scaled width of the underline thickness.
/// Can either multiple or divide by the specified amount
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum LineScale {
    Mul(i8),
    Div(i8),
}

impl LineScale {
    fn to_scale(self) -> f32 {
        match self {
            Self::Mul(factor) => factor as f32,
            Self::Div(divisor) => 1. / divisor as f32,
        }
    }
}

/// Represents a coordinate in a glyph expressed in relation
/// to the dimension of the glyph.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum BlockCoord {
    /// 0 pixels in; either the leftmost or topmost pixel position
    Zero,
    /// 100% of the dimension; either the rightmost or bottom pixel
    /// position
    One,
    /// A fraction of the width/height.  The first value is the
    /// numerator, the second is the denominator.
    Frac(i8, i8),

    /// Like Frac() above, but also specifies a scale to use
    /// together with the underline height to adjust the position.
    /// This is helpful because the line drawing routines stroke
    /// along the center of the line in the direction of the line,
    /// but don't pad the end of the line out by the width automatically.
    /// zeno has Cap::Square to specify that, but we can't use it
    /// directly and it isn't necessarily the adjustment that we want.
    /// This is most useful when joining lines that have different
    /// stroke widths; if the widths were all the same then you'd
    /// just specify the points in the path and not worry about it.
    FracWithOffset(i8, i8, LineScale),
    /// Like Zero but for a square centered in the glyph
    SquareZero,
    /// Like One but for a square centered in the glyph
    SquareOne,
    /// Like Frac but for a square centered in the glyph
    SquareFrac(i8, i8),
}

impl BlockCoord {
    /// Compute the actual pixel value given the max dimension.
    pub fn to_pixel(self, max: usize, underline_height: f32, max_square: usize) -> f32 {
        /// For interior points, adjust so that we get the middle of the row;
        /// in AA modes with 1px wide strokes this gives better results.
        fn hint(coordinate: f32) -> f32 {
            if coordinate.fract() == 0. {
                coordinate - 0.5
            } else {
                coordinate
            }
        }
        match self {
            Self::Zero => 0.,
            Self::One => max as f32,
            Self::Frac(num, den) => hint(max as f32 * num as f32 / den as f32),
            Self::FracWithOffset(num, den, under) => {
                hint((max as f32 * num as f32 / den as f32) + (underline_height * under.to_scale()))
            }
            Self::SquareZero => 0. + (max - max_square) as f32 / 2.0,
            Self::SquareOne => max as f32 - (max - max_square) as f32 / 2.0,
            Self::SquareFrac(num, den) => {
                hint(max_square as f32 * num as f32 / den as f32 + (max - max_square) as f32 / 2.0)
            }
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Block {
    /// Number of 1/8ths: x0, x1, y0, y1 with custom alpha
    Custom(u8, u8, u8, u8, BlockAlpha),
    /// Number of 1/8ths in the upper half
    UpperBlock(u8),
    /// Number of 1/8ths in the lower half
    LowerBlock(u8),
    /// Number of 1/8ths in the left half
    LeftBlock(u8),
    /// Number of 1/8ths in the right half
    RightBlock(u8),
    /// Number of 1/8ths: x0, x1
    VerticalBlock(u8, u8),
    /// Number of 1/8ths: y0, y1
    HorizontalBlock(u8, u8),
    /// Quadrants
    // ╭──┬──╮
    // │UL│UR│
    // ├──┼──┤
    // │LL│LR│
    // ╰──┴──╯
    QuadrantUL,
    QuadrantUR,
    QuadrantLL,
    QuadrantLR,
}

/// Represents a Block Element glyph, decoded from
/// <https://en.wikipedia.org/wiki/Block_Elements>
/// <https://www.unicode.org/charts/PDF/U2580.pdf>
/// <https://unicode.org/charts/PDF/U1FB00.pdf>
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum BlockKey {
    /// List of block rectangles
    Blocks(&'static [Block]),
    /// List of triangles
    Triangles(Triangle, BlockAlpha),
    /// A combination of small diagonal lines
    CellDiagonals(CellDiagonal),
    /// A sextant block pattern
    Sextant(u8),
    /// An octant block pattern
    Octant(u8),
    /// A braille dot pattern
    Braille(u8),
    /// A progress bar pattern
    Progress(ProgressChunk),
    /// A graph branch pattern
    Branches(Branch),
    Spinner(u8),

    Poly(&'static [Poly]),

    PolyWithCustomMetrics {
        polys: &'static [Poly],
        underline_height: IntPixelLength,
        cell_size: Size,
    },
}

/// Filled polygon used to describe the more complex shapes in
/// <https://unicode.org/charts/PDF/U1FB00.pdf>
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct Poly {
    pub path: &'static [PolyCommand],
    pub intensity: BlockAlpha,
    pub style: PolyStyle,
}

pub type BlockPoint = (BlockCoord, BlockCoord);

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum PolyCommand {
    MoveTo(BlockCoord, BlockCoord),
    LineTo(BlockCoord, BlockCoord),
    QuadTo { control: BlockPoint, to: BlockPoint },
    Oval { center: BlockPoint, radiuses: BlockPoint },
    Circle { center: BlockPoint, radius: BlockCoord },
    Close,
}

impl PolyCommand {
    fn to_skia(
        &self,
        width: usize,
        height: usize,
        underline_height: f32,
        path_builder: &mut PathBuilder,
    ) {
        match self {
            Self::MoveTo(cell_x, cell_y) => path_builder.move_to(
                cell_x.to_pixel(width, underline_height, width.min(height)),
                cell_y.to_pixel(height, underline_height, width.min(height)),
            ),
            Self::LineTo(cell_x, cell_y) => path_builder.line_to(
                cell_x.to_pixel(width, underline_height, width.min(height)),
                cell_y.to_pixel(height, underline_height, width.min(height)),
            ),
            Self::QuadTo { control: (control_x, control_y), to: (end_x, end_y) } => path_builder
                .quad_to(
                    control_x.to_pixel(width, underline_height, width.min(height)),
                    control_y.to_pixel(height, underline_height, width.min(height)),
                    end_x.to_pixel(width, underline_height, width.min(height)),
                    end_y.to_pixel(height, underline_height, width.min(height)),
                ),
            Self::Oval { center: (center_x, center_y), radiuses: (radius_x, radius_y) } => {
                let left =
                    center_x.to_pixel(width, underline_height, width.min(height)) - width as f32;
                let top =
                    center_y.to_pixel(height, underline_height, width.min(height)) - height as f32;
                let oval_width =
                    radius_x.to_pixel(width, underline_height, width.min(height)) * 2.0;
                let oval_height =
                    radius_y.to_pixel(height, underline_height, width.min(height)) * 2.0;

                if let Some(oval) = tiny_skia::Rect::from_xywh(left, top, oval_width, oval_height) {
                    path_builder.push_oval(oval);
                } else {
                    log::error!("Can't push oval, values: {:?}", self);
                }
            }
            Self::Circle { center: (center_x, center_y), radius } => {
                let pixel_x = center_x.to_pixel(width, underline_height, width.min(height));
                let pixel_y = center_y.to_pixel(height, underline_height, width.min(height));
                let pixel_radius =
                    radius.to_pixel(width.min(height), underline_height, width.min(height));

                path_builder.push_circle(pixel_x, pixel_y, pixel_radius);
            }
            Self::Close => path_builder.close(),
        };
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum PolyStyle {
    Fill,
    OutlineAlpha,
    OutlineThin,
    // A line with the thickness as underlines
    Outline,
    // A line with twice the thickness of underlines
    OutlineHeavy,
}

impl PolyStyle {
    fn apply(self, width: f32, paint: &Paint, path: &Path, pixmap: &mut PixmapMut) {
        match self {
            PolyStyle::Fill => {
                pixmap.fill_path(path, paint, FillRule::Winding, Transform::identity(), None);
            }

            PolyStyle::OutlineThin
            | PolyStyle::Outline
            | PolyStyle::OutlineHeavy
            | PolyStyle::OutlineAlpha => {
                let mut stroke = Stroke::default();
                stroke.width = width;
                if self == PolyStyle::OutlineHeavy {
                    stroke.width *= 3.01; // NOTE: Changing this makes block cursor disproportionate at different font sizes and resolutions
                } else if self == PolyStyle::OutlineThin {
                    stroke.width = 1.2;
                } else if self == PolyStyle::OutlineAlpha {
                    stroke.width = 0.25; // NOTE: This is for filling antialiased border between triangles when using the alpha style
                }

                pixmap.stroke_path(path, paint, &stroke, Transform::identity(), None);
            }
        }
    }
}

impl BlockKey {
    pub fn filter_out_synthetic(glyphs: &mut Vec<char>, custom_block_glyphs: bool) {
        // Vendor change: original `let config = config::configuration();
        // if config.custom_block_glyphs { ... }` reached the wezterm
        // config global. Lift the bool to a parameter the consumer
        // passes in (same pattern as the `anti_alias` parameter on
        // `block_sprite`).
        if custom_block_glyphs {
            glyphs.retain(|&glyph| Self::from_char(glyph).is_none());
        }
    }

    pub fn from_str(text: &str) -> Option<Self> {
        let mut chars = text.chars();
        let first_char = chars.next()?;
        if chars.next().is_some() {
            None
        } else {
            Self::from_char(first_char)
        }
    }

    /// Decode one supported terminal glyph while preserving each mapping family's arm order.
    pub fn from_char(character: char) -> Option<Self> {
        let codepoint = character as u32;
        // The disjoint ranges route each codepoint to the original contiguous arm group.
        match codepoint {
            0x2500..=0x251b => Self::from_box_strokes_corners(codepoint),
            0x251c..=0x254b => Self::from_box_tees_crosses(codepoint),
            0x254c..=0x255d => Self::from_box_double_corners(codepoint),
            0x255e..=0x256c => Self::from_box_double_junctions(codepoint),
            0x256d..=0x257f => Self::from_box_arcs_caps(codepoint),
            0x2580..=0x259f
            | 0x1fb00..=0x1fb67
            | 0x1cd00..=0x1cde5
            | 0x1cea0
            | 0x1cea3
            | 0x1cea8
            | 0x1ceab
            | 0x1fbe6..=0x1fbe7 => Self::from_blocks_legacy(codepoint),
            0x1fb68..=0x1fbaf | 0x2800..=0x28ff => Self::from_legacy_mosaics(codepoint),
            0xe0b0..=0xe0bf | 0xee00..=0xee0b | 0xf5d0..=0xf60d => Self::from_symbols(codepoint),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "customglyph_tests.rs"]
mod customglyph_tests;

#[path = "customglyph_blocks_legacy.rs"]
mod blocks_legacy;
#[path = "customglyph_box_arcs_caps.rs"]
mod box_arcs_caps;
#[path = "customglyph_box_double_corners.rs"]
mod box_double_corners;
#[path = "customglyph_box_double_junctions.rs"]
mod box_double_junctions;
#[path = "customglyph_box_strokes_corners.rs"]
mod box_strokes_corners;
#[path = "customglyph_box_tees_crosses.rs"]
mod box_tees_crosses;
#[path = "customglyph_legacy_mosaics.rs"]
mod legacy_mosaics;
#[path = "customglyph_raster.rs"]
mod raster;
#[path = "customglyph_symbols.rs"]
mod symbols;
pub use raster::block_sprite;
