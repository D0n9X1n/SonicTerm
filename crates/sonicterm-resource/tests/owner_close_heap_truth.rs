//! Does closing an owner give its memory back?
//!
//! An owner that reaches `Closed` has satisfied the lifecycle contract, and a
//! test reading the state machine sees that and passes. Whether the process
//! got its bytes back is a different question, and only a counting allocator
//! can answer it: `OwnerRecord` holds two `EnumMap`s over every resource
//! class, an `RwLock`, a `Mutex`, and an `Arc` to its parent, so a record that
//! stays in its shard after close is a leak the state machine cannot see.
//!
//! Measured before the registry could remove a record: 14,000 owners created
//! and fully closed, every close returning `Ok`, every owner reporting
//! `Closed`, and **14,054,504 bytes** still held — 1,001 bytes per closed
//! owner, perfectly linear in the number of cycles. The governor's own figure
//! for process bytes was **0**.
//!
//! Reachable from ordinary use: every tab or pane opened and closed is one
//! owner, and every pane moved between windows closes one and creates another.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use enum_map::enum_map;
use sonicterm_resource::ResourceGovernor;
use sonicterm_types::{GovernorLimits, OwnerKind, OwnerLimits, ProcessKind};

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Serialises every measurement in this file.
///
/// The counting allocator is process-global, so two tests measuring
/// concurrently attribute each other's allocations to whichever one is
/// reading. A lock rather than `--test-threads=1`, because a suite that only
/// works under a flag will eventually run without it.
static MEASURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Counting;

// SAFETY: Operations forward exact pointers, layouts, and sizes to `System`; atomic bookkeeping allocates nothing and cannot re-enter.
unsafe impl GlobalAlloc for Counting {
    // SAFETY: `layout` must be valid; the atomic byte update is allocation-free before forwarding it unchanged.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        // SAFETY: `layout` is the exact valid layout received under `GlobalAlloc::alloc`.
        unsafe { System.alloc(layout) }
    }
    // SAFETY: `ptr` and its original `layout` must match; allocation-free bookkeeping cannot re-enter deallocation.
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: `ptr` and original `layout` are forwarded unchanged from the valid deallocation call.
        unsafe { System.dealloc(ptr, layout) }
    }
    // SAFETY: `ptr`, original `layout`, and `new_size` must be valid; atomic bookkeeping allocates nothing and cannot re-enter.
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE_BYTES.fetch_add(new_size.saturating_sub(layout.size()), Ordering::Relaxed);
        LIVE_BYTES.fetch_sub(layout.size().saturating_sub(new_size), Ordering::Relaxed);
        // SAFETY: `ptr`, original `layout`, and `new_size` are forwarded unchanged under `GlobalAlloc::realloc`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn held() -> usize {
    LIVE_BYTES.load(Ordering::Relaxed)
}

/// Limits that never refuse, so the measurement is about lifetime not budget.
///
/// Built here rather than taken from the crate's `test-util` helpers: this file
/// must compile in the per-crate gate, which builds without extra features.
fn unlimited_owner_limits() -> OwnerLimits {
    OwnerLimits {
        owner_bytes: usize::MAX,
        class_bytes: enum_map! { _ => usize::MAX },
        class_items: enum_map! { _ => None },
    }
}

fn governor() -> ResourceGovernor {
    ResourceGovernor::new(
        ProcessKind::Gui,
        GovernorLimits {
            process_bytes: usize::MAX,
            class_bytes: enum_map! { _ => usize::MAX },
            class_items: enum_map! { _ => None },
        },
    )
    .expect("a governor")
}

/// Open and fully close `count` owners, returning heap held afterwards.
///
/// Each cycle is what a tab open/close does: create a window owner, create a
/// pane under it, then close both from the leaf up.
fn cycle_owners(governor: &ResourceGovernor, count: usize) {
    let root = governor.root_owner();
    for _ in 0..count {
        let window = governor
            .create_child(root, OwnerKind::Window, unlimited_owner_limits())
            .expect("a window owner");
        let pane = governor
            .create_child(window, OwnerKind::AppPane, unlimited_owner_limits())
            .expect("a pane owner");

        governor.begin_close(pane).expect("the pane begins closing");
        governor.finish_close(pane).expect("the pane closes");
        governor.begin_close(window).expect("the window begins closing");
        governor.finish_close(window).expect("the window closes");
    }
}

/// A closed owner must not keep its record on the heap.
///
/// The assertion is on real heap rather than on owner state, because state is
/// what the previous test measured and it was `Closed` while the bytes were
/// still held.
#[test]
fn closing_an_owner_returns_its_memory() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    const CYCLES: usize = 2_000;

    let governor = governor();
    // One warm-up cycle so the first-allocation costs of the shard maps are
    // not attributed to the measured run.
    cycle_owners(&governor, 1);

    let before = held();
    cycle_owners(&governor, CYCLES);
    let after = held();

    let retained = after.saturating_sub(before);
    let per_owner = retained / (CYCLES * 2);

    println!(
        "MEASURED {CYCLES} open/close cycles ({} owners):\n  \
         retained  {retained} bytes\n  \
         per owner {per_owner} bytes",
        CYCLES * 2
    );

    // A small residual is honest — shard maps keep capacity once grown, and
    // the id counter advances. What must not happen is growth proportional to
    // the number of owners closed.
    assert!(
        per_owner < 32,
        "{retained} bytes are still held after {} owners were created and fully closed \
         — {per_owner} bytes per closed owner. A closed owner whose record stays in its \
         shard is a leak the lifecycle state machine cannot see: every owner reports \
         `Closed` and the governor reports zero process bytes.",
        CYCLES * 2
    );
}

/// The leak, if present, must be visible as growth in the number of cycles.
///
/// A fixed overhead and a per-owner leak look identical at one sample size.
/// Two sizes separate them: a leak scales, a fixed cost does not.
#[test]
fn retained_memory_does_not_scale_with_the_number_of_closed_owners() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let governor = governor();
    cycle_owners(&governor, 1);

    let small_before = held();
    cycle_owners(&governor, 500);
    let small = held().saturating_sub(small_before);

    let large_before = held();
    cycle_owners(&governor, 4_000);
    let large = held().saturating_sub(large_before);

    println!("MEASURED 500 cycles: {small} bytes; 4000 cycles: {large} bytes");

    // Eight times the cycles must not cost eight times the memory. Generous
    // factor: the assertion is about proportionality, not a tight bound.
    assert!(
        large < small.saturating_mul(4).max(64 * 1024),
        "500 cycles retained {small} bytes and 4000 retained {large} — memory grows with \
         the number of owners closed, which is the signature of a per-owner leak rather \
         than a fixed cost"
    );
}
