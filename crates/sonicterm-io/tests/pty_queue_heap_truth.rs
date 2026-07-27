//! Does `queued_output_bytes` track the heap the PTY output queue holds?
//!
//! The reader hands out `Bytes` views into a reused 64 KiB ring, so three
//! different numbers describe one full queue: the slot count times a constant,
//! the sum of the view lengths, and the ring memory those views pin. They
//! disagree by three orders of magnitude, and only the last one is memory the
//! process cannot reclaim while the queue stays full.
//!
//! Measured here against a real `/bin/sh`, 64 slots occupied in every case:
//!
//! | scenario              | sum of views | ring pinned |
//! | --------------------- | ------------ | ----------- |
//! | keystroke echo (1 B)  |           64 |      65,536 |
//! | shell prompt (20 B)   |        1,280 |      65,536 |
//! | flood (64 B)          |        4,160 |      65,536 |
//!
//! Charging the sum of views would admit work against 64 bytes of headroom
//! while 64 KiB is held; charging the slot count times 8 KiB refuses work
//! against 512 KiB that was never allocated. The pinned ring is the figure a
//! counting allocator agrees with, so it is the figure this asserts.
//!
//! A counting allocator is the only check that separates the three, and
//! `#[global_allocator]` is crate-wide, so this has to live in an integration
//! test rather than beside the module.

// Real-PTY measurement through `/bin/sh`. ConPTY reader behaviour differs
// enough that the ring arithmetic would need its own measurements to assert.
#![cfg(unix)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use sonicterm_io::pty::{queued_output_bytes, PtyHandle, PTY_OUTPUT_QUEUE_CAPACITY};

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

/// The reader's ring size. A pinned queue is always a whole multiple of this.
const RING_CAP: usize = 64 * 1024;

struct QueueTruth {
    slots: usize,
    /// What `queued_output_bytes` claims while the queue is full.
    reported: usize,
    /// Sum of the queued view lengths — the payload actually waiting.
    payload: usize,
    /// Ring bytes released when the queued views are dropped.
    pinned: usize,
}

/// Fill one pane's output queue from a real child, then weigh what it holds.
///
/// Nothing drains the channel while `script` runs, so the queue reaches
/// capacity and the reader parks in `send`. The reported figure is sampled at
/// that point — the state the governor would actually observe.
///
/// Each script writes without end rather than a fixed count: the PTY coalesces
/// adjacent writes into one read, so a fixed count fills an unpredictable
/// number of slots. Backpressure stops the child once the queue is full, and
/// dropping the handle kills it.
fn measure_full_queue(script: &str) -> QueueTruth {
    let args = vec!["-c".to_string(), script.to_string()];
    let pty = PtyHandle::spawn_with_args("/bin/sh", &args, 80, 24).expect("spawn /bin/sh");

    let deadline = Instant::now() + Duration::from_secs(20);
    while pty.out_rx.len() < PTY_OUTPUT_QUEUE_CAPACITY && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }

    let slots = pty.out_rx.len();
    let reported = queued_output_bytes(&pty);

    // Take the views out without dropping them. Allocated up front so the
    // holder's own buffer is never inside a measurement window.
    let mut holder = Vec::with_capacity(PTY_OUTPUT_QUEUE_CAPACITY * 4);
    while let Ok(chunk) = pty.out_rx.try_recv() {
        holder.push(chunk);
    }
    let payload: usize = holder.iter().map(|chunk| chunk.len()).sum();

    // Retire the reader before weighing. While it lives it holds its own view
    // into the newest ring, and that ring would be freed by dropping the
    // handle rather than by dropping the queued views — which would land in
    // the delta below and be miscounted as chunk-pinned.
    drop(pty);
    std::thread::sleep(Duration::from_millis(500));

    // `clear` drops the views but keeps the Vec's buffer, so this delta is the
    // ring memory the views were pinning and nothing else.
    let with_chunks = held();
    holder.clear();
    let after = held();

    QueueTruth { slots, reported, payload, pinned: with_chunks.saturating_sub(after) }
}

/// The reported figure must track the ring the queue pins, in both directions.
///
/// Measured before the fix: 524,288 reported against 65,576 pinned in all
/// three scenarios — **8x over**, and the same number every time because the
/// old figure was the slot count restated, blind to what the slots held.
#[test]
fn reported_bytes_track_the_ring_the_queue_pins() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    for (label, script) in [
        ("keystroke echo", "while :; do printf 'x'; sleep 0.01; done"),
        ("shell prompt", "while :; do printf 'abcdefghijklmnopqrst'; sleep 0.01; done"),
        ("flood", "while :; do printf '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef'; done"),
    ] {
        let truth = measure_full_queue(script);

        assert_eq!(
            truth.slots, PTY_OUTPUT_QUEUE_CAPACITY,
            "{label}: precondition — the queue must be full, or the measurement is vacuous"
        );
        assert!(
            truth.pinned >= RING_CAP,
            "{label}: precondition — a full queue must pin at least one ring (pinned {})",
            truth.pinned
        );

        // Understating is the direction that admits work past the ceiling: the
        // governor charges this figure, so an undercount lets a pane through
        // while the ring is already held. The sum of view lengths fails here —
        // 64 bytes reported against 64 KiB held.
        assert!(
            truth.reported + truth.pinned / 10 + 8192 >= truth.pinned,
            "{label}: reported {} understates pinned ring {} by {} ({:.1}x); payload was {}",
            truth.reported,
            truth.pinned,
            truth.pinned.saturating_sub(truth.reported),
            truth.pinned as f64 / truth.reported.max(1) as f64,
            truth.payload
        );

        // Overstating refuses work the user asked for while memory is free.
        assert!(
            truth.reported <= truth.pinned + truth.pinned / 10 + 8192,
            "{label}: reported {} overstates pinned ring {} by {} ({:.1}x); payload was {}",
            truth.reported,
            truth.pinned,
            truth.reported.saturating_sub(truth.pinned),
            truth.reported as f64 / truth.pinned.max(1) as f64,
            truth.payload
        );
    }
}

/// A queue of keystroke echoes must not be reported as half a megabyte.
///
/// The headline case: 64 one-byte echoes hold 64 bytes of payload in one 64 KiB
/// ring, and the old figure called it 524,288 — enough to refuse a pane over
/// memory that was never allocated.
#[test]
fn keystroke_echo_queue_is_not_reported_as_half_a_megabyte() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let truth = measure_full_queue("while :; do printf 'x'; sleep 0.01; done");

    assert_eq!(truth.slots, PTY_OUTPUT_QUEUE_CAPACITY, "precondition — the queue must be full");
    assert!(
        truth.payload <= 4096,
        "precondition — keystroke echoes must stay tiny (payload {})",
        truth.payload
    );

    // One ring holds every one of these views; two is already generous.
    assert!(
        truth.reported <= 2 * RING_CAP,
        "reported {} for {} bytes of payload pinning {} of ring — the figure is tracking \
         the slot count, not the memory",
        truth.reported,
        truth.payload,
        truth.pinned
    );
}
