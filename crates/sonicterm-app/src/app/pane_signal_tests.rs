//! Pins the pane signal's handshake: a change announced after the waiter read the sequence is
//! never slept through, every waiter wakes, and an unannounced wait ends at its deadline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::PaneSignal;

/// A notification between the waiter's snapshot and its sleep returns at once.
#[test]
fn a_change_after_the_snapshot_is_not_slept_through() {
    let signal = PaneSignal::default();
    let seen = signal.sequence();
    signal.notify();
    let started = Instant::now();
    let sequence = signal.wait_past(seen, Instant::now() + Duration::from_secs(10));
    assert_eq!(sequence, seen + 1);
    assert!(started.elapsed() < Duration::from_secs(1), "the wait slept through the change");
}

/// One notification wakes every waiter sleeping on the same pane.
#[test]
fn every_waiter_wakes_on_one_change() {
    let signal = Arc::new(PaneSignal::default());
    let seen = signal.sequence();
    let waiters: Vec<_> = (0..4)
        .map(|_| {
            let signal = Arc::clone(&signal);
            std::thread::spawn(move || {
                let started = Instant::now();
                signal.wait_past(seen, Instant::now() + Duration::from_secs(10));
                started.elapsed()
            })
        })
        .collect();
    // The waiters are asleep before the change, so only the notification can wake them early.
    std::thread::sleep(Duration::from_millis(50));
    signal.notify();
    for waiter in waiters {
        assert!(waiter.join().unwrap() < Duration::from_secs(5), "a waiter missed the change");
    }
}

/// With no change the wait ends at its deadline and reports the unchanged sequence.
#[test]
fn an_unannounced_wait_ends_at_its_deadline() {
    let signal = PaneSignal::default();
    let started = Instant::now();
    assert_eq!(signal.wait_past(0, Instant::now() + Duration::from_millis(30)), 0);
    assert!(started.elapsed() >= Duration::from_millis(30));
}
