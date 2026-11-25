//! The F-ConPTY fixture's clocks and bounded input, kept free of native calls so every platform
//! tests them.
//!
//! [`CaseClock`] gives each case one absolute deadline, fixed when its first section opens; no
//! later sentinel extends it, and a chunk received at or past the deadline in force is refused
//! whole, before any sentinel in it can close, abandon or open a section. [`RunClock`] is the
//! outer test's one total deadline over staging and both hosts, with a teardown allowance held
//! back after every host. [`reap`] and [`abandon`] spend one teardown deadline across their steps:
//! a failed step fails, and only a poll that begins before the deadline counts. [`settle_input`] and
//! [`collect_input`] bound the child's input by time and bytes; a reply that overflows or never
//! goes quiet is reported as such, never as a cut or half reply. Each reads its clock through a
//! seam ([`Teardown::now`], [`Unjobbed::now`], [`InputSource`]), so the tests run them on a scripted virtual clock.
//!
//! Quiet input reduces carry-over; it does not prove which case produced a reply. A reply delayed
//! outside the child's queue for longer than [`SETTLE_QUIET`] can still be read by the next echo
//! case, so echo rows inject distinct bytes, and a carried reply then reads as a mismatch.

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
/// The least scanning a host is launched with. A runtime that answers no query spends a 2 s read
/// and a settle on every echo case, so the longest case list needs most of a full host's loop.
pub(crate) const MIN_LOOP_BUDGET: Duration = Duration::from_secs(45);
/// One host's teardown: exit, job termination and an empty job, together.
pub(crate) const TEARDOWN_BUDGET: Duration = Duration::from_secs(5);
/// The part of a teardown a host gets to exit on its own before its job is ended.
pub(crate) const NATURAL_EXIT: Duration = Duration::from_secs(2);
/// Silence that shows earlier input has stopped before a case reads its reply.
pub(crate) const SETTLE_QUIET: Duration = Duration::from_millis(300);
/// The most a settle waits for that silence.
pub(crate) const SETTLE_DEADLINE: Duration = Duration::from_secs(1);
/// The most a settle discards; more means a flood the case cannot read past.
pub(crate) const MAX_SETTLE_BYTES: usize = 64 * 1024;
/// How long a case reads its reply.
pub(crate) const READ_DEADLINE: Duration = Duration::from_secs(2);
/// The silence that ends a reply once bytes arrived.
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

    /// Whether a chunk received at `received_at` may be scanned at all. Checked against the
    /// deadline in force before the chunk, so no sentinel inside a late chunk can end the section
    /// whose deadline refuses it and then open another.
    pub(crate) fn admits(&self, received_at: Instant) -> Arrival {
        if received_at >= self.deadline() {
            // When: received_at is at or past the deadline in force, the whole chunk is refused.
            return Arrival::Late;
        }
        Arrival::InTime
    }

    /// Record section `section` opening in a chunk received at `received_at`. A case's end is
    /// fixed at its first opening; a later section of the same case keeps it.
    pub(crate) fn opened(&mut self, section: &str, received_at: Instant) -> Arrival {
        if self.admits(received_at) == Arrival::Late {
            // When: admits says Arrival::Late, the opening is refused.
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

    /// The case whose section is open, if any.
    pub(crate) fn open_case(&self) -> Option<&str> {
        self.open.as_deref()
    }

    /// Record the open section closing in a chunk received at `received_at`.
    pub(crate) fn closed(&mut self, received_at: Instant) -> Arrival {
        self.ended(received_at)
    }

    /// Record the open section abandoned (another sentinel came first) at `received_at`.
    pub(crate) fn abandoned(&mut self, received_at: Instant) -> Arrival {
        self.ended(received_at)
    }

    /// End the open section at `received_at`, unless that is past the deadline in force.
    fn ended(&mut self, received_at: Instant) -> Arrival {
        if self.admits(received_at) == Arrival::Late {
            // When: admits says Arrival::Late, the section's end is refused and nothing moves.
            return Arrival::Late;
        }
        self.open = None;
        self.last_sentinel = received_at;
        Arrival::InTime
    }
}

/// The case a section belongs to: `name` in `name/out` or `name/in`.
fn case_of(section: &str) -> &str {
    section.rsplit_once('/').map_or(section, |(case, _)| case)
}

/// Why a host's scan ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ScanEnd {
    /// The child's `DONE` arrived in time.
    Done,
    /// A deadline passed, with the named case's section open or none.
    Expired(Option<String>),
    /// The child's output ended before `DONE`.
    Disconnected,
}

