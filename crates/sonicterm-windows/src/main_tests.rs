//! Public-surface smoke checks folded from the former tests/smoke.rs integration binary.
//! Runs as a `--lib` unit test so it links once with the crate.
//!
//! Also carries the heap-truth measurement for the software presentation
//! buffer. That measurement needs a counting `#[global_allocator]`, which is
//! crate-wide, and `sonicterm-windows` is a binary crate with no lib target —
//! so `tests/` cannot reach `SoftwareSurface` and the allocator has to live
//! here, behind `#[cfg(test)]`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::software_presenter::{DirtyRect, SoftwareSurface};

#[test]
fn integration_test_target_is_present() {
    assert_eq!(env!("CARGO_PKG_NAME"), "sonicterm-windows");
}

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Serialises every measuring test in this file.
///
/// The counting allocator is process-global, so two tests measuring
/// concurrently attribute each other's allocations to whichever one is
/// reading.
///
/// A lock rather than `--test-threads=1`, because the gate cannot be told to
/// serialise one file and a suite that only works under a flag is a suite that
/// will eventually run without it.
static MEASURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE_BYTES.fetch_add(new_size.saturating_sub(layout.size()), Ordering::Relaxed);
        LIVE_BYTES.fetch_sub(layout.size().saturating_sub(new_size), Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn live() -> usize {
    LIVE_BYTES.load(Ordering::Relaxed)
}

/// Heap actually held by a surface of the given size.
///
/// Measured as a delta across construction, so the baseline the harness itself
/// holds cancels out.
///
/// Sampled several times and taking the median, because `MEASURE` serialises
/// the tests that take it and not the whole binary — the other tests in this
/// crate allocate on their own threads while this one reads. Concurrent
/// allocation pushes a sample up and concurrent deallocation pushes it down,
/// so the median converges on the figure while a single sample does not.
/// Measured: an unsampled read came back 432 bytes under a 59 MB surface.
fn measured_bytes(width: u32, height: u32) -> usize {
    let mut samples: Vec<usize> = (0..5)
        .map(|_| {
            let before = live();
            let surface = SoftwareSurface::try_new(width, height).expect("valid surface");
            let held = live().saturating_sub(before);
            // Read before the drop, or the release races the measurement.
            assert_eq!(surface.width(), width.max(1));
            drop(surface);
            held
        })
        .collect();
    samples.sort_unstable();
    samples[samples.len() / 2]
}

/// The tabled `ClassCoverage::UnchargedRetention` figure for `SoftwareFrame`.
///
/// Mirrored here rather than imported: `sonicterm-types` is not a dependency
/// of this crate, and the point of the test is to check the tabled number
/// against real heap. Importing it would make the comparison circular.
const TABLED_PER_OWNER_BYTES: usize = 3840 * 2160 * 4;

/// The software frame's real cost is its pixel buffer, and it is one 4K frame
/// only if the window is 4K.
///
/// `ClassCoverage::UnchargedRetention { per_owner_bytes: 3840 * 2160 * 4 }` was
/// the last figure in the resource table never checked against an allocator.
/// Every other figure was measured during this milestone; this one was a size
/// chosen by assumption, because the buffer exists only on the Windows
/// software path and CI ran with a hardware adapter.
///
/// It does not need one. `SoftwareSurface` is plain Rust holding a `Vec<u8>`,
/// so a counting allocator measures it on any host, including this one.
#[test]
fn software_surface_heap_matches_its_pixel_arithmetic() {
    let _guard = MEASURE.lock().unwrap_or_else(|e| e.into_inner());

    for (w, h, label) in [
        (1920u32, 1080u32, "1080p"),
        (2560, 1440, "1440p"),
        (3840, 2160, "4K"),
        (5120, 2880, "5K"),
        (7680, 4320, "8K"),
    ] {
        let expected = w as usize * h as usize * 4;
        let measured = measured_bytes(w, h);

        // Within 1%. The tolerance is for harness noise, not for the figure:
        // every structural error this test exists to catch — a buffer never
        // allocated, allocated twice, or sized per pixel instead of per byte —
        // is off by 100% or more, three orders of magnitude outside it.
        let drift = measured.abs_diff(expected);
        assert!(
            drift * 100 <= expected,
            "{label} ({w}x{h}): measured {measured} against computed {expected}, \
             a drift of {drift} bytes; the pixel buffer is not the size the \
             arithmetic claims"
        );
    }
}

/// The tabled figure is right for a 4K window and wrong for anything else.
///
/// This is the finding, stated as a test so it cannot quietly stop being true:
/// the figure is not a per-owner constant, it is a function of window size.
/// A 4K window costs what the table says. A 5K window costs 78% more, and
/// nothing in the table says so.
#[test]
fn the_tabled_figure_holds_only_at_4k() {
    let _guard = MEASURE.lock().unwrap_or_else(|e| e.into_inner());

    let at_4k = measured_bytes(3840, 2160);
    assert!(
        at_4k.abs_diff(TABLED_PER_OWNER_BYTES) <= 4096,
        "the tabled figure {TABLED_PER_OWNER_BYTES} should match a real 4K surface, \
         measured {at_4k}"
    );

    // A 5K display is ordinary hardware — the 27-inch iMac panel, and common
    // in the VDI configurations where software rendering runs.
    let at_5k = measured_bytes(5120, 2880);
    assert!(
        at_5k > TABLED_PER_OWNER_BYTES,
        "a 5K surface must exceed the tabled 4K figure: {at_5k} vs {TABLED_PER_OWNER_BYTES}"
    );

    // Stated as a ratio so the assertion says how wrong, not merely that it is.
    let over = (at_5k * 100) / TABLED_PER_OWNER_BYTES;
    assert!(
        over >= 175,
        "a 5K surface should be ~178% of the tabled figure, measured {over}%"
    );
}

/// The real ceiling is the module's own clamp, not the tabled figure.
///
/// `pixel_len` rejects anything past 160 MiB, so that — not 31.6 MiB — is the
/// most one surface can hold. The table understates the worst case by 5x.
#[test]
fn the_surface_clamp_is_the_real_upper_bound() {
    let _guard = MEASURE.lock().unwrap_or_else(|e| e.into_inner());

    const CLAMP_BYTES: usize = 160 * 1024 * 1024;

    // Largest surface the clamp admits, at a 16:9-ish shape.
    let (w, h) = (8192u32, 4320u32);
    let bytes = w as usize * h as usize * 4;
    assert!(bytes <= CLAMP_BYTES, "test input must be inside the clamp");

    let measured = measured_bytes(w, h);
    assert!(
        measured > TABLED_PER_OWNER_BYTES * 4,
        "the largest admissible surface must dwarf the tabled figure: \
         {measured} vs {TABLED_PER_OWNER_BYTES}"
    );

    // And past the clamp, nothing is allocated at all.
    let before = live();
    assert!(
        SoftwareSurface::try_new(16_384, 16_384).is_none(),
        "a surface past the clamp must be refused"
    );
    assert!(
        live().saturating_sub(before) < 4096,
        "a refused surface must not allocate its buffer first"
    );
}

/// Dropping a surface returns every byte.
///
/// Criterion 3 of the issue: the buffer must not be retained per window after
/// close. Run over many cycles, because a leak of one buffer per window is
/// invisible in a single open-and-close.
#[test]
fn surfaces_release_their_buffer_on_drop() {
    let _guard = MEASURE.lock().unwrap_or_else(|e| e.into_inner());

    let baseline = live();
    for _ in 0..64 {
        let surface = SoftwareSurface::try_new(1920, 1080).expect("valid surface");
        assert_eq!(surface.pixels().len(), 1920 * 1080 * 4);
        drop(surface);
    }
    let after = live();

    // Compared against one buffer, which is what a per-surface leak costs.
    // 64 leaked buffers would be 530 MB; a threshold in kilobytes would be
    // measuring harness noise instead of the property.
    const ONE_BUFFER: usize = 1920 * 1080 * 4;
    let retained = after.saturating_sub(baseline);
    assert!(
        retained < ONE_BUFFER,
        "64 open/close cycles retained {retained} bytes over baseline, which is \
         at least one whole {ONE_BUFFER}-byte buffer; the surface is not \
         releasing on drop"
    );
}

/// A shrink returns memory; a grow within the hysteresis band does not
/// reallocate.
///
/// The resize path keeps capacity while the new size is at least half the old
/// one, so a drag-resize does not thrash the allocator. That band is a
/// deliberate retention: a window dragged smaller holds up to 2x its current
/// pixels until it crosses the halfway point.
#[test]
fn resize_retention_follows_the_hysteresis_band() {
    let _guard = MEASURE.lock().unwrap_or_else(|e| e.into_inner());

    let mut surface = SoftwareSurface::try_new(3840, 2160).expect("valid surface");
    let at_4k = live();

    // Just inside the band: still over half the 4K byte count, so the
    // allocation is reused and the process holds 4K worth for a smaller window.
    assert!(surface.try_resize(3840, 1200));
    let inside_band = live();
    assert!(
        inside_band >= at_4k.saturating_sub(4096),
        "a resize inside the hysteresis band must not release the buffer: \
         {inside_band} vs {at_4k}"
    );

    // Past the halfway point: reallocated down.
    assert!(surface.try_resize(640, 480));
    let after_shrink = live();
    assert!(
        after_shrink < at_4k / 2,
        "a shrink past half capacity must release memory: {after_shrink} vs {at_4k}"
    );

    surface.mark_dirty(DirtyRect { x: 0, y: 0, w: 640, h: 480 });
    drop(surface);
}
