//! Pins the control tools' worker side: arguments are checked before anything reaches the loop,
//! the tool list marks destructive tools, and a run's wait reads its output and exit code from the
//! pinned record, sees interference, and frees the pane however it ends.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sonicterm_cfg::config::McpControl;

use super::*;
use crate::app::mcp::run::{wait_run, RunStarted, RunState};
use crate::app::mcp::{test_entry, PaneEntry};

/// A call admitted now on `shared` for a connection that stays open.
fn scope(shared: &McpShared) -> CallScope {
    CallScope::admit(shared, &Arc::new(AtomicBool::new(false)))
}

/// Every control tool is listed; creating is not destructive, the others are.
#[test]
fn the_tool_list_marks_destructive_tools() {
    let tools = control_tools();
    let hint = |name: &str| {
        let tool = tools.iter().find(|tool| tool["name"] == name).expect(name);
        tool["annotations"]["destructiveHint"].as_bool()
    };
    assert_eq!(hint("create_pane"), Some(false));
    for name in ["send_text", "send_keys", "run_command", "close_pane"] {
        assert_eq!(hint(name), Some(true), "{name}");
    }
}

/// Every control tool declares an object `outputSchema`, and a refusal's `error` and `message` fit
/// it, so a client validating results accepts errors too.
#[test]
fn every_control_tool_declares_an_output_schema_errors_fit() {
    for tool in control_tools() {
        let schema = &tool["outputSchema"];
        assert_eq!(schema["type"], "object", "{}", tool["name"]);
        for key in ["error", "message"] {
            assert_eq!(schema["properties"][key]["type"], "string", "{} {key}", tool["name"]);
        }
    }
}

/// Each structured field a run result carries is declared in `run_command`'s schema.
#[test]
fn a_run_result_fits_its_schema() {
    let shared = McpShared::new(McpControl::Agent, None);
    let run = started(test_entry(30, 6, 100));
    run.entry.parser.lock().advance(b"ls\r\n\x1b]133;C\x07a\r\n\x1b]133;D;0\x07");
    let outcome = wait_run(&shared, run, Duration::from_secs(5), &scope(&shared));
    let tools = control_tools();
    let schema = &tools.iter().find(|tool| tool["name"] == "run_command").unwrap()["outputSchema"];
    let states = schema["properties"]["state"]["enum"].as_array().unwrap();
    assert!(states.iter().any(|state| state == outcome.state.as_str()));
    for state in [RunState::NotStarted, RunState::Interfered, RunState::Lost, RunState::Revoked] {
        assert!(states.iter().any(|known| known == state.as_str()), "{state:?}");
    }
}

/// Bad arguments are refused on the worker, before anything is queued for the loop.
#[test]
fn bad_arguments_never_reach_the_loop() {
    let shared = McpShared::new(McpControl::Any, None);
    let call_scope = scope(&shared);
    let cases = [
        ("send_text", json!({ "pane_id": "1", "text": "x".repeat(MAX_TEXT_BYTES + 1) })),
        ("send_text", json!({ "text": "x" })),
        ("send_keys", json!({ "pane_id": "1", "keys": vec!["a"; keys::MAX_KEYS + 1] })),
        ("send_keys", json!({ "pane_id": "1", "keys": ["Enter", "Hyper+x"] })),
        ("send_keys", json!({ "pane_id": "1", "keys": ["Enter", 7] })),
        ("send_keys", json!({ "pane_id": "1", "keys": [null] })),
        ("create_pane", json!({ "anchor_pane_id": "1", "placement": "up" })),
        ("create_pane", json!({ "anchor_pane_id": "1", "placement": "tab", "argv": [1, 2] })),
        ("run_command", json!({ "pane_id": "1" })),
        ("close_pane", json!({})),
    ];
    for (name, arguments) in cases {
        let result = call(&shared, name, &arguments, &call_scope).expect(name);
        assert_eq!(result["isError"], true, "{name} {arguments}");
        assert_eq!(result["structuredContent"]["error"], "invalid_arguments", "{name}");
    }
    assert!(shared.inbox.lock().is_empty(), "nothing was queued");
    assert!(call(&shared, "not_a_tool", &json!({}), &call_scope).is_none());
}

