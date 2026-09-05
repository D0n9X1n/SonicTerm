use super::*;
use crate::proto::{ClientMsg, ServerMsg};
use crossbeam_channel::bounded;
use std::io::{Cursor, Read, Write};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

struct BlockingWriteStream {
    read: Cursor<Vec<u8>>,
    block_first_write: bool,
    write_started: crossbeam_channel::Sender<()>,
    release_write: crossbeam_channel::Receiver<()>,
    dropped: Option<Arc<AtomicBool>>,
}

impl Drop for BlockingWriteStream {
    fn drop(&mut self) {
        if let Some(dropped) = &self.dropped {
            dropped.store(true, Ordering::Release);
        }
    }
}

impl Read for BlockingWriteStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.read.read(buffer)
    }
}

impl Write for BlockingWriteStream {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if self.block_first_write {
            self.block_first_write = false;
            let _ = self.write_started.try_send(());
            self.release_write.recv().map_err(std::io::Error::other)?;
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Cross-platform: a fresh server has no sessions, and control operations on
/// unknown panes/sessions surface an error instead of panicking. Guards the
/// `find_pane` / lookup paths without spawning a real PTY.
#[test]
fn unknown_pane_and_session_ops_error() {
    let state = ServerState::new();
    assert_eq!(state.session_count(), 0);
    assert!(state.input(999, b"x".to_vec()).is_err(), "input on unknown pane must Err");
    assert!(state.resize(999, 80, 24).is_err(), "resize on unknown pane must Err");

    let (tx, rx) = bounded(CHANNEL_CAP);
    let sink = SubscriberSink::new(tx, rx);
    assert!(state.attach(12345, sink).is_err(), "attach to unknown session must Err");
}

/// Cross-platform: the bounded subscriber mailbox never evicts control
/// responses to make room for later messages.
#[test]
fn subscriber_sink_preserves_queued_control_when_full() {
    let (tx, rx) = bounded(1);
    let sink = SubscriberSink::new(tx, rx.clone());

    sink.send_control(ServerMsg::Exit { pane_id: 1 }).unwrap();
    assert!(sink.send_control(ServerMsg::Exit { pane_id: 2 }).is_err());

    match rx.try_recv() {
        Ok(ServerMsg::Exit { pane_id }) => assert_eq!(pane_id, 1),
        other => panic!("expected the original Exit, got {other:?}"),
    }
    assert!(rx.try_recv().is_err(), "only one message should remain");
}

/// Saturation emits one gap marker, suppresses later bytes, and resumes only after replay.
#[test]
fn subscriber_gap_requires_snapshot_before_live_output_resumes() {
    for split in [vec![0xe2], b"\x1b[".to_vec(), b"\x1b]0;title".to_vec()] {
        let (tx, rx) = bounded(3);
        let subscriber = Mutex::new(Some(SubscriberSink::new(tx, rx.clone())));
        let replay = Mutex::new(VecDeque::new());

        forward_pane_output(b"prefix".as_slice(), &replay, &subscriber, 1);
        forward_pane_output(&split, &replay, &subscriber, 1);
        forward_pane_output(b"post-gap".as_slice(), &replay, &subscriber, 1);

        assert!(matches!(
            rx.recv(),
            Ok(ServerMsg::Output { pane_id: 1, bytes }) if bytes == b"prefix"
        ));
        assert!(matches!(rx.recv(), Ok(ServerMsg::ResyncRequired { pane_id: 1 })));
        assert!(rx.try_recv().is_err(), "post-gap output must stay suppressed");

        let requester = subscriber.lock().as_ref().unwrap().clone();
        send_replay_snapshot(&replay, &subscriber, 1, &requester).unwrap();
        assert!(matches!(
            rx.recv(),
            Ok(ServerMsg::ReplaySnapshot {
                pane_id: 1,
                start: true,
                complete: true,
                bytes,
            }) if bytes == [b"prefix".as_slice(), split.as_slice(), b"post-gap"].concat()
        ));

        forward_pane_output(b"live".as_slice(), &replay, &subscriber, 1);
        assert!(matches!(
            rx.recv(),
            Ok(ServerMsg::Output { pane_id: 1, bytes }) if bytes == b"live"
        ));
    }
}

/// Replay payloads obey the same per-message ceiling as live output.
#[test]
fn replay_snapshot_payload_respects_message_frame_ceiling() {
    const TEST_CAPACITY: usize = 4;
    let (tx, rx) = bounded(TEST_CAPACITY);
    let sink = SubscriberSink::new(tx, rx.clone());

    for _ in 0..=TEST_CAPACITY {
        sink.pause_for_replay(1);
        if sink.send_snapshot(1, vec![b'x'; REPLAY_CAP]).is_err() {
            break;
        }
    }

    let queued = rx.try_iter().collect::<Vec<_>>();
    let payload_bytes = queued
        .iter()
        .map(|message| match message {
            ServerMsg::ReplaySnapshot { bytes, .. } => {
                assert!(bytes.len() <= SUBSCRIBER_OUTPUT_FRAME_BYTES);
                bytes.len()
            }
            _ => 0,
        })
        .sum::<usize>();
    assert!(
        payload_bytes <= TEST_CAPACITY * SUBSCRIBER_OUTPUT_FRAME_BYTES,
        "queued replay payload {payload_bytes} exceeded the mailbox ceiling"
    );
}

/// A multi-fragment replay keeps live delivery paused until its completion fragment queues.
#[test]
fn replay_pause_survives_every_nonfinal_snapshot_fragment() {
    let (tx, rx) = bounded(1);
    let sink = SubscriberSink::new(tx, rx.clone());
    sink.pause_for_replay(1);
    let sender = sink.clone();
    let (done_tx, done_rx) = bounded(1);
    let snapshot = vec![b'x'; SUBSCRIBER_OUTPUT_FRAME_BYTES + 1];
    let send_thread = std::thread::spawn(move || {
        done_tx.send(sender.send_snapshot(1, snapshot)).unwrap();
    });

    let deadline = Instant::now() + Duration::from_secs(1);
    while rx.is_empty() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(rx.len(), 1, "the first replay fragment must queue");
    let observation_deadline = Instant::now() + Duration::from_millis(25);
    while Instant::now() < observation_deadline {
        assert!(
            sink.shared.replay_pauses.lock().contains_key(&1),
            "the replay pause must survive until the completion fragment queues"
        );
        std::thread::yield_now();
    }
    assert!(done_rx.try_recv().is_err(), "the completion fragment must still be pending");

    assert!(matches!(
        rx.recv(),
        Ok(ServerMsg::ReplaySnapshot {
            pane_id: 1,
            start: true,
            complete: false,
            bytes,
        }) if bytes.len() == SUBSCRIBER_OUTPUT_FRAME_BYTES
    ));
    done_rx.recv_timeout(Duration::from_secs(1)).expect("snapshot sender finished").unwrap();
    send_thread.join().unwrap();
    assert!(matches!(
        rx.recv(),
        Ok(ServerMsg::ReplaySnapshot {
            pane_id: 1,
            start: false,
            complete: true,
            bytes,
        }) if bytes == vec![b'x']
    ));
    assert!(!sink.shared.replay_pauses.lock().contains_key(&1));
}

/// Full-ring replay fragments reconstruct one atomic snapshot with one completion marker.
#[test]
fn replay_snapshot_fragments_reconstruct_the_bounded_ring() {
    let snapshot = (0..REPLAY_CAP).map(|index| index as u8).collect::<Vec<_>>();
    let fragment_count = REPLAY_CAP.div_ceil(SUBSCRIBER_OUTPUT_FRAME_BYTES);
    let (tx, rx) = bounded(fragment_count);
    let sink = SubscriberSink::new(tx, rx.clone());
    sink.pause_for_replay(1);

    sink.send_snapshot(1, snapshot.clone()).unwrap();

    let fragments = rx.try_iter().collect::<Vec<_>>();
    assert_eq!(fragments.len(), fragment_count);
    let mut reconstructed = Vec::new();
    for (index, message) in fragments.into_iter().enumerate() {
        match message {
            ServerMsg::ReplaySnapshot { pane_id, start, complete, bytes } => {
                assert_eq!(pane_id, 1);
                assert_eq!(start, index == 0);
                assert_eq!(complete, index + 1 == fragment_count);
                assert!(bytes.len() <= SUBSCRIBER_OUTPUT_FRAME_BYTES);
                reconstructed.extend_from_slice(&bytes);
            }
            other => panic!("expected replay fragment, got {other:?}"),
        }
    }
    assert_eq!(reconstructed, snapshot);
}

/// Replay releases daemon-wide session state before waiting on pane-local snapshot locks.
#[cfg(any(unix, windows))]
#[test]
fn replay_does_not_hold_sessions_while_waiting_on_the_subscriber() {
    #[cfg(unix)]
    let command = "/bin/sh";
    #[cfg(windows)]
    let command = "cmd.exe";

    let state = ServerState::new();
    let pending = state.spawn_paused(command, 80, 24).expect("spawn paused pane");
    let pane_id = pending.pane_id;
    let (tx, rx) = bounded(4);
    let sink = SubscriberSink::new(tx, rx.clone());
    state.attach(pending.session_id, sink.clone()).expect("attach");
    let (replay, subscriber) = {
        let sessions = state.sessions.lock();
        let pane = find_pane(&sessions, pane_id).unwrap();
        (Arc::clone(&pane.replay), Arc::clone(&pane.subscriber))
    };
    let subscriber_guard = subscriber.lock();
    let state_for_replay = Arc::clone(&state);
    let (started_tx, started_rx) = bounded(1);
    let replay_thread = std::thread::spawn(move || {
        started_tx.send(()).unwrap();
        state_for_replay.replay(pane_id, &sink)
    });
    started_rx.recv().unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while replay.try_lock().is_some() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(replay.try_lock().is_none(), "replay never reached the pane-local lock boundary");

    assert!(state.sessions.try_lock().is_some(), "replay retained the global sessions lock");

    drop(subscriber_guard);
    replay_thread.join().unwrap().unwrap();
}

/// Attach-time pause suppresses output until the initial snapshot is queued.
#[test]
fn initial_replay_pause_blocks_live_output() {
    let (tx, rx) = bounded(4);
    let sink = SubscriberSink::new(tx, rx.clone());
    sink.pause_for_replay(1);

    assert_eq!(sink.send_output(1, b"before-snapshot"), OutputSendResult::Lagged);
    assert!(rx.try_recv().is_err());
    sink.send_snapshot(1, b"snapshot".to_vec()).unwrap();
    assert!(matches!(
        rx.recv(),
        Ok(ServerMsg::ReplaySnapshot {
            pane_id: 1,
            start: true,
            complete: true,
            bytes,
        }) if bytes == b"snapshot"
    ));
    assert_eq!(sink.send_output(1, b"after-snapshot"), OutputSendResult::Queued);
}

/// A replaced client cannot resume the active subscriber's paused stream.
#[test]
fn replay_rejects_a_replaced_subscriber() {
    let (old_tx, old_rx) = bounded(4);
    let old = SubscriberSink::new(old_tx, old_rx);
    let (new_tx, new_rx) = bounded(4);
    let new = SubscriberSink::new(new_tx, new_rx.clone());
    new.pause_for_replay(1);
    let subscriber = Mutex::new(Some(new));
    let replay = Mutex::new(VecDeque::from(b"snapshot".to_vec()));

    let error = send_replay_snapshot(&replay, &subscriber, 1, &old).unwrap_err();

    assert!(error.to_string().contains("not attached to this client"));
    assert!(new_rx.try_recv().is_err());
}

/// Disconnecting an old client cannot clear a replacement attachment.
#[test]
fn stale_subscriber_detach_preserves_the_replacement() {
    let (old_tx, old_rx) = bounded(4);
    let old = SubscriberSink::new(old_tx, old_rx);
    let (new_tx, new_rx) = bounded(4);
    let new = SubscriberSink::new(new_tx, new_rx);
    let state = ServerState::new();
    *state.attached.lock() = Some(Attachment { session_id: 7, sink: new.clone() });

    state.detach_subscriber(&old);

    assert!(state
        .attached
        .lock()
        .as_ref()
        .is_some_and(|attachment| attachment.sink.same_subscriber(&new)));
    state.detach_subscriber(&new);
    assert!(state.attached.lock().is_none());
}

/// A transiently full mailbox keeps the subscriber paused until recovery can queue.
#[test]
fn full_mailbox_retries_resync_without_detaching_the_subscriber() {
    let (tx, rx) = bounded(3);
    let subscriber = Mutex::new(Some(SubscriberSink::new(tx, rx.clone())));
    let replay = Mutex::new(VecDeque::new());

    forward_pane_output(b"prefix".as_slice(), &replay, &subscriber, 1);
    {
        let guard = subscriber.lock();
        let sink = guard.as_ref().unwrap();
        sink.send_control(ServerMsg::Exit { pane_id: 2 }).unwrap();
        sink.send_control(ServerMsg::Exit { pane_id: 3 }).unwrap();
    }
    forward_pane_output(b"gap".as_slice(), &replay, &subscriber, 1);

    assert!(subscriber.lock().is_some(), "mailbox pressure is not a disconnect");
    assert!(matches!(rx.recv(), Ok(ServerMsg::Output { bytes, .. }) if bytes == b"prefix"));
    assert_eq!(
        subscriber.lock().as_ref().unwrap().shared.retry_pending_recovery(),
        OutputSendResult::Lagged,
    );

    forward_pane_output(b"suppressed".as_slice(), &replay, &subscriber, 1);

    assert!(matches!(rx.recv(), Ok(ServerMsg::Exit { pane_id: 2 })));
    assert!(matches!(rx.recv(), Ok(ServerMsg::Exit { pane_id: 3 })));
    assert!(matches!(rx.recv(), Ok(ServerMsg::ResyncRequired { pane_id: 1 })));
    assert!(rx.try_recv().is_err(), "paused output must not follow the recovery marker");
}

/// Output saturation preserves capacity for both recovery and terminal control.
#[test]
fn subscriber_sink_reserves_capacity_for_resync_and_exit() {
    let (tx, rx) = bounded(3);
    let sink = SubscriberSink::new(tx, rx.clone());
    assert_eq!(sink.send_output(1, &[1]), OutputSendResult::Queued);
    assert_eq!(sink.send_output(1, &[2]), OutputSendResult::Lagged);

    sink.send_control(ServerMsg::Exit { pane_id: 1 }).unwrap();

    assert!(matches!(rx.try_recv(), Ok(ServerMsg::Output { bytes, .. }) if bytes == vec![1]));
    assert!(matches!(rx.try_recv(), Ok(ServerMsg::ResyncRequired { pane_id: 1 })));
    assert!(matches!(rx.try_recv(), Ok(ServerMsg::Exit { pane_id: 1 })));
}

#[test]
fn subscriber_output_rechunks_to_the_documented_byte_ceiling() {
    let (tx, rx) = bounded(5);
    let sink = SubscriberSink::new(tx, rx.clone());

    assert_eq!(sink.send_output(1, &vec![b'x'; 64 * 1024]), OutputSendResult::Lagged);

    let queued = rx.try_iter().collect::<Vec<_>>();
    assert_eq!(queued.len(), 4, "two slots are reserved before recovery is queued");
    assert!(queued[..3].iter().all(|message| {
        matches!(message, ServerMsg::Output { bytes, .. } if bytes.len() <= SUBSCRIBER_OUTPUT_FRAME_BYTES)
    }));
    assert!(matches!(queued[3], ServerMsg::ResyncRequired { pane_id: 1 }));
    let queued_bytes = queued
        .iter()
        .map(|message| match message {
            ServerMsg::Output { bytes, .. } => bytes.len(),
            _ => 0,
        })
        .sum::<usize>();
    assert_eq!(queued_bytes, 3 * SUBSCRIBER_OUTPUT_FRAME_BYTES);
}

#[test]
fn v120_queue_accounting_covers_messages_and_payload_bytes() {
    // The inventory records "message count and frame ceiling" as tracked
    // separately, with nothing accounting for their product. The product is in
    // fact bounded, by a compile-time assert over the two constants plus runtime
    // paths that keep live output and replay fragments within the frame size.
    // Live output also stops two slots short of capacity.
    //
    // At saturation 4094 output messages consume 33,538,048 bytes, followed by
    // one recovery marker; the final slot stays available for terminal control.
    let (tx, rx) = crossbeam_channel::bounded(CHANNEL_CAP);
    let sink = SubscriberSink::new(tx, rx.clone());

    let payload = vec![0u8; SUBSCRIBER_OUTPUT_FRAME_BYTES * 64];
    for _ in 0..200 {
        assert_ne!(sink.send_output(1, &payload), OutputSendResult::Disconnected);
    }

    let queued = rx.len();
    assert_eq!(
        queued,
        CHANNEL_CAP - 1,
        "output plus recovery marker leave one slot for terminal control"
    );

    // A control message still lands after the recovery marker. Raw output loss
    // never consumes the final slot needed to close or report an error.
    sink.send_control(ServerMsg::Error("lagging".into()))
        .expect("control delivery survives an output flood");

    let mut bytes = 0usize;
    let mut messages = 0usize;
    let mut recovery = 0usize;
    while let Ok(msg) = rx.try_recv() {
        messages += 1;
        match msg {
            ServerMsg::Output { bytes: chunk, .. } => {
                assert!(
                    chunk.len() <= SUBSCRIBER_OUTPUT_FRAME_BYTES,
                    "every queued frame respects the per-frame ceiling"
                );
                bytes += chunk.len();
            }
            ServerMsg::ResyncRequired { pane_id: 1 } => recovery += 1,
            _ => {}
        }
    }

    let ceiling = (CHANNEL_CAP - 2) * SUBSCRIBER_OUTPUT_FRAME_BYTES;
    assert!(bytes <= ceiling, "queued payload {bytes} exceeded the composed ceiling {ceiling}");
    assert_eq!(recovery, 1, "one recovery marker reports the entire gap");
    assert_eq!(messages, CHANNEL_CAP, "the terminal control message occupied the final slot");
}

#[test]
fn subscriber_control_returns_error_when_mailbox_stays_full() {
    let (tx, rx) = bounded(1);
    let sink = SubscriberSink::new(tx, rx.clone());
    sink.send_control(ServerMsg::Exit { pane_id: 1 }).unwrap();
    let (done_tx, done_rx) = bounded(1);
    let sender = std::thread::spawn(move || {
        done_tx.send(sink.send_control(ServerMsg::Exit { pane_id: 2 })).unwrap();
    });

    let result = done_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("control delivery must have a deadline");
    let error = result.expect_err("a full mailbox must reject control delivery after the deadline");
    assert!(error.to_string().contains("mailbox remained full"), "unexpected error: {error}");
    sender.join().expect("control sender thread");
    assert!(matches!(rx.recv(), Ok(ServerMsg::Exit { pane_id: 1 })));
}

#[test]
fn request_reply_uses_the_bounded_control_deadline() {
    let (tx, rx) = bounded(1);
    let sink = SubscriberSink::new(tx, rx.clone());
    sink.send_control(ServerMsg::Exit { pane_id: 1 }).unwrap();
    let (done_tx, done_rx) = bounded(1);
    std::thread::spawn(move || {
        done_tx
            .send(send_reply(&sink, ServerMsg::Sessions(Vec::new())))
            .expect("report reply outcome");
    });

    let result =
        done_rx.recv_timeout(Duration::from_secs(1)).expect("request reply must have a deadline");
    assert!(result.is_err());
    assert!(matches!(rx.recv(), Ok(ServerMsg::Exit { pane_id: 1 })));
}

#[test]
fn blocked_writer_does_not_block_connection_cleanup() {
    let mut requests = Vec::new();
    for _ in 0..(CHANNEL_CAP + 4) {
        crate::frame::write_frame(&mut requests, &ClientMsg::ListSessions).unwrap();
    }
    let (unused_started_tx, _unused_started_rx) = bounded(1);
    let (_unused_release_tx, unused_release_rx) = bounded(1);
    let read_half = BlockingWriteStream {
        read: Cursor::new(requests),
        block_first_write: false,
        write_started: unused_started_tx,
        release_write: unused_release_rx,
        dropped: None,
    };
    let (write_started_tx, write_started_rx) = bounded(1);
    let (release_write_tx, release_write_rx) = bounded(1);
    let writer_dropped = Arc::new(AtomicBool::new(false));
    let write_half = BlockingWriteStream {
        read: Cursor::new(Vec::new()),
        block_first_write: true,
        write_started: write_started_tx,
        release_write: release_write_rx,
        dropped: Some(writer_dropped.clone()),
    };
    let state = ServerState::new();
    let (done_tx, done_rx) = bounded(1);
    let shutdown_release = release_write_tx.clone();
    let handler = std::thread::spawn(move || {
        done_tx
            .send(handle_connection_with_shutdown(state, read_half, write_half, move || {
                let _ = shutdown_release.try_send(());
            }))
            .unwrap();
    });

    write_started_rx.recv_timeout(Duration::from_secs(1)).expect("writer blocked");
    let completed = done_rx.recv_timeout(Duration::from_secs(2));
    if let Err(error) = &completed {
        let _ = release_write_tx.try_send(());
        handler.join().expect("handler cleanup");
        panic!("server shutdown did not interrupt writer: {error}");
    }
    let final_result = completed.expect("checked above");
    handler.join().expect("handler thread");

    assert!(writer_dropped.load(Ordering::Acquire), "writer stream must be reclaimed");
    assert!(final_result.is_err(), "full control mailbox must end the connection");
}

#[test]
fn v120_blocked_worker_owner_orders_cancel_join_and_drop() {
    // The inventory records reader and writer threads per connection with
    // "bounded channels; no compositional owner", and asks that teardown
    // cancel, unblock, join, then release the streams.
    //
    // The behaviour is already covered: a writer blocked mid-send does not
    // stall cleanup. What was not asserted is the *ordering* — that
    // cancellation reaches the writer before the join, rather than the join
    // waiting on a writer nobody told to stop. Those look identical from the
    // outside whenever the writer happens to finish on its own.
    let mut requests = Vec::new();
    for _ in 0..(CHANNEL_CAP + 4) {
        crate::frame::write_frame(&mut requests, &ClientMsg::ListSessions).unwrap();
    }
    let (unused_started_tx, _unused_started_rx) = bounded(1);
    let (_unused_release_tx, unused_release_rx) = bounded(1);
    let read_half = BlockingWriteStream {
        read: Cursor::new(requests),
        block_first_write: false,
        write_started: unused_started_tx,
        release_write: unused_release_rx,
        dropped: None,
    };
    let (write_started_tx, write_started_rx) = bounded(1);
    let (release_write_tx, release_write_rx) = bounded(1);
    let writer_dropped = Arc::new(AtomicBool::new(false));
    let write_half = BlockingWriteStream {
        read: Cursor::new(Vec::new()),
        block_first_write: true,
        write_started: write_started_tx,
        release_write: release_write_rx,
        dropped: Some(writer_dropped.clone()),
    };

    // Records that cancellation ran, so the join can be shown to follow it
    // rather than substitute for it.
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancelled_for_shutdown = cancelled.clone();

    let state = ServerState::new();
    let (done_tx, done_rx) = bounded(1);
    let shutdown_release = release_write_tx.clone();
    let handler = std::thread::spawn(move || {
        done_tx
            .send(handle_connection_with_shutdown(state, read_half, write_half, move || {
                cancelled_for_shutdown.store(true, Ordering::Release);
                let _ = shutdown_release.try_send(());
            }))
            .unwrap();
    });

    write_started_rx.recv_timeout(Duration::from_secs(1)).expect("writer blocked mid-send");
    assert!(
        !cancelled.load(Ordering::Acquire),
        "cancellation must not have run yet — the writer is still blocked"
    );

    let completed = done_rx.recv_timeout(Duration::from_secs(2));
    if let Err(error) = &completed {
        let _ = release_write_tx.try_send(());
        handler.join().expect("handler cleanup");
        panic!("teardown did not interrupt a blocked writer: {error}");
    }
    let final_result = completed.expect("checked above");
    handler.join().expect("handler thread");

    // Ordering: cancellation ran, and only then did the writer end and its
    // stream get released. A join that completed without cancellation would
    // mean the teardown got lucky rather than being correct.
    assert!(cancelled.load(Ordering::Acquire), "teardown must cancel the blocked writer");
    assert!(
        writer_dropped.load(Ordering::Acquire),
        "the writer stream is released after the join, not leaked"
    );
    assert!(final_result.is_err(), "a full control mailbox ends the connection");
}

#[test]
fn pane_exit_releases_subscriber_when_control_mailbox_stays_full() {
    let (tx, rx) = bounded(1);
    let sink = SubscriberSink::new(tx, rx.clone());
    sink.send_control(ServerMsg::Exit { pane_id: 1 }).unwrap();
    let subscriber = Mutex::new(Some(sink));

    notify_subscriber_exit(&subscriber, 2);

    assert!(subscriber.lock().is_none(), "exited pane must release its subscriber sender");
    assert!(matches!(rx.recv(), Ok(ServerMsg::Exit { pane_id: 1 })));
    assert!(rx.try_recv().is_err(), "timed-out exit must not replace queued control");
}

#[test]
fn pane_exit_drain_forwards_all_buffered_output() {
    let (out_tx, out_rx) = bounded(4);
    out_tx.send(b"one".to_vec()).unwrap();
    out_tx.send(b"two".to_vec()).unwrap();
    let replay = Mutex::new(VecDeque::new());
    let (subscriber_tx, subscriber_rx) = bounded(4);
    let subscriber = Mutex::new(Some(SubscriberSink::new(subscriber_tx, subscriber_rx.clone())));

    drain_ready_pane_output(&out_rx, &replay, &subscriber, 7);

    assert_eq!(replay.lock().iter().copied().collect::<Vec<_>>(), b"onetwo");
    assert!(matches!(
        subscriber_rx.recv(),
        Ok(ServerMsg::Output { pane_id: 7, bytes }) if bytes == b"one"
    ));
    assert!(matches!(
        subscriber_rx.recv(),
        Ok(ServerMsg::Output { pane_id: 7, bytes }) if bytes == b"two"
    ));
    assert!(subscriber_rx.try_recv().is_err());
}

#[cfg(any(unix, windows))]
#[test]
fn naturally_exited_pane_is_reaped_from_its_session() {
    #[cfg(unix)]
    let command = "/usr/bin/true";
    #[cfg(windows)]
    let command = "whoami.exe";

    let state = ServerState::new();
    let (session_id, pane_id) = state.spawn(command, 80, 24).expect("spawn short-lived shell");
    #[cfg(windows)]
    state.input(pane_id, b"\x1b[1;1R".to_vec()).expect("answer ConPTY cursor query");

    let deadline = Instant::now() + Duration::from_secs(3);
    while state.session_count() != 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(state.session_count(), 0, "natural exit must remove the empty session");
    assert!(state.kill_pane(pane_id).is_err(), "naturally reaped pane must be unknown");
    let (tx, rx) = bounded(CHANNEL_CAP);
    assert!(
        state.attach(session_id, SubscriberSink::new(tx, rx)).is_err(),
        "empty reaped session must be unknown"
    );
}

#[cfg(unix)]
#[test]
fn exited_shell_with_background_descendant_is_reaped() {
    let state = ServerState::new();
    let (_session_id, pane_id) = state.spawn("/bin/sh", 80, 24).expect("spawn shell");
    state
        .input(pane_id, b"trap '' HUP\n(while :; do printf x; sleep 0.01; done) &\nexit\n".to_vec())
        .expect("launch background descendant");

    let deadline = Instant::now() + Duration::from_secs(3);
    while state.session_count() != 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(state.session_count(), 0, "exit probe must reap pane with inherited PTY child");
}

#[cfg(any(unix, windows))]
#[test]
fn paused_spawn_queues_spawned_before_exit_and_reap() {
    #[cfg(unix)]
    let command = "/usr/bin/true";
    #[cfg(windows)]
    let command = "whoami.exe";

    let state = ServerState::new();
    let pending = state.spawn_paused(command, 80, 24).expect("spawn paused pane");
    let session_id = pending.session_id;
    let pane_id = pending.pane_id;
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(state.session_count(), 1, "paused pane must remain published until announced");

    let (tx, rx) = bounded(CHANNEL_CAP);
    let sink = SubscriberSink::new(tx, rx.clone());
    state.subscribe_if_unattached(session_id, sink.clone());
    send_reply(&sink, ServerMsg::Spawned { session_id, pane_id }).unwrap();
    pending.start().unwrap();
    #[cfg(windows)]
    state.input(pane_id, b"\x1b[1;1R".to_vec()).expect("answer ConPTY cursor query");

    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(1)),
        Ok(ServerMsg::Spawned { session_id: actual_session, pane_id: actual_pane })
            if actual_session == session_id && actual_pane == pane_id
    ));
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut saw_exit = false;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ServerMsg::Exit { pane_id: actual }) if actual == pane_id => {
                saw_exit = true;
                break;
            }
            Ok(_) | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(saw_exit, "announced short-lived pane must emit Exit");
    let deadline = Instant::now() + Duration::from_secs(1);
    while state.session_count() != 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(state.session_count(), 0);
}

