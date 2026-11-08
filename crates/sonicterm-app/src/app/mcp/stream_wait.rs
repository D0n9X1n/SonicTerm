//! `wait_for` with `source: "stream"`: the regex runs against the text the pane printed since the
//! wait began, as the parser's output tap recorded it, rather than against screen rows. Text a
//! program overwrote in place with `\r`, or that scrolled away between checks, still matches.
//! The tap is armed under the parser lock when the wait starts, so its start offset and the
//! first recorded character agree; the wait's hold is released without that lock on every exit.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::jsonrpc::{tool_error, tool_result};
use super::tools_read::{WaitReason, CLOSE_DRAIN};
use super::{CallScope, McpShared, PaneEntry};

/// The longest a stream wait waits for the parser lock to arm its tap.
const ARM_WAIT: Duration = Duration::from_secs(1);

/// What a stream wait waits on; the screen wait's `after_row` has no meaning here.
pub(crate) struct StreamCondition {
    /// The pattern to find in the printed text.
    pub(crate) regex: regex::Regex,
    /// Stop when the process exits.
    pub(crate) watch_exit: bool,
    /// Stop when no text was printed for this long.
    pub(crate) quiet: Option<Duration>,
    /// Stop after this long regardless.
    pub(crate) timeout: Duration,
    /// The longest idle sleep between checks.
    pub(crate) recheck: Duration,
}

/// How a stream wait ended.
struct StreamOutcome {
    /// Why it stopped.
    reason: WaitReason,
    /// The printed line holding the match, for `Matched`.
    line: Option<String>,
    /// The tap offset the match starts at, for `Matched`.
    offset: Option<u64>,
    /// The exit code, for `Exited` with one.
    exit_code: Option<i64>,
    /// The ring dropped text printed during the wait before it was checked.
    context_truncated: bool,
    /// Output was published that the wait never read (its parser stayed busy), so a non-match
    /// proves nothing about it.
    unread: bool,
}

