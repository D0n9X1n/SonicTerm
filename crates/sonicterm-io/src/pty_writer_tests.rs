use super::*;
use crate::pty::pty_test_support::*;

struct ControlledInputWriter {
    entered: Sender<PtyWriterPhase>,
    permit: Receiver<bool>,
    written: Arc<Mutex<Vec<Vec<u8>>>>,
    block_flush: bool,
}

impl Write for ControlledInputWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if !self.block_flush {
            self.entered.send(PtyWriterPhase::Writing).unwrap();
            if !self.permit.recv_timeout(Duration::from_secs(2)).unwrap_or(false) {
                return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
            }
        }
        self.written.lock().push(bytes.to_vec());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.block_flush {
            self.entered.send(PtyWriterPhase::Flushing).unwrap();
            if !self.permit.recv_timeout(Duration::from_secs(2)).unwrap_or(false) {
                return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
            }
        }
        Ok(())
    }
}

#[test]
fn healthy_writer_drains_a_prequeued_small_message_burst_in_order() {
    // A burst fills four slots before scheduling even when the eventual writer is completely healthy.
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    let (sender, rx) = diagnostic_input_channel();
    let messages = [
        b"key".to_vec(),
        b"\x1b[<0;12;8M".to_vec(),
        b"\x1b[<64;12;8M".to_vec(),
        b"\x1b[<0;12;8m".to_vec(),
    ];
    for bytes in &messages {
        sender.send(bytes.clone()).unwrap();
    }
    let rejected = b"\x1b[<35;12;8M".to_vec();
    assert!(
        matches!(sender.send(rejected.clone()), Err(PtyInputError::QueueFull(bytes)) if bytes == rejected)
    );
    let burst = sender.diagnostics();
    assert_eq!(burst.queued_messages, PTY_INPUT_QUEUE_CAPACITY);
    assert_eq!(burst.queued_bytes, messages.iter().map(Vec::len).sum());
    assert_eq!(burst.writer_phase, PtyWriterPhase::Idle);
    assert_eq!(burst.in_flight_bytes, 0);
    assert_eq!(burst.completed_messages, 0);

    let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
    let (permit_tx, permit_rx) = crossbeam_channel::bounded(1);
    let written = Arc::new(Mutex::new(Vec::new()));
    let (cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let mut writer = spawn_writer_thread(
        Box::new(ControlledInputWriter {
            entered: entered_tx,
            permit: permit_rx,
            written: written.clone(),
            block_flush: false,
        }),
        rx,
        cancel_rx,
        sender.queued_bytes.clone(),
        sender.writer_progress.clone(),
        reply_spool().1,
    );
    for _ in &messages {
        assert_eq!(
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            PtyWriterPhase::Writing
        );
        permit_tx.send(true).unwrap();
    }
    let drained = observe_until(&sender, |d| {
        d.completed_messages == 4 && d.writer_phase == PtyWriterPhase::Idle
    });
    assert_eq!(drained.queued_messages, 0);
    assert_eq!(drained.queued_bytes, 0);
    assert_eq!(drained.in_flight_bytes, 0);
    assert_eq!(*written.lock(), messages);
    cancel_tx.send(()).unwrap();
    writer.finish("controlled healthy input writer");
    assert_eq!(sender.diagnostics().writer_phase, PtyWriterPhase::Stopped);
}

#[test]
fn slow_writer_recovers_without_replaying_rejected_input() {
    // Releasing a slow writer drains accepted keys, button edges, and wheel reports in FIFO order without replay.
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    let (sender, rx) = diagnostic_input_channel();
    let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
    let (permit_tx, permit_rx) = crossbeam_channel::bounded(1);
    let written = Arc::new(Mutex::new(Vec::new()));
    let (cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let mut writer = spawn_writer_thread(
        Box::new(ControlledInputWriter {
            entered: entered_tx,
            permit: permit_rx,
            written: written.clone(),
            block_flush: false,
        }),
        rx,
        cancel_rx,
        sender.queued_bytes.clone(),
        sender.writer_progress.clone(),
        reply_spool().1,
    );
    let first = b"key".to_vec();
    sender.send(first.clone()).unwrap();
    assert_eq!(entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(), PtyWriterPhase::Writing);
    let waiting = [
        b"\x1b[<0;12;8M".to_vec(),
        b"\x1b[<64;12;8M".to_vec(),
        b"\x1b[<64;12;8M".to_vec(),
        b"\x1b[<0;12;8m".to_vec(),
    ];
    for bytes in &waiting {
        sender.send(bytes.clone()).unwrap();
    }
    assert!(matches!(sender.send(b"rejected".to_vec()), Err(PtyInputError::QueueFull(_))));
    let full = sender.diagnostics();
    assert_eq!(full.writer_phase, PtyWriterPhase::Writing);
    assert_eq!(full.completed_messages, 0);
    assert_eq!(full.in_flight_bytes, first.len());
    assert_eq!(full.queued_messages, PTY_INPUT_QUEUE_CAPACITY);
    permit_tx.send(true).unwrap();
    for completed in 1..=waiting.len() {
        assert_eq!(
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            PtyWriterPhase::Writing
        );
        assert_eq!(sender.diagnostics().completed_messages, completed as u64);
        permit_tx.send(true).unwrap();
    }
    let drained = observe_until(&sender, |d| {
        d.completed_messages == 5 && d.writer_phase == PtyWriterPhase::Idle
    });
    assert_eq!(drained.queued_messages, 0);
    assert_eq!(drained.queued_bytes, 0);
    assert_eq!(drained.in_flight_bytes, 0);
    let expected = std::iter::once(first).chain(waiting).collect::<Vec<_>>();
    assert_eq!(*written.lock(), expected);
    cancel_tx.send(()).unwrap();
    writer.finish("controlled slow input writer");
    assert_eq!(sender.diagnostics().writer_phase, PtyWriterPhase::Stopped);
}

#[test]
fn stalled_native_writer_is_distinct_from_queue_pressure_and_cancels_boundedly() {
    // A held native write is observable with zero queued bytes, then four later messages saturate the channel.
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    let (sender, rx) = diagnostic_input_channel();
    let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
    let (permit_tx, permit_rx) = crossbeam_channel::bounded(1);
    let written = Arc::new(Mutex::new(Vec::new()));
    let (cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let mut writer = spawn_writer_thread(
        Box::new(ControlledInputWriter {
            entered: entered_tx,
            permit: permit_rx,
            written: written.clone(),
            block_flush: false,
        }),
        rx,
        cancel_rx,
        sender.queued_bytes.clone(),
        sender.writer_progress.clone(),
        reply_spool().1,
    );
    sender.send(vec![b'a'; 12]).unwrap();
    assert_eq!(entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(), PtyWriterPhase::Writing);
    let stalled = sender.diagnostics();
    assert_eq!(stalled.writer_phase, PtyWriterPhase::Writing);
    assert_eq!(stalled.queued_bytes, 0);
    assert_eq!(stalled.queued_messages, 0);
    assert_eq!(stalled.in_flight_bytes, 12);
    assert_eq!(stalled.completed_messages, 0);
    assert!(stalled.in_flight_millis.is_some());
    for len in [11, 12, 13, 24] {
        sender.send(vec![b'b'; len]).unwrap();
    }
    let full = sender.diagnostics();
    let began = Instant::now();
    for _ in 0..1000 {
        assert!(matches!(sender.send(vec![b'c'; 12]), Err(PtyInputError::QueueFull(_))));
    }
    assert!(
        began.elapsed() < Duration::from_secs(1),
        "refused input must not wait for the native writer"
    );
    assert_eq!(full.queued_messages, 4);
    assert_eq!(full.queued_bytes, 60);
    assert_eq!(sender.diagnostics().queued_bytes, 60);
    assert_eq!(sender.diagnostics().in_flight_bytes, 12);
    assert_eq!(sender.diagnostics().completed_messages, 0);
    cancel_tx.send(()).unwrap();
    permit_tx.send(false).unwrap();
    writer.finish("controlled stalled input writer");
    assert_eq!(sender.diagnostics().writer_phase, PtyWriterPhase::Stopped);
    assert_eq!(sender.diagnostics().in_flight_bytes, 0);
    assert_eq!(sender.diagnostics().queued_bytes, 0);
    assert!(written.lock().is_empty());
    assert!(matches!(sender.send(vec![b'd']), Err(PtyInputError::WriterDisconnected(_))));
}

#[test]
fn blocked_flush_remains_in_flight_until_the_native_call_returns() {
    // A flush can stall after write_all succeeded; queued bytes alone must not label the writer idle.
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    let (sender, rx) = diagnostic_input_channel();
    let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
    let (permit_tx, permit_rx) = crossbeam_channel::bounded(1);
    let written = Arc::new(Mutex::new(Vec::new()));
    let (cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let mut writer = spawn_writer_thread(
        Box::new(ControlledInputWriter {
            entered: entered_tx,
            permit: permit_rx,
            written: written.clone(),
            block_flush: true,
        }),
        rx,
        cancel_rx,
        sender.queued_bytes.clone(),
        sender.writer_progress.clone(),
        reply_spool().1,
    );
    sender.send(vec![b'x'; 13]).unwrap();
    assert_eq!(entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(), PtyWriterPhase::Flushing);
    let flushing = sender.diagnostics();
    assert_eq!(flushing.writer_phase, PtyWriterPhase::Flushing);
    assert_eq!(flushing.queued_bytes, 0);
    assert_eq!(flushing.in_flight_bytes, 13);
    assert_eq!(flushing.completed_messages, 0);
    assert!(flushing.in_flight_millis.is_some());
    let waiting = [b"key".to_vec(), b"button".to_vec(), b"wheel".to_vec(), b"release".to_vec()];
    for bytes in &waiting {
        sender.send(bytes.clone()).unwrap();
    }
    assert!(matches!(sender.send(vec![b'y']), Err(PtyInputError::QueueFull(_))));
    let saturated = sender.diagnostics();
    assert_eq!(saturated.writer_phase, PtyWriterPhase::Flushing);
    assert_eq!(saturated.queued_messages, PTY_INPUT_QUEUE_CAPACITY);
    assert_eq!(saturated.queued_bytes, waiting.iter().map(Vec::len).sum());
    permit_tx.send(true).unwrap();
    for _ in &waiting {
        assert_eq!(
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            PtyWriterPhase::Flushing
        );
        permit_tx.send(true).unwrap();
    }
    let drained = observe_until(&sender, |d| {
        d.completed_messages == 5 && d.writer_phase == PtyWriterPhase::Idle
    });
    assert_eq!(drained.queued_bytes, 0);
    assert_eq!(drained.queued_messages, 0);
    assert_eq!(drained.in_flight_bytes, 0);
    let expected = std::iter::once(vec![b'x'; 13]).chain(waiting).collect::<Vec<_>>();
    assert_eq!(*written.lock(), expected);
    cancel_tx.send(()).unwrap();
    writer.finish("controlled flushing input writer");
    assert_eq!(sender.diagnostics().writer_phase, PtyWriterPhase::Stopped);
}

#[test]
fn reply_spill_and_ui_input_keep_independent_fifo_with_fair_turns() {
    // Hold native I/O while both queues fill; replies must admit without consuming UI slots or byte charges.
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    let (sender, rx) = diagnostic_input_channel();
    let (replies, reply_reader) = reply_spool();
    let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
    let (permit_tx, permit_rx) = crossbeam_channel::bounded(1);
    let written = Arc::new(Mutex::new(Vec::new()));
    let (cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let mut writer = spawn_writer_thread(
        Box::new(ControlledInputWriter {
            entered: entered_tx,
            permit: permit_rx,
            written: written.clone(),
            block_flush: false,
        }),
        rx,
        cancel_rx,
        sender.queued_bytes.clone(),
        sender.writer_progress.clone(),
        reply_reader,
    );
    sender.send(vec![0]).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    for marker in 1..=PTY_INPUT_QUEUE_CAPACITY {
        sender.send(vec![marker as u8]).unwrap();
    }
    for marker in 10..16 {
        replies.send(vec![marker; 32 * 1024]).unwrap();
    }
    assert_eq!(sender.diagnostics().queued_bytes, PTY_INPUT_QUEUE_CAPACITY);
    assert_eq!(sender.diagnostics().queued_messages, PTY_INPUT_QUEUE_CAPACITY);
    permit_tx.send(true).unwrap();
    for _ in 0..10 {
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        permit_tx.send(true).unwrap();
    }
    observe_until(&sender, |d| {
        d.completed_messages == 11 && d.writer_phase == PtyWriterPhase::Idle
    });
    let writes = written.lock();
    let ui: Vec<_> = writes.iter().filter(|bytes| bytes.len() == 1).map(|bytes| bytes[0]).collect();
    assert_eq!(ui, vec![0, 1, 2, 3, 4]);
    let reply: Vec<_> =
        writes.iter().filter(|bytes| bytes.len() > 1).map(|bytes| bytes[0]).collect();
    assert_eq!(reply, vec![10, 11, 12, 13, 14, 15]);
    for (index, pair) in writes.windows(2).enumerate() {
        if pair[0].len() > 1 && pair[1].len() > 1 {
            let previous_ui = writes[..index].iter().filter(|bytes| bytes.len() == 1).count();
            assert_eq!(previous_ui, 5, "ready UI input must get the turn after a reply chunk");
        }
    }
    drop(writes);
    assert_eq!(sender.diagnostics().queued_bytes, 0);
    cancel_tx.send(()).unwrap();
    writer.finish("reply and UI writer");
    assert_eq!(replies.send(vec![1]).unwrap_err().kind(), std::io::ErrorKind::BrokenPipe);
}

#[test]
fn native_writer_failure_closes_reply_spill_with_live_sender() {
    // A native failure must discard pending spill and remain observable, not accept work into an abandoned spool.
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    let (sender, rx) = diagnostic_input_channel();
    let (replies, reply_reader) = reply_spool();
    let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
    let (permit_tx, permit_rx) = crossbeam_channel::bounded(1);
    let (_cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let mut writer = spawn_writer_thread(
        Box::new(ControlledInputWriter {
            entered: entered_tx,
            permit: permit_rx,
            written: Arc::new(Mutex::new(Vec::new())),
            block_flush: false,
        }),
        rx,
        cancel_rx,
        sender.queued_bytes.clone(),
        sender.writer_progress.clone(),
        reply_reader,
    );
    sender.send(vec![0]).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    for _ in 0..4 {
        replies.send(vec![b'r'; 32 * 1024]).unwrap();
    }
    permit_tx.send(false).unwrap();
    writer.finish("failed reply writer");
    assert_eq!(sender.diagnostics().writer_phase, PtyWriterPhase::Stopped);
    assert_eq!(replies.send(vec![1]).unwrap_err().kind(), std::io::ErrorKind::BrokenPipe);
    assert!(!sender.is_closing());
}

#[test]
fn reply_storage_failure_keeps_native_ui_input_available() {
    // A failed spool must not prevent keyboard input from interrupting the child.
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    let (sender, rx) = diagnostic_input_channel();
    let (replies, reader) = reply_spool();
    reader.fail(std::io::Error::new(std::io::ErrorKind::StorageFull, "injected full disk"));
    assert!(replies.send(vec![1]).is_err());
    let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
    let (permit_tx, permit_rx) = crossbeam_channel::bounded(1);
    let (cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let written = Arc::new(Mutex::new(Vec::new()));
    let mut writer = spawn_writer_thread(
        Box::new(ControlledInputWriter {
            entered: entered_tx,
            permit: permit_rx,
            written: written.clone(),
            block_flush: false,
        }),
        rx,
        cancel_rx,
        sender.queued_bytes.clone(),
        sender.writer_progress.clone(),
        reader,
    );
    sender.send(vec![3]).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    permit_tx.send(true).unwrap();
    observe_until(&sender, |d| d.completed_messages == 1);
    assert_eq!(*written.lock(), [vec![3]]);
    cancel_tx.send(()).unwrap();
    writer.finish("spool-failure UI writer");
}

#[test]
fn flush_failure_remains_observable_without_counting_completion() {
    // A successful write followed by a failed flush must latch the native error, not a completed message.
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    let (sender, rx) = diagnostic_input_channel();
    let (replies, reader) = reply_spool();
    let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
    let (permit_tx, permit_rx) = crossbeam_channel::bounded(1);
    let (_cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let mut writer = spawn_writer_thread(
        Box::new(ControlledInputWriter {
            entered: entered_tx,
            permit: permit_rx,
            written: Arc::new(Mutex::new(Vec::new())),
            block_flush: true,
        }),
        rx,
        cancel_rx,
        sender.queued_bytes.clone(),
        sender.writer_progress.clone(),
        reader,
    );
    sender.send(vec![1]).unwrap();
    assert_eq!(entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(), PtyWriterPhase::Flushing);
    permit_tx.send(false).unwrap();
    writer.finish("failed-flush writer");
    assert_eq!(sender.diagnostics().completed_messages, 0);
    assert!(replies.send(vec![2]).is_err());
    assert!(!sender.is_closing());
}

#[test]
fn diagnostics_do_not_weaken_the_per_message_limit() {
    // Oversized refusals never charge the queue or advance writer progress, and retain the original bytes at the IO boundary.
    let (sender, _rx) = diagnostic_input_channel();
    let before = sender.diagnostics();
    let bytes = vec![b'x'; MAX_PTY_INPUT_MESSAGE_BYTES + 1];
    assert!(
        matches!(sender.send(bytes), Err(PtyInputError::MessageTooLarge(bytes)) if bytes.len() == MAX_PTY_INPUT_MESSAGE_BYTES + 1)
    );
    assert_eq!(sender.diagnostics(), before);
}

/// A writer whose native write fails.
struct FailingWriter;

impl Write for FailingWriter {
    fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("injected write failure (test-only)"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A native write failure with three writes admitted reports all three lost, and the next
/// admission is refused as disconnected without changing that; a deliberate stop is not a failure.
#[test]
fn failed_writer_reports_every_admitted_write_lost() {
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    let (sender, rx) = diagnostic_input_channel();
    for byte in *b"abc" {
        sender.send(vec![byte]).unwrap();
    }
    let (_cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let mut writer = spawn_writer_thread(
        Box::new(FailingWriter),
        rx,
        cancel_rx,
        sender.queued_bytes.clone(),
        sender.writer_progress.clone(),
        reply_spool().1,
    );
    writer.finish("failing input writer");
    let lost = PtyInputProgress::Terminal { failed: true, lost: 3 };
    assert_eq!(sender.input_progress(), lost);
    assert!(matches!(sender.send(vec![b'd']), Err(PtyInputError::WriterDisconnected(_))));
    assert_eq!(sender.input_progress(), lost, "a refused admission changes nothing");

    let stopped = PtyWriterProgress::new();
    stopped.record_terminal(TERMINAL_STOPPED);
    assert_eq!(stopped.input_progress(), PtyInputProgress::Terminal { failed: false, lost: 0 });
}

/// Wait up to two seconds for `sender`'s input progress to become `expected`.
fn wait_for_input_progress(sender: &PtyInputSender, expected: PtyInputProgress) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while sender.input_progress() != expected {
        assert!(Instant::now() < deadline, "input progress never became {expected:?}");
        std::thread::yield_now();
    }
}

/// A writer that blocks on a parser reply reports the user input queued behind it as the oldest
/// outstanding write, with that input's own admission time and `on_reply`; the reply never takes
/// an ordinal, so it never advances the count.
#[test]
fn a_reply_blocked_ahead_of_input_reports_that_inputs_age() {
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    let (sender, rx) = diagnostic_input_channel();
    let (replies, reply_reader) = reply_spool();
    let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
    let (permit_tx, permit_rx) = crossbeam_channel::bounded(1);
    let (cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let mut writer = spawn_writer_thread(
        Box::new(ControlledInputWriter {
            entered: entered_tx,
            permit: permit_rx,
            written: Arc::new(Mutex::new(Vec::new())),
            block_flush: false,
        }),
        rx,
        cancel_rx,
        sender.queued_bytes.clone(),
        sender.writer_progress.clone(),
        reply_reader,
    );
    replies.send(vec![9, 9]).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(sender.input_progress(), PtyInputProgress::Idle, "a reply is not user input");
    let before = Instant::now();
    sender.send(vec![1]).unwrap();
    let PtyInputProgress::Oldest { ordinal, admitted_at, on_reply } = sender.input_progress()
    else {
        panic!("the input write is outstanding behind the reply");
    };
    assert_eq!((ordinal, on_reply), (1, true));
    assert!(admitted_at + Duration::from_millis(2) >= before, "the input's own admission time");
    permit_tx.send(true).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(
        sender.input_progress(),
        PtyInputProgress::Oldest { ordinal: 1, on_reply: false, .. }
    ));
    permit_tx.send(true).unwrap();
    wait_for_input_progress(&sender, PtyInputProgress::Idle);
    assert_eq!(sender.writer_progress.input_admitted.load(Ordering::Acquire), 1);
    assert_eq!(sender.writer_progress.input_completed.load(Ordering::Acquire), 1);
    cancel_tx.send(()).unwrap();
    writer.finish("reply ahead of input");
}

/// A native `flush` error is recorded as a flush failure, and every write admitted after the last
/// completion, including one still queued, is reported lost.
#[test]
fn a_flush_failure_reports_failed_flush_and_its_lost_writes() {
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    let (sender, rx) = diagnostic_input_channel();
    let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
    let (permit_tx, permit_rx) = crossbeam_channel::bounded(1);
    let (_cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let mut writer = spawn_writer_thread(
        Box::new(ControlledInputWriter {
            entered: entered_tx,
            permit: permit_rx,
            written: Arc::new(Mutex::new(Vec::new())),
            block_flush: true,
        }),
        rx,
        cancel_rx,
        sender.queued_bytes.clone(),
        sender.writer_progress.clone(),
        reply_spool().1,
    );
    sender.send(vec![1]).unwrap();
    assert_eq!(entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(), PtyWriterPhase::Flushing);
    sender.send(vec![2]).unwrap();
    permit_tx.send(false).unwrap();
    writer.finish("failing flush");
    assert_eq!(sender.writer_progress.terminal.load(Ordering::Acquire), TERMINAL_FAILED_FLUSH);
    assert_eq!(sender.input_progress(), PtyInputProgress::Terminal { failed: true, lost: 2 });
}

/// Cancellation that interrupts a write is a deliberate stop, not a failure, even though the
/// interrupted write never completed.
#[test]
fn cancellation_is_a_stop_not_a_failure() {
    #[cfg(windows)]
    let _live_pty_guard = lock_live_pty_test();
    let (sender, rx) = diagnostic_input_channel();
    let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
    let (permit_tx, permit_rx) = crossbeam_channel::bounded(1);
    let (cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let mut writer = spawn_writer_thread(
        Box::new(ControlledInputWriter {
            entered: entered_tx,
            permit: permit_rx,
            written: Arc::new(Mutex::new(Vec::new())),
            block_flush: false,
        }),
        rx,
        cancel_rx,
        sender.queued_bytes.clone(),
        sender.writer_progress.clone(),
        reply_spool().1,
    );
    sender.send(vec![1]).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    cancel_tx.send(()).unwrap();
    permit_tx.send(false).unwrap();
    writer.finish("cancelled writer");
    assert_eq!(sender.writer_progress.terminal.load(Ordering::Acquire), TERMINAL_STOPPED);
    assert_eq!(sender.input_progress(), PtyInputProgress::Terminal { failed: false, lost: 1 });
}
