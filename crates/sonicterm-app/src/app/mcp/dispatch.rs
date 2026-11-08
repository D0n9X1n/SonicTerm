//! What runs around each MCP request: the event loop's budgeted drain of queued requests, and the
//! one log line per tool call, whose byte counts are the request and reply lines' wire lengths
//! measured from bytes already in hand, never from a second serialization.

use std::time::{Duration, Instant};

use serde_json::Value;

use super::{LoopRequest, McpShared};

/// How long one drain answers queued requests before it yields to the event loop. Cooperative: a
/// request that takes longer still runs to its end.
pub(crate) const DRAIN_BUDGET: Duration = Duration::from_millis(2);

/// The drain's budget: [`DRAIN_BUDGET`], or a test's replacement on this thread.
pub(crate) fn drain_budget() -> Duration {
    #[cfg(test)]
    if let Some(budget) = BUDGET_OVERRIDE.get() {
        // When: a test set a `budget` on this thread, the drain uses it.
        return budget;
    }
    DRAIN_BUDGET
}

#[cfg(test)]
thread_local! {
    /// A test's drain budget, so an App-level test can make every drain yield after one request.
    pub(crate) static BUDGET_OVERRIDE: std::cell::Cell<Option<Duration>> = const { std::cell::Cell::new(None) };
}

impl McpShared {
    /// Put `rest` back at the front of the inbox, ahead of requests that arrived during the drain,
    /// so the loop answers them in order next. It posts no wake-up itself: on macOS and X11 a user
    /// event posted while the loop drains user events is drained in the same pass, before native
    /// input, so the App posts it from `about_to_wait` instead ([`Self::wake_for_backlog`]).
    pub(crate) fn requeue_front(&self, rest: Vec<LoopRequest>) {
        if rest.is_empty() {
            // When: `rest` is empty, the drain answered everything and nothing waits.
            return;
        }
        let mut inbox = self.inbox.lock();
        let arrived = std::mem::replace(&mut *inbox, rest);
        inbox.extend(arrived);
    }

    /// Re-arm the wake-up at the end of the loop's turn: post one if requests wait, otherwise clear
    /// the flag so the next request wakes the loop itself. Called only from `about_to_wait`, so
    /// neither a re-queue nor a request arriving mid-drain chains the next slice into the same pass
    /// of macOS's or X11's user-event drain; native input and redraws run first.
    // Ordering: wake_pending Release, stored under the inbox lock producers push under first.
    pub(crate) fn wake_for_backlog(&self) {
        let inbox = self.inbox.lock();
        if inbox.is_empty() {
            // When: the inbox `is_empty`, the next request must wake the loop itself.
            self.wake_pending.store(false, std::sync::atomic::Ordering::Release);
            return;
        }
        drop(inbox);
        self.wake_pending.store(false, std::sync::atomic::Ordering::Release);
        let _ = self.wake();
    }
}

/// Answer `requests` in order through `answer` until `budget` has passed, read from `now` after
/// each; return the ones not reached, for [`McpShared::requeue_front`].
pub(crate) fn drain_within(
    requests: Vec<LoopRequest>,
    budget: Duration,
    now: impl Fn() -> Instant,
    mut answer: impl FnMut(LoopRequest),
) -> Vec<LoopRequest> {
    let started = now();
    let mut pending = requests.into_iter();
    for request in pending.by_ref() {
        answer(request);
        if now().duration_since(started) >= budget {
            // When: the drain has run its `budget`, the rest wait for the next wake-up.
            break;
        }
    }
    pending.collect()
}

/// One tool call's log fields, measured from what the call already holds.
pub(crate) struct CallRecord<'call> {
    /// The tool's name.
    pub(crate) tool: &'call str,
    /// The `pane_id` argument, if any.
    pub(crate) pane: Option<u64>,
    /// The request line's length on the wire, newline excluded.
    pub(crate) in_bytes: usize,
    /// The reply line's length on the wire, newline excluded.
    pub(crate) out_bytes: usize,
    /// The client's name from `initialize`.
    pub(crate) client: Option<&'call str>,
    /// The result was not a tool error.
    pub(crate) ok: bool,
    /// How long the call ran.
    pub(crate) elapsed: Duration,
}

/// The fields of a `tools/call` request's `params`: its tool name and `pane_id` argument.
pub(crate) fn call_target(params: &Value) -> (&str, Option<u64>) {
    let tool = params.get("name").and_then(Value::as_str).unwrap_or("");
    let pane = params.get("arguments").and_then(super::tools_read::pane_id_arg);
    (tool, pane)
}

/// Log one tool call; never its arguments or result text.
pub(crate) fn log_call(record: &CallRecord<'_>) {
    tracing::info!(
        target: "mcp",
        tool = record.tool,
        pane = record.pane,
        in_bytes = record.in_bytes as u64,
        out_bytes = record.out_bytes as u64,
        client = record.client.unwrap_or("unknown"),
        ok = record.ok,
        elapsed_ms = record.elapsed.as_millis() as u64,
        "tool call"
    );
}

/// Run one tool call through `call`, which returns its result, form the reply line from it with
/// `respond`, and log the call from the bytes in hand: the request's `wire_bytes` and the reply's
/// length. Success is read from the result's own `isError`, never from the reply's text.
pub(crate) fn run_logged(
    params: &Value,
    wire_bytes: usize,
    caller: &super::Caller,
    call: impl FnOnce(&Value) -> Value,
    respond: impl FnOnce(Value) -> String,
) -> String {
    let started = Instant::now();
    let result = call(params);
    let ok = !result["isError"].as_bool().unwrap_or(false);
    let reply = respond(result);
    let (tool, pane) = call_target(params);
    log_call(&CallRecord {
        tool,
        pane,
        in_bytes: wire_bytes,
        out_bytes: reply.len(),
        client: caller.client.as_deref(),
        ok,
        elapsed: started.elapsed(),
    });
    reply
}

/// Insert `lines` into `result` as its `lines` array, moving each string's allocation in. Passing
/// them through `json!`, even as an owned `Value`, serializes them by reference and copies every
/// string, so a read of many lines would hold each one twice.
pub(crate) fn insert_owned_lines(result: &mut Value, lines: Vec<String>) {
    if let Value::Object(map) = result {
        map.insert(
            "lines".to_owned(),
            Value::Array(lines.into_iter().map(Value::String).collect()),
        );
    }
}

/// Free a notice's slot once the loop handled it, shown or dropped; called once per notice.
pub(crate) fn notice_handled(shared: &McpShared, request: &LoopRequest) {
    if matches!(request.call, super::LoopCall::Notice { .. }) {
        // The request is a notice: its slot frees now, never on a re-queue.
        shared.notice_shown();
    }
}

/// The current instant, for the production drain.
pub(crate) fn clock_now() -> Instant {
    Instant::now()
}

/// Whether the loop's wake-up is pending, for a test that checks one is posted after a re-queue.
// Ordering: wake_pending Acquire pairs with take_requests' Release clear and wake's swap.
#[cfg(test)]
pub(crate) fn wake_pending(shared: &McpShared) -> bool {
    shared.wake_pending.load(std::sync::atomic::Ordering::Acquire)
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod dispatch_tests;
