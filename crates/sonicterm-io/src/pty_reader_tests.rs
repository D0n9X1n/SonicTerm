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
    let mut thread =
        spawn_reader_thread_with(Box::new(reader), tx, cancel_rx, meter, stamp_reads, after_read);
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
