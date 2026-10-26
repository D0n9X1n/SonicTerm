use super::*;

#[test]
fn pty_output_queue_applies_backpressure_at_fixed_capacity() {
    let (tx, rx) = pty_output_channel();
    let meter = Arc::new(QueuedOutputMeter::default());
    for _ in 0..PTY_OUTPUT_QUEUE_CAPACITY {
        tx.try_send(PtyOutputChunk::for_test(b"x", PTY_READ_RING_BYTES, &meter))
            .expect("queue has bounded capacity");
    }

    assert!(matches!(
        tx.try_send(PtyOutputChunk::for_test(b"overflow", PTY_READ_RING_BYTES, &meter)),
        Err(crossbeam_channel::TrySendError::Full(_))
    ));
    assert_eq!(rx.len(), PTY_OUTPUT_QUEUE_CAPACITY);
}

#[test]
fn pty_output_send_can_be_cancelled_while_queue_is_full() {
    let (tx, _rx) = pty_output_channel();
    let meter = Arc::new(QueuedOutputMeter::default());
    for _ in 0..PTY_OUTPUT_QUEUE_CAPACITY {
        tx.try_send(PtyOutputChunk::for_test(b"x", PTY_READ_RING_BYTES, &meter))
            .expect("fill bounded queue");
    }
    let (cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let (done_tx, done_rx) = crossbeam_channel::bounded(1);
    let blocked = PtyOutputChunk::for_test(b"blocked", PTY_READ_RING_BYTES, &meter);
    std::thread::spawn(move || {
        done_tx.send(send_pty_output(&tx, &cancel_rx, blocked)).expect("report send outcome");
    });

    cancel_tx.send(()).expect("signal cancellation");

    assert!(!done_rx.recv_timeout(std::time::Duration::from_secs(1)).expect("reader unblocked"));
}

/// Many views into one ring cost one ring, not one ring per view.
///
/// This is the whole reason the figure is not a per-chunk sum: 64 keystroke
/// echoes are 64 views of one 64 KiB allocation, and counting them separately
/// would report 4 MiB for 64 bytes of output.
#[test]
fn views_sharing_one_ring_are_charged_once() {
    let meter = Arc::new(QueuedOutputMeter::default());
    let ring = RingCharge::new(PTY_READ_RING_BYTES, &meter);

    let chunks: Vec<PtyOutputChunk> = (0..PTY_OUTPUT_QUEUE_CAPACITY)
        .map(|_| PtyOutputChunk::new(Bytes::from_static(b"x"), ring.clone(), &meter, None))
        .collect();

    assert_eq!(meter.ring_bytes.load(Ordering::Acquire), PTY_READ_RING_BYTES);
    assert_eq!(meter.payload_bytes.load(Ordering::Acquire), PTY_OUTPUT_QUEUE_CAPACITY);

    drop(chunks);
    drop(ring);
    assert_eq!(meter.ring_bytes.load(Ordering::Acquire), 0);
    assert_eq!(meter.payload_bytes.load(Ordering::Acquire), 0);
}

/// Views into different rings add up.
#[test]
fn views_from_distinct_rings_each_cost_a_ring() {
    let meter = Arc::new(QueuedOutputMeter::default());

    let chunks: Vec<PtyOutputChunk> =
        (0..3).map(|_| PtyOutputChunk::for_test(b"x", PTY_READ_RING_BYTES, &meter)).collect();

    assert_eq!(meter.ring_bytes.load(Ordering::Acquire), 3 * PTY_READ_RING_BYTES);

    drop(chunks);
    assert_eq!(meter.ring_bytes.load(Ordering::Acquire), 0);
}

/// A ring stays charged until its last view goes, not its first.
///
/// The failure this rules out is releasing on the first drop: the allocation
/// is still held by every surviving view, so a figure that let go early would
/// report free memory the process cannot reuse.
#[test]
fn a_ring_is_released_only_when_its_last_view_drops() {
    let meter = Arc::new(QueuedOutputMeter::default());
    let ring = RingCharge::new(PTY_READ_RING_BYTES, &meter);

    let first = PtyOutputChunk::new(Bytes::from_static(b"first"), ring.clone(), &meter, None);
    let second = PtyOutputChunk::new(Bytes::from_static(b"second"), ring.clone(), &meter, None);
    drop(ring);

    drop(first);
    assert_eq!(
        meter.ring_bytes.load(Ordering::Acquire),
        PTY_READ_RING_BYTES,
        "the ring is still viewed by the second chunk"
    );
    assert_eq!(meter.payload_bytes.load(Ordering::Acquire), b"second".len());

    drop(second);
    assert_eq!(meter.ring_bytes.load(Ordering::Acquire), 0);
    assert_eq!(meter.payload_bytes.load(Ordering::Acquire), 0);
}

/// The figures come back to zero when the queue drains.
#[test]
fn draining_the_queue_returns_both_figures_to_zero() {
    let (tx, rx) = pty_output_channel();
    let meter = Arc::new(QueuedOutputMeter::default());
    for _ in 0..PTY_OUTPUT_QUEUE_CAPACITY {
        tx.try_send(PtyOutputChunk::for_test(b"payload", PTY_READ_RING_BYTES, &meter))
            .expect("fill bounded queue");
    }

    assert!(meter.ring_bytes.load(Ordering::Acquire) > 0);

    while rx.try_recv().is_ok() {}

    assert_eq!(meter.ring_bytes.load(Ordering::Acquire), 0);
    assert_eq!(meter.payload_bytes.load(Ordering::Acquire), 0);
}

/// The worst case is one ring per slot, and it is the queue's real ceiling.
#[test]
fn the_ring_ceiling_is_one_ring_per_slot() {
    assert_eq!(max_queued_output_ring_bytes(), PTY_OUTPUT_QUEUE_CAPACITY * PTY_READ_RING_BYTES);
    assert_eq!(max_queued_output_ring_bytes(), 4 * 1024 * 1024);
}
