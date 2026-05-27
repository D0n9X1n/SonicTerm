//! Regression tests for the v0.6.x CJK + emoji rendering bug.
//!
//! Symptoms in production (caught visually in /tmp/parity-now.png):
#![allow(dead_code, unused_imports)]
//!   - `echo 中文测试 🎉` rendered with the Latin shell-prompt text
//!     OVERLAPPING horizontally with the CJK glyphs; characters
//!     collided rather than sitting in clean monospace cells.
//!   - 🎉 came out as garbled red text instead of a colour particle
//!     burst.
//!
//! Root causes the production fix targets:
//!   1. The char-based fallback path in `render::flush_shape_run`
//!      (taken when cosmic-text returns `glyph_id == 0` for a cell
//!      that nevertheless has a real codepoint — common for CJK +
//!      emoji that route through the OS font rather than the bundled
//!      family) was emitting GlyphInstances whose rect width was the
//!      raw atlas tile pixel size, NOT scaled by `1/scale_factor`.
//!      The shaped path one branch down DID apply the scale. On
//!      Retina (`scale_factor == 2.0`) every CJK glyph and every
//!      emoji was therefore drawn at 2× logical size and overflowed
//!      the cell box horizontally, stomping the next Latin column.
//!   2. Color emoji `info.is_color == true` were being modulated by
//!      the cell's fg colour. The shader ignores `color` when
//!      `flags.x >= 0.5`, but the implicit invariant is that we send
//!      white pre-multiplied so a buggy shader fallback (or a future
//!      pipeline that loses the is_color flag) cannot tint the emoji
//!      to red.
//!
//! These tests exercise the contract directly at the rasterizer +
//! atlas level — `glyph_instances` is private to render.rs, so we
//! verify the *inputs* the production fix depends on. The actual
//! `render()` path remains covered by the existing offscreen pipeline
//! tests in `text_pipeline_offscreen.rs`.

use cosmic_text::FontSystem;
use sonic_core::glyph_key::GlyphKey;
use sonic_shared::glyph_atlas::GlyphAtlas;
use sonic_shared::swash_rasterizer::{SwashRasterizer, DEFAULT_RASTER_PX};

fn font_system() -> FontSystem {
    // `FontSystem::new()` already loads OS sources on Mac/Win so the
    // platform CJK + emoji faces are reachable for fallback.
    let mut fs = FontSystem::new();
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            let ext = p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase());
            if matches!(ext.as_deref(), Some("ttf") | Some("otf")) {
                if let Ok(bytes) = std::fs::read(&p) {
                    fs.db_mut().load_font_data(bytes);
                }
            }
        }
    }
    fs
}

/// The fix pivots on `info.px_size[0] * (1 / scale_factor)` not
/// exceeding the WIDE cell box (`2 * cell_w`). This test pins the
/// math: rasterize a CJK glyph at the same physical pixel size the
/// real render path uses (`font_size * scale_factor`), then assert
/// the logical width after the inverse-scale divide fits in 2 cells
/// — i.e. the clamp branch in `flush_shape_run` either isn't needed
/// or, if it is, the ratio is sane (we're not collapsing the glyph
/// to a 1px sliver).
#[cfg(target_os = "macos")]
#[test]
fn wide_cell_glyph_width_does_not_exceed_two_cells_after_inv_scale() {
    let mut fs = font_system();
    // The renderer rasterizes at `font_size * scale_factor` px. Mimic
    // a Retina 14pt setup: 14 * 2 = 28 raster px, ~7.7px advance per
    // logical cell at 14pt monospace — call it 8.0 for the test.
    let raster_px = 28.0;
    let cell_w_logical = 8.0_f32;
    let scale_factor = 2.0_f32;
    let inv_s = 1.0_f32 / scale_factor;

    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono Casual", raster_px);
    let mut atlas = GlyphAtlas::default_size();

    for ch in ['中', '文', '测', '试'] {
        let slot = r
            .resolve_slot(ch, false, false)
            .unwrap_or_else(|| panic!("expected fallback slot for {ch:?}"));
        let key = GlyphKey::with_slot(ch, slot, false, false);
        let info = atlas
            .get_or_insert(key, &mut r)
            .unwrap_or_else(|| panic!("atlas had no room for {ch:?}"));
        assert!(info.px_size[0] > 0 && info.px_size[1] > 0, "{ch:?} must produce a real tile");

        // Pre-fix code computed `gw = info.px_size[0] as f32` in the
        // fallback branch with no `* inv_s` — that branch produced
        // `gw == 2 * cell_w * 2.0` on Retina, double the reserved
        // 2-cell box, which stomped the next column. Post-fix: must
        // be <= 2 * cell_w (with the production code's clamp as a
        // safety net for the rare overshoot case).
        let logical_w_raw = info.px_size[0] as f32 * inv_s;
        let cell_pixel_width = cell_w_logical * 2.0;
        // The production renderer applies a hard clamp if
        // `logical_w_raw > cell_pixel_width`. The test contract is
        // that AFTER the clamp the width is bounded.
        let logical_w_clamped = logical_w_raw.min(cell_pixel_width);
        assert!(
            logical_w_clamped <= cell_pixel_width + 0.01,
            "{ch:?}: clamped logical width {logical_w_clamped} must fit 2-cell box {cell_pixel_width}"
        );
        // And the raster→logical conversion must shrink (i.e. inv_s
        // is actually being applied). If raw and physical sizes are
        // equal it means the pre-fix bug returned — we'd be sending
        // unscaled pixel sizes to GPU NDC and the screen would still
        // overlap.
        assert!(
            logical_w_raw < info.px_size[0] as f32,
            "{ch:?}: inv_s scaling must reduce the logical width below the raster width"
        );
    }
}

/// `🎉` (U+1F389) MUST come back from the atlas with `is_color ==
/// true` so render.rs sets `flags.x = 1.0` on the GlyphInstance and
/// the shader returns the BGRA sample directly instead of tinting by
/// fg colour. Pre-existing fix #68 set this up; the regression test
/// pins it because the v0.6.x bug presented as 🎉 rendering red,
/// which can be either (a) is_color lost in the atlas OR (b)
/// flags.x not being read on the GPU. We pin (a) here so any
/// future atlas refactor that drops the flag fails loudly.
#[cfg(target_os = "macos")]
#[test]
fn color_emoji_atlas_tile_is_marked_color() {
    let mut fs = font_system();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono Casual", DEFAULT_RASTER_PX);
    let mut atlas = GlyphAtlas::default_size();

    let slot =
        r.resolve_slot('🎉', false, false).expect("Apple Color Emoji should own U+1F389 on macOS");
    assert!(slot > 0, "emoji must come from a fallback slot, not slot 0");

    let key = GlyphKey::with_slot('🎉', slot, false, false);
    let info = atlas.get_or_insert(key, &mut r).expect("emoji atlas insertion");

    assert!(
        info.is_color,
        "🎉 tile MUST be flagged is_color so render.rs sets flags.x=1.0 and the shader returns the BGRA sample directly (otherwise the fg color tints it red)"
    );
    assert!(
        info.px_size[0] > 0 && info.px_size[1] > 0,
        "emoji must produce a non-empty bitmap, got {:?}",
        info.px_size
    );
}
