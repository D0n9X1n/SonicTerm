use std::sync::mpsc;
use std::time::Duration;

use super::*;

fn report(state: ProgressState, percent: u8) -> Option<ProgressReport> {
    Some(ProgressReport { state, percent })
}

#[test]
fn the_dock_badge_names_each_state() {
    // The badge is text only: the percent, `!` for an error, `…` indeterminate, `‖` paused.
    assert_eq!(badge_text(report(ProgressState::Normal, 56)).as_deref(), Some("56%"));
    assert_eq!(badge_text(report(ProgressState::Error, 25)).as_deref(), Some("!"));
    assert_eq!(badge_text(report(ProgressState::Indeterminate, 0)).as_deref(), Some("…"));
    assert_eq!(badge_text(report(ProgressState::Paused, 40)).as_deref(), Some("‖"));
    assert_eq!(badge_text(None), None, "a clear empties the badge");
}

#[test]
fn launcher_values_hide_the_bar_without_a_fraction() {
    // An error is urgent; indeterminate and cleared reports hide the bar instead of showing 0%.
    let normal = launcher_values(report(ProgressState::Normal, 50));
    assert_eq!(normal, LauncherValues { progress: 0.5, visible: true, urgent: false });
    assert!(launcher_values(report(ProgressState::Error, 10)).urgent);
    assert!(!launcher_values(report(ProgressState::Indeterminate, 0)).visible);
    assert!(!launcher_values(None).visible);
}

#[test]
fn the_progress_thread_sends_only_the_newest_value_after_a_delayed_wake() {
    // Values posted while the thread is busy replace each other, so it never sends a stale one.
    let (sent_tx, sent_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let thread = LatestValueThread::spawn("progress-test", move |value: u32| {
        if value == 1 {
            // The first value holds the thread busy until the test has posted the rest.
            let _ = release_rx.recv();
        }
        let _ = sent_tx.send(value);
    })
    .unwrap();
    thread.post(1);
    std::thread::sleep(Duration::from_millis(50));
    thread.post(2);
    thread.post(3);
    release_tx.send(()).unwrap();
    assert!(thread.stop(4, Duration::from_secs(5)), "an idle thread stops in time");
    let sent: Vec<u32> = sent_rx.try_iter().collect();
    assert_eq!(sent.first(), Some(&1));
    assert_eq!(sent.last(), Some(&4), "the shutdown clear is always the last value sent");
    assert!(!sent.contains(&2), "a value replaced before the thread took it is never sent");
}

/// Every message logged while `body` runs on this thread.
fn logged_messages<T>(body: impl FnOnce() -> T) -> (T, Vec<String>) {
    use tracing_subscriber::layer::SubscriberExt;

    /// Collects each event's `message` field.
    #[derive(Clone, Default)]
    struct Messages(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

    /// Reads the `message` field of one event.
    struct MessageField<'messages>(&'messages mut String);

    impl tracing::field::Visit for MessageField<'_> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                *self.0 = format!("{value:?}");
            }
        }
    }

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Messages {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _context: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut message = String::new();
            event.record(&mut MessageField(&mut message));
            self.0.lock().expect("not poisoned").push(message);
        }
    }

    let layer = Messages::default();
    let subscriber = tracing_subscriber::Registry::default().with(layer.clone());
    let value = sonicterm_logging::test_capture::with_default(subscriber, body);
    let messages = layer.0.lock().expect("not poisoned").clone();
    (value, messages)
}

/// Stop `thread` with the real bound on a helper thread, so a stop that never returns fails the
/// test instead of hanging it. Returns whether it finished, how long it took, and its log lines.
fn stop_within_the_bound(thread: LatestValueThread<u32>) -> (bool, Duration, Vec<String>) {
    let (result_tx, result_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let (finished, messages) = logged_messages(|| thread.stop(9, PROGRESS_STOP_BOUND));
        let _ = result_tx.send((finished, started.elapsed(), messages));
    });
    result_rx
        .recv_timeout(PROGRESS_STOP_BOUND + Duration::from_secs(3))
        .expect("stop returned near its bound")
}

const DETACHED: &str = "progress thread did not stop in time; detached";

#[test]
fn a_progress_thread_blocked_past_its_bound_is_detached() {
    // Shutdown never waits on a blocked D-Bus call: at the real 500 ms bound the thread is left
    // behind and the detach is logged.
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let thread = LatestValueThread::spawn("progress-blocked", move |_: u32| {
        let _ = release_rx.recv();
    })
    .unwrap();
    thread.post(1);
    std::thread::sleep(Duration::from_millis(50));
    let (finished, elapsed, messages) = stop_within_the_bound(thread);
    // Releasing the sender lets the detached thread finish on its own.
    drop(release_tx);
    assert!(!finished);
    assert!(elapsed >= PROGRESS_STOP_BOUND, "it waited the whole bound: {elapsed:?}");
    assert!(elapsed < PROGRESS_STOP_BOUND + Duration::from_millis(500), "{elapsed:?}");
    assert_eq!(messages, [DETACHED]);
}

/// Blocks in `drop` until the test releases it, as a D-Bus connection's close can.
struct BlockingDrop(mpsc::Receiver<()>);

// Lifecycle: dropping waits for the release, standing in for a slow connection close.
impl Drop for BlockingDrop {
    fn drop(&mut self) {
        let _ = self.0.recv();
    }
}

#[test]
fn a_blocking_destructor_in_the_sender_is_bounded_too() {
    // The thread drops everything it owns before it acknowledges the stop, so a sender whose
    // destructor blocks is waited on only up to the bound, then detached and logged.
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let guard = BlockingDrop(release_rx);
    let thread = LatestValueThread::spawn("progress-blocking-drop", move |_: u32| {
        let _held = &guard;
    })
    .unwrap();
    let (finished, elapsed, messages) = stop_within_the_bound(thread);
    drop(release_tx);
    assert!(!finished, "the destructor was still blocked at the bound");
    assert!(elapsed < PROGRESS_STOP_BOUND + Duration::from_millis(500), "{elapsed:?}");
    assert_eq!(messages, [DETACHED]);
}

thread_local! {
    /// A guard the sender parks in its thread's locals, dropped only when that thread exits.
    static PARKED: std::cell::RefCell<Option<BlockingDrop>> = const { std::cell::RefCell::new(None) };
}

#[test]
fn a_blocking_thread_local_destructor_never_holds_the_stop() {
    // Thread-local destructors run after the thread acknowledges, when it exits; stop must still
    // return within the bound, so it never joins a thread that has not exited.
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let mut guard = Some(release_rx);
    let thread = LatestValueThread::spawn("progress-blocking-tls", move |_: u32| {
        if let Some(release) = guard.take() {
            PARKED.with(|parked| *parked.borrow_mut() = Some(BlockingDrop(release)));
        }
    })
    .unwrap();
    let (finished, elapsed, messages) = stop_within_the_bound(thread);
    drop(release_tx);
    assert!(finished, "the thread acknowledged after sending its final value");
    assert!(elapsed < PROGRESS_STOP_BOUND, "{elapsed:?}");
    assert!(messages.is_empty(), "{messages:?}");
}
