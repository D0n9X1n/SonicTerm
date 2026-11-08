//! `run_command`: start a command at a shell prompt and learn how it ended.
//!
//! The loop checks the precondition under the pane's parser lock, pins the prompt region with
//! `Grid::pin_run`, resets the pane's first-typing stamp and writes the command. The worker then
//! polls the pinned record until it completes, is evicted, sees typing before C, or times out.
//! The returned [`RunStarted`] holds the reservation: dropping it, delivered or not, frees the pane.
//! A terminal has no input-consumption barrier, so the precondition is best effort: it refuses
//! when a draft is visible, input is queued, or anyone typed while the previous command ran.

use std::sync::atomic::Ordering;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use sonicterm_grid::grid::{Grid, PinnedRun};

use super::control_loop::{has_control_character, Refusal};
use super::read::{collect_span, ReadLimits, RowSpan};
use super::{CallScope, LoopReply, McpShared, PaneEntry};
use crate::app::input_dispatch::PtyInputSource;
use crate::app::App;

/// The default and largest `run_command` timeouts.
pub(crate) const DEFAULT_RUN_WAIT: Duration = Duration::from_secs(60);
/// The largest timeout a caller may ask for.
pub(crate) const MAX_RUN_WAIT: Duration = Duration::from_secs(600);
/// How often the worker reads the pinned record.
const RUN_POLL: Duration = Duration::from_millis(50);
/// Longest a run wait sleeps on the pane's change signal before it rechecks typing, which
/// announces nothing, and revocation, which a missed wake would otherwise leave unseen.
const RUN_RECHECK: Duration = Duration::from_millis(250);

/// How long `finish` waits for the parser to read a run's output before reporting it truncated.
const FINISH_WAIT: Duration = Duration::from_millis(250);

/// The origin typing stamps count from, fixed by the first stamp taken.
static TYPING_ORIGIN: OnceLock<Instant> = OnceLock::new();

/// `at` as a typing stamp: nanoseconds past the process's typing origin, at least 1, because 0
/// means no typing since the run's reset.
pub(crate) fn typing_stamp(at: Instant) -> u64 {
    let origin = *TYPING_ORIGIN.get_or_init(|| at);
    u64::try_from(at.saturating_duration_since(origin).as_nanos()).unwrap_or(u64::MAX).max(1)
}

/// Record `at` as the first typing since the run's reset, before the bytes are queued, so the
/// stamp is published before the shell can read them; `true` when this call set it.
// Ordering: run_typed_at SeqCst exchange publishes the stamp before the PTY write that follows.
pub(crate) fn claim_typing_stamp(stamp: &std::sync::atomic::AtomicU64, at: Instant) -> bool {
    stamp.compare_exchange(0, typing_stamp(at), Ordering::SeqCst, Ordering::SeqCst).is_ok()
}

/// Take back a stamp [`claim_typing_stamp`] set for a write that was then refused, unless a newer
/// run already reset it.
// Ordering: run_typed_at SeqCst exchange clears only the stamp this write claimed.
pub(crate) fn release_typing_stamp(stamp: &std::sync::atomic::AtomicU64, at: Instant) {
    // A failed exchange means a run already reset the stamp, so there is nothing to take back.
    let _cleared = stamp.compare_exchange(typing_stamp(at), 0, Ordering::SeqCst, Ordering::SeqCst);
}

/// The instant a typing stamp names, or `None` for 0.
fn typing_instant(stamp: u64) -> Option<Instant> {
    let origin = *TYPING_ORIGIN.get()?;
    (stamp != 0).then(|| origin + Duration::from_nanos(stamp))
}

/// A run the loop started. It owns the pane's run reservation and the pin on region `seq`;
/// dropping it releases both, so a reply nobody received never leaves the pane busy.
pub(crate) struct RunStarted {
    /// The pane's handles.
    pub(crate) entry: PaneEntry,
    /// The pinned region's `seq`.
    pub(crate) seq: u64,
}

impl RunStarted {
    /// Release the pin on `grid` when it is still this run's.
    fn unpin_in(&self, grid: &mut Grid) -> Option<PinnedRun> {
        let ours = grid.pinned_run().is_some_and(|run| run.seq == self.seq);
        ours.then(|| grid.unpin_run()).flatten()
    }
}

