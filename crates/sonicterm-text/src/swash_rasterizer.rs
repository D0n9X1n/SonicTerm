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
//! "Rec Mono St.Helens") and returned `None` for any codepoint that face
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

use cosmic_text::{fontdb, FontSystem};
use sonicterm_types::GlyphKey;
use std::collections::HashMap;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::Format;
use tracing;

use crate::async_fallback::AsyncFallbackLoader;
use crate::glyph_atlas::{GlyphAtlas, RasterTile, Rasterizer};

/// Unicode ranges whose glyphs we eagerly bake into the atlas at font
/// load. These are the codepoints terminal users hit on first paint of
/// any TUI (htop, btop, vim splits, fzf, tmux status, powerline prompts)
/// — pre-warming them means the *first* frame after launch doesn't pay
/// the font-fallback charmap-walk + swash outline-scale cost for each.
///
/// - `0x2500..=0x259F` — Box Drawing + Block Elements. Covered by the
///   primary Recursive Mono family, so resolution stops at slot 0.
/// - `0xE0A0..=0xE0D7` — Powerline PUA. The bundled `Rec Mono St.Helens`
///   is Nerd-Font-patched and carries these glyphs, so resolution stops
///   at slot 0 without needing a system Nerd Font.
///
/// Codepoints the font chain lacks (returns no glyph) are silently
/// skipped — no atlas entry is created, so a later real use still goes
/// through the regular fallback path. The two ranges combined are ~250
/// codepoints, comfortably under the 16k-tile atlas budget even at 2×.
pub const PREBAKE_RANGES: &[std::ops::RangeInclusive<u32>] = &[0x2500..=0x259F, 0xE0A0..=0xE0D7];

/// Powerline "Symbols" PUA block (U+E0B0..=U+E0BF) — the cell-filling
/// separators (left/right arrow, half/full triangle, etc.) used by every
/// powerline-style shell prompt (oh-my-zsh agnoster, p10k, starship).
///
/// These glyphs are intentionally designed to paint the entire cell
/// rectangle — the arrow's diagonal must meet the cell's edge exactly so
/// adjacent arrows on stacked rows form a continuous "tab" shape. They
/// MUST be anchored to the cell rect, never to the text baseline:
///
///   * Baseline anchoring drifts because `placement.top` differs across
///     glyphs in the range (U+E0B0 is full-bleed; U+E0B1 has thin
///     stroke at different y). A row of arrows then sits at multiple
///     vertical positions — visually one row "high", the next "low or
///     missing" (the user-reported regression).
///   * Cell-rect anchoring guarantees every powerline glyph paints at
///     exactly (cell_x, cell_y, cell_w, cell_h) regardless of the
///     resolving font face's metrics. Adjacent rows align by
///     construction.
///
/// See [`anchor_powerline_rect`] for the helper applied at every glyph
/// emit site in the render core.
pub const POWERLINE_PUA_FIRST: u32 = 0xE0B0;
pub const POWERLINE_PUA_LAST: u32 = 0xE0BF;

/// Classify `ch` as a cell-filling Powerline glyph (see
/// [`POWERLINE_PUA_FIRST`] for the rationale). Inline-cheap; used on the
/// per-glyph emit hot path.
#[inline]
pub fn is_powerline_char(ch: char) -> bool {
    let cp = ch as u32;
    (POWERLINE_PUA_FIRST..=POWERLINE_PUA_LAST).contains(&cp)
}

/// Cell-rect anchor for cell-filling glyphs. If `ch` is a Powerline
/// codepoint, returns the exact cell rect `(cx, cy, cell_w, cell_h)`;
/// otherwise returns `natural` unchanged.
///
/// This is the single point of policy referenced from each glyph-emit
/// path in `sonicterm-shared/src/render/core.rs` — keeping it here (in the
/// crate that owns Powerline classification) ensures the policy stays
/// consistent across the ASCII fast path, the shaped path, and the
/// char-fallback path.
#[inline]
pub fn anchor_powerline_rect(
    ch: char,
    cx: f32,
    cy: f32,
    cell_w: f32,
    cell_h: f32,
    natural: (f32, f32, f32, f32),
) -> (f32, f32, f32, f32) {
    if is_powerline_char(ch) {
        (cx, cy, cell_w, cell_h)
    } else {
        natural
    }
}

