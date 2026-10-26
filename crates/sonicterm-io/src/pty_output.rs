use super::*;

impl RingCharge {
    // Ordering: `ring_bytes` uses `AcqRel` for atomic charge accounting; it does not guard ring memory.
    pub(super) fn new(bytes: usize, meter: &Arc<QueuedOutputMeter>) -> Arc<Self> {
        meter.ring_bytes.fetch_add(bytes, Ordering::AcqRel);
        Arc::new(Self { bytes, meter: meter.clone() })
    }
}

// Lifecycle: dropping `RingCharge` releases its `ring_bytes` allocation charge.
impl Drop for RingCharge {
    // Ordering: `ring_bytes` uses `AcqRel` for atomic charge accounting; it does not synchronize ring release.
    fn drop(&mut self) {
        self.meter.ring_bytes.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

impl PtyOutputChunk {
    // Ordering: `payload_bytes` uses `AcqRel` for atomic payload accounting; it does not guard chunk data.
    pub(super) fn new(
        bytes: Bytes,
        ring: Arc<RingCharge>,
        meter: &Arc<QueuedOutputMeter>,
        read_at: Option<Instant>,
    ) -> Self {
        meter.payload_bytes.fetch_add(bytes.len(), Ordering::AcqRel);
        Self { bytes, _ring: ring, meter: meter.clone(), read_at }
    }

    /// When the native read that produced this chunk returned, sampled before the chunk was split
    /// or charged; `None` unless the PTY was spawned with `ShellSpawnOpts::diagnostic_timestamps`.
    #[must_use]
    pub fn read_at(&self) -> Option<Instant> {
        self.read_at
    }

    /// The output bytes, as a refcounted slice.
    ///
    /// Cloning the returned `Bytes` outlives this chunk's charge, so a caller
    /// that retains one holds ring memory the queue figure no longer reports.
    /// Parse from it and drop it; do not park it in a cache.
    #[must_use]
    pub fn bytes(&self) -> &Bytes {
        &self.bytes
    }

    /// Build a chunk charged against `meter` as if split from a `ring_bytes`
    /// ring, for tests that drive the queue without a child process.
    #[cfg(test)]
    fn for_test(bytes: &'static [u8], ring_bytes: usize, meter: &Arc<QueuedOutputMeter>) -> Self {
        Self::new(Bytes::from_static(bytes), RingCharge::new(ring_bytes, meter), meter, None)
    }
}

// Lifecycle: dropping `PtyOutputChunk` releases its `payload_bytes` queue charge.
impl Drop for PtyOutputChunk {
    // Ordering: `payload_bytes` uses `AcqRel` for atomic payload accounting; it does not synchronize chunk release.
    fn drop(&mut self) {
        self.meter.payload_bytes.fetch_sub(self.bytes.len(), Ordering::AcqRel);
    }
}

impl std::ops::Deref for PtyOutputChunk {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}

impl AsRef<[u8]> for PtyOutputChunk {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

pub(super) fn pty_output_channel() -> (Sender<Incoming>, Receiver<Incoming>) {
    crossbeam_channel::bounded(PTY_OUTPUT_QUEUE_CAPACITY)
}

/// Ring memory this pane's queued PTY output holds down.
///
/// The reader hands out `Bytes` views into a reused 64 KiB ring, so neither
/// obvious arithmetic describes the queue. The slot count times a chunk size
/// counts buffers that were never allocated; the sum of the view lengths
/// counts payload while ignoring the ring those views pin. Measured on a full
/// 64-slot queue from `/bin/sh`: 64 bytes of keystroke echo, 65,536 bytes of
/// ring — the payload figure would have understated the memory 1024x.
///
/// This reports the ring: the bytes that stay allocated until the queue
/// drains, which is what the governor is deciding about. Use
/// [`queued_output_payload_bytes`] for the data waiting to be parsed.
///
/// Maintained by the chunks as they are created and dropped, so this observes
/// without consuming and may be sampled from any thread without disturbing the
/// pump.
// Ordering: `ring_bytes` uses `Acquire` to sample the atomic accounting value, not to guard ring memory.
#[must_use]
pub fn queued_output_bytes(handle: &PtyHandle) -> usize {
    handle.output_meter.ring_bytes.load(Ordering::Acquire)
}

/// Payload bytes waiting in this pane's PTY output channel.
///
/// The sum of the queued view lengths — what the VT thread still has to parse.
/// This is not the memory cost: 64 keystroke echoes are 64 bytes here and
/// 65,536 bytes of pinned ring. [`queued_output_bytes`] reports the latter.
// Ordering: `payload_bytes` uses `Acquire` to sample the atomic accounting value, not to guard payload data.
#[must_use]
pub fn queued_output_payload_bytes(handle: &PtyHandle) -> usize {
    handle.output_meter.payload_bytes.load(Ordering::Acquire)
}

/// Reader ring size, and the granularity of [`queued_output_bytes`].
///
/// The reader fills one allocation of this size and splits views out of it
/// until the remaining headroom is too small for another read, so a queue's
/// pinned total is always a multiple of this.
pub const PTY_READ_RING_BYTES: usize = 64 * 1024;

/// Worst-case ring memory one pane's output queue can pin.
///
/// Reached only if every queued view came from a different ring, which needs
/// reads large enough to exhaust an allocation each. Real shells do not do
/// this: every `/bin/sh` workload measured pinned exactly one ring, because
/// PTY reads arrive far smaller than the ring.
#[must_use]
pub const fn max_queued_output_ring_bytes() -> usize {
    PTY_OUTPUT_QUEUE_CAPACITY.saturating_mul(PTY_READ_RING_BYTES)
}

#[cfg(test)]
#[path = "pty_output_tests.rs"]
mod pty_output_tests;
