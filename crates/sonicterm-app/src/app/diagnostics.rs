//! The App's diagnostic and accounting state: counters, retention and trim samples, the
//! breadcrumb recorder and test-visible call counts. None of it decides what a window shows;
//! it records what happened, so it lives apart from window, runtime and interaction state.

/// Diagnostic and accounting state: counters, retention and trim samples, breadcrumbs.
pub(in crate::app) struct DiagnosticsState {
    /// Set by the first window or pane; the gate can no longer be forced on after it.
    pub(in crate::app) frame_counters_sealed: std::cell::Cell<bool>,
    /// Trees sent to assistive technology since start; stays 0 while none is connected.
    pub(in crate::app) accessibility_tree_builds: u64,
    /// The token the next `arm_echo_watch` issues; starts at 1, so 0 never names an arming.
    pub(in crate::app) next_echo_arm: u64,
    /// Nonblocking recorder for bounded postmortem breadcrumbs.
    ///
    /// The platform binary owns the writer thread; the app only holds this cheap
    /// sender and never performs filesystem IO on the event-loop path.
    pub(in crate::app) breadcrumb_recorder:
        Option<sonicterm_logging::breadcrumbs::BreadcrumbRecorder>,
    /// Test-only redraw request counter. Every
    /// production code path that calls `window.request_redraw()` after
    /// a `run_action` dispatch also bumps this counter in lock-step.
    /// Tests assert against this rather than the live winit window
    /// (which has no public introspection API). Stays at zero in
    /// release builds whose tests don't touch it.
    #[doc(hidden)]
    pub redraw_request_count: std::sync::atomic::AtomicUsize,
    /// Test-only counter incremented on every call to
    /// `App::reap_empty_child`. Lets tests
    /// distinguish "child window cleanup went through the unified reap
    /// contract" from "a direct `windows.remove` happened" — both would
    /// shrink the `windows` map, but only the former nulls out straggler
    /// `redraw_target`s and fires the reap trace. Stays at zero in
    /// release builds whose tests don't touch it.
    #[doc(hidden)]
    pub reap_call_count: std::sync::atomic::AtomicUsize,
    /// When pane retention was last sampled for the memory log.
    ///
    /// `None` until the first sample. Gating on elapsed time rather than
    /// sampling every idle turn keeps a measurement that walks every pane off
    /// the path that governs idle CPU.
    pub(in crate::app) last_retention_sample: Option<std::time::Instant>,
    /// Covered-window trims performed since startup; the latest trim's number.
    pub(in crate::app) trim_seq: u64,
    /// What requested the latest trim; `None` until the first one.
    pub(in crate::app) last_trim_source: Option<super::retention::TrimSource>,
    /// The preceding cycle's totals, so a snapshot can report movement.
    ///
    /// `None` until the first sample has been taken, which is what makes the
    /// first snapshot's deltas report `unavailable` rather than `+0` — the
    /// latter claims the process did not move, which is a measurement nobody
    /// made.
    ///
    /// Only the totals are retained rather than the whole snapshot: the
    /// per-renderer breakdown is a string per renderer, and holding it between
    /// samples would keep it alive for the life of the session to serve a
    /// report that never reads it.
    pub(in crate::app) last_memory_totals: Option<super::memory_snapshot::MemoryTotals>,
}
