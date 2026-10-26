//! A transition phase's completion: the instant its endpoint was latched, measured from the
//! phase's start, or the reason it has none. A missing endpoint stays missing, never zero.

use std::time::Instant;

/// The run's deadline passed before the endpoint was reached.
pub(crate) const EXPIRED: &str = "expired";
/// The run ended for another reason before the endpoint was reached.
pub(crate) const INCOMPLETE: &str = "incomplete";

/// A transition's completion in ms from its phase's start, or why it has none; both absent
/// for a hold or sustained phase.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Completion {
    /// Milliseconds from the phase's start to its latched endpoint.
    pub(crate) elapsed_ms: Option<f64>,
    /// Why the endpoint was not reached.
    pub(crate) missing: Option<&'static str>,
}

/// The completion of a phase that started at `started`, whose endpoint was latched at
/// `endpoint_at`, in a run whose deadline is `deadline`, finalized at `now`. Only an endpoint
/// before the deadline counts; `now` never moves a latched one.
pub(crate) fn completion(
    started: Instant,
    endpoint_at: Option<Instant>,
    deadline: Instant,
    now: Instant,
    unreached: &'static str,
) -> Completion {
    match endpoint_at {
        Some(reached) if reached < deadline => Completion {
            elapsed_ms: Some(reached.saturating_duration_since(started).as_secs_f64() * 1_000.0),
            missing: None,
        },
        _ if now >= deadline => Completion { elapsed_ms: None, missing: Some(EXPIRED) },
        _ => Completion { elapsed_ms: None, missing: Some(unreached) },
    }
}

/// What a phase that presented no frame records instead of its first and last presentation.
pub(crate) const NO_PRESENTATION: &str = "no-presentation";

/// A phase's presentation trace from its main-window `RedrawRequested` dispatches: the first and last that
/// advanced the main renderer's presented count, each as (ms from the phase's start to the dispatch's end, the
/// presented count after it), and how many redraws did not advance it. A phase that releases the roles also
/// records, as diagnostics only, when their first output after GO arrived and the first frame after it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct PresentTrace {
    /// The first presenting redraw.
    pub(crate) first: Option<(f64, u64)>,
    /// The last presenting redraw.
    pub(crate) last: Option<(f64, u64)>,
    /// Redraws that did not advance the presented count.
    pub(crate) nonpresenting: u64,
    /// `None` when the phase records no output diagnostics; `Some(None)` while the roles' first output after
    /// GO is awaited; `Some(Some(ms))` once it arrived, `ms` after the phase's start.
    output: Option<Option<f64>>,
    /// The first frame presented after that output, in ms after it.
    after_output: Option<f64>,
    /// How long before the phase's start GO was written, in ms.
    go_lead_ms: f64,
}

impl PresentTrace {
    /// Feed one main-window redraw that ended `offset_ms` after the phase's start; `presented` is the presented
    /// count after it, or None when it did not advance the count.
    pub(crate) fn observe_redraw(&mut self, offset_ms: f64, presented: Option<u64>) {
        match presented {
            Some(count) => {
                self.first.get_or_insert((offset_ms, count));
                self.last = Some((offset_ms, count));
                if let Some(Some(output_ms)) = self.output {
                    // A frame presented once the roles' first output was noted; the first one times the diagnostic.
                    self.after_output.get_or_insert((offset_ms - output_ms).max(0.0));
                }
            }
            None => self.nonpresenting += 1,
        }
    }

    /// Record the roles' first output after GO, written `go_lead_ms` before the phase started, as diagnostics;
    /// the phase's first present is unchanged.
    pub(crate) fn await_output(&mut self, go_lead_ms: f64) {
        self.output = Some(None);
        self.go_lead_ms = go_lead_ms;
    }

    /// Whether the phase still awaits its roles' first output after GO.
    pub(crate) fn awaits_output(&self) -> bool {
        self.output == Some(None)
    }

    /// The roles' first output after GO arrived `offset_ms` after the phase's start; the first frame presented
    /// after it is timed from it.
    pub(crate) fn note_output(&mut self, offset_ms: f64) {
        if self.awaits_output() {
            // When: the output was awaited, this is the first one, so the clock starts here.
            self.output = Some(Some(offset_ms));
        }
    }

    /// The output diagnostics: ms from GO to the roles' first output, and from that output to the first frame
    /// presented after it. Either is `None` when the phase never saw it; neither is ever a measured metric.
    pub(crate) fn output_diagnostics(&self) -> (Option<f64>, Option<f64>) {
        let go_to_output = self.output.flatten().map(|output_ms| output_ms + self.go_lead_ms);
        (go_to_output, self.after_output)
    }

    /// Why the phase records no presentation: `no-presentation` when no redraw presented, else None.
    pub(crate) fn missing(&self) -> Option<&'static str> {
        self.first.is_none().then_some(NO_PRESENTATION)
    }
}

/// The latest first-observed sentinel instant among `roles`, or None until every role's
/// sentinel was seen.
pub(crate) fn latest_sentinel(seen: &[Option<Instant>], roles: &[usize]) -> Option<Instant> {
    let instants: Option<Vec<Instant>> =
        roles.iter().map(|role| seen.get(*role).copied().flatten()).collect();
    instants?.into_iter().max()
}

#[cfg(test)]
#[path = "transition_tests.rs"]
mod transition_tests;
