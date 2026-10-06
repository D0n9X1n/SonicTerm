//! The S10 attribution watch: per-present records a perf harness arms for one pane. Each presented
//! main-window attempt emits one `sonic::perf_present` WARN line per watched pane in the frame,
//! carrying the update identity, cursor-relative markers and admission copied under the frame's
//! guards. A failed or retried attempt emits nothing.

use std::time::Instant;

use sonicterm_grid::grid::Grid;
use sonicterm_vt::vt::{Parser, SyncState};

use super::sync_frame::SyncAdmission;

/// The longest sentinel or prompt a watch accepts, in bytes.
const MARKER_MAX_BYTES: usize = 128;
/// Rows above the cursor's row a marker scan reaches, on every host: cmd.exe's banner needs 8.
const MARKER_ROWS_ABOVE: u16 = 8;
/// The log target every attribution line uses.
pub(super) const PERF_PRESENT_TARGET: &str = "sonic::perf_present";

impl super::App {
    /// Arm per-present S10 attribution for pane `pane_id`. `nonce` is the sentinel line bound to the
    /// run's nonce, exactly as the workload prints it, and `prompt` the shell's prompt; each is at
    /// most 128 bytes and not empty. `updates` is the phase's logical update count, which bounds the
    /// watch at `4 × updates + 64` lines. Returns the arming id every line carries; re-arming a pane
    /// replaces its watch. `None` when the frame-counter gate is off, the pane does not exist, a
    /// marker is empty or too long, `updates` is 0 or the arming ids are exhausted.
    #[doc(hidden)]
    pub fn arm_s10_attribution(
        &mut self,
        pane_id: u64,
        nonce: &str,
        prompt: &str,
        updates: u32,
    ) -> Option<u64> {
        let usable = |marker: &str| !marker.is_empty() && marker.len() <= MARKER_MAX_BYTES;
        if !usable(nonce) || !usable(prompt) || updates == 0 || self.find_pane(pane_id).is_none() {
            // When: `usable` rejects a marker, `updates` is 0 or `find_pane` finds no `pane_id`, nothing is armed.
            return None;
        }
        let counters = self.frame_counters.as_mut()?;
        let arming = counters.next_s10_arming;
        counters.next_s10_arming = arming.checked_add(1)?;
        let watch = S10Watch::new(arming, nonce, prompt, updates);
        counters.s10_watches.insert(pane_id, watch);
        Some(arming)
    }

    /// Disarm pane `pane_id`'s S10 attribution watch. Lines already emitted stay in the log; there is
    /// nothing to drain. Disarming a pane with no watch does nothing.
    #[doc(hidden)]
    pub fn disarm_s10_attribution(&mut self, pane_id: u64) {
        if let Some(counters) = self.frame_counters.as_mut() {
            counters.s10_watches.remove(&pane_id);
        }
    }

    /// The candidate records of main window `id`'s admitted attempt at `now`, one per watched pane in
    /// `guarded`, copied while the frame's parser guards are held. Empty, and nothing is read, when no
    /// watch is armed.
    pub(super) fn s10_candidates<'guard>(
        &self,
        id: winit::window::WindowId,
        guarded: impl Iterator<Item = (u64, &'guard Parser)>,
        now: Instant,
    ) -> Vec<S10Candidate> {
        let Some(watches) = self
            .frame_counters
            .as_ref()
            .map(|counters| &counters.s10_watches)
            .filter(|watches| !watches.is_empty())
        else {
            // When: `frame_counters` is None or its `watches` is empty, the attempt records nothing.
            return Vec::new();
        };
        let Some(window) = self.windows.get(&id) else {
            // When: `self.windows` no longer holds `id`, the window closed and has no frame to record.
            return Vec::new();
        };
        guarded
            .filter_map(|(pane_id, parser)| {
                let watch = watches.get(&pane_id)?;
                let state = parser.synchronized_output();
                let admission = window.sync_admission(pane_id, state, now)?;
                let (sentinel, prompt) = watch.markers(parser.grid());
                Some(S10Candidate { pane_id, state, sentinel, prompt, admission })
            })
            .collect()
    }

