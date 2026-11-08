use super::*;
#[cfg(windows)]
use crate::pty::pty_test_support::lock_live_pty_test;

/// A reader that returns each scripted chunk once, then EOF, recording the instant just before it
/// returns each chunk so a test can bound when the read completed.
struct ScriptedReader {
    chunks: std::collections::VecDeque<&'static [u8]>,
    returned_at: Arc<parking_lot::Mutex<Vec<Instant>>>,
}

impl Read for ScriptedReader {
    fn read(&mut self, destination: &mut [u8]) -> std::io::Result<usize> {
        let Some(chunk) = self.chunks.pop_front() else {
            return Ok(0);
        };
        destination[..chunk.len()].copy_from_slice(chunk);
        self.returned_at.lock().push(Instant::now());
        Ok(chunk.len())
    }
}

/// Run a reader thread over `script` with `stamp_reads`, delaying each chunk's construction through
/// `after_read`, and return the chunks it sent plus the instants each read returned.
fn pump_script(
    script: &[&'static [u8]],
    stamp_reads: bool,
    after_read: impl FnMut() + Send + 'static,
) -> (Vec<PtyOutputChunk>, Vec<Instant>) {
    let returned_at = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let reader = ScriptedReader {
        chunks: script.iter().copied().collect(),
        returned_at: returned_at.clone(),
    };
    let (tx, rx) = crossbeam_channel::unbounded();
    let (_cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let meter = Arc::new(QueuedOutputMeter::default());
    let mut thread = spawn_reader_thread_with(
        Box::new(reader),
        tx,
        cancel_rx,
        meter,
        stamp_reads,
        after_read,
        || {},
    );
    let chunks: Vec<PtyOutputChunk> = rx.iter().collect();
    thread.handle.take().expect("reader handle").join().expect("reader thread");
    let returned = returned_at.lock().clone();
    (chunks, returned)
}

/// A chunk's `read_at` is the read's return, taken before construction: with construction delayed
/// after every read, each stamp lies between the reader returning and the delay starting, never
/// after.
#[test]
fn a_chunk_is_stamped_at_its_read_before_construction() {
    let entered = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let hook_entered = entered.clone();
    let delay = move || {
        hook_entered.lock().push(Instant::now());
        std::thread::sleep(Duration::from_millis(5));
    };
    let (chunks, returned) = pump_script(&[b"one", b"two", b"three"], true, delay);
    let entered = entered.lock().clone();
    assert_eq!((chunks.len(), returned.len(), entered.len()), (3, 3, 3));
    for (index, chunk) in chunks.iter().enumerate() {
        let read_at = chunk.read_at().expect("a stamped chunk");
        assert!(returned[index] <= read_at, "chunk {index} stamped before its read returned");
        assert!(read_at <= entered[index], "chunk {index} stamped after construction began");
    }
}

/// The flag decides every chunk for the reader's life: off, no chunk carries a stamp; on, every
/// nonempty read does. `spawn` and `spawn_with_args` keep it off through the default options.
#[test]
fn diagnostic_timestamps_stamp_every_read_only_when_on() {
    let script: [&'static [u8]; 4] = [b"a", b"bb", b"ccc", b"dddd"];
    let (off, _) = pump_script(&script, false, || {});
    assert_eq!(off.len(), 4);
    assert!(off.iter().all(|chunk| chunk.read_at().is_none()), "a stamp with the flag off");
    let (on, _) = pump_script(&script, true, || {});
    assert_eq!(on.len(), 4);
    assert!(on.iter().all(|chunk| chunk.read_at().is_some()), "a read left unstamped");
    assert!(!ShellSpawnOpts::default().diagnostic_timestamps, "the default stamps nothing");
}

/// A real PTY spawned with `diagnostic_timestamps` stamps its output chunks, and one spawned
/// without it does not: the spawn options reach the reader thread, not just the struct.
#[test]
fn a_spawned_pty_stamps_its_output_only_with_the_diagnostic_option() {
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    for stamped in [false, true] {
        let pty = PtyHandle::spawn_default_shell(
            80,
            24,
            ShellSpawnOpts {
                clean_e2e: true,
                diagnostic_timestamps: stamped,
                ..Default::default()
            },
        )
        .unwrap();
        let chunk = pty
            .out_rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|error| panic!("no output with stamping {stamped}: {error}"));
        assert_eq!(chunk.read_at().is_some(), stamped, "stamping {stamped}");
        drop(chunk);
        drop(pty);
    }
}

/// A reader that answers its first `interruptions` reads with `Interrupted`, then returns `chunk`
/// once, then EOF.
struct InterruptingReader {
    interruptions: usize,
    chunk: Option<&'static [u8]>,
}

impl Read for InterruptingReader {
    fn read(&mut self, destination: &mut [u8]) -> std::io::Result<usize> {
        if self.interruptions > 0 {
            self.interruptions -= 1;
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        let Some(chunk) = self.chunk.take() else {
            return Ok(0);
        };
        destination[..chunk.len()].copy_from_slice(chunk);
        Ok(chunk.len())
    }
}

/// Pump `reader` to EOF and return every byte the pump published, in order.
fn pumped_bytes(reader: impl Read + Send + 'static) -> Vec<u8> {
    let (tx, rx) = crossbeam_channel::unbounded();
    let (_cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let meter = Arc::new(QueuedOutputMeter::default());
    let mut thread = spawn_reader_thread(Box::new(reader), tx, cancel_rx, meter, false);
    let chunks: Vec<PtyOutputChunk> = rx.iter().collect();
    thread.handle.take().expect("reader handle").join().expect("reader thread");
    chunks.iter().flat_map(|chunk| chunk.bytes().to_vec()).collect()
}

/// `Interrupted` means "read again", not the end of the pane's output: the pump retries and
/// publishes exactly the bytes that follow, with no zeroed filler from the interrupted read.
#[test]
fn an_interrupted_read_is_retried_and_the_output_still_arrives() {
    let delivered = pumped_bytes(InterruptingReader { interruptions: 1, chunk: Some(b"ready") });
    assert_eq!(delivered, b"ready");
}

/// Many interruptions in a row still publish exactly the following bytes: each retry restores the
/// buffer, so no zeroed region accumulates or reaches the output.
#[test]
fn repeated_interruptions_publish_only_the_real_bytes() {
    let delivered = pumped_bytes(InterruptingReader { interruptions: 50, chunk: Some(b"ready") });
    assert_eq!(delivered, b"ready");
}

/// A reader that is interrupted forever: the pump keeps retrying until the pane is cancelled.
struct AlwaysInterrupted;

impl Read for AlwaysInterrupted {
    fn read(&mut self, _destination: &mut [u8]) -> std::io::Result<usize> {
        std::thread::sleep(Duration::from_millis(1));
        Err(std::io::ErrorKind::Interrupted.into())
    }
}

/// Cancelling a pane whose reads keep being interrupted ends the pump, and it published nothing.
#[test]
fn cancellation_ends_a_pump_that_keeps_being_interrupted() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let (cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let meter = Arc::new(QueuedOutputMeter::default());
    let mut thread = spawn_reader_thread(Box::new(AlwaysInterrupted), tx, cancel_rx, meter, false);
    std::thread::sleep(Duration::from_millis(20));
    cancel_tx.send(()).expect("the pump is still running");
    let handle = thread.handle.take().expect("reader handle");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !handle.is_finished() {
        assert!(Instant::now() < deadline, "the pump ignored cancellation while interrupted");
        std::thread::sleep(Duration::from_millis(5));
    }
    handle.join().expect("reader thread");
    assert_eq!(rx.try_iter().count(), 0, "an interrupted read published a chunk");
}

/// What a [`WindowProbe`] saw on one read: the destination's length, and whether the bytes it
/// marked past its last return were still there, which tells a reused window from a re-zeroed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReadWindow {
    /// The destination slice's length.
    len: usize,
    /// Whether the destination's tail still held this probe's marks from the previous read.
    marks_kept: bool,
}

/// A reader that returns up to `sizes` bytes of a running counter pattern (a read never exceeds its
/// destination, as a real one cannot), then EOF, recording each read's return. After each read it
/// writes a mark byte over the rest of the destination; seeing those marks on the next read shows
/// the pump reused the window instead of zeroing it again.
struct WindowProbe {
    sizes: std::collections::VecDeque<usize>,
    next_byte: u8,
    marked: usize,
    windows: Arc<parking_lot::Mutex<Vec<ReadWindow>>>,
    returned: Arc<parking_lot::Mutex<Vec<usize>>>,
}

/// The byte a [`WindowProbe`] leaves in the unread tail of each destination.
const MARK: u8 = 0xA5;

impl Read for WindowProbe {
    fn read(&mut self, destination: &mut [u8]) -> std::io::Result<usize> {
        let tail_start = destination.len().saturating_sub(self.marked);
        let marks_kept =
            self.marked > 0 && destination[tail_start..].iter().all(|byte| *byte == MARK);
        self.windows.lock().push(ReadWindow { len: destination.len(), marks_kept });
        let Some(wanted) = self.sizes.pop_front() else {
            return Ok(0);
        };
        let size = wanted.min(destination.len());
        self.returned.lock().push(size);
        for byte in &mut destination[..size] {
            *byte = self.next_byte;
            // Never the mark, so a leaked mark is always visible in the output.
            self.next_byte = if self.next_byte == MARK - 1 { 0 } else { self.next_byte + 1 };
        }
        destination[size..].fill(MARK);
        self.marked = destination.len() - size;
        Ok(size)
    }
}

/// The counter pattern a [`WindowProbe`] produces for `total` bytes.
fn probe_pattern(total: usize) -> Vec<u8> {
    let mut next_byte = 0u8;
    (0..total)
        .map(|_| {
            let byte = next_byte;
            next_byte = if next_byte == MARK - 1 { 0 } else { next_byte + 1 };
            byte
        })
        .collect()
}

/// One [`WindowProbe`] run: what the pump published and what the reader saw and returned.
struct ProbeRun {
    /// Every published byte, in order.
    bytes: Vec<u8>,
    /// Each read's window.
    windows: Vec<ReadWindow>,
    /// Each nonempty read's byte count.
    returned: Vec<usize>,
    /// The chunks, held so no ring is reclaimed while the run is inspected.
    _chunks: Vec<PtyOutputChunk>,
}

/// Pump a [`WindowProbe`] over `sizes` to EOF.
fn pump_probe(sizes: &[usize]) -> ProbeRun {
    let windows = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let returned = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let reader = WindowProbe {
        sizes: sizes.iter().copied().collect(),
        next_byte: 0,
        marked: 0,
        windows: windows.clone(),
        returned: returned.clone(),
    };
    let (tx, rx) = crossbeam_channel::unbounded();
    let (_cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let meter = Arc::new(QueuedOutputMeter::default());
    let mut thread = spawn_reader_thread(Box::new(reader), tx, cancel_rx, meter, false);
    let chunks: Vec<PtyOutputChunk> = rx.iter().collect();
    thread.handle.take().expect("reader handle").join().expect("reader thread");
    let bytes = chunks.iter().flat_map(|chunk| chunk.bytes().to_vec()).collect();
    let windows = windows.lock().clone();
    let returned = returned.lock().clone();
    ProbeRun { bytes, windows, returned, _chunks: chunks }
}

/// A ring is zeroed once, not before every read: a hundred small reads into one ring each find the
/// previous read's unread tail intact, and each window is the ring's remainder after the bytes
/// already handed out, exactly the room the old per-read zeroing offered.
#[test]
fn small_reads_reuse_one_zeroed_window() {
    let ProbeRun { bytes, windows, .. } = pump_probe(&[16; 100]);
    assert_eq!(bytes, probe_pattern(16 * 100), "published bytes differ from what was read");
    let ring = windows[0].len;
    assert!(ring >= PTY_READ_RING_BYTES, "the first window is a whole ring: {ring}");
    for (index, window) in windows.iter().enumerate() {
        assert_eq!(window.len, ring - 16 * index, "read {index}'s window is not the ring's rest");
        assert_eq!(window.marks_kept, index > 0, "read {index} saw a re-zeroed window");
    }
}

/// Mixed reads across several refills publish exactly the bytes read, never a zero or mark from the
/// window; a refill happens only once the window is shorter than the read headroom, and it zeroes
/// a fresh window once, while every read in between reuses the window it was given.
#[test]
fn mixed_reads_across_refills_publish_only_what_was_read() {
    let sizes: Vec<usize> =
        [16, 1024, 16 * 1024, 7, 16 * 1024, 16 * 1024, 900, 16 * 1024, 16 * 1024, 16 * 1024, 3]
            .repeat(3);
    let ProbeRun { bytes, windows, returned, .. } = pump_probe(&sizes);
    let total: usize = returned.iter().sum();
    assert_eq!(bytes, probe_pattern(total), "published bytes differ from what was read");
    let mut refills = 0;
    for (index, window) in windows.iter().enumerate().skip(1).take(returned.len() - 1) {
        let previous = windows[index - 1];
        let left = previous.len - returned[index - 1];
        if left < 8 * 1024 {
            // The previous window ran short: this read gets a fresh, zeroed ring.
            refills += 1;
            assert!(!window.marks_kept, "read {index} reused a window after a refill");
            assert!(window.len >= PTY_READ_RING_BYTES, "read {index}'s refilled window is short");
        } else {
            assert!(window.marks_kept, "read {index} re-zeroed a window that had room");
            assert_eq!(window.len, left, "read {index}'s window is not the previous one's rest");
        }
    }
    assert!(refills >= 3, "the script crossed only {refills} refills");
}

/// An interrupted read leaves the window as it was, so the retry reads into the same room and
/// publishes only what it read.
#[test]
fn an_interrupted_read_keeps_its_window() {
    /// Returns 10 bytes, is interrupted once, then returns 10 more and EOF; records each window.
    struct InterruptOnce {
        step: usize,
        windows: Arc<parking_lot::Mutex<Vec<usize>>>,
    }
    impl Read for InterruptOnce {
        fn read(&mut self, destination: &mut [u8]) -> std::io::Result<usize> {
            self.windows.lock().push(destination.len());
            self.step += 1;
            match self.step {
                1 | 3 => {
                    destination[..10].fill(b'a' + self.step as u8);
                    Ok(10)
                }
                2 => Err(std::io::ErrorKind::Interrupted.into()),
                _ => Ok(0),
            }
        }
    }
    let windows = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let delivered = pumped_bytes(InterruptOnce { step: 0, windows: windows.clone() });
    assert_eq!(delivered, [[b'b'; 10], [b'd'; 10]].concat());
    let seen = windows.lock().clone();
    assert_eq!(seen[1], seen[0] - 10, "the interrupted read's window");
    assert_eq!(seen[2], seen[1], "the retry's window changed after an interruption");
}

/// A reader that returns `size` bytes per read until `left` runs out, then EOF, recording each
/// destination's start address and length.
struct WindowRecorder {
    size: usize,
    left: usize,
    windows: Arc<parking_lot::Mutex<Vec<(usize, usize)>>>,
}

impl Read for WindowRecorder {
    fn read(&mut self, destination: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            return Ok(0);
        }
        self.windows.lock().push((destination.as_ptr() as usize, destination.len()));
        let size = self.size.min(self.left).min(destination.len());
        destination[..size].fill(b'r');
        self.left -= size;
        Ok(size)
    }
}

/// With every view released before a refill reserves (the consumer drops each chunk, and the pump
/// waits at `before_refill` until it has), every refill reclaims the ring in place: no window
/// after a refill starts anywhere but the first ring's address, each refilled window is a whole
/// ring, the bytes arrive intact, and the meter charges one ring per live chunk and nothing after.
#[test]
fn a_freed_ring_is_reclaimed_in_place_and_charged_once() {
    let windows = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let read_size = 16 * 1024;
    let reads = 13;
    let reader =
        WindowRecorder { size: read_size, left: read_size * reads, windows: windows.clone() };
    let (tx, rx) = crossbeam_channel::unbounded();
    let (_cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let meter = Arc::new(QueuedOutputMeter::default());
    // The consumer reports each drop, and the pump waits for every chunk it sent to be dropped
    // before it reserves, so the reserve always finds the ring unshared.
    let (dropped_tx, dropped_rx) = crossbeam_channel::unbounded::<()>();
    let sent = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sent_by_pump = Arc::clone(&sent);
    let mut released = 0usize;
    let mut thread = spawn_reader_thread_with(
        Box::new(reader),
        tx,
        cancel_rx,
        meter.clone(),
        false,
        move || {
            sent_by_pump.fetch_add(1, Ordering::SeqCst);
        },
        move || {
            while released < sent.load(Ordering::SeqCst) {
                dropped_rx.recv().expect("the consumer reports each drop");
                released += 1;
            }
        },
    );
    let mut bytes = Vec::new();
    let mut charged = Vec::new();
    for chunk in rx.iter() {
        // The pump may already be filling the next view of this ring, so the charge is one ring
        // whatever the moment; it is never two.
        charged.push(meter.ring_bytes.load(Ordering::Relaxed));
        bytes.extend_from_slice(chunk.bytes());
        drop(chunk);
        let _ = dropped_tx.send(());
    }
    thread.handle.take().expect("reader handle").join().expect("reader thread");
    assert_eq!(meter.ring_bytes.load(Ordering::Relaxed), 0, "every ring is released at the end");
    assert_eq!(bytes, vec![b'r'; read_size * reads], "the bytes arrive intact");
    let windows = windows.lock().clone();
    let ring = windows[0].1;
    assert!(
        charged.iter().all(|total| *total == ring),
        "each live chunk charges one ring: {charged:?}"
    );
    let refills: Vec<usize> =
        (1..windows.len()).filter(|index| windows[*index].1 == ring).collect();
    assert!(refills.len() >= 2, "the reads crossed two refills: {windows:?}");
    for index in refills {
        assert_eq!(
            windows[index].0, windows[0].0,
            "refill {index} reclaimed the ring: {windows:?}"
        );
    }
}

/// With a view still held across a refill, the ring cannot be reclaimed: the refill takes a fresh
/// allocation, and while the old view lives both rings are charged.
#[test]
fn a_held_view_makes_the_refill_allocate_and_charge_a_second_ring() {
    let windows = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let read_size = 16 * 1024;
    let reader = WindowRecorder { size: read_size, left: read_size * 5, windows: windows.clone() };
    let (tx, rx) = crossbeam_channel::unbounded();
    let (_cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let meter = Arc::new(QueuedOutputMeter::default());
    let mut thread = spawn_reader_thread(Box::new(reader), tx, cancel_rx, meter.clone(), false);
    // Every chunk is kept, so the first ring still has live views when the pump refills.
    let held: Vec<PtyOutputChunk> = rx.iter().collect();
    thread.handle.take().expect("reader handle").join().expect("reader thread");
    let mut held = held;
    let windows = windows.lock().clone();
    let ring = windows[0].1;
    let refill = (1..windows.len()).find(|index| windows[*index].1 == ring).expect("a refill");
    assert_ne!(windows[refill].0, windows[0].0, "a held view forces a fresh allocation");
    let second_ring = windows[refill].1;
    assert_eq!(
        meter.ring_bytes.load(Ordering::Relaxed),
        ring + second_ring,
        "both rings are charged"
    );
    held.clear();
    assert_eq!(meter.ring_bytes.load(Ordering::Relaxed), 0, "every ring is released");
}
