//! How a pane's VT worker reacts to a terminal reply its PTY reply spool did not admit.

use std::io;

/// What the worker does with one reply's admission result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReplyVerdict {
    /// Admitted, or refused quietly: nothing to report.
    Quiet,
    /// The backlog reached its disk budget: this reply is dropped and a warning is due, once per backlog.
    BudgetWarn,
    /// Storage or the writer failed: report once, then drop every later reply.
    Failed,
}

/// Per-pane reply delivery state, owned by the VT worker.
#[derive(Debug, Default)]
pub(super) struct ReplyDelivery {
    /// Storage or the writer failed; later replies are dropped without reports.
    failed: bool,
    /// A budget warning was logged and the backlog has not yet drained below half its budget.
    budget_warned: bool,
}

impl ReplyDelivery {
    /// Whether replies are still attempted.
    pub(super) fn accepting(&self) -> bool {
        !self.failed
    }

    /// Classify one admission result. A budget refusal drops only that reply and leaves the spool
    /// usable, so the worker keeps sending. A backlog warns once; the warning re-arms only after an
    /// admission finds the live spill below half the budget (`live_bytes` is read only then), so a
    /// reader that frees one record at a time cannot make every refusal warn.
    pub(super) fn record(
        &mut self,
        result: &io::Result<()>,
        live_bytes: impl FnOnce() -> u64,
    ) -> ReplyVerdict {
        match result {
            Ok(()) => {
                if self.budget_warned && live_bytes() < sonicterm_io::SPILL_BUDGET_BYTES / 2 {
                    // Drained below half the budget: a later refusal is a new backlog and warns.
                    self.budget_warned = false;
                }
                ReplyVerdict::Quiet
            }
            Err(error) if sonicterm_io::is_budget_refusal(error) => {
                // A backlog warns on its first drop only; later drops are counted by the spool.
                let already_warned = std::mem::replace(&mut self.budget_warned, true);
                [ReplyVerdict::BudgetWarn, ReplyVerdict::Quiet][usize::from(already_warned)]
            }
            Err(_) => {
                // Any other error means the spool latched failure or closed; stop sending.
                self.failed = true;
                ReplyVerdict::Failed
            }
        }
    }

    /// Record one admission result and log what it calls for. `stats` is read only for a budget
    /// warning; a failure during shutdown (`closing`) is expected and not logged.
    pub(super) fn report(
        &mut self,
        pane_id: u64,
        rejected_bytes: usize,
        result: &io::Result<()>,
        closing: bool,
        stats: impl Fn() -> sonicterm_io::ReplySpoolStats,
    ) {
        let verdict = self.record(result, || stats().live_bytes);
        if verdict == ReplyVerdict::BudgetWarn {
            // The backlog's first drop is logged with its counters.
            let stats = stats();
            tracing::warn!(
                pane_id,
                rejected_bytes,
                dropped = stats.dropped_replies,
                peak_live_bytes = stats.peak_live_bytes,
                "terminal replies dropped: the program is not reading its input"
            );
        }
        if let (ReplyVerdict::Failed, Err(error), false) = (verdict, result, closing) {
            // A failure outside shutdown (`closing`) is unexpected and reported.
            tracing::warn!(pane_id, rejected_bytes, %error, "terminal reply delivery failed");
        }
    }
}

#[cfg(test)]
#[path = "reply_delivery_tests.rs"]
mod reply_delivery_tests;
