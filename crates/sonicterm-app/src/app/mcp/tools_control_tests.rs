//! Pins the control tools' worker side: arguments are checked before anything reaches the loop,
//! the tool list marks destructive tools, and a run's wait reads its output and exit code from the
//! pinned record, sees interference, and frees the pane however it ends.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sonicterm_cfg::config::McpControl;

use super::*;
use crate::app::mcp::run::{wait_run, wait_run_every, RunStarted, RunState, RUN_CHECKS};
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
    // Every `exit_code_source` value, `null` included, is in the schema's enum.
    let sources = schema["properties"]["exit_code_source"]["enum"].as_array().unwrap();
    let values = ["native", "powershell", "status", "reported", "unverified", "unknown"];
    for value in values.iter().map(|value| json!(value)).chain([Value::Null]) {
        assert!(sources.contains(&value), "{value}");
    }
    assert_eq!(sources.len(), values.len() + 1, "no undocumented value");
    assert!(sources.contains(&json!(outcome.exit_code_source)));
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
    started_with(entry, b"\x1b]133;A\x07$ \x1b]133;B\x07")
}

/// The shell token a tokened test pane's integration carries.
const TEST_TOKEN: &str = "00112233445566778899aabbccddeeff";

/// OSC 133 `kind` (with any `extra` parameter) carrying [`TEST_TOKEN`].
fn tokened(kind: &str, extra: &str) -> Vec<u8> {
    format!("\x1b]133;{kind}{extra};sonicterm_token={TEST_TOKEN}\x07").into_bytes()
}

/// A run started after the prompt marks `prompt`, as [`started`] does.
fn started_with(entry: PaneEntry, prompt: &[u8]) -> RunStarted {
    entry.parser.lock().advance(prompt);
    let seq = entry.parser.lock().grid().newest_prompt().unwrap().seq;
    assert!(entry.parser.lock().grid_mut().pin_run(seq));
    entry.agent_run.store(true, Ordering::Release);
    entry.run_typed_at.store(0, Ordering::Release);
    RunStarted { entry, seq }
}

/// The shell's marks complete a pinned run once its next prompt (A) follows D: its output from C
/// to D and its exit code are read, it is not verified (no shell token), and the pin and the pane
/// reservation are released.
#[test]
fn a_run_completes_with_its_output_and_exit_code() {
    let shared = McpShared::new(McpControl::Agent, None);
    let run = started(test_entry(30, 6, 100));
    let entry = run.entry.clone();
    let parser = Arc::clone(&entry.parser);
    let shell = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(60));
        parser
            .lock()
            .advance(b"make\r\n\x1b]133;C\x07built\r\nok\r\n\x1b]133;D;3\x07\x1b]133;A\x07$ ");
    });
    let outcome = wait_run(&shared, run, Duration::from_secs(5), &scope(&shared));
    shell.join().unwrap();
    assert_eq!(outcome.state, RunState::Completed);
    assert_eq!(outcome.exit_code, Some(3));
    assert!(!outcome.exit_verified, "tokenless markers never verify an exit code");
    assert_eq!(outcome.output, vec!["built", "ok"]);
    assert!(entry.parser.lock().grid().pinned_run().is_none(), "the pin is released");
    assert!(!entry.agent_run.load(Ordering::Acquire), "the pane is free again");
}

/// D without C, then the shell's next prompt, reports `not_started` with the shell's code and no
/// output.
#[test]
fn a_prompt_ended_without_c_did_not_start() {
    let shared = McpShared::new(McpControl::Agent, None);
    let run = started(test_entry(30, 6, 100));
    run.entry.parser.lock().advance(b"^C\r\n\x1b]133;D;130\x07\x1b]133;A\x07");
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
        b"vim\r\n\x1b]133;C\x07before\r\n\x1b[?1049hFULLSCREEN\x1b[?1049l\x1b]133;D;0\x07\x1b]133;A\x07",
    );
    let outcome = wait_run(&shared, run, Duration::from_secs(5), &scope(&shared));
    assert_eq!(outcome.state, RunState::Completed);
    assert_eq!(outcome.output, vec!["before"]);

    // A run that ended just before a program opened the alternate screen gets no next prompt on
    // its screen, so only authenticated marks complete it; its output is read from the saved
    // primary.
    let entry = test_entry(30, 6, 100);
    entry.parser.lock().set_shell_token(Some(TEST_TOKEN.to_owned()));
    let run = started_with(entry, &[tokened("A", ""), b"$ ".to_vec(), tokened("B", "")].concat());
    let parser = Arc::clone(&run.entry.parser);
    let output = [
        b"ls\r\n".to_vec(),
        tokened("C", ""),
        b"listed\r\n".to_vec(),
        tokened("D", ";0"),
        b"\x1b[?1049h".to_vec(),
    ];
    parser.lock().advance(&output.concat());
    let outcome = wait_run(&shared, run, Duration::from_secs(5), &scope(&shared));
    assert_eq!(outcome.state, RunState::Completed);
    assert!(outcome.exit_verified);
    assert_eq!(outcome.output, vec!["listed"]);
    assert!(parser.lock().grid().is_alt(), "the alternate screen is still up");
}

