//! Pins `wait_for`'s conditions and row tokens: an existing match is found at once, `after_row`
//! skips old lines in full and disabled scrollback, a token from the other screen is stale, quiet,
//! timeout and exit end the wait, and a closed client or a server change revokes it.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use regex::Regex;
use sonicterm_cfg::config::McpControl;
use sonicterm_types::PaneExitStatus;

use super::*;
use crate::app::mcp::test_entry;

/// A call admitted now on `shared`, for a connection that stays open.
fn scope(shared: &McpShared) -> CallScope {
    CallScope::admit(shared, &Arc::new(AtomicBool::new(false)))
}

/// Feed `bytes` to the pane and publish a new output generation, as the VT worker does.
fn emit(entry: &PaneEntry, bytes: &[u8]) {
    entry.parser.lock().advance(bytes);
    entry.output_generation.fetch_add(1, Ordering::Release);
    entry.change_signal.notify();
}

/// A wait on `regex` after `after`, bounded by `timeout_ms`.
fn regex_wait(pattern: &str, after: Option<RowToken>, timeout_ms: u64) -> WaitCondition {
    WaitCondition {
        regex: Some(Regex::new(pattern).unwrap()),
        after,
        quiet: None,
        watch_exit: false,
        timeout: Duration::from_millis(timeout_ms),
        recheck: WAIT_RECHECK,
    }
}

/// The row token of the pane's newest written line, as `read_pane`'s `end_row` reports it.
fn end_row(entry: &PaneEntry) -> RowToken {
    let snapshot = read_pane(&entry.parser, ReadMode::Tail, ReadLimits::clamped(None, None));
    snapshot.lines.last().expect("the pane has text").row
}

/// Tokens survive the round trip through their string form; malformed ones are refused.
#[test]
fn row_tokens_round_trip_through_strings() {
    let token = RowToken { line: 1234, alt: true };
    assert_eq!(encode_token(token), "a1234");
    assert_eq!(decode_token("a1234"), Some(token));
    assert_eq!(decode_token("p0"), Some(RowToken { line: 0, alt: false }));
    for bad in ["", "x1", "p", "p-1", "a1.5"] {
        assert_eq!(decode_token(bad), None, "{bad}");
    }
}

/// A match already present in the tail ends the wait on its first check.
#[test]
fn an_existing_match_is_found_at_once() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(40, 5, 100);
    emit(&entry, b"building\r\nBUILD OK\r\n$ ");
    let started = Instant::now();
    let outcome =
        wait_until(&shared, &entry, &regex_wait("BUILD OK", None, 5_000), &scope(&shared));
    assert_eq!(outcome.reason, WaitReason::Matched);
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(outcome.matched.unwrap().0, "BUILD OK");
}

/// With `after_row` past an old matching line, unrelated output does not match and the wait
/// times out; a new matching line after the token does.
#[test]
fn after_row_skips_old_lines() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(40, 5, 100);
    emit(&entry, b"DONE\r\n");
    let after = end_row(&entry);
    emit(&entry, b"noise\r\n");
    let outcome =
        wait_until(&shared, &entry, &regex_wait("DONE", Some(after), 200), &scope(&shared));
    assert_eq!(outcome.reason, WaitReason::Timeout);

    emit(&entry, b"DONE again\r\n");
    let outcome =
        wait_until(&shared, &entry, &regex_wait("DONE", Some(after), 2_000), &scope(&shared));
    assert_eq!(outcome.reason, WaitReason::Matched);
    assert_eq!(outcome.matched.unwrap().0, "DONE again");
}

