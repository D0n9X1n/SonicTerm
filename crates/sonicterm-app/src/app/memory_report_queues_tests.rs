//! Pins the report's PTY QUEUES section: memory and disk totals kept apart, each category named,
//! the fallback count, slow count and timings, and the histogram only once a fallback ran.

use std::time::Duration;

use sonicterm_io::pty::PtyQueueFootprint;

use super::super::reaper_fallback::FallbackTiming;
use super::super::retention_queues::QueueTotals;
use super::QueueSection;

/// Two panes' queues and two fallbacks, one slow, render as named categories and buckets; disk
/// spill is shown apart from the in-memory total.
#[test]
fn the_section_names_every_category_and_the_fallback_buckets() {
    let mut fallback = FallbackTiming::default();
    fallback.record(Duration::from_micros(30));
    fallback.record(Duration::from_millis(80));
    let section = QueueSection {
        totals: QueueTotals {
            panes: 2,
            footprint: PtyQueueFootprint {
                input_queued_bytes: 100,
                writer_in_flight_bytes: 20,
                reply_resident_bytes: 65_536,
                reply_spill_bytes: 4_096,
            },
        },
        fallback,
        spill_files: (8_192, 268_435_456),
    };
    let text = section.lines().join("\n");
    assert!(text.contains("reply spill files 8192 B of the 268435456 B process budget"), "{text}");
    assert!(text.contains("PTY QUEUES (2 pane(s)): 65656 B in memory, 4096 B on disk"), "{text}");
    assert!(
        text.contains("input queued 100 B, writer in flight 20 B, reply tier allocated 65536 B"),
        "{text}"
    );
    assert!(text.contains("teardown fallbacks 2 (1 over 50 ms)"), "{text}");
    assert!(text.contains("<=64 us: 1") && text.contains("<=262144 us: 1"), "{text}");
    // No fallback yet: the histogram line is left out.
    let quiet = QueueSection::default().lines();
    assert!(!quiet.iter().any(|line| line.contains("fallback blocked time")), "{quiet:?}");
}
