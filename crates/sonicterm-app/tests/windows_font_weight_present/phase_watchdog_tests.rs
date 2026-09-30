use super::*;
use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};

/// Tells `abort_on_overrun_fixture` to run, and whether its stuck thread holds stderr (`held`).
const ABORT_FIXTURE_MODE: &str = "SONICTERM_PHASE_WATCHDOG_ABORT_FIXTURE";

/// A phase that never ends is reported with its label and limit once the limit passes, so a stuck
/// render fails the probe instead of holding the test job until its platform limit.
#[test]
fn a_phase_that_never_ends_is_reported_after_its_limit() {
    let (reports, reported) = mpsc::channel();
    let watchdog = PhaseWatchdog::spawn(move |label, limit| {
        let _ = reports.send((label.to_string(), limit));
    });
    let started = Instant::now();
    watchdog.start("stuck".to_string(), Duration::from_millis(100));
    let report = reported.recv_timeout(Duration::from_secs(5)).expect("the overrun is reported");
    assert_eq!(report, ("stuck".to_string(), Duration::from_millis(100)));
    assert!(started.elapsed() >= Duration::from_millis(100), "the overrun was reported early");
}

/// Phases that end within their limits are never reported, however long they run in total.
#[test]
fn phases_that_end_in_time_are_never_reported() {
    let (reports, reported) = mpsc::channel::<String>();
    let watchdog = PhaseWatchdog::spawn(move |label, _| {
        let _ = reports.send(label.to_string());
    });
    // Twelve 100 ms phases under a 1 s limit run 1.2 s together, so only per-phase timing passes.
    for index in 0..12 {
        watchdog.start(format!("phase-{index}"), Duration::from_secs(1));
        thread::sleep(Duration::from_millis(100));
        watchdog.end();
    }
    drop(watchdog);
    assert_eq!(
        reported.recv_timeout(Duration::from_secs(5)),
        Err(RecvTimeoutError::Disconnected),
        "a phase that ended in time was reported"
    );
}

/// Dropping the watchdog mid-phase stops its thread without a report, so a probe that ends its
/// own run is never aborted.
#[test]
fn dropping_the_watchdog_mid_phase_reports_nothing() {
    let (reports, reported) = mpsc::channel::<String>();
    let watchdog = PhaseWatchdog::spawn(move |label, _| {
        let _ = reports.send(label.to_string());
    });
    watchdog.start("interrupted".to_string(), Duration::from_millis(200));
    drop(watchdog);
    assert_eq!(
        reported.recv_timeout(Duration::from_secs(5)),
        Err(RecvTimeoutError::Disconnected),
        "a dropped watchdog reported its running phase"
    );
}

/// `abort_on_overrun` ends the process even while the stuck phase's thread holds stderr, so a
/// phase stuck in the middle of a write cannot keep the test job alive.
#[test]
fn an_overrun_aborts_the_process_while_the_stuck_thread_holds_stderr() {
    let (status, _) = run_abort_fixture("held");
    let status =
        status.expect("the watchdog did not end a process whose stuck thread holds stderr");
    assert!(!status.success(), "the fixture exited normally: {status}");
}

/// With stderr free, `abort_on_overrun` names the phase and its limit before it aborts.
#[test]
fn an_overrun_reports_the_phase_before_it_aborts() {
    let (status, stderr) = run_abort_fixture("free");
    let status = status.expect("the watchdog did not end the process");
    assert!(!status.success(), "the fixture exited normally: {status}");
    assert!(
        stderr.contains("phase `fixture phase` ran past its 200ms limit"),
        "the overrun report is missing: {stderr}"
    );
}

/// Run `abort_on_overrun_fixture` in a child process for at most 20 s. Return how it ended, or
/// `None` if it was still running, and its stderr.
fn run_abort_fixture(mode: &str) -> (Option<ExitStatus>, String) {
    let module = module_path!().split_once("::").map_or(module_path!(), |(_, rest)| rest);
    let name = format!("{module}::abort_on_overrun_fixture");
    let mut child = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", name.as_str(), "--ignored", "--nocapture"])
        .env(ABORT_FIXTURE_MODE, mode)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the abort fixture");
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll the abort fixture") {
            // When: `try_wait` returned `status`, the fixture process has ended.
            break Some(status);
        }
        if Instant::now() >= deadline {
            // When: `deadline` passed with the fixture still running; stop it and report `None`.
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let mut stderr = String::new();
    let _ = child.stderr.take().expect("fixture stderr").read_to_string(&mut stderr);
    (status, stderr)
}

/// Child side of the abort tests: start a phase that never ends, holding stderr when the mode is
/// `held`, and leave the process to `abort_on_overrun`. It does nothing when run directly.
#[test]
#[ignore = "fixture that aborts its own process; run only through the abort tests"]
fn abort_on_overrun_fixture() {
    let Ok(mode) = std::env::var(ABORT_FIXTURE_MODE) else {
        // When: `ABORT_FIXTURE_MODE` is unset, the fixture was run directly, so it does nothing.
        return;
    };
    let watchdog = PhaseWatchdog::spawn(abort_on_overrun);
    let stderr = std::io::stderr();
    let _held_stderr = (mode == "held").then(|| stderr.lock());
    watchdog.start("fixture phase".to_string(), Duration::from_millis(200));
    thread::sleep(Duration::from_secs(60));
}
