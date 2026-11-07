//! Pins the control tools at their boundaries with the rest of the app, through the real tool and
//! loop paths: every control tool refuses out of scope or in READONLY without writing or changing
//! topology, queued input refuses a run, only typing stamps the pane, an agent closing a child
//! window's last tab reaps that window, and a helper created with argv is held after a real exit
//! that `wait_for` reports.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sonicterm_cfg::config::{Config, McpConfig, McpControl};
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;
use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_vt::vt::CaptureStagingPool;

use super::*;
use crate::app::input_dispatch::PtyInputSource;
use crate::app::mcp::lifecycle::McpServer;
use crate::app::mcp::{tools_read, CallScope, Caller, McpShared};
use crate::app::media::InlineMediaPool;

/// A headless app with private pools.
fn app() -> App {
    App::new(Theme::default(), Config::default(), Keymap::default())
        .with_capture_staging_pool(CaptureStagingPool::new())
        .with_inline_media_pool(InlineMediaPool::new())
}

/// Install a running server at `control` that binds nothing.
fn install(app: &mut App, control: McpControl) -> Arc<McpShared> {
    let shared = McpShared::new(control, None);
    let config = McpConfig { enabled: true, control, ..McpConfig::default() };
    app.config.mcp = config;
    let owner = app.register_mcp_owner();
    app.mcp_server = Some(McpServer {
        shared: Arc::clone(&shared),
        endpoint: "test-endpoint".into(),
        _endpoint_guard: None,
        config,
        charges: HashMap::new(),
        owner,
    });
    shared
}

