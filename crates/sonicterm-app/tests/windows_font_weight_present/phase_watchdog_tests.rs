use super::*;

/// A phase that never ends is reported with its label and limit once the limit passes, so a stuck
/// render fails the probe instead of holding the test job until its platform limit.
#[test]
fn a_phase_that_never_ends_is_reported_after_its_limit() {
    let (reports, reported) = mpsc::channel();
    let watchdog = PhaseWatchdog::spawn(move |label, limit| {
        let _ = reports.send((label.to_string(), limit));
    });
    let started = Instant::now();
    watchdog.start("stuck".to_string(), Duration::from_millis(100));
    let report = reported.recv_timeout(Duration::from_secs(5)).expect("the overrun is reported");
    assert_eq!(report, ("stuck".to_string(), Duration::from_millis(100)));
    assert!(started.elapsed() >= Duration::from_millis(100), "the overrun was reported early");
}

/// Phases that end within their limits are never reported, however long they run in total.
#[test]
fn phases_that_end_in_time_are_never_reported() {
    let (reports, reported) = mpsc::channel::<String>();
    let watchdog = PhaseWatchdog::spawn(move |label, _| {
        let _ = reports.send(label.to_string());
    });
    // Twelve 100 ms phases under a 1 s limit run 1.2 s together, so only per-phase timing passes.
    for index in 0..12 {
        watchdog.start(format!("phase-{index}"), Duration::from_secs(1));
        thread::sleep(Duration::from_millis(100));
        watchdog.end();
    }
    drop(watchdog);
    assert_eq!(
        reported.recv_timeout(Duration::from_secs(5)),
        Err(RecvTimeoutError::Disconnected),
        "a phase that ended in time was reported"
    );
}

/// Dropping the watchdog mid-phase stops its thread without a report, so a probe that ends its
/// own run is never aborted.
#[test]
fn dropping_the_watchdog_mid_phase_reports_nothing() {
    let (reports, reported) = mpsc::channel::<String>();
    let watchdog = PhaseWatchdog::spawn(move |label, _| {
        let _ = reports.send(label.to_string());
    });
    watchdog.start("interrupted".to_string(), Duration::from_millis(200));
    drop(watchdog);
    assert_eq!(
        reported.recv_timeout(Duration::from_secs(5)),
        Err(RecvTimeoutError::Disconnected),
        "a dropped watchdog reported its running phase"
    );
}
