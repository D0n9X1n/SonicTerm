//! The platform side of `OSC 9;4` progress: the Windows taskbar button, the macOS Dock badge and
//! the Linux `com.canonical.Unity.LauncherEntry` signal.
//!
//! The App decides what each surface shows and when; a backend only shows it. Every call comes
//! from the event loop. A backend that talks to a slow service (D-Bus) hands the value to its own
//! thread through a [`LatestValueThread`], so the event loop never waits on it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, Sender, TrySendError};
use parking_lot::Mutex;
pub use sonicterm_vt::vt::{ProgressReport, ProgressState};

/// How long App shutdown waits for a progress thread's final clear before detaching it.
pub const PROGRESS_STOP_BOUND: Duration = Duration::from_millis(500);

/// The surface one progress value is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProgressSurface {
    /// The app-wide surface: the macOS Dock tile or the Linux launcher entry.
    App,
    /// One window's taskbar button on Windows, by its native window handle.
    Window(isize),
}

/// A platform progress surface. Called only on the event loop.
pub trait TaskbarProgressBackend {
    /// Whether this platform shows progress per window (Windows) rather than app-wide.
    fn per_window(&self) -> bool;

    /// Show `report` on `surface`, or clear it on `None`.
    fn deliver(&mut self, surface: ProgressSurface, report: Option<ProgressReport>);

    /// Release the backend at App shutdown, after the App delivered its final clears.
    fn shutdown(&mut self) {}
}

/// The Dock badge text for `report`: the percent, `!` for an error, `…` while indeterminate and
/// `‖` while paused; `None` clears the badge.
pub fn badge_text(report: Option<ProgressReport>) -> Option<String> {
    let report = report?;
    Some(match report.state {
        ProgressState::Normal => format!("{}%", report.percent),
        ProgressState::Error => "!".to_owned(),
        ProgressState::Indeterminate => "…".to_owned(),
        ProgressState::Paused => "‖".to_owned(),
    })
}

/// The `LauncherEntry.Update` properties for `report`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LauncherValues {
    /// `progress`, `0.0..=1.0`.
    pub progress: f64,
    /// `progress-visible`.
    pub visible: bool,
    /// `urgent`, set for an error.
    pub urgent: bool,
}

/// The launcher values for `report`. The entry has no indeterminate bar, so indeterminate hides
/// the bar rather than drawing a misleading 0%.
pub fn launcher_values(report: Option<ProgressReport>) -> LauncherValues {
    let Some(report) = report else {
        // When: report is None, the entry hides its bar.
        return LauncherValues { progress: 0.0, visible: false, urgent: false };
    };
    let progress = f64::from(report.percent) / 100.0;
    match report.state {
        ProgressState::Normal | ProgressState::Paused => {
            LauncherValues { progress, visible: true, urgent: false }
        }
        ProgressState::Error => LauncherValues { progress, visible: true, urgent: true },
        ProgressState::Indeterminate => {
            LauncherValues { progress: 0.0, visible: false, urgent: false }
        }
    }
}

/// A thread that sends the newest of a stream of values, never a queue: a value posted while the
/// thread is busy replaces any value it has not taken yet.
pub struct LatestValueThread<Value: Send + 'static> {
    slot: Arc<Mutex<Option<Value>>>,
    wake: Sender<()>,
    stop: Arc<AtomicBool>,
    done: Receiver<()>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl<Value: Send + 'static> LatestValueThread<Value> {
    /// Start a thread named `name` that passes each newest value to `send`.
    // Ordering: thread_stop is Acquire before the slot drain, pairing with the Release in `stop`,
    // so a stopping thread still sends the final value.
    pub fn spawn(
        name: &str,
        mut send: impl FnMut(Value) + Send + 'static,
    ) -> std::io::Result<Self> {
        let slot: Arc<Mutex<Option<Value>>> = Arc::default();
        let (wake, woken) = bounded::<()>(1);
        let (done_tx, done) = bounded::<()>(1);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_slot = Arc::clone(&slot);
        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::Builder::new().name(name.to_owned()).spawn(move || {
            while woken.recv().is_ok() {
                // Read the flag before the slot: a stop's final value is written before the flag.
                let stopping = thread_stop.load(Ordering::Acquire);
                let value = thread_slot.lock().take();
                if let Some(value) = value {
                    send(value);
                }
                if stopping {
                    // When: stopping was read before the drain, the final value has been sent.
                    break;
                }
            }
            // Everything the thread owns is dropped before the acknowledgement, so a sender whose
            // destructor blocks (a D-Bus connection's close) counts against `stop`'s bound.
            drop((send, woken, thread_slot, thread_stop));
            let _ = done_tx.send(());
        })?;
        Ok(Self { slot, wake, stop, done, handle: Some(handle) })
    }

    /// Post `value`, replacing any the thread has not taken yet.
    pub fn post(&self, value: Value) {
        *self.slot.lock() = Some(value);
        // A full wake channel already holds a wake the thread has not consumed.
        if let Err(TrySendError::Disconnected(())) = self.wake.try_send(()) {
            // A disconnected wake channel means the thread already stopped.
            tracing::debug!(target: "sonicterm_app::taskbar", "progress thread already stopped");
        }
    }

    /// Post `last`, then stop the thread and wait at most `bound` for its acknowledgement. Returns
    /// whether it acknowledged; a thread still blocked is detached and logged. The thread is never
    /// joined: its thread-local destructors run after the acknowledgement, so a join could block.
    // Ordering: stop is Release after the final slot write, pairing with the Acquire in `spawn`.
    pub fn stop(mut self, last: Value, bound: Duration) -> bool {
        *self.slot.lock() = Some(last);
        self.stop.store(true, Ordering::Release);
        let _ = self.wake.try_send(());
        if let Err(RecvTimeoutError::Timeout) = self.done.recv_timeout(bound) {
            // When: recv_timeout hits Timeout, the thread is blocked past `bound`; it is detached.
            tracing::warn!(target: "sonicterm_app::taskbar", bound_ms = bound.as_millis() as u64, "progress thread did not stop in time; detached");
            return false;
        }
        // The thread acknowledged after sending its final value and dropping what it owned. Only
        // its exit is left, which can still run blocking thread-local destructors, so dropping
        // the handle detaches it instead of joining.
        drop(self.handle.take());
        true
    }
}

#[cfg(test)]
#[path = "taskbar_progress_tests.rs"]
mod taskbar_progress_tests;
