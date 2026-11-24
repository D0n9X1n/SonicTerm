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

/// The tool list names every read tool, `search_pane` and `command_history` included, and marks
/// each read-only; `read_pane` offers the `history` mode, its `before_row` cursor and the
/// `last_command` `seq`.
#[test]
fn the_tool_list_names_the_read_tools() {
    let list = crate::app::mcp::tool_schemas::tool_list();
    let tools = list["tools"].as_array().unwrap();
    for name in ["list_panes", "read_pane", "wait_for", "search_pane", "command_history"] {
        let tool = tools.iter().find(|tool| tool["name"] == name).expect(name);
        assert_eq!(tool["annotations"]["readOnlyHint"], true, "{name}");
    }
    let read = tools.iter().find(|tool| tool["name"] == "read_pane").unwrap();
    let modes = read["inputSchema"]["properties"]["mode"]["enum"].as_array().unwrap();
    assert!(modes.contains(&json!("history")), "{modes:?}");
    assert!(read["inputSchema"]["properties"]["before_row"].is_object());
    assert!(read["inputSchema"]["properties"]["seq"].is_object());
    let search = tools.iter().find(|tool| tool["name"] == "search_pane").unwrap();
    assert_eq!(search["inputSchema"]["required"], json!(["pane_id", "regex"]));
}

/// `read_pane` in `history` mode pages back by passing each result's `first_row` as the next
/// `before_row`, and reports `at_start` once the oldest retained row is reached.
#[test]
fn read_pane_history_pages_back_through_first_row() {
    let entry = test_entry(20, 4, 100);
    emit(&entry, b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
    let first = read_reply(&entry, &json!({ "mode": "history", "max_lines": 3 }));
    let result = &first["structuredContent"];
    assert_eq!(result["lines"], json!(["three", "four", "five"]));
    assert_eq!(result["at_start"], false);
    assert_eq!(result["cut"], false);
    let before = result["first_row"].as_str().expect("a token").to_owned();
    let second = read_reply(&entry, &json!({ "mode": "history", "before_row": before }));
    assert_eq!(second["structuredContent"]["lines"], json!(["one", "two"]));
    assert_eq!(second["structuredContent"]["at_start"], true);
}

/// `before_row` is refused outside `history` and when malformed; a token naming the other screen
/// or a row that left history is refused with its stable code.
#[test]
fn read_pane_refuses_bad_history_cursors() {
    let entry = test_entry(10, 3, 3);
    let refusal = |reply: Value| reply["structuredContent"]["error"].as_str().map(str::to_owned);
    let tail = json!({ "mode": "tail", "before_row": "p0" });
    assert_eq!(refusal(read_reply(&entry, &tail)).as_deref(), Some("invalid_arguments"));
    let malformed = json!({ "mode": "history", "before_row": "z9" });
    assert_eq!(refusal(read_reply(&entry, &malformed)).as_deref(), Some("invalid_arguments"));
    emit(&entry, b"0\r\n1\r\n2\r\n3");
    let oldest = encode_token(entry.parser.lock().grid().row_token(0));
    emit(&entry, b"\r\n4\r\n5\r\n6\r\n7\r\n8\r\n9");
    let evicted = json!({ "mode": "history", "before_row": oldest });
    assert_eq!(refusal(read_reply(&entry, &evicted)).as_deref(), Some("evicted"));
    let primary = encode_token(entry.parser.lock().grid().row_token(1));
    emit(&entry, b"\x1b[?1049h");
    let other = json!({ "mode": "history", "before_row": primary });
    assert_eq!(refusal(read_reply(&entry, &other)).as_deref(), Some("other_screen"));
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
    let (_snapshot, structured) =
        read_with_generation(&entry, ReadMode::Tail, ReadLimits::clamped(None, None));
    READ_HOOK.with_borrow_mut(|hook| *hook = None);
    assert_eq!(structured["generation"], 1);
    assert_eq!(structured["current_generation"], 2);
    let lines = structured["lines"].as_array().unwrap();
    assert!(lines.iter().any(|line| line == "during"), "at least: later text may show");
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
    let panes: Vec<Value> = entries
        .iter()
        .map(|entry| pane_summary(entry, budget_ends, &mut ReplyBudget::new(), false))
        .collect();
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
    let summary =
        pane_summary(&listed, Instant::now() + LIST_BUDGET, &mut ReplyBudget::new(), false);
    assert_eq!(summary["shell_integration_state"], "unverified");
    assert_eq!(summary["shell_integration"], true, "the boolean keeps its meaning");
}

/// `list_panes` reports both panes of a split under the tab's one displayed label (`tab_title`,
/// after the active pane's OSC 1) and each pane's own OSC 1 request (`icon_title`, `null` when it
/// sent none), while `title` stays the OSC 0/2 title.
#[test]
fn list_panes_reports_the_tab_label_and_each_panes_icon_title() {
    use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
    let mut app = crate::app::App::new(Theme::default(), Config::default(), Keymap::default());
    let named = app.__test_seed_tab("main");
    app.__test_set_main_pane_viewport(
        sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0),
        10.0,
        10.0,
    );
    app.__test_split_active_right();
    let window = app.main_mut().expect("main window");
    let other = window.tab_states[0].active_pane;
    assert_ne!(other, named, "precondition: the split's new pane is active");
    window.panes[&named].parser.lock().advance(b"\x1b]1;alpha\x07\x1b]2;window title\x07");
    window.tab_states[0].active_pane = named;
    let crate::app::WindowState { tabs, panes, .. } = window;
    let parser = panes[&named].parser.lock();
    crate::app::refresh_active_tab_title(tabs, named, &panes[&named], &parser, 0);
    drop(parser);
    let label = tabs.active().expect("one tab").title.clone();
    let budget_ends = Instant::now() + LIST_BUDGET;
    let listed: Vec<Value> = app
        .mcp_pane_entries()
        .iter()
        .map(|entry| pane_summary(entry, budget_ends, &mut ReplyBudget::new(), false))
        .collect();
    let summary = |pane_id: u64| {
        let wanted = pane_id.to_string();
        listed
            .iter()
            .find(|pane| pane["pane_id"].as_str() == Some(&wanted))
            .expect("listed")
            .clone()
    };
    for pane_id in [named, other] {
        assert_eq!(summary(pane_id)["tab_title"], label.as_str(), "pane {pane_id}");
    }
    assert!(label.ends_with(" alpha"), "{label}");
    assert_eq!(summary(named)["icon_title"], "alpha");
    assert_eq!(summary(named)["title"], "window title");
    assert_eq!(summary(other)["icon_title"], Value::Null);
}

