use super::*;

// Ordering: queued_bytes, in_flight_bytes, completed_messages, operation Relaxed; closing SeqCst;
// writer_on, input_completed Release.
pub(super) fn spawn_writer_thread(
    mut writer: Box<dyn Write + Send>,
    rx: Receiver<Outgoing>,
    cancel: Receiver<()>,
    queued_bytes: Arc<AtomicUsize>,
    progress: Arc<PtyWriterProgress>,
    replies: ReplyReader,
) -> PtyIoThread {
    let (done_tx, done) = crossbeam_channel::bounded(1);
    let handle = thread::Builder::new()
        .name("sonic-pty-writer".into())
        .spawn(move || {
            let _active = ActivePtyIoThread::enter();
            let mut last_was_reply = false;
            let mut reply_wake = replies.wake.clone();
            // Cancellation, closing and a disconnected input channel end the writer on purpose.
            let mut exit = TERMINAL_STOPPED;
            loop {
                let cancelled =
                    !matches!(cancel.try_recv(), Err(crossbeam_channel::TryRecvError::Empty));
                if cancelled {
                    // When: cancelled is true, stop before beginning another native write.
                    break;
                }
                // A ready UI message gets the next turn after each reply chunk.
                // Otherwise select fairly between UI input and the reply wake hint.
                let ui_turn = last_was_reply.then(|| rx.try_recv().ok()).flatten();
                let (bytes, is_reply) = if let Some(bytes) = ui_turn {
                    (bytes, false)
                } else {
                    // When: ui_turn is absent, park until cancellation or either input source becomes ready.
                    crossbeam_channel::select! {
                        recv(cancel) -> _ => break,
                        recv(rx) -> result => match result {
                            Ok(bytes) => (bytes, false),
                            Err(_) => break,
                        },
                        recv(reply_wake) -> _ => match replies.pop() {
                            Ok(Some(bytes)) => (bytes, true),
                            Ok(None) => continue,
                            Err(error) => {
                                if !progress.closing.load(Ordering::SeqCst) {
                                    tracing::warn!(%error, "PTY reply spool read failed");
                                }
                                reply_wake = crossbeam_channel::never();
                                continue;
                            },
                        },
                    }
                };
                last_was_reply = is_reply;
                if !is_reply {
                    // Only UI admissions charged queued_bytes; replies own separate storage.
                    queued_bytes.fetch_sub(bytes.len(), Ordering::Relaxed);
                }
                progress.in_flight_bytes.store(bytes.len(), Ordering::Relaxed);
                let on = if is_reply { WRITER_ON_REPLY } else { WRITER_ON_INPUT };
                progress.writer_on.store(on, Ordering::Release);
                progress.begin(PtyWriterPhase::Writing);
                if let Err(e) = writer.write_all(&bytes) {
                    // When: `write_all` fails, report it unless cancellation intentionally interrupted I/O.
                    let intentional =
                        cancel.try_recv().is_ok() || progress.closing.load(Ordering::SeqCst);
                    if !intentional {
                        tracing::warn!("pty write error: {e}");
                        exit = TERMINAL_FAILED_WRITE;
                    }
                    replies.fail(e);
                    break;
                }
                progress.begin(PtyWriterPhase::Flushing);
                if let Err(error) = writer.flush() {
                    // When: flush fails, preserve the actual native failure rather than reporting a completed write.
                    let intentional =
                        cancel.try_recv().is_ok() || progress.closing.load(Ordering::SeqCst);
                    if !intentional {
                        tracing::warn!(%error, "pty flush error");
                        exit = TERMINAL_FAILED_FLUSH;
                    }
                    replies.fail(error);
                    break;
                }
                if !is_reply {
                    // Only a flushed user-input message completes an admitted ordinal.
                    progress.input_completed.fetch_add(1, Ordering::Release);
                }
                progress.writer_on.store(WRITER_ON_IDLE, Ordering::Release);
                progress.completed_messages.fetch_add(1, Ordering::Relaxed);
                progress.in_flight_bytes.store(0, Ordering::Relaxed);
                progress.operation.store(0, Ordering::Relaxed);
            }
            // Recorded before the queue drops, so no admission succeeds unrecorded after this point.
            progress.record_terminal(exit);
            // Reader lifetime, not surviving producer clones, owns pending spill storage.
            drop(replies);
            progress.in_flight_bytes.store(0, Ordering::Relaxed);
            progress.operation.store(3, Ordering::Relaxed);
            // Nothing will drain the queue again. Anything still in it is
            // released when the channel drops, so the figure must not keep
            // reporting it as held.
            queued_bytes.store(0, Ordering::Relaxed);
            let _ = done_tx.send(());
        })
        // PANIC: see `spawn_reader_thread` rationale above — OS-level
        // thread-spawn failure at PTY init is unrecoverable.
        .expect("spawn pty writer");
    PtyIoThread { handle: Some(handle), done, failed: false }
}

#[cfg(test)]
#[path = "pty_writer_tests.rs"]
mod pty_writer_tests;
