//! Pane isolation tests for `RowGlyphCache`.
//!
//! Pre-#199 the cache was keyed by `(abs_row, hash)`. PR #199 introduces
//! a per-pane render loop where every pane writes/reads the cache in
//! the same frame; without pane separation in the key, two panes whose
//! rows happen to hash identically would clobber each other's glyphs.
//!
//! This file pins the contract: identical `(abs_row, hash)` under two
//! different `pane_id`s must not collide, and per-pane invalidation
//! must leave peer panes untouched.

use sonicterm_text::row_glyph_cache::{CachedRow, RowGlyphCache};
use sonicterm_text::GlyphInstance;

fn distinct_row(marker: f32) -> CachedRow {
    // The instances themselves are irrelevant for the isolation
    // contract — we only need two CachedRows that compare unequal so a
    // collision would be observable. Stash the marker in the first
    // component of the synthetic glyph instance's rect.
    CachedRow {
        glyphs: vec![GlyphInstance {
            rect: [marker, 0.0, 0.0, 0.0],
            uv: [0.0, 0.0, 0.0, 0.0],
            color: [1.0, 1.0, 1.0, 1.0],
            flags: [0.0, 0.0, 0.0, 0.0],
        }],
        underlines: Vec::new(),
        tofu: Vec::new(),
        missing_chars: Vec::new(),
        geometry_quads: Vec::new(),
    }
}

#[test]
fn same_row_and_hash_in_two_panes_do_not_collide() {
    let mut cache = RowGlyphCache::new();
    cache.resize(8);

    let abs_row: u64 = 42;
    let hash: u64 = 0xdead_beef;

    cache.insert(1, abs_row, hash, distinct_row(1.0));
    cache.insert(2, abs_row, hash, distinct_row(2.0));

    let a = cache.get(1, abs_row, hash).expect("pane 1 entry present");
    let b = cache.get(2, abs_row, hash).expect("pane 2 entry present");

    assert_eq!(a.glyphs.len(), 1);
    assert_eq!(b.glyphs.len(), 1);
    assert_eq!(a.glyphs[0].rect[0], 1.0, "pane 1 keeps its own row");
    assert_eq!(b.glyphs[0].rect[0], 2.0, "pane 2 keeps its own row");
    assert_eq!(cache.len(), 2, "both entries coexist");
}

#[test]
fn invalidate_row_abs_is_scoped_to_the_named_pane() {
    let mut cache = RowGlyphCache::new();
    cache.resize(8);

    let abs_row: u64 = 7;
    let hash: u64 = 0x0123;

    cache.insert(10, abs_row, hash, distinct_row(10.0));
    cache.insert(20, abs_row, hash, distinct_row(20.0));

    cache.invalidate_row_abs(10, abs_row);

    assert!(cache.get(10, abs_row, hash).is_none(), "pane 10's row dropped");
    assert!(cache.get(20, abs_row, hash).is_some(), "pane 20's row preserved");
}

#[test]
fn invalidate_pane_drops_only_that_panes_entries() {
    let mut cache = RowGlyphCache::new();
    cache.resize(16);

    for row in 0..3 {
        cache.insert(1, row, row, distinct_row(row as f32));
        cache.insert(2, row, row, distinct_row(row as f32 + 100.0));
    }
    assert_eq!(cache.len(), 6);

    cache.invalidate_pane(1);

    assert_eq!(cache.len(), 3, "only pane 2 survives");
    for row in 0..3 {
        assert!(cache.get(1, row, row).is_none());
        assert!(cache.get(2, row, row).is_some());
    }
}

#[test]
fn invalidate_all_clears_every_pane() {
    let mut cache = RowGlyphCache::new();
    cache.resize(8);
    cache.insert(1, 0, 0, distinct_row(1.0));
    cache.insert(2, 0, 0, distinct_row(2.0));
    cache.invalidate_all();
    assert!(cache.is_empty());
}
