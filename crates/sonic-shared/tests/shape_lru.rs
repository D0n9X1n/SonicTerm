//! Tests for the LRU eviction policy in [`sonic_shared::shape::ShapeCache`].
//!
//! The previous cache cleared the entire `HashMap` on overflow at 512
//! entries, causing a cold-cache stall every time a user opened a long
//! file. The replacement uses `lru::LruCache` with capacity 4096 and
//! evicts only the least-recently-used entry on overflow. These tests
//! pin both invariants so the regression cannot silently return:
//!
//! 1. After inserting 5000 distinct entries, the cache size is exactly
//!    the configured capacity (4096) — not 0 (the old clear-on-overflow
//!    behaviour) and not 5000 (no eviction at all).
//! 2. The 4096 most-recently-inserted entries are still present.
//! 3. The oldest entries (those evicted earliest) are gone.

use cosmic_text::FontSystem;
use sonic_core::grid::Cell;
use sonic_shared::{
    shape::{RunStyle, ShapeCache},
    swash_rasterizer::{SwashRasterizer, DEFAULT_RASTER_PX},
};

fn font_system() -> FontSystem {
    let mut fs = FontSystem::new();
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
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

/// Build a unique single-cell run for a given index. We synthesize the
/// text by formatting the index — each `i` produces a distinct cache
/// key so we can drive 5000 unique inserts with one tiny font.
fn cells_for_index(i: usize) -> Vec<(u16, Cell)> {
    let s = format!("k{i}");
    s.chars()
        .enumerate()
        .map(|(col, ch)| {
            let mut c = Cell::default();
            c.ch = ch;
            (col as u16, c)
        })
        .collect()
}

#[test]
fn lru_caps_at_capacity_and_evicts_oldest() {
    const N: usize = 5000;
    const CAP: usize = ShapeCache::CAPACITY;
    assert_eq!(CAP, 4096, "capacity contract: 4096 entries");

    let mut fs = font_system();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono Casual", DEFAULT_RASTER_PX);
    let mut cache = ShapeCache::new();
    assert_eq!(cache.capacity(), CAP);
    let style = RunStyle { bold: false, italic: false };

    for i in 0..N {
        let cells = cells_for_index(i);
        let _ = cache.get_or_shape(&mut r, "Rec Mono Casual", DEFAULT_RASTER_PX, style, &cells);
    }

    // Invariant 1: size pinned to capacity — not blown away (old
    // clear-on-overflow == 0 or some tiny number) and not unbounded
    // (== N).
    assert_eq!(
        cache.len(),
        CAP,
        "after {N} distinct inserts the cache must hold exactly {CAP} entries (LRU eviction, not clear-on-overflow)"
    );

    // Invariant 2: the most-recently-inserted CAP entries are present.
    for i in (N - CAP)..N {
        let cells = cells_for_index(i);
        assert!(
            cache.contains_run("Rec Mono Casual", DEFAULT_RASTER_PX, style, &cells),
            "recent entry {i} must still be in the cache"
        );
    }

    // Invariant 3: the oldest entries have been evicted.
    for i in 0..(N - CAP) {
        let cells = cells_for_index(i);
        assert!(
            !cache.contains_run("Rec Mono Casual", DEFAULT_RASTER_PX, style, &cells),
            "old entry {i} must have been evicted"
        );
    }
}

#[test]
fn lru_touch_keeps_recently_used_entries_alive() {
    // Insert CAP entries, then touch entry 0 (making it most recent),
    // then insert one more new entry. Entry 1 (now the LRU) must be
    // evicted; entry 0 must survive.
    const CAP: usize = ShapeCache::CAPACITY;

    let mut fs = font_system();
    let mut r = SwashRasterizer::new(&mut fs, "Rec Mono Casual", DEFAULT_RASTER_PX);
    let mut cache = ShapeCache::new();
    let style = RunStyle { bold: false, italic: false };

    for i in 0..CAP {
        let cells = cells_for_index(i);
        let _ = cache.get_or_shape(&mut r, "Rec Mono Casual", DEFAULT_RASTER_PX, style, &cells);
    }
    assert_eq!(cache.len(), CAP);

    // Touch entry 0 — moves it to most-recently-used.
    let cells0 = cells_for_index(0);
    let _ = cache.get_or_shape(&mut r, "Rec Mono Casual", DEFAULT_RASTER_PX, style, &cells0);

    // One overflow insert — should evict entry 1 (now the LRU), not 0.
    let overflow = cells_for_index(CAP);
    let _ = cache.get_or_shape(&mut r, "Rec Mono Casual", DEFAULT_RASTER_PX, style, &overflow);

    assert_eq!(cache.len(), CAP);
    assert!(
        cache.contains_run("Rec Mono Casual", DEFAULT_RASTER_PX, style, &cells0),
        "entry 0 was touched and must survive overflow"
    );
    let cells1 = cells_for_index(1);
    assert!(
        !cache.contains_run("Rec Mono Casual", DEFAULT_RASTER_PX, style, &cells1),
        "entry 1 was the LRU after touching 0 and must have been evicted"
    );
}
