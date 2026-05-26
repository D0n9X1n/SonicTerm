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
//! ## Font fallback (B3.1, this PR)
//!
//! Before B3.1, the rasterizer queried a single family (default
//! "Rec Mono Casual") and returned `None` for any codepoint that face
//! lacked — every CJK character, emoji, and most accented letters
//! rendered as a tofu box. Glyphon (the pre-B3 path) had this for free
//! via cosmic-text's `Buffer` shaping; the atlas path lost it.
//!
//! We now hold a **fallback chain**: an ordered list of family names
//! built from the user's configured `font_family` plus a platform-
//! specific tail. On a miss we walk the chain in order and rasterize
//! through the first face whose `charmap` has the codepoint.
//!
//! Per-codepoint resolution is cached in `slot_cache` so the second
//! occurrence of '中' doesn't re-walk the chain. The resolved slot is
//! also baked into the [`GlyphKey`] before it reaches the atlas —
//! without this, two cells with the same char/style but resolved by
//! different fonts would collide in the atlas's `HashMap`.
//!
//! ## What still returns `None`
//! - Every face in the chain lacks the codepoint (true tofu — caller
//!   draws the missing-glyph outline box)
//! - swash's `Render` returns `None` for a valid glyph id (rare)

use cosmic_text::FontSystem;
use sonic_core::glyph_key::GlyphKey;
use std::collections::HashMap;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::Format;

use crate::glyph_atlas::{RasterTile, Rasterizer};

/// Default rasterization size in pixels. We bake at this fixed em-size
/// so a single tile per `GlyphKey` is enough — the renderer never
/// resizes the grid font at runtime (that would invalidate the entire
/// atlas anyway). Matches the default font size used by [`crate::render`].
pub const DEFAULT_RASTER_PX: f32 = 14.0;

/// Platform-specific tail appended after the user's primary family. The
/// chain is walked in order, so put the most-commonly-needed CJK face
/// first, then the emoji face.
///
/// macOS: PingFang SC ships with the OS and covers Simplified Chinese,
/// Traditional Chinese, Japanese kana, Korean Hangul (via the broader
/// PingFang family fontdb tends to resolve). Hiragino is a strong
/// secondary for Japanese-only. Apple Color Emoji covers emoji.
///
/// Windows: Microsoft YaHei (Simplified Chinese + most CJK), MS Gothic
/// (Japanese), Malgun Gothic (Korean), Segoe UI Emoji (emoji).
///
/// Other (Linux/CI): Noto family. Tests don't depend on these resolving,
/// but the chain shouldn't be empty.
#[cfg(target_os = "macos")]
const PLATFORM_FALLBACK_CHAIN: &[&str] = &[
    "PingFang SC",
    "Hiragino Sans GB",
    "Apple SD Gothic Neo",
    "Symbols Nerd Font Mono",
    "Apple Color Emoji",
];
#[cfg(target_os = "windows")]
const PLATFORM_FALLBACK_CHAIN: &[&str] =
    &["Microsoft YaHei", "MS Gothic", "Malgun Gothic", "Symbols Nerd Font Mono", "Segoe UI Emoji"];
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const PLATFORM_FALLBACK_CHAIN: &[&str] = &[
    "Noto Sans CJK SC",
    "Noto Sans CJK JP",
    "Noto Sans CJK KR",
    "Symbols Nerd Font Mono",
    "Noto Color Emoji",
];

/// Maximum number of families in the fallback chain. One byte in the
/// `GlyphKey` is plenty; we also keep an end-of-chain sentinel below
/// for cells the entire chain can't satisfy.
pub const MAX_FALLBACK_SLOTS: u8 = 8;