// Lifecycle: RunStarted's drop unpins its region when the parser is free at once, then clears
// agent_run; a pin left behind is cleared by the next run's pin_and_write.
impl Drop for RunStarted {
    // Ordering: agent_run Release publishes the unpin before the pane is free again.
    fn drop(&mut self) {
        // A drop can run on the event loop, so it never waits for the parser.
        if let Some(mut parser) = self.entry.parser.try_lock() {
            self.unpin_in(parser.grid_mut());
        }
        self.entry.agent_run.store(false, Ordering::Release);
    }
}

/// Whether the prompt `grid` shows is ready for a command; `typed_since` says a typing write was
/// admitted at the given time.
pub(crate) fn check_prompt(grid: &Grid, last_typing: Option<Instant>) -> Result<u64, Refusal> {
    // At most `PROMPT_REGION_LIMIT` regions; only the newest two are read.
    let prompts: Vec<_> = grid.prompts().collect();
    let mut newest_first = prompts.iter().rev();
    let newest = newest_first.next().ok_or(Refusal::NoShellIntegration)?;
    let Some((input_row, input_col)) = newest.input_start else {
        // When: the newest region has no `input_start` (B), the prompt is not marked.
        return Err(Refusal::NoShellIntegration);
    };
    if newest.output_seen {
        // When: the newest region already saw C (`output_seen`), its command is still running.
        return Err(Refusal::Busy);
    }
    let previous_start = newest_first.next().and_then(|region| region.output_start_at);
    let typed_during_previous = match (last_typing, previous_start) {
        (None, _) => false,
        (Some(typed), Some(started)) => typed >= started,
        // A pane with no earlier command must never have been typed into.
        (Some(_), None) => true,
    };
    if typed_during_previous {
        // When: typing arrived `typed_during_previous` command, it may still be in flight.
        return Err(Refusal::DraftPresent);
    }
    let bottom = grid.scrollback_len() as u64 + u64::from(grid.rows);
    for abs in input_row..bottom {
        let Some(row) = grid.row_at_abs(abs) else {
            // When: `row_at_abs` has no row, the screen ended.
            break;
        };
        let from = draft_start(abs, input_row, input_col);
        if row.iter().skip(from).any(|cell| cell.ch != ' ' && cell.ch != '\0') {
            // When: a non-blank cell sits after `input_start`, a draft is present.
            return Err(Refusal::DraftPresent);
        }
    }
    Ok(newest.seq)
}

/// The first cell of row `abs` that can hold a draft: on the input row only the cells from
/// `input_start` on, on later rows every cell.
fn draft_start(abs: u64, input_row: u64, input_col: u16) -> usize {
    if abs == input_row {
        usize::from(input_col)
    } else {
        // When: `abs` is past the `input_row`, the whole row belongs to the input area.
        0
    }
}

impl App {
    /// The loop side of `run_command`: reserve the pane, check the prompt, pin its region, write
    /// the command and record the typing count. Any refusal releases what it took and writes
    /// nothing.
    // Ordering: agent_run AcqRel swap is the single reservation point; agent_run Release frees a
    // refused start; run_typed_at SeqCst publishes the reset.
    pub(crate) fn mcp_run_start(
        &mut self,
        access: super::agents::Access,
        pane_id: u64,
        command: &str,
    ) -> LoopReply {
        if has_control_character(command) {
            // When: `has_control_character` finds CR, LF, ESC or another control, nothing runs.
            return LoopReply::Refused(
                "invalid_command",
                "the command holds a control character".into(),
            );
        }
        if let Err(refusal) = self.authorize_control(access, pane_id, false) {
            // When: `authorize_control` refused the pane, nothing runs.
            return refusal.reply();
        }
        let Some(entry) = self.mcp_resolve_pane(pane_id) else {
            // When: the pane has no entry, it is gone.
            return Refusal::PaneGone.reply();
        };
        if entry.agent_run.swap(true, Ordering::AcqRel) {
            // When: `agent_run` was already set, another run owns the pane.
            return Refusal::Busy.reply();
        }
        // Typing from here on is measured against this run's C. The loop is the only writer of
        // pane input, so nothing is admitted between this reset and the command's write.
        entry.run_typed_at.store(0, Ordering::SeqCst);
        match self.pin_and_write(&entry, pane_id, command) {
            Ok(seq) => LoopReply::RunStarted(Box::new(RunStarted { entry, seq })),
            Err(refusal) => {
                // The start was refused, so the reservation is released.
                entry.agent_run.store(false, Ordering::Release);
                refusal.reply()
            }
        }
    }

