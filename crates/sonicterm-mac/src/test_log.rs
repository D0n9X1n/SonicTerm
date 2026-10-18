//! Test support: capture every tracing level into a string.

use std::io::Write;
use std::sync::{Arc, Mutex};

/// A log sink the tests read back after a capture.
#[derive(Clone, Default)]
struct SharedLog(Arc<Mutex<Vec<u8>>>);

impl Write for SharedLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run `action` with every tracing level captured, and return the text.
pub(crate) fn capture_all_levels(action: impl FnOnce()) -> String {
    let log = SharedLog::default();
    let writer = log.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    sonicterm_logging::test_capture::with_default(subscriber, action);
    let bytes = log.0.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}
