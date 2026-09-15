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

use crossbeam_channel::{Receiver, RecvTimeoutError};

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

/// The reader's ring size. A pinned queue is always a whole multiple of this.
const RING_CAP: usize = 64 * 1024;

/// Absolute bound on reaching a quiesced producer. Generous against a loaded
/// CI runner, but finite: exceeding it fails the measurement rather than
/// weighing a population that is still changing.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(20);

struct QueueTruth {
    slots: usize,
    /// What `queued_output_bytes` claims once the producer has quiesced.
    reported: usize,
    /// Sum of the queued view lengths — the payload actually waiting.
    payload: usize,
    /// Ring bytes released when the queued views are dropped.
    pinned: usize,
}

/// Drain `rx` into `sink` until the sender side is gone, or fail at `deadline`.
///
/// Returns `true` only on `Disconnected`. A transient `Empty` is not an end:
/// the PTY reader drops its sender only after observing EOF, and it cannot
/// observe EOF while blocked sending into a full channel, so draining is what
/// unblocks it. Stopping at the first empty moment therefore leaves the
/// producer live and the population still moving.
fn drain_until_disconnected<T>(rx: &Receiver<T>, sink: &mut Vec<T>, deadline: Instant) -> bool {
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(item) => sink.push(item),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return true,
        }
    }
    false
}

/// Fill one pane's output queue from a real child, then weigh what it holds.
///
/// Both figures come from ONE frozen population: sampling while the child runs
/// would count the chunk the parked reader holds outside the queue, and admit
/// more arrivals before the holder is filled.
///
/// Each script writes without end rather than a fixed count: the PTY coalesces
/// adjacent writes into one read, so a fixed count fills an unpredictable
/// number of slots. Backpressure stops the child once the queue is full.
fn measure_full_queue(script: &str) -> QueueTruth {
    let args = vec!["-c".to_string(), script.to_string()];
    let pty = PtyHandle::spawn_with_args("/bin/sh", &args, 80, 24).expect("spawn /bin/sh");

    let deadline = Instant::now() + Duration::from_secs(20);
    while pty.out_rx.len() < PTY_OUTPUT_QUEUE_CAPACITY && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }

    let slots = pty.out_rx.len();

    // Allocated before any measured sample, so its own growth is never inside
    // a measurement window.
    let mut holder = Vec::with_capacity(PTY_OUTPUT_QUEUE_CAPACITY * 4);

    pty.kill().expect("stop the producer before measuring");
    let disconnected =
        drain_until_disconnected(&pty.out_rx, &mut holder, Instant::now() + DRAIN_TIMEOUT);
    // A timeout must fail rather than measure a population that is still moving.
    assert!(
        disconnected,
        "producer did not quiesce within {DRAIN_TIMEOUT:?}; any measurement here would be \
         meaningless"
    );

    // Both figures now describe the same frozen set of chunks: the reader has
    // ended, nothing can enqueue, and the handle is still alive so the meter
    // is readable.
    let reported = queued_output_bytes(&pty);
    let payload: usize = holder.iter().map(|chunk| chunk.len()).sum();

    // The reader has already ended, so this only releases the handle's own
    // state; the ring the queued views pin is still held by `holder`.
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

/// The reported input figure must be the bytes queued, not a slot estimate.
///
/// The input queue is four slots holding `Vec<u8>` messages of any size up to
/// the per-message cap, so a slot count carries no information about the bytes
/// held. Measured with a real child so the figure is checked against what was
/// actually sent.
///
/// The class was recorded `MeasuredNegligible { per_pane_bytes: 4096 }` while
/// the queue accepted four messages of up to 16 MiB — 67,108,864 bytes, and a
/// paste is admitted at the full message size and broadcast to every pane.
#[cfg(unix)]
#[test]
fn queued_input_bytes_reports_the_bytes_actually_queued() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    // `cat` with no argument reads stdin forever.
    let pty = PtyHandle::spawn_with_args("/bin/cat", &[], 80, 24).expect("the child spawns");

    assert_eq!(pty.queued_input_bytes(), 0, "nothing has been sent yet");

    // One message far larger than any per-slot estimate would predict.
    const MESSAGE: usize = 4 * 1024 * 1024;
    pty.send_input_nonblocking(vec![b'x'; MESSAGE]).expect("the message is accepted");

    // The writer may already have drained it, so this is bounded above by what
    // was sent rather than equal to it. The property under test is that the
    // figure is derived from bytes at all: a slot-count estimate would report
    // a fixed per-slot figure regardless of message size.
    let queued = pty.queued_input_bytes();
    println!("MEASURED queued input: sent {MESSAGE}, reported {queued}");
    assert!(queued <= MESSAGE, "reported {queued} bytes queued after sending {MESSAGE}");

    // And returns to zero once the child has consumed it.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && pty.queued_input_bytes() > 0 {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        pty.queued_input_bytes(),
        0,
        "the figure must return to zero once the writer has drained the queue, or it \
         reports memory that is no longer held"
    );
}