/// Run tool `name` as a connection's worker does while this thread serves the loop, calling
/// `tick` between drains; the call's result.
fn call_through_loop(
    app: &mut App,
    shared: &Arc<McpShared>,
    name: &str,
    arguments: Value,
    mut tick: impl FnMut(&mut App),
) -> Value {
    let worker_shared = Arc::clone(shared);
    let params = json!({ "name": name, "arguments": arguments });
    let worker = std::thread::spawn(move || {
        let scope = CallScope::admit(&worker_shared, &Arc::new(AtomicBool::new(false)));
        let caller = Caller { self_pane: None, client: None, initialized: true };
        tools_read::call(&worker_shared, &caller, &params, &scope)
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    while !worker.is_finished() {
        assert!(Instant::now() < deadline, "{name} did not finish");
        app.drain_mcp_requests();
        tick(app);
        std::thread::sleep(Duration::from_millis(5));
    }
    worker.join().unwrap()
}

/// Attach a PTY running `program` to `pane_id`, wherever it lives.
fn attach(app: &mut App, pane_id: u64, program: &str, args: &[&str], agent_owned: bool) {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let pty = sonicterm_io::pty::PtyHandle::spawn_with_args(program, &args, 80, 24).expect("pty");
    let pane = app.windows.values_mut().find_map(|window| window.panes.get_mut(&pane_id)).unwrap();
    pane.pty = Some(pty);
    pane.agent_owned = agent_owned;
}

/// A program that echoes nothing and stays alive, on this platform.
fn idle_program() -> (&'static str, &'static [&'static str]) {
    #[cfg(unix)]
    return ("/bin/cat", &[]);
    #[cfg(windows)]
    return ("cmd.exe", &["/D", "/Q"]);
}

/// The panes and tabs of every window, to show nothing changed.
fn topology(app: &App) -> Vec<(usize, usize)> {
    let mut shape: Vec<_> =
        app.windows.values().map(|window| (window.tab_states.len(), window.panes.len())).collect();
    shape.sort_unstable();
    shape
}

/// Every control tool, dispatched through the tool path and the loop, refuses with Off, with
/// agent scope on the user's pane (creating beside it is the one allowed call) and in READONLY
/// under any scope; a refusal writes nothing and changes no pane or tab.
#[test]
fn every_control_tool_refuses_out_of_scope_without_side_effects() {
    let mut app = app();
    let user = app.__test_seed_tab("user");
    let agent = app.__test_seed_tab("agent");
    let (program, args) = idle_program();
    attach(&mut app, user, program, args, false);
    attach(&mut app, agent, program, args, true);
    app.pty_write_log_enabled = true;
    let tools = |pane: u64| {
        [
            ("create_pane", json!({ "anchor_pane_id": pane.to_string(), "placement": "tab" })),
            ("send_text", json!({ "pane_id": pane.to_string(), "text": "x" })),
            ("send_keys", json!({ "pane_id": pane.to_string(), "keys": ["Enter"] })),
            ("run_command", json!({ "pane_id": pane.to_string(), "command": "ls" })),
            ("close_pane", json!({ "pane_id": pane.to_string() })),
        ]
    };
    let cases = [
        (McpControl::Off, user, false, "control_off"),
        (McpControl::Off, agent, false, "control_off"),
        (McpControl::Agent, user, false, "out_of_scope"),
        (McpControl::Any, agent, true, "readonly"),
        (McpControl::Agent, agent, true, "readonly"),
    ];
    for (control, pane, readonly, code) in cases {
        let shared = install(&mut app, control);
        app.main_mut().unwrap().copy_mode = readonly.then(|| CopyModeState::read_only_at((0, 0)));
        for (name, arguments) in tools(pane) {
            if control == McpControl::Agent && !readonly && name == "create_pane" {
                // Agent scope lets a helper open beside any pane; the scope matrix test covers it.
                continue;
            }
            let shape = topology(&app);
            let result = call_through_loop(&mut app, &shared, name, arguments, |_| {});
            let label = format!("{control:?} pane {pane} readonly {readonly} {name}");
            assert_eq!(result["isError"], true, "{label}: {result}");
            assert_eq!(result["structuredContent"]["error"], code, "{label}");
            assert_eq!(topology(&app), shape, "{label} changed the layout");
            assert!(app.test_pty_writes.lock().is_empty(), "{label} wrote");
        }
    }
}

/// A pane whose program never reads, on a raw terminal so nothing is discarded, keeps its input
/// queued for as long as it lives; `run_command` then refuses with `input_busy` and pins nothing.
#[cfg(unix)]
#[test]
fn queued_input_refuses_a_run_before_anything_is_pinned() {
    let mut app = app();
    let pane_id = app.__test_seed_tab("agent");
    attach(&mut app, pane_id, "/bin/sh", &["-c", "stty raw -echo; exec sleep 30"], true);
    // `stty` must take effect before the writes, or the line discipline could discard them.
    std::thread::sleep(Duration::from_millis(300));
    let parser = Arc::clone(&app.main().unwrap().panes[&pane_id].parser);
    parser.lock().advance(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    // With the reader asleep the writer blocks once the terminal buffer fills; two whole
    // messages still queued behind it cannot drain while `sleep` runs.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let _ = app.write_agent_input(pane_id, vec![b'x'; 16 * 1024], PtyInputSource::Keyboard);
        let pane = &app.main().unwrap().panes[&pane_id];
        if pane.pty.as_ref().unwrap().input_diagnostics().queued_messages >= 2 {
            break;
        }
        assert!(Instant::now() < deadline, "the stalled writer should keep input queued");
    }
    // The writes above stamped typing; clear it so only the queue can refuse.
    app.main_mut().unwrap().panes.get_mut(&pane_id).unwrap().last_input_at = None;
    let reply = app.mcp_run_start(McpControl::Agent, pane_id, "ls");
    assert!(matches!(reply, LoopReply::Refused("input_busy", _)));
    assert!(parser.lock().grid().pinned_run().is_none(), "nothing is pinned");
    assert!(!app.main().unwrap().panes[&pane_id].agent_run.load(Ordering::Acquire));
}

/// Typing admitted through the real input queue stamps the pane once per run, before its bytes
/// are queued; a focus report and the agent's own command do not.
#[test]
fn only_typing_stamps_the_pane_and_only_the_first_is_kept() {
    let mut app = app();
    let pane_id = app.__test_seed_tab("agent");
    let (program, args) = idle_program();
    attach(&mut app, pane_id, program, args, true);
    let stamp = Arc::clone(&app.main().unwrap().panes[&pane_id].run_typed_at);
    stamp.store(0, Ordering::SeqCst);
    app.write_agent_input(pane_id, b"\x1b[I".to_vec(), PtyInputSource::FocusReport).unwrap();
    assert_eq!(stamp.load(Ordering::SeqCst), 0, "a focus report is not typing");
    let before = Instant::now();
    app.write_agent_input(pane_id, b"a".to_vec(), PtyInputSource::Keyboard).unwrap();
    let first = stamp.load(Ordering::SeqCst);
    assert_ne!(first, 0, "typing stamps the pane");
    std::thread::sleep(Duration::from_millis(2));
    app.write_agent_input(pane_id, b"b".to_vec(), PtyInputSource::Keyboard).unwrap();
    assert_eq!(stamp.load(Ordering::SeqCst), first, "the first typing is kept");
    app.write_agent_input(pane_id, b"c".to_vec(), PtyInputSource::AgentRun).unwrap();
    assert_eq!(stamp.load(Ordering::SeqCst), first, "the agent's own command is not typing");
    assert!(crate::app::mcp::run::typing_stamp(before) <= first, "stamped at admission");
}

/// A stamp claimed for a write that is then refused is taken back, unless a run reset it first.
#[test]
fn a_refused_write_takes_its_typing_stamp_back() {
    use crate::app::mcp::run::{claim_typing_stamp, release_typing_stamp};
    let stamp = std::sync::atomic::AtomicU64::new(0);
    let at = Instant::now();
    assert!(claim_typing_stamp(&stamp, at));
    assert!(!claim_typing_stamp(&stamp, at + Duration::from_millis(1)), "the first is kept");
    release_typing_stamp(&stamp, at);
    assert_eq!(stamp.load(Ordering::SeqCst), 0);
    assert!(claim_typing_stamp(&stamp, at));
    stamp.store(0, Ordering::SeqCst);
    let later = at + Duration::from_millis(3);
    assert!(claim_typing_stamp(&stamp, later));
    release_typing_stamp(&stamp, at);
    assert_ne!(stamp.load(Ordering::SeqCst), 0, "another write's stamp is left alone");
}

/// Closing the only pane of a child window's last tab closes the tab and reaps the window; the
/// main window's last tab stays refused.
#[test]
fn closing_a_child_windows_last_tab_reaps_the_window() {
    let mut app = app();
    let main_pane = app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["helper"]);
    let child_pane = *app.windows[&child].panes.keys().next().unwrap();
    app.windows.get_mut(&child).unwrap().panes.get_mut(&child_pane).unwrap().agent_owned = true;
    assert!(matches!(app.mcp_close_pane(McpControl::Agent, child_pane), LoopReply::Closed));
    assert!(!app.windows.contains_key(&child), "the emptied child window is reaped");
    assert!(app.mcp_pane_entries().iter().all(|entry| entry.pane_id != child_pane));
    assert!(matches!(
        app.mcp_close_pane(McpControl::Any, main_pane),
        LoopReply::Refused("last_tab", _)
    ));
}

/// A helper created with argv runs its program directly; when the program really exits, even
/// with code 0, the pane is held, `wait_for` with `exit` reports the code, and the status line is
/// in the pane.
#[cfg(unix)]
#[test]
fn a_helper_with_argv_is_held_after_its_real_exit() {
    let mut app = app();
    let anchor = app.__test_seed_tab("agent");
    app.main_mut().unwrap().panes.get_mut(&anchor).unwrap().agent_owned = true;
    let shared = install(&mut app, McpControl::Agent);
    let created = call_through_loop(
        &mut app,
        &shared,
        "create_pane",
        json!({
            "anchor_pane_id": anchor.to_string(),
            "placement": "tab",
            "argv": ["/bin/sh", "-c", "sleep 0.3; exit 0"],
        }),
        |_| {},
    );
    assert_eq!(created["structuredContent"]["started"], true, "{created}");
    let helper: u64 = created["structuredContent"]["pane_id"].as_str().unwrap().parse().unwrap();
    let probe = app.main().unwrap().panes[&helper].pty.as_ref().unwrap().child_exit_probe();
    // Headless tests have no VT worker to report the exit, so this loop does what it would:
    // observe the real child and hand its status to the app's exit path.
    let mut reported = false;
    let waited = call_through_loop(
        &mut app,
        &shared,
        "wait_for",
        json!({ "pane_id": helper.to_string(), "exit": true, "timeout_ms": 15000 }),
        |app| {
            if !reported && probe.has_exited().unwrap_or(false) {
                reported = true;
                let status = crate::app::spawn_pane::observe_child_exit_status(&probe);
                app.handle_pane_process_exited(helper, status);
            }
        },
    );
    assert_eq!(waited["structuredContent"]["reason"], "exited", "{waited}");
    assert_eq!(waited["structuredContent"]["exit_code"], 0);
    assert!(app.main().unwrap().panes.contains_key(&helper), "a held pane stays after code 0");
}