/// Production [`Rasterizer`] impl. Holds a mutable borrow on the
/// renderer's `FontSystem` and an owned `ScaleContext` (swash's
/// per-thread cache for glyph outlines + hinted bitmaps).
///
/// One instance per renderer; not `Send`/`Sync` and that's fine since
/// rendering is single-threaded.
pub struct SwashRasterizer<'a> {
    font_system: &'a mut FontSystem,
    scale_ctx: ScaleContext,
    /// Fallback chain. Slot 0 is the user's configured primary family;
    /// slots 1..N are the platform fallback chain. We cap at
    /// `MAX_FALLBACK_SLOTS` entries; configured + platform usually fits
    /// in 4–5.
    families: Vec<String>,
    px: f32,
    /// Memoizes which slot in `families` claims a given (char,
    /// weight_bold, italic). Lets the second hit on '中' skip the
    /// charmap walk. Capped only by the working set of distinct
    /// codepoints rendered.
    slot_cache: HashMap<(char, bool, bool), Option<u8>>,
}

impl<'a> SwashRasterizer<'a> {
    /// Build a rasterizer with `family` as the primary face, followed
    /// by the platform fallback chain. `px` is the em-size every
    /// resolved face will be scaled to.
    pub fn new(font_system: &'a mut FontSystem, family: &str, px: f32) -> Self {
        let mut families: Vec<String> = Vec::with_capacity(1 + PLATFORM_FALLBACK_CHAIN.len());
        families.push(family.to_string());
        for f in PLATFORM_FALLBACK_CHAIN {
            // Dedup the primary if a user set their main font to one of
            // the platform CJK faces.
            if families.iter().any(|existing| existing.eq_ignore_ascii_case(f)) {
                continue;
            }
            if families.len() >= MAX_FALLBACK_SLOTS as usize {
                break;
            }
            families.push((*f).to_string());
        }
        Self {
            font_system,
            scale_ctx: ScaleContext::new(),
            families,
            px,
            slot_cache: HashMap::new(),
        }
    }

    /// Em-size (px) the rasterizer was constructed with.
    pub fn px(&self) -> f32 {
        self.px
    }

    /// Primary family name (slot 0). Companion to `px` for the
    /// renderer-config-honored test.
    pub fn family(&self) -> &str {
        &self.families[0]
    }

    /// Full fallback chain in resolution order. Exposed for tests
    /// asserting the platform tail is wired correctly.
    pub fn families(&self) -> &[String] {
        &self.families
    }

    /// Convenience: build at [`DEFAULT_RASTER_PX`] with the bundled
    /// "Rec Mono Casual" family. Used by the test harness.
    pub fn with_default_family(font_system: &'a mut FontSystem) -> Self {
        Self::new(font_system, "Rec Mono Casual", DEFAULT_RASTER_PX)
    }

    /// Look up the fontdb ID for `family` at the given (bold, italic)
    /// combination, returning `None` if nothing in the fontdb matches.
    fn lookup_id(&self, family: &str, weight_bold: bool, italic: bool) -> Option<fontdb::ID> {
        let weight = if weight_bold { fontdb::Weight::BOLD } else { fontdb::Weight::NORMAL };
        let style = if italic { fontdb::Style::Italic } else { fontdb::Style::Normal };
        // Only ask fontdb for `Name(family)` — no Monospace tail here,
        // otherwise the lookup for a CJK family on a system without it
        // would silently substitute the default monospace and shadow
        // a real fallback in the next slot.
        let families = [fontdb::Family::Name(family)];
        let query =
            fontdb::Query { families: &families, weight, stretch: fontdb::Stretch::Normal, style };
        self.font_system.db().query(&query)
    }