/// Eagerly rasterize every codepoint in [`PREBAKE_RANGES`] into `atlas`
/// using `rasterizer`'s configured family/size. Returns the number of
/// glyphs that were successfully inserted (i.e. the font chain resolved
/// the codepoint and the atlas accepted the tile).
///
/// Why this exists: TUIs draw a wall of box-drawing characters on first
/// paint. Without prebake the renderer pays one charmap-walk + outline
/// scale per unique glyph in the first frame, which is the visible
/// "stutter on launch" WezTerm avoids by baking these at font load.
///
/// The atlas's LRU may eventually evict these tiles if the user opens a
/// session with extreme glyph diversity; that's fine — they'll be
/// re-rasterized lazily like any other glyph. The win is the *first*
/// frame, which is exactly when the cost is most visible.
pub fn prebake_box_and_powerline(
    rasterizer: &mut SwashRasterizer<'_>,
    atlas: &mut GlyphAtlas,
) -> usize {
    let mut inserted = 0usize;
    for range in PREBAKE_RANGES {
        for cp in range.clone() {
            let Some(ch) = char::from_u32(cp) else { continue };
            // Skip codepoints the chain can't satisfy — `resolve_slot`
            // returns None and we leave the atlas untouched. A later
            // real use will still fall back through the normal path.
            let Some(slot) = rasterizer.resolve_slot(ch, false, false) else { continue };
            let key = GlyphKey::with_slot(ch, slot, false, false);
            if let Some(info) = atlas.get_or_insert(key, rasterizer) {
                // Zero-area UV means rasterizer-miss sentinel; don't
                // count those as a real prebake hit.
                if info.px_size[0] != 0 && info.px_size[1] != 0 {
                    inserted += 1;
                }
            }
        }
    }
    inserted
}

/// In-place convert a buffer of straight-alpha RGBA pixels (the format
/// swash returns for `Content::Color` strikes) into premultiplied BGRA
/// (the format our atlas texture + alpha-blend state expect).
///
/// Both transformations happen in a single pass:
///   - channel swap: `R` and `B` are exchanged
///   - premultiply:  `R`, `G`, `B` are each scaled by `A / 255`
///
/// Without this, color emoji would render with red and blue swapped and
/// with bright edge fringes when composited over a non-black background
/// (the classic straight-alpha-into-premultiplied-blend artifact).
#[doc(hidden)]
pub fn rgba_straight_to_bgra_premul(pixels: &mut [u8]) {
    for px in pixels.chunks_exact_mut(4) {
        let r = px[0];
        let g = px[1];
        let b = px[2];
        let a = px[3];
        // Standard "round to nearest" 8-bit premultiply: (c * a + 127) / 255.
        // The +127 makes the truncating divide round-half-up without a
        // float conversion.
        let pm = |c: u8| -> u8 { ((c as u16 * a as u16 + 127) / 255) as u8 };
        px[0] = pm(b);
        px[1] = pm(g);
        px[2] = pm(r);
        px[3] = a;
    }
}

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
// NOTE: The bundled primary `Rec Mono St.Helens` is Nerd-Font-patched,
// so Powerline (U+E0B0–U+E0BF) and Nerd Font PUA (U+E000–U+F8FF)
// codepoints resolve in slot 0 without needing any system Nerd Font.
// The platform fallback chain below is retained for non-Latin scripts
// (CJK, emoji) that the primary doesn't cover — CJK glyph resolution
// goes to PingFang/Microsoft YaHei/Noto, color emoji to the platform
// color font.
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

const MONOCHROME_SOURCES: &[Source] = &[Source::Outline, Source::Bitmap(StrikeWith::BestFit)];

