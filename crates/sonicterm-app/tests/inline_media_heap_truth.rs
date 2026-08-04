//! Does the inline-media figure track memory actually held?
//!
//! The retained-media figure is not only reported — it is the number panes are
//! *admitted against*. `trim_inline_images_charged` sets each pane's charge
//! from `retained_inline_media`, those charges sum into the process total, and
//! `pane_inline_media_budget` divides the ceiling by what that total implies.
//! A figure that under-reports therefore admits past the real ceiling, and the
//! overshoot is invisible precisely because the same number is used to measure
//! it and to judge it.
//!
//! That shape has now produced several defects in this milestone: a hyperlink
//! registry that under-reported 4.8x, a grid attribute box charged by pointer
//! size rather than string length, and a cap whose "derived" constant omitted
//! one of the seams it summed. Each had a passing test, because each test
//! checked the figure against the arithmetic it came from rather than against
//! the heap.
//!
//! A counting allocator is the check that does not share that blind spot, and
//! it lives in an integration test because `#[global_allocator]` is crate-wide
//! — declaring it in-crate would impose counting on every unit test in the
//! crate and fill the measurement window with their allocations.
//!
//! Everything here is driven through the crate's existing public API:
//! `measure_pane` reports `inline_media` from the same `retained_inline_media`
//! that sets the charge, so measuring what it returns is measuring the figure
//! admission actually uses. No visibility was widened for this file.
//!
//! # Measuring a global counter from one thread
//!
//! The counter is process-wide, and the lock below serialises this file's own
//! tests rather than the harness — sibling test threads keep running and keep
//! allocating. A window opened here therefore observes their activity as well
//! as its own, and a sibling *free* can make the observed delta smaller than
//! what was genuinely allocated, or negative.
//!
//! That is not noise to be tolerated with an allowance; it is a property the
//! measurement is built around. Both disturbances are one-directional and they
//! point opposite ways: a sibling free only subtracts from an observed delta,
//! a sibling allocation only adds to it. Neither extreme is therefore the
//! right estimator on its own — the maximum selects for allocation-inflated
//! samples, the minimum for free-deflated ones, and both were measured doing
//! exactly that.
//!
//! What pins the figure is combining the two directional properties: the
//! payload is a floor no clean sample can fall below, so it excludes every
//! free-contaminated sample, and among the samples clearing it the smallest is
//! the least inflated. That is the closest observable value to an undisturbed
//! window, with no tolerance anywhere in it.
//!
//! This corrects the measurement, never the figure. A genuinely over-reporting
//! `retained_inline_media` produces samples that never reach the payload
//! however many are taken, and fails with every observed delta printed.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use sonicterm_app::app::retention::measure_pane;
use sonicterm_app::app::PaneState;
use sonicterm_grid::grid::Grid;
use sonicterm_render_model::InlineImage;
use sonicterm_vt::vt::Parser;

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Serialises every test in this file.
///
/// The counting allocator is process-global, so two tests measuring
/// concurrently attribute each other's allocations to whichever one is
/// reading.
///
/// A lock rather than `--test-threads=1`, because a suite that only works
/// under a flag is a suite that will eventually run without it.
///
/// Note the limit of what this can do: it serialises the code *in this file*,
/// not the harness. Sibling test threads are already running and keep
/// allocating regardless. `the_measurement_window_residue_is_reported` below
/// reports how much they move an empty window; the accounting tests handle
/// them by resampling rather than by assuming them away.
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

fn pane() -> PaneState {
    PaneState::new(Arc::new(Mutex::new(Parser::new(Grid::new(80, 24)))), None)
}

fn reported(pane: &PaneState) -> usize {
    measure_pane(pane)
        .expect("a single-threaded test never contends the parser lock")
        .inline_media
        .bytes
}