#[cfg(test)]
thread_local! {
    /// Passes of the wait loop on this thread, so a test can pin the retry pacing.
    pub(crate) static PASSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Run a stream wait on `entry` and form its tool result.
// Ordering: output_generation Acquire pairs with the VT worker's Release publish after each batch.
pub(crate) fn wait(
    shared: &McpShared,
    entry: &PaneEntry,
    condition: &StreamCondition,
    scope: &CallScope,
) -> Value {
    let Some(mut parser) = entry.parser.try_lock_for(ARM_WAIT) else {
        // When: the parser stayed busy past `ARM_WAIT`, the tap cannot be armed.
        return tool_error("busy", "the pane's parser stayed busy; try again");
    };
    // The hold releases its count on drop without the parser lock, so no exit path waits on it.
    let hold = parser.arm_output_tap();
    drop(parser);
    let outcome = run(shared, entry, hold.start, condition, scope);
    drop(hold);
    // The ring is freed now if the parser is free and no other wait holds it; a busy parser
    // leaves it to its next batch or the retention pass, so this never waits.
    if let Some(mut parser) = entry.parser.try_lock() {
        parser.release_idle_tap();
    }
    let incomplete = outcome.context_truncated || outcome.unread;
    let mut structured = json!({
        "reason": outcome.reason.as_str(),
        "source": "stream",
        "current_generation": entry.output_generation.load(Ordering::Acquire),
        "context_truncated": outcome.context_truncated,
        "output_incomplete": incomplete,
    });
    if let Some(line) = outcome.line {
        structured["line"] = json!(line);
    }
    if let Some(offset) = outcome.offset {
        structured["offset"] = json!(offset);
    }
    if let Some(code) = outcome.exit_code {
        structured["exit_code"] = json!(code);
    }
    tool_result(outcome.reason.as_str(), structured)
}

/// The wait loop: check the tap after each change the pane announces, until a match, exit, quiet,
/// close, revocation or the timeout. Output whose batch the wait has not read holds off `quiet`
/// and a closed pane's end, and is reported as unread if the wait ends first.
// Ordering: output_generation Acquire, loaded before each read, pairs with the VT worker's Release.
fn run(
    shared: &McpShared,
    entry: &PaneEntry,
    start: u64,
    condition: &StreamCondition,
    scope: &CallScope,
) -> StreamOutcome {
    let started = Instant::now();
    let _waiter = shared.register_waiter(&entry.change_signal);
    let mut truncated = false;
    let mut checked_to = start;
    let mut scan = super::stream_scan::StreamScan::new(start);
    let mut last_change = started;
    let mut closed_since: Option<Instant> = None;
    // The output generation the last successful read started from; `None` before the first.
    let mut inspected: Option<u64> = None;
    let end = |reason, truncated, unread| StreamOutcome {
        reason,
        line: None,
        offset: None,
        exit_code: None,
        context_truncated: truncated,
        unread,
    };
    loop {
        #[cfg(test)]
        PASSES.with(|passes| passes.set(passes.get() + 1));
        let seen = entry.change_signal.sequence();
        // Read before the generation, so output published before the close is below it.
        let closed = entry.change_signal.is_closed();
        if closed && closed_since.is_none() {
            closed_since = Some(Instant::now());
        }
        let generation = entry.output_generation.load(Ordering::Acquire);
        let unread = |inspected: Option<u64>| {
            inspected != Some(entry.output_generation.load(Ordering::Acquire))
        };
        if scope.revoked(shared) {
            // When: the call `scope` was `revoked`, the wait stops.
            return end(WaitReason::Revoked, truncated, unread(inspected));
        }
        // The tap is read before the exit is checked, so text printed before the exit matches.
        // Only the characters since the last read are copied under the lock; the search runs
        // after it is released.
        let read = entry
            .parser
            .try_lock_for(super::read::PARSER_WAIT)
            .and_then(|parser| scan.read(&parser));
        let parser_busy = read.is_none();
        if let Some(read) = read {
            // When: the `read` of the tap succeeded, everything up to `generation` was checked.
            inspected = Some(generation);
            scan.absorb(read);
            truncated = scan.truncated();
            if scan.end() != checked_to {
                last_change = Instant::now();
                checked_to = scan.end();
            }
            if let Some(found) = scan.find(&condition.regex) {
                // When: the regex `found` a match in the text printed since the wait began.
                return StreamOutcome {
                    reason: WaitReason::Matched,
                    line: Some(found.line),
                    offset: Some(found.offset),
                    exit_code: None,
                    context_truncated: truncated,
                    unread: false,
                };
            }
        }
        let pending = unread(inspected);
        if let Some(status) = entry.exit_status.get().filter(|_| condition.watch_exit) {
            // When: `watch_exit` is on and the `exit_status` cell is set, the process exited.
            let mut done = end(WaitReason::Exited, truncated, pending);
            done.exit_code = match status {
                sonicterm_types::PaneExitStatus::Code(code) => Some(i64::from(*code)),
                _ => None,
            };
            return done;
        }
        let now = Instant::now();
        let drained = closed_since.is_some_and(|since| now.duration_since(since) >= CLOSE_DRAIN);
        if closed && (!pending || drained) {
            // When: the pane was `closed` and its last output was read, or `CLOSE_DRAIN` passed
            // with the parser still busy, nothing more will be printed.
            return end(WaitReason::PaneClosed, truncated, pending);
        }
        if !pending && condition.quiet.is_some_and(|quiet| now.duration_since(last_change) >= quiet)
        {
            // When: nothing was printed for `quiet` and every batch was read, the pane is quiet.
            return end(WaitReason::Quiet, truncated, false);
        }
        if now.duration_since(started) >= condition.timeout {
            // When: the `timeout` passed first, the wait ends without a match.
            return end(WaitReason::Timeout, truncated, pending);
        }
        let mut wake_at = (started + condition.timeout).min(now + condition.recheck);
        // The quiet deadline counts only once every batch was read; with output pending it has
        // nothing to decide, and a passed one would turn every retry into a spin.
        if let Some(quiet) = condition.quiet.filter(|_| !pending) {
            wake_at = wake_at.min(last_change + quiet);
        }
        if closed || parser_busy {
            // When: the pane is `closed` (its signal no longer blocks) or `parser_busy` (no
            // announcement comes), so the retry is a plain bounded sleep rather than a signal wait.
            let retry_at = wake_at.min(now + super::tools_read::WAIT_POLL);
            std::thread::sleep(retry_at.saturating_duration_since(Instant::now()));
            continue;
        }
        entry.change_signal.wait_past(seen, wake_at);
    }
}

/// Whether `arguments` ask for a stream wait; an unknown `source` is an error.
pub(crate) fn wants_stream(arguments: &Value) -> Result<bool, Value> {
    match arguments.get("source").and_then(Value::as_str) {
        None | Some("screen") => Ok(false),
        Some("stream") => Ok(true),
        Some(other) => Err(tool_error("invalid_arguments", &format!("unknown source {other:?}"))),
    }
}

#[cfg(test)]
#[path = "stream_wait_tests.rs"]
mod stream_wait_tests;