/// The reported forgery: a command prints a fake `D;0`, then keeps running and fails. The run must
/// not report success at the fake D; it completes only at the shell's real D and next prompt,
/// with the real exit code, and is never marked verified without the shell token.
#[test]
fn a_forged_d_from_command_output_does_not_complete_the_run() {
    let shared = McpShared::new(McpControl::Agent, None);
    let run = started(test_entry(40, 6, 100));
    let parser = Arc::clone(&run.entry.parser);
    let shell = std::thread::spawn(move || {
        parser.lock().advance(b"cmd\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
        // The command is still running here; a run that ended at the forged D is already wrong.
        std::thread::sleep(Duration::from_millis(300));
        parser.lock().advance(b"\r\n\x1b]133;D;1\x07\x1b]133;A\x07$ ");
    });
    let started_at = std::time::Instant::now();
    let outcome = wait_run(&shared, run, Duration::from_secs(5), &scope(&shared));
    shell.join().unwrap();
    assert!(started_at.elapsed() >= Duration::from_millis(250), "it ended at the forged D");
    assert_eq!(outcome.state, RunState::Completed);
    assert_eq!(outcome.exit_code, Some(1), "the shell's real exit code wins");
    assert!(!outcome.exit_verified);
}

/// In a tokened pane whose shell proved itself, a forged tokenless D cannot complete the run at
/// all: it is dropped, and the run completes, verified, at the shell's own D.
#[test]
fn a_tokened_pane_drops_a_forged_d_and_verifies_the_real_one() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(40, 6, 100);
    entry.parser.lock().set_shell_token(Some(TEST_TOKEN.to_owned()));
    let run = started_with(entry, &[tokened("A", ""), b"$ ".to_vec(), tokened("B", "")].concat());
    let parser = Arc::clone(&run.entry.parser);
    let shell = std::thread::spawn(move || {
        parser.lock().advance(&[b"cmd\r\n".to_vec(), tokened("C", "")].concat());
        parser.lock().advance(b"\x1b]133;D;0\x07\x1b]133;A\x07");
        std::thread::sleep(Duration::from_millis(200));
        parser.lock().advance(&tokened("D", ";7"));
    });
    let outcome = wait_run(&shared, run, Duration::from_secs(5), &scope(&shared));
    shell.join().unwrap();
    assert_eq!(outcome.state, RunState::Completed);
    assert_eq!(outcome.exit_code, Some(7));
    assert!(outcome.exit_verified);
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

/// A recheck no run wake in these tests may rely on.
const LONG_RUN_RECHECK: Duration = Duration::from_secs(10);
/// The bound a signalled run wake meets with room to spare on a loaded runner.
const RUN_WAKE_BOUND: Duration = Duration::from_secs(5);

/// A run wait on its own thread with a 10 s recheck; `parked` fires each time it is about to sleep
/// on the pane's signal, with the signal's mutex held, so the test acts only once it sleeps.
struct ParkedRun {
    handle: std::thread::JoinHandle<(RunState, Duration, u64)>,
    parked: std::sync::mpsc::Receiver<()>,
}

impl ParkedRun {
    /// Start waiting on `run` for `shared`.
    fn start(shared: &Arc<McpShared>, run: RunStarted) -> Self {
        let shared = Arc::clone(shared);
        let (sender, parked) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            crate::app::pane_signal::PARK_HOOK.with_borrow_mut(|hook| {
                *hook = Some(Box::new(move || {
                    let _ = sender.send(());
                }));
            });
            RUN_CHECKS.with(|checks| checks.set(0));
            let begun = std::time::Instant::now();
            let call = scope(&shared);
            let outcome =
                wait_run_every(&shared, run, Duration::from_secs(20), &call, LONG_RUN_RECHECK);
            (outcome.state, begun.elapsed(), RUN_CHECKS.with(std::cell::Cell::get))
        });
        Self { handle, parked }
    }

    /// Block until the wait is asleep on the signal.
    fn until_parked(&self) {
        self.parked.recv_timeout(RUN_WAKE_BOUND).expect("the run wait parked");
    }

    /// The run's state, how long the wait ran, and how many checks it made.
    fn join(self) -> (RunState, Duration, u64) {
        self.handle.join().unwrap()
    }
}

