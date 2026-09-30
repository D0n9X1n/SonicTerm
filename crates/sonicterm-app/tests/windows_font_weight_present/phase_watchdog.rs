//! A watchdog thread for the native font probe's synchronous phases.
//!
//! The probe checks its own deadlines on the event-loop thread, so a phase that never returns
//! would stop every one of those checks. This thread keeps time independently and reports a
//! phase that runs past its limit.

use std::io::Write;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// A request to the watchdog thread.
enum Signal {
    /// Phase `label` started; it must end by `deadline`, which is `limit` after its start.
    Start { label: String, limit: Duration, deadline: Instant },
    /// The running phase ended in time.
    End,
}

/// Times one synchronous phase at a time on its own thread.
pub(super) struct PhaseWatchdog {
    signals: Option<Sender<Signal>>,
    thread: Option<JoinHandle<()>>,
}

impl PhaseWatchdog {
    /// Start the watchdog thread. `on_overrun` runs once, with the phase's label and limit, if a
    /// phase is still running when its limit passes.
    pub(super) fn spawn(on_overrun: impl FnOnce(&str, Duration) + Send + 'static) -> Self {
        let (signals, receiver) = mpsc::channel();
        let thread = thread::spawn(move || watch(&receiver, on_overrun));
        Self { signals: Some(signals), thread: Some(thread) }
    }

    /// Start timing phase `label`, which must end within `limit`; it replaces any running phase.
    pub(super) fn start(&self, label: String, limit: Duration) {
        self.send(Signal::Start { label, limit, deadline: Instant::now() + limit });
    }

    /// Stop timing the running phase.
    pub(super) fn end(&self) {
        self.send(Signal::End);
    }

    fn send(&self, signal: Signal) {
        if let Some(signals) = &self.signals {
            // When: `signals` is open; the thread stops only after reporting an overrun, and then
            // has nothing left to time, so a failed send is ignored.
            let _ = signals.send(signal);
        }
    }
}

// Lifecycle: dropping `signals` ends the watch loop, so joining `thread` never waits for a phase limit.
impl Drop for PhaseWatchdog {
    fn drop(&mut self) {
        self.signals = None;
        if let Some(thread) = self.thread.take() {
            // When: `thread` is still owned; a watch loop that panicked has nothing left to report.
            let _ = thread.join();
        }
    }
}

/// Wait for signals, and call `on_overrun` if a started phase is still running at its deadline.
fn watch(receiver: &Receiver<Signal>, on_overrun: impl FnOnce(&str, Duration)) {
    let mut running: Option<(String, Duration, Instant)> = None;
    loop {
        let signal = match &running {
            None => match receiver.recv() {
                Ok(signal) => signal,
                // When: `receiver` disconnects with no phase running, the probe finished.
                Err(_) => return,
            },
            Some((label, limit, deadline)) => {
                match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(signal) => signal,
                    Err(RecvTimeoutError::Timeout) => {
                        // When: `deadline` passed before the running phase's end signal arrived.
                        on_overrun(label.as_str(), *limit);
                        return;
                    }
                    // When: the watchdog was dropped mid-phase; the probe ends its run itself.
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        };
        running = match signal {
            Signal::Start { label, limit, deadline } => Some((label, limit, deadline)),
            Signal::End => None,
        };
    }
}

/// How long `abort_on_overrun` waits for its report to reach stderr before it aborts anyway.
const REPORT_GRACE: Duration = Duration::from_secs(1);

/// Report the overrunning phase on stderr, then abort the process: the phase's thread is stuck,
/// so no assertion on it can fail the test. The stuck thread may hold stderr's lock, so another
/// thread writes the report, and the process aborts once it is written or after `REPORT_GRACE`.
pub(super) fn abort_on_overrun(label: &str, limit: Duration) {
    let report = format!(
        "native font probe phase `{label}` ran past its {limit:?} limit; aborting the stuck process\n"
    );
    let (written, reported) = mpsc::channel::<()>();
    // A thread that cannot start drops `written` with its closure, so the wait below ends at once.
    let _ = thread::Builder::new().spawn(move || {
        // Write to stderr directly: output that the test harness captures is lost when the process aborts.
        let _ = std::io::stderr().write_all(report.as_bytes());
        let _ = written.send(());
    });
    let _ = reported.recv_timeout(REPORT_GRACE);
    std::process::abort();
}

#[cfg(test)]
#[path = "phase_watchdog_tests.rs"]
mod phase_watchdog_tests;
