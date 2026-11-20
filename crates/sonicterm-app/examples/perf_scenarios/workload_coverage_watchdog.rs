#![cfg(test)]
//! The deadline a native coverage run in a child process gets, and the verdict on how it ended.
//! Kept apart from the Windows fixture so its rules run as unit tests on every host.

use std::io;
use std::time::Duration;

/// How a supervised child ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ChildExit {
    /// Whether it exited successfully.
    pub(super) success: bool,
    /// Its exit status as the OS describes it.
    pub(super) status: String,
}

/// The process the watchdog supervises: a real child, or a scripted one in tests.
pub(super) trait Supervised {
    /// Its exit, once it has exited and been reaped; `None` while it runs.
    fn try_wait(&mut self) -> io::Result<Option<ChildExit>>;
    /// Ask the OS to end it.
    fn kill(&mut self) -> io::Result<()>;
    /// Its process id, named when it cannot be stopped.
    fn pid(&self) -> u32;
}

/// Elapsed time and waiting, real in production and virtual in tests.
pub(super) trait WatchClock {
    /// Time since supervision began.
    fn elapsed(&self) -> Duration;
    /// Wait `step` before the next check.
    fn sleep(&mut self, step: Duration);
}

/// What the coverage run proved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CoverageVerdict {
    /// The child exited successfully: its own assertions passed.
    Passed,
    /// A native call blocked past the deadline, and the child was stopped and reaped.
    HostIncapable,
    /// The child failed, or it could not be stopped or reaped, which leaves a live process behind.
    Failed(String),
}

/// The bounds one supervision runs under.
#[derive(Debug, Clone, Copy)]
pub(super) struct WatchBounds {
    /// How long the child may run before it counts as blocked.
    pub(super) deadline: Duration,
    /// How long a killed child has to be reaped.
    pub(super) reap: Duration,
    /// How often the child is checked.
    pub(super) poll: Duration,
}

/// Supervise `child` until it exits or `bounds.deadline` passes. A deadline is `HostIncapable`
/// only once the child was killed and reaped within `bounds.reap`; a failed kill or a child still
/// running after it is a failure, so a stuck process is never reported as a host limit.
pub(super) fn supervise(
    child: &mut impl Supervised,
    clock: &mut impl WatchClock,
    bounds: WatchBounds,
) -> CoverageVerdict {
    loop {
        match child.try_wait() {
            Ok(Some(exit)) if exit.success => return CoverageVerdict::Passed,
            Ok(Some(exit)) => {
                return CoverageVerdict::Failed(format!(
                    "the coverage child failed: {}",
                    exit.status
                ));
            }
            Ok(None) => {}
            Err(error) => {
                return CoverageVerdict::Failed(format!("the coverage child's status: {error}"));
            }
        }
        if clock.elapsed() > bounds.deadline {
            // When: the child ran past `deadline`, a native call is taken to have blocked; it is
            // stopped before any verdict.
            break;
        }
        clock.sleep(bounds.poll);
    }
    if let Err(error) = child.kill() {
        // When: `kill` failed, the child may still run, which no host limit excuses.
        return CoverageVerdict::Failed(format!(
            "could not stop coverage child pid {}: {error}",
            child.pid()
        ));
    }
    let reap_until = clock.elapsed() + bounds.reap;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return CoverageVerdict::HostIncapable,
            Ok(None) if clock.elapsed() < reap_until => clock.sleep(bounds.poll),
            Ok(None) => {
                return CoverageVerdict::Failed(format!(
                    "coverage child pid {} still running after kill",
                    child.pid()
                ));
            }
            Err(error) => {
                return CoverageVerdict::Failed(format!(
                    "coverage child pid {} could not be reaped: {error}",
                    child.pid()
                ));
            }
        }
    }
}

#[cfg(test)]
#[path = "workload_coverage_watchdog_tests.rs"]
mod workload_coverage_watchdog_tests;
