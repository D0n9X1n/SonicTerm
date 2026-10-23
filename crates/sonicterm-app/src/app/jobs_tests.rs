use super::*;

/// The fake clock's start, far enough from process start that subtraction never underflows.
fn origin() -> Instant {
    Instant::now() + Duration::from_secs(3600)
}

fn millis(value: u64) -> Duration {
    Duration::from_millis(value)
}

/// A guard stamps its slot and clears it when dropped, including on unwind.
#[test]
fn a_busy_slot_is_cleared_however_its_call_ends() {
    let slot = BusySlot::new(Instant::now());
    assert_eq!(slot.started_at(), None);
    {
        let _guard = slot.enter();
        assert!(slot.started_at().is_some());
    }
    assert_eq!(slot.started_at(), None);
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = slot.enter();
        panic!("classify panicked");
    }));
    assert!(panicked.is_err());
    assert_eq!(slot.started_at(), None, "an unwind clears the slot");
    let busy = PathBusy::new();
    let _probe = busy.probe.enter();
    std::thread::sleep(millis(2));
    let _open = busy.open.enter();
    assert_eq!(busy.oldest(), busy.probe.started_at(), "the oldest running call");
}

/// 299 ms of work shows nothing and 301 ms shows; after the work ends at 400 ms the display stays
/// until 1,400 ms; a call that starts during the hold continues without a hide; the warning
/// starts at `warn_after_secs`; every wake is after `now`.
#[test]
fn the_hold_rule_shows_holds_continues_and_warns() {
    let settings = PathCheckSettings::default();
    let start = origin();
    let mut hold = PathCheckHold::default();
    let (shown, wake) = hold.step(Some(start), 0, start + millis(299), settings);
    assert_eq!((shown, wake), (None, Some(start + millis(300))));
    let (shown, wake) = hold.step(Some(start), 0, start + millis(301), settings);
    assert_eq!(shown, Some(PathCheckInput { secs: 0, warn: false, queued: 0 }));
    assert_eq!(wake, Some(start + millis(1000)));
    let (shown, wake) = hold.step(None, 0, start + millis(400), settings);
    assert!(shown.is_some());
    assert_eq!(wake, Some(start + millis(1400)));
    let (shown, _) = hold.step(None, 0, start + millis(1399), settings);
    assert!(shown.is_some(), "still held");
    let restarted = start + millis(1000);
    let (shown, _) = hold.step(Some(restarted), 0, start + millis(1050), settings);
    assert_eq!(
        shown,
        Some(PathCheckInput { secs: 0, warn: false, queued: 0 }),
        "continues, no hide"
    );
    let (shown, _) = hold.step(None, 0, start + millis(1100), settings);
    assert!(shown.is_some());
    let (shown, wake) = hold.step(None, 0, start + millis(2100), settings);
    assert_eq!((shown, wake), (None, None), "hidden after the hold");
    let mut hung = PathCheckHold::default();
    let (shown, wake) = hung.step(Some(start), 0, start + Duration::from_secs(10), settings);
    assert_eq!(shown, Some(PathCheckInput { secs: 10, warn: true, queued: 0 }));
    assert!(wake.unwrap() > start + Duration::from_secs(10));
}

/// A submit arms one recheck at `show_after_ms`, kept unchanged by later submits; once it fires
/// with nothing running or queued, no wake is demanded.
#[test]
fn a_submit_rechecks_once_at_its_threshold() {
    let settings = PathCheckSettings::default();
    let start = origin();
    let mut hold = PathCheckHold::default();
    hold.note_submit(start, settings);
    hold.note_submit(start + millis(100), settings);
    assert_eq!(
        hold.step(None, 0, start + millis(150), settings),
        (None, Some(start + millis(300)))
    );
    assert_eq!(hold.step(None, 0, start + millis(300), settings), (None, None));
    // Evaluated late, past several transitions at once, the state lands on the right one.
    let mut late = PathCheckHold::default();
    let (shown, wake) = late.step(Some(start), 0, start + millis(5_500), settings);
    assert_eq!(shown, Some(PathCheckInput { secs: 5, warn: false, queued: 0 }));
    assert_eq!(wake, Some(start + Duration::from_secs(6)));
}

/// Several queued checks keep the recheck firing every `show_after_ms` until the queue drains;
/// a running check shows how many wait behind it; the hold starts only once nothing waits.
#[test]
fn queued_checks_recheck_until_the_queue_drains() {
    let settings = PathCheckSettings::default();
    let start = origin();
    let mut hold = PathCheckHold::default();
    for _ in 0..3 {
        hold.note_submit(start, settings);
    }
    assert_eq!(
        hold.step(None, 3, start + millis(100), settings),
        (None, Some(start + millis(300)))
    );
    assert_eq!(
        hold.step(None, 2, start + millis(300), settings),
        (None, Some(start + millis(600)))
    );
    assert_eq!(
        hold.step(None, 2, start + millis(600), settings),
        (None, Some(start + millis(900)))
    );
    let running = start + millis(600);
    let (shown, _) = hold.step(Some(running), 1, start + millis(950), settings);
    assert_eq!(shown, Some(PathCheckInput { secs: 0, warn: false, queued: 1 }));
    let (shown, wake) = hold.step(None, 1, start + millis(1000), settings);
    assert_eq!(shown.map(|shown| shown.queued), Some(1), "still waiting: no hold yet");
    assert!(wake.is_some_and(|wake| wake > start + millis(1000)));
    let (shown, wake) = hold.step(None, 0, start + millis(1200), settings);
    assert_eq!(shown.map(|shown| shown.queued), Some(1), "drained: the hold keeps the display");
    assert_eq!(wake, Some(start + millis(2200)));
    assert_eq!(hold.step(None, 0, start + millis(2200), settings), (None, None));
}
