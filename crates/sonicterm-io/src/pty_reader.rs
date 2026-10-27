use super::*;

pub(super) fn send_pty_output(
    tx: &Sender<Incoming>,
    cancel: &Receiver<()>,
    chunk: Incoming,
) -> bool {
    crossbeam_channel::select! {
        send(tx, chunk) -> result => result.is_ok(),
        recv(cancel) -> _ => false,
    }
}

/// Pump `reader` into `tx` on a dedicated thread. With `stamp_reads`, each chunk carries the
/// instant its native read returned; the flag is fixed for the reader's life.
pub(super) fn spawn_reader_thread(
    reader: Box<dyn Read + Send>,
    tx: Sender<Incoming>,
    cancel: Receiver<()>,
    meter: Arc<QueuedOutputMeter>,
    stamp_reads: bool,
) -> PtyIoThread {
    spawn_reader_thread_with(reader, tx, cancel, meter, stamp_reads, || {})
}

/// As [`spawn_reader_thread`], with `after_read` run right after each successful nonempty read is
/// stamped and before the bytes are split, charged or sent; tests delay construction through it.
fn spawn_reader_thread_with(
    mut reader: Box<dyn Read + Send>,
    tx: Sender<Incoming>,
    cancel: Receiver<()>,
    meter: Arc<QueuedOutputMeter>,
    stamp_reads: bool,
    mut after_read: impl FnMut() + Send + 'static,
) -> PtyIoThread {
    let (done_tx, done) = crossbeam_channel::bounded(1);
    let handle = thread::Builder::new()
        .name("sonic-pty-reader".into())
        .spawn(move || {
            let _active = ActivePtyIoThread::enter();
            // 64 KiB ring. We `split` the filled prefix into a `Bytes`
            // (refcounted view into the same allocation) on each read and
            // send it downstream. Once consumers drop their `Bytes`, the
            // next `reserve` call reclaims the original allocation in-place
            // — no per-read heap alloc, no `to_vec`. Replaces the previous
            // `[u8; 8192]` stack buffer + `buf[..n].to_vec()` pattern that
            // allocated once per read (and the reader can fire thousands of
            // reads per second under `cat largefile`).
            const RING_CAP: usize = PTY_READ_RING_BYTES;
            // Keep at least one full PTY chunk (typical kernel pipe buffer
            // is 4–16 KiB) of headroom before each read to avoid forcing a
            // realloc mid-read.
            const READ_HEADROOM: usize = 8 * 1024;
            let mut buf = BytesMut::with_capacity(RING_CAP);
            // The whole allocation, not the unsplit remainder. `split` hands
            // out views into one buffer and shrinks `buf`'s window; the memory
            // behind an earlier view is not reclaimable while any view lives,
            // so a charge computed from the shrunken window would undercount
            // the ring by everything already handed out. Sampled straight
            // after each `reserve`, when the window is the allocation.
            let mut ring_bytes = buf.capacity();
            // Weak, so the charge for the ring we are filling lives exactly as
            // long as some chunk still views into it. Holding an `Arc` here
            // would keep an emptied queue charged for a ring nobody is waiting
            // on; upgrading tells us whether this ring is already charged.
            let mut ring_charge: std::sync::Weak<RingCharge> = std::sync::Weak::new();
            loop {
                if buf.capacity() - buf.len() < READ_HEADROOM {
                    // If downstream has dropped its `Bytes` views, this
                    // reclaims the original buffer; otherwise it allocates
                    // a fresh one and drops our half of the previous ring.
                    buf.reserve(RING_CAP);
                    // Either way the bytes we hand out from here belong to a
                    // different ring than the charge above describes.
                    ring_bytes = buf.capacity();
                    ring_charge = std::sync::Weak::new();
                }
                // Zero-initialise the spare region before handing it to
                // `Read::read`. `Read` requires an initialised destination
                // slice (passing `MaybeUninit` bytes via a `&mut [u8]` cast
                // is UB even though most impls never read from it). The
                // memset cost on a 64 KiB region is dominated by the syscall
                // itself; the underlying allocation is still reused across
                // reads, preserving the zero-alloc steady state.
                let initial_len = buf.len();
                let read_cap = buf.capacity() - initial_len;
                buf.resize(initial_len + read_cap, 0);
                // When: `reader` data publishes its initialized prefix; EOF or error stops the pump.
                match reader.read(&mut buf[initial_len..]) {
                    Ok(0) => break,
                    Ok(n) => {
                        // The stamp is the read's return: taken before any split, charge or send.
                        let read_at = stamp_reads.then(Instant::now);
                        after_read();
                        buf.truncate(initial_len + n);
                        let chunk = buf.split().freeze();
                        // Reuse this ring's charge if any queued view still
                        // holds it, so one allocation is counted once however
                        // many views come out of it.
                        let charge = match ring_charge.upgrade() {
                            Some(existing) => existing,
                            None => {
                                let fresh = RingCharge::new(ring_bytes, &meter);
                                ring_charge = Arc::downgrade(&fresh);
                                fresh
                            }
                        };
                        let chunk = PtyOutputChunk::new(chunk, charge, &meter, read_at);
                        if !send_pty_output(&tx, &cancel, chunk) {
                            // When: `send_pty_output` reports cancellation or disconnection, stop the reader pump.
                            break;
                        }
                    }
                    Err(e) => {
                        if cancel.try_recv().is_err() {
                            tracing::warn!("pty read error: {e}");
                        }
                        break;
                    }
                }
            }
            let _ = done_tx.send(());
        })
        // PANIC: thread::Builder::spawn only fails on OS-level resource
        // exhaustion (out of memory / out of process handles). At terminal
        // startup we cannot meaningfully recover — propagating a Result up
        // through `spawn_pane` would land on the same `expect`. Documented.
        .expect("spawn pty reader");
    PtyIoThread { handle: Some(handle), done, failed: false }
}

#[cfg(test)]
#[path = "pty_reader_tests.rs"]
mod pty_reader_tests;