    /// Check the prompt and pin its region under one parser lock, then queue the command and return
    /// the pinned `seq`. The pin is released when the write is refused.
    fn pin_and_write(
        &mut self,
        entry: &PaneEntry,
        pane_id: u64,
        command: &str,
    ) -> Result<u64, Refusal> {
        let (last_typing, queued) = self
            .pane_by_id(pane_id)
            .map(|pane| {
                let queued = pane.pty.as_ref().is_some_and(|pty| {
                    let diagnostics = pty.input_diagnostics();
                    diagnostics.queued_messages > 0 || diagnostics.in_flight_bytes > 0
                });
                (pane.last_input_at, queued)
            })
            .ok_or(Refusal::PaneGone)?;
        if queued {
            // When: input is `queued` or in flight, the shell has not consumed it yet.
            return Err(Refusal::InputBusy);
        }
        let seq = {
            // Bounded, so a parser the VT worker holds cannot stall the event loop: the start is
            // refused as `parser_busy` and the agent retries.
            let mut parser = crate::app::frame_counters::try_lock_parser_for(
                &entry.parser,
                super::read::PARSER_WAIT,
            )
            .ok_or(Refusal::ParserBusy)?;
            let seq = check_prompt(parser.grid(), last_typing)?;
            // This run holds the pane's reservation, so a pin still here is one an earlier run's
            // drop could not release without waiting.
            parser.grid_mut().unpin_run();
            if !parser.grid_mut().pin_run(seq) {
                // When: `pin_run` refused, another run is pinned on this screen.
                return Err(Refusal::Busy);
            }
            seq
        };
        let mut line = command.as_bytes().to_vec();
        line.push(b'\r');
        #[cfg(test)]
        let refused_write = AFTER_PIN.with_borrow(|hook| hook.as_ref().is_some_and(|hook| hook()));
        #[cfg(not(test))]
        let refused_write = false;
        let written = if refused_write {
            Err(crate::app::pane_target::TargetError::InputBusy)
        } else {
            // When: `refused_write` is false, the line is written to the pane.
            self.write_agent_input(pane_id, line, PtyInputSource::AgentRun)
        };
        if let Err(error) = written {
            // When: `written` is an `error`, nothing ran, so the run's pin is released.
            // A parser still busy keeps it; only this run's `seq` reads it, and the next start unpins.
            if let Some(mut parser) = crate::app::frame_counters::try_lock_parser_for(
                &entry.parser,
                super::read::PARSER_WAIT,
            ) {
                parser.grid_mut().unpin_run();
            }
            return Err(Refusal::from(error));
        }
        tracing::info!(target: "mcp", pane = pane_id, "agent run started");
        Ok(seq)
    }
}

#[cfg(test)]
thread_local! {
    /// A test's action once a start has pinned its run and before it writes; returning `true`
    /// refuses the write, to drive the cleanup branch.
    pub(crate) static AFTER_PIN: std::cell::RefCell<Option<Box<dyn Fn() -> bool>>> =
        const { std::cell::RefCell::new(None) };
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunState {
    /// C and then D arrived for the pinned region.
    Completed,
    /// D arrived without C: the shell ended the prompt without running a command, as after
    /// Ctrl+C at the prompt or a line the shell rejected.
    NotStarted,
    /// Someone else typed into the pane before the command's output began.
    Interfered,
    /// The pinned record was evicted, or the shell exited, before the command ended.
    Lost,
    /// The timeout passed first; the command may still be running.
    Running,
    /// The client went away or the server stopped or changed.
    Revoked,
    /// The pane was closed before the run ended.
    PaneClosed,
}

impl RunState {
    /// Every state, for the documentation check.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 7] = [
        Self::Completed,
        Self::NotStarted,
        Self::Interfered,
        Self::Lost,
        Self::Running,
        Self::Revoked,
        Self::PaneClosed,
    ];

    /// The state's spelling in results.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::NotStarted => "not_started",
            Self::Interfered => "interfered",
            Self::Lost => "lost",
            Self::Running => "running",
            Self::Revoked => "revoked",
            Self::PaneClosed => "pane_closed",
        }
    }
}

