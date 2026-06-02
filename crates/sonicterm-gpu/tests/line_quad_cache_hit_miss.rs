//! Hit/miss regression tests for `LineQuadCache` (Epic #300 P2).
//!
//! Mirrors the structure of `sonicterm_text::tests::row_glyph_cache` for
//! the quad cache layer added in Epic #300 Phase P2.

use sonicterm_gpu::quad::QuadInstance;
use sonicterm_gpu::row_quad_cache::{row_quad_hash, CachedRowQuads, LineQuadCache};
use sonicterm_grid::grid::Cell;

fn dummy_row(s: &str) -> Vec<Cell> {
    use sonicterm_grid::grid::{CellFlags, Color};

    s.chars()
        .map(|c| Cell::plain(c, Color::default(), Color::default(), CellFlags::empty()))
        .collect()
}

fn dummy_quad(x: f32) -> QuadInstance {
    QuadInstance::sharp([x, 0.0, 0.1, 0.1], [1.0, 0.0, 0.0, 1.0])
}

#[test]
fn empty_cache_misses_then_inserts() {
    let mut cache = LineQuadCache::new();
    cache.resize(24);
    let row = dummy_row("hello world");
    let key = row_quad_hash(0, 0, &row, 0, 8.0, 16.0, 0.0, 0.0, 640.0, 384.0, None, 1.0);
    assert!(cache.get(0, 0, key).is_none());
    cache.insert(0, 0, key, CachedRowQuads { quads: vec![dummy_quad(0.1)] });
    let hit = cache.get(0, 0, key).expect("just inserted");
    assert_eq!(hit.quads.len(), 1);
}

#[test]
fn same_row_same_key_hits() {
    let mut cache = LineQuadCache::new();
    cache.resize(24);
    let row = dummy_row("status: OK");
    let k1 = row_quad_hash(0, 5, &row, 0, 8.0, 16.0, 0.0, 0.0, 640.0, 384.0, None, 1.0);
    let k2 = row_quad_hash(0, 5, &row, 0, 8.0, 16.0, 0.0, 0.0, 640.0, 384.0, None, 1.0);
    assert_eq!(k1, k2, "same inputs must produce same hash");
    cache.insert(0, 5, k1, CachedRowQuads { quads: vec![dummy_quad(0.2), dummy_quad(0.3)] });
    let hit = cache.get(0, 5, k2).expect("hit on identical key");
    assert_eq!(hit.quads.len(), 2);
}

#[test]
fn content_change_misses() {
    let mut cache = LineQuadCache::new();
    cache.resize(24);
    let row_a = dummy_row("foo");
    let row_b = dummy_row("bar");
    let k_a = row_quad_hash(0, 0, &row_a, 0, 8.0, 16.0, 0.0, 0.0, 640.0, 384.0, None, 1.0);
    let k_b = row_quad_hash(0, 0, &row_b, 0, 8.0, 16.0, 0.0, 0.0, 640.0, 384.0, None, 1.0);
    assert_ne!(k_a, k_b, "different cell content => different hash");
    cache.insert(0, 0, k_a, CachedRowQuads { quads: vec![dummy_quad(0.1)] });
    assert!(cache.get(0, 0, k_b).is_none(), "miss after content change");
}

#[test]
fn selection_overlap_misses() {
    let mut cache = LineQuadCache::new();
    cache.resize(24);
    let row = dummy_row("text");
    let k_none = row_quad_hash(0, 3, &row, 0, 8.0, 16.0, 0.0, 0.0, 640.0, 384.0, None, 1.0);
    // Selection covering rows 2..=4 overlaps row 3.
    let k_sel =
        row_quad_hash(0, 3, &row, 0, 8.0, 16.0, 0.0, 0.0, 640.0, 384.0, Some((2, 0, 4, 79)), 1.0);
    assert_ne!(k_none, k_sel, "selection overlap must perturb the hash");
    cache.insert(0, 3, k_none, CachedRowQuads { quads: vec![dummy_quad(0.1)] });
    assert!(cache.get(0, 3, k_sel).is_none(), "miss when selection appears");
}

#[test]
fn style_rev_bump_misses() {
    let mut cache = LineQuadCache::new();
    cache.resize(24);
    let row = dummy_row("themed");
    let k_old = row_quad_hash(0, 1, &row, 7, 8.0, 16.0, 0.0, 0.0, 640.0, 384.0, None, 1.0);
    let k_new = row_quad_hash(0, 1, &row, 8, 8.0, 16.0, 0.0, 0.0, 640.0, 384.0, None, 1.0);
    assert_ne!(k_old, k_new);
    cache.insert(0, 1, k_old, CachedRowQuads { quads: vec![dummy_quad(0.4)] });
    assert!(cache.get(0, 1, k_new).is_none());
}

#[test]
fn invalidate_row_abs_drops_one_entry() {
    let mut cache = LineQuadCache::new();
    cache.resize(24);
    let row = dummy_row("ab");
    let k = row_quad_hash(0, 0, &row, 0, 8.0, 16.0, 0.0, 0.0, 640.0, 384.0, None, 1.0);
    cache.insert(0, 0, k, CachedRowQuads { quads: vec![dummy_quad(0.1)] });
    cache.insert(0, 1, k, CachedRowQuads { quads: vec![dummy_quad(0.2)] });
    assert_eq!(cache.len(), 2);
    cache.invalidate_row_abs(0, 0);
    assert!(cache.get(0, 0, k).is_none());
    assert!(cache.get(0, 1, k).is_some());
}
