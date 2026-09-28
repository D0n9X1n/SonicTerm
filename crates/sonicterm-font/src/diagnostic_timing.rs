use std::cell::Cell;
use std::time::Instant;

thread_local! {
    static SUPPRESSED: Cell<bool> = const { Cell::new(false) };
}

struct Suppression {
    previous: bool,
}

impl Suppression {
    fn enter(suppressed: bool) -> Self {
        Self { previous: SUPPRESSED.replace(suppressed) }
    }
}

// Lifecycle: Suppression restores SUPPRESSED to its previous thread-local value on return and unwind.
impl Drop for Suppression {
    fn drop(&mut self) {
        SUPPRESSED.set(self.previous);
    }
}

/// Whether this request admits font-operation timing records.
pub(crate) fn enabled() -> bool {
    !SUPPRESSED.get() && tracing::enabled!(target: "render_timing", tracing::Level::DEBUG)
}

pub(crate) struct Timing {
    operation: &'static str,
    started: Instant,
    parent: tracing::Span,
}

impl Timing {
    /// Record entry and retain its parent only when DEBUG timing is enabled.
    pub(crate) fn begin(operation: &'static str) -> Option<Self> {
        Self::begin_with_clock(operation, Instant::now)
    }

    fn begin_with_clock(operation: &'static str, clock: impl FnOnce() -> Instant) -> Option<Self> {
        if !enabled() {
            // When: enabled rejects this request, avoid clock reads and retained diagnostic context.
            return None;
        }
        let parent = tracing::Span::current();
        tracing::debug!(target: "render_timing", parent: &parent, operation, phase = "enter", "font operation");
        Some(Self { operation, started: clock(), parent })
    }

    /// Measure a fallible operation without formatting or changing its result.
    pub(crate) fn result<T, E>(
        operation: &'static str,
        work: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let timing = Self::begin(operation);
        let result = work();
        Self::finish(timing, if result.is_ok() { "ok" } else { "error" });
        result
    }

    /// Record an explicit return using the parent retained at operation entry.
    pub(crate) fn finish(timing: Option<Self>, outcome: &'static str) {
        let Some(timing) = timing else {
            // When: timing is None, its entry was disabled and no return should be measured.
            return;
        };
        let elapsed_ms = timing.started.elapsed().as_secs_f64() * 1000.0;
        tracing::debug!(target: "render_timing", parent: &timing.parent,
            operation = timing.operation, phase = "return", outcome, elapsed_ms, "font operation");
    }
}

pub(crate) struct RequestTiming {
    dispatch: tracing::Dispatch,
    parent: tracing::Span,
    enqueued: Instant,
    request_id: usize,
}

impl RequestTiming {
    /// Capture per-request timing with an identifier allocated only when enabled.
    // Ordering: NEXT_REQUEST uses Relaxed for diagnostic identity only; it publishes no application state.
    pub(crate) fn capture_next() -> Option<Self> {
        if !enabled() {
            // When: enabled rejects this request, avoid clock reads and retained diagnostic context.
            return None;
        }
        static NEXT_REQUEST: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(1);
        Self::capture(NEXT_REQUEST.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }

    /// Capture the originating dispatcher, parent and enqueue time for one request.
    pub(crate) fn capture(request_id: usize) -> Option<Self> {
        Self::capture_with_clock(request_id, Instant::now)
    }

    fn capture_with_clock(request_id: usize, clock: impl FnOnce() -> Instant) -> Option<Self> {
        if !enabled() {
            // When: enabled rejects this request, avoid clock reads and retained diagnostic context.
            return None;
        }
        Some(Self {
            dispatch: tracing::dispatcher::get_default(Clone::clone),
            parent: tracing::Span::current(),
            enqueued: clock(),
            request_id,
        })
    }

    /// Apply the queued request's timing context without inheriting a previous worker request.
    pub(crate) fn run<T>(context: Option<Self>, work: impl FnOnce() -> T) -> T {
        let _suppression = Suppression::enter(context.is_none());
        let Some(context) = context else {
            // When: context is None, run normally without borrowing the worker's timing filter.
            return work();
        };
        tracing::dispatcher::with_default(&context.dispatch, || {
            let span = tracing::debug_span!(target: "render_timing", parent: &context.parent,
                "font_request", request_id = context.request_id);
            span.in_scope(|| {
                let elapsed_ms = context.enqueued.elapsed().as_secs_f64() * 1000.0;
                tracing::debug!(target: "render_timing", operation = "queue_wait", phase = "return",
                    outcome = "returned", elapsed_ms, "font operation");
                work()
            })
        })
    }
}

#[cfg(test)]
#[path = "diagnostic_timing_tests.rs"]
mod diagnostic_timing_tests;