/// The reported figure must track real heap across counts and sizes.
///
/// Driven at several shapes because the two candidate error modes look
/// different: a term that scales with **pixels** (the wrong buffer length)
/// shows up as a ratio that stays high as images grow, while a term that
/// scales with **image count** (the `Arc` header, the `Vec` spine) shows up as
/// a ratio that decays toward 1.00x as images grow. Only measuring at one size
/// cannot tell those apart.
///
/// # Reading a global counter from one thread
///
/// `held()` is process-global, so a window opened here also observes whatever
/// sibling test threads allocate and free in the same interval. The lock in
/// this file serialises its own tests; it cannot serialise the harness.
///
/// That is not a tolerance to be widened — it is a property the measurement
/// has to be built around. A sibling *free* inside the window makes the
/// observed delta smaller than what was really allocated, and can drive it
/// negative: measured on Windows CI at `reported 4096, held 3800`, a 296-byte
/// shortfall against 4 KiB of pixels that were unquestionably on the heap.
///
/// The two disturbances are one-directional and point opposite ways, and that
/// is what pins the figure. A sibling free only ever *subtracts* from the
/// observed delta, so no clean sample can fall below the payload — that floor
/// excludes every free-contaminated sample. A sibling allocation only ever
/// *adds*, so among the samples clearing the floor, the smallest is the least
/// inflated. Measured both ways round: taking the maximum instead reported
/// 129 B/image against a true 16, and taking the minimum reported the Windows
/// 3800 against a true 4160.
///
/// Note what this cannot do: it corrects for the *measurement*, never for the
/// figure. An over-reporting `retained_inline_media` produces samples that
/// never reach the payload no matter how many are taken, so it fails with
/// every observed delta printed.
#[test]
fn reported_bytes_track_real_heap() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    // (images, bytes each)
    let shapes = [(4usize, 1024usize), (128, 1024), (16, 64 * 1024), (64, 1024 * 1024)];

    for (count, image_bytes) in shapes {
        let pixels = count * image_bytes;

        // The **smallest** delta that still accounts for the whole payload.
        //
        // Both disturbances are one-directional, and they point opposite ways:
        // a sibling *free* inside the window subtracts from the observed
        // delta, a sibling *allocation* adds to it. So neither the maximum nor
        // the minimum alone is the right estimator — the maximum selects for
        // allocation-inflated samples (measured: 129 B/image against a true
        // 16 B/image), and the minimum selects for free-deflated ones
        // (measured on Windows CI: 3800 against a true 4160).
        //
        // Combining the two directional properties pins it from both sides. A
        // sample below `pixels` cannot be clean, because a counting
        // `GlobalAlloc` always charges the full payload — so that floor
        // excludes every free-contaminated sample. Among the samples that
        // clear it, the smallest is the least inflated. The result is the
        // closest observable value to an undisturbed window, without needing
        // one to occur and without a tolerance anywhere.
        //
        // Sixteen samples, and the loop cannot exit early: an early exit would
        // return the *first* qualifying sample rather than the smallest, which
        // is the max estimator again under another name. Sixteen windows over
        // a few KiB cost microseconds.
        let mut truth: Option<i128> = None;
        let mut figure = 0usize;
        let mut observed: Vec<i128> = Vec::with_capacity(16);

        for _ in 0..16 {
            // The pane, its grid and its parser are built *before* the window:
            // they are not the subject, and their allocations would otherwise
            // be charged to inline media.
            let pane = pane();
            pane.inline_images.lock().reserve_exact(count);

            let before = held();
            {
                let mut images = pane.inline_images.lock();
                for id in 0..count {
                    // `Arc::from(Vec<u8>)` allocates the `Arc` (header +
                    // payload) and frees the `Vec`, so both live inside the
                    // window and the transient cancels. Building the `Vec`
                    // outside would leave the `Arc` allocation uncounted,
                    // which is the measurement error that would make this test
                    // agree with a wrong figure.
                    let bgra: Arc<[u8]> = Arc::from(vec![0u8; image_bytes]);
                    images.push(InlineImage {
                        id: id as u64,
                        row: 0,
                        col: 0,
                        width: 1,
                        height: 1,
                        bgra,
                    });
                }
            }
            // Signed, deliberately. `saturating_sub` clamps a
            // sibling-disturbed window to `0` and hides the disturbance
            // entirely — that clamp is what turned a 296-byte sibling free
            // into a red CI run reporting `overhead 0 B` while the real
            // shortfall was invisible.
            let delta = held() as i128 - before as i128;
            observed.push(delta);
            // Only samples that account for the whole payload are candidates;
            // among those, keep the smallest.
            if delta >= pixels as i128 && truth.is_none_or(|best| delta < best) {
                truth = Some(delta);
                figure = reported(&pane);
            }
        }

        let truth = truth.unwrap_or_else(|| {
            panic!(
                "no sample for {count} x {image_bytes} B reached the {pixels} bytes of pixels \
                 allocated inside the window; observed {observed:?}. A counting `GlobalAlloc` \
                 charges `layout.size()` on every request, so the payload is always charged — a \
                 shortfall in every one of {} samples means sibling threads freed memory inside \
                 each window, not that the allocation went uncounted",
                observed.len()
            )
        });

        let truth = truth as usize;

        let ratio = truth as f64 / figure as f64;
        println!(
            "HEAP {count:>4} x {:>7} B: reported {figure:>9} held {truth:>9} ratio {ratio:.4}x \
             overhead {:>6} B ({:>3} B/image)",
            image_bytes,
            truth.saturating_sub(figure),
            truth.saturating_sub(figure) / count
        );

        assert!(
            truth >= figure,
            "reported {figure} exceeds the {truth} bytes actually held — the figure claims \
             memory that was never allocated"
        );

        // The undercount must be a per-image constant, not a share of the
        // pixels. `Arc<[u8]>` carries a 16-byte strong/weak header and each
        // `InlineImage` occupies a slot in the vector's spine; neither term
        // grows with image size. A figure that missed part of the *pixel*
        // buffer would scale with `image_bytes` and fail this bound at the
        // larger shapes, which is the failure this test exists to catch.
        //
        // Derived, not fitted: 16 B of `Arc` header + 40 B of `InlineImage`
        // spine slot = 56 B, doubled to 128 B so that an allocator with
        // coarser size classes than macOS's — Windows rounds differently — has
        // room without the bound ceasing to discriminate. A pixel-scaling
        // undercount is caught at 528 B/image, four times this bound.
        const MAX_OVERHEAD_PER_IMAGE: usize = 128;
        let overhead = truth.saturating_sub(figure);
        assert!(
            overhead <= count * MAX_OVERHEAD_PER_IMAGE,
            "{overhead} bytes unaccounted across {count} images ({} B/image) exceeds the \
             {MAX_OVERHEAD_PER_IMAGE} B/image of `Arc` header and vector spine; an undercount \
             larger than that scales with something other than the image count, and admission \
             uses this figure",
            overhead / count
        );
    }
}

