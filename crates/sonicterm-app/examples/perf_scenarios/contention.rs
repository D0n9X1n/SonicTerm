//! S13's contention evidence: the flooding pane's progress through the measured phases and every
//! role pane's grid, which `result.json` records as `contention` for the comparison's verdict.
//!
//! The flood's liveness is observed on the flooding pane itself, not inferred from process-wide
//! byte totals: an early burst followed by a stall would satisfy a total yet leave most typing
//! samples measured against an idle parser.

use serde_json::{json, Value};

/// The longest the flooding pane may go without a new scrolled line inside a measured phase; a
/// live `yes` scrolls thousands of lines in this time, so a longer gap means the flood stalled.
pub(crate) const MAX_FLOOD_GAP_MS: u64 = 1_000;
/// How often the probe looks at the flooding pane while a measured phase runs.
pub(crate) const OBSERVE_EVERY_MS: u64 = 100;
/// The longest one look waits for the flooding pane's parser: the frame collection's own budget.
pub(crate) const OBSERVE_WAIT: std::time::Duration = std::time::Duration::from_millis(2);

/// The flooding pane's progress through the measured phases: each observation's time and the
/// pane's monotonic scrolled-line count, read only when its parser was free.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FloodWatch {
    /// `(ms since the first observation, lines scrolled)`, in order.
    observations: Vec<(u64, u64)>,
    /// Observations skipped because the parser was busy; they are not stalls.
    busy: u64,
}

impl FloodWatch {
    /// Record one observation at `elapsed_ms`; `lines` is `None` when the parser was busy.
    pub(crate) fn observe(&mut self, elapsed_ms: u64, lines: Option<u64>) {
        match lines {
            Some(lines) => self.observations.push((elapsed_ms, lines)),
            None => self.busy += 1,
        }
    }

    /// The longest interval in which the count did not advance, in ms: from the last observation
    /// that advanced (or the first) to the next that advanced, or to the last observation.
    pub(crate) fn longest_stall_ms(&self) -> Option<u64> {
        let (mut stall_start, mut last_lines) = *self.observations.first()?;
        let mut longest = 0;
        for &(at_ms, lines) in &self.observations[1..] {
            longest = longest.max(at_ms - stall_start);
            if lines > last_lines {
                // When: the pane scrolled since the last advance, the stall ends here.
                stall_start = at_ms;
                last_lines = lines;
            }
        }
        Some(longest)
    }

    /// The JSON record: observation and busy counts, total lines and the longest stall.
    pub(crate) fn to_json(&self) -> Value {
        let lines = match (self.observations.first(), self.observations.last()) {
            (Some(first), Some(last)) => last.1.saturating_sub(first.1),
            _ => 0,
        };
        json!({
            "observations": self.observations.len(),
            "busy": self.busy,
            "lines": lines,
            "longest_stall_ms": self.longest_stall_ms(),
        })
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
