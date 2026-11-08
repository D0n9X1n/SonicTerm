//! The memory log's PTY queue line: what every pane's input and reply queues hold, memory and disk
//! apart, beside the reaper's synchronous fallback timing. Reply spill is disk, so it is reported
//! separately and never counted in the session's resident retention.

use sonicterm_io::pty::PtyQueueFootprint;

use super::reaper_fallback::FallbackTiming;

/// Every pane's queue footprint added up.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct QueueTotals {
    /// Panes with a PTY.
    pub(super) panes: u64,
    /// The summed footprint.
    pub(super) footprint: PtyQueueFootprint,
}

/// Add up `footprints`, one per pane with a PTY.
pub(super) fn sum_queues(footprints: impl IntoIterator<Item = PtyQueueFootprint>) -> QueueTotals {
    footprints.into_iter().fold(QueueTotals::default(), |mut totals, pane| {
        totals.panes += 1;
        totals.footprint.input_queued_bytes += pane.input_queued_bytes;
        totals.footprint.writer_in_flight_bytes += pane.writer_in_flight_bytes;
        totals.footprint.reply_resident_bytes += pane.reply_resident_bytes;
        totals.footprint.reply_spill_bytes += pane.reply_spill_bytes;
        totals
    })
}

impl super::App {
    /// Every pane's PTY queue footprint, summed: each read is lock-free, so this never waits.
    pub(super) fn pty_queue_totals(&self) -> QueueTotals {
        sum_queues(
            self.registry
                .windows
                .values()
                .flat_map(|window| window.panes.values())
                .filter_map(|pane| pane.pty.as_ref().map(|pty| pty.queue_footprint())),
        )
    }

    /// Log the summed PTY queue footprint and the reaper's fallback timing on the `memory` target,
    /// on the retention sampling cadence.
    pub(super) fn log_pty_queues(&self) {
        let totals = self.pty_queue_totals();
        let fallback: FallbackTiming = self.services.pty_reaper.fallback_timing();
        tracing::debug!(
            target: "memory",
            panes = totals.panes,
            resident_bytes = totals.footprint.resident_bytes(),
            input_queued_bytes = totals.footprint.input_queued_bytes,
            writer_in_flight_bytes = totals.footprint.writer_in_flight_bytes,
            reply_resident_bytes = totals.footprint.reply_resident_bytes,
            reply_spill_disk_bytes = totals.footprint.reply_spill_bytes,
            teardown_fallbacks = fallback.count,
            teardown_fallbacks_slow = fallback.slow,
            teardown_fallback_longest_us = fallback.longest_us,
            teardown_fallback_total_us = fallback.total_us,
            "pty queues"
        );
    }
}

#[cfg(test)]
#[path = "retention_queues_tests.rs"]
mod retention_queues_tests;
