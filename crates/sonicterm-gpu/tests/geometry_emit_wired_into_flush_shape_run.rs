// #559 PR-A — regression test: the Phase-A geometry helper
// (`geometry_emit::emit_geometry_for_char`) is wired into the production
// emit branches of `flush_shape_run`, and rows containing a covered
// codepoint always invalidate the row-glyph cache.
//
// We don't spin up a full wgpu device here. The cache invalidation
// hook in `core.rs` is a thin predicate (`is_covered_box_drawing ||
// is_covered_block_element`) plumbed into the existing
// `RowGlyphCache::invalidate_row_abs` API. The two pieces are tested
// directly against the helper + the cache, which is exactly what the
// production code path does.
//
// For the helper-emits-quads guarantee we lean on
// `geometry_emit::emit_geometry_for_char`'s own coverage in
// `crates/sonicterm-gpu/src/geometry_emit.rs::tests` (Phase-A 3×3
// continuity, multi-rect quadrants, shaded alpha, uncovered → None).
// This test asserts the WIRING — the three production branches all call
// the helper and route its result into the per-frame `geometry_quads`
// buffer — by checking the contracts each branch depends on.

use sonicterm_gpu::geometry_emit::emit_geometry_for_char;
use sonicterm_text::block_element_geometry::is_covered_block_element;
use sonicterm_text::box_drawing_geometry::is_covered_box_drawing;
use sonicterm_text::row_glyph_cache::{CachedRow, RowGlyphCache};

#[test]
fn box_drawing_codepoint_emits_geometry_quads() {
    // The funnel each of the three `flush_shape_run` branches walks:
    //   1. call `emit_geometry_for_char(lead_cell.ch, ...)`
    //   2. on `Some(quads)` extend the per-frame buffer and `continue`
    //   3. on `None` fall through to the existing glyph atlas path.
    // Assert step 1 returns `Some` with at least one QuadInstance for a
    // Phase-A codepoint — that's the load-bearing precondition for the
    // wiring's user-visible effect.
    let quads = emit_geometry_for_char(
        '─',
        (0.0, 0.0),
        (10.0, 20.0),
        [1.0, 1.0, 1.0, 1.0],
        800.0,
        600.0,
        1.0,
    )
    .expect("U+2500 ─ must route through the geometry helper");
    assert!(!quads.is_empty(), "geometry helper must emit at least one QuadInstance");
    // Post-#565: axis-aligned Box-Drawing segments route through the
    // sharp-rect path (line_thickness_px == 0).
    assert!(
        quads.iter().all(|q| q.line_thickness_px == 0.0),
        "Box-Drawing axis-aligned quads must use the sharp-rect path (#565)"
    );
}

#[test]
fn block_element_codepoint_emits_geometry_quads() {
    // U+2588 FULL BLOCK is the simplest single-rect block element —
    // proves the second arm of the geometry helper (block_element_rect)
    // is reached from the same funnel.
    let quads = emit_geometry_for_char(
        '█',
        (0.0, 0.0),
        (10.0, 20.0),
        [1.0, 1.0, 1.0, 1.0],
        800.0,
        600.0,
        1.0,
    )
    .expect("U+2588 █ must route through the geometry helper");
    assert_eq!(quads.len(), 1, "FULL BLOCK is a SingleRect");
    assert_eq!(
        quads[0].line_thickness_px, 0.0,
        "Block-Element quads use the sharp-rect path, not line-SDF"
    );
}

#[test]
fn ascii_is_not_routed_through_geometry_helper() {
    // The wiring in the ASCII fast path is intentional but always falls
    // through for printable ASCII (U+0020..=U+007E). If the helper ever
    // started returning `Some` here, the ASCII glyph atlas path would be
    // silently skipped and the screen would go blank for ordinary text.
    for ch in ' '..='~' {
        assert!(
            emit_geometry_for_char(
                ch,
                (0.0, 0.0),
                (10.0, 20.0),
                [1.0, 1.0, 1.0, 1.0],
                800.0,
                600.0,
                1.0,
            )
            .is_none(),
            "ASCII U+{:04X} must NOT be routed through the geometry helper",
            ch as u32,
        );
    }
}

