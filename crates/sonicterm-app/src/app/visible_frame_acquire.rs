//! Bounded parser acquisition against the frame collection deadline.

use super::*;

/// Take one visible parser under the collection's single absolute `deadline`, or the kind of miss.
///
/// The deadline is checked before the probe, again after a failed probe, and after either path
/// succeeds; a guard obtained after the deadline is dropped and counts as a miss. The kind comes
/// from the same clock read that decided it: a deadline already passed before the probe is
/// [`MissKind::BudgetExpired`] (this pane was never offered time), anything later is
/// [`MissKind::Parser`].
pub(super) fn acquire_parser<'a>(
    acquire: &impl ParserAcquire,
    pane_id: u64,
    parser: &'a Mutex<Parser>,
    deadline: Instant,
) -> Result<MutexGuard<'a, Parser>, MissKind> {
    if acquire.now() >= deadline {
        // When: the deadline passed on an earlier pane's wait, no further acquisition is attempted.
        return Err(MissKind::BudgetExpired);
    }
    let guard = match acquire.probe(pane_id, parser) {
        Some(guard) => guard,
        None => {
            // When: the probe found the parser busy, the collection queues on it until the deadline.
            acquire.after_failed_probe(pane_id);
            if acquire.now() >= deadline {
                // When: the deadline passed after the failed probe, the timed wait is not entered.
                return Err(MissKind::Parser);
            }
            acquire.wait_until(pane_id, parser, deadline).ok_or(MissKind::Parser)?
        }
    };
    if acquire.now() >= deadline {
        // When: the guard arrived after the deadline, it is released and reported as contention.
        drop(guard);
        return Err(MissKind::Parser);
    }
    Ok(guard)
}

#[cfg(test)]
#[path = "visible_frame_acquire_tests.rs"]
mod visible_frame_acquire_tests;
