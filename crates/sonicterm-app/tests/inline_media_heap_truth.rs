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
/// Note what this lock can and cannot do: it serialises the code *in this
/// file*, not the harness. A sibling thread blocked on it has already been
/// started and can still allocate. `the_measurement_window_is_clean` below
/// quantifies that residue rather than assuming it away.
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
#[test]
fn reported_bytes_track_real_heap() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    // (images, bytes each)
    let shapes = [(4usize, 1024usize), (128, 1024), (16, 64 * 1024), (64, 1024 * 1024)];

    for (count, image_bytes) in shapes {
        // The pane, its grid and its parser are built *before* the window:
        // they are not the subject, and their allocations would otherwise be
        // charged to inline media.
        let pane = pane();
        pane.inline_images.lock().reserve_exact(count);

        let before = held();
        {
            let mut images = pane.inline_images.lock();
            for id in 0..count {
                // `Arc::from(Vec<u8>)` allocates the `Arc` (header + payload)
                // and frees the `Vec`, so both live inside the window and the
                // transient cancels. Building the `Vec` outside would leave
                // the `Arc` allocation uncounted, which is the measurement
                // error that would make this test agree with a wrong figure.
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
        let truth = held().saturating_sub(before);
        let figure = reported(&pane);

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

/// What the residue is when nothing is allocated.
///
/// The lock in this file serialises its own tests, not the harness: a sibling
/// test thread already running can allocate while a measurement window is
/// open. This quantifies that rather than assuming it away, so that any
/// overshoot above has a measured floor to be judged against instead of being
/// explained by hand.
///
/// Asserted at exactly zero. If this ever fails, the overshoot in the tests
/// above is sibling noise and not an accounting defect — diagnose it here
/// first.
#[test]
fn the_measurement_window_is_clean() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let before = held();
    let after = held();
    assert_eq!(
        after,
        before,
        "an empty measurement window moved by {} bytes, so the figures measured in this file \
         carry that much sibling-thread noise",
        after.saturating_sub(before)
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
#[test]
fn releasing_media_returns_what_the_figure_reported() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    const COUNT: usize = 32;
    const IMAGE_BYTES: usize = 256 * 1024;

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

    let figure = reported(&pane);
    let before = held();
    pane.inline_images.lock().clear();
    let freed = before.saturating_sub(held());

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