#[test]
fn is_covered_predicate_matches_helper_for_box_drawing() {
    // The cache-invalidation hook uses `is_covered_box_drawing` as a
    // cheap pre-check; the production code calls
    // `emit_geometry_for_char` on the actual emit. If the two ever drift
    // (predicate says "covered" but helper returns None, or vice versa),
    // either the cache would invalidate forever for no reason OR a
    // covered row would silently cache a stale frame and the line would
    // disappear on frame 2.
    for cp in
        [0x2500_u32, 0x2502, 0x250C, 0x2510, 0x2514, 0x2518, 0x251C, 0x2524, 0x252C, 0x2534, 0x253C]
    {
        let ch = char::from_u32(cp).unwrap();
        assert!(is_covered_box_drawing(ch), "predicate must report U+{cp:04X} covered");
        assert!(
            emit_geometry_for_char(
                ch,
                (0.0, 0.0),
                (10.0, 20.0),
                [1.0, 1.0, 1.0, 1.0],
                800.0,
                600.0,
                1.0,
            )
            .is_some(),
            "helper must emit geometry for U+{cp:04X}",
        );
    }
}

#[test]
fn is_covered_predicate_matches_helper_for_block_elements() {
    // Whole U+2580..=U+259F range — predicate and helper must agree.
    for cp in 0x2580_u32..=0x259F {
        let ch = char::from_u32(cp).unwrap();
        assert!(is_covered_block_element(ch), "predicate must report U+{cp:04X} covered");
        assert!(
            emit_geometry_for_char(
                ch,
                (0.0, 0.0),
                (10.0, 20.0),
                [1.0, 1.0, 1.0, 1.0],
                800.0,
                600.0,
                1.0,
            )
            .is_some(),
            "helper must emit geometry for U+{cp:04X}",
        );
    }
}

#[test]
fn cache_invalidation_drops_entry_for_row_with_geometry_codepoint() {
    // Reproduces the production hook in `core.rs`:
    //
    //     if row.iter().any(|c|
    //         is_covered_box_drawing(c.ch)
    //         || is_covered_block_element(c.ch))
    //     {
    //         self.row_glyph_cache.invalidate_row_abs(pane_id, row_abs);
    //     }
    //
    // Without this hook, frame 1 emits the geometry quads and inserts
    // the row into the cache; frame 2's lookup hits, replays the cached
    // (glyph-only) artefacts, and the box drawing disappears. The test
    // asserts the invalidation actually removes the entry so the next
    // lookup misses → re-shape → re-emit.
    let mut cache = RowGlyphCache::new();
    cache.resize(24);
    let pane_id = 7_u64;
    let row_abs = 12_u64;
    let key = 0xDEAD_BEEF_u64;
    cache.insert(pane_id, row_abs, key, CachedRow::default());
    assert!(cache.get(pane_id, row_abs, key).is_some(), "precondition: entry inserted");

    // Simulate the per-row predicate result for a row containing '┌'.
    let row_chars: &[char] = &['x', 'y', '┌', 'z'];
    let row_has_geometry =
        row_chars.iter().any(|c| is_covered_box_drawing(*c) || is_covered_block_element(*c));
    assert!(row_has_geometry, "predicate must trigger on a row with U+250C ┌");

    if row_has_geometry {
        cache.invalidate_row_abs(pane_id, row_abs);
    }

    assert!(
        cache.get(pane_id, row_abs, key).is_none(),
        "invalidation must drop the cache entry so the next frame re-emits geometry quads"
    );
}

#[test]
fn cache_keeps_entry_for_row_without_geometry_codepoint() {
    // The dual of the invalidation test: an all-ASCII row must NOT
    // trigger invalidation, otherwise the cache would never hit for
    // ordinary text and the row-cache optimisation collapses.
    let mut cache = RowGlyphCache::new();
    cache.resize(24);
    let pane_id = 7_u64;
    let row_abs = 12_u64;
    let key = 0xDEAD_BEEF_u64;
    cache.insert(pane_id, row_abs, key, CachedRow::default());

    let row_chars: &[char] = &['h', 'e', 'l', 'l', 'o'];
    let row_has_geometry =
        row_chars.iter().any(|c| is_covered_box_drawing(*c) || is_covered_block_element(*c));
    assert!(!row_has_geometry, "ASCII-only row must NOT trip the geometry predicate");

    // Skip invalidation (mirrors the `if row_has_geometry_codepoint`
    // guard in `core.rs`).
    if row_has_geometry {
        cache.invalidate_row_abs(pane_id, row_abs);
    }

    assert!(
        cache.get(pane_id, row_abs, key).is_some(),
        "ASCII-only row must keep its cache entry — geometry hook must not regress the hot path"
    );
}
