//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::pty::ShellSpawnOpts;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// The unit-test binary's allocator: `System`, which also records whether a block of the one
/// watched size was freed, so a test can see when a buffer is released on another thread.
struct WatchingAllocator;

/// The block size the allocator watches; 0 watches none, as no block has size 0.
static WATCHED_SIZE: AtomicUsize = AtomicUsize::new(0);

/// Whether a block of the watched size was freed or moved since the size was set.
static WATCHED_FREED: AtomicBool = AtomicBool::new(false);

/// Record that a block of `byte_count` bytes was released, if that is the watched size.
fn note_release(byte_count: usize) {
    if byte_count == WATCHED_SIZE.load(Ordering::SeqCst) {
        WATCHED_FREED.store(true, Ordering::SeqCst);
    }
}

// SAFETY: every method forwards its exact arguments to `System`, which upholds the `GlobalAlloc`
// contract; the watch is two atomics that never allocate.
unsafe impl std::alloc::GlobalAlloc for WatchingAllocator {
    // SAFETY: the caller's `layout` contract is forwarded unchanged to `System::alloc`.
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        // SAFETY: `layout` satisfies `GlobalAlloc::alloc`'s contract and is forwarded unchanged.
        unsafe { std::alloc::System.alloc(layout) }
    }

    // SAFETY: `block` came from this allocator, which is `System` underneath, with this `layout`.
    unsafe fn dealloc(&self, block: *mut u8, layout: std::alloc::Layout) {
        note_release(layout.size());
        // SAFETY: `block` was returned by `System` through this allocator with this `layout`.
        unsafe { std::alloc::System.dealloc(block, layout) }
    }

    // SAFETY: `block`, `layout` and `new_size` meet `GlobalAlloc::realloc`'s contract for `System`.
    unsafe fn realloc(
        &self,
        block: *mut u8,
        layout: std::alloc::Layout,
        new_size: usize,
    ) -> *mut u8 {
        note_release(layout.size());
        // SAFETY: `block` is a live `System` block of `layout`; `new_size` is the caller's request.
        unsafe { std::alloc::System.realloc(block, layout, new_size) }
    }
}

#[global_allocator]
static WATCHING_ALLOCATOR: WatchingAllocator = WatchingAllocator;

/// Watch blocks of `size` bytes from now on, forgetting any earlier release.
pub(crate) fn watch_frees_of(size: usize) {
    WATCHED_FREED.store(false, Ordering::SeqCst);
    WATCHED_SIZE.store(size, Ordering::SeqCst);
}

/// Whether a watched block was freed since [`watch_frees_of`].
pub(crate) fn watched_block_freed() -> bool {
    WATCHED_FREED.load(Ordering::SeqCst)
}

/// The watch sees a block of the watched size freed, and only that size.
#[test]
fn watching_allocator_sees_the_watched_size_freed() {
    const WATCHED: usize = 1_000_003;
    watch_frees_of(WATCHED);
    let other = std::hint::black_box(vec![0_u8; 17]);
    drop(other);
    assert!(!watched_block_freed());
    drop(std::hint::black_box(vec![1_u8; WATCHED]));
    assert!(watched_block_freed());
    watch_frees_of(0);
}

#[test]
fn exports_pty_spawn_options() {
    let opts = ShellSpawnOpts::default();
    assert!(!opts.clean_e2e);
}

#[test]
fn windows_process_snapshot_uses_aligned_storage() {
    const SOURCE: &str = include_str!("foreground_proc.rs");
    assert!(SOURCE.contains("let mut buf: Vec<u64>"));
    assert!(
        SOURCE.contains("align_of::<u64>() >= std::mem::align_of::<SystemProcessInformation>()")
    );
    assert!(!SOURCE.contains("let mut buf: Vec<u8>"));
}
