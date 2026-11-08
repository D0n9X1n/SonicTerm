//! Pins the event loop's budgeted MCP drain and the tool-call log's byte counts: a drain yields
//! after its budget and answers the rest next, in order and ahead of later arrivals; a notice frees
//! its slot exactly once; a re-queue posts one wake-up; the log reports wire lengths; and moving
//! `read_pane`'s lines into its JSON changes no byte of the result.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use sonicterm_cfg::config::McpControl;

use super::*;
use crate::app::mcp::read::{ReadLimits, ReadMode};
use crate::app::mcp::{test_entry, LoopCall};

/// A queued `ResolvePane` for `pane_id`, admitted under `shared`'s current generation.
fn request(shared: &McpShared, pane_id: u64) -> LoopRequest {
    let (reply, _answer) = sync_channel(1);
    LoopRequest {
        call: LoopCall::ResolvePane { pane_id },
        generation: shared.generation.load(Ordering::Acquire),
        deadline: Instant::now() + Duration::from_secs(5),
        closed: Arc::new(AtomicBool::new(false)),
        session: shared.sessions.open(),
        reply,
    }
}

/// The pane ids the queued `ResolvePane` requests name, in order.
fn ids(requests: &[LoopRequest]) -> Vec<u64> {
    requests
        .iter()
        .map(|request| match request.call {
            LoopCall::ResolvePane { pane_id } => pane_id,
            _ => 0,
        })
        .collect()
}

/// A clock that advances `step` on every read, so the budget runs out after a known count.
fn stepping(step: Duration) -> impl Fn() -> Instant {
    let start = Instant::now();
    let reads = Cell::new(0u32);
    move || {
        reads.set(reads.get() + 1);
        start + step * reads.get()
    }
}

/// With each answer taking 1 ms of a 2 ms budget, a drain answers two of five requests and hands
/// back the other three, in order.
#[test]
fn a_drain_yields_after_its_budget_in_order() {
    let shared = McpShared::new(McpControl::Agent, None);
    let requests = (1..=5).map(|pane| request(&shared, pane)).collect();
    let mut answered = Vec::new();
    let rest = drain_within(requests, DRAIN_BUDGET, stepping(Duration::from_millis(1)), |done| {
        answered.extend(ids(std::slice::from_ref(&done)));
    });
    assert_eq!(answered, [1, 2]);
    assert_eq!(ids(&rest), [3, 4, 5]);
}

/// Re-queued requests go back ahead of one that arrived during the drain, so the next drain
/// answers them first. The re-queue itself posts no wake-up, because macOS and X11 drain a user
/// event posted during the user-event drain in the same pass, before native input; the wake-up is
/// posted from `about_to_wait`, after the turn's input, and only while requests wait.
#[test]
fn a_requeue_goes_ahead_and_wakes_only_from_about_to_wait() {
    let shared = McpShared::new(McpControl::Agent, None);
    for pane in 1..=3 {
        shared.inbox.lock().push(request(&shared, pane));
    }
    assert!(shared.wake());
    let taken = shared.take_requests();
    // A request arrives while the drain runs; the flag the first wake-up set is still up.
    shared.inbox.lock().push(request(&shared, 9));
    assert!(shared.wake());
    let rest = drain_within(taken, DRAIN_BUDGET, stepping(Duration::from_millis(5)), drop);
    shared.requeue_front(rest);
    // The flag still records the wake-up the loop is handling: nothing new was posted.
    assert!(wake_pending(&shared));
    // `about_to_wait` posts the one wake-up the backlog is owed.
    shared.wake_for_backlog();
    assert!(wake_pending(&shared), "about_to_wait posts the backlog's wake-up");
    assert_eq!(ids(&shared.take_requests()), [2, 3, 9]);
    shared.wake_for_backlog();
    assert!(!wake_pending(&shared), "an empty inbox owes no wake-up");
}

/// A notice frees its slot once it is handled, and a notice that is re-queued untaken keeps it, so
/// the pending count returns to zero exactly once.
#[test]
fn a_notice_frees_its_slot_once() {
    let shared = McpShared::new(McpControl::Agent, None);
    shared.notify("one".into());
    shared.notify("two".into());
    let notices = shared.take_requests();
    let rest = drain_within(notices, DRAIN_BUDGET, stepping(Duration::from_millis(5)), |done| {
        notice_handled(&shared, &done);
    });
    assert_eq!(shared.pending_notices.load(Ordering::Acquire), 1, "one handled, one re-queued");
    shared.requeue_front(rest);
    for done in shared.take_requests() {
        notice_handled(&shared, &done);
    }
    assert_eq!(shared.pending_notices.load(Ordering::Acquire), 0);
}