/// Why a scan that reached a deadline ended, given the PTY output channel at that moment and
/// whether the driver already holds a late chunk it refused (`holding_late`). A late chunk, held or
/// still queued, is never scanned and might have held `DONE`, so the scan stays `Expired` with
/// `open_case`, the case whose section was open, wherever that chunk was when the deadline came.
/// Only a channel already closed with nothing held or queued means the child's output ended
/// before `DONE`, even if the scanner noticed only after the deadline, so that is `Disconnected`.
pub(crate) fn expiry_end<T>(
    output: &crossbeam_channel::Receiver<T>,
    open_case: Option<&str>,
    holding_late: bool,
) -> ScanEnd {
    let expired = || ScanEnd::Expired(open_case.map(str::to_owned));
    if holding_late {
        // When: holding_late, the refused chunk might have held DONE, so the end is a timeout.
        return expired();
    }
    match output.try_recv() {
        Err(crossbeam_channel::TryRecvError::Disconnected) => ScanEnd::Disconnected,
        Ok(_) | Err(crossbeam_channel::TryRecvError::Empty) => expired(),
    }
}

/// The `scan` field of a host's end line: `done` for a timely `DONE`; `required` only when the
/// deadline passed inside the last case, that case is scan-optional, and every other case's
/// sections closed (`others_closed`); `expired` for anything else, a disconnect included.
pub(crate) fn scan_status(
    end: &ScanEnd,
    last_optional: Option<&str>,
    others_closed: bool,
) -> &'static str {
    match end {
        ScanEnd::Done => "done",
        ScanEnd::Expired(Some(open)) if Some(open.as_str()) == last_optional && others_closed => {
            "required"
        }
        ScanEnd::Expired(_) | ScanEnd::Disconnected => "expired",
    }
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

    /// The end of the whole run; no teardown or cleanup goes past it.
    pub(crate) fn total_deadline(&self) -> Instant {
        self.total_deadline
    }

    /// Admit a host at `now`, or `None` when what is left cannot hold [`MIN_LOOP_BUDGET`] of
    /// scanning and the report after it: a host that could not reach its rows is never started.
    pub(crate) fn launch(&self, now: Instant) -> Option<Launch> {
        let left = self.work_deadline().checked_duration_since(now)?.min(HOST_BUDGET);
        let loop_budget =
            left.checked_sub(REPORT_MARGIN).filter(|budget| *budget >= MIN_LOOP_BUDGET)?;
        Some(Launch { host_deadline: now + left, loop_budget })
    }

    /// The teardown deadline for a teardown starting at `now`: one allowance, never past the run.
    pub(crate) fn teardown_deadline(&self, now: Instant) -> Instant {
        (now + TEARDOWN_BUDGET).min(self.total_deadline)
    }
}

/// The native steps of one host's teardown, so a test can fail any of them.
pub(crate) trait Teardown {
    /// Whether the host process has exited and been reaped.
    fn host_exited(&mut self) -> bool;
    /// End every process in the host's job; `false` when the native call failed.
    fn terminate_job(&mut self) -> bool;
    /// Whether the job holds no live process.
    fn job_empty(&mut self) -> bool;
    /// The clock the teardown's waits read; a test scripts it.
    fn now(&mut self) -> Instant {
        Instant::now()
    }
    /// Pause between two polls of a wait.
    fn pause(&mut self, length: Duration) {
        std::thread::sleep(length);
    }
}

/// The longest pause between two polls of a wait.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Poll `ready` until it holds or `deadline` passes; return whether it held. The clock is read
/// before each poll, so a poll that starts at or past `deadline` never counts.
pub(crate) fn wait_until(deadline: Instant, mut ready: impl FnMut() -> bool) -> bool {
    poll_until(
        &mut (),
        deadline,
        |_| Instant::now(),
        |_, length| std::thread::sleep(length),
        |_| ready(),
    )
}