/// Publish `bytes` to `entry` as the VT worker does: parse, bump the generation, announce.
fn publish_to(entry: &PaneEntry, bytes: &[u8]) {
    entry.parser.lock().advance(bytes);
    crate::app::spawn_pane::publish_grid_change(&entry.output_generation, &entry.change_signal);
}

/// The shell's C, D and next prompt, published while the run wait sleeps, end it at once on the
/// unverified path (`prompt_after_end`): the wait sleeps on the signal rather than polling.
#[test]
fn published_markers_wake_a_sleeping_run_wait() {
    let shared = McpShared::new(McpControl::Agent, None);
    let run = started(test_entry(30, 6, 100));
    let entry = run.entry.clone();
    let waiter = ParkedRun::start(&shared, run);
    waiter.until_parked();
    publish_to(&entry, b"ls\r\n\x1b]133;C\x07a\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ");
    let (state, elapsed, checks) = waiter.join();
    assert_eq!(state, RunState::Completed);
    assert!(elapsed < RUN_WAKE_BOUND, "the run woke after {elapsed:?}");
    // One look before the markers and one after; a 50 ms poll would have looked many times.
    assert!(checks <= 3, "the run wait checked {checks} times");
}

/// Tokened C and D, published while the run wait sleeps, end it at once on the verified path.
#[test]
fn verified_markers_wake_a_sleeping_run_wait() {
    let shared = McpShared::new(McpControl::Agent, None);
    let entry = test_entry(30, 6, 100);
    entry.parser.lock().set_shell_token(Some(TEST_TOKEN.to_owned()));
    let mut prompt = tokened("A", "");
    prompt.extend_from_slice(b"$ ");
    prompt.extend(tokened("B", ""));
    let run = started_with(entry, &prompt);
    let entry = run.entry.clone();
    let waiter = ParkedRun::start(&shared, run);
    waiter.until_parked();
    let mut markers = b"ls\r\n".to_vec();
    markers.extend(tokened("C", ""));
    markers.extend_from_slice(b"a\r\n");
    markers.extend(tokened("D", ";0"));
    publish_to(&entry, &markers);
    let (state, elapsed, _) = waiter.join();
    assert_eq!(state, RunState::Completed);
    assert!(elapsed < RUN_WAKE_BOUND, "the verified run woke after {elapsed:?}");
}

/// Retiring the pane ends a sleeping run wait with `pane_closed` at once.
#[test]
fn closing_the_pane_ends_a_run_wait() {
    let shared = McpShared::new(McpControl::Agent, None);
    let run = started(test_entry(30, 6, 100));
    let entry = run.entry.clone();
    let waiter = ParkedRun::start(&shared, run);
    waiter.until_parked();
    entry.change_signal.close();
    let (state, elapsed, _) = waiter.join();
    assert_eq!(state, RunState::PaneClosed);
    assert!(elapsed < RUN_WAKE_BOUND, "the run ended after {elapsed:?}");
    assert!(!entry.agent_run.load(Ordering::Acquire));
}

/// Stopping the server wakes a sleeping run wait at once with `revoked`.
#[test]
fn stopping_the_server_wakes_a_run_wait() {
    let shared = McpShared::new(McpControl::Agent, None);
    let run = started(test_entry(30, 6, 100));
    let waiter = ParkedRun::start(&shared, run);
    waiter.until_parked();
    shared.stop();
    let (state, elapsed, _) = waiter.join();
    assert_eq!(state, RunState::Revoked);
    assert!(elapsed < RUN_WAKE_BOUND, "the run ended after {elapsed:?}");
}