/// The log's byte counts are the lines' wire lengths: whitespace and escaped Unicode count as sent,
/// and the call's target is read from its params.
#[test]
fn the_log_counts_wire_lengths() {
    let line = r#"{"jsonrpc":"2.0", "id":1,"method":"tools/call","params":{"name":"read_pane","arguments":{"pane_id":"7","x":"é"}}}"#;
    let params = serde_json::from_str::<serde_json::Value>(line).unwrap()["params"].clone();
    assert_eq!(call_target(&params), ("read_pane", Some(7)));
    let reply = crate::app::mcp::jsonrpc::response(&json!(1), json!({ "text": "é" }));
    let record = CallRecord {
        tool: "read_pane",
        pane: Some(7),
        in_bytes: line.len(),
        out_bytes: reply.len(),
        client: None,
        ok: true,
        elapsed: Duration::ZERO,
    };
    // The wire length counts the space and the six-byte escape as sent; re-serializing the params,
    // the byte charge's figure, gives a different and smaller count.
    assert_eq!(record.in_bytes, 114);
    assert!(params.to_string().len() < record.in_bytes, "the charge and the log stay apart");
    assert!(!reply.ends_with('\n'), "the newline is not part of the length");
    log_call(&record);
}

/// `read_pane`'s result is the same whether its lines are moved or copied: the text of a wrapped
/// line, a wide character and a capped read are unchanged.
#[test]
fn moving_lines_into_the_result_changes_nothing() {
    let entry = test_entry(10, 4, 100);
    entry.parser.lock().advance("one\r\n漢字 two\r\na very long wrapped line\r\n".as_bytes());
    for (mode, max_lines) in
        [(ReadMode::Tail, None), (ReadMode::Screen, None), (ReadMode::Tail, Some(1))]
    {
        let limits = ReadLimits::clamped(max_lines, None);
        let copied: Vec<String> = crate::app::mcp::read::read_pane(&entry.parser, mode, limits)
            .lines
            .into_iter()
            .map(|line| line.text)
            .collect();
        let (_, moved) = crate::app::mcp::tools_read::read_with_generation(&entry, mode, limits);
        assert_eq!(moved["lines"], json!(copied), "{mode:?} {max_lines:?}");
    }
}

/// A read's lines move into the JSON array: each string keeps its allocation, so a large read is
/// never held twice.
#[test]
fn read_lines_move_into_the_json_without_a_copy() {
    let lines = vec!["first line".to_owned(), "second".to_owned()];
    let pointers: Vec<*const u8> = lines.iter().map(|line| line.as_ptr()).collect();
    // Inserted into a result object as production does, after `json!` built the other fields.
    let mut value = json!({ "pane_id": "1" });
    insert_owned_lines(&mut value, lines);
    let moved: Vec<*const u8> = value["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line.as_str().unwrap().as_ptr())
        .collect();
    assert_eq!(moved, pointers);
}

/// A capture buffer the test subscriber writes into.
struct SharedLog(Arc<parking_lot::Mutex<Vec<u8>>>);

impl std::io::Write for SharedLog {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The production call path logs the request line's length it was handed and the length of the
/// reply line it formed, read from the bytes themselves, and the call's success from the result.
#[test]
fn run_logged_logs_the_wire_lengths_of_the_lines_it_handles() {
    let line = r#"{"jsonrpc":"2.0", "id":3,"method":"tools/call","params":{"name":"read_pane","arguments":{"pane_id":"4"}}}"#;
    let params = serde_json::from_str::<serde_json::Value>(line).unwrap()["params"].clone();
    let caller =
        super::super::Caller { client: Some("probe".into()), ..super::super::Caller::default() };
    let log = Arc::new(parking_lot::Mutex::new(Vec::<u8>::new()));
    let writer = Arc::clone(&log);
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || SharedLog(Arc::clone(&writer)))
        .finish();
    let reply = sonicterm_logging::test_capture::with_default(subscriber, || {
        run_logged(
            &params,
            line.len(),
            &caller,
            |_| json!({ "content": [{ "type": "text", "text": "é" }], "isError": false }),
            |result| {
                serde_json::to_string(&json!({ "jsonrpc": "2.0", "id": 3, "result": result }))
                    .unwrap()
            },
        )
    });
    let text = String::from_utf8(log.lock().clone()).unwrap();
    assert!(text.contains(&format!("in_bytes={}", line.len())), "{text}");
    assert!(text.contains(&format!("out_bytes={}", reply.len())), "{text}");
    assert!(text.contains("tool=\"read_pane\"") || text.contains("tool=read_pane"), "{text}");
    assert!(text.contains("pane=4") && text.contains("ok=true"), "{text}");
}

/// A request that arrives while the loop answers earlier ones posts no wake-up of its own: the flag
/// stays set through the drain, so nothing chains into the same pass of the user-event drain, and
/// `about_to_wait` posts exactly one for whatever waits; with nothing waiting it clears the flag, so
/// the next request wakes the loop itself.
#[test]
fn a_request_arriving_mid_drain_waits_for_about_to_wait() {
    let shared = McpShared::new(McpControl::Agent, None);
    shared.inbox.lock().push(request(&shared, 1));
    assert!(shared.wake(), "the first request wakes the loop");
    let taken = shared.take_requests();
    assert!(wake_pending(&shared), "taking keeps the flag, so arrivals post nothing");
    // An agent's call arrives while the loop answers.
    shared.inbox.lock().push(request(&shared, 2));
    assert!(shared.wake());
    assert!(wake_pending(&shared));
    drop(taken);
    shared.wake_for_backlog();
    assert!(wake_pending(&shared), "the waiting request is owed one wake-up");
    assert_eq!(ids(&shared.take_requests()), [2]);
    shared.wake_for_backlog();
    assert!(!wake_pending(&shared), "nothing waits, so the next request wakes the loop itself");
}
