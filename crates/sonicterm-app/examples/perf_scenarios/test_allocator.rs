//! A counting global allocator for the perf scenarios' unit tests only: it lets a test assert that a
//! piece of work allocates nothing. Counting is per thread and off by default, so tests running in
//! parallel never see each other's allocations. Timed runs never compile this module.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    /// Whether this thread is counting allocations.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    /// Allocation calls this thread made while counting.
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

/// Note one allocation call on this thread, when it is counting. A thread whose locals are already
/// gone counts nothing.
fn note_allocation() {
    let counting = COUNTING.try_with(Cell::get).unwrap_or(false);
    if counting {
        // When: this thread is inside `allocations_during`, the call is counted.
        let _ = COUNT.try_with(|count| count.set(count.get() + 1));
    }
}

/// Forwards every request to the system allocator and counts each allocation call while counting.
struct CountingAllocator;

// SAFETY: every method forwards its exact arguments to `System`, which upholds the `GlobalAlloc`
// contract; the counter is a const-initialized thread local with no destructor, so it never allocates.
unsafe impl GlobalAlloc for CountingAllocator {
    // SAFETY: the caller's `layout` contract passes unchanged to `System::alloc`.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_allocation();
        // SAFETY: `layout` satisfies `GlobalAlloc::alloc`'s contract, forwarded as given.
        unsafe { System.alloc(layout) }
    }

    // SAFETY: the caller's `layout` contract passes unchanged to `System::alloc_zeroed`.
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note_allocation();
        // SAFETY: `layout` satisfies `GlobalAlloc::alloc_zeroed`'s contract, forwarded as given.
        unsafe { System.alloc_zeroed(layout) }
    }

    // SAFETY: `ptr` came from this allocator, which is `System` underneath, with this `layout`.
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` and `layout` describe a live `System` block, forwarded as given.
        unsafe { System.dealloc(ptr, layout) }
    }

    // SAFETY: `ptr`, `layout` and `new_size` meet `GlobalAlloc::realloc`'s contract for `System`.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_allocation();
        // SAFETY: `ptr` is a live `System` block of `layout`, and `new_size` is the caller's request.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// This thread's counting state as it was before a counted call, put back when the call ends.
struct Restore {
    /// Whether the thread was counting.
    counting: bool,
    /// Its count then.
    count: u64,
}

// Lifecycle: Restore puts this thread's COUNTING and COUNT back on every exit from the counted work, unwinding included.
impl Drop for Restore {
    fn drop(&mut self) {
        COUNTING.with(|counting| counting.set(self.counting));
        COUNT.with(|count| count.set(self.count));
    }
}

/// Run `work` on this thread and return its result with the allocation calls it made.
///
/// # Panics
///
/// When called inside another `allocations_during` on the same thread: the outer count would then
/// include the inner work, so a nested call is refused before any state changes. A panic in `work`
/// propagates, and the thread's counting state is restored first.
pub(crate) fn allocations_during<Output>(work: impl FnOnce() -> Output) -> (Output, u64) {
    let counting = COUNTING.with(Cell::get);
    assert!(!counting, "allocations_during cannot be nested");
    let restore = Restore { counting, count: COUNT.with(Cell::get) };
    COUNT.with(|count| count.set(0));
    COUNTING.with(|counting| counting.set(true));
    let output = work();
    let count = COUNT.with(Cell::get);
    drop(restore);
    (output, count)
}

#[cfg(test)]
#[path = "test_allocator_tests.rs"]
mod test_allocator_tests;
