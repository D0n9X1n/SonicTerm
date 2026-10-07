#![warn(clippy::min_ident_chars)]
//! The perf scenarios under a counting global allocator.
//!
//! An allocator is fixed when a binary is built, so allocation counts come
//! from this second root over the same modules. Timed runs use
//! `perf_scenarios`, which declares no allocator; only allocations per frame
//! are read from this binary.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(any(target_os = "macos", windows, test))]
mod atlas_retry;
#[cfg(any(target_os = "macos", windows, test))]
mod attribution;
mod cli;
#[cfg(any(target_os = "macos", windows, test))]
mod counters;
#[cfg(any(windows, test))]
mod delivery;
#[cfg(any(target_os = "macos", windows, test))]
mod dispatch_timeline;
#[cfg(any(target_os = "macos", windows))]
mod probe;
#[cfg(any(target_os = "macos", windows, test))]
mod record;
#[cfg(any(target_os = "macos", windows, test))]
mod scan_throttle;
mod scenarios;
#[cfg(any(target_os = "macos", windows, test))]
mod transition;
#[cfg(any(target_os = "macos", windows, test))]
mod waits;
#[cfg(any(target_os = "macos", windows, test))]
mod workload;

/// Allocation calls since process start: `alloc`, `alloc_zeroed` and `realloc`.
static ALLOCATION_COUNT: AtomicU64 = AtomicU64::new(0);

/// Forwards every request to the system allocator and counts each allocation call.
struct CountingAllocator;

// SAFETY: every method forwards its exact arguments to `System`, which upholds the
// `GlobalAlloc` contract; the counter is a lock-free atomic and never allocates.
unsafe impl GlobalAlloc for CountingAllocator {
    // Ordering: Relaxed suffices; the counter is a statistic that orders no other memory.
    // SAFETY: the caller's `layout` contract passes unchanged to `System::alloc`.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        // SAFETY: `layout` satisfies `GlobalAlloc::alloc`'s contract, forwarded as given.
        unsafe { System.alloc(layout) }
    }

    // Ordering: Relaxed suffices; the counter is a statistic that orders no other memory.
    // SAFETY: the caller's `layout` contract passes unchanged to `System::alloc_zeroed`.
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        // SAFETY: `layout` satisfies `GlobalAlloc::alloc_zeroed`'s contract, forwarded as given.
        unsafe { System.alloc_zeroed(layout) }
    }

    // SAFETY: `ptr` came from this allocator, which is `System` underneath, with this `layout`.
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` and `layout` describe a live `System` block, forwarded as given.
        unsafe { System.dealloc(ptr, layout) }
    }

    // Ordering: Relaxed suffices; the counter is a statistic that orders no other memory.
    // SAFETY: `ptr`, `layout` and `new_size` meet `GlobalAlloc::realloc`'s contract for `System`.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
        // SAFETY: `ptr` is a live `System` block of `layout`, and `new_size` is the caller's request.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

// Ordering: Relaxed suffices; the probe reads the counter on the thread that dispatches frames.
fn allocation_count() -> u64 {
    ALLOCATION_COUNT.load(Ordering::Relaxed)
}

fn main() -> std::process::ExitCode {
    cli::run(Some(allocation_count))
}
