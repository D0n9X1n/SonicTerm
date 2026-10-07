//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.

use crate::shell::LinuxShell;
use crate::{KeymapLoader, ProcessPrivilege, ThemeLoader};

#[test]
fn exports_loader_type_aliases() {
    let _: Option<KeymapLoader> = None;
    let _: Option<ThemeLoader> = None;
}

#[test]
fn exports_linux_platform_shell() {
    // Protect the platform-neutral app runner surface consumed by the Linux binary crate.
    let _: Option<LinuxShell> = None;
}

#[test]
fn exports_process_privilege_contract() {
    // Protect platform binaries from replacing the typed process-level state with title inference.
    assert!(!ProcessPrivilege::default().is_privileged());
    assert!(ProcessPrivilege::Privileged.is_privileged());
}

#[test]
fn headless_app_defaults_to_unprivileged() {
    // Protect existing and headless constructors from painting an unobserved privilege warning.
    let app = crate::app::App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );

    assert_eq!(app.process_privilege(), ProcessPrivilege::Unprivileged);
}

/// The unit-test binary's allocator: it counts `alloc`, `alloc_zeroed` and `realloc` only on a thread
/// that enabled counting through [`CountAllocations`], so the rest of the suite runs as on `System`.
struct CountingAllocator;

thread_local! {
    /// Whether this thread counts its allocations; const-initialized with no destructor.
    static COUNTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The allocations this thread counted since its last [`CountAllocations::start`].
    static COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Count one allocation if this thread is counting; a thread in TLS teardown is never counted.
fn note_allocation() {
    if COUNTING.try_with(std::cell::Cell::get).unwrap_or(false) {
        let _ = COUNT.try_with(|count| count.set(count.get() + 1));
    }
}

// SAFETY: every method forwards its exact arguments to `System`, which upholds the `GlobalAlloc`
// contract; the counters are const-initialized thread locals read through `try_with`, so they never allocate.
unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
    // SAFETY: the caller's `layout` contract is forwarded unchanged to `System::alloc`.
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        note_allocation();
        // SAFETY: `layout` satisfies `GlobalAlloc::alloc`'s contract and is forwarded unchanged.
        unsafe { std::alloc::System.alloc(layout) }
    }

    // SAFETY: the caller's `layout` contract is forwarded unchanged to `System::alloc_zeroed`.
    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        note_allocation();
        // SAFETY: `layout` satisfies `GlobalAlloc::alloc_zeroed`'s contract and is forwarded unchanged.
        unsafe { std::alloc::System.alloc_zeroed(layout) }
    }

    // SAFETY: `block` came from this allocator, which is `System` underneath, with this `layout`.
    unsafe fn dealloc(&self, block: *mut u8, layout: std::alloc::Layout) {
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
        note_allocation();
        // SAFETY: `block` is a live `System` block of `layout`; `new_size` is the caller's valid request.
        unsafe { std::alloc::System.realloc(block, layout, new_size) }
    }
}

#[global_allocator]
static COUNTING_ALLOCATOR: CountingAllocator = CountingAllocator;

/// Scoped allocation counting on the current thread: starts at zero, restores the previous setting on drop.
pub(crate) struct CountAllocations {
    previous: bool,
}

impl CountAllocations {
    /// Reset this thread's count and start counting.
    pub(crate) fn start() -> Self {
        COUNT.with(|count| count.set(0));
        Self { previous: COUNTING.with(|counting| counting.replace(true)) }
    }

    /// The allocations this thread counted since [`CountAllocations::start`].
    pub(crate) fn count(&self) -> u64 {
        COUNT.with(std::cell::Cell::get)
    }
}

// Lifecycle: dropping CountAllocations restores this thread's previous counting setting.
impl Drop for CountAllocations {
    fn drop(&mut self) {
        COUNTING.with(|counting| counting.set(self.previous));
    }
}

/// The counting allocator counts only while a thread enabled it, and the scope restores the setting.
#[test]
fn counting_allocator_counts_only_inside_its_scope() {
    let counter = CountAllocations::start();
    let held = std::hint::black_box(vec![1_u8; 32]);
    assert_eq!(counter.count(), 1);
    drop(counter);
    let after = CountAllocations::start();
    drop(after);
    let _unseen = std::hint::black_box(vec![2_u8; 32]);
    assert!(!COUNTING.with(std::cell::Cell::get), "the scope restored the previous setting");
    drop(held);
}
