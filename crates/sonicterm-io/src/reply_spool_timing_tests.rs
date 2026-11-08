//! Pins the spool's rare-path timing: a compaction's count, total and longest are recorded exactly
//! with an injected clock, one slow compaction per pane is flagged for its warning, and the
//! spool-lock contention gauge moves only when a send found the lock taken.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;

const RECORD: u64 = (CHUNK_BYTES + HEADER_BYTES) as u64;

/// A clock that advances `step` on every read from a fixed start, so each timed span lasts exactly
/// one step.
fn stepping_clock(step: Duration) -> SpoolClock {
    let start = Instant::now();
    let reads = Arc::new(AtomicU64::new(0));
    Arc::new(move || start + step * reads.fetch_add(1, Ordering::Relaxed) as u32)
}

/// Fill `sender` past the compaction minimum and consume more than half, so its next spilled
/// record compacts.
fn due_for_compaction(sender: &PtyReplySender, reader: &ReplyReader) {
    let spilled = (2 * COMPACT_MIN_BYTES / RECORD + 2) as usize;
    for _ in 0..=spilled {
        sender.send(vec![b'a'; CHUNK_BYTES]).unwrap();
    }
    for _ in 0..(spilled * 3 / 4) {
        reader.pop().unwrap().unwrap();
    }
}

/// With every timed span lasting exactly 60 ms, each compaction adds one to the count and 60 ms
/// to the total, and the longest is 60 ms; the first is flagged slow, and a second is counted but
/// not flagged again.
#[test]
fn compactions_are_timed_exactly_and_warn_once() {
    let clock = stepping_clock(Duration::from_millis(60));
    let (sender, reader) = reply_spool_with(SpillPool::new(u64::MAX), clock);
    assert_eq!(sender.stats().compactions, 0);
    due_for_compaction(&sender, &reader);
    sender.send(vec![b'b'; CHUNK_BYTES]).unwrap();
    let stats = sender.stats();
    assert_eq!(
        (stats.compactions, stats.compact_us_total, stats.compact_us_max),
        (1, 60_000, 60_000)
    );
    assert!(
        sender.stats.warned_slow_compaction.load(Ordering::Acquire),
        "the first slow one warns"
    );
    due_for_compaction(&sender, &reader);
    sender.send(vec![b'c'; CHUNK_BYTES]).unwrap();
    let stats = sender.stats();
    assert_eq!((stats.compactions, stats.compact_us_total), (2, 120_000));
    assert_eq!(stats.compact_us_max, 60_000);
}

/// The warning decision itself: the first slow compaction of a pane warns, every later one,
/// slow or not, does not; a fast one never warns.
#[test]
fn only_the_first_slow_compaction_of_a_pane_warns() {
    let (sender, _reader) = reply_spool_with(SpillPool::new(u64::MAX), Arc::new(Instant::now));
    assert!(!sender.record_compaction(Duration::from_millis(10)), "fast");
    assert!(sender.record_compaction(Duration::from_millis(60)), "first slow");
    assert!(!sender.record_compaction(Duration::from_millis(90)), "second slow");
    assert_eq!(sender.stats().compactions, 3);
    assert_eq!(sender.stats().compact_us_max, 90_000);
}

/// A compaction under the threshold is counted and never flagged.
#[test]
fn a_fast_compaction_is_counted_without_a_warning() {
    let clock = stepping_clock(Duration::from_millis(5));
    let (sender, reader) = reply_spool_with(SpillPool::new(u64::MAX), clock);
    due_for_compaction(&sender, &reader);
    sender.send(vec![b'b'; CHUNK_BYTES]).unwrap();
    assert_eq!(sender.stats().compactions, 1);
    assert!(!sender.stats.warned_slow_compaction.load(Ordering::Acquire));
}

/// An uncontended send reads no clock and leaves the contention gauge at zero; a send that finds
/// the lock taken by another thread records how long it waited.
#[test]
fn the_contention_gauge_moves_only_when_the_lock_was_taken() {
    let reads = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&reads);
    let start = Instant::now();
    let clock: SpoolClock = Arc::new(move || {
        let read = counted.fetch_add(1, Ordering::Relaxed);
        start + Duration::from_millis(30) * read as u32
    });
    let (sender, _reader) = reply_spool_with(SpillPool::new(u64::MAX), clock);
    sender.send(vec![1]).unwrap();
    assert_eq!(reads.load(Ordering::Relaxed), 0, "an uncontended send read the clock");
    assert_eq!(sender.stats().send_lock_wait_us_max, 0);
    let held = sender.state.lock();
    let (taken_tx, taken_rx) = std::sync::mpsc::channel();
    let waiter = {
        let sender = sender.clone();
        std::thread::spawn(move || {
            taken_tx.send(()).unwrap();
            sender.send(vec![2]).unwrap();
        })
    };
    taken_rx.recv().unwrap();
    // The waiter is now blocked on the lock this thread holds.
    while reads.load(Ordering::Relaxed) == 0 {
        std::thread::yield_now();
    }
    drop(held);
    waiter.join().unwrap();
    assert_eq!(
        sender.stats().send_lock_wait_us_max,
        30_000,
        "one 30 ms step between the two reads"
    );
}
