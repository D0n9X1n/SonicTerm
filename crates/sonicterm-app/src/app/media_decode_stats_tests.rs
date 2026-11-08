use std::cell::Cell;
use std::time::Duration;

use super::*;

/// A clock that advances by the next of `steps_us` each time it is read, so a test decides how long
/// each timed attempt takes.
fn scripted_clock(steps_us: &'static [u64]) -> impl FnMut() -> Instant {
    let start = Instant::now();
    let elapsed = Cell::new(Duration::ZERO);
    let index = Cell::new(0usize);
    move || {
        let step = steps_us.get(index.get()).copied().unwrap_or(0);
        index.set(index.get() + 1);
        elapsed.set(elapsed.get() + Duration::from_micros(step));
        start + elapsed.get()
    }
}

/// 7,999 µs lands under the threshold and in the 7,999 bucket; 8,000 and 8,001 µs do not, so a
/// decode just under 8 ms never shares a bucket with one at or above it. The maximum is exact.
#[test]
fn the_threshold_splits_7999_from_8000() {
    let stats = DecodeStats::default();
    for elapsed_us in [7_999, 8_000, 8_001] {
        stats.record(elapsed_us, DecodeOutcome::Decoded);
    }
    assert_eq!(stats.attempts.load(Ordering::Relaxed), 3);
    assert_eq!(stats.under_threshold.load(Ordering::Relaxed), 1);
    assert_eq!(stats.max_us.load(Ordering::Relaxed), 8_001);
    let snapshot = stats.duration.snapshot();
    let buckets = snapshot.buckets();
    let edge = buckets.bounds.iter().position(|bound| *bound == 7_999).expect("a 7,999 µs bound");
    assert_eq!(buckets.counts[edge], 1, "only 7,999 µs is in the bucket ending at 7,999");
    assert_eq!(buckets.counts[edge + 1], 2, "8,000 and 8,001 µs share the next bucket");
}

/// The p95 rule is exact: two distributions that differ only at 7,999 against 8,000 µs reach
/// different verdicts, and a maximum above 8 ms with a p95 below it reads as below.
#[test]
fn the_p95_rule_decides_at_the_threshold() {
    // Twenty attempts: nineteen fast and one at the threshold's edge. Nearest-rank p95 is the 19th.
    let fast_then = |slowest_us: u64, second_us: u64| {
        let stats = DecodeStats::default();
        for _ in 0..18 {
            stats.record(1_000, DecodeOutcome::Decoded);
        }
        stats.record(second_us, DecodeOutcome::Decoded);
        stats.record(slowest_us, DecodeOutcome::Decoded);
        stats
    };
    assert!(!fast_then(50_000, 7_999).p95_at_least_threshold(), "p95 7,999 µs, max 50 ms");
    assert!(fast_then(50_000, 8_000).p95_at_least_threshold(), "p95 8,000 µs");
    assert!(!p95_at_least_threshold(0, 0), "no attempt is no verdict");
    assert!(p95_at_least_threshold(1, 0));
}

/// Each outcome has its own count, and every attempt counts once whatever its outcome.
#[test]
fn outcomes_are_counted_apart() {
    let stats = DecodeStats::default();
    stats.record(10, DecodeOutcome::Decoded);
    stats.record(20, DecodeOutcome::Failed);
    stats.record(30, DecodeOutcome::RefusedAfterDecode);
    stats.record(40, DecodeOutcome::RefusedAfterDecode);
    assert_eq!(stats.attempts.load(Ordering::Relaxed), 4);
    assert_eq!(stats.decoded.load(Ordering::Relaxed), 1);
    assert_eq!(stats.failed.load(Ordering::Relaxed), 1);
    assert_eq!(stats.refused_after_decode.load(Ordering::Relaxed), 2);
}

/// With the counters on, the timer reads its clock around the decode and records the elapsed time;
/// with them off it reads no clock and records nothing.
#[test]
fn the_timer_reads_its_clock_only_while_counting() {
    let stats = DecodeStats::default();
    let mut timer = DecodeTimer::new(Some(&stats), scripted_clock(&[0, 9_500]));
    let (value, elapsed) = timer.time(|| 7);
    assert_eq!((value, elapsed), (7, Some(9_500)));
    timer.record(elapsed, DecodeOutcome::Decoded);
    assert_eq!(stats.max_us.load(Ordering::Relaxed), 9_500);

    let reads = Cell::new(0);
    let mut off = DecodeTimer::new(None, || {
        reads.set(reads.get() + 1);
        Instant::now()
    });
    let (value, elapsed) = off.time(|| 8);
    off.record(elapsed, DecodeOutcome::Decoded);
    assert_eq!((value, elapsed, reads.get()), (8, None, 0), "no clock read while off");
}