/// A run started at `entry`'s newest prompt, as the loop hands one back: the region pinned, the
/// pane reserved and its typing stamp reset.
fn started(entry: PaneEntry) -> RunStarted {
    entry.parser.lock().advance(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    let seq = entry.parser.lock().grid().newest_prompt().unwrap().seq;
    assert!(entry.parser.lock().grid_mut().pin_run(seq));
    entry.agent_run.store(true, Ordering::Release);
    entry.run_typed_at.store(0, Ordering::Release);
    RunStarted { entry, seq }
}

/// The shell's marks complete a pinned run: its output from C to D and its exit code are read,
/// and the pin and the pane reservation are released.
#[test]
fn a_run_completes_with_its_output_and_exit_code() {
    let shared = McpShared::new(McpControl::Agent, None);
    let run = started(test_entry(30, 6, 100));
    let entry = run.entry.clone();
    let parser = Arc::clone(&entry.parser);
    let shell = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(60));
        parser.lock().advance(b"make\r\n\x1b]133;C\x07built\r\nok\r\n\x1b]133;D;3\x07$ ");
    });
    let outcome = wait_run(&shared, run, Duration::from_secs(5), &scope(&shared));
    shell.join().unwrap();
    assert_eq!(outcome.state, RunState::Completed);
    assert_eq!(outcome.exit_code, Some(3));
    assert_eq!(outcome.output, vec!["built", "ok"]);
    assert!(entry.parser.lock().grid().pinned_run().is_none(), "the pin is released");
    assert!(!entry.agent_run.load(Ordering::Acquire), "the pane is free again");
}

/// D without C reports `not_started` with the shell's code and no output.
#[test]
fn a_prompt_ended_without_c_did_not_start() {
    let shared = McpShared::new(McpControl::Agent, None);
    let run = started(test_entry(30, 6, 100));
    run.entry.parser.lock().advance(b"^C\r\n\x1b]133;D;130\x07");
    let outcome = wait_run(&shared, run, Duration::from_secs(5), &scope(&shared));
    assert_eq!(outcome.state, RunState::NotStarted);
    assert_eq!(outcome.exit_code, Some(130));
    assert!(outcome.output.is_empty());
}

/// A run whose program switched to the alternate screen and left it still reads its output from
/// the primary screen; one that finished while the alternate screen was up reads it from the
/// saved primary.
#[test]
fn a_run_reads_its_output_from_the_screen_it_was_pinned_on() {
    let shared = McpShared::new(McpControl::Agent, None);
    let run = started(test_entry(30, 6, 100));
    run.entry.parser.lock().advance(
        b"vim\r\n\x1b]133;C\x07before\r\n\x1b[?1049hFULLSCREEN\x1b[?1049l\x1b]133;D;0\x07",
    );
    let outcome = wait_run(&shared, run, Duration::from_secs(5), &scope(&shared));
    assert_eq!(outcome.state, RunState::Completed);
    assert_eq!(outcome.output, vec!["before"]);

    let run = started(test_entry(30, 6, 100));
    let parser = Arc::clone(&run.entry.parser);
    parser.lock().advance(b"ls\r\n\x1b]133;C\x07listed\r\n\x1b]133;D;0\x07\x1b[?1049h");
    let outcome = wait_run(&shared, run, Duration::from_secs(5), &scope(&shared));
    assert_eq!(outcome.state, RunState::Completed);
    assert_eq!(outcome.output, vec!["listed"]);
    assert!(parser.lock().grid().is_alt(), "the alternate screen is still up");
}

/// Dropping a started run nobody waited on, as when its reply is never delivered, unpins its
/// region and frees the pane; a drop that finds the parser busy still frees the pane, and the
/// next run's start clears the pin left behind.
#[test]
fn dropping_an_undelivered_run_frees_the_pane() {
    let run = started(test_entry(30, 6, 100));
    let entry = run.entry.clone();
    drop(run);
    assert!(entry.parser.lock().grid().pinned_run().is_none());
    assert!(!entry.agent_run.load(Ordering::Acquire));

    let run = started(test_entry(30, 6, 100));
    let entry = run.entry.clone();
    let held = entry.parser.lock();
    drop(run);
    assert!(!entry.agent_run.load(Ordering::Acquire), "the pane is free without the parser");
    drop(held);
    assert!(entry.parser.lock().grid().pinned_run().is_some(), "the pin waits for the next run");
}

/// Typing admitted before C makes the run `interfered`, even when C and D both arrive before the
/// worker's next look; a timeout reports `running`, never success; both release the pane.
#[test]
fn interference_and_timeouts_end_a_run_honestly() {
    let shared = McpShared::new(McpControl::Agent, None);
    let run = started(test_entry(30, 6, 100));
    let entry = run.entry.clone();
    let typed = crate::app::mcp::run::typing_stamp(std::time::Instant::now());
    entry.run_typed_at.store(typed, Ordering::Release);
    entry.parser.lock().advance(b"ls\r\n\x1b]133;C\x07a\r\n\x1b]133;D;0\x07");
    let outcome = wait_run(&shared, run, Duration::from_secs(5), &scope(&shared));
    assert_eq!(outcome.state, RunState::Interfered);
    assert!(!entry.agent_run.load(Ordering::Acquire));

    let run = started(test_entry(30, 6, 100));
    let entry = run.entry.clone();
    let outcome = wait_run(&shared, run, Duration::from_millis(150), &scope(&shared));
    assert_eq!(outcome.state, RunState::Running);
    assert_eq!(outcome.exit_code, None);
    assert!(!entry.agent_run.load(Ordering::Acquire));
}