/// What the worker learned about a run.
pub(crate) struct RunOutcome {
    /// How it ended.
    pub(crate) state: RunState,
    /// The exit code the shell reported, for `Completed` and `NotStarted`.
    pub(crate) exit_code: Option<i32>,
    /// The output from the C row to the D row, for `Completed`.
    pub(crate) output: Vec<String>,
    /// The output hit a cap.
    pub(crate) truncated: bool,
    /// The C row already left history.
    pub(crate) output_truncated: bool,
    /// The exit code came from authenticated shell markers; see [`exit_verified`].
    pub(crate) exit_verified: bool,
}

/// Check the pinned record until the run ends, then read its result. Between checks the wait
/// sleeps on the pane's change signal, which output (the shell's markers), exit, pane close and
/// server revocation announce; a busy parser is retried on the short poll. Runs on the
/// connection's worker, never on the loop; `started` is dropped on return, which frees the pane.
// Ordering: run_typed_at SeqCst load pairs with claim_typing_stamp's exchange.
pub(crate) fn wait_run(
    shared: &McpShared,
    started: RunStarted,
    timeout: Duration,
    scope: &CallScope,
) -> RunOutcome {
    wait_run_every(shared, started, timeout, scope, RUN_RECHECK)
}

/// [`wait_run`] with its recheck interval given, so a test can lengthen it past any wake it checks.
// Ordering: run_typed_at SeqCst load pairs with claim_typing_stamp's exchange.
pub(crate) fn wait_run_every(
    shared: &McpShared,
    started: RunStarted,
    timeout: Duration,
    scope: &CallScope,
    recheck: Duration,
) -> RunOutcome {
    let deadline = Instant::now() + timeout;
    let entry = &started.entry;
    let _waiter = shared.register_waiter(&entry.change_signal);
    // When the run wait first saw the pane closed; a busy parser is retried at most this long.
    let mut closed_since: Option<Instant> = None;
    loop {
        #[cfg(test)]
        RUN_CHECKS.with(|checks| checks.set(checks.get() + 1));
        // Read before anything is inspected, so a change during the checks moves past it.
        let seen = entry.change_signal.sequence();
        // Read before the pinned record: markers published before the close are in the record read
        // below, so a run that ended before its pane closed is still reported as it ended.
        let closed = entry.change_signal.is_closed();
        if closed && closed_since.is_none() {
            closed_since = Some(Instant::now());
        }
        let revoked = scope.revoked(shared);
        let pinned = entry
            .parser
            .try_lock_for(super::read::PARSER_WAIT)
            .map(|guard| guard.grid().pinned_run().filter(|run| run.seq == started.seq));
        // Read after the pinned record: typing stamped before bytes that led to this C is
        // already visible here, because the stamp is published before the bytes are queued.
        let typed_at = typing_instant(entry.run_typed_at.load(Ordering::SeqCst));
        let exited = entry.exit_status.get().is_some();
        let busy = pinned.is_none();
        if let Some(state) = run_state(pinned.flatten(), typed_at, exited, revoked) {
            // When: `run_state` decided the run ended, its result is read below.
            return finish(&started, state, close_budget(closed_since));
        }
        let now = Instant::now();
        let drained = closed_since
            .is_some_and(|since| now.duration_since(since) >= super::tools_read::CLOSE_DRAIN);
        if closed && (!busy || drained) {
            // When: the pane was `closed` before its record was read and the run had not ended,
            // or the parser stayed `busy` through the `CLOSE_DRAIN`, the run can no longer end.
            return finish(&started, RunState::PaneClosed, close_budget(closed_since));
        }
        if now >= deadline {
            // When: the `deadline` passed, the run is reported as still running.
            return finish(&started, RunState::Running, close_budget(closed_since));
        }
        if busy {
            // When: the parser was `busy`, nothing announces its release, so retry on the short poll.
            std::thread::sleep(RUN_POLL.min(deadline - now));
            continue;
        }
        entry.change_signal.wait_past(seen, deadline.min(now + recheck));
    }
}

