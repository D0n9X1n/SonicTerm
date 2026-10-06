use std::time::Duration;

use super::*;

/// `start` plus `offset_ms` milliseconds.
fn after(start: Instant, offset_ms: u64) -> Instant {
    start + Duration::from_millis(offset_ms)
}

/// An endpoint latched before the deadline is the completion, measured from the phase's start;
/// a later finalization (`now`) never moves it.
#[test]
fn a_latched_endpoint_is_the_completion_however_late_the_phase_finalizes() {
    let start = Instant::now();
    let deadline = after(start, 10_000);
    for now in [after(start, 300), after(start, 9_000), after(start, 20_000)] {
        let reached = completion(start, Some(after(start, 250)), deadline, now, INCOMPLETE);
        assert_eq!(reached, Completion { elapsed_ms: Some(250.0), missing: None }, "{now:?}");
    }
}

/// An endpoint at or after the deadline is not eligible, and with no endpoint the deadline
/// decides: `expired` once it passed, else `incomplete`. None is zero.
#[test]
fn a_missing_endpoint_is_expired_or_unreached_and_never_zero() {
    let start = Instant::now();
    let deadline = after(start, 1_000);
    let late = completion(start, Some(deadline), deadline, after(start, 1_500), INCOMPLETE);
    assert_eq!(late, Completion { elapsed_ms: None, missing: Some(EXPIRED) });
    let expired = completion(start, None, deadline, deadline, INCOMPLETE);
    assert_eq!(expired, Completion { elapsed_ms: None, missing: Some(EXPIRED) });
    let early = completion(start, None, deadline, after(start, 400), INCOMPLETE);
    assert_eq!(early, Completion { elapsed_ms: None, missing: Some(INCOMPLETE) });
}

/// The sentinel endpoint is the latest first observation among the required roles; a role not
/// yet seen leaves it missing, and roles not required are ignored.
#[test]
fn the_sentinel_endpoint_is_the_latest_required_first_observation() {
    let start = Instant::now();
    let seen = [Some(after(start, 30)), Some(after(start, 90)), None, Some(after(start, 500))];
    assert_eq!(latest_sentinel(&seen, &[0, 1]), Some(after(start, 90)));
    assert_eq!(latest_sentinel(&seen, &[1, 0]), Some(after(start, 90)));
    assert_eq!(latest_sentinel(&seen, &[0, 2]), None, "role 2 not seen yet");
    assert_eq!(latest_sentinel(&seen, &[]), None, "no required role");
}

/// A presentation trace keeps the first and the last redraw that presented, each with its offset
/// and presented count, and counts every redraw that did not present, wherever it falls.
#[test]
fn a_presentation_trace_keeps_the_first_and_last_presenting_redraw_and_counts_the_rest() {
    let mut trace = PresentTrace::default();
    trace.observe_redraw(1.0, None);
    trace.observe_redraw(2.0, Some(5));
    trace.observe_redraw(3.0, None);
    trace.observe_redraw(4.0, Some(6));
    trace.observe_redraw(7.5, Some(9));
    assert_eq!(
        trace,
        PresentTrace { first: Some((2.0, 5)), last: Some((7.5, 9)), nonpresenting: 2 }
    );
    assert_eq!(trace.missing(), None);
}

/// A phase whose redraws never presented records `no-presentation` and no first or last.
#[test]
fn a_trace_without_a_presenting_redraw_is_missing_as_no_presentation() {
    let mut trace = PresentTrace::default();
    assert_eq!(trace.missing(), Some(NO_PRESENTATION));
    trace.observe_redraw(1.0, None);
    assert_eq!((trace.first, trace.last, trace.nonpresenting), (None, None, 1));
    assert_eq!(trace.missing(), Some("no-presentation"));
}