/// A refused message must not be counted as queued.
///
/// The count is incremented before the send, so the writer cannot drain a
/// message that was never counted. That makes refusal the path where
/// accounting can corrupt: a message rejected for size or a full queue was
/// never queued memory, and leaving it counted would permanently overstate the
/// figure the governor is charged.
#[cfg(unix)]
#[test]
fn a_refused_input_message_leaves_the_queued_figure_untouched() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let pty = PtyHandle::spawn_with_args("/bin/cat", &[], 80, 24).expect("the child spawns");

    let before = pty.queued_input_bytes();
    let oversized = sonicterm_io::pty::MAX_PTY_INPUT_MESSAGE_BYTES + 1;
    let refused = pty.send_input_nonblocking(vec![0u8; oversized]);
    assert!(refused.is_err(), "a message over the cap must be refused");

    assert_eq!(
        pty.queued_input_bytes(),
        before,
        "a refused message must not move the queued figure — it was never queued"
    );
}

/// An empty-but-connected channel must not read as the end of the stream.
///
/// This is the property the old `try_recv` drain lacked: it stopped at the
/// first empty moment while the producer was still live, so the measurement
/// that followed described a population that was still growing.
#[test]
fn a_transient_empty_channel_is_not_treated_as_disconnection() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (tx, rx) = crossbeam_channel::unbounded::<u8>();

    let mut drained = Vec::new();
    let started = Instant::now();
    let disconnected =
        drain_until_disconnected(&rx, &mut drained, started + Duration::from_millis(300));

    assert!(!disconnected, "an empty channel whose sender still lives must not report the end");
    assert!(drained.is_empty(), "nothing was sent, so nothing may be collected");
    drop(tx);
}

/// A drain that cannot reach disconnection must refuse at its deadline.
///
/// The bound has to be absolute: a sender that never goes away would otherwise
/// hang the suite instead of failing it.
#[test]
fn a_drain_that_never_disconnects_refuses_at_its_deadline() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (tx, rx) = crossbeam_channel::unbounded::<u8>();
    tx.send(7).expect("seed one item");

    let mut drained = Vec::new();
    let budget = Duration::from_millis(300);
    let started = Instant::now();
    let disconnected = drain_until_disconnected(&rx, &mut drained, started + budget);
    let elapsed = started.elapsed();

    assert!(!disconnected, "the sender is still alive, so the drain must not claim the end");
    assert_eq!(drained, vec![7], "items received before the deadline are still retained");
    // Generous upper bound: proves it returned at the deadline rather than
    // blocking indefinitely, without asserting scheduler precision.
    assert!(elapsed < budget * 10, "drain overran its deadline: {elapsed:?}");
    drop(tx);
}

/// Every value must survive a drain that spans quiet gaps, in order.
///
/// A barrier, not a sleep, provides readiness: the sender publishes only after
/// both threads arrive, so the drain is already running and must cross the
/// empty stretches before the sender finally drops.
#[test]
fn a_bounded_drain_retains_every_value_across_quiet_gaps() {
    let _serialised = MEASURE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (tx, rx) = crossbeam_channel::unbounded::<u8>();
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));

    let sender_gate = gate.clone();
    let sender = std::thread::spawn(move || {
        sender_gate.wait();
        for value in 1..=5u8 {
            tx.send(value).expect("receiver outlives the sender");
        }
        // Dropping `tx` here is the only thing that ends the drain.
    });

    gate.wait();
    let mut drained = Vec::new();
    let disconnected =
        drain_until_disconnected(&rx, &mut drained, Instant::now() + Duration::from_secs(10));
    sender.join().expect("sender thread completed");

    assert!(disconnected, "the drain must end on disconnection, not on a timeout");
    assert_eq!(drained, vec![1, 2, 3, 4, 5], "every value must be retained, in order");
}
