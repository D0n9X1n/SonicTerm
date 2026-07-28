use super::*;

#[test]
fn test_clock_only_moves_when_advanced() {
    let clock = TestClock::new();
    let first = clock.now();
    let second = clock.now();
    assert_eq!(first, second, "an unadvanced clock must not drift");
    clock.advance(Duration::from_millis(250));
    assert_eq!(clock.now(), first + Duration::from_millis(250));
}

#[test]
fn test_clock_shares_time_across_clones() {
    let clock = TestClock::new();
    let observer = clock.clone();
    let start = clock.now();
    clock.advance(Duration::from_secs(5));
    assert_eq!(observer.now(), start + Duration::from_secs(5));
}

#[test]
fn system_clock_is_monotonic() {
    let clock = SystemClock;
    let first = clock.now();
    let second = clock.now();
    assert!(second >= first);
}

#[test]
fn clock_trait_is_object_safe() {
    let clock: Arc<dyn Clock> = Arc::new(TestClock::new());
    let _ = clock.now();
}