/// [`wait_until`] over `subject`, reading its clock with `now` and pausing with `pause`.
fn poll_until<S: ?Sized>(
    subject: &mut S,
    deadline: Instant,
    now: impl Fn(&mut S) -> Instant,
    pause: impl Fn(&mut S, Duration),
    mut ready: impl FnMut(&mut S) -> bool,
) -> bool {
    loop {
        let Some(left) =
            deadline.checked_duration_since(now(subject)).filter(|left| !left.is_zero())
        else {
            // When: checked_duration_since finds the deadline reached, the wait failed.
            return false;
        };
        if ready(subject) {
            // When: ready() holds on a poll that began before the deadline, the wait succeeded.
            return true;
        }
        pause(subject, left.min(POLL_INTERVAL));
    }
}

/// [`wait_until`] on the teardown's own clock, polling `ready` against `steps`.
fn wait_steps<T: Teardown + ?Sized>(
    steps: &mut T,
    deadline: Instant,
    ready: impl FnMut(&mut T) -> bool,
) -> bool {
    poll_until(steps, deadline, |steps| steps.now(), |steps, length| steps.pause(length), ready)
}

/// Tear one host down within `deadline`: give it [`NATURAL_EXIT`] to exit, end its job if it
/// did not, and require that it exited and its job emptied on that same deadline. A failed job
/// end is a failure even when the host later exits.
pub(crate) fn reap(steps: &mut impl Teardown, started: Instant, deadline: Instant) -> bool {
    reap_within(steps, (started + NATURAL_EXIT).min(deadline), deadline)
}

/// [`reap`] with the end of the natural-exit wait given as `natural`.
pub(crate) fn reap_within(steps: &mut impl Teardown, natural: Instant, deadline: Instant) -> bool {
    if !wait_steps(steps, natural, |steps| steps.host_exited()) && !steps.terminate_job() {
        // When: terminate_job failed for a host still running, the teardown failed.
        return false;
    }
    // Both must hold before the deadline: a surviving host or a job still holding a process fails.
    wait_steps(steps, deadline, |steps| steps.host_exited())
        && wait_steps(steps, deadline, |steps| steps.job_empty())
}

/// The native steps for a host that never joined its job.
pub(crate) trait Unjobbed {
    /// Kill the host; `false` when the call failed.
    fn kill(&mut self) -> bool;
    /// Whether the host has exited and been reaped.
    fn exited(&mut self) -> bool;
    /// The clock the reap wait reads; a test scripts it.
    fn now(&mut self) -> Instant {
        Instant::now()
    }
    /// Pause between two polls of the reap wait.
    fn pause(&mut self, length: Duration) {
        std::thread::sleep(length);
    }
}

/// Kill and reap a host outside its job within `deadline`; return (killed, reaped), where reaped
/// means it was seen exited on a poll that began before the deadline.
pub(crate) fn abandon<H: Unjobbed + ?Sized>(host: &mut H, deadline: Instant) -> (bool, bool) {
    let killed = host.kill();
    let reaped = poll_until(
        host,
        deadline,
        |host| host.now(),
        |host, length| host.pause(length),
        |host| host.exited(),
    );
    (killed, reaped)
}

/// Whether the host's stdin `read` gave exactly the `go` line that confirms its job object.
pub(crate) fn confirmed(read: &std::io::Result<usize>, line: &str) -> bool {
    read.is_ok() && line == "go\n"
}

/// What a case read as its reply.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Input {
    /// Every byte, at most [`MAX_READ_BYTES`], ended by a whole quiet window or a closed reader.
    Bytes(Vec<u8>),
    /// More arrived than a case keeps, or earlier input never settled: no reply can be trusted.
    Overflow,
    /// Bytes were still arriving when the read deadline came: the reply may be incomplete.
    Unsettled,
}

/// [`settle_input_within`] with the fixture's bounds.
pub(crate) fn settle_input(receiver: &Receiver<Vec<u8>>) -> bool {
    settle_input_within(receiver, SETTLE_DEADLINE, SETTLE_QUIET)
}

/// What the child's input reads need: a clock and a timed receive, so a test can script both.
pub(crate) trait InputSource {
    /// The current instant.
    fn now(&mut self) -> Instant;
    /// Wait at most `wait` for the next chunk.
    fn recv(&mut self, wait: Duration) -> Result<Vec<u8>, RecvTimeoutError>;
}

/// The child's real input: the monotonic clock and its reader thread's channel.
pub(crate) struct ChannelInput<'a>(pub(crate) &'a Receiver<Vec<u8>>);

