//! The Memory Usage report's PTY QUEUES section: what every pane's input and reply queues hold, in
//! memory and on disk, and how long the reaper's synchronous teardown fallback has blocked.

use super::reaper_fallback::{FallbackTiming, FALLBACK_BUCKETS_US};
use super::retention_queues::QueueTotals;

/// The figures the PTY QUEUES section shows, sampled with the rest of the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct QueueSection {
    /// Every pane's queue footprint, summed.
    pub(crate) totals: QueueTotals,
    /// The reaper's synchronous fallback timing so far.
    pub(crate) fallback: FallbackTiming,
    /// The process-wide spill pool: file bytes charged now, and its budget.
    pub(crate) spill_files: (u64, u64),
}

impl QueueSection {
    /// The section's lines, to follow the panes.
    pub(crate) fn lines(&self) -> Vec<String> {
        let queues = self.totals.footprint;
        let fallback = self.fallback;
        let mut lines = vec![
            format!(
                "PTY QUEUES ({} pane(s)): {} B in memory, {} B on disk",
                self.totals.panes,
                queues.resident_bytes(),
                queues.reply_spill_bytes
            ),
            format!(
                "  input queued {} B, writer in flight {} B, reply tier allocated {} B, reply spill {} B (disk)",
                queues.input_queued_bytes,
                queues.writer_in_flight_bytes,
                queues.reply_resident_bytes,
                queues.reply_spill_bytes
            ),
            format!(
                "  reply spill files {} B of the {} B process budget (file length, consumed prefixes included)",
                self.spill_files.0, self.spill_files.1
            ),
            format!(
                "  teardown fallbacks {} ({} over 50 ms), longest {} us, total {} us",
                fallback.count, fallback.slow, fallback.longest_us, fallback.total_us
            ),
        ];
        if fallback.count > 0 {
            // Once any fallback ran, its blocked-time buckets are listed (empty ones left out).
            let buckets: Vec<String> = fallback
                .histogram
                .iter()
                .enumerate()
                .filter(|(_, count)| **count > 0)
                .map(|(index, count)| match FALLBACK_BUCKETS_US.get(index) {
                    Some(bound) => format!("<={bound} us: {count}"),
                    None => format!(
                        ">{} us: {count}",
                        FALLBACK_BUCKETS_US[FALLBACK_BUCKETS_US.len() - 1]
                    ),
                })
                .collect();
            lines.push(format!("  fallback blocked time: {}", buckets.join(", ")));
        }
        lines
    }
}

#[cfg(test)]
#[path = "memory_report_queues_tests.rs"]
mod memory_report_queues_tests;