/// Typing into a pane whose program echoes nothing announces no change, so the run wait sees the
/// interference at its next recheck (250 ms in production) rather than on a 50 ms poll.
#[test]
fn silent_typing_is_seen_at_the_recheck() {
    let shared = McpShared::new(McpControl::Agent, None);
    let run = started(test_entry(30, 6, 100));
    let entry = run.entry.clone();
    let typist = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        let typed = crate::app::mcp::run::typing_stamp(std::time::Instant::now());
        entry.run_typed_at.store(typed, Ordering::SeqCst);
    });
    let begun = std::time::Instant::now();
    let outcome = wait_run(&shared, run, Duration::from_secs(5), &scope(&shared));
    typist.join().unwrap();
    assert_eq!(outcome.state, RunState::Interfered);
    // Seen at the first recheck after the typing, never at the 5 s timeout.
    assert!(begun.elapsed() < Duration::from_secs(1), "seen after {:?}", begun.elapsed());
}

/// A closed pane's run returns within the one close drain even while its parser stays held, the
/// output read included, and frees the pane's run reservation.
#[test]
fn a_closed_pane_with_a_held_parser_ends_its_run_within_the_drain() {
    let shared = McpShared::new(McpControl::Agent, None);
    let run = started(test_entry(30, 6, 100));
    let entry = run.entry.clone();
    entry.change_signal.close();
    let held = Arc::clone(&entry.parser);
    let (locked, ready) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = held.lock();
        locked.send(()).unwrap();
        std::thread::sleep(Duration::from_secs(3));
    });
    ready.recv().unwrap();
    let begun = std::time::Instant::now();
    let outcome =
        wait_run_every(&shared, run, Duration::from_secs(20), &scope(&shared), LONG_RUN_RECHECK);
    let elapsed = begun.elapsed();
    assert_eq!(outcome.state, RunState::PaneClosed);
    let drain = crate::app::mcp::tools_read::CLOSE_DRAIN;
    assert!(elapsed < drain + Duration::from_millis(200), "the run returned after {elapsed:?}");
    assert!(!entry.agent_run.load(Ordering::Acquire), "the reservation outlived the run");
    holder.join().unwrap();
}

/// The end-reason table on the MCP Server page, English and Chinese, lists exactly the values
/// `wait_for` and `run_command` can report, so a new reason cannot ship undocumented.
#[test]
fn the_documented_end_reasons_match_the_code() {
    let mut expected: Vec<&str> = crate::app::mcp::tools_read::WaitReason::ALL
        .iter()
        .map(|reason| reason.as_str())
        .chain(RunState::ALL.iter().map(|state| state.as_str()))
        .collect();
    expected.sort_unstable();
    expected.dedup();
    let wiki = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../wiki");
    for (page, heading) in
        [("MCP-Server.md", "### How a wait ends"), ("MCP-Server-zh-CN.md", "### 等待如何结束")]
    {
        let text = std::fs::read_to_string(wiki.join(page)).unwrap();
        let section = text.split(heading).nth(1).expect(page).split("\n### ").next().unwrap();
        let mut documented: Vec<&str> = section
            .lines()
            .filter_map(|line| line.strip_prefix("| `"))
            .filter_map(|rest| rest.split('`').next())
            .collect();
        documented.sort_unstable();
        assert_eq!(documented, expected, "{page}");
    }
}

/// A write reports where its bytes stand: `written` once the pane's writer flushed them, `queued`
/// when the writer is still busy after the wait, `failed` when it stopped first. A pane without a
/// PTY reports `queued`, never a delivery it cannot see.
#[test]
fn a_write_reports_its_delivery() {
    let without_pty = written(LoopReply::Written(2, None));
    assert_eq!(without_pty["structuredContent"]["delivery"], "queued");
    // A real PTY whose child reads its input, so the writer flushes: `cat` on Unix, `cmd` on Windows.
    #[cfg(unix)]
    let (program, args) = ("/bin/cat", Vec::new());
    #[cfg(windows)]
    let (program, args) = ("cmd.exe", vec!["/D".into(), "/Q".into()]);
    let pty = sonicterm_io::pty::PtyHandle::spawn_with_args(program, &args, 80, 24).unwrap();
    let probe = pty.delivery_probe();
    pty.send_input_nonblocking(b"ok".to_vec()).unwrap();
    let ticket = sonicterm_io::pty::last_admitted_ticket();
    let reply = written(LoopReply::Written(2, Some((ticket, probe.clone()))));
    assert_eq!(reply["structuredContent"]["delivery"], "written");
    assert_eq!(reply["structuredContent"]["bytes"], 2);
}