// LCD subpixel rendering is temporarily disabled on every platform.
//
// PR #267 enabled `Format::Subpixel` on Windows for ClearType parity, but
// the current text pipeline composites subpixel masks incorrectly after the
// later linear-space/gamma changes: terminal cells become unreadable horizontal
// ink-stroke artifacts while the glyphon-rendered tab titles remain fine
// (#316). Use grayscale alpha masks until the LCD path has a dedicated shader
// and blend-mode fix.
const MONOCHROME_FORMAT: Format = Format::Alpha;

/// Test-visible snapshot of the monochrome rasterizer quality settings.
/// Keep outline hinting enabled, but use grayscale alpha masks until the
/// Windows LCD/subpixel integration is fixed (#316).
#[doc(hidden)]
pub fn monochrome_render_config_for_test() -> (&'static [Source], Format, bool) {
    (MONOCHROME_SOURCES, MONOCHROME_FORMAT, true)
}

/// Test-visible snapshot of the platform fallback chain. Used by the
/// `lcd_only_on_windows` regression to assert Nerd Font sits at the TAIL
/// (not the FRONT) of every chain — see the P0 fix for PR #267.
#[doc(hidden)]
pub fn platform_fallback_chain_for_test() -> &'static [&'static str] {
    PLATFORM_FALLBACK_CHAIN
}

/// Test-visible helper for the exact fontdb lookup semantics used by
/// [`SwashRasterizer`].
#[doc(hidden)]
pub fn lookup_id_in_db(
    db: &fontdb::Database,
    family: &str,
    weight_bold: bool,
    italic: bool,
) -> Option<fontdb::ID> {
    let weight = if weight_bold { fontdb::Weight::BOLD } else { fontdb::Weight::NORMAL };
    let style = if italic { fontdb::Style::Italic } else { fontdb::Style::Normal };
    // Only ask fontdb for `Name(family)` — no Monospace tail here,
    // otherwise the lookup for a CJK family on a system without it
    // would silently substitute the default monospace and shadow
    // a real fallback in the next slot.
    let families = [fontdb::Family::Name(family)];
    let query =
        fontdb::Query { families: &families, weight, stretch: fontdb::Stretch::Normal, style };
    let id = db.query(&query)?;
    let face = db.face(id)?;
    if face.style == style {
        Some(id)
    } else {
        None
    }
}

