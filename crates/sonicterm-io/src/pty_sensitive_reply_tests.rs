use super::*;
use crate::pty::pty_test_support::*;

/// A writer that reports each write and waits for the test's permit before taking it.
struct GatedWriter {
    entered: Sender<()>,
    permit: Receiver<()>,
    written: Arc<Mutex<Vec<u8>>>,
}

impl Write for GatedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.entered.send(()).unwrap();
        self.permit.recv_timeout(Duration::from_secs(5)).unwrap();
        self.written.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A sensitive reply whose custody is a counted marker, so a test sees when the writer drops it.
fn reply(bytes: &[u8], marker: &Arc<()>) -> SensitiveReply {
    SensitiveReply::new(bytes.to_vec(), Box::new(Arc::clone(marker)))
}

/// Contract: the writer holds a sensitive reply's custody (its clipboard lease) until the native
/// write returns, and releases it after; the bytes reach the PTY unchanged and count as no input.
#[test]
fn lease_released_by_writer_after_write() {
    let (sender, rx) = diagnostic_input_channel();
    let (_replies, reader) = reply_spool();
    let sensitive = reader.sensitive_sender();
    let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
    let (permit_tx, permit_rx) = crossbeam_channel::bounded(1);
    let written = Arc::new(Mutex::new(Vec::new()));
    let (cancel_tx, cancel_rx) = crossbeam_channel::bounded(1);
    let mut writer = spawn_writer_thread(
        Box::new(GatedWriter { entered: entered_tx, permit: permit_rx, written: written.clone() }),
        rx,
        cancel_rx,
        sender.queued_bytes.clone(),
        sender.writer_progress.clone(),
        reader,
    );
    let marker = Arc::new(());
    let admitted =
        try_admit_sensitive(&sender.writer_progress, &sensitive, reply(b"secret", &marker));
    assert!(admitted.is_ok());
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    // The write is in progress, so the writer still holds the custody.
    assert_eq!(Arc::strong_count(&marker), 2);
    permit_tx.send(()).unwrap();
    observe_until(&sender, |diagnostics| diagnostics.completed_messages == 1);
    assert_eq!(Arc::strong_count(&marker), 1, "custody released after the write");
    assert_eq!(*written.lock(), b"secret");
    // A reply is not user input: no ordinal is admitted or completed for it.
    assert_eq!(sender.input_progress(), PtyInputProgress::Idle);
    cancel_tx.send(()).unwrap();
    writer.finish("gated sensitive writer");
}

/// Contract: admission never waits for the admission lock; a contended lock hands the reply back
/// whole, custody included, so the caller retries on a later turn.
#[test]
fn admission_contention_hands_the_reply_back() {
    let (sender, _rx) = diagnostic_input_channel();
    let (_replies, reader) = reply_spool();
    let marker = Arc::new(());
    let held = sender.writer_progress.admission.lock();
    let refused = try_admit_sensitive(
        &sender.writer_progress,
        &reader.sensitive_sender(),
        reply(b"x", &marker),
    );
    drop(held);
    let Err(SensitiveReplyError::Contended(returned)) = refused else {
        panic!("a contended admission is handed back");
    };
    assert_eq!(returned.len(), 1);
    assert_eq!(Arc::strong_count(&marker), 2, "the custody came back with the reply");
}

/// Contract: a reply is refused, never queued, once the writer stopped, when the one sensitive
/// slot is taken, or past the per-message cap; a queued reply's custody is released when the
/// writer's reply side drops.
#[test]
fn stopped_full_and_oversize_replies_are_refused() {
    let (sender, _rx) = diagnostic_input_channel();
    let (_replies, reader) = reply_spool();
    let sensitive = reader.sensitive_sender();
    let progress = &sender.writer_progress;
    let queued = Arc::new(());
    assert!(try_admit_sensitive(progress, &sensitive, reply(b"one", &queued)).is_ok());
    let marker = Arc::new(());
    let full = try_admit_sensitive(progress, &sensitive, reply(b"two", &marker));
    assert!(matches!(full, Err(SensitiveReplyError::Full(_))));
    let oversize = SensitiveReply::new(vec![0; MAX_PTY_INPUT_MESSAGE_BYTES + 1], Box::new(()));
    let refused = try_admit_sensitive(progress, &sensitive, oversize);
    assert!(matches!(refused, Err(SensitiveReplyError::TooLarge(_))));
    progress.record_terminal(TERMINAL_STOPPED);
    let stopped = try_admit_sensitive(progress, &sensitive, reply(b"three", &marker));
    assert!(matches!(stopped, Err(SensitiveReplyError::WriterStopped(_))));
    drop(full);
    drop(stopped);
    assert_eq!(Arc::strong_count(&queued), 2, "the queued reply still holds its custody");
    drop(reader);
    assert_eq!(Arc::strong_count(&queued), 1, "dropping the reply side releases queued custody");
}

/// Contract: a sensitive reply never marks the shell-input flag the parser's shell-token latch
/// reads, as the ordinary reply spool does not.
#[test]
fn input_seen_unchanged() {
    let seen = Arc::new(AtomicBool::new(false));
    let progress = PtyWriterProgress::with_input_seen(Some(seen.clone()));
    let (_replies, reader) = reply_spool();
    let admitted =
        try_admit_sensitive(&progress, &reader.sensitive_sender(), reply(b"x", &Arc::new(())));
    assert!(admitted.is_ok());
    assert!(!seen.load(Ordering::SeqCst));
}

/// Contract: every spawned PTY handle takes a distinct, nonzero incarnation, so a clipboard read
/// bound to one PTY never answers a later one in the same pane.
#[test]
fn incarnations_are_distinct_and_nonzero() {
    let first = next_pty_incarnation();
    let second = next_pty_incarnation();
    assert!(first > 0 && second > first);
}
