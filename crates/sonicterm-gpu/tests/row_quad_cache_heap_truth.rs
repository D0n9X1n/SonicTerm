//! Checks row-quad cache reporting against live heap rather than its formula.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use sonicterm_gpu::quad::QuadInstance;
use sonicterm_gpu::row_quad_cache::{CachedRowQuads, LineQuadCache};

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

/// Reported table and nested quad-vector capacities track the heap the cache retains.
#[test]
fn reported_quad_cache_bytes_track_live_heap() {
    const ROWS: u16 = 32;
    const QUADS: usize = 512;
    let before = held();
    let mut cache = LineQuadCache::new();
    cache.resize(ROWS);
    for index in 0..usize::from(ROWS) * 4 {
        cache.insert(
            7,
            index as u64,
            index as u64,
            CachedRowQuads { quads: vec![QuadInstance::default(); QUADS] },
        );
    }

    let truth = held().saturating_sub(before);
    let reported = cache.retained_amount();
    assert_eq!(reported.items, usize::from(ROWS) * 4);
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
