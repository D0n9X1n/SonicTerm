//! Native probe watchdog ownership and bounded expiry checks.

/// A snapshot never borrows the parser or app, so a blocked handler cannot hide its last probe boundary.
pub(super) struct NativeProbeStallState {
    probe: String,
    case: String,
    phase: String,
    parser_lock_held: bool,
    // Callback counts preserve delivery order even when adjacent clock readings are equal.
    heartbeat_count: u64,
    last_about_to_wait: Option<std::time::Instant>,
}

/// Heartbeats and disarm share a channel so neither watchdog wait needs polling or a sleep loop.
pub(super) enum NativeWatchdogEvent {
    AboutToWait,
    Disarm,
}

/// Shares only short-lived diagnostic state locks with the watchdog, independently of the fixture's parser lock.
pub(super) struct NativeProbeProgress {
    state: std::sync::Mutex<NativeProbeStallState>,
    events: std::sync::mpsc::Sender<NativeWatchdogEvent>,
}

impl NativeProbeProgress {
    /// Give the fixture shared progress and the watchdog sole ownership of its notification receiver.
    pub(super) fn new() -> (std::sync::Arc<Self>, std::sync::mpsc::Receiver<NativeWatchdogEvent>) {
        let (events, receiver) = std::sync::mpsc::channel();
        (
            std::sync::Arc::new(Self {
                state: std::sync::Mutex::new(NativeProbeStallState {
                    probe: "fixture".into(),
                    case: "all".into(),
                    phase: "before_run_app".into(),
                    parser_lock_held: false,
                    heartbeat_count: 0,
                    last_about_to_wait: None,
                }),
                events,
            }),
            receiver,
        )
    }

    /// Publish the next operation before entering it, without keeping this lock during native or parser work.
    pub(super) fn set(&self, probe: &str, case: &str, phase: impl Into<String>) {
        let mut state = self.state.lock().unwrap();
        state.probe = probe.into();
        state.case = case.into();
        state.phase = phase.into();
    }

    /// Bracket the deliberately held parser lock, including a stall while trying to acquire it.
    pub(super) fn parser_lock(&self, held: bool) {
        self.state.lock().unwrap().parser_lock_held = held;
    }

    /// Mark callback entry before the loop can block in any of the fixture's probes.
    pub(super) fn about_to_wait(&self) {
        {
            let mut state = self.state.lock().unwrap();
            state.heartbeat_count += 1;
            state.last_about_to_wait = Some(std::time::Instant::now());
        }
        // A finished watchdog no longer needs heartbeats; closed notification channels are harmless.
        let _ = self.events.send(NativeWatchdogEvent::AboutToWait);
    }
}

/// The guard owns the worker only until run_app returns, including its unwind path.
pub(super) struct NativeWatchdog {
    disarm: std::sync::mpsc::Sender<NativeWatchdogEvent>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl NativeWatchdog {
    /// Start the injected expiry actions on a separate thread before any native callback runs.
    pub(super) fn start(
        progress: std::sync::Arc<NativeProbeProgress>,
        events: std::sync::mpsc::Receiver<NativeWatchdogEvent>,
        deadline: std::time::Instant,
        wake: impl FnOnce() -> bool + Send + 'static,
        mut report: impl std::io::Write + Send + 'static,
        exit: impl FnOnce(i32) + Send + 'static,
    ) -> Self {
        let disarm = progress.events.clone();
        let worker = std::thread::spawn(move || {
            run_native_watchdog(&progress, &events, deadline, wake, &mut report, exit);
        });
        Self { disarm, worker: Some(worker) }
    }
}

// Lifecycle: NativeWatchdog disarms its channel wait and joins the worker before the fixture leaves scope.
impl Drop for NativeWatchdog {
    fn drop(&mut self) {
        // An already expired worker may have closed its receiver; disarm still must not mask a fixture failure.
        let _ = self.disarm.send(NativeWatchdogEvent::Disarm);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("native watchdog thread");
        }
    }
}

/// Expiry reports through Write rather than libtest capture, tries one wake, and bounds the follow-up to two seconds.
pub(super) fn run_native_watchdog(
    progress: &NativeProbeProgress,
    events: &std::sync::mpsc::Receiver<NativeWatchdogEvent>,
    deadline: std::time::Instant,
    wake: impl FnOnce() -> bool,
    report: &mut impl std::io::Write,
    exit: impl FnOnce(i32),
) {
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Duration, Instant};

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match events.recv_timeout(remaining) {
            Ok(NativeWatchdogEvent::AboutToWait) => {}
            Ok(NativeWatchdogEvent::Disarm) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => break,
        }
    }
    let initial = {
        let state = progress.state.lock().unwrap();
        format!(
            "native watchdog expired: probe={} case={} phase={} parser_lock_held={} last_about_to_wait={:?}\n",
            state.probe, state.case, state.phase, state.parser_lock_held, state.last_about_to_wait,
        )
    };
    // Reporting failure cannot prevent the wake or nonzero exit; stderr may already be closed.
    let _ = report.write_all(initial.as_bytes());
    let wake_started = Instant::now();
    // Snapshot callback order, not clock time: an immediate heartbeat need not advance Instant.
    let heartbeats_before_wake = progress.state.lock().unwrap().heartbeat_count;
    let wake_sent = wake();
    let wake_deadline = wake_started + Duration::from_secs(2);
    let after_wake = loop {
        let observed = progress.state.lock().unwrap().heartbeat_count;
        if observed > heartbeats_before_wake {
            break true;
        }
        let remaining = wake_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break false;
        }
        match events.recv_timeout(remaining) {
            Ok(NativeWatchdogEvent::AboutToWait | NativeWatchdogEvent::Disarm) => {}
            Err(RecvTimeoutError::Disconnected | RecvTimeoutError::Timeout) => {
                break progress.state.lock().unwrap().heartbeat_count > heartbeats_before_wake;
            }
        }
    };
    // Direct write_all also keeps this post-wake verdict visible when libtest captures the stalled test's output.
    let _ = report.write_all(
        format!("native watchdog wake_sent={wake_sent} about_to_wait_after_wake={after_wake}\n")
            .as_bytes(),
    );
    exit(1);
}

#[cfg(test)]
#[path = "test_hooks_window_pointer_watchdog_fixture_tests.rs"]
mod test_hooks_window_pointer_watchdog_fixture_tests;
