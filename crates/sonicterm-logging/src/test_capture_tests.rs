use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::{layer::Context, layer::SubscriberExt, Layer, Registry};

/// Keep only this test's event messages, independently of other subscribers in the logging test binary.
struct Messages(Arc<Mutex<Vec<String>>>);

impl<S: Subscriber> Layer<S> for Messages {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        // Read the tracing message field rather than relying on formatter timestamps or presentation.
        struct Message(String);
        impl Visit for Message {
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        self.0.lock().unwrap().push(message.0);
    }
}

/// This private call site is first reached only by the child test's uncaptured thread.
fn private_first_reach_event() {
    tracing::warn!("private first-reach capture probe");
}

/// Re-exec excludes other tests' dispatchers, so they cannot accidentally mask the uncaptured first reach.
#[test]
fn capture_records_after_an_uncaptured_thread_reaches_the_call_site_first() {
    let output = crate::lib_tests::child_output(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "test_capture::test_capture_tests::uncaptured_first_reach_child",
                "--ignored",
                "--nocapture",
            ])
            .env("SONICTERM_CAPTURE_FIRST_REACH_CHILD", "1"),
    );
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// The background warning is not captured, but a later warning at that exact call site must be.
#[test]
#[ignore = "fresh-process child selected by the capture regression test"]
fn uncaptured_first_reach_child() {
    assert!(std::env::var_os("SONICTERM_CAPTURE_FIRST_REACH_CHILD").is_some());
    let messages = Arc::new(Mutex::new(Vec::new()));
    let subscriber = Registry::default().with(Messages(messages.clone()));
    super::with_default(subscriber, || {
        std::thread::spawn(private_first_reach_event).join().unwrap();
        private_first_reach_event();
    });
    assert_eq!(*messages.lock().unwrap(), ["private first-reach capture probe"]);
}

/// The silent global must not admit an event or feed a scoped sink on a thread outside the capture.
#[test]
fn uncaptured_threads_stay_disabled_and_unrecorded() {
    crate::test_capture::ensure_callsite_interest();
    let messages = Arc::new(Mutex::new(Vec::new()));
    let subscriber = Registry::default().with(Messages(messages.clone()));
    crate::test_capture::with_default(subscriber, || {
        assert!(tracing::enabled!(tracing::Level::TRACE), "capture is a positive control");
        std::thread::spawn(|| {
            assert!(!tracing::enabled!(tracing::Level::TRACE));
            assert!(!tracing::enabled!(tracing::Level::WARN));
            tracing::warn!("outside capture");
        })
        .join()
        .unwrap();
        assert!(messages.lock().unwrap().is_empty());
        tracing::warn!("inside capture");
    });
    assert!(!tracing::enabled!(tracing::Level::WARN));
    tracing::warn!("after capture");
    assert_eq!(*messages.lock().unwrap(), ["inside capture"]);
}