/// Tokens keep counting while history is full or disabled, so new matching output after the
/// token still matches once old rows have been dropped.
#[test]
fn after_row_holds_with_full_or_disabled_scrollback() {
    for scrollback in [3, 0] {
        let shared = McpShared::new(McpControl::Agent, None);
        let entry = test_entry(20, 4, scrollback);
        emit(&entry, b"MARK old\r\n");
        let after = end_row(&entry);
        for index in 0..20 {
            emit(&entry, format!("filler {index}\r\n").as_bytes());
        }
        let condition = regex_wait("MARK", Some(after), 200);
        let outcome = wait_until(&shared, &entry, &condition, &scope(&shared));
        assert_eq!(outcome.reason, WaitReason::Timeout, "scrollback {scrollback}");

        emit(&entry, b"MARK new\r\n");
        let condition = regex_wait("MARK", Some(after), 2_000);
        let outcome = wait_until(&shared, &entry, &condition, &scope(&shared));
        assert_eq!(outcome.reason, WaitReason::Matched, "scrollback {scrollback}");
        assert_eq!(outcome.matched.unwrap().0, "MARK new");
    }
}

/// A primary-screen token used while the alternate screen is active is reported stale.
#[test]
fn a_token_from_the_other_screen_is_stale() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(20, 4, 100);
    emit(&entry, b"prompt\r\n");
    let after = end_row(&entry);
    emit(&entry, b"\x1b[?1049hfull screen");
    let outcome =
        wait_until(&shared, &entry, &regex_wait("x", Some(after), 2_000), &scope(&shared));
    assert!(outcome.stale_row);
}

/// A pane with no new output for `quiet_ms` ends the wait as quiet, measured from the call.
#[test]
fn a_silent_pane_is_quiet() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(20, 4, 100);
    let condition = WaitCondition {
        regex: None,
        after: None,
        quiet: Some(Duration::from_millis(150)),
        watch_exit: false,
        timeout: Duration::from_secs(5),
        recheck: WAIT_RECHECK,
    };
    let started = Instant::now();
    let outcome = wait_until(&shared, &entry, &condition, &scope(&shared));
    assert_eq!(outcome.reason, WaitReason::Quiet);
    assert!(started.elapsed() >= Duration::from_millis(150));
}

/// A process that exits without printing ends an exit wait with its code, read from the
/// pane's exit cell.
#[test]
fn a_silent_exit_ends_the_wait() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(20, 4, 100);
    let (exit_status, signal) = (Arc::clone(&entry.exit_status), Arc::clone(&entry.change_signal));
    let setter = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        let _ = exit_status.set(PaneExitStatus::Code(3));
        signal.notify();
    });
    let condition = WaitCondition {
        regex: None,
        after: None,
        quiet: None,
        watch_exit: true,
        timeout: Duration::from_secs(5),
        recheck: WAIT_RECHECK,
    };
    let outcome = wait_until(&shared, &entry, &condition, &scope(&shared));
    setter.join().unwrap();
    assert_eq!(outcome.reason, WaitReason::Exited);
    assert_eq!(outcome.exit_code, Some(3));
}

/// A closed client, a stopped server and a control change each revoke a running wait.
#[test]
fn a_running_wait_is_revoked() {
    type Revoke = fn(&McpShared, &AtomicBool);
    let revokers: [(&str, Revoke); 3] = [
        ("closed", |_, closed| closed.store(true, Ordering::Release)),
        ("stopped", |shared, _| shared.stop()),
        ("control", |shared, _| shared.set_control(McpControl::Off)),
    ];
    for (label, revoke) in revokers {
        let shared = McpShared::new(McpControl::Agent, None);
        let closed = Arc::new(AtomicBool::new(false));
        let entry = test_entry(20, 4, 100);
        let waiter = {
            let (shared, closed) = (Arc::clone(&shared), Arc::clone(&closed));
            std::thread::spawn(move || {
                let call = CallScope::admit(&shared, &closed);
                wait_until(&shared, &entry, &regex_wait("never", None, 10_000), &call).reason
            })
        };
        std::thread::sleep(Duration::from_millis(100));
        revoke(&shared, &closed);
        let started = Instant::now();
        assert_eq!(waiter.join().unwrap(), WaitReason::Revoked, "{label}");
        assert!(started.elapsed() < Duration::from_secs(1), "{label}");
    }
}

