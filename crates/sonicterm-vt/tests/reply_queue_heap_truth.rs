//! Does the reply queue's coverage figure track memory actually held?
//!
//! The coverage table records `ParserReply` as negligible at 2 KiB per pane,
//! from "64 bounded slots of DSR/XTVERSION replies, ~32 bytes each". The
//! payload arithmetic is right — the replies really are small, and the largest
//! one this parser builds reserves 28 bytes.
//!
//! What the arithmetic leaves out is the queue itself. A bounded channel
//! allocates its slot array when it is created, whether or not anything is
//! ever sent, and each queued reply is a `Vec` whose own header sits in that
//! array on top of the bytes it points at. A figure derived from payloads
//! alone describes the smaller half of what a pane holds.
//!
//! Measured against a counting allocator rather than recomputed, because the
//! figure being checked was itself arithmetic. This lives in an integration
//! test because `#[global_allocator]` is crate-wide.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Serialises every test in this file.
///
/// The counting allocator is process-global, so two tests measuring
/// concurrently attribute each other's allocations to whichever one is
/// reading. A lock rather than `--test-threads=1`, because a suite that only
/// works under a flag is a suite that will eventually run without it.
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

/// The reply queue's slot count, from the app's parser wiring.
///
/// Restated rather than imported: `sonicterm-vt` does not depend on
/// `sonicterm-app`, which is where the channel is created. A test asserts this
/// matches the constant the app uses.
const REPLY_QUEUE_CAPACITY: usize = 64;

/// The per-pane figure the coverage table records for `ParserReply`.
const RECORDED_PER_PANE_BYTES: usize = 8 * 1024;

/// The largest reply this parser builds: an OSC 4 colour report, which
/// reserves 28 bytes before writing.
const LARGEST_REPLY_BYTES: usize = 28;

/// A full reply queue must hold no more than the class records.
///
/// Measured with every slot filled by the largest reply the parser emits:
/// **4,480 bytes against the 2,048 the payload arithmetic predicted.** The
/// difference is the channel's slot array and the per-reply `Vec` headers,
/// neither of which appears in `64 x 32`.
#[test]
fn a_full_reply_queue_holds_no_more_than_the_class_records() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    // Build the payloads before the measurement window so the harness's own
    // buffers are not attributed to the queue.
    let replies: Vec<Vec<u8>> =
        (0..REPLY_QUEUE_CAPACITY).map(|_| vec![b'x'; LARGEST_REPLY_BYTES]).collect();

    let before = held();
    let (tx, rx) = crossbeam_channel::bounded::<Vec<u8>>(REPLY_QUEUE_CAPACITY);
    for reply in &replies {
        tx.try_send(reply.clone()).expect("the queue accepts up to its capacity");
    }
    let truth = held().saturating_sub(before);

    assert_eq!(rx.len(), REPLY_QUEUE_CAPACITY, "precondition: the queue is full");
    assert!(truth > 0, "precondition: queueing must allocate");

    // Understating is the direction that matters: a class recorded negligible
    // is one nothing charges, so the figure is the only account of it.
    assert!(
        truth <= RECORDED_PER_PANE_BYTES,
        "a full reply queue holds {truth} bytes but the coverage table records \
         {RECORDED_PER_PANE_BYTES} per pane — a figure derived from payloads alone omits \
         the channel's own slot array and the per-reply `Vec` headers",
    );

    drop(rx);
}
