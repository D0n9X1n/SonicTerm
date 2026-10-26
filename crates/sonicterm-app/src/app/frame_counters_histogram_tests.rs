//! Regression contracts for this responsibility, sharing the existing fixture state.

use super::*;

#[test]
fn histogram_reports_exact_sums_and_bucket_bounds_never_exact_maxima() {
    // A maximum or p95 is only known to lie in a bucket, so it prints as `_le` or `_gt`.
    let mut frames = Histogram::new(HistogramUnit::Millis);
    frames.record_us(3_000);
    frames.record_us(5_500);
    assert_eq!(frames.count(), 2);
    assert_eq!(frames.sum_us(), 8_500);
    assert_eq!(frames.max(), Some(Bound::AtMost(7)));
    assert_eq!(frames.p95(), Some(Bound::AtMost(7)));
    frames.record_us(120_000);
    assert_eq!(frames.max(), Some(Bound::Above(100)));
    let fields = frames.line_fields("present_interval");
    assert!(fields.contains("present_interval_sum_us=128500"), "{fields}");
    assert!(fields.contains("present_interval_max_gt_ms=100"), "{fields}");
    assert!(fields.contains("present_interval_p95_gt_ms=100"), "{fields}");
    // A value equal to a bound belongs to that bound's bucket.
    let mut waits = Histogram::new(HistogramUnit::Micros);
    waits.record_us(10);
    assert_eq!(waits.max(), Some(Bound::AtMost(10)));
    assert!(waits.line_fields("ui_parser_wait").contains("ui_parser_wait_max_le_us=10"));
}

#[test]
fn an_empty_interval_prints_no_fields() {
    // Lines omit zero fields, so a histogram with no events contributes nothing.
    let frames = Histogram::new(HistogramUnit::Millis);
    assert_eq!(frames.line_fields("handler"), "");
    assert_eq!((frames.max(), frames.p95()), (None, None));
}

#[test]
fn interval_bounds_come_only_from_buckets_that_grew() {
    // Counters are cumulative and never reset; a reader takes deltas from its own snapshot.
    let mut frames = Histogram::new(HistogramUnit::Millis);
    frames.record_us(150_000);
    let earlier = frames.clone();
    frames.record_us(3_000);
    let interval = frames.delta_since(&earlier);
    assert_eq!(interval.count(), 1);
    assert_eq!(interval.sum_us(), 3_000);
    assert_eq!(interval.max(), Some(Bound::AtMost(4)));
}

#[test]
fn atomic_histogram_snapshot_matches_its_records() {
    // VT workers record into shared atomics; a snapshot reads the same counts and sum.
    let shared = AtomicHistogram::new(HistogramUnit::Micros);
    shared.record_us(40);
    shared.record_us(6_000);
    let snapshot = shared.snapshot();
    assert_eq!((snapshot.count(), snapshot.sum_us()), (2, 6_040));
    assert_eq!(snapshot.max(), Some(Bound::Above(5_000)));
}

#[test]
fn histogram_buckets_export_the_used_slots_and_the_exact_sum() {
    // The harness writes each histogram as unit, bounds, used bucket counts (overflow last)
    // and the exact microsecond sum; a count or a missing name has no histogram.
    let mut handler = Histogram::new(HistogramUnit::Millis);
    handler.record_us(3_000);
    handler.record_us(150_000);
    let mut wait = Histogram::new(HistogramUnit::Micros);
    wait.record_us(10);
    wait.record_us(6_000);
    let mut record = CounterRecord::default();
    record.push_histogram("handler", handler);
    record.push_histogram("ui_parser_wait", wait);
    record.push_count("attempts", 1);
    let millis = record.histogram_buckets("handler").expect("ms histogram");
    assert_eq!((millis.unit, millis.bounds, millis.sum_us), ("ms", &MILLIS_BOUNDS[..], 153_000));
    assert_eq!(millis.counts, &[1, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
    let micros = record.histogram_buckets("ui_parser_wait").expect("us histogram");
    assert_eq!((micros.unit, micros.bounds, micros.sum_us), ("us", &MICROS_BOUNDS[..], 6_010));
    assert_eq!(micros.counts, &[1, 0, 0, 0, 0, 0, 1]);
    assert!(record.histogram_buckets("attempts").is_none());
    assert!(record.histogram_buckets("missing").is_none());
}
