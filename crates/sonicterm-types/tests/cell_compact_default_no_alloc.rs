//! Epic #300 P1: building default cells must not allocate.
//!
//! The compact layout puts the rare attrs (hyperlink, extras) behind
//! an [`Option<Box<FatAttributes>>`]. The default cell — plain ASCII
//! space, no link, no extras — must leave the box as `None`.
//!
//! We assert this two ways:
//!
//! 1. [`Cell::has_fat`] returns `false` for the default and for cells
//!    built via [`Cell::plain`].
//! 2. A custom counting allocator confirms `Box::new`-shaped allocs
//!    do not happen across a default-cell construction burst.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use sonicterm_types::{Cell, CellFlags, Color};

struct CountingAlloc;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static A: CountingAlloc = CountingAlloc;

#[test]
fn default_cell_has_no_fat() {
    let c = Cell::default();
    assert!(!c.has_fat(), "Default Cell must not allocate FatAttributes");
}

#[test]
fn plain_constructor_has_no_fat() {
    let c = Cell::plain('x', Color::Default, Color::Default, CellFlags::empty());
    assert!(!c.has_fat(), "Cell::plain must not allocate FatAttributes");
}

#[test]
fn many_default_cells_do_not_allocate() {
    // Warm any one-shot lazy globals before measuring.
    let _warm: Vec<Cell> = (0..16).map(|_| Cell::default()).collect();
    drop(_warm);

    let before = ALLOCS.load(Ordering::Relaxed);
    let mut total_ch: u32 = 0;
    for _ in 0..10_000 {
        let c = Cell::default();
        // Touch ch so the optimizer can't elide the construction.
        total_ch = total_ch.wrapping_add(c.ch as u32);
    }
    let after = ALLOCS.load(Ordering::Relaxed);
    assert!(total_ch > 0);
    let delta = after - before;
    // Allow a handful of ambient runtime allocs (formatter init, panic
    // hook book-keeping). If the default cell allocated a Box per
    // construction we'd see ≥10_000 here, not ≤16.
    assert!(
        delta <= 16,
        "10k default Cell constructions allocated {delta} times; expected ~0 (max 16)"
    );
}
