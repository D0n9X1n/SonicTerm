//! S13's contention evidence: the flooding pane's progress through the measured phases and every
//! role pane's grid, which `result.json` records as `contention` for the comparison's verdict.
//!
//! The flood's liveness is observed on the flooding pane itself, not inferred from process-wide
//! byte totals: an early burst followed by a stall would satisfy a total yet leave most typing
//! samples measured against an idle parser. Progress is accounted over each measured phase's whole
//! span, from its start to its end: a stretch with no reading that showed new lines (before the
//! first, between two, after the last, or covered only by busy reads) counts against it.

use serde_json::{json, Value};

/// The longest stretch of a measured phase that may pass without a reading showing new lines; a
/// live `yes` scrolls thousands of lines in this time, so a longer stretch means the flood stalled
/// or went unobserved.
pub(crate) const MAX_FLOOD_GAP_MS: u64 = 1_000;
/// How often the probe looks at the flooding pane while a measured phase runs.
pub(crate) const OBSERVE_EVERY_MS: u64 = 100;
/// The longest one look waits for the flooding pane's parser: the frame collection's own budget.
/// Read only by the probe, which runs where the harness measures (macOS and Windows).
#[cfg(any(target_os = "macos", windows))]
pub(crate) const OBSERVE_WAIT: std::time::Duration = std::time::Duration::from_millis(2);

/// One measured phase's flood progress, in ms since the phase began.
#[derive(Clone, Debug, PartialEq)]
struct PhaseWatch {
    /// The phase's name.
    name: &'static str,
    /// Each look's time and the scrolled-line count it read, `None` when the parser was busy.
    looks: Vec<(u64, Option<u64>)>,
    /// When the phase ended; `None` while it runs.
    ended_ms: Option<u64>,
}

impl PhaseWatch {
    /// The longest stretch from the phase's start to its end with no reading that advanced: the
    /// first reading only sets the baseline, so the stretch before the first advance counts too.
    fn longest_gap_ms(&self) -> Option<u64> {
        let end_ms = self.ended_ms?;
        let mut last_progress_ms = 0;
        let mut last_lines: Option<u64> = None;
        let mut longest = 0;
        for &(at_ms, lines) in &self.looks {
            let Some(lines) = lines else { continue };
            if last_lines.is_some_and(|previous| lines > previous) {
                // When: this reading shows new lines since the last one, the stretch ends here.
                longest = longest.max(at_ms - last_progress_ms);
                last_progress_ms = at_ms;
            }
            last_lines = Some(lines);
        }
        Some(longest.max(end_ms.saturating_sub(last_progress_ms)))
    }
}

/// The flooding pane's progress through every measured phase.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FloodWatch {
    phases: Vec<PhaseWatch>,
}

impl FloodWatch {
    /// A measured phase named `name` begins; later looks belong to it until [`Self::end`].
    pub(crate) fn begin(&mut self, name: &'static str) {
        self.phases.push(PhaseWatch { name, looks: Vec::new(), ended_ms: None });
    }

    /// Record one look at `at_ms` into the running phase; `lines` is `None` when the parser was busy.
    pub(crate) fn observe(&mut self, at_ms: u64, lines: Option<u64>) {
        if let Some(phase) = self.phases.last_mut().filter(|phase| phase.ended_ms.is_none()) {
            phase.looks.push((at_ms, lines));
        }
    }

    /// The running phase ends at `at_ms`.
    pub(crate) fn end(&mut self, at_ms: u64) {
        if let Some(phase) = self.phases.last_mut().filter(|phase| phase.ended_ms.is_none()) {
            phase.ended_ms = Some(at_ms);
        }
    }

    /// The longest unprogressed stretch over every ended phase; `None` before any phase ended.
    pub(crate) fn longest_gap_ms(&self) -> Option<u64> {
        self.phases.iter().filter_map(PhaseWatch::longest_gap_ms).max()
    }

    /// The JSON record: per phase, its length, looks, busy looks and longest unprogressed stretch.
    pub(crate) fn to_json(&self) -> Value {
        let phases: Vec<Value> = self
            .phases
            .iter()
            .map(|phase| {
                let busy = phase.looks.iter().filter(|(_, lines)| lines.is_none()).count();
                json!({
                    "name": phase.name,
                    "duration_ms": phase.ended_ms,
                    "looks": phase.looks.len(),
                    "busy": busy,
                    "longest_gap_ms": phase.longest_gap_ms(),
                })
            })
            .collect();
        json!({ "phases": phases, "longest_gap_ms": self.longest_gap_ms() })
    }
}

/// `result.json`'s `contention`: each role pane's grid as `[cols, rows]`, and the flood's progress.
pub(crate) fn record(grids: &[Option<(u16, u16)>], flood: &FloodWatch) -> Value {
    let grids: Vec<Value> = grids
        .iter()
        .map(|grid| grid.map_or(Value::Null, |(cols, rows)| json!([cols, rows])))
        .collect();
    json!({
        "role_grids": grids,
        "flood": flood.to_json(),
        "max_flood_gap_ms": MAX_FLOOD_GAP_MS,
        "observe_every_ms": OBSERVE_EVERY_MS,
    })
}

#[cfg(test)]
#[path = "contention_tests.rs"]
mod contention_tests;