    /// Walk the fallback chain and return the first slot whose face
    /// has a non-zero glyph for `ch`. Memoized per (ch, bold, italic).
    /// Returns `None` only if every face in the chain returns a zero
    /// glyph id (true tofu).
    pub fn resolve_slot(&mut self, ch: char, weight_bold: bool, italic: bool) -> Option<u8> {
        if let Some(slot) = self.slot_cache.get(&(ch, weight_bold, italic)) {
            return *slot;
        }
        let weight = if weight_bold { fontdb::Weight::BOLD } else { fontdb::Weight::NORMAL };
        let mut found: Option<u8> = None;
        for (idx, family) in self.families.iter().enumerate() {
            let Some(id) = self.lookup_id(family, weight_bold, italic) else { continue };
            let Some(font) = self.font_system.get_font(id, weight) else { continue };
            let swash_font = font.as_swash();
            if swash_font.charmap().map(ch) != 0 {
                found = Some(idx as u8);
                break;
            }
        }
        self.slot_cache.insert((ch, weight_bold, italic), found);
        found
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
                advance: self.px * 0.6,
                coverage: Vec::new(),
                is_color: false,
            });
        }

        let weight = if key.weight_bold { fontdb::Weight::BOLD } else { fontdb::Weight::NORMAL };

        // Use the slot pinned in the key. The renderer is expected to
        // have called `resolve_slot` first; if it didn't (e.g. tests
        // built a key with `new(..)` which defaults to slot 0), we
        // still try slot 0 and fall back to chain-walking on a charmap
        // miss so the rasterizer stays usable standalone.
        let slot = key.font_slot as usize;
        let family = self.families.get(slot)?;
        let id = self.lookup_id(family, key.weight_bold, key.italic)?;
        let font = self.font_system.get_font(id, weight)?;
        let swash_font = font.as_swash();
        let glyph_id = swash_font.charmap().map(key.ch);
        if glyph_id == 0 {
            // The slot the caller pinned doesn't have this glyph. If
            // the caller is the renderer, they will have already
            // resolved the right slot via `resolve_slot`, so this
            // branch is mainly for the bench/test path that builds a
            // GlyphKey with slot=0 and expects a sensible answer.
            if slot == 0 {
                if let Some(resolved) = self.resolve_slot(key.ch, key.weight_bold, key.italic) {
                    if resolved != 0 {
                        let retry = key.with_font_slot(resolved);
                        return self.rasterize(retry);
                    }
                }
            }
            return None;
        }

        let mut scaler = self.scale_ctx.builder(swash_font).size(self.px).hint(true).build();

        // Two-phase render: try color sources first (Subpixel format
        // preserves the BGRA bitmap from sbix/CBDT/COLR strikes). If swash
        // returns Color content, the tile is BGRA premultiplied and the
        // atlas stores it as-is (`is_color = true`). Otherwise re-render
        // with Alpha format from the outline/mono-bitmap sources so we
        // get a proper coverage mask rather than the all-zero alpha
        // channel a color strike emits under Format::Alpha.
        let color_attempt =
            Render::new(&[Source::ColorBitmap(StrikeWith::BestFit), Source::ColorOutline(0)])
                .format(Format::Subpixel)
                .render(&mut scaler, glyph_id);

        if let Some(image) = color_attempt {
            if image.content == swash::scale::image::Content::Color {
                let p = image.placement;
                if p.width == 0 || p.height == 0 {
                    return Some(RasterTile {
                        width: 0,
                        height: 0,
                        offset_x: p.left,
                        offset_y: -p.top,
                        advance: self.px * 0.6,
                        coverage: Vec::new(),
                        is_color: true,
                    });
                }
                let expected = (p.width as usize) * (p.height as usize) * 4;
                let mut data = image.data;
                if data.len() != expected {
                    data.resize(expected, 0);
                }
                return Some(RasterTile {
                    width: p.width,
                    height: p.height,
                    offset_x: p.left,
                    offset_y: -p.top,
                    advance: self.px * 0.6,
                    coverage: data,
                    is_color: true,
                });
            }
        }

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
                is_color: false,
            });
        }

        let mut coverage = image.data;
        let expected = (p.width as usize) * (p.height as usize);
        if coverage.len() != expected {
            coverage.resize(expected, 0);
        }

        Some(RasterTile {
            width: p.width,
            height: p.height,
            offset_x: p.left,
            offset_y: -p.top,
            advance: self.px * 0.6,
            coverage,
            is_color: false,
        })
    }
}
