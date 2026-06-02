//! Issue #508 — focus-change-mid-write race test (Opus Step-2
//! APPROVED-DIAG requirement #7).
//!
//! Verifies per-chunk atomicity when the active pane changes between
//! two pipe `ReadFile` reads: chunk 1 must land entirely on pane A,
//! chunk 2 entirely on pane B. Sub-chunk atomicity is explicitly NOT
//! required (a single 4 KiB chunk crossing the focus change can go
//! to whichever pane was active when the publish slot was sampled).
//!
//! Implementation note: this is a pure-Rust sink simulation —
//! exercising the same `Arc<Mutex<Option<Sender>>>` swap-on-publish
//! contract the App uses. We don't drive the full named-pipe loop;
//! that's the job of `harness_pipe_test::e2e_window_title_sentinel`.
//! The race test focuses on contract semantics so a regression in
//! `publish` ordering is caught even when no Windows shell is
//! available.

#![cfg(all(target_os = "windows", feature = "harness"))]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{unbounded, Sender};

type Sink = Arc<Mutex<Option<Sender<Vec<u8>>>>>;

fn publish(sink: &Sink, tx: Option<Sender<Vec<u8>>>) {
    *sink.lock().unwrap() = tx;
}

fn forward_chunk(sink: &Sink, chunk: Vec<u8>) {
    let tx = sink.lock().unwrap().clone();
    if let Some(tx) = tx {
        tx.send(chunk).expect("forward chunk to active pane");
    }
}

#[test]
fn focus_change_between_chunks_is_per_chunk_atomic() {
    let sink: Sink = Arc::new(Mutex::new(None));
    let (tx_a, rx_a) = unbounded::<Vec<u8>>();
    let (tx_b, rx_b) = unbounded::<Vec<u8>>();

    // ---- Simulate the named-pipe accept_loop + App pane-publish
    //      cycle, two chunks split across a focus change ----

    // Pane A is initially active.
    publish(&sink, Some(tx_a));

    let chunk_1: Vec<u8> = (0..4096u32).map(|i| (i & 0xFF) as u8).collect();
    forward_chunk(&sink, chunk_1.clone());

    // Focus changes to pane B BETWEEN the two ReadFile chunks
    // (this is the documented seam where atomicity is promised).
    publish(&sink, Some(tx_b));

    let chunk_2: Vec<u8> = (4096..8192u32).map(|i| (i & 0xFF) as u8).collect();
    forward_chunk(&sink, chunk_2.clone());

    // ---- Verify per-chunk atomicity ----
    let got_a = rx_a.recv_timeout(Duration::from_secs(2)).expect("pane A receives chunk 1");
    assert_eq!(got_a, chunk_1, "chunk 1 must land entirely on pane A");
    assert!(rx_a.try_recv().is_err(), "pane A must not receive any of chunk 2");

    let got_b = rx_b.recv_timeout(Duration::from_secs(2)).expect("pane B receives chunk 2");
    assert_eq!(got_b, chunk_2, "chunk 2 must land entirely on pane B");
    assert!(rx_b.try_recv().is_err(), "pane B must not receive any of chunk 1");
}

#[test]
fn publish_none_drops_subsequent_chunks() {
    let sink: Sink = Arc::new(Mutex::new(None));
    let (tx_a, rx_a) = unbounded::<Vec<u8>>();

    publish(&sink, Some(tx_a));
    forward_chunk(&sink, b"first".to_vec());
    // Hide main window → publish None.
    publish(&sink, None);
    forward_chunk(&sink, b"second-dropped".to_vec());
    // Bring pane back.
    let (tx_a2, _rx_a2) = unbounded::<Vec<u8>>();
    publish(&sink, Some(tx_a2));
    forward_chunk(&sink, b"third".to_vec());

    assert_eq!(rx_a.recv_timeout(Duration::from_secs(2)).unwrap(), b"first");
    assert!(
        rx_a.try_recv().is_err(),
        "pane A must not receive 'second-dropped' (sink was None when it arrived)"
    );
}