/// A line that began at or before `after_row` is not new even when output later extends it by a
/// soft wrap; a line that begins after the token is.
#[test]
fn text_appended_to_an_old_wrapped_line_is_not_new() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(5, 4, 100);
    emit(&entry, b"DONE");
    let after = end_row(&entry);
    emit(&entry, b" xxxx");
    let outcome =
        wait_until(&shared, &entry, &regex_wait("DONE", Some(after), 200), &scope(&shared));
    assert_eq!(outcome.reason, WaitReason::Timeout, "the old DONE stays old");
    emit(&entry, b"\r\nDONE");
    let outcome =
        wait_until(&shared, &entry, &regex_wait("DONE", Some(after), 2_000), &scope(&shared));
    assert_eq!(outcome.reason, WaitReason::Matched);
}

/// Output that keeps moving while the parser is held still resets the quiet timer: a regex wait
/// with `quiet_ms` does not report quiet while output arrives.
#[test]
fn moving_output_is_not_quiet_while_the_parser_is_held() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(20, 4, 100);
    let held = Arc::clone(&entry.parser);
    let generation = Arc::clone(&entry.output_generation);
    let signal = Arc::clone(&entry.change_signal);
    let ticker = std::thread::spawn(move || {
        let _guard = held.lock();
        for _ in 0..30 {
            generation.fetch_add(1, Ordering::Release);
            signal.notify();
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    let condition = WaitCondition {
        regex: Some(Regex::new("never").unwrap()),
        after: None,
        quiet: Some(Duration::from_millis(100)),
        watch_exit: false,
        timeout: Duration::from_millis(250),
        recheck: WAIT_RECHECK,
    };
    // Output moves every 10 ms for 300 ms, so the 250 ms timeout comes before any 100 ms of quiet.
    let outcome = wait_until(&shared, &entry, &condition, &scope(&shared));
    ticker.join().unwrap();
    assert_eq!(outcome.reason, WaitReason::Timeout, "quiet was reported while output moved");
}

/// The tool list names every read tool and marks each read-only.
#[test]
fn the_tool_list_names_the_read_tools() {
    let list = tool_list();
    let tools = list["tools"].as_array().unwrap();
    for name in ["list_panes", "read_pane", "wait_for"] {
        let tool = tools.iter().find(|tool| tool["name"] == name).expect(name);
        assert_eq!(tool["annotations"]["readOnlyHint"], true, "{name}");
    }
}

/// `pane_id` arrives as the string `list_panes` reports, or as a number.
#[test]
fn pane_ids_are_read_from_strings_or_numbers() {
    assert_eq!(pane_id_arg(&json!({ "pane_id": "42" })), Some(42));
    assert_eq!(pane_id_arg(&json!({ "pane_id": 42 })), Some(42));
    assert_eq!(pane_id_arg(&json!({ "pane_id": "x" })), None);
    assert_eq!(pane_id_arg(&json!({})), None);
}

/// `read_pane`'s `generation` is loaded before the read, so a batch published between the load
/// and the read shows in the lines and in `current_generation`, never as a `generation` the
/// snapshot does not contain.
#[test]
fn a_read_reports_the_generation_it_contains_at_least() {
    let entry = test_entry(20, 4, 100);
    entry.parser.lock().advance(b"before\r\n");
    entry.output_generation.fetch_add(1, Ordering::Release);
    let racing = entry.clone();
    READ_HOOK.with_borrow_mut(|hook| {
        *hook = Some(Box::new(move || {
            racing.parser.lock().advance(b"during\r\n");
            racing.output_generation.fetch_add(1, Ordering::Release);
        }));
    });
    let (snapshot, structured) =
        read_with_generation(&entry, ReadMode::Tail, ReadLimits::clamped(None, None));
    READ_HOOK.with_borrow_mut(|hook| *hook = None);
    assert_eq!(structured["generation"], 1);
    assert_eq!(structured["current_generation"], 2);
    assert!(
        snapshot.lines.iter().any(|line| line.text == "during"),
        "at least: later text may show"
    );
}

/// `wait_for` reports the generation its last inspection covered, not whatever was current when it
/// answered: a batch published after that inspection, whose own inspection found the parser busy
/// until the timeout, leaves `generation` behind `current_generation`.
#[test]
fn a_wait_reports_the_generation_it_inspected() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(20, 4, 100);
    entry.parser.lock().advance(b"x\r\n");
    entry.output_generation.fetch_add(1, Ordering::Release);
    let later = entry.clone();
    let condition = WaitCondition {
        regex: Some(Regex::new("never").unwrap()),
        after: None,
        quiet: None,
        watch_exit: false,
        timeout: Duration::from_millis(100),
        recheck: Duration::from_secs(10),
    };
    let scope = CallScope::admit(&shared, &Arc::new(AtomicBool::new(false)));
    let mut holder = None;
    let outcome = wait_until_observed(&shared, &entry, &condition, &scope, || {
        // After the first inspection: a new batch is published while another thread holds the
        // parser past the timeout, so every later inspection is busy.
        later.output_generation.fetch_add(1, Ordering::Release);
        let (held, ready) = std::sync::mpsc::channel();
        let parser = Arc::clone(&later.parser);
        holder = Some(std::thread::spawn(move || {
            let _guard = parser.lock();
            held.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(600));
        }));
        ready.recv().unwrap();
    });
    holder.unwrap().join().unwrap();
    assert_eq!(outcome.reason, WaitReason::Timeout);
    assert_eq!(outcome.inspected_generation, Some(1));
    assert_eq!(entry.output_generation.load(Ordering::Acquire), 2);
}