#[cfg(test)]
thread_local! {
    /// How many checks this thread's run waits made, so a test tells a signalled wake from polling.
    pub(crate) static RUN_CHECKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Whether a run ended, given its pinned record (`None` while the parser was busy or the pin is
/// gone), the first typing since the run began, whether the shell exited, and whether the request
/// was revoked. Typing at or before C means the command line may not be the agent's alone.
pub(crate) fn run_state(
    pinned: Option<PinnedRun>,
    typed_at: Option<Instant>,
    exited: bool,
    revoked: bool,
) -> Option<RunState> {
    if revoked {
        // When: the request was `revoked`, the run is abandoned.
        return Some(RunState::Revoked);
    }
    let run = pinned?;
    let typed_first = match (typed_at, run.output_at) {
        (None, _) => false,
        (Some(typed), Some(output)) => typed <= output,
        // Typing with no C yet came before any output.
        (Some(_), None) => true,
    };
    if typed_first {
        // When: typing came `typed_first`, before C, the command may not be the agent's.
        return Some(RunState::Interfered);
    }
    // Command output can print D itself, so D ends a run only when the pane's token proves the
    // shell sent the run's C and D, or once the shell's next prompt followed it.
    if run.exit.is_some() && (exit_verified(&run) || run.prompt_after_end) {
        // When: `run.exit` is set and `exit_verified` or `prompt_after_end` holds, D is trusted or
        // closed by the shell's next prompt; without C nothing ran.
        return Some(if run.output_seen { RunState::Completed } else { RunState::NotStarted });
    }
    if run.evicted || exited {
        // When: the record was `evicted` or the shell `exited`, D can no longer arrive.
        return Some(RunState::Lost);
    }
    None
}

/// Whether `run`'s exit code is verified: its own C and latest D both carried the pane's shell
/// token. Any other exit code was read from terminal output a program could have printed.
pub(crate) fn exit_verified(run: &PinnedRun) -> bool {
    run.output_authenticated && run.end_authenticated
}

/// How long [`finish`] may wait for the parser: [`FINISH_WAIT`] normally, and only what is left of
/// the [`CLOSE_DRAIN`](super::tools_read::CLOSE_DRAIN) once the pane was seen closed, so a closed
/// pane's run returns within that one bound however busy its parser stays.
fn close_budget(closed_since: Option<Instant>) -> Duration {
    closed_since.map_or(FINISH_WAIT, |since| {
        super::tools_read::CLOSE_DRAIN.saturating_sub(since.elapsed()).min(FINISH_WAIT)
    })
}

/// Release the pin and, for a completed run, read its output from the C row to the D row. The
/// parser wait is bounded by `wait`; on a miss the output is reported truncated, and the drop of
/// `started` frees the pane either way.
fn finish(started: &RunStarted, state: RunState, wait: Duration) -> RunOutcome {
    let mut outcome = RunOutcome {
        state,
        exit_code: None,
        output: Vec::new(),
        truncated: false,
        output_truncated: false,
        exit_verified: false,
    };
    let Some(mut parser) = started.entry.parser.try_lock_for(wait) else {
        // When: the parser stayed busy past `wait`, the output cannot be read in time.
        outcome.truncated = state == RunState::Completed;
        return outcome;
    };
    let run = started.unpin_in(parser.grid_mut());
    let Some(run) = run.filter(|_| matches!(state, RunState::Completed | RunState::NotStarted))
    else {
        // When: the run neither completed nor ended without starting, there is nothing to read.
        return outcome;
    };
    outcome.exit_code = run.exit.flatten();
    outcome.exit_verified = exit_verified(&run);
    if state != RunState::Completed {
        // When: the `state` is `NotStarted`, no command printed anything.
        return outcome;
    }
    let live = parser.grid();
    // The run's rows live on the screen it was pinned on: the saved primary while an alternate
    // screen is up. A run pinned on an alternate screen that has closed has no rows left.
    let grid = if run.alt {
        live
    } else {
        // When: the run was not pinned on an `alt` screen, its rows are the primary screen's.
        live.primary_screen()
    };
    let start = run.output_start.and_then(|token| grid.resolve_row(token));
    let end = run.end.and_then(|token| grid.resolve_row(token));
    outcome.output_truncated = run.output_start.is_some() && start.is_none();
    // D at column 0 opens an empty row that holds none of the output.
    let last = end.and_then(|row| row.checked_sub(u64::from(run.end_col == 0)));
    if let Some(last) = last {
        // A C row that left history leaves the oldest row kept as the span's start.
        let start = start.unwrap_or(0);
        let last_cols = (run.end_col > 0).then_some(run.end_col);
        let span = RowSpan { start, last, last_cols };
        let (lines, truncated) = collect_span(grid, span, ReadLimits::clamped(None, None));
        outcome.output = lines;
        outcome.truncated = truncated;
    } else {
        // When: no `last` row resolved, the D row left every screen and the output is gone.
        outcome.output_truncated = true;
    }
    outcome
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod run_tests;

#[cfg(test)]
#[path = "run_bounded_lock_tests.rs"]
mod run_bounded_lock_tests;
