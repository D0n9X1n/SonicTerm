//! Checks row-glyph cache reporting against live heap rather than its formula.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use sonicterm_text::row_glyph_cache::{
    CachedRow, RowGlyph, RowGlyphBits, RowGlyphCache, RowGlyphKind, RowTofu, UnderlineRun,
};
use sonicterm_types::{Color, UnderlineStyle};

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

struct Counting;

// SAFETY: Operations forward exact pointers, layouts, and sizes to `System`; atomic bookkeeping allocates nothing and cannot re-enter.
unsafe impl GlobalAlloc for Counting {
    // SAFETY: `layout` is forwarded unchanged after allocation-free bookkeeping.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        // SAFETY: `layout` is the valid layout received from the allocator caller.
        unsafe { System.alloc(layout) }
    }

    // SAFETY: `ptr` and `layout` are forwarded unchanged after allocation-free bookkeeping.
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: `ptr` and `layout` are the matching pair received from the allocator caller.
        unsafe { System.dealloc(ptr, layout) }
    }

    // SAFETY: `ptr`, `layout`, and `new_size` are forwarded unchanged after allocation-free bookkeeping.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE_BYTES.fetch_add(new_size.saturating_sub(layout.size()), Ordering::Relaxed);
        LIVE_BYTES.fetch_sub(layout.size().saturating_sub(new_size), Ordering::Relaxed);
        // SAFETY: all arguments are the exact valid values received from the allocator caller.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn held() -> usize {
    LIVE_BYTES.load(Ordering::Relaxed)
}

/// A row of `width` glyph records with proportionate underlines, tofu and missing characters.
fn row(width: usize) -> CachedRow {
    let bits = RowGlyphBits {
        kind: RowGlyphKind::Natural,
        is_color: false,
        is_subpixel: false,
        marker_fit_eligible: false,
        is_wide: false,
        has_extras: false,
        cluster_cells: 1,
    };
    CachedRow {
        glyphs: vec![
            RowGlyph {
                uv: [0.0; 4],
                color: [0.0; 4],
                raster_offset: [0.0; 2],
                shape_offset: [0.0; 2],
                raster_size: [0.0; 2],
                lead_col: 0,
                end_col: 1,
                kind_and_bits: bits.pack(),
            };
            width
        ],
        underlines: vec![
            UnderlineRun {
                start_col: 0,
                end_col: 1,
                style: UnderlineStyle::Single,
                color: Color::Default,
            };
            width / 8
        ],
        tofu: vec![
            RowTofu { lead_col: 0, inset: 1.0, width: 1.0, height: 1.0, color: [0; 4] };
            width / 16
        ],
        missing_chars: vec!['x'; width / 32],
    }
}

/// Reported payload and tracking capacities track the heap the cache retains, through quota
/// eviction, and dropping the cache returns every byte.
#[test]
fn reported_glyph_cache_bytes_track_live_heap() {
    const ROWS: u16 = 32;
    const WIDTH: usize = 512;
    let before = held();
    let mut cache = RowGlyphCache::new();
    cache.begin_frame(&[(7, ROWS, WIDTH as u16)]);
    for index in 0..usize::from(ROWS) * 4 {
        assert!(cache.insert(7, index as u64 + 1, 1, row(WIDTH)));
    }

    let truth = held().saturating_sub(before);
    let reported = cache.retained_amount();
    assert_eq!(reported.items, cache.len());
    assert!(reported.items > usize::from(ROWS), "the fixture holds more than one viewport");
    assert!(truth > 1024 * 1024, "fixture retained only {truth} bytes");
    assert!(
        reported.bytes + truth / 100 + 4096 >= truth,
        "reported {} understates live heap {truth}",
        reported.bytes
    );
    assert!(
        reported.bytes <= truth + truth / 100 + 4096,
        "reported {} overstates live heap {truth}",
        reported.bytes
    );

    drop(cache);
    let after = held().saturating_sub(before);
    assert!(after < 4096, "dropping the cache left {after} measured bytes live");
}
