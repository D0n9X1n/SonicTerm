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

    sink.send_drop_oldest(ServerMsg::Exit { pane_id: 1 }).unwrap();
    assert!(sink.send_drop_oldest(ServerMsg::Exit { pane_id: 2 }).is_err());

    match rx.try_recv() {
        Ok(ServerMsg::Exit { pane_id }) => assert_eq!(pane_id, 1),
        other => panic!("expected the original Exit, got {other:?}"),
    }
    assert!(rx.try_recv().is_err(), "only one message should remain");
}

#[test]
fn subscriber_sink_drops_new_output_when_full() {
    let (tx, rx) = bounded(1);
    let sink = SubscriberSink::new(tx, rx.clone());
    sink.send_drop_oldest(ServerMsg::Exit { pane_id: 1 }).unwrap();

    sink.send_drop_oldest(ServerMsg::Output { pane_id: 1, bytes: vec![1, 2, 3] }).unwrap();

    assert!(matches!(rx.try_recv(), Ok(ServerMsg::Exit { pane_id: 1 })));
}

#[test]
fn subscriber_sink_reserves_capacity_for_exit() {
    let (tx, rx) = bounded(2);
    let sink = SubscriberSink::new(tx, rx.clone());
    sink.send_drop_oldest(ServerMsg::Output { pane_id: 1, bytes: vec![1] }).unwrap();
    sink.send_drop_oldest(ServerMsg::Output { pane_id: 1, bytes: vec![2] }).unwrap();

    sink.send_drop_oldest(ServerMsg::Exit { pane_id: 1 }).unwrap();

    assert!(matches!(rx.try_recv(), Ok(ServerMsg::Output { bytes, .. }) if bytes == vec![1]));
    assert!(matches!(rx.try_recv(), Ok(ServerMsg::Exit { pane_id: 1 })));
}

#[test]
fn subscriber_output_rechunks_to_the_documented_byte_ceiling() {
    let (tx, rx) = bounded(5);
    let sink = SubscriberSink::new(tx, rx.clone());

    sink.send_drop_oldest(ServerMsg::Output { pane_id: 1, bytes: vec![b'x'; 64 * 1024] }).unwrap();

    let queued = rx.try_iter().collect::<Vec<_>>();
    assert_eq!(queued.len(), 4, "one slot must remain reserved for control");
    assert!(queued.iter().all(|message| {
        matches!(message, ServerMsg::Output { bytes, .. } if bytes.len() <= SUBSCRIBER_OUTPUT_FRAME_BYTES)
    }));
    let queued_bytes = queued
        .iter()
        .map(|message| match message {
            ServerMsg::Output { bytes, .. } => bytes.len(),
            _ => 0,
        })
        .sum::<usize>();
    assert!(queued_bytes <= 4 * SUBSCRIBER_OUTPUT_FRAME_BYTES);
}

#[test]
#[ignore = "v120-invariant-baseline:v120_queue_accounting_covers_messages_and_payload_bytes:WP-MUX"]
fn v120_queue_accounting_covers_messages_and_payload_bytes() {
    panic!("baseline invariant requires WP-MUX byte and message accounting");
}

#[test]
fn subscriber_control_returns_error_when_mailbox_stays_full() {
    let (tx, rx) = bounded(1);
    let sink = SubscriberSink::new(tx, rx.clone());
    sink.send_drop_oldest(ServerMsg::Exit { pane_id: 1 }).unwrap();
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
    sink.send_drop_oldest(ServerMsg::Exit { pane_id: 1 }).unwrap();
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
#[ignore = "v120-invariant-baseline:v120_blocked_worker_owner_orders_cancel_join_and_drop:WP-MUX"]
fn v120_blocked_worker_owner_orders_cancel_join_and_drop() {
    blocked_writer_does_not_block_connection_cleanup();
    panic!("baseline invariant requires WP-MUX connection owner");
}

#[test]
fn pane_exit_releases_subscriber_when_control_mailbox_stays_full() {
    let (tx, rx) = bounded(1);
    let sink = SubscriberSink::new(tx, rx.clone());
    sink.send_drop_oldest(ServerMsg::Exit { pane_id: 1 }).unwrap();
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
/// guard for the #810 refactor (build_pane via PtyHandle, out_rx drain,
/// in_tx write path). Unix-only: it depends on `/bin/sh` and printf.
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
    let panes = state.attach(sid, sink).expect("attach");
    assert_eq!(panes.len(), 1);
    assert_eq!(panes[0].id, pane_id);

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