/// How much a measurement window moves when nothing is allocated in it.
///
/// **A diagnostic, not a gate.** It reports the residue and asserts only that
/// the counter is live; the accounting tests carry the pass/fail, because they
/// measure the figure admission actually uses.
///
/// It exists to answer one question quickly when those tests misbehave: is
/// this an accounting defect, or sibling noise? A large delta here means the
/// harness is active and the figures are being disturbed; a small one means
/// the accounting is the place to look.
///
/// # Why it is not an assertion
///
/// It used to assert an exactly-zero delta between two consecutive reads, and
/// it passed everywhere — including on the Windows run where the real
/// measurement beside it failed for exactly the reason this was supposed to
/// detect. Two back-to-back reads span a sub-microsecond window that a sibling
/// has almost no chance of landing in, so it reported "clean" for a process
/// that was not. It was measuring its own speed, not the harness's quiescence.
///
/// Sampling over a window comparable to a real measurement is what makes the
/// figure mean anything. Being honest about it also means not gating on it:
/// the residue is a property of the harness and the platform allocator rather
/// than of this crate, and a test that fails on another thread's scheduling is
/// a test that gets silenced rather than read.
#[test]
fn the_measurement_window_residue_is_reported() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    // Sampled repeatedly rather than once: a single window says nothing about
    // whether a sibling *could* have landed in it.
    let mut worst: i128 = 0;
    for _ in 0..64 {
        let before = held();
        std::hint::black_box(());
        let delta = held() as i128 - before as i128;
        if delta.abs() > worst.abs() {
            worst = delta;
        }
    }

    println!(
        "HEAP residue: worst empty-window delta over 64 samples: {worst:+} bytes \
         (negative means a sibling thread freed memory inside the window)"
    );

    // The one real assertion: the counter is live. A counter stuck at zero
    // would make every ratio in this file meaningless and every assertion
    // vacuous — the single failure mode here that would otherwise go
    // unnoticed, because it looks exactly like success.
    assert!(
        held() > 0,
        "the counting allocator reports nothing held, so the measurements in this file are \
         vacuous"
    );
}

