//! Pins the scan throttle: one trailing check after App activity, none from the harness's own wakes.

use super::*;

const INTERVAL: Duration = Duration::from_millis(20);

/// `start` plus `millis` milliseconds.
fn after(start: Instant, millis: u64) -> Instant {
    start + Duration::from_millis(millis)
}

#[test]
fn first_app_dispatch_scans_at_once() {
    // Nothing has been scanned yet, so the first dispatch the App receives scans immediately.
    let start = Instant::now();
    let mut throttle = ScanThrottle::new(INTERVAL);
    assert!(throttle.observe(start, true, ScanTrigger::App));
    assert_eq!(throttle.trailing(), None);
}

#[test]
fn throttled_app_dispatch_arms_one_trailing_check_an_interval_after_the_scan() {
    // Output that arrives inside the interval is checked once, an interval after the last scan.
    let start = Instant::now();
    let mut throttle = ScanThrottle::new(INTERVAL);
    assert!(throttle.observe(start, true, ScanTrigger::App));
    assert!(!throttle.observe(after(start, 5), true, ScanTrigger::App));
    assert_eq!(throttle.trailing(), Some(after(start, 20)));
    // A second throttled dispatch keeps the same check rather than pushing it later.
    assert!(!throttle.observe(after(start, 12), true, ScanTrigger::App));
    assert_eq!(throttle.trailing(), Some(after(start, 20)));
}

#[test]
fn trailing_check_runs_once_and_its_own_wake_arms_nothing() {
    // The harness timer wakes the loop for the check: new_events scans, then the same turn's
    // about_to_wait is throttled and must not arm another check.
    let start = Instant::now();
    let mut throttle = ScanThrottle::new(INTERVAL);
    assert!(throttle.observe(start, true, ScanTrigger::App));
    assert!(!throttle.observe(after(start, 5), true, ScanTrigger::App));
    assert!(throttle.observe(after(start, 20), true, ScanTrigger::Harness));
    assert_eq!(throttle.trailing(), None);
    assert!(!throttle.observe(after(start, 21), true, ScanTrigger::Harness));
    assert_eq!(throttle.trailing(), None, "the harness's own wake armed another check");
}

#[test]
fn idle_app_gets_no_periodic_wakes_while_a_scan_is_wanted() {
    // A sentinel that never arrives keeps a scan wanted. With the App idle, the only wakes are
    // the throttle's own: follow each one as the event loop would, for one second.
    let start = Instant::now();
    let mut throttle = ScanThrottle::new(INTERVAL);
    assert!(throttle.observe(start, true, ScanTrigger::App));
    assert!(!throttle.observe(after(start, 5), true, ScanTrigger::App));
    let horizon = after(start, 1_000);
    let mut wakes = 0_u32;
    while let Some(due) = throttle.trailing().filter(|due| *due <= horizon) {
        wakes += 1;
        // The wake's loop turn: new_events at the due time, then about_to_wait just after.
        throttle.observe(due, true, ScanTrigger::Harness);
        throttle.observe(due + Duration::from_millis(1), true, ScanTrigger::Harness);
    }
    assert_eq!(wakes, 1, "the trailing check re-armed itself while the App was idle");
}

#[test]
fn harness_turn_scans_when_the_interval_allows_but_never_arms_a_check() {
    // The probe is awake anyway, so a loop turn or an injection may scan; a throttled one leaves
    // nothing behind, because new output always reaches the App as a dispatch of its own.
    let start = Instant::now();
    let mut throttle = ScanThrottle::new(INTERVAL);
    assert!(throttle.observe(start, true, ScanTrigger::Harness));
    assert!(!throttle.observe(after(start, 5), true, ScanTrigger::Harness));
    assert_eq!(throttle.trailing(), None);
    assert!(throttle.observe(after(start, 25), true, ScanTrigger::Harness));
}

#[test]
fn app_activity_after_a_trailing_check_arms_a_new_one() {
    // Only the harness's own wakes stay quiet: later output still gets its trailing check.
    let start = Instant::now();
    let mut throttle = ScanThrottle::new(INTERVAL);
    assert!(throttle.observe(start, true, ScanTrigger::App));
    assert!(!throttle.observe(after(start, 5), true, ScanTrigger::App));
    assert!(throttle.observe(after(start, 20), true, ScanTrigger::Harness));
    assert!(!throttle.observe(after(start, 30), true, ScanTrigger::App));
    assert_eq!(throttle.trailing(), Some(after(start, 40)));
}

#[test]
fn nothing_awaited_drops_the_trailing_check() {
    // Once the sentinel, prompt or image is found, no scan runs and no check stays armed.
    let start = Instant::now();
    let mut throttle = ScanThrottle::new(INTERVAL);
    assert!(throttle.observe(start, true, ScanTrigger::App));
    assert!(!throttle.observe(after(start, 5), true, ScanTrigger::App));
    assert!(!throttle.observe(after(start, 10), false, ScanTrigger::App));
    assert_eq!(throttle.trailing(), None);
    assert!(!throttle.observe(after(start, 30), false, ScanTrigger::App));
}

#[test]
fn early_wake_keeps_the_trailing_check_armed() {
    // A wake before the check is due, an App timer say, neither scans nor drops the check.
    let start = Instant::now();
    let mut throttle = ScanThrottle::new(INTERVAL);
    assert!(throttle.observe(start, true, ScanTrigger::App));
    assert!(!throttle.observe(after(start, 5), true, ScanTrigger::App));
    assert!(!throttle.observe(after(start, 15), true, ScanTrigger::Harness));
    assert_eq!(throttle.trailing(), Some(after(start, 20)));
}
