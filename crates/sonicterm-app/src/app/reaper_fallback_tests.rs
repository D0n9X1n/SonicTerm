//! Pins the fallback timing record: totals, the longest, and which ones count as slow.

use std::time::Duration;

use super::{slow_fallback_warning, FallbackTiming, FALLBACK_BUCKETS_US, SLOW_FALLBACK};

/// Each fallback adds to the count and total; one past the threshold is slow, one at it is not.
#[test]
fn fallbacks_are_timed_and_slow_ones_counted() {
    let mut timing = FallbackTiming::default();
    assert!(!timing.record(Duration::from_millis(3)));
    assert!(!timing.record(SLOW_FALLBACK), "exactly the threshold is not slow");
    assert!(timing.record(SLOW_FALLBACK + Duration::from_millis(1)));
    assert_eq!(timing.count, 3);
    assert_eq!(timing.slow, 1);
    assert_eq!(timing.longest_us, 51_000);
    assert_eq!(timing.total_us, 3_000 + 50_000 + 51_000);
}

/// Each fallback lands in the bucket of its blocked time: 30 µs in the 64 µs bucket, 40 ms in the
/// 65,536 µs one, and 300 s, past every bound, in the last.
#[test]
fn fallbacks_fill_the_histogram_by_blocked_time() {
    let mut timing = FallbackTiming::default();
    timing.record(Duration::from_micros(30));
    timing.record(Duration::from_millis(40));
    timing.record(Duration::from_secs(300));
    let bucket =
        |micros: u64| FALLBACK_BUCKETS_US.iter().position(|bound| *bound == micros).unwrap();
    assert_eq!(timing.histogram[bucket(64)], 1);
    assert_eq!(timing.histogram[bucket(65_536)], 1);
    assert_eq!(timing.histogram[FALLBACK_BUCKETS_US.len()], 1);
    assert_eq!(timing.histogram.iter().sum::<u64>(), timing.count);
}

/// The driver warns for a fallback past 50 ms and not for one under it, through the same capture
/// production logging uses.
#[test]
fn a_slow_fallback_warns_and_a_fast_one_does_not() {
    assert_eq!(slow_fallback_warning(Duration::from_millis(49)), None);
    assert_eq!(slow_fallback_warning(SLOW_FALLBACK), None, "the bound itself is not slow");
    assert_eq!(slow_fallback_warning(Duration::from_millis(80)), Some(80));
    let driver = super::super::reaper_driver::ReaperDriver::new(governor()).unwrap();
    let log = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<u8>::new()));
    let writer = std::sync::Arc::clone(&log);
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(move || SharedLog(std::sync::Arc::clone(&writer)))
        .finish();
    sonicterm_logging::test_capture::with_default(subscriber, || {
        driver.note_fallback(7, Duration::from_millis(10));
        driver.note_fallback(7, Duration::from_millis(120));
    });
    let text = String::from_utf8(log.lock().clone()).unwrap();
    let slow: Vec<_> =
        text.lines().filter(|line| line.contains("fallback blocked the event loop")).collect();
    assert_eq!(slow.len(), 1, "{text}");
    assert!(slow[0].contains("blocked_ms=120"), "{slow:?}");
    assert_eq!(driver.fallback_timing().count, 2);
}

/// A capture buffer the test subscriber writes into.
struct SharedLog(std::sync::Arc<parking_lot::Mutex<Vec<u8>>>);

impl std::io::Write for SharedLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A governor with no limits, as the driver's own tests build one.
fn governor() -> sonicterm_resource::ResourceGovernor {
    use sonicterm_types::{GovernorLimits, ProcessKind};
    sonicterm_resource::ResourceGovernor::new(
        ProcessKind::Gui,
        GovernorLimits {
            process_bytes: usize::MAX,
            class_bytes: enum_map::enum_map! { _ => usize::MAX },
            class_items: enum_map::enum_map! { _ => None },
        },
    )
    .unwrap()
}
