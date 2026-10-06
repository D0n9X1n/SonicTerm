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
/// presented count after it), and how many redraws did not advance it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct PresentTrace {
    /// The first presenting redraw.
    pub(crate) first: Option<(f64, u64)>,
    /// The last presenting redraw.
    pub(crate) last: Option<(f64, u64)>,
    /// Redraws that did not advance the presented count.
    pub(crate) nonpresenting: u64,
}

impl PresentTrace {
    /// Feed one main-window redraw that ended `offset_ms` after the phase's start; `presented` is the presented
    /// count after it, or None when it did not advance the count.
    pub(crate) fn observe_redraw(&mut self, offset_ms: f64, presented: Option<u64>) {
        match presented {
            Some(count) => {
                self.first.get_or_insert((offset_ms, count));
                self.last = Some((offset_ms, count));
            }
            None => self.nonpresenting += 1,
        }
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
