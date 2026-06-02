//! Regression test for #348 — after a region scroll mutates row
//! contents, the `LineQuadCache` must return fresh data (cache miss)
//! for every affected line. The renderer drives this by calling
//! `invalidate_row_abs` for every row reported by
//! `grid.dirty_rows()`, so this test simulates that exact dance.

use sonicterm_gpu::quad::QuadInstance;
use sonicterm_gpu::row_quad_cache::{row_quad_hash, CachedRowQuads, LineQuadCache};
use sonicterm_grid::grid::Cell;
use sonicterm_grid::grid::{CellFlags, Color};

fn row_of(ch: char) -> Vec<Cell> {
    (0..20).map(|_| Cell::plain(ch, Color::Default, Color::Default, CellFlags::empty())).collect()
}

fn quad_for(tag: f32) -> QuadInstance {
    QuadInstance::sharp([tag, 0.0, 0.1, 0.1], [tag, tag, tag, 1.0])
}

#[test]
fn region_scroll_evicts_affected_rows_via_dirty_invalidation() {
    let mut cache = LineQuadCache::new();
    cache.resize(30);

    let pane_id: u64 = 1;
    let style_rev: u64 = 0;
    let geom = (8.0_f32, 16.0_f32, 0.0_f32, 0.0_f32, 640.0_f32, 480.0_f32);

    // Frame 1: cache one cluster per row, rows 5..20, content 'A'.
    let mut keys_before = Vec::new();
    for r in 5..20u64 {
        let row = row_of('A');
        let key = row_quad_hash(
            0, r as usize, &row, style_rev, geom.0, geom.1, geom.2, geom.3, geom.4, geom.5, None,
        );
        cache.insert(pane_id, r, key, CachedRowQuads { quads: vec![quad_for(r as f32)] });
        keys_before.push((r, key));
    }
    assert_eq!(cache.len(), 15);

    // Renderer reaction to a region scroll: every dirty row's slot is
    // dropped before re-emission. Simulate rows 5..=19 being dirty.
    for r in 5..=19u64 {
        cache.invalidate_row_abs(pane_id, r);
    }

    // All previously cached rows must now miss.
    for (r, key) in &keys_before {
        assert!(
            cache.get(pane_id, *r, *key).is_none(),
            "row {r} must miss after dirty invalidation"
        );
    }

    // Frame 2: rows now contain 'B' (shifted-in content). Re-insert
    // and confirm the fresh hash returns the fresh data, not stale.
    for r in 5..20u64 {
        let row = row_of('B');
        let key = row_quad_hash(
            0, r as usize, &row, style_rev, geom.0, geom.1, geom.2, geom.3, geom.4, geom.5, None,
        );
        // A naively-recomputed hash on the OLD ('A') content would
        // match an old entry — that's the #348 failure mode. With the
        // dirty-row invalidation above, no entry exists for `r` so
        // the hash difference does not matter for correctness, but we
        // still assert miss-then-insert-then-hit to lock the contract.
        assert!(cache.get(pane_id, r, key).is_none(), "row {r} must miss before re-insert");
        cache.insert(pane_id, r, key, CachedRowQuads { quads: vec![quad_for(-(r as f32))] });
        let hit = cache.get(pane_id, r, key).expect("just inserted");
        assert_eq!(hit.quads.len(), 1);
        assert_eq!(hit.quads[0].rect[0], -(r as f32), "fresh quad, not stale 'A' quad");
    }
}

#[test]
fn unaffected_rows_outside_region_keep_their_cache_entries() {
    let mut cache = LineQuadCache::new();
    cache.resize(30);

    let pane_id: u64 = 1;
    let geom = (8.0_f32, 16.0_f32, 0.0_f32, 0.0_f32, 640.0_f32, 480.0_f32);

    // Cache rows 0..30.
    let mut keys = Vec::new();
    for r in 0..30u64 {
        let row = row_of('X');
        let key = row_quad_hash(
            0, r as usize, &row, 0, geom.0, geom.1, geom.2, geom.3, geom.4, geom.5, None,
        );
        cache.insert(pane_id, r, key, CachedRowQuads { quads: vec![quad_for(r as f32)] });
        keys.push(key);
    }

    // Region scroll [5, 19]: only those rows get invalidated.
    for r in 5..=19u64 {
        cache.invalidate_row_abs(pane_id, r);
    }

    // Rows 0..5 and 20..30 must still hit.
    for r in 0..5u64 {
        assert!(cache.get(pane_id, r, keys[r as usize]).is_some(), "row {r} kept");
    }
    for r in 20..30u64 {
        assert!(cache.get(pane_id, r, keys[r as usize]).is_some(), "row {r} kept");
    }
}
