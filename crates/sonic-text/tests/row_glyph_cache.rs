//! Tests for the row-glyph-cache hash + LRU policy.
//!
//! Migrated from inline `#[cfg(test)] mod tests` in `src/row_glyph_cache.rs`.

use sonic_text::row_glyph_cache::{row_hash, CachedRow, RowGlyphCache};
use sonic_types::Cell;

fn cells(n: usize) -> Vec<Cell> {
    (0..n)
        .map(|i| {
            let mut c = Cell::default();
            c.ch = char::from_u32(b'a' as u32 + i as u32).unwrap();
            c
        })
        .collect()
}

#[test]
fn hash_stable_for_identical_inputs() {
    let c = cells(4);
    let a = row_hash(0, 0, &c, 0, 8.0, 16.0, 2.0, None);
    let b = row_hash(0, 0, &c, 0, 8.0, 16.0, 2.0, None);
    assert_eq!(a, b);
}

#[test]
fn hash_changes_with_cells() {
    let c1 = cells(4);
    let mut c2 = c1.clone();
    c2[0].ch = 'Z';
    assert_ne!(
        row_hash(0, 0, &c1, 0, 8.0, 16.0, 2.0, None),
        row_hash(0, 0, &c2, 0, 8.0, 16.0, 2.0, None),
    );
}

#[test]
fn hash_changes_with_style_rev() {
    let c = cells(4);
    assert_ne!(
        row_hash(0, 0, &c, 0, 8.0, 16.0, 2.0, None),
        row_hash(0, 0, &c, 1, 8.0, 16.0, 2.0, None),
    );
}

#[test]
fn hash_changes_with_geometry() {
    let c = cells(4);
    assert_ne!(
        row_hash(0, 0, &c, 0, 8.0, 16.0, 2.0, None),
        row_hash(0, 0, &c, 0, 9.0, 16.0, 2.0, None),
    );
    assert_ne!(
        row_hash(0, 0, &c, 0, 8.0, 16.0, 2.0, None),
        row_hash(0, 0, &c, 0, 8.0, 16.0, 1.0, None),
    );
}

#[test]
fn hash_changes_when_selection_overlaps_row() {
    let c = cells(4);
    let none = row_hash(0, 2, &c, 0, 8.0, 16.0, 2.0, None);
    let overlap = row_hash(0, 2, &c, 0, 8.0, 16.0, 2.0, Some((1, 0, 3, 4)));
    assert_ne!(none, overlap);
}

#[test]
fn hash_ignores_selection_that_does_not_overlap_row() {
    let c = cells(4);
    let none = row_hash(0, 8, &c, 0, 8.0, 16.0, 2.0, None);
    let elsewhere = row_hash(0, 8, &c, 0, 8.0, 16.0, 2.0, Some((1, 0, 3, 4)));
    assert_eq!(none, elsewhere);
}

#[test]
fn selection_endpoints_normalised() {
    let c = cells(4);
    let a = row_hash(0, 2, &c, 0, 8.0, 16.0, 2.0, Some((1, 0, 3, 4)));
    let b = row_hash(0, 2, &c, 0, 8.0, 16.0, 2.0, Some((3, 4, 1, 0)));
    assert_eq!(a, b);
}

#[test]
fn abs_row_in_hash() {
    let c = cells(4);
    assert_ne!(
        row_hash(0, 0, &c, 0, 8.0, 16.0, 2.0, None),
        row_hash(10, 0, &c, 0, 8.0, 16.0, 2.0, None),
    );
}

#[test]
fn cache_get_insert_invalidate() {
    let mut cache = RowGlyphCache::new();
    let row = CachedRow::default();
    cache.insert(0, 5, 0xabcd, row.clone());
    assert!(cache.get(0, 5, 0xabcd).is_some());
    assert!(cache.get(0, 5, 0xdead).is_none());
    cache.invalidate_row_abs(0, 5);
    assert!(cache.get(0, 5, 0xabcd).is_none());
    cache.insert(0, 7, 0xabcd, row);
    assert_eq!(cache.len(), 1);
    cache.invalidate_all();
    assert!(cache.is_empty());
}

#[test]
fn cache_cap_resets_when_full() {
    let mut cache = RowGlyphCache::new();
    cache.resize(1);
    for i in 0..4 {
        cache.insert(0, i as u64, i as u64, CachedRow::default());
    }
    assert_eq!(cache.len(), 4);
    cache.insert(0, 99, 99, CachedRow::default());
    assert_eq!(cache.len(), 1);
    assert!(cache.get(0, 99, 99).is_some());
}

#[test]
fn resize_updates_cap_from_visible_rows() {
    let mut cache = RowGlyphCache::new();
    cache.resize(300);
    for i in 0..1100 {
        cache.insert(0, i, i, CachedRow::default());
    }
    assert_eq!(cache.len(), 1100);
}
