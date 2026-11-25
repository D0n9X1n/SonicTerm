//! The F-ConPTY fixture's clocks and bounded input, kept free of native calls so every platform
//! tests them.
//!
//! [`CaseClock`] gives each case one absolute deadline, fixed when its first section opens; no
//! later sentinel extends it, and evidence from a chunk received at or past it is refused.
//! [`RunClock`] is the outer test's one total deadline over staging and both hosts, with a
//! teardown allowance held back after every host. [`reap`] and [`abandon`] spend one teardown
//! deadline across their steps and report each failure. [`settle_input`] and [`collect_input`]
//! bound the child's input by time and bytes and report an overflow instead of a cut reply.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// One case's whole run, from its first section opening, however many sentinels it sends.
pub(crate) const CASE_BUDGET: Duration = Duration::from_secs(5);
/// Silence allowed while no section is open: before the first case and between cases.
pub(crate) const IDLE_BUDGET: Duration = Duration::from_secs(5);
/// The outer test's whole run: staging, both hosts and their teardowns.
pub(crate) const TOTAL_BUDGET: Duration = Duration::from_secs(150);
/// The most one host may run, launch to its end line.
pub(crate) const HOST_BUDGET: Duration = Duration::from_secs(60);
/// What a host keeps after its scanning loop to print its rows and end line.
pub(crate) const REPORT_MARGIN: Duration = Duration::from_secs(10);
/// One host's teardown: exit, job termination and an empty job, together.
pub(crate) const TEARDOWN_BUDGET: Duration = Duration::from_secs(5);
/// Silence that shows earlier input has stopped before a case reads its reply.
pub(crate) const SETTLE_QUIET: Duration = Duration::from_millis(300);
/// The most a settle waits for that silence.
pub(crate) const SETTLE_DEADLINE: Duration = Duration::from_secs(1);
/// The most a settle discards; more means a flood the case cannot read past.
pub(crate) const MAX_SETTLE_BYTES: usize = 64 * 1024;
/// How long a case reads its reply, and the silence that ends it once bytes arrived.
pub(crate) const READ_DEADLINE: Duration = Duration::from_secs(2);
pub(crate) const READ_QUIET: Duration = Duration::from_millis(300);
/// The most reply bytes a case keeps; one byte more is an overflow.
pub(crate) const MAX_READ_BYTES: usize = 64 * 1024;

/// Whether a sentinel's chunk arrived inside the deadline that governs it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Arrival {
    /// The evidence counts.
    InTime,
    /// The chunk arrived at or past the deadline: the evidence is refused.
    Late,
}

/// The host's deadlines: its own end, each case's fixed end, and silence while nothing is open.
pub(crate) struct CaseClock {
    host_deadline: Instant,
    /// Each case's end, set when its first section opened and never moved.
    case_deadlines: HashMap<String, Instant>,
    /// The case whose section is open.
    open: Option<String>,
    /// When the last sentinel's chunk arrived; silence is measured from it.
    last_sentinel: Instant,
}

impl CaseClock {
    /// A clock that starts at `started` and never runs past `host_deadline`.
    pub(crate) fn new(started: Instant, host_deadline: Instant) -> Self {
        Self { host_deadline, case_deadlines: HashMap::new(), open: None, last_sentinel: started }
    }

    /// The deadline in force: the open case's end, or the silence bound while none is open.
    pub(crate) fn deadline(&self) -> Instant {
        let governing = match &self.open {
            Some(case) => self.case_deadlines[case],
            None => self.last_sentinel + IDLE_BUDGET,
        };
        governing.min(self.host_deadline)
    }

    /// Record section `section` opening in a chunk received at `received_at`. A case's end is
    /// fixed at its first opening; a later section of the same case keeps it.
    pub(crate) fn opened(&mut self, section: &str, received_at: Instant) -> Arrival {
        if received_at >= self.deadline() {
            // When: received_at is at or past the deadline in force, the opening is refused.
            return Arrival::Late;
        }
        let case = case_of(section).to_owned();
        let case_deadline =
            *self.case_deadlines.entry(case.clone()).or_insert(received_at + CASE_BUDGET);
        if received_at >= case_deadline.min(self.host_deadline) {
            // When: received_at reached the case's fixed end, a reopened case is refused.
            return Arrival::Late;
        }
        self.open = Some(case);
        self.last_sentinel = received_at;
        Arrival::InTime
    }

    /// Record the open section closing in a chunk received at `received_at`.
    pub(crate) fn closed(&mut self, received_at: Instant) -> Arrival {
        if received_at >= self.deadline() {
            // When: received_at is at or past the open case's end, its evidence is refused.
            return Arrival::Late;
        }
        self.open = None;
        self.last_sentinel = received_at;
        Arrival::InTime
    }

    /// Record the open section abandoned (another sentinel came first) at `received_at`.
    pub(crate) fn abandoned(&mut self, received_at: Instant) {
        self.open = None;
        self.last_sentinel = received_at;
    }
}

/// The case a section belongs to: `name` in `name/out` or `name/in`.
fn case_of(section: &str) -> &str {
    section.rsplit_once('/').map_or(section, |(case, _)| case)
}

/// The outer test's one total deadline. Work (staging and each host's run) ends a teardown
/// allowance before it, so the last host's teardown still fits.
pub(crate) struct RunClock {
    total_deadline: Instant,
}

/// One host's admitted run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Launch {
    /// When the outer test stops waiting for the host's end line.
    pub(crate) host_deadline: Instant,
    /// How long the host's own scanning loop may run, so its rows arrive before that.
    pub(crate) loop_budget: Duration,
}

