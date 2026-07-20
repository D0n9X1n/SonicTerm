use super::*;
use crate::proto::ServerMsg;
use crossbeam_channel::bounded;
#[cfg(unix)]
use std::time::{Duration, Instant};

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
fn subscriber_control_waits_for_capacity_instead_of_dropping() {
    let (tx, rx) = bounded(1);
    let sink = SubscriberSink::new(tx, rx.clone());
    sink.send_drop_oldest(ServerMsg::Exit { pane_id: 1 }).unwrap();
    let (done_tx, done_rx) = bounded(1);
    std::thread::spawn(move || {
        done_tx.send(sink.send_control(ServerMsg::Exit { pane_id: 2 })).unwrap();
    });

    assert!(done_rx.try_recv().is_err(), "control send should wait while full");
    assert!(matches!(rx.recv(), Ok(ServerMsg::Exit { pane_id: 1 })));
    done_rx.recv_timeout(std::time::Duration::from_secs(1)).expect("send unblocked").unwrap();
    assert!(matches!(rx.recv(), Ok(ServerMsg::Exit { pane_id: 2 })));
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
    state
        .input(pane_id, format!("printf '{MARKER}\\n'\n").into_bytes())
        .expect("input write");

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