    /// Emit one line per candidate of a presented attempt whose window sequence is `presented_seq`;
    /// an attempt that did not present (`None`) emits nothing and drops its candidates.
    pub(super) fn commit_s10_candidates(
        &mut self,
        candidates: Vec<S10Candidate>,
        presented_seq: Option<u64>,
    ) {
        let (Some(seq), Some(counters)) = (presented_seq, self.frame_counters.as_mut()) else {
            // When: `presented_seq` or `frame_counters` is None, the attempt's candidates are dropped.
            return;
        };
        for candidate in candidates {
            if let Some(line) = counters
                .s10_watches
                .get_mut(&candidate.pane_id)
                .and_then(|watch| watch.admit(&candidate))
            {
                line.emit(seq);
            }
        }
    }
}

/// One pane's armed watch: its arming, owned markers (at most 128 bytes each) and line budget.
#[derive(Debug)]
pub(crate) struct S10Watch {
    arming: u64,
    sentinel: String,
    prompt: String,
    /// Present lines this arming may emit before its one `overflow` line.
    capacity: u64,
    emitted: u64,
    overflowed: bool,
}

impl S10Watch {
    /// A watch of arming `arming` for a phase of `updates` updates: `4 × updates + 64` lines.
    fn new(arming: u64, sentinel: &str, prompt: &str, updates: u32) -> Self {
        Self {
            arming,
            sentinel: sentinel.to_owned(),
            prompt: prompt.to_owned(),
            capacity: 4 * u64::from(updates) + 64,
            emitted: 0,
            overflowed: false,
        }
    }

    /// Whether `grid` shows the sentinel and the prompt, from the cursor's row up to 8 rows above it.
    fn markers(&self, grid: &Grid) -> (bool, bool) {
        (
            line_near_cursor(grid, &self.sentinel, MARKER_ROWS_ABOVE),
            line_near_cursor(grid, &self.prompt, MARKER_ROWS_ABOVE),
        )
    }

    /// The line `candidate` emits under this watch: a present line within capacity, then one overflow
    /// line, then nothing.
    fn admit(&mut self, candidate: &S10Candidate) -> Option<S10Line> {
        if self.overflowed {
            // When: `overflowed` is set, the overflow line was emitted and the arming emits nothing more.
            return None;
        }
        if self.emitted == self.capacity {
            // When: `emitted` reached `capacity`, one overflow line ends the arming's output.
            self.overflowed = true;
            return Some(S10Line::Overflow {
                arming: self.arming,
                pane_id: candidate.pane_id,
                emitted: self.emitted,
            });
        }
        self.emitted += 1;
        Some(S10Line::Present { arming: self.arming, candidate: *candidate })
    }
}

/// Whether a visible row from the cursor's row up to `rows_above` rows above it starts with `text`;
/// each row compares at most `text`'s character count of columns.
pub(super) fn line_near_cursor(grid: &Grid, text: &str, rows_above: u16) -> bool {
    let cursor_row = grid.cursor.row.min(grid.rows.saturating_sub(1));
    (cursor_row.saturating_sub(rows_above)..=cursor_row).any(|row| {
        let line = grid.row(row);
        text.chars()
            .enumerate()
            .all(|(column, expected)| line.get(column).is_some_and(|cell| cell.ch == expected))
    })
}

/// One watched pane's record of an admitted attempt, copied under its parser guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct S10Candidate {
    pane_id: u64,
    state: SyncState,
    sentinel: bool,
    prompt: bool,
    admission: SyncAdmission,
}

/// A line a presented attempt emits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum S10Line {
    /// One watched pane's record of the presented frame.
    Present { arming: u64, candidate: S10Candidate },
    /// The arming's capacity was spent; it emits nothing more.
    Overflow { arming: u64, pane_id: u64, emitted: u64 },
}

impl S10Line {
    /// Write the line at WARN on `sonic::perf_present`, naming the main window's presented sequence `seq`.
    fn emit(self, seq: u64) {
        match self {
            Self::Present { arming, candidate } => tracing::warn!(
                target: PERF_PRESENT_TARGET,
                kind = "present",
                arming,
                seq,
                window = "main",
                pane = candidate.pane_id,
                resets = candidate.state.resets,
                epoch = candidate.state.epoch,
                set = candidate.state.set,
                sentinel = candidate.sentinel,
                prompt = candidate.prompt,
                admission = candidate.admission.name(),
                "perf_present"
            ),
            Self::Overflow { arming, pane_id, emitted } => tracing::warn!(
                target: PERF_PRESENT_TARGET,
                kind = "overflow",
                arming,
                seq,
                window = "main",
                pane = pane_id,
                emitted,
                "perf_present"
            ),
        }
    }
}

#[cfg(test)]
#[path = "perf_present_tests.rs"]
mod perf_present_tests;
