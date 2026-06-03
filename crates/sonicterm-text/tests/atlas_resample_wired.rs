//! Integration tests for the production wiring (#610 sym-1 PR-D) that
//! routes `SymbolFit::IconCellFit` glyphs through
//! `GlyphAtlas::get_or_insert_resampled` instead of the natural-size
//! `get_or_insert` path. Pure CPU — no fonts required, uses the
//! `SyntheticRasterizer` to drive the atlas.
//!
//! These tests sit beside `atlas_resample.rs` (which covers PR-B's
//! atlas-side API in isolation) and prove the helper actually behaves
//! the way `core.rs::flush_shape_run` will use it: cache hits
//! short-circuit without re-resampling, misses produce a tile at the
//! target dimensions (not natural), and rasterizer failures degrade
//! gracefully to a zero-area sentinel rather than panicking.

use sonicterm_text::glyph_atlas::{GlyphAtlas, RasterTile, Rasterizer, SyntheticRasterizer};
use sonicterm_types::GlyphKey;

/// Mirrors the production call site: ask for an icon-fit glyph at a
/// 2x DPR target. The synthetic rasterizer produces an 8..=15 px ramp
/// at natural size; the resample path must store a tile at the
/// requested target dimensions instead.
#[test]
fn get_or_insert_resampled_stores_target_size_not_natural() {
    let mut atlas = GlyphAtlas::new(256, 256);
    let mut rast = SyntheticRasterizer::default();
    // U+F0E7 (NF lightning) — IconCellFit under classify_symbol().
    let key = GlyphKey::new('\u{f0e7}', false, false);
    let (target_w, target_h) = (16, 32);

    let info = atlas
        .get_or_insert_resampled(key, &mut rast, |_tile| Some((target_w, target_h)))
        .expect("fresh atlas + synthetic rasterizer cannot fail");

    assert_eq!(
        info.px_size,
        [target_w, target_h],
        "tile stored at target cell-box size, NOT natural rasterizer size — \
         the whole point of #610 sym-1 PR-D"
    );
    // Second call: cache hit MUST short-circuit and bump hit count.
    // Without this, the resample would re-run every frame and burn CPU.
    let hits_before = atlas.hits();
    let again =
        atlas.get_or_insert_resampled(key, &mut rast, |_| Some((target_w, target_h))).expect("hit");
    assert_eq!(again.px_size, [target_w, target_h]);
    assert_eq!(atlas.hits(), hits_before + 1, "second call must be a cache hit");
    // And: rasterizer was NOT invoked on the hit — that's the LRU
    // bookkeeping contract (a re-rasterize on hit would also blow
    // perf on the per-frame icon-render path).
    assert_eq!(rast.calls, 1, "rasterizer invoked exactly once across both calls (miss + hit)");
}

/// Aspect-ratio-preserving target (the real production case): the
/// closure mimics what `core.rs` does — derive target_w from the
/// natural tile's aspect ratio at fixed target_h. Verifies the
/// closure-driven path computes dimensions from the actual rasterized
/// tile and not from stale inputs.
#[test]
fn get_or_insert_resampled_uses_per_tile_target_closure() {
    let mut atlas = GlyphAtlas::new(256, 256);
    let mut rast = SyntheticRasterizer::default();
    // Pick a char whose synthetic ramp is 8x8 (key.ch as u32 % 8 == 0 →
    // 'h' = 0x68, 0x68 % 8 = 0, so N=8). Aspect = 1.0.
    let key = GlyphKey::new('h', false, false);
    let target_h_phys: u32 = 32;
    let info = atlas
        .get_or_insert_resampled(key, &mut rast, |tile| {
            // Exactly the closure shape used in core.rs:
            let aspect = tile.width as f32 / tile.height as f32;
            let w = ((target_h_phys as f32) * aspect).round().max(1.0) as u32;
            Some((w, target_h_phys))
        })
        .expect("alloc");
    assert_eq!(info.px_size[1], target_h_phys, "height matches closure-computed target");
    assert_eq!(
        info.px_size[0], target_h_phys,
        "aspect-preserving square synth glyph → square target (h == w)"
    );
}

/// Rasterizer failure (the trait returning `None`) caches a zero-area
/// sentinel — same contract as `get_or_insert`. Without this, an
/// uncacheable miss would re-rasterize every frame and either panic
/// or churn CPU.
#[test]
fn get_or_insert_resampled_caches_rasterizer_failure_as_sentinel() {
    /// Rasterizer that always reports failure — models the production
    /// case where the swash chain has no face for a codepoint.
    struct NullRasterizer {
        calls: u32,
    }
    impl Rasterizer for NullRasterizer {
        fn rasterize(&mut self, _key: GlyphKey) -> Option<RasterTile> {
            self.calls += 1;
            None
        }
    }

    let mut atlas = GlyphAtlas::new(64, 64);
    let mut rast = NullRasterizer { calls: 0 };
    let key = GlyphKey::new('\u{f0e7}', false, false);

    let info = atlas
        .get_or_insert_resampled(key, &mut rast, |_| Some((16, 16)))
        .expect("sentinel returned, not panicked");
    assert_eq!(info.px_size, [0, 0], "zero-area sentinel matches get_or_insert behavior");

    // Re-issue: must hit the sentinel cache, NOT re-call rasterize.
    let _ = atlas.get_or_insert_resampled(key, &mut rast, |_| Some((16, 16)));
    assert_eq!(rast.calls, 1, "rasterizer invoked exactly once even across repeated misses");
}

/// Caller's closure returning `None` (degenerate cell dims) MUST fall
/// back to the natural-size insert — losing the glyph entirely would
/// be a worse regression than the pre-#610 aliasing.
#[test]
fn get_or_insert_resampled_falls_back_to_natural_when_target_none() {
    let mut atlas = GlyphAtlas::new(256, 256);
    let mut rast = SyntheticRasterizer::default();
    let key = GlyphKey::new('\u{f0e7}', false, false);

    let info = atlas
        .get_or_insert_resampled(key, &mut rast, |_| None) // caller declined
        .expect("natural-size fallback path");
    // Natural-size insert is non-zero (synthetic ramp produces 8..=15 px
    // tiles); the test just asserts we got SOMETHING rather than the
    // zero-area sentinel that would mean "draw tofu".
    assert!(info.px_size[0] > 0 && info.px_size[1] > 0, "fallback inserts the natural-size tile");
}
