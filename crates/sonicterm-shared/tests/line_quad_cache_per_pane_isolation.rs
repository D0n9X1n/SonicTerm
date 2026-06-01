//! Per-pane isolation tests for `LineQuadCache` (Epic #300 P2).
//!
//! Mirrors `sonicterm_text::tests::row_glyph_cache_pane_isolation`. Two
//! panes that hash identical row content must not share a cache slot;
//! `pane_id` is part of the key tuple precisely so split-pane redraws
//! don't clobber each other's quads.

use sonicterm_gpu::quad::QuadInstance;
use sonicterm_shared::render::row_quad_cache::{CachedRowQuads, LineQuadCache};

fn marker_row(marker: f32) -> CachedRowQuads {
    CachedRowQuads {
        quads: vec![QuadInstance::sharp([marker, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0])],
    }
}

#[test]
fn same_row_and_hash_in_two_panes_do_not_collide() {
    let mut cache = LineQuadCache::new();
    cache.resize(8);

    let abs_row: u64 = 17;
    let hash: u64 = 0xC0FF_EE00;

    cache.insert(1, abs_row, hash, marker_row(1.0));
    cache.insert(2, abs_row, hash, marker_row(2.0));

    let a = cache.get(1, abs_row, hash).expect("pane 1 entry");
    let b = cache.get(2, abs_row, hash).expect("pane 2 entry");
    assert_eq!(a.quads[0].rect[0], 1.0, "pane 1 untouched");
    assert_eq!(b.quads[0].rect[0], 2.0, "pane 2 untouched");
    assert_eq!(cache.len(), 2);
}

#[test]
fn invalidate_pane_leaves_peer_alone() {
    let mut cache = LineQuadCache::new();
    cache.resize(8);
    cache.insert(1, 0, 1, marker_row(1.0));
    cache.insert(1, 1, 1, marker_row(1.0));
    cache.insert(2, 0, 1, marker_row(2.0));
    assert_eq!(cache.len(), 3);
    cache.invalidate_pane(1);
    assert!(cache.get(1, 0, 1).is_none());
    assert!(cache.get(1, 1, 1).is_none());
    assert!(cache.get(2, 0, 1).is_some(), "peer pane untouched");
}
