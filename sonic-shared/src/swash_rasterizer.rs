//! A real [`Rasterizer`] backed by [`swash`], sourcing fonts from the
//! same [`cosmic_text::FontSystem`] the renderer uses to shape tab
//! titles and the search bar.
//!
//! Why share the FontSystem? Two reasons:
//!  1. We already pay to load `assets/fonts/*.ttf` into one fontdb at
//!     startup; loading them a second time into a private swash table
//!     would double the memory cost and add a code-path that could go
//!     out of sync with the glyphon side.
//!  2. The grid uses the SAME family that glyphon resolves for tab
//!     titles. Going through `font_system.db().query()` guarantees the
//!     atlas's tiles match what glyphon would have shaped for the same
//!     character/weight/style.
//!
//! ## Lookup pipeline (per miss)
//! ```text
//! GlyphKey { ch, weight_bold, italic }
//!     │
//!     ▼ fontdb::Query (family + Weight + Style)
//!     │
//!     ▼ FontSystem::get_font(id, weight) -> Arc<cosmic_text::Font>
//!     │
//!     ▼ font.as_swash() -> swash::FontRef
//!     │
//!     ▼ ScaleContext::builder(font).size(px).build()
//!     │
//!     ▼ font.charmap().map(ch) -> glyph_id
//!     │
//!     ▼ Render::new(&[Source::Outline]).format(Alpha).render(...)
//!     │
//!     ▼ swash::Image { content: Mask, placement, data }
//!     │
//!     ▼ RasterTile { width, height, offset_x = placement.left,
//!                    offset_y = -placement.top + ascent,
//!                    coverage = data }
//! ```
//!
//! ## What returns `None`
//! - Family not present in fontdb (lookup failure)
//! - Charmap doesn't have a glyph for `ch` (0 glyph id, no fallback)
//! - swash's Render returns `None` (rare)
//!
//! Callers (the atlas) treat `None` as "blank tile" — the renderer
//! never panics, the character is just invisible. Emoji and fallback
//! fonts are not handled in this PR; see the PR body's "out of scope".

use cosmic_text::FontSystem;
use sonic_core::glyph_key::GlyphKey;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::Format;

use crate::glyph_atlas::{RasterTile, Rasterizer};

/// Default rasterization size in pixels. We bake at this fixed em-size
/// so a single tile per `GlyphKey` is enough — the renderer never
/// resizes the grid font at runtime (that would invalidate the entire
/// atlas anyway). Matches the default font size used by [`crate::render`].
pub const DEFAULT_RASTER_PX: f32 = 14.0;

/// Production [`Rasterizer`] impl. Holds a mutable borrow on the
/// renderer's `FontSystem` and an owned `ScaleContext` (swash's
/// per-thread cache for glyph outlines + hinted bitmaps).
///
/// One instance per renderer; not `Send`/`Sync` and that's fine since
/// rendering is single-threaded.
pub struct SwashRasterizer<'a> {
    font_system: &'a mut FontSystem,
    scale_ctx: ScaleContext,
    family: String,
    px: f32,
}

impl<'a> SwashRasterizer<'a> {
    /// Build a rasterizer that resolves all glyphs to `family` at
    /// `px` em-size. `family` should be the same string the renderer
    /// passes to glyphon's `Family::Name`.
    pub fn new(font_system: &'a mut FontSystem, family: &str, px: f32) -> Self {
        Self { font_system, scale_ctx: ScaleContext::new(), family: family.to_string(), px }
    }

    /// Em-size (px) the rasterizer was constructed with. Exposed for
    /// tests that verify the renderer threads `config.font_size`
    /// through instead of the legacy hardcoded `DEFAULT_RASTER_PX`.
    pub fn px(&self) -> f32 {
        self.px
    }

    /// Font family the rasterizer was constructed with. Companion to
    /// [`Self::px`] for the same renderer-config-honored test.
    pub fn family(&self) -> &str {
        &self.family
    }

    /// Convenience: build at [`DEFAULT_RASTER_PX`] with the bundled
    /// "Rec Mono Casual" family. Used by the test harness.
    pub fn with_default_family(font_system: &'a mut FontSystem) -> Self {
        Self::new(font_system, "Rec Mono Casual", DEFAULT_RASTER_PX)
    }

    /// Look up the font ID for the given (bold, italic) combination,
    /// returning `None` if nothing in the fontdb matches.
    fn lookup_id(&self, weight_bold: bool, italic: bool) -> Option<fontdb::ID> {
        let weight = if weight_bold { fontdb::Weight::BOLD } else { fontdb::Weight::NORMAL };
        let style = if italic { fontdb::Style::Italic } else { fontdb::Style::Normal };
        let families = [fontdb::Family::Name(self.family.as_str()), fontdb::Family::Monospace];
        let query =
            fontdb::Query { families: &families, weight, stretch: fontdb::Stretch::Normal, style };
        self.font_system.db().query(&query)
    }
}

impl<'a> Rasterizer for SwashRasterizer<'a> {
    fn rasterize(&mut self, key: GlyphKey) -> Option<RasterTile> {
        // Whitespace and other zero-pixel chars: short-circuit to an
        // empty tile. The atlas stores a zero-area UV for these and
        // the renderer skips the draw instance — saves an outline
        // scaler build for every blank cell on the screen.
        if key.ch == ' ' || key.ch == '\t' {
            return Some(RasterTile {
                width: 0,
                height: 0,
                offset_x: 0,
                offset_y: 0,
                advance: self.px * 0.6, // approximate; not used on the grid path
                coverage: Vec::new(),
            });
        }

        let weight = if key.weight_bold { fontdb::Weight::BOLD } else { fontdb::Weight::NORMAL };
        let id = self.lookup_id(key.weight_bold, key.italic)?;
        let font = self.font_system.get_font(id, weight)?;
        let swash_font = font.as_swash();
        let glyph_id = swash_font.charmap().map(key.ch);
        if glyph_id == 0 {
            // No glyph in this face for this codepoint. We could fall
            // back to a sibling family here but emoji / fallback fonts
            // are out of scope for this PR — return None and let the
            // atlas record a blank tile.
            return None;
        }

        let mut scaler = self.scale_ctx.builder(swash_font).size(self.px).hint(true).build();

        let image = Render::new(&[Source::Outline, Source::Bitmap(StrikeWith::BestFit)])
            .format(Format::Alpha)
            .render(&mut scaler, glyph_id)?;

        let p = image.placement;
        if p.width == 0 || p.height == 0 {
            return Some(RasterTile {
                width: 0,
                height: 0,
                offset_x: p.left,
                offset_y: -p.top,
                advance: self.px * 0.6,
                coverage: Vec::new(),
            });
        }

        // The atlas only understands 8-bit alpha coverage; we requested
        // Format::Alpha so `image.data` is exactly that — one byte per
        // pixel, row-major, top-down.
        let mut coverage = image.data;
        let expected = (p.width as usize) * (p.height as usize);
        if coverage.len() != expected {
            // Defensive: if swash ever hands us a different layout, we
            // bail rather than scribble out-of-bounds in the atlas
            // copy_from_slice. Truncate or pad to the expected size.
            coverage.resize(expected, 0);
        }

        Some(RasterTile {
            width: p.width,
            height: p.height,
            offset_x: p.left,
            // swash gives `top` as the distance from the baseline up
            // to the top of the bitmap (positive = above baseline). We
            // want offset relative to the cell-box top — the renderer
            // adds an ascent-based baseline correction when placing
            // the quad, so we just flip the sign here.
            offset_y: -p.top,
            advance: self.px * 0.6,
            coverage,
        })
    }
}