/// `list_panes` copies a pane's live fields and its user-variable snapshot under the parser guard
/// and builds no JSON until the guard is released, so a long list cannot hold a parser for its
/// serialization.
#[test]
fn a_pane_summary_serializes_after_the_parser_guard_is_released() {
    let mut entry = test_entry(20, 4, 100);
    entry.parser.lock().advance(b"\x1b]1337;SetUserVar=a=MQ==\x07\x1b]2;title\x07");
    entry.metadata = Some(crate::app::mcp::PaneMetadata {
        window_index: 0,
        tab_index: 0,
        active: true,
        readonly: false,
    });
    let observed = Arc::new(std::sync::Mutex::new(None));
    let record = Arc::clone(&observed);
    SUMMARY_HOOK.with_borrow_mut(|hook| {
        *hook = Some(Box::new(move |entry: &PaneEntry| {
            *record.lock().unwrap() = Some(entry.parser.is_locked());
        }));
    });
    let summary =
        pane_summary(&entry, Instant::now() + LIST_BUDGET, &mut ReplyBudget::new(), false);
    SUMMARY_HOOK.with_borrow_mut(|hook| *hook = None);
    assert_eq!(*observed.lock().unwrap(), Some(false), "the parser was held while serializing");
    assert_eq!(summary["title"], "title");
    assert_eq!(summary["user_vars"], json!([{ "name": "a", "value": "1" }]));
}

/// Every field's text of each event logged while `body` runs on this thread.
fn logged_fields(body: impl FnOnce()) -> Vec<String> {
    use tracing_subscriber::layer::SubscriberExt;

    /// Collects every field of every event, rendered with `Debug`.
    #[derive(Clone, Default)]
    struct Fields(Arc<std::sync::Mutex<Vec<String>>>);

    /// Renders one event's fields as `name=value`.
    struct Render(Vec<String>);

    impl tracing::field::Visit for Render {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.push(format!("{}={value:?}", field.name()));
        }
    }

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Fields {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _context: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut render = Render(Vec::new());
            event.record(&mut render);
            self.0.lock().unwrap().extend(render.0);
        }
    }

    let layer = Fields::default();
    let subscriber = tracing_subscriber::Registry::default().with(layer.clone());
    sonicterm_logging::test_capture::with_default(subscriber, body);
    let fields = layer.0.lock().unwrap().clone();
    fields
}

/// User variables are never logged: parsing a valid record and a dropped one, then listing the
/// pane, logs the drop's reason code and counts but no name or value, while the listing still
/// carries the variable.
#[test]
fn user_var_names_and_values_are_never_logged() {
    let entry = test_entry(20, 4, 100);
    let mut summary = Value::Null;
    let fields = logged_fields(|| {
        // `c2VjcmV0LXZhbHVl` is `secret-value`; the second record's base64 is invalid.
        entry.parser.lock().advance(
            b"\x1b]1337;SetUserVar=secret-name=c2VjcmV0LXZhbHVl;SetUserVar=secret-bad=!!\x07",
        );
        summary =
            pane_summary(&entry, Instant::now() + LIST_BUDGET, &mut ReplyBudget::new(), false);
    });
    assert_eq!(summary["user_vars"], json!([{ "name": "secret-name", "value": "secret-value" }]));
    assert!(fields.iter().any(|field| field == "reason=\"value_base64\""), "{fields:?}");
    assert!(fields.iter().all(|field| !field.contains("secret")), "leaked: {fields:?}");
}
