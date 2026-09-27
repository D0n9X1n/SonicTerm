//! Opt-in initialization timings without owning or changing the measured work.

use std::time::Instant;

/// Whether a call merely returned or supplied an explicit Result outcome.
pub(super) enum InitOutcome {
    Returned,
    Ok,
    Error,
}

/// One unfinished operation; dropping it deliberately emits no completion.
#[must_use]
pub(super) struct InitTiming {
    operation: &'static str,
    started: Instant,
    parent: tracing::Span,
}

impl InitTiming {
    /// Record entry only when initialization timing is enabled.
    pub(super) fn begin(operation: &'static str) -> Option<Self> {
        Self::begin_with_clock(operation, Instant::now)
    }

    fn begin_with_clock(operation: &'static str, clock: impl FnOnce() -> Instant) -> Option<Self> {
        if !tracing::enabled!(target: "render_timing", tracing::Level::DEBUG) {
            // When: enabled! rejects render_timing DEBUG, skip clock access and span retention.
            return None;
        }
        let parent = tracing::Span::current();
        tracing::debug!(target: "render_timing", parent: &parent, operation, phase = "enter", "renderer initialization");
        Some(Self { operation, started: clock(), parent })
    }

    /// Record an explicit return under the operation's original parent span.
    pub(super) fn finish(timing: Option<Self>, outcome: InitOutcome) {
        let Some(timing) = timing else {
            // When: begin admitted no timing, finish must not read the clock or emit a record.
            return;
        };
        let elapsed_ms = timing.started.elapsed().as_secs_f64() * 1000.0;
        let outcome = match outcome {
            InitOutcome::Returned => "returned",
            InitOutcome::Ok => "ok",
            InitOutcome::Error => "error",
        };
        tracing::debug!(
            target: "render_timing",
            parent: &timing.parent,
            operation = timing.operation,
            phase = "return",
            outcome,
            elapsed_ms,
            "renderer initialization"
        );
    }
}

#[cfg(test)]
#[path = "init_timing_tests.rs"]
mod init_timing_tests;
