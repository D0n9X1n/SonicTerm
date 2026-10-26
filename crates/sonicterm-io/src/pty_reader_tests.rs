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