/// Load bundled TTF/OTF files from the same locations used by the windowed
/// renderers. Shared by the terminal renderer and the preferences renderer so
/// Nerd Font PUA codepoints resolve consistently in both paths.
pub fn load_bundled_fonts(fs: &mut FontSystem) {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            candidates.push(d.join("assets/fonts"));
            if let Some(contents) = d.parent() {
                candidates.push(contents.join("Resources/assets/fonts"));
            }
        }
    }
    candidates
        .push(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts"));

    let mut total: usize = 0;
    let mut n_dirs: usize = 0;
    for dir in &candidates {
        tracing::debug!("load_bundled_fonts: checking candidate {dir:?}");
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("load_bundled_fonts: read_dir failed for {dir:?}: {e}");
                continue;
            }
        };
        let mut n = 0;
        for e in entries.flatten() {
            let p = e.path();
            let ext = p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase());
            if matches!(ext.as_deref(), Some("ttf") | Some("otf")) {
                if let Ok(bytes) = std::fs::read(&p) {
                    crate::load_font_data_with_sonic_overrides(fs, bytes);
                    n += 1;
                }
            }
        }
        if n > 0 {
            tracing::debug!("load_bundled_fonts: loaded {n} font(s) from {dir:?}");
            total += n;
            n_dirs += 1;
            // First populated dir wins — preserves prior behaviour that
            // an installed bundle shadows the in-repo source tree.
            break;
        }
    }
    tracing::info!("load_bundled_fonts: loaded {total} font(s) across {n_dirs} dirs");
    if total == 0 {
        tracing::warn!("load_bundled_fonts: NO bundled fonts found; checked: {candidates:?}");
    }
}

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
    /// Optional async loader for off-startup-path font fallback
    /// resolution (Epic #300 P4 follow-up). When set, a miss in the
    /// already-loaded fallback chain enqueues background `request_load`
    /// calls for every static [`PLATFORM_FALLBACK_CHAIN`] entry the
    /// loader has not yet attempted, then returns `None` (tofu)
    /// WITHOUT sync-blocking on the load. When the loader completes a
    /// family it fires its notifier — `sonicterm-app` wires that to a
    /// `UserEvent::ClearShapeCache` which calls
    /// [`Self::clear_caches`] and bumps the renderer's `style_rev`.
    async_loader: Option<AsyncFallbackLoader>,
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
            async_loader: None,
        }
    }

    /// Attach an [`AsyncFallbackLoader`] for off-startup-path fallback
    /// resolution. See the [`Self::async_loader`] field doc for the
    /// flow. Returns `self` to allow chaining at renderer construction.
    #[must_use]
    pub fn with_async_loader(mut self, loader: AsyncFallbackLoader) -> Self {
        self.async_loader = Some(loader);
        self
    }

    /// Replace (or install) the async fallback loader on an existing
    /// rasterizer. Used by the renderer-construction path where the
    /// loader is built after the rasterizer.
    pub fn set_async_loader(&mut self, loader: AsyncFallbackLoader) {
        self.async_loader = Some(loader);
    }

    /// Borrow the configured loader, if any. Test/diagnostic only.
    #[doc(hidden)]
    #[must_use]
    pub fn async_loader(&self) -> Option<&AsyncFallbackLoader> {
        self.async_loader.as_ref()
    }

    /// Drop the memoized slot cache. Called from the renderer's
    /// `clear_shape_cache()` after the async loader has fired its
    /// notifier — without this, a negative slot decision recorded
    /// before the family loaded would stick for the rest of the
    /// session.
    pub fn clear_caches(&mut self) {
        self.slot_cache.clear();
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

    /// Borrow the underlying [`FontSystem`] mutably. Needed by the
    /// shaper-driven render path so a single mutable borrow of
    /// `GpuRenderer.font_system` can be threaded through *both* the
    /// rasterizer (charmap + outline scaling) and cosmic-text shaping
    /// (which also wants `&mut FontSystem`). Without this accessor
    /// the borrow checker would force the renderer to drop and rebuild
    /// the rasterizer between every shape pass.
    pub fn font_system_mut(&mut self) -> &mut FontSystem {
        self.font_system
    }

    /// Convenience: build at [`DEFAULT_RASTER_PX`] with the bundled
    /// "Rec Mono St.Helens" family. Used by the test harness.
    pub fn with_default_family(font_system: &'a mut FontSystem) -> Self {
        Self::new(font_system, "Rec Mono St.Helens", DEFAULT_RASTER_PX)
    }

    /// Look up the fontdb ID for `family` at the given (bold, italic)
    /// combination, returning `None` if nothing in the fontdb exactly matches
    /// the requested style.
    fn lookup_id(&self, family: &str, weight_bold: bool, italic: bool) -> Option<fontdb::ID> {
        lookup_id_in_db(self.font_system.db(), family, weight_bold, italic)
    }

    /// Reverse-lookup the slot index for a fontdb ID. Used by the
    /// shaper-driven render path: cosmic-text returns a
    /// `LayoutGlyph::font_id`, and we need the matching slot to bake
    /// into the [`GlyphKey`] so atlas tiles don't collide across
    /// faces. Returns `None` for IDs not in our chain (cosmic-text
    /// substituted from the fontdb at large — we fall back to the
    /// slot we asked for at the run level in that case).
    pub fn slot_for_font_id(
        &self,
        target: fontdb::ID,
        weight_bold: bool,
        italic: bool,
    ) -> Option<u8> {
        for (idx, family) in self.families.iter().enumerate() {
            if let Some(id) = self.lookup_id(family, weight_bold, italic) {
                if id == target {
                    return Some(idx as u8);
                }
            }
        }
        None
    }

    /// Walk the fallback chain and return the first slot whose face
    /// has a non-zero glyph for `ch`. Memoized per (ch, bold, italic).
    /// Returns `None` only if every face in the chain returns a zero
    /// glyph id (true tofu).
    ///
    /// When an [`AsyncFallbackLoader`] is attached AND the live chain
    /// cannot satisfy `ch`, this method enqueues a background
    /// `request_load` for every static [`PLATFORM_FALLBACK_CHAIN`]
    /// entry the loader has not yet attempted, then returns `None`
    /// (tofu) WITHOUT sync-blocking. The negative decision is NOT
    /// cached when at least one such load was actually spawned —
    /// otherwise the eventual `ClearShapeCache` clear-and-redraw
    /// would still hit a stale `Some(None)` here.
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
        if found.is_none() {
            if let Some(loader) = &self.async_loader {
                // Spawn background loads for any chain entry not yet
                // resolved. `request_load` itself dedups against
                // pending / loaded / failed sets — we just need to
                // poke it so the eventual completion fires the
                // notifier.
                let mut spawned_any = false;
                for family in PLATFORM_FALLBACK_CHAIN {
                    if loader.is_loaded(family)
                        || loader.is_pending(family)
                        || loader.is_failed(family)
                    {
                        continue;
                    }
                    if loader.request_load(family) {
                        spawned_any = true;
                    }
                }
                if spawned_any {
                    // Skip caching the negative so the post-load
                    // re-render actually re-walks the chain instead
                    // of fast-pathing through the memo.
                    return None;
                }
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
        //
        // Skipped for shaped keys (glyph_id != 0): the shaper may have
        // produced a real shaped glyph whose first cluster codepoint
        // happens to be ' ' (rare but possible inside RTL/cluster
        // edge cases), so we rasterize by glyph_id regardless.
        if key.glyph_id == 0 && (key.ch == ' ' || key.ch == '\t') {
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
        // Shaped path: the caller already knows the glyph id (cosmic-text
        // shaped it). Skip the charmap lookup entirely — for ligatures
        // and ZWJ-composed clusters the charmap of the *first*
        // codepoint would resolve to a different (component) glyph or
        // none at all.
        let glyph_id = if key.glyph_id != 0 {
            key.glyph_id
        } else {
            let g = swash_font.charmap().map(key.ch);
            if g == 0 {
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
            g
        };

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
                // swash emits color bitmaps as straight-alpha RGBA; the
                // atlas contract (and our wgpu blend state) is
                // premultiplied BGRA. Swap R↔B and multiply RGB by A in
                // a single pass so the upload is a memcpy.
                rgba_straight_to_bgra_premul(&mut data);
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

        let image = Render::new(MONOCHROME_SOURCES)
            .format(MONOCHROME_FORMAT)
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

        let coverage = match image.content {
            swash::scale::image::Content::SubpixelMask => {
                let expected = (p.width as usize) * (p.height as usize) * 4;
                let mut data = image.data;
                if data.len() != expected {
                    data.resize(expected, 0);
                }
                for px in data.chunks_exact_mut(4) {
                    let r = px[0];
                    let g = px[1];
                    let b = px[2];
                    let a = r.max(g).max(b);
                    px[0] = b;
                    px[1] = g;
                    px[2] = r;
                    px[3] = a;
                }
                data
            }
            _ => {
                // Format::Alpha (macOS + Linux): swash returns one alpha
                // byte per pixel and the atlas blit in
                // `glyph_atlas::insert_glyph` for `is_color = false`
                // replicates each alpha byte into BGRA itself. Returning
                // 1 byte per pixel here is REQUIRED — pre-#267 we did
                // exactly this and #267 inadvertently broke it by always
                // pre-expanding to 4 bytes per pixel (the atlas was not
                // updated to match, so on macOS post-#267 every
                // monochrome glyph was read as 1/4 the actual buffer
                // length, producing the "smeared color stripes" P0
                // user-reported in the wake of #282 — which reverted the
                // FORMAT but left the expansion in place. See the
                // `mono_alpha_returns_one_byte_per_pixel` regression
                // test in `crates/sonicterm-text/tests/mono_alpha_byte_layout.rs`.
                let expected = (p.width as usize) * (p.height as usize);
                let mut alpha = image.data;
                if alpha.len() != expected {
                    alpha.resize(expected, 0);
                }
                alpha
            }
        };

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
