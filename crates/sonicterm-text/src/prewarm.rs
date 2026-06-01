//! Startup pre-warm for the shape cache + glyph atlas.
//!
//! ## Why this exists
//!
//! Issue #415: launching nvim / vim / lazygit / any TUI that draws a
//! status-line of Nerd Font PUA icons (devicons, FontAwesome) costs
//! ~111 ms of first-frame stall because cosmic-text's
//! `BufferLine::layout` does ~3.3 ms of synchronous fallback-chain
//! probing the **first time** it sees a PUA codepoint that the primary
//! font lacks (warm-cache cost: 0.008 ms). 30 row × 3.3 ms ≈ 100 ms of
//! visible "stutter on launch" — exactly the class of bug
//! `prebake_box_and_powerline` already handles for the box-drawing
//! range.
//!
//! ## What this does
//!
//! At startup (after the rasterizer + atlas are built but before the
//! first frame) we:
//!
//!   1. Rasterize **ASCII 0x20..=0x7E** at `(regular, bold, italic)` so
//!      the steady-state prompt + shell echo has zero atlas misses on
//!      the first paint.
//!   2. Rasterize a **bounded Nerd Font subset** (≤ 1500 codepoints)
//!      covering devicons (U+E700–U+E7C5), FontAwesome
//!      (U+F000–U+F2E0), and Pomicons (U+EE00–U+EE0B). These are the
//!      ranges every Nerd-Font-patched developer font ships and are
//!      what nvim status-line plugins emit.
//!   3. **Skip codepoints the primary font lacks** — calling into the
//!      OS fallback chain at startup is precisely the cost we're
//!      avoiding for issue #415. The `primary_has_glyph` check uses
//!      the loaded font's swash charmap directly, no shaping.
//!
//! ## Bounds
//!
//! The Nerd Font list below is **`const`** and explicitly capped — any
//! growth requires a code review of the size. Current count:
//!
//!   - devicons U+E700..=U+E7C5 → 198 codepoints
//!   - FontAwesome U+F000..=U+F2E0 → 737 codepoints
//!   - Pomicons U+EE00..=U+EE0B → 12 codepoints
//!   - **Total: 947 codepoints** (≤ 1500 cap, per #415 spec)
//!
//! ## What this is NOT
//!
//! - Not a substitute for lazy fallback — codepoints outside the
//!   pre-warm set still get the on-demand fallback walk.
//! - Not exhaustive — emoji, CJK, and the long tail of Nerd Font
//!   "Material Design Icons" (U+F0001..) are deliberately excluded
//!   because their probability-per-launch is far lower than the
//!   per-codepoint atlas cost.

use cosmic_text::FontSystem;

use crate::glyph_atlas::GlyphAtlas;
use crate::shape::{shape_run, RunStyle};
use crate::swash_rasterizer::{lookup_id_in_db, SwashRasterizer};
use sonicterm_types::{Cell, CellFlags, Color, GlyphKey};

/// Common Nerd Font PUA codepoints to pre-rasterize at startup.
///
/// Bounded list (≤ 1500 codepoints, currently 947) — auditable in one
/// place. Adding a new range requires a code review per the bounds
/// comment in the module docs.
pub const NERD_PREWARM_RANGES: &[std::ops::RangeInclusive<u32>] = &[
    // devicons — language / framework / editor logos used by every
    // nvim file-tree plugin (NvimTree, neo-tree, nvim-web-devicons).
    0xE700..=0xE7C5,
    // FontAwesome — the dominant Nerd Font glyph block. lualine /
    // airline / lazygit status icons live here.
    0xF000..=0xF2E0,
    // Pomicons — small block used by tmux-pomodoro / waybar.
    0xEE00..=0xEE0B,
];

/// True when the primary font (`family` at the requested weight) has a
/// non-zero glyph for `ch`. Does NOT walk the fallback chain — by
/// design: pre-warm callers want to skip codepoints that would force
/// an OS-fallback resolution at startup (the exact cost issue #415 is
/// avoiding).
#[must_use]
pub fn primary_has_glyph(
    font_system: &mut FontSystem,
    family: &str,
    weight_bold: bool,
    italic: bool,
    ch: char,
) -> bool {
    let Some(id) = lookup_id_in_db(font_system.db(), family, weight_bold, italic) else {
        return false;
    };
    let weight = if weight_bold { fontdb::Weight::BOLD } else { fontdb::Weight::NORMAL };
    let Some(font) = font_system.get_font(id, weight) else { return false };
    font.as_swash().charmap().map(ch) != 0
}