impl InputSource for ChannelInput<'_> {
    fn now(&mut self) -> Instant {
        Instant::now()
    }

    fn recv(&mut self, wait: Duration) -> Result<Vec<u8>, RecvTimeoutError> {
        self.0.recv_timeout(wait)
    }
}

/// [`settle_input_from`] over the real channel and clock.
pub(crate) fn settle_input_within(
    receiver: &Receiver<Vec<u8>>,
    limit: Duration,
    quiet: Duration,
) -> bool {
    settle_input_from(&mut ChannelInput(receiver), limit, quiet)
}

/// Discard earlier input until `quiet` passes with none, within `limit` and
/// [`MAX_SETTLE_BYTES`]; return whether it settled. A disconnected reader is settled.
pub(crate) fn settle_input_from(
    source: &mut impl InputSource,
    limit: Duration,
    quiet: Duration,
) -> bool {
    let deadline = source.now() + limit;
    let mut discarded = 0usize;
    loop {
        let Some(left) = deadline.checked_duration_since(source.now()) else {
            // When: checked_duration_since finds the deadline passed, input is still arriving.
            return false;
        };
        let wait = left.min(quiet);
        match source.recv(wait) {
            Ok(bytes) => {
                // When: recv gave Ok bytes, they are a leftover to discard and count.
                discarded += bytes.len();
                if discarded > MAX_SETTLE_BYTES {
                    // When: discarded passed MAX_SETTLE_BYTES, this is a flood, not a leftover.
                    return false;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                // When: a Timeout ends a whole quiet window (wait == quiet), input stopped; a
                // window the deadline cut short proves nothing.
                return wait == quiet;
            }
            Err(RecvTimeoutError::Disconnected) => {
                // When: the reader is Disconnected, nothing more can arrive.
                return true;
            }
        }
    }
}

/// [`collect_input_within`] with the fixture's bounds.
pub(crate) fn collect_input(receiver: &Receiver<Vec<u8>>) -> Input {
    collect_input_within(receiver, READ_DEADLINE, READ_QUIET)
}

/// [`collect_input_from`] over the real channel and clock.
pub(crate) fn collect_input_within(
    receiver: &Receiver<Vec<u8>>,
    limit: Duration,
    quiet: Duration,
) -> Input {
    collect_input_from(&mut ChannelInput(receiver), limit, quiet)
}

/// The bytes received within `limit`, ended by `quiet` after the last one. The buffer is sized
/// once; a chunk that would pass [`MAX_READ_BYTES`] is an overflow, and bytes still arriving at
/// `limit` are unsettled, so neither reads as a whole reply.
pub(crate) fn collect_input_from(
    source: &mut impl InputSource,
    limit: Duration,
    quiet: Duration,
) -> Input {
    let deadline = source.now() + limit;
    let mut received = Vec::with_capacity(MAX_READ_BYTES);
    loop {
        let Some(left) =
            deadline.checked_duration_since(source.now()).filter(|left| !left.is_zero())
        else {
            // When: checked_duration_since finds the deadline reached, any bytes are unsettled.
            if received.is_empty() {
                // When: received is empty, no reply came at all: an empty reply.
                return Input::Bytes(received);
            }
            return Input::Unsettled;
        };
        let wait = if received.is_empty() {
            left
        } else {
            // When: some input was received, the reply ends after a quiet window of silence.
            left.min(quiet)
        };
        match source.recv(wait) {
            Ok(bytes) => {
                // When: recv gave Ok bytes, they join the reply within MAX_READ_BYTES.
                if received.len() + bytes.len() > MAX_READ_BYTES {
                    // When: this chunk would pass MAX_READ_BYTES, the reply is not kept at all.
                    return Input::Overflow;
                }
                received.extend_from_slice(&bytes);
            }
            Err(RecvTimeoutError::Timeout) if received.is_empty() || wait == quiet => {
                // When: received.is_empty() or wait == quiet, this Timeout ends a whole reply.
                return Input::Bytes(received);
            }
            Err(RecvTimeoutError::Timeout) => {
                // When: any other Timeout, the deadline cut the quiet window short: still arriving.
                return Input::Unsettled;
            }
            Err(RecvTimeoutError::Disconnected) => {
                // When: the reader is Disconnected, nothing more can arrive; the reply is whole.
                return Input::Bytes(received);
            }
        }
    }
}

#[cfg(test)]
#[path = "conpty_fixture_clock_tests.rs"]
mod conpty_fixture_clock_tests;
