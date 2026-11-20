use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::*;

/// A native call's completion callback, as the worker hands it to a backend.
type Completion = Box<dyn FnOnce(DeliveryOutcome) + Send>;

/// A backend that records each request and finishes it as `script` says: at once, never, or
/// only when the test releases it.
#[derive(Clone, Default)]
struct FakeBackend {
    received: Arc<Mutex<Vec<String>>>,
    /// Completion callbacks still held, one per outstanding native call.
    held: Arc<Mutex<Vec<Completion>>>,
    hold_calls: bool,
    quarantines: bool,
}

impl NotifyBackend for FakeBackend {
    fn deliver(&mut self, notice: DesktopNotice, done: Box<dyn FnOnce(DeliveryOutcome) + Send>) {
        self.received.lock().unwrap().push(notice.body);
        if self.hold_calls {
            self.held.lock().unwrap().push(done);
        } else {
            done(DeliveryOutcome::Delivered);
        }
    }

    fn quarantines(&self) -> bool {
        self.quarantines
    }
}

fn notice(body: &str) -> DesktopNotice {
    DesktopNotice { title: "t".into(), body: body.into(), native_id: None }
}

/// Wait until `received` holds `count` requests, or fail after a second.
fn wait_for(received: &Arc<Mutex<Vec<String>>>, count: usize) {
    let start = Instant::now();
    while received.lock().unwrap().len() < count {
        assert!(start.elapsed() < Duration::from_secs(1), "the worker never sent request {count}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// A responsive backend gets every request in order.
#[test]
fn requests_reach_the_backend_in_order() {
    let backend = FakeBackend::default();
    let received = Arc::clone(&backend.received);
    let mut notifier = DesktopNotifier::spawn(backend, Duration::from_millis(200));
    for body in ["one", "two", "three"] {
        notifier.send(notice(body));
    }
    wait_for(&received, 3);
    assert_eq!(*received.lock().unwrap(), ["one", "two", "three"]);
}

/// A native call that outlives its bound on a quarantining backend stays the only outstanding
/// call: later requests are dropped until it completes, then sending resumes.
#[test]
fn a_stuck_call_is_never_joined_by_another() {
    let backend = FakeBackend { hold_calls: true, quarantines: true, ..FakeBackend::default() };
    let (received, held) = (Arc::clone(&backend.received), Arc::clone(&backend.held));
    let mut notifier = DesktopNotifier::spawn(backend, Duration::from_millis(20));
    notifier.send(notice("stuck"));
    wait_for(&received, 1);
    std::thread::sleep(Duration::from_millis(60));
    for body in ["while stuck 1", "while stuck 2"] {
        notifier.send(notice(body));
    }
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(held.lock().unwrap().len(), 1, "exactly one native call outstanding");
    assert_eq!(*received.lock().unwrap(), ["stuck"], "requests while stuck were dropped");
    let late = held.lock().unwrap().pop().unwrap();
    late(DeliveryOutcome::Delivered);
    notifier.send(notice("after"));
    wait_for(&received, 2);
    assert_eq!(received.lock().unwrap().last().map(String::as_str), Some("after"));
}

/// A backend that cancels its own late call is not quarantined: the next request goes out.
#[test]
fn a_cancelling_backend_keeps_sending_after_a_timeout() {
    let backend = FakeBackend { hold_calls: true, quarantines: false, ..FakeBackend::default() };
    let received = Arc::clone(&backend.received);
    let mut notifier = DesktopNotifier::spawn(backend, Duration::from_millis(20));
    notifier.send(notice("slow"));
    wait_for(&received, 1);
    std::thread::sleep(Duration::from_millis(40));
    notifier.send(notice("next"));
    wait_for(&received, 2);
}

/// A full queue drops the overflow and counts it; the app never blocks on a send.
#[test]
fn a_full_queue_drops_and_counts() {
    let backend = FakeBackend { hold_calls: true, quarantines: true, ..FakeBackend::default() };
    let received = Arc::clone(&backend.received);
    let mut notifier = DesktopNotifier::spawn(backend, Duration::from_secs(10));
    notifier.send(notice("busy"));
    wait_for(&received, 1);
    let start = Instant::now();
    for index in 0..10 {
        notifier.send(notice(&format!("queued {index}")));
    }
    assert!(start.elapsed() < Duration::from_millis(100), "send never blocks");
    assert_eq!(notifier.dropped(), 10 - QUEUE_CAPACITY as u64);
}

/// Dropping the handle with a call stuck in the backend returns at once: the worker is never joined.
#[test]
fn shutdown_with_a_stuck_backend_does_not_wait() {
    let backend = FakeBackend { hold_calls: true, quarantines: true, ..FakeBackend::default() };
    let received = Arc::clone(&backend.received);
    let mut notifier = DesktopNotifier::spawn(backend, Duration::from_secs(10));
    notifier.send(notice("stuck"));
    wait_for(&received, 1);
    let start = Instant::now();
    drop(notifier);
    assert!(start.elapsed() < Duration::from_millis(100));
}

/// Markup escaping covers the three characters a notification server reads as markup, ampersand
/// first so it is not escaped twice.
#[test]
fn markup_escaping_keeps_text_literal() {
    assert_eq!(escape_markup("a & <b> c"), "a &amp; &lt;b&gt; c");
    assert_eq!(escape_markup("plain"), "plain");
}