/// `list_panes` spends one budget on parser probes: with every parser held, the list returns well
/// within a few probe waits instead of one per pane, later panes are marked
/// `"details": "skipped"`, and the result says it is `partial`.
#[test]
fn a_busy_list_stops_probing_after_its_budget() {
    let entries: Vec<PaneEntry> = (0..40)
        .map(|index| {
            let mut entry = crate::app::mcp::test_entry(20, 4, 100);
            entry.pane_id = index;
            entry.metadata = Some(crate::app::mcp::PaneMetadata {
                window_index: 0,
                tab_index: index as usize,
                active: false,
                readonly: false,
            });
            entry
        })
        .collect();
    let guards: Vec<_> = entries.iter().map(|entry| entry.parser.lock()).collect();
    let started = Instant::now();
    let budget_ends = started + LIST_BUDGET;
    let panes: Vec<Value> = entries.iter().map(|entry| pane_summary(entry, budget_ends)).collect();
    let elapsed = started.elapsed();
    drop(guards);
    // 40 panes at a 5 ms wait each would take 200 ms; the budget bounds the whole list. The bound
    // is half of that, leaving a loaded CI runner room for oversleeping timed lock attempts.
    assert!(elapsed < PARSER_WAIT * 40 / 2, "the list took {elapsed:?}");
    assert!(panes.iter().any(|pane| pane["details"] == "skipped"), "no pane was skipped");
    assert!(panes.iter().all(|pane| pane["details"] == "skipped" || pane["busy"] == true));
}

/// `read_pane` and `list_panes` report the observed shell state beside the existing boolean:
/// `none` before any marker, `unverified` for a tokenless one.
#[test]
fn reads_report_the_shell_integration_state() {
    let entry = test_entry(20, 4, 100);
    let (_, fresh) = read_with_generation(&entry, ReadMode::Tail, ReadLimits::clamped(None, None));
    assert_eq!(fresh["shell_integration_state"], "none");
    entry.parser.lock().advance(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    let (_, marked) = read_with_generation(&entry, ReadMode::Tail, ReadLimits::clamped(None, None));
    assert_eq!(marked["shell_integration_state"], "unverified");
    let mut listed = entry.clone();
    listed.metadata = Some(crate::app::mcp::PaneMetadata {
        window_index: 0,
        tab_index: 0,
        active: true,
        readonly: false,
    });
    let summary = pane_summary(&listed, Instant::now() + LIST_BUDGET);
    assert_eq!(summary["shell_integration_state"], "unverified");
    assert_eq!(summary["shell_integration"], true, "the boolean keeps its meaning");
}
