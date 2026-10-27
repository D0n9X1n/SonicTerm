use super::*;

/// A native resize failure must reach the caller instead of being swallowed.
#[test]
fn a_failed_native_resize_reports_the_error() {
    let resize = resize_callback(Box::new(|_, _| Err(anyhow::anyhow!("native resize refused"))));

    assert!(resize(80, 24).is_err(), "the native failure must reach the caller");
}

/// A failure is not an applied size, so an identical retry must reach native again.
#[test]
fn a_failed_resize_does_not_suppress_an_identical_retry() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let resize = resize_callback(Box::new(move |_, _| {
        seen.fetch_add(1, Ordering::Relaxed);
        Err(anyhow::anyhow!("native resize refused"))
    }));

    assert!(resize(80, 24).is_err(), "the first attempt fails natively");
    assert!(resize(80, 24).is_err(), "the identical retry must fail rather than report success");
    assert_eq!(
        calls.load(Ordering::Relaxed),
        2,
        "a size that never applied must not be cached as applied"
    );
}

/// Repeating a size that did apply skips the native reflow that would change nothing.
#[test]
fn a_repeated_successful_size_skips_the_native_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let resize = resize_callback(Box::new(move |_, _| {
        seen.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }));

    assert!(resize(80, 24).is_ok(), "the first size applies");
    assert!(resize(80, 24).is_ok(), "the repeat is accepted");
    assert_eq!(calls.load(Ordering::Relaxed), 1, "an unchanged size must not reach native twice");
}

/// A failed resize leaves the last successful size in place, so returning to it still dedups.
#[test]
fn a_failed_resize_preserves_the_previous_successful_size() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let resize = resize_callback(Box::new(move |cols, _| {
        seen.fetch_add(1, Ordering::Relaxed);
        if cols == 100 {
            // When: `cols` is the poisoned width, fail natively without disturbing the stored size.
            return Err(anyhow::anyhow!("native resize refused"));
        }
        Ok(())
    }));

    assert!(resize(80, 24).is_ok(), "the first size applies");
    assert!(resize(100, 30).is_err(), "the second size fails natively");
    assert!(resize(80, 24).is_ok(), "returning to the applied size is accepted");
    assert_eq!(
        calls.load(Ordering::Relaxed),
        2,
        "the failed size must not replace the last size that actually applied"
    );
}

/// A zero axis is refused before the native call and before the stored size changes.
#[test]
fn a_zero_axis_is_refused_before_the_native_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let resize = resize_callback(Box::new(move |_, _| {
        seen.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }));

    let zero_cols = resize(0, 24).expect_err("zero columns must be refused");
    let zero_rows = resize(80, 0).expect_err("zero rows must be refused");
    for refused in [&zero_cols, &zero_rows] {
        assert_eq!(
            refused.downcast_ref::<std::io::Error>().map(std::io::Error::kind),
            Some(std::io::ErrorKind::InvalidInput),
            "a refused axis must be distinguishable from a native failure"
        );
    }
    assert_eq!(calls.load(Ordering::Relaxed), 0, "a refused size must not reach native");
    assert!(resize(80, 24).is_ok(), "a valid size still applies after a refusal");
    assert_eq!(calls.load(Ordering::Relaxed), 1, "the refusal must not have cached a size");

    // A refusal against an established size must leave that size cached: the
    // refused request is not state, so the prior successful one still is.
    assert!(resize(0, 30).is_err(), "a zero axis is refused after a successful size too");
    assert_eq!(calls.load(Ordering::Relaxed), 1, "the later refusal did not reach native either");
    assert!(resize(80, 24).is_ok(), "the previously applied size is still the cached one");
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "a refusal must not disturb the cached size, so the repeat still dedups"
    );
}

/// A concurrent identical request must not report success off a failed in-flight native call.
#[test]
fn a_concurrent_identical_request_cannot_succeed_while_the_native_call_fails() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let (entered_tx, entered_rx) = crossbeam_channel::bounded::<()>(4);
    let (attempting_tx, attempting_rx) = crossbeam_channel::bounded::<()>(4);
    let (release_tx, release_rx) = crossbeam_channel::bounded::<()>(4);
    let resize = Arc::new(resize_callback(Box::new(move |_, _| {
        seen.fetch_add(1, Ordering::Relaxed);
        let _ = entered_tx.send(());
        let _ = release_rx.recv_timeout(std::time::Duration::from_secs(5));
        Err(anyhow::anyhow!("native resize refused"))
    })));

    let first = std::thread::spawn({
        let resize = resize.clone();
        move || (**resize)(80, 24).is_err()
    });
    let first_entered = entered_rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok();

    let second = std::thread::spawn({
        let resize = resize.clone();
        let attempting = attempting_tx.clone();
        move || {
            let _ = attempting.send(());
            (**resize)(80, 24).is_err()
        }
    });
    let second_attempting = attempting_rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok();
    // The gate: the second request has started and still cannot enter native
    // while the first holds the lock. Without this observation the test would
    // also pass if the two requests merely ran one after another.
    let second_blocked = entered_rx.recv_timeout(std::time::Duration::from_millis(200)).is_err();

    let _ = release_tx.send(());
    let second_entered = entered_rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok();
    let _ = release_tx.send(());

    // Join before asserting so a failed expectation cannot leak a blocked thread.
    let first_failed = first.join().expect("first resize thread");
    let second_failed = second.join().expect("second resize thread");

    assert!(first_entered, "the first native call must start before the second request");
    assert!(second_attempting, "the second request starts while the first holds the lock");
    assert!(second_blocked, "the second request must not enter native while the first is inside");
    assert!(second_entered, "the second request reaches native once the first releases");
    assert!(first_failed, "the in-flight call fails natively");
    assert!(second_failed, "the concurrent identical request must not report success");
    assert_eq!(calls.load(Ordering::Relaxed), 2, "each unapplied request must reach native");
}