/// The figure must predict what is actually returned when media is released.
///
/// Reporting a number that tracks the heap while it is *held* is only half the
/// property. Reclamation subtracts the same figure from the process total, so
/// if freeing returned less than was reported the total would drift downward
/// away from reality and admit panes against memory that is still held.
///
/// This is where `Arc<[u8]>` earns its own test: the buffer is shared with the
/// renderer, so a release only reaches the allocator when the last reference
/// goes. With no renderer clone alive — the case here — the reported figure
/// must come back in full.
///
/// Measured the same way as the tests above, and for the same reason: the
/// counter is process-global, so a sibling *allocation* inside this window
/// makes the observed release look smaller than it was, and a sibling free
/// makes it look larger. The smallest release that still accounts for the
/// whole payload is taken, which is the least-disturbed observable value.
#[test]
fn releasing_media_returns_what_the_figure_reported() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    const COUNT: usize = 32;
    const IMAGE_BYTES: usize = 256 * 1024;
    const PIXELS: usize = COUNT * IMAGE_BYTES;

    let mut freed: Option<i128> = None;
    let mut figure = 0usize;
    let mut cleared = None;
    let mut observed: Vec<i128> = Vec::with_capacity(16);

    for _ in 0..16 {
        let pane = pane();
        {
            let mut images = pane.inline_images.lock();
            images.reserve_exact(COUNT);
            for id in 0..COUNT {
                images.push(InlineImage {
                    id: id as u64,
                    row: 0,
                    col: 0,
                    width: 1,
                    height: 1,
                    bgra: Arc::from(vec![0u8; IMAGE_BYTES]),
                });
            }
        }

        let sampled_figure = reported(&pane);
        let before = held();
        pane.inline_images.lock().clear();
        // Signed: a sibling allocating inside the window makes this smaller
        // than the true release, and clamping it at zero would hide that.
        let sampled = before as i128 - held() as i128;
        observed.push(sampled);

        // Same estimator as the tests above, for the same reason: the payload
        // is the floor no clean sample can fall below, and among the samples
        // that clear it the smallest is the least inflated by a sibling free.
        if sampled >= PIXELS as i128 && freed.is_none_or(|best| sampled < best) {
            freed = Some(sampled);
            figure = sampled_figure;
            cleared = Some(pane);
        }
    }

    let freed = freed.unwrap_or_else(|| {
        panic!(
            "no sample released the {PIXELS} bytes of pixels dropped inside the window; observed \
             {observed:?}. Releasing an `Arc<[u8]>` with no other reference alive returns the \
             payload to the allocator, so a shortfall in every one of {} samples means sibling \
             threads allocated inside each window",
            observed.len()
        )
    });

    let pane = cleared.expect("a sample was accepted");
    let freed = freed as usize;

    println!(
        "HEAP release: reported {figure} freed {freed} ratio {:.4}x",
        freed as f64 / figure as f64
    );

    assert!(
        freed >= figure,
        "releasing returned {freed} bytes against a reported {figure} — the process total is \
         credited more than the allocator gave back, so later panes are admitted against \
         memory that is still held"
    );
    assert_eq!(reported(&pane), 0, "a cleared pane must report no retained media");
}
