//! Native probe watchdog ownership and bounded expiry checks. regression contracts.

use super::*;

/// Expiry names the stalled native step and distinguishes a delivered wake from an unresponsive loop.
#[test]
fn native_watchdog_expiry_reports_phase_lock_and_wake_progress() {
    use std::time::{Duration, Instant};

    for reaches_about_to_wait in [false, true] {
        let (progress, events) = NativeProbeProgress::new();
        progress.set("hover", "child/OSC8", "PointerContendedActive");
        progress.parser_lock(true);
        let mut report = Vec::new();
        let mut wakes = 0;
        let mut exit_code = None;
        run_native_watchdog(
            &progress,
            &events,
            Instant::now() + Duration::from_millis(2),
            || {
                wakes += 1;
                if reaches_about_to_wait {
                    progress.about_to_wait();
                }
                true
            },
            &mut report,
            |code| exit_code = Some(code),
        );
        let report = String::from_utf8(report).unwrap();
        assert!(
            report.contains("probe=hover case=child/OSC8 phase=PointerContendedActive"),
            "{report}"
        );
        assert!(report.contains("parser_lock_held=true"), "{report}");
        assert!(
            report.contains(&format!("about_to_wait_after_wake={reaches_about_to_wait}")),
            "{report}"
        );
        assert_eq!(wakes, 1);
        assert!(exit_code.is_some_and(|code| code != 0), "{exit_code:?}");
    }
}

/// A delivered heartbeat counts as progress even when the clock cannot order its timestamp after the wake.
#[test]
fn native_watchdog_detects_heartbeat_without_a_clock_tick() {
    use std::time::{Duration, Instant};

    let (progress, events) = NativeProbeProgress::new();
    let before_watchdog = Instant::now();
    let mut report = Vec::new();
    let mut wakes = 0;
    let mut exit_code = None;
    run_native_watchdog(
        &progress,
        &events,
        Instant::now() + Duration::from_millis(2),
        || {
            wakes += 1;
            progress.about_to_wait();
            // Freeze the recorded time before the watchdog so callback ordering cannot depend on clock resolution.
            progress.state.lock().unwrap().last_about_to_wait = Some(before_watchdog);
            true
        },
        &mut report,
        |code| exit_code = Some(code),
    );
    let report = String::from_utf8(report).unwrap();
    assert!(report.contains("about_to_wait_after_wake=true"), "{report}");
    assert_eq!(wakes, 1);
    assert!(exit_code.is_some_and(|code| code != 0), "{exit_code:?}");
}

/// A heartbeat recorded before the wake is not a response to it, even when it arrives after the watchdog starts.
#[test]
fn native_watchdog_does_not_count_pre_wake_heartbeat_as_response() {
    use std::time::{Duration, Instant};

    // The first report write runs after the deadline wait and before the wake baseline is sampled.
    struct PreWakeHeartbeat<'a> {
        first_write: Option<&'a NativeProbeProgress>,
        bytes: Vec<u8>,
    }
    impl std::io::Write for PreWakeHeartbeat<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Some(progress) = self.first_write.take() {
                progress.about_to_wait();
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (progress, events) = NativeProbeProgress::new();
    let mut report = PreWakeHeartbeat { first_write: Some(&progress), bytes: Vec::new() };
    let mut wakes = 0;
    let mut exit_code = None;
    run_native_watchdog(
        &progress,
        &events,
        Instant::now() + Duration::from_millis(2),
        || {
            wakes += 1;
            assert_eq!(
                progress.state.lock().unwrap().heartbeat_count,
                1,
                "heartbeat precedes wake"
            );
            true
        },
        &mut report,
        |code| exit_code = Some(code),
    );
    let report = String::from_utf8(report.bytes).unwrap();
    assert!(report.contains("about_to_wait_after_wake=false"), "{report}");
    assert_eq!(wakes, 1);
    assert!(exit_code.is_some_and(|code| code != 0), "{exit_code:?}");
}

/// Once the deadline expires, a loop that returns during the wake grace period still fails instead of hiding the stall.
#[test]
fn native_watchdog_expiry_cannot_be_erased_by_a_late_disarm() {
    use std::time::{Duration, Instant};

    let (progress, events) = NativeProbeProgress::new();
    let mut report = Vec::new();
    let mut exit_code = None;
    run_native_watchdog(
        &progress,
        &events,
        Instant::now() + Duration::from_millis(2),
        || {
            progress.events.send(NativeWatchdogEvent::Disarm).unwrap();
            true
        },
        &mut report,
        |code| exit_code = Some(code),
    );
    assert!(exit_code.is_some_and(|code| code != 0), "{exit_code:?}");
    assert!(String::from_utf8(report).unwrap().contains("about_to_wait_after_wake=false"));
}

/// Dropping the guard cancels its channel wait without waking, reporting, or invoking the exit action.
#[test]
fn native_watchdog_guard_disarms_before_deadline() {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    // Count writes rather than their content: a passing fixture must not produce even a partial report.
    struct ReportWrites(Arc<Mutex<usize>>);
    impl std::io::Write for ReportWrites {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            *self.0.lock().unwrap() += 1;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let writes = Arc::new(Mutex::new(0));
    let exit_code = Arc::new(Mutex::new(None));
    let exit_result = exit_code.clone();
    let (progress, events) = NativeProbeProgress::new();
    let guard = NativeWatchdog::start(
        progress,
        events,
        Instant::now() + Duration::from_secs(180),
        || panic!("a disarmed watchdog must not wake the event loop"),
        ReportWrites(writes.clone()),
        move |code| *exit_result.lock().unwrap() = Some(code),
    );
    drop(guard);
    assert_eq!(*writes.lock().unwrap(), 0);
    assert_eq!(*exit_code.lock().unwrap(), None);
}