/// Only a successful apply caches a size, so a recovered size dedups on its next repeat.
#[test]
fn a_size_that_recovers_after_failure_dedups_on_the_next_repeat() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let resize = resize_callback(Box::new(move |_, _| {
        if seen.fetch_add(1, Ordering::Relaxed) == 0 {
            // When: this is the first attempt, fail natively so the size is never cached.
            return Err(anyhow::anyhow!("native resize refused"));
        }
        Ok(())
    }));

    assert!(resize(80, 24).is_err(), "the first attempt fails natively");
    assert!(resize(80, 24).is_ok(), "the retry reaches native again and succeeds");
    assert!(resize(80, 24).is_ok(), "the repeat after success is accepted");
    assert_eq!(
        calls.load(Ordering::Relaxed),
        2,
        "the successful apply caches the size, so only the third request dedups"
    );
}

/// Distinct concurrent sizes are serialized, applied in order, and the last one caches.
#[test]
fn concurrent_different_sizes_are_serialized_and_both_apply() {
    let applied = Arc::new(Mutex::new(Vec::<(u16, u16)>::new()));
    let (entered_tx, entered_rx) = crossbeam_channel::bounded::<()>(4);
    let (attempting_tx, attempting_rx) = crossbeam_channel::bounded::<()>(4);
    let (release_tx, release_rx) = crossbeam_channel::bounded::<()>(4);
    let recorded = applied.clone();
    let resize = Arc::new(resize_callback(Box::new(move |cols, rows| {
        recorded.lock().push((cols, rows));
        let _ = entered_tx.send(());
        let _ = release_rx.recv_timeout(std::time::Duration::from_secs(5));
        Ok(())
    })));

    let first = std::thread::spawn({
        let resize = resize.clone();
        move || (**resize)(80, 24).is_ok()
    });
    let first_entered = entered_rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok();

    let second = std::thread::spawn({
        let resize = resize.clone();
        let attempting = attempting_tx.clone();
        move || {
            let _ = attempting.send(());
            (**resize)(100, 30).is_ok()
        }
    });
    let second_attempting = attempting_rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok();
    // The gate: a distinct size still cannot enter native while the first call
    // holds the lock, so the two are ordered rather than merely both completing.
    let second_blocked = entered_rx.recv_timeout(std::time::Duration::from_millis(200)).is_err();

    let _ = release_tx.send(());
    let second_entered = entered_rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok();
    let _ = release_tx.send(());

    // Join before asserting so a failed expectation cannot leak a live thread.
    let first_ok = first.join().expect("first resize thread");
    let second_ok = second.join().expect("second resize thread");

    assert!(first_entered, "the first size reaches native");
    assert!(second_attempting, "the second request starts while the first holds the lock");
    assert!(second_blocked, "a distinct size must wait rather than overlap the first call");
    assert!(second_entered, "the second size reaches native once the first releases");
    assert!(first_ok, "the first distinct size applies");
    assert!(second_ok, "the second distinct size applies");
    assert_eq!(
        applied.lock().as_slice(),
        [(80, 24), (100, 30)],
        "native saw both sizes once, in the order the lock admitted them"
    );

    // The cache holds the last successful request, not the first: repeating the
    // most recent size dedups, while returning to the earlier one does not.
    assert!((*resize)(100, 30).is_ok(), "repeating the last applied size is accepted");
    assert_eq!(applied.lock().len(), 2, "the last successful size dedups without a native call");

    let _ = release_tx.send(());
    assert!((*resize)(80, 24).is_ok(), "an earlier size is not the cached one");
    assert_eq!(
        applied.lock().as_slice(),
        [(80, 24), (100, 30), (80, 24)],
        "only the last successful request is cached, so an earlier size reapplies"
    );
}

/// The first request always reaches native, even at the size the pty was spawned with.
///
/// The cache starts empty rather than seeded from the spawn dimensions, so a
/// first request cannot be mistaken for a repeat of geometry already applied.
#[test]
fn the_first_request_reaches_native_even_at_the_spawn_size() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let resize = resize_callback(Box::new(move |_, _| {
        seen.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }));

    assert!(resize(80, 24).is_ok(), "the first request applies");
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "an empty cache cannot match a first request, whatever the pty was spawned with"
    );
}
