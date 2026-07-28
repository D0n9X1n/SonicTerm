use super::*;
use std::sync::Barrier;

#[test]
fn cancellation_is_level_triggered_for_late_observers() {
    // The defining property: a token created or checked AFTER cancellation still
    // observes it. An edge-triggered signal would lose this and strand a worker
    // that started late.
    let source = CancelSource::new();
    source.cancel(CancelReason::Requested);
    let late = source.token();
    assert!(late.is_cancelled());
    assert_eq!(late.reason(), Some(CancelReason::Requested));
    // A wait after the fact returns immediately rather than blocking forever.
    late.wait();
}

#[test]
fn first_reason_is_preserved_across_repeat_cancellation() {
    let source = CancelSource::new();
    let token = source.token();
    source.cancel(CancelReason::Timeout);
    source.cancel(CancelReason::Shutdown);
    source.cancel(CancelReason::ParentClosing);
    assert_eq!(token.reason(), Some(CancelReason::Timeout));
}

#[test]
fn uncancelled_token_reports_nothing() {
    let source = CancelSource::new();
    let token = source.token();
    assert!(!token.is_cancelled());
    assert!(!source.is_cancelled());
    assert_eq!(token.reason(), None);
}

#[test]
fn wait_until_reports_timeout_without_releasing_ownership() {
    let source = CancelSource::new();
    let token = source.token();
    let observed = token.wait_for(Duration::from_millis(20));
    assert!(!observed, "a timeout must not report cancellation");
    // The token remains usable: a timeout is an outcome, not a terminal state.
    assert!(!token.is_cancelled());
    source.cancel(CancelReason::Requested);
    assert!(token.wait_for(Duration::from_millis(50)));
}

#[test]
fn every_waiter_wakes_on_a_single_cancellation() {
    let source = CancelSource::new();
    let barrier = Arc::new(Barrier::new(5));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let token = source.token();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                token.wait();
                token.reason()
            })
        })
        .collect();
    barrier.wait();
    source.cancel(CancelReason::ParentClosing);
    for handle in handles {
        assert_eq!(handle.join().unwrap(), Some(CancelReason::ParentClosing));
    }
}

#[test]
fn token_outlives_its_source() {
    let token = {
        let source = CancelSource::new();
        let token = source.token();
        source.cancel(CancelReason::Faulted);
        token
    };
    assert!(token.is_cancelled());
    assert_eq!(token.reason(), Some(CancelReason::Faulted));
}

#[test]
fn cancellation_racing_a_waiter_is_never_missed() {
    // Publish from another thread while a waiter is entering wait_for. The
    // level-triggered flag plus the reason lock must close the gap between the
    // check and the sleep.
    for _ in 0..200 {
        let source = CancelSource::new();
        let token = source.token();
        let barrier = Arc::new(Barrier::new(2));
        let waiter_barrier = barrier.clone();
        let waiter = std::thread::spawn(move || {
            waiter_barrier.wait();
            token.wait_for(Duration::from_secs(5))
        });
        barrier.wait();
        source.cancel(CancelReason::Shutdown);
        assert!(waiter.join().unwrap(), "waiter missed a concurrent cancellation");
    }
}