impl RunClock {
    /// A run that started at `started`.
    pub(crate) fn new(started: Instant) -> Self {
        Self { total_deadline: started + TOTAL_BUDGET }
    }

    /// When staging and host runs must end, leaving one teardown allowance.
    pub(crate) fn work_deadline(&self) -> Instant {
        self.total_deadline - TEARDOWN_BUDGET
    }

    /// Admit a host at `now`, or `None` when too little time is left for its loop and its report.
    pub(crate) fn launch(&self, now: Instant) -> Option<Launch> {
        let left = self.work_deadline().checked_duration_since(now)?.min(HOST_BUDGET);
        let loop_budget = left.checked_sub(REPORT_MARGIN).filter(|budget| !budget.is_zero())?;
        Some(Launch { host_deadline: now + left, loop_budget })
    }
}

/// The native steps of one host's teardown, so a test can fail any of them.
pub(crate) trait Teardown {
    /// Whether the host process has exited and been reaped.
    fn host_exited(&mut self) -> bool;
    /// End every process in the host's job.
    fn terminate_job(&mut self);
    /// Whether the job holds no live process.
    fn job_empty(&mut self) -> bool;
}

/// Poll `ready` every 20 ms until it holds or `deadline` passes; return whether it held.
pub(crate) fn wait_until(deadline: Instant, mut ready: impl FnMut() -> bool) -> bool {
    loop {
        if ready() {
            // When: ready() holds, the wait succeeded.
            return true;
        }
        if Instant::now() >= deadline {
            // When: now is at or past the deadline with ready() false, the wait failed.
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Tear one host down within `deadline`: wait for it to exit, end its job if it did not, and
/// require an empty job. Every step shares the one deadline; a failed step reports `false`.
pub(crate) fn reap(steps: &mut impl Teardown, deadline: Instant) -> bool {
    let mut exited = wait_until(deadline, || steps.host_exited());
    if !exited {
        // The host outlived its run: end its whole tree; it is already a failure.
        steps.terminate_job();
        exited = wait_until(deadline, || steps.host_exited());
    }
    // Both must hold: a host the job end did not reap, or a job still holding a process, fails.
    exited && wait_until(deadline, || steps.job_empty())
}

/// The native steps for a host that never joined its job.
pub(crate) trait Unjobbed {
    /// Kill the host; `false` when the call failed.
    fn kill(&mut self) -> bool;
    /// Whether the host has exited and been reaped.
    fn exited(&mut self) -> bool;
}

/// Kill and reap a host outside its job within `deadline`; return (killed, reaped).
pub(crate) fn abandon(host: &mut impl Unjobbed, deadline: Instant) -> (bool, bool) {
    let killed = host.kill();
    let reaped = wait_until(deadline, || host.exited());
    (killed, reaped)
}

/// Whether the host's stdin `read` gave exactly the `go` line that confirms its job object.
pub(crate) fn confirmed(read: &std::io::Result<usize>, line: &str) -> bool {
    read.is_ok() && line == "go\n"
}

/// What a case read as its reply.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Input {
    /// Every byte, at most [`MAX_READ_BYTES`].
    Bytes(Vec<u8>),
    /// More arrived than a case keeps, or earlier input never settled: no reply can be trusted.
    Overflow,
}

/// Discard earlier input until [`SETTLE_QUIET`] passes with none, within [`SETTLE_DEADLINE`] and
/// [`MAX_SETTLE_BYTES`]; return whether it settled. A disconnected reader is settled.
pub(crate) fn settle_input(receiver: &Receiver<Vec<u8>>) -> bool {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut discarded = 0usize;
    loop {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            // When: checked_duration_since finds the deadline passed, input is still arriving.
            return false;
        };
        let wait = left.min(SETTLE_QUIET);
        match receiver.recv_timeout(wait) {
            Ok(bytes) => {
                // When: recv_timeout gave Ok bytes, they are a leftover to discard and count.
                discarded += bytes.len();
                if discarded > MAX_SETTLE_BYTES {
                    // When: discarded passed MAX_SETTLE_BYTES, this is a flood, not a leftover.
                    return false;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                // When: a Timeout ends a whole SETTLE_QUIET window (wait == SETTLE_QUIET), input
                // stopped; a window the deadline cut short proves nothing.
                return wait == SETTLE_QUIET;
            }
            Err(RecvTimeoutError::Disconnected) => {
                // When: the reader is Disconnected, nothing more can arrive.
                return true;
            }
        }
    }
}

/// The bytes received within [`READ_DEADLINE`], ending [`READ_QUIET`] after the last one. The
/// buffer is sized once; a chunk that would pass [`MAX_READ_BYTES`] is an overflow, not a cut.
pub(crate) fn collect_input(receiver: &Receiver<Vec<u8>>) -> Input {
    let deadline = Instant::now() + READ_DEADLINE;
    let mut received = Vec::with_capacity(MAX_READ_BYTES);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let wait = if received.is_empty() {
            left
        } else {
            // When: some input was received, the reply ends after READ_QUIET of silence.
            left.min(READ_QUIET)
        };
        let Ok(bytes) = receiver.recv_timeout(wait) else {
            // When: recv_timeout expired (or the reader ended), the reply is complete.
            break;
        };
        if received.len() + bytes.len() > MAX_READ_BYTES {
            // When: this chunk would pass MAX_READ_BYTES, the reply is not kept at all.
            return Input::Overflow;
        }
        received.extend_from_slice(&bytes);
    }
    Input::Bytes(received)
}

#[cfg(test)]
#[path = "conpty_fixture_clock_tests.rs"]
mod conpty_fixture_clock_tests;
