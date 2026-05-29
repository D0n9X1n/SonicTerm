//! Tests for [`sonic_text::async_fallback::AsyncFallbackLoader`].
//!
//! Covers the Epic #300 P4 contract:
//!
//! 1. `request_load` spawns a background worker, the worker populates
//!    `loaded`, and the notifier fires exactly once per successful load.
//! 2. Repeated `request_load` calls for the same family are
//!    deduplicated — only one worker thread ever runs per family.
//! 3. A load that returns `None` is remembered as `failed` and never
//!    retried (no spurious re-spawns on every shape pass).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use sonic_text::async_fallback::{AsyncFallbackLoader, FontHandle, LoadFn, NotifyFn};

fn wait_until<F: Fn() -> bool>(timeout: Duration, cond: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    cond()
}

#[test]
fn request_load_populates_loaded_and_fires_notifier() {
    let load_calls = Arc::new(AtomicUsize::new(0));
    let load_calls_for_fn = load_calls.clone();
    let load_fn: LoadFn = Arc::new(move |family: &'static str| {
        load_calls_for_fn.fetch_add(1, Ordering::SeqCst);
        Some(FontHandle { family, bytes_loaded: 42 })
    });
    let notify_calls = Arc::new(AtomicUsize::new(0));
    let notify_calls_for_fn = notify_calls.clone();
    let notify: NotifyFn = Arc::new(move || {
        notify_calls_for_fn.fetch_add(1, Ordering::SeqCst);
    });

    let loader = AsyncFallbackLoader::new(load_fn, notify);
    assert!(!loader.is_loaded("PingFang SC"));

    let spawned = loader.request_load("PingFang SC");
    assert!(spawned, "first call to request_load should spawn a worker");

    let loaded = wait_until(Duration::from_secs(2), || loader.is_loaded("PingFang SC"));
    assert!(loaded, "loader.loaded should populate within 2s");

    // Notifier fires AFTER loaded is published — so wait_until on
    // `is_loaded` may observe the populated map a beat before the
    // notify callback returns. Give it a short grace window.
    let notified = wait_until(Duration::from_secs(1), || notify_calls.load(Ordering::SeqCst) >= 1);
    assert!(notified, "notifier should fire after successful load");

    assert_eq!(load_calls.load(Ordering::SeqCst), 1, "load_fn should be called exactly once");
    assert_eq!(
        notify_calls.load(Ordering::SeqCst),
        1,
        "notifier should fire exactly once per successful load"
    );

    let snapshot = loader.loaded_snapshot();
    assert_eq!(snapshot, vec!["PingFang SC"]);
}

#[test]
fn repeated_request_load_is_idempotent() {
    // Hold the worker inside the load_fn until the test releases it.
    // This lets us call request_load multiple times while the first
    // worker is still in flight, proving dedup happens at request
    // time (not only after completion).
    let barrier = Arc::new(Barrier::new(2));
    let barrier_for_fn = barrier.clone();
    let load_calls = Arc::new(AtomicUsize::new(0));
    let load_calls_for_fn = load_calls.clone();
    let load_fn: LoadFn = Arc::new(move |family: &'static str| {
        load_calls_for_fn.fetch_add(1, Ordering::SeqCst);
        barrier_for_fn.wait();
        Some(FontHandle { family, bytes_loaded: 0 })
    });
    let notify_calls = Arc::new(AtomicUsize::new(0));
    let notify_calls_for_fn = notify_calls.clone();
    let notify: NotifyFn = Arc::new(move || {
        notify_calls_for_fn.fetch_add(1, Ordering::SeqCst);
    });
    let loader = AsyncFallbackLoader::new(load_fn, notify);

    assert!(loader.request_load("Apple Color Emoji"));
    // Worker is now blocked on the barrier — call again from a few
    // different threads, all should be no-ops.
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let l = loader.clone();
            std::thread::spawn(move || l.request_load("Apple Color Emoji"))
        })
        .collect();
    for h in handles {
        assert!(!h.join().unwrap(), "concurrent request while pending must be a no-op");
    }
    assert!(loader.is_pending("Apple Color Emoji"));

    // Release the worker.
    barrier.wait();
    assert!(wait_until(Duration::from_secs(2), || loader.is_loaded("Apple Color Emoji")));

    // After completion, request again — still a no-op (the family is
    // in `loaded`, not `pending`).
    assert!(!loader.request_load("Apple Color Emoji"));

    assert_eq!(
        load_calls.load(Ordering::SeqCst),
        1,
        "load_fn must run exactly once across 1 + 8 + 1 request_load calls"
    );
    assert_eq!(notify_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn failed_load_is_remembered_and_not_retried() {
    let load_calls = Arc::new(AtomicUsize::new(0));
    let load_calls_for_fn = load_calls.clone();
    let load_fn: LoadFn = Arc::new(move |_family: &'static str| {
        load_calls_for_fn.fetch_add(1, Ordering::SeqCst);
        None
    });
    let notify_calls = Arc::new(AtomicUsize::new(0));
    let notify_calls_for_fn = notify_calls.clone();
    let notify: NotifyFn = Arc::new(move || {
        notify_calls_for_fn.fetch_add(1, Ordering::SeqCst);
    });
    let loader = AsyncFallbackLoader::new(load_fn, notify);

    assert!(loader.request_load("Definitely Not Installed"));
    assert!(wait_until(Duration::from_secs(2), || loader.is_failed("Definitely Not Installed")));
    assert!(!loader.is_loaded("Definitely Not Installed"));
    assert!(!loader.is_pending("Definitely Not Installed"));

    // Re-requesting after a failure is a no-op — no second worker.
    assert!(!loader.request_load("Definitely Not Installed"));
    assert!(!loader.request_load("Definitely Not Installed"));
    assert_eq!(load_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        notify_calls.load(Ordering::SeqCst),
        0,
        "notifier must not fire for failed loads — shape cache stays valid"
    );
}