/// Pre-rasterize ASCII × {regular, bold, italic} and the bounded Nerd
/// Font subset into `atlas`. Returns the number of glyphs successfully
/// inserted.
///
/// Pre-warm is **best-effort**: any codepoint the primary font lacks
/// is skipped (never recursing into fallback at startup), and any
/// atlas insertion that fails (rasterizer-miss sentinel) is counted as
/// not-inserted. The terminal still works correctly without pre-warm
/// — the cost reappears as on-demand first-encounter rasterization.
pub fn prewarm_ascii_and_nerd(
    rasterizer: &mut SwashRasterizer<'_>,
    atlas: &mut GlyphAtlas,
) -> usize {
    let mut inserted = 0usize;

    // 1. ASCII 0x20..=0x7E for every style we shape with. Bold + italic
    //    are common enough on prompts (powerlevel10k, starship) that
    //    they're worth pre-rasterizing alongside regular.
    for &(bold, italic) in &[(false, false), (true, false), (false, true)] {
        for cp in 0x20u32..=0x7Eu32 {
            let Some(ch) = char::from_u32(cp) else { continue };
            let Some(slot) = rasterizer.resolve_slot(ch, bold, italic) else { continue };
            let key = GlyphKey::with_slot(ch, slot, bold, italic);
            if let Some(info) = atlas.get_or_insert(key, rasterizer) {
                if info.uv[2] > info.uv[0] && info.uv[3] > info.uv[1] {
                    inserted += 1;
                }
            }
        }
    }

    // 2. Nerd Font PUA subset — regular weight only (status-line
    //    icons are virtually never bold or italic in practice, and we
    //    have a 1500-codepoint cap to honor).
    //
    //    We do TWO things per surviving codepoint: (a) rasterize into
    //    the atlas (cheap, ~µs), and (b) run a one-cell `shape_run`
    //    through cosmic-text. (b) is the expensive bit issue #415
    //    measured (3.3 ms / row first-encounter) — running it here
    //    moves the cost from first-frame to startup, where it's
    //    masked by the rest of the wgpu device init.
    let family = rasterizer.family().to_string();
    let mut shape_cells: Vec<(u16, Cell)> = Vec::with_capacity(1);
    let style = RunStyle { bold: false, italic: false };
    for range in NERD_PREWARM_RANGES {
        for cp in range.clone() {
            let Some(ch) = char::from_u32(cp) else { continue };
            // Primary-font gate: never resolve through fallback at
            // startup. If the user's primary font isn't a Nerd Font
            // patch, the whole range is a no-op (correct behavior).
            if !primary_has_glyph(rasterizer.font_system_mut(), &family, false, false, ch) {
                continue;
            }
            let Some(slot) = rasterizer.resolve_slot(ch, false, false) else { continue };
            let key = GlyphKey::with_slot(ch, slot, false, false);
            if let Some(info) = atlas.get_or_insert(key, rasterizer) {
                if info.uv[2] > info.uv[0] && info.uv[3] > info.uv[1] {
                    inserted += 1;
                }
            }
            // Prime cosmic-text's internal font-discovery caches by
            // shaping a single-cell run containing this icon. We
            // discard the result — the value is the side-effect on
            // the shaper's per-font caches.
            shape_cells.clear();
            shape_cells
                .push((0, Cell::plain(ch, Color::Default, Color::Default, CellFlags::empty())));
            let _shaped =
                shape_run(rasterizer, &family, DEFAULT_PREWARM_SHAPE_PX, style, &shape_cells);
        }
    }

    inserted
}

/// Em-size used when priming cosmic-text's shaping caches for the
/// Nerd Font subset. Picking a fixed value (rather than threading
/// the renderer's actual font size through) is fine because cosmic-
/// text's font-discovery caches are size-independent — the value
/// just needs to be a legal positive float.
const DEFAULT_PREWARM_SHAPE_PX: f32 = 14.0;

#[cfg(test)]
mod tests {
    use super::*;

    /// Audit gate: the static Nerd Font list must stay below 1500
    /// codepoints per the bounds in the module docs and the issue
    /// #415 contract. Growth past this needs an explicit code review.
    #[test]
    fn nerd_prewarm_under_cap() {
        let total: u32 = NERD_PREWARM_RANGES
            .iter()
            .map(|r| r.end().saturating_sub(*r.start()).saturating_add(1))
            .sum();
        assert!(total <= 1500, "NERD_PREWARM_RANGES grew to {total}, cap is 1500");
        assert!(total > 0);
    }

    #[test]
    fn primary_has_glyph_handles_unknown_family() {
        let mut fs = FontSystem::new();
        // A family that does not exist must return false rather than
        // panicking — pre-warm runs with the user's configured font,
        // which may be misspelled in their toml.
        assert!(!primary_has_glyph(&mut fs, "NoSuchFamily-xyz", false, false, 'a'));
    }
}