/// Real PTY integration (unix): spawn a shell through the sonicterm-io seam,
/// attach a subscriber, feed a command on stdin, and assert the marker bytes
/// travel PTY -> reader thread -> replay/subscriber. This is the end-to-end
/// guard for the PTY-seam refactor: `build_pane` via `PtyHandle`, the `out_rx`
/// drain, and the `in_tx` write path. Unix-only: it depends on `/bin/sh` and
/// printf.
#[cfg(unix)]
#[test]
fn shell_output_flows_through_pty_seam_to_subscriber() {
    const MARKER: &str = "sonic_marker_42";

    let state = ServerState::new();
    let (sid, pane_id) = state.spawn("/bin/sh", 80, 24).expect("spawn /bin/sh");
    assert_eq!(state.session_count(), 1);

    // Install a subscriber before driving input so live output is forwarded.
    let (tx, rx) = bounded(CHANNEL_CAP);
    let sink = SubscriberSink::new(tx, rx.clone());
    let panes = state.attach(sid, sink.clone()).expect("attach");
    assert_eq!(panes.len(), 1);
    assert_eq!(panes[0].id, pane_id);
    state.replay(pane_id, &sink).expect("initial replay");
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(1)),
        Ok(ServerMsg::ReplaySnapshot { pane_id: actual, .. }) if actual == pane_id
    ));

    // Drive the shell to emit the marker on its own line, then exit.
    state.input(pane_id, format!("printf '{MARKER}\\n'\n").into_bytes()).expect("input write");

    // Poll the subscriber mailbox until the marker shows up (bounded wait so
    // the test can never hang CI). Output arrives as ServerMsg::Output chunks.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    let mut found = false;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMsg::Output { bytes, .. }) => {
                seen.extend_from_slice(&bytes);
                if String::from_utf8_lossy(&seen).contains(MARKER) {
                    found = true;
                    break;
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert!(
        found,
        "marker {MARKER:?} never reached the subscriber; got {:?}",
        String::from_utf8_lossy(&seen)
    );
}
